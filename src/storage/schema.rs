use rusqlite::Connection;

use crate::error::Result;

const CREATE_FILES: &str = "
CREATE TABLE IF NOT EXISTS files (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    path       TEXT    NOT NULL UNIQUE,
    mtime_ns   INTEGER NOT NULL,
    size_bytes INTEGER NOT NULL,
    language   TEXT    NOT NULL,
    line_count INTEGER NOT NULL,
    pagerank   REAL    NOT NULL DEFAULT 0.0,
    indexed_at INTEGER NOT NULL
)";

const CREATE_SYMBOLS: &str = "
CREATE TABLE IF NOT EXISTS symbols (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    kind        TEXT    NOT NULL,
    line_start  INTEGER NOT NULL,
    line_end    INTEGER NOT NULL,
    signature   TEXT,
    is_exported INTEGER NOT NULL DEFAULT 0,
    shape_hash  TEXT,
    parent_id   INTEGER,
    unused_excluded INTEGER NOT NULL DEFAULT 0,
    pagerank    REAL    NOT NULL DEFAULT 0.0
)";

const CREATE_SYMBOL_REFS: &str = "
CREATE TABLE IF NOT EXISTS symbol_refs (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    from_symbol_id  INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    to_symbol_id    INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    kind            TEXT    NOT NULL DEFAULT 'call',
    UNIQUE(from_symbol_id, to_symbol_id, kind)
)";

const CREATE_EDGES: &str = "
CREATE TABLE IF NOT EXISTS edges (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    from_file INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    to_file   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    kind      TEXT    NOT NULL DEFAULT 'import',
    specifier TEXT,
    UNIQUE(from_file, to_file, kind)
)";

const CREATE_CO_CHANGES: &str = "
CREATE TABLE IF NOT EXISTS co_changes (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    file_a INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    file_b INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    count  INTEGER NOT NULL DEFAULT 1,
    UNIQUE(file_a, file_b)
)";

const CREATE_SYMBOLS_FTS: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
    name,
    kind,
    file_path,
    tokenize='porter unicode61'
)";

const CREATE_SYMBOLS_BODY_FTS: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_body_fts USING fts5(
    body,
    tokenize='unicode61'
)";

const CREATE_UNUSED_EXPORTS: &str = "
CREATE TABLE IF NOT EXISTS unused_exports (
    symbol_id INTEGER PRIMARY KEY REFERENCES symbols(id) ON DELETE CASCADE,
    file_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE
)";

const CREATE_IDX_UNUSED_EXPORTS_FILE: &str =
    "CREATE INDEX IF NOT EXISTS idx_unused_exports_file ON unused_exports(file_id)";

const CREATE_META: &str = "
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT
)";

const CREATE_IDX_FILES_PAGERANK: &str =
    "CREATE INDEX IF NOT EXISTS idx_files_pagerank ON files(pagerank DESC)";

const CREATE_IDX_SYMBOLS_FILE: &str =
    "CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_id)";

const CREATE_IDX_SYMBOLS_NAME: &str =
    "CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name)";

const CREATE_IDX_SYMBOLS_PAGERANK: &str =
    "CREATE INDEX IF NOT EXISTS idx_symbols_pagerank ON symbols(pagerank DESC)";

const CREATE_IDX_SYMBOL_REFS_FROM: &str =
    "CREATE INDEX IF NOT EXISTS idx_symbol_refs_from ON symbol_refs(from_symbol_id)";

const CREATE_IDX_SYMBOL_REFS_TO: &str =
    "CREATE INDEX IF NOT EXISTS idx_symbol_refs_to ON symbol_refs(to_symbol_id)";

const CREATE_IDX_EDGES_FROM: &str = "CREATE INDEX IF NOT EXISTS idx_edges_from ON edges(from_file)";

const CREATE_IDX_EDGES_TO: &str = "CREATE INDEX IF NOT EXISTS idx_edges_to ON edges(to_file)";

const CREATE_IDX_COCHANGES_A: &str =
    "CREATE INDEX IF NOT EXISTS idx_cochanges_a ON co_changes(file_a)";

const CREATE_IDX_COCHANGES_B: &str =
    "CREATE INDEX IF NOT EXISTS idx_cochanges_b ON co_changes(file_b)";

const CREATE_FILE_CLUSTERS: &str = "
CREATE TABLE IF NOT EXISTS file_clusters (
    file_id     INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    cluster_id  INTEGER NOT NULL,
    computed_at INTEGER NOT NULL
)";

const CREATE_IDX_FILE_CLUSTERS_CLUSTER: &str =
    "CREATE INDEX IF NOT EXISTS idx_file_clusters_cluster ON file_clusters(cluster_id)";

const CREATE_IDX_SYMBOLS_SHAPE_HASH: &str = "CREATE INDEX IF NOT EXISTS idx_symbols_shape_hash ON symbols(shape_hash) WHERE shape_hash IS NOT NULL";

const CREATE_TYPE_HIERARCHY: &str = "
CREATE TABLE IF NOT EXISTS type_hierarchy (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    sub_name    TEXT    NOT NULL,
    super_name  TEXT    NOT NULL,
    kind        TEXT    NOT NULL DEFAULT 'implements',
    line        INTEGER NOT NULL DEFAULT 0,
    UNIQUE(file_id, sub_name, super_name, kind)
)";

const CREATE_IDX_TYPE_HIERARCHY_SUB: &str =
    "CREATE INDEX IF NOT EXISTS idx_type_hierarchy_sub ON type_hierarchy(sub_name)";

const CREATE_IDX_TYPE_HIERARCHY_SUPER: &str =
    "CREATE INDEX IF NOT EXISTS idx_type_hierarchy_super ON type_hierarchy(super_name)";

pub fn create_schema(conn: &Connection) -> Result<()> {
    // Phase 1: create tables (IF NOT EXISTS). Idempotent on existing DBs.
    conn.execute_batch(
        &[
            CREATE_FILES,
            CREATE_SYMBOLS,
            CREATE_EDGES,
            CREATE_SYMBOL_REFS,
            CREATE_CO_CHANGES,
            CREATE_SYMBOLS_FTS,
            CREATE_SYMBOLS_BODY_FTS,
            CREATE_UNUSED_EXPORTS,
            CREATE_META,
            CREATE_FILE_CLUSTERS,
            CREATE_TYPE_HIERARCHY,
        ]
        .join(";\n"),
    )?;

    // Phase 2: apply ALTER TABLE migrations BEFORE we touch indexes. Legacy
    // DBs missing the new `symbols.pagerank` column would otherwise break
    // `CREATE INDEX idx_symbols_pagerank` below.
    migrate(conn)?;

    // Phase 3: create indexes — safe now that every referenced column exists.
    conn.execute_batch(
        &[
            CREATE_IDX_FILES_PAGERANK,
            CREATE_IDX_SYMBOLS_FILE,
            CREATE_IDX_SYMBOLS_NAME,
            CREATE_IDX_SYMBOLS_PAGERANK,
            CREATE_IDX_SYMBOL_REFS_FROM,
            CREATE_IDX_SYMBOL_REFS_TO,
            CREATE_IDX_EDGES_FROM,
            CREATE_IDX_EDGES_TO,
            CREATE_IDX_COCHANGES_A,
            CREATE_IDX_COCHANGES_B,
            CREATE_IDX_UNUSED_EXPORTS_FILE,
            CREATE_IDX_FILE_CLUSTERS_CLUSTER,
            CREATE_IDX_SYMBOLS_SHAPE_HASH,
            CREATE_IDX_TYPE_HIERARCHY_SUB,
            CREATE_IDX_TYPE_HIERARCHY_SUPER,
        ]
        .join(";\n"),
    )?;

    Ok(())
}

/// Run an `ALTER TABLE … ADD COLUMN` statement, ignoring the expected
/// "duplicate column name" error from SQLite when the column already exists.
/// Any other error (disk full, corruption, read-only database) is propagated.
fn try_add_column(conn: &Connection, sql: &str) -> Result<()> {
    match conn.execute(sql, []) {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("duplicate column name") => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Apply idempotent schema migrations so existing `.qartez/index.db` files
/// (created before a column was added) pick up new columns on the next open.
/// Each `ALTER TABLE ADD COLUMN` is expected to fail with "duplicate column"
/// when the migration has already run — that specific error is ignored.
/// All other errors (disk full, corruption, read-only) are propagated.
fn migrate(conn: &Connection) -> Result<()> {
    // shape_hash is in the original CREATE_SYMBOLS but legacy DBs created
    // before that column existed need the migration.
    try_add_column(conn, "ALTER TABLE symbols ADD COLUMN shape_hash TEXT")?;
    try_add_column(conn, "ALTER TABLE symbols ADD COLUMN parent_id INTEGER")?;
    try_add_column(
        conn,
        "ALTER TABLE symbols ADD COLUMN unused_excluded INTEGER NOT NULL DEFAULT 0",
    )?;
    // Symbol-level PageRank column. Added in the symbol-PageRank work so
    // existing DBs written by older binaries pick up the column without a
    // manual reindex. The column defaults to 0.0 so a DB that predates the
    // resolver still reads cleanly via `row_to_symbol`.
    try_add_column(
        conn,
        "ALTER TABLE symbols ADD COLUMN pagerank REAL NOT NULL DEFAULT 0.0",
    )?;
    // Per-symbol cyclomatic complexity. NULL for non-function symbols or
    // languages that do not extract control-flow information.
    try_add_column(conn, "ALTER TABLE symbols ADD COLUMN complexity INTEGER")?;
    // Owner type for methods extracted from `impl Foo { fn bar() }` blocks.
    // NULL for free functions and top-level items.
    try_add_column(conn, "ALTER TABLE symbols ADD COLUMN owner_type TEXT")?;
    // Per-file git change count — how many commits touched this file within
    // the configured analysis window. Defaults to 0 for non-git repos or
    // files that appear only in the working tree.
    try_add_column(
        conn,
        "ALTER TABLE files ADD COLUMN change_count INTEGER NOT NULL DEFAULT 0",
    )?;
    // symbols_body_fts used to be declared as `content=''` (contentless),
    // which rejects plain `DELETE FROM`. The one-time migration drops and
    // recreates the table so `rebuild_symbol_bodies` can repopulate it.
    //
    // Critically, this migration must NOT run when the table is already in
    // the new shape: `open_db` is called on every tool invocation, and an
    // unconditional drop wipes the body FTS between indexing runs,
    // silently breaking every consumer that relies on body text search
    // (qartez_refs / qartez_rename fallback scans, benchmark scoring).
    //
    // We detect the legacy shape by looking for `content=''` in the stored
    // CREATE statement; everything else is already current.
    let current_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='symbols_body_fts'",
            [],
            |row| row.get(0),
        )
        .ok();
    let needs_recreate = match current_sql.as_deref() {
        Some(sql) => sql.contains("content=''") || sql.contains("content = ''"),
        None => true,
    };
    if needs_recreate {
        let _ = conn.execute("DROP TABLE IF EXISTS symbols_body_fts", []);
        let _ = conn.execute(CREATE_SYMBOLS_BODY_FTS, []);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn in_memory_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn
    }

    #[test]
    fn test_create_schema_succeeds() {
        let conn = in_memory_conn();
        create_schema(&conn).unwrap();
    }

    #[test]
    fn test_create_schema_idempotent() {
        let conn = in_memory_conn();
        create_schema(&conn).unwrap();
        create_schema(&conn).unwrap();
    }

    #[test]
    fn test_tables_exist_after_creation() {
        let conn = in_memory_conn();
        create_schema(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"files".to_string()));
        assert!(tables.contains(&"symbols".to_string()));
        assert!(tables.contains(&"edges".to_string()));
        assert!(tables.contains(&"symbol_refs".to_string()));
        assert!(tables.contains(&"co_changes".to_string()));
        assert!(tables.contains(&"meta".to_string()));
        assert!(tables.contains(&"file_clusters".to_string()));
    }

    #[test]
    fn test_symbols_pagerank_column_exists() {
        let conn = in_memory_conn();
        create_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES ('t.rs', 0, 0, 'rust', 0, 0)",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, line_start, line_end) VALUES (?1, 'f', 'function', 1, 2)",
            [file_id],
        )
        .unwrap();
        let pr: f64 = conn
            .query_row("SELECT pagerank FROM symbols WHERE name = 'f'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(pr, 0.0);
    }

    #[test]
    fn test_symbol_refs_unique_constraint() {
        let conn = in_memory_conn();
        create_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES ('t.rs', 0, 0, 'rust', 0, 0)",
            [],
        )
        .unwrap();
        let fid = conn.last_insert_rowid();
        for name in ["a", "b"] {
            conn.execute(
                "INSERT INTO symbols (file_id, name, kind, line_start, line_end) VALUES (?1, ?2, 'function', 1, 2)",
                rusqlite::params![fid, name],
            )
            .unwrap();
        }
        let a_id: i64 = conn
            .query_row("SELECT id FROM symbols WHERE name='a'", [], |r| r.get(0))
            .unwrap();
        let b_id: i64 = conn
            .query_row("SELECT id FROM symbols WHERE name='b'", [], |r| r.get(0))
            .unwrap();

        conn.execute(
            "INSERT INTO symbol_refs (from_symbol_id, to_symbol_id, kind) VALUES (?1, ?2, 'call')",
            rusqlite::params![a_id, b_id],
        )
        .unwrap();
        // Duplicate edge with the same kind must be rejected.
        let err = conn.execute(
            "INSERT INTO symbol_refs (from_symbol_id, to_symbol_id, kind) VALUES (?1, ?2, 'call')",
            rusqlite::params![a_id, b_id],
        );
        assert!(err.is_err());
        // A different kind on the same pair is fine.
        conn.execute(
            "INSERT INTO symbol_refs (from_symbol_id, to_symbol_id, kind) VALUES (?1, ?2, 'type')",
            rusqlite::params![a_id, b_id],
        )
        .unwrap();
    }

    #[test]
    fn test_create_schema_migrates_pagerank_idempotent() {
        // Simulate an existing DB that predates the pagerank column by
        // creating the table without it and then running create_schema twice.
        let conn = in_memory_conn();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS files (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                path       TEXT    NOT NULL UNIQUE,
                mtime_ns   INTEGER NOT NULL,
                size_bytes INTEGER NOT NULL,
                language   TEXT    NOT NULL,
                line_count INTEGER NOT NULL,
                pagerank   REAL    NOT NULL DEFAULT 0.0,
                indexed_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS symbols (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                name        TEXT    NOT NULL,
                kind        TEXT    NOT NULL,
                line_start  INTEGER NOT NULL,
                line_end    INTEGER NOT NULL,
                signature   TEXT,
                is_exported INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
        create_schema(&conn).unwrap();
        create_schema(&conn).unwrap();
        // Column must now exist and default to 0.0.
        conn.execute(
            "INSERT INTO files (path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES ('m.rs', 0, 0, 'rust', 0, 0)",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, line_start, line_end) VALUES (?1, 'g', 'function', 1, 2)",
            [file_id],
        )
        .unwrap();
        let pr: f64 = conn
            .query_row("SELECT pagerank FROM symbols WHERE name = 'g'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(pr, 0.0);
    }

    #[test]
    fn test_fts_table_exists() {
        let conn = in_memory_conn();
        create_schema(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='symbols_fts'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert_eq!(tables.len(), 1);
    }

    #[test]
    fn test_foreign_keys_cascade_delete() {
        let conn = in_memory_conn();
        create_schema(&conn).unwrap();

        conn.execute(
            "INSERT INTO files (path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES ('test.rs', 0, 100, 'rust', 10, 1000)",
            [],
        )
        .unwrap();

        let file_id: i64 = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, line_start, line_end)
             VALUES (?1, 'main', 'function', 1, 10)",
            [file_id],
        )
        .unwrap();

        let sym_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sym_count, 1);

        conn.execute("DELETE FROM files WHERE id = ?1", [file_id])
            .unwrap();

        let sym_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sym_count, 0);
    }
}
