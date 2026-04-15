pub mod models;
pub mod read;
pub mod schema;
pub mod write;

use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

pub fn open_db(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;

    // Enable incremental auto-vacuum for new databases so freed pages (e.g.
    // from FTS table rebuilds during indexing) are reclaimed on disk rather
    // than accumulating indefinitely. This must be set before any tables are
    // created; for existing databases without auto_vacuum the change has no
    // effect without a full VACUUM, which we skip here to avoid a long
    // startup stall — WAL checkpointing after indexing is the primary
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
