//! Shared SQLite schema-introspection helpers used across the `/api/*`
//! endpoints. The index DB schema drifts between qartez versions, so most
//! handlers probe for a table or column before referencing it and degrade
//! gracefully (empty response) when it is absent. These two helpers were
//! previously copy-pasted byte-for-byte into six endpoint modules; they now
//! live here so the probing logic has a single source of truth.

use rusqlite::{Connection, OptionalExtension, params};

/// Return whether `table` exists in the connected database.
///
/// # Errors
///
/// Propagates any SQLite error from the `sqlite_master` lookup.
pub fn table_exists(conn: &Connection, table: &str) -> anyhow::Result<bool> {
    let exists: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
            params![table],
            |r| r.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

/// Return whether `column` exists on `table` in the connected database.
///
/// # Errors
///
/// Propagates any SQLite error from the `PRAGMA table_info` scan.
pub fn column_exists(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT, change_count INTEGER);",
        )
        .expect("create schema");
        conn
    }

    #[test]
    fn table_exists_detects_presence_and_absence() {
        let conn = db();
        assert!(table_exists(&conn, "files").unwrap());
        assert!(!table_exists(&conn, "unused_exports").unwrap());
    }

    #[test]
    fn column_exists_detects_presence_and_absence() {
        let conn = db();
        assert!(column_exists(&conn, "files", "change_count").unwrap());
        assert!(!column_exists(&conn, "files", "complexity").unwrap());
    }
}
