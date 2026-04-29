//! `GET /api/focused-file` - per-file impact, symbol table, and dependent list.
//!
//! Reads `<project_root>/.qartez/index.db`, joins `files`, `symbols`, and
//! `edges`, and returns the data the Project Pulse focused-file card needs
//! to render: language, line count, symbols (clipped to 500 to bound payload
//! size), incoming dependents (clipped to 200), and aggregate impact counts
//! (uncapped).
//!
//! The transitive impact is computed via a recursive CTE walking the `edges`
//! table from `to_file = ?` upward through importing files, capped at depth
//! 5. Five hops covers typical fan-out without runaway traversal on
//! pathological cycles; `UNION` (not `UNION ALL`) deduplicates already-seen
//! files so cycles terminate.

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Maximum symbols returned for a single file. Bounds payload size on
/// generated files (e.g. multi-thousand-symbol vendored crates).
const SYMBOL_LIMIT: i64 = 500;

/// Maximum direct dependents returned. The full count still appears in
/// `impact.direct`; this only caps the path list rendered to the UI.
const DEPENDENT_LIMIT: i64 = 200;

/// Maximum traversal depth for the transitive impact CTE. Five hops
/// matches the dashboard's "blast radius" framing without letting deeply
/// connected hubs balloon the result set.
const TRANSITIVE_DEPTH: i64 = 5;

/// Query string for `GET /api/focused-file?path=...`.
#[derive(Debug, Deserialize)]
pub struct FocusedQuery {
    /// Repository-relative path of the file to focus on.
    pub path: String,
}

/// One row from the `symbols` table, projected for the focused-file card.
#[derive(Debug, Serialize)]
pub struct FocusedSymbol {
    pub name: String,
    pub kind: String,
    pub line_start: i64,
    pub line_end: i64,
    pub visibility: &'static str,
    /// Cyclomatic complexity from `symbols.complexity`. `None` when the
    /// row carries no value, or when the column is absent on very old
    /// index DBs that predate the migration.
    pub complexity: Option<i64>,
}

/// One incoming edge (a file that imports the focused file).
#[derive(Debug, Serialize)]
pub struct Dependent {
    pub path: String,
    pub kind: String,
}

/// Aggregate impact counts. `direct` is the unclipped count of incoming
/// edges; `transitive` walks `edges` up to `TRANSITIVE_DEPTH` hops with
/// cycle deduplication.
#[derive(Debug, Serialize)]
pub struct Impact {
    pub direct: usize,
    pub transitive: usize,
}

/// Response body for `GET /api/focused-file`.
#[derive(Debug, Serialize)]
pub struct FocusedFile {
    pub path: String,
    pub language: String,
    pub lines: i64,
    pub symbols: Vec<FocusedSymbol>,
    pub dependents: Vec<Dependent>,
    pub impact: Impact,
}

/// JSON error envelope returned on 400 / 404 / 500.
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: &'static str,
}

/// Handle `GET /api/focused-file?path=<relative_path>`.
///
/// # Errors
///
/// - `400 Bad Request` when `path` is empty or whitespace.
/// - `404 Not Found` when the index DB is missing or contains no row for
///   the requested path.
/// - `500 Internal Server Error` when the spawn-blocking task panics or
///   SQLite returns an error.
pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<FocusedQuery>,
) -> Result<Json<FocusedFile>, (StatusCode, Json<ApiError>)> {
    if query.path.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "missing path",
            }),
        ));
    }

    let root = state.project_root().to_path_buf();
    let path = query.path.clone();

    let result = tokio::task::spawn_blocking(move || compute_focused_file_at_root(&root, &path))
        .await
        .map_err(|error| {
            tracing::error!(?error, "focused_file.join.failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError {
                    error: "join error",
                }),
            )
        })?;

    match result {
        Ok(Some(file)) => Ok(Json(file)),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "file not found in index",
            }),
        )),
        Err(error) => {
            tracing::error!(?error, "focused_file.query.failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError { error: "internal" }),
            ))
        }
    }
}

fn compute_focused_file_at_root(
    root: &Path,
    rel_path: &str,
) -> anyhow::Result<Option<FocusedFile>> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(&db_path)?;
    compute_focused_file(&conn, rel_path)
}

/// Compute the focused-file payload from an open SQLite connection.
///
/// Factored out so unit tests can drive it against an in-memory or
/// `tempfile`-backed DB without spinning up the HTTP layer.
pub(crate) fn compute_focused_file(
    conn: &Connection,
    rel_path: &str,
) -> anyhow::Result<Option<FocusedFile>> {
    let row: Option<(i64, String, i64)> = conn
        .query_row(
            "SELECT id, language, line_count FROM files WHERE path = ?1",
            params![rel_path],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;

    let Some((file_id, language, lines)) = row else {
        return Ok(None);
    };

    let symbols = load_symbols(conn, file_id)?;
    let dependents = load_dependents(conn, file_id)?;
    let direct = load_direct_impact(conn, file_id)?;
    let transitive = load_transitive_impact(conn, file_id)?;

    Ok(Some(FocusedFile {
        path: rel_path.to_string(),
        language,
        lines,
        symbols,
        dependents,
        impact: Impact { direct, transitive },
    }))
}

fn load_symbols(conn: &Connection, file_id: i64) -> anyhow::Result<Vec<FocusedSymbol>> {
    let has_complexity = column_exists(conn, "symbols", "complexity")?;
    let sql = if has_complexity {
        "SELECT name, kind, line_start, line_end, is_exported, complexity
         FROM symbols
         WHERE file_id = ?1
         ORDER BY line_start
         LIMIT ?2"
    } else {
        "SELECT name, kind, line_start, line_end, is_exported, NULL
         FROM symbols
         WHERE file_id = ?1
         ORDER BY line_start
         LIMIT ?2"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![file_id, SYMBOL_LIMIT], |r| {
        let is_exported: i64 = r.get(4)?;
        Ok(FocusedSymbol {
            name: r.get(0)?,
            kind: r.get(1)?,
            line_start: r.get(2)?,
            line_end: r.get(3)?,
            visibility: if is_exported != 0 {
                "public"
            } else {
                "private"
            },
            complexity: r.get::<_, Option<i64>>(5)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
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

fn load_dependents(conn: &Connection, file_id: i64) -> anyhow::Result<Vec<Dependent>> {
    let mut stmt = conn.prepare(
        "SELECT f.path, e.kind
         FROM edges e
         JOIN files f ON f.id = e.from_file
         WHERE e.to_file = ?1
         ORDER BY f.path
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![file_id, DEPENDENT_LIMIT], |r| {
        Ok(Dependent {
            path: r.get(0)?,
            kind: r.get(1)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn load_direct_impact(conn: &Connection, file_id: i64) -> anyhow::Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM edges WHERE to_file = ?1",
        params![file_id],
        |r| r.get(0),
    )?;
    Ok(usize::try_from(count).unwrap_or(0))
}

fn load_transitive_impact(conn: &Connection, file_id: i64) -> anyhow::Result<usize> {
    let count: i64 = conn.query_row(
        "WITH RECURSIVE deps(file_id, depth) AS (
             SELECT from_file, 1 FROM edges WHERE to_file = ?1
             UNION
             SELECT e.from_file, d.depth + 1
             FROM edges e
             JOIN deps d ON e.to_file = d.file_id
             WHERE d.depth < ?2
         )
         SELECT COUNT(DISTINCT file_id) FROM deps",
        params![file_id, TRANSITIVE_DEPTH],
        |r| r.get(0),
    )?;
    Ok(usize::try_from(count).unwrap_or(0))
}

fn default_db_path(root: &Path) -> PathBuf {
    root.join(".qartez").join("index.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Minimal subset of the `qartez-mcp` schema needed to drive the
    /// focused-file queries. Mirrors `qartez-public/src/storage/schema.rs`
    /// for the three tables we read; intentionally omits FTS, indexes,
    /// and unrelated tables to keep the fixture readable.
    const TEST_SCHEMA: &str = "
        CREATE TABLE files (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            path       TEXT    NOT NULL UNIQUE,
            mtime_ns   INTEGER NOT NULL,
            size_bytes INTEGER NOT NULL,
            language   TEXT    NOT NULL,
            line_count INTEGER NOT NULL,
            pagerank   REAL    NOT NULL DEFAULT 0.0,
            indexed_at INTEGER NOT NULL
        );
        CREATE TABLE symbols (
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
            pagerank    REAL    NOT NULL DEFAULT 0.0,
            complexity  INTEGER
        );
        CREATE TABLE edges (
            id        INTEGER PRIMARY KEY AUTOINCREMENT,
            from_file INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            to_file   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            kind      TEXT    NOT NULL DEFAULT 'import',
            specifier TEXT,
            UNIQUE(from_file, to_file, kind)
        );
    ";

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(TEST_SCHEMA).expect("create schema");
        conn
    }

    #[test]
    fn responds_with_404_when_path_missing() {
        let conn = fresh_db();
        let result = compute_focused_file(&conn, "src/lib.rs").expect("query ok");
        assert!(result.is_none(), "no row should map to None / 404");
    }

    #[test]
    fn returns_symbols_and_impact_for_indexed_file() {
        let conn = fresh_db();

        conn.execute(
            "INSERT INTO files (id, path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES (1, 'src/lib.rs', 0, 0, 'rust', 42, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (id, path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES (2, 'src/main.rs', 0, 0, 'rust', 30, 0)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO symbols
             (id, file_id, name, kind, line_start, line_end, signature, is_exported)
             VALUES (1, 1, 'foo', 'function', 10, 20, 'pub fn foo()', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols
             (id, file_id, name, kind, line_start, line_end, signature, is_exported)
             VALUES (2, 1, 'helper', 'function', 22, 30, 'fn helper()', 0)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO edges (from_file, to_file, kind, specifier)
             VALUES (2, 1, 'import', 'crate::lib')",
            [],
        )
        .unwrap();

        let focused = compute_focused_file(&conn, "src/lib.rs")
            .expect("query ok")
            .expect("file row present");

        assert_eq!(focused.path, "src/lib.rs");
        assert_eq!(focused.language, "rust");
        assert_eq!(focused.lines, 42);

        assert_eq!(focused.symbols.len(), 2);
        let foo = focused
            .symbols
            .iter()
            .find(|s| s.name == "foo")
            .expect("foo present");
        assert_eq!(foo.kind, "function");
        assert_eq!(foo.line_start, 10);
        assert_eq!(foo.line_end, 20);
        assert_eq!(foo.visibility, "public");

        let helper = focused
            .symbols
            .iter()
            .find(|s| s.name == "helper")
            .expect("helper present");
        assert_eq!(helper.visibility, "private");

        assert_eq!(
            foo.complexity, None,
            "complexity must be None when not inserted"
        );
        assert_eq!(
            helper.complexity, None,
            "complexity must be None when not inserted"
        );

        assert_eq!(focused.dependents.len(), 1);
        assert_eq!(focused.dependents[0].path, "src/main.rs");
        assert_eq!(focused.dependents[0].kind, "import");

        assert_eq!(focused.impact.direct, 1);
        assert_eq!(focused.impact.transitive, 1);
    }

    #[test]
    fn complexity_is_round_tripped_when_present() {
        let conn = fresh_db();

        conn.execute(
            "INSERT INTO files (id, path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES (1, 'src/lib.rs', 0, 0, 'rust', 42, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols
             (id, file_id, name, kind, line_start, line_end, signature, is_exported, complexity)
             VALUES (1, 1, 'busy', 'function', 5, 25, 'fn busy()', 0, 7)",
            [],
        )
        .unwrap();

        let focused = compute_focused_file(&conn, "src/lib.rs")
            .expect("query ok")
            .expect("file row present");

        assert_eq!(focused.symbols.len(), 1);
        assert_eq!(focused.symbols[0].complexity, Some(7));
    }
}

// Rust guideline compliant 2026-04-26
