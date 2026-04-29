//! `GET /api/symbol-cochanges` - file-level co-change partners of a
//! symbol's defining file, fanned out to the symbols those partner files
//! contain.
//!
//! Reads `<project_root>/.qartez/index.db`, resolves the file the target
//! symbol lives in, then walks `co_changes` in both directions to gather
//! every partner file with `count >= 2`. Each partner contributes its
//! symbols, ordered by the file-level `count` first and then by
//! `symbols.pagerank` so the strongest signals surface to the top of the
//! "Frequently changes with" panel.
//!
//! The endpoint returns at most 10 rows; an 11th row is fetched solely to
//! drive the `truncated` flag without doing a second `COUNT(*)`.
//!
//! Older index DBs predate the `co_changes` table. In that case the
//! handler returns an empty list with `truncated = false` rather than 500
//! so the dashboard can stay live during a partial migration.

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Maximum number of partner symbols returned. Matches the size of the
/// dashboard's "Frequently changes with" panel; one extra row is fetched
/// to populate the `truncated` flag.
const RESULT_LIMIT: usize = 10;

/// Minimum file-level cochange count to surface. Single shared commits
/// are too noisy at scale and would drown structural signal; the
/// threshold matches `/api/graph` and the `qartez_cochange` MCP tool.
const MIN_COCHANGE_COUNT: i64 = 2;

/// Query string for `GET /api/symbol-cochanges?id=<symbol_id>`.
#[derive(Debug, Deserialize)]
pub struct SymbolCochangesQuery {
    /// Primary key from `symbols.id`. Required.
    pub id: Option<i64>,
}

/// One partner symbol that lives in a file co-changing with the target's
/// defining file. `count` is the file-level cochange count, fanned out to
/// every symbol in the partner file.
#[derive(Debug, Serialize)]
pub struct CochangeNeighbor {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub line_start: i64,
    pub count: i64,
}

/// Response body for `GET /api/symbol-cochanges`.
#[derive(Debug, Serialize)]
pub struct SymbolCochangesResponse {
    pub cochanges: Vec<CochangeNeighbor>,
    pub truncated: bool,
}

/// JSON error envelope returned on 400 / 404 / 500.
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: &'static str,
}

/// Handle `GET /api/symbol-cochanges?id=<symbol_id>`.
///
/// # Errors
///
/// - `400 Bad Request` when `id` is missing or non-numeric.
/// - `404 Not Found` when the index DB is missing or the symbol id does
///   not exist.
/// - `500 Internal Server Error` when the spawn-blocking task panics or
///   SQLite returns an error.
pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<SymbolCochangesQuery>,
) -> Result<Json<SymbolCochangesResponse>, (StatusCode, Json<ApiError>)> {
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
        tokio::task::spawn_blocking(move || compute_symbol_cochanges_at_root(&root, symbol_id))
            .await
            .map_err(|error| {
                tracing::error!(?error, "symbol_cochanges.join.failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError {
                        error: "join error",
                    }),
                )
            })?;

    match result {
        Ok(Some(response)) => Ok(Json(response)),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "symbol not found in index",
            }),
        )),
        Err(error) => {
            tracing::error!(?error, "symbol_cochanges.query.failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError { error: "internal" }),
            ))
        }
    }
}

fn compute_symbol_cochanges_at_root(
    root: &Path,
    symbol_id: i64,
) -> anyhow::Result<Option<SymbolCochangesResponse>> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(&db_path)?;
    compute_symbol_cochanges(&conn, symbol_id)
}

/// Compute the symbol-cochanges payload from an open SQLite connection.
///
/// Factored out so unit tests can drive it against an in-memory DB
/// without spinning up the HTTP layer.
pub(crate) fn compute_symbol_cochanges(
    conn: &Connection,
    symbol_id: i64,
) -> anyhow::Result<Option<SymbolCochangesResponse>> {
    let file_id: Option<i64> = conn
        .query_row(
            "SELECT file_id FROM symbols WHERE id = ?1",
            params![symbol_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(file_id) = file_id else {
        return Ok(None);
    };

    if !table_exists(conn, "co_changes")? {
        tracing::debug!("symbol_cochanges.table_missing");
        return Ok(Some(SymbolCochangesResponse {
            cochanges: Vec::new(),
            truncated: false,
        }));
    }

    let neighbors = load_partner_symbols(conn, file_id)?;
    let truncated = neighbors.len() > RESULT_LIMIT;
    let mut cochanges = neighbors;
    cochanges.truncate(RESULT_LIMIT);

    Ok(Some(SymbolCochangesResponse {
        cochanges,
        truncated,
    }))
}

fn load_partner_symbols(conn: &Connection, file_id: i64) -> anyhow::Result<Vec<CochangeNeighbor>> {
    #[expect(
        clippy::cast_possible_wrap,
        reason = "RESULT_LIMIT is small constant, fits in i64 comfortably"
    )]
    let fetch_cap = (RESULT_LIMIT as i64) + 1;

    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.kind, f.path, s.line_start, cc.count
         FROM co_changes cc
         JOIN files f ON f.id = (CASE WHEN cc.file_a = ?1 THEN cc.file_b ELSE cc.file_a END)
         JOIN symbols s ON s.file_id = f.id
         WHERE (cc.file_a = ?1 OR cc.file_b = ?1) AND cc.count >= ?2
         ORDER BY cc.count DESC, s.pagerank DESC, s.id ASC
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![file_id, MIN_COCHANGE_COUNT, fetch_cap], |r| {
        Ok(CochangeNeighbor {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            file_path: r.get(3)?,
            line_start: r.get(4)?,
            count: r.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn table_exists(conn: &Connection, table: &str) -> anyhow::Result<bool> {
    let exists: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
            params![table],
            |r| r.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

fn default_db_path(root: &Path) -> PathBuf {
    root.join(".qartez").join("index.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Minimal subset of the `qartez-mcp` schema needed to drive the
    /// symbol-cochanges queries. Mirrors `qartez-public/src/storage/schema.rs`
    /// for the tables we read.
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
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            name       TEXT    NOT NULL,
            kind       TEXT    NOT NULL,
            line_start INTEGER NOT NULL,
            line_end   INTEGER NOT NULL,
            pagerank   REAL    NOT NULL DEFAULT 0.0
        );
        CREATE TABLE co_changes (
            id     INTEGER PRIMARY KEY AUTOINCREMENT,
            file_a INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            file_b INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            count  INTEGER NOT NULL DEFAULT 1,
            UNIQUE(file_a, file_b)
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

    fn insert_symbol(
        conn: &Connection,
        id: i64,
        file_id: i64,
        name: &str,
        line_start: i64,
        pagerank: f64,
    ) {
        conn.execute(
            "INSERT INTO symbols (id, file_id, name, kind, line_start, line_end, pagerank)
             VALUES (?1, ?2, ?3, 'function', ?4, ?5, ?6)",
            params![id, file_id, name, line_start, line_start + 5, pagerank],
        )
        .expect("insert symbol");
    }

    fn insert_cochange(conn: &Connection, file_a: i64, file_b: i64, count: i64) {
        conn.execute(
            "INSERT INTO co_changes (file_a, file_b, count) VALUES (?1, ?2, ?3)",
            params![file_a, file_b, count],
        )
        .expect("insert cochange");
    }

    #[test]
    fn returns_404_when_symbol_id_missing() {
        let conn = fresh_db();
        let result = compute_symbol_cochanges(&conn, 999).expect("query ok");
        assert!(result.is_none(), "missing id must map to None / 404");
    }

    #[test]
    fn returns_empty_when_no_cochanges() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs");
        insert_symbol(&conn, 100, 1, "target", 10, 0.5);

        let response = compute_symbol_cochanges(&conn, 100)
            .expect("query ok")
            .expect("symbol present");
        assert!(response.cochanges.is_empty());
        assert!(!response.truncated);
    }

    #[test]
    fn orders_by_count_desc_and_returns_top_10() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/target.rs");
        insert_symbol(&conn, 100, 1, "target", 10, 0.9);

        // 12 partner files, each contributes one symbol, varying cochange counts.
        for i in 0_i64..12 {
            let file_id = 100 + i;
            let path = format!("src/partner_{i}.rs");
            insert_file(&conn, file_id, &path);
            let sym_id = 1000 + i;
            let name = format!("partner_sym_{i}");
            insert_symbol(&conn, sym_id, file_id, &name, 1, 0.1);
            // i=0 gets count=2 (lowest), i=11 gets count=13 (highest).
            insert_cochange(&conn, 1, file_id, 2 + i);
        }

        let response = compute_symbol_cochanges(&conn, 100)
            .expect("query ok")
            .expect("symbol present");

        assert_eq!(response.cochanges.len(), 10, "must clamp to top 10");
        assert!(response.truncated, "had 12 partners, must report truncated");

        // Verify count DESC ordering: first row is count=13 (i=11), last
        // kept row is count=4 (i=2). The two lowest (count=2 i=0, count=3
        // i=1) must be omitted.
        assert_eq!(response.cochanges[0].count, 13);
        assert_eq!(response.cochanges[0].name, "partner_sym_11");
        assert_eq!(response.cochanges[9].count, 4);
        assert_eq!(response.cochanges[9].name, "partner_sym_2");
    }

    #[test]
    fn cochanges_table_missing_returns_empty_gracefully() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs");
        insert_symbol(&conn, 100, 1, "target", 10, 0.5);
        conn.execute_batch("DROP TABLE co_changes")
            .expect("drop co_changes");

        let response = compute_symbol_cochanges(&conn, 100)
            .expect("query ok")
            .expect("symbol present");
        assert!(response.cochanges.is_empty());
        assert!(!response.truncated);
    }

    #[test]
    fn ignores_partner_files_with_count_below_threshold() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/target.rs");
        insert_symbol(&conn, 100, 1, "target", 10, 0.5);
        insert_file(&conn, 2, "src/noise.rs");
        insert_symbol(&conn, 200, 2, "noise_sym", 1, 0.5);
        insert_cochange(&conn, 1, 2, 1);

        let response = compute_symbol_cochanges(&conn, 100)
            .expect("query ok")
            .expect("symbol present");
        assert!(
            response.cochanges.is_empty(),
            "count=1 partners must be filtered out"
        );
    }
}

// Rust guideline compliant 2026-04-26
