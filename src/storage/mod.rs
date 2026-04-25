pub mod maintenance;
pub mod models;
pub mod read;
pub mod schema;
pub mod write;

use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

/// Run `PRAGMA foreign_key_check` on the connection and return an error if
/// any violations are found. Call this after committing an
/// `unchecked_transaction` to catch FK inconsistencies that were deferred
/// for bulk-write performance.
pub fn verify_foreign_keys(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let violations: Vec<String> = stmt
        .query_map([], |row| {
            let table: String = row.get(0)?;
            let rowid: i64 = row.get(1)?;
            let parent: String = row.get(2)?;
            Ok(format!("{table} rowid={rowid} -> {parent}"))
        })?
        .filter_map(|r| r.ok())
        .collect();
    if !violations.is_empty() {
        return Err(crate::error::QartezError::Integrity(format!(
            "foreign key violations after unchecked_transaction: {}",
            violations.join("; ")
        )));
    }
    Ok(())
}

pub fn open_db(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;

    // Enable incremental auto-vacuum for new databases so freed pages (e.g.
    // from FTS table rebuilds during indexing) are reclaimed on disk rather
    // than accumulating indefinitely. This must be set before any tables are
    // created; for existing databases without auto_vacuum the change has no
    // effect without a full VACUUM, which we skip here to avoid a long
    // startup stall - WAL checkpointing after indexing is the primary
    // mitigation for those.
    let av: i32 = conn
        .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
        .unwrap_or(0);
    if av == 0 {
        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if table_count == 0 {
            conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")?;
        }
    }

    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -64000;
         PRAGMA busy_timeout = 5000;",
    )?;

    schema::create_schema(&conn)?;

    Ok(conn)
}

#[cfg(test)]
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;

    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;",
    )?;

    schema::create_schema(&conn)?;

    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::QartezError;

    fn pragma_i64(conn: &Connection, pragma: &str) -> i64 {
        conn.query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0))
            .unwrap_or(0)
    }

    fn pragma_string(conn: &Connection, pragma: &str) -> String {
        conn.query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0))
            .unwrap_or_default()
    }

    #[test]
    fn open_in_memory_enables_foreign_keys_and_creates_schema() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        assert_eq!(pragma_i64(&conn, "foreign_keys"), 1);

        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .expect("sqlite_master query must succeed");
        assert!(
            table_count > 0,
            "create_schema must register at least one table"
        );
    }

    #[test]
    fn open_db_sets_expected_pragmas_on_fresh_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("qartez.db");
        let conn = open_db(&db_path).expect("open_db must succeed on fresh path");

        assert_eq!(pragma_i64(&conn, "foreign_keys"), 1);
        assert_eq!(pragma_i64(&conn, "busy_timeout"), 5000);
        let journal = pragma_string(&conn, "journal_mode").to_lowercase();
        assert_eq!(journal, "wal");
        let auto_vacuum = pragma_i64(&conn, "auto_vacuum");
        assert_eq!(
            auto_vacuum, 2,
            "fresh database must use INCREMENTAL auto_vacuum (= 2)"
        );
    }

    #[test]
    fn open_db_is_idempotent_on_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("qartez.db");
        {
            let _ = open_db(&db_path).expect("first open must succeed");
        }
        let _conn = open_db(&db_path).expect("second open must succeed on existing file");
    }

    #[test]
    fn verify_foreign_keys_passes_on_clean_schema() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        verify_foreign_keys(&conn).expect("empty schema must have no FK violations");
    }

    #[test]
    fn verify_foreign_keys_detects_violations_when_checks_deferred() {
        let conn = open_in_memory().expect("in-memory open must succeed");

        conn.execute_batch(
            "CREATE TABLE parent(id INTEGER PRIMARY KEY);
             CREATE TABLE child(
                id INTEGER PRIMARY KEY,
                parent_id INTEGER NOT NULL REFERENCES parent(id)
             );",
        )
        .expect("helper tables must be created");

        conn.execute_batch("PRAGMA foreign_keys = OFF;")
            .expect("disabling FKs must succeed");
        conn.execute("INSERT INTO child(id, parent_id) VALUES (1, 99)", [])
            .expect("orphan insert must succeed with FKs off");
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .expect("re-enabling FKs must succeed");

        let err =
            verify_foreign_keys(&conn).expect_err("verify_foreign_keys must report the orphan row");
        match err {
            QartezError::Integrity(msg) => {
                assert!(
                    msg.contains("child"),
                    "integrity message should name offending table, got: {msg}"
                );
            }
            other => panic!("expected Integrity error, got {other:?}"),
        }
    }
}
