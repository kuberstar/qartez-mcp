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
