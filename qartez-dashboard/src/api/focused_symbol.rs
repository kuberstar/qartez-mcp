//! `GET /api/focused-symbol` - per-symbol detail card with top callers and
//! callees from the `symbol_refs` graph.
//!
//! Reads `<project_root>/.qartez/index.db`, joins `symbols`, `symbol_refs`,
//! and `files`, and returns the data the focused-symbol card needs to
//! render: name, kind, signature, source location, complexity, the top 5
//! callers and callees by PageRank, and the total incoming reference
//! count (across all `symbol_refs.kind` values).
//!
//! Unlike `/api/focused-file`, this endpoint takes a numeric symbol id
//! rather than a path so the UI can link directly from the symbol-graph
//! response without round-tripping through name resolution.

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Maximum number of callers and callees returned in each direction.
/// Five matches the dashboard's "top neighbors" framing without flooding
/// the side panel for hub symbols with hundreds of references.
const NEIGHBOR_LIMIT: i64 = 5;

/// Query string for `GET /api/focused-symbol?id=<symbol_id>`.
#[derive(Debug, Deserialize)]
pub struct FocusedSymbolQuery {
    /// Primary key from `symbols.id`. Required.
    pub id: Option<i64>,
}

/// One caller or callee neighbor: a symbol that references (or is
/// referenced by) the focused symbol.
#[derive(Debug, Serialize)]
pub struct Neighbor {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub line_start: i64,
}

/// Response body for `GET /api/focused-symbol`.
#[derive(Debug, Serialize)]
pub struct FocusedSymbolResponse {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub signature: Option<String>,
    pub file_path: String,
    pub line_start: i64,
    pub line_end: i64,
    pub complexity: Option<i64>,
    pub callers: Vec<Neighbor>,
    pub callees: Vec<Neighbor>,
    pub reference_count: i64,
}

/// JSON error envelope returned on 400 / 404 / 500.
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: &'static str,
}

/// Raw row read from the `symbols` join. Stays private; callers
/// destructure it into the response struct.
struct SymbolRow {
    id: i64,
    name: String,
    kind: String,
    signature: Option<String>,
    line_start: i64,
    line_end: i64,
    file_path: String,
    complexity: Option<i64>,
}

/// Handle `GET /api/focused-symbol?id=<symbol_id>`.
///
/// # Errors
///
/// - `400 Bad Request` when `id` is missing or non-numeric.
/// - `404 Not Found` when the index DB is missing or contains no row for
///   the requested id.
/// - `500 Internal Server Error` when the spawn-blocking task panics or
///   SQLite returns an error.
pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<FocusedSymbolQuery>,
) -> Result<Json<FocusedSymbolResponse>, (StatusCode, Json<ApiError>)> {
    let Some(symbol_id) = query.id else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "missing id",
            }),
        ));
    };

    let root = state.project_root().to_path_buf();

    let result =
        tokio::task::spawn_blocking(move || compute_focused_symbol_at_root(&root, symbol_id))
            .await
            .map_err(|error| {
                tracing::error!(?error, "focused_symbol.join.failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError {
                        error: "join error",
                    }),
                )
            })?;

    match result {
        Ok(Some(symbol)) => Ok(Json(symbol)),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "symbol not found in index",
            }),
        )),
        Err(error) => {
            tracing::error!(?error, "focused_symbol.query.failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError { error: "internal" }),
            ))
        }
    }
}

fn compute_focused_symbol_at_root(
    root: &Path,
    symbol_id: i64,
) -> anyhow::Result<Option<FocusedSymbolResponse>> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(&db_path)?;
    compute_focused_symbol(&conn, symbol_id)
}

/// Compute the focused-symbol payload from an open SQLite connection.
///
/// Factored out so unit tests can drive it against an in-memory DB
/// without spinning up the HTTP layer.
pub(crate) fn compute_focused_symbol(
    conn: &Connection,
    symbol_id: i64,
) -> anyhow::Result<Option<FocusedSymbolResponse>> {
    let has_complexity = column_exists(conn, "symbols", "complexity")?;
    let sql = if has_complexity {
        "SELECT s.id, s.name, s.kind, s.signature, s.line_start, s.line_end,
                f.path, s.complexity
         FROM symbols s
         JOIN files f ON f.id = s.file_id
         WHERE s.id = ?1"
    } else {
        "SELECT s.id, s.name, s.kind, s.signature, s.line_start, s.line_end,
                f.path, NULL
         FROM symbols s
         JOIN files f ON f.id = s.file_id
         WHERE s.id = ?1"
    };

    let row: Option<SymbolRow> = conn
        .query_row(sql, params![symbol_id], |r| {
            Ok(SymbolRow {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                signature: r.get(3)?,
                line_start: r.get(4)?,
                line_end: r.get(5)?,
                file_path: r.get(6)?,
                complexity: r.get(7)?,
            })
        })
        .optional()?;

    let Some(SymbolRow {
        id,
        name,
        kind,
        signature,
        line_start,
        line_end,
        file_path,
        complexity,
    }) = row
    else {
        return Ok(None);
    };

    let callers = load_callers(conn, id)?;
    let callees = load_callees(conn, id)?;
    let reference_count = load_reference_count(conn, id)?;

    Ok(Some(FocusedSymbolResponse {
        id,
        name,
        kind,
        signature,
        file_path,
        line_start,
        line_end,
        complexity,
        callers,
        callees,
        reference_count,
    }))
}

fn load_callers(conn: &Connection, symbol_id: i64) -> anyhow::Result<Vec<Neighbor>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.kind, f.path, s.line_start
         FROM symbol_refs sr
         JOIN symbols s ON s.id = sr.from_symbol_id
         JOIN files   f ON f.id = s.file_id
         WHERE sr.to_symbol_id = ?1 AND sr.kind = 'call'
         ORDER BY s.pagerank DESC, s.id ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![symbol_id, NEIGHBOR_LIMIT], |r| {
        Ok(Neighbor {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            file_path: r.get(3)?,
            line_start: r.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn load_callees(conn: &Connection, symbol_id: i64) -> anyhow::Result<Vec<Neighbor>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.kind, f.path, s.line_start
         FROM symbol_refs sr
         JOIN symbols s ON s.id = sr.to_symbol_id
         JOIN files   f ON f.id = s.file_id
         WHERE sr.from_symbol_id = ?1 AND sr.kind = 'call'
         ORDER BY s.pagerank DESC, s.id ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![symbol_id, NEIGHBOR_LIMIT], |r| {
        Ok(Neighbor {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            file_path: r.get(3)?,
            line_start: r.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn load_reference_count(conn: &Connection, symbol_id: i64) -> anyhow::Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM symbol_refs WHERE to_symbol_id = ?1",
        params![symbol_id],
        |r| r.get(0),
    )?;
    Ok(count)
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

fn default_db_path(root: &Path) -> PathBuf {
    root.join(".qartez").join("index.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Minimal subset of the `qartez-mcp` schema needed to drive the
    /// focused-symbol queries.
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
            pagerank    REAL    NOT NULL DEFAULT 0.0,
            complexity  INTEGER
        );
        CREATE TABLE symbol_refs (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            from_symbol_id  INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
            to_symbol_id    INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
            kind            TEXT    NOT NULL DEFAULT 'call',
            UNIQUE(from_symbol_id, to_symbol_id, kind)
        );
    ";

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(TEST_SCHEMA).expect("create schema");
        conn
    }

    fn insert_file(conn: &Connection, id: i64, path: &str) {
        conn.execute(
            "INSERT INTO files (id, path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES (?1, ?2, 0, 0, 'rust', 100, 0)",
            params![id, path],
        )
        .expect("insert file");
    }

    struct TestSymbol<'a> {
        id: i64,
        file_id: i64,
        name: &'a str,
        signature: Option<&'a str>,
        line_start: i64,
        pagerank: f64,
        complexity: Option<i64>,
    }

    fn insert_symbol(conn: &Connection, sym: &TestSymbol<'_>) {
        conn.execute(
            "INSERT INTO symbols
             (id, file_id, name, kind, line_start, line_end, signature, pagerank, complexity)
             VALUES (?1, ?2, ?3, 'function', ?4, ?5, ?6, ?7, ?8)",
            params![
                sym.id,
                sym.file_id,
                sym.name,
                sym.line_start,
                sym.line_start + 10,
                sym.signature,
                sym.pagerank,
                sym.complexity
            ],
        )
        .expect("insert symbol");
    }

    fn insert_ref_kind(conn: &Connection, from: i64, to: i64, kind: &str) {
        conn.execute(
            "INSERT INTO symbol_refs (from_symbol_id, to_symbol_id, kind)
             VALUES (?1, ?2, ?3)",
            params![from, to, kind],
        )
        .expect("insert symbol_ref");
    }

    fn insert_ref(conn: &Connection, from: i64, to: i64) {
        insert_ref_kind(conn, from, to, "call");
    }

    #[test]
    fn returns_404_when_symbol_id_missing() {
        let conn = fresh_db();
        let result = compute_focused_symbol(&conn, 42).expect("query ok");
        assert!(result.is_none(), "missing id must map to None / 404");
    }

    #[test]
    fn returns_callers_and_callees_with_top5_by_pagerank() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs");
        insert_symbol(
            &conn,
            &TestSymbol {
                id: 100,
                file_id: 1,
                name: "target",
                signature: Some("fn target()"),
                line_start: 50,
                pagerank: 0.9,
                complexity: Some(8),
            },
        );

        for i in 0_i64..7 {
            let id = 200 + i;
            #[expect(
                clippy::cast_precision_loss,
                reason = "small loop counter, exact float conversion is fine"
            )]
            let pr = (i as f64) * 0.1;
            let name = format!("caller_{i}");
            insert_symbol(
                &conn,
                &TestSymbol {
                    id,
                    file_id: 1,
                    name: &name,
                    signature: None,
                    line_start: 100 + i,
                    pagerank: pr,
                    complexity: None,
                },
            );
            insert_ref(&conn, id, 100);
        }

        for i in 0_i64..7 {
            let id = 300 + i;
            #[expect(
                clippy::cast_precision_loss,
                reason = "small loop counter, exact float conversion is fine"
            )]
            let pr = (i as f64) * 0.1;
            let name = format!("callee_{i}");
            insert_symbol(
                &conn,
                &TestSymbol {
                    id,
                    file_id: 1,
                    name: &name,
                    signature: None,
                    line_start: 200 + i,
                    pagerank: pr,
                    complexity: None,
                },
            );
            insert_ref(&conn, 100, id);
        }

        let focused = compute_focused_symbol(&conn, 100)
            .expect("query ok")
            .expect("symbol present");

        assert_eq!(focused.id, 100);
        assert_eq!(focused.name, "target");
        assert_eq!(focused.signature.as_deref(), Some("fn target()"));
        assert_eq!(focused.complexity, Some(8));
        assert_eq!(focused.line_start, 50);
        assert_eq!(focused.file_path, "src/lib.rs");

        assert_eq!(focused.callers.len(), 5);
        assert_eq!(focused.callees.len(), 5);

        let caller_prs: Vec<i64> = focused.callers.iter().map(|n| n.id).collect();
        assert_eq!(caller_prs, vec![206, 205, 204, 203, 202]);

        let callee_prs: Vec<i64> = focused.callees.iter().map(|n| n.id).collect();
        assert_eq!(callee_prs, vec![306, 305, 304, 303, 302]);
    }

    #[test]
    fn reference_count_uses_all_incoming_refs_regardless_of_kind() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs");
        for sym in [
            TestSymbol {
                id: 1,
                file_id: 1,
                name: "target",
                signature: None,
                line_start: 1,
                pagerank: 0.5,
                complexity: None,
            },
            TestSymbol {
                id: 2,
                file_id: 1,
                name: "a",
                signature: None,
                line_start: 10,
                pagerank: 0.4,
                complexity: None,
            },
            TestSymbol {
                id: 3,
                file_id: 1,
                name: "b",
                signature: None,
                line_start: 20,
                pagerank: 0.3,
                complexity: None,
            },
            TestSymbol {
                id: 4,
                file_id: 1,
                name: "c",
                signature: None,
                line_start: 30,
                pagerank: 0.2,
                complexity: None,
            },
        ] {
            insert_symbol(&conn, &sym);
        }

        insert_ref_kind(&conn, 2, 1, "call");
        insert_ref_kind(&conn, 3, 1, "import");
        insert_ref_kind(&conn, 4, 1, "extends");

        let focused = compute_focused_symbol(&conn, 1)
            .expect("query ok")
            .expect("symbol present");

        assert_eq!(focused.reference_count, 3);
        assert_eq!(
            focused.callers.len(),
            1,
            "callers list filters to kind='call'"
        );
    }
}

// Rust guideline compliant 2026-04-26
