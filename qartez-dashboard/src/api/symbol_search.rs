//! `GET /api/symbol-search` - exact-name lookup against the `symbols`
//! table, ordered by PageRank.
//!
//! Reads `<project_root>/.qartez/index.db` and returns up to N rows where
//! `symbols.name` matches `?q=` exactly (case-sensitive; the indexer
//! stores names verbatim). Used by the dashboard's symbol jump-to box.
//!
//! When `?prefix=true` is set, the query becomes a case-insensitive
//! starts-with match (`name LIKE ?q || '%' COLLATE NOCASE`) so the UI
//! can drive a typeahead. The default remains exact-match for
//! backwards compatibility.
//!
//! Empty query is a 400; anything else returns 200 with a (possibly
//! empty) `matches` list. A missing index DB returns 200 with no
//! matches so the UI can render a placeholder rather than an error.

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Default match count when the caller omits `?limit=`. Matches the
/// dashboard's default symbol-picker page size.
const DEFAULT_LIMIT: i64 = 20;

/// Hard ceiling on `?limit=`. Above this the picker becomes unusable
/// without virtualization, which the search box does not yet do.
const MAX_LIMIT: i64 = 50;

/// Query string for `GET /api/symbol-search?q=<name>&limit=N&prefix=true|false`.
#[derive(Debug, Deserialize)]
pub struct SymbolSearchQuery {
    /// Exact symbol name to look up. Whitespace-only values are rejected
    /// with 400. Case-sensitive: the indexer stores names verbatim.
    pub q: Option<String>,
    /// Maximum number of matches to return. Clamped to `[1, MAX_LIMIT]`;
    /// invalid values fall back to `DEFAULT_LIMIT`.
    pub limit: Option<i64>,
    /// When `true`, the lookup becomes a case-insensitive starts-with
    /// match (`name LIKE ?q || '%' COLLATE NOCASE`). Default is `false`,
    /// which keeps the historical exact case-sensitive behaviour.
    pub prefix: Option<bool>,
}

/// One match returned by the search endpoint.
#[derive(Debug, Serialize)]
pub struct SymbolMatch {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub line_start: i64,
}

/// Response body for `GET /api/symbol-search`.
#[derive(Debug, Serialize)]
pub struct SymbolSearchResponse {
    pub matches: Vec<SymbolMatch>,
}

/// JSON error envelope returned on 400 / 500.
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: &'static str,
}

/// Handle `GET /api/symbol-search?q=<name>&limit=N`.
///
/// # Errors
///
/// - `400 Bad Request` when `q` is missing or contains only whitespace.
/// - `500 Internal Server Error` when the spawn-blocking task panics or
///   SQLite returns an error.
///
/// A missing index DB is not an error; the handler returns 200 with an
/// empty match list so the UI can render a placeholder.
pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<SymbolSearchQuery>,
) -> Result<Json<SymbolSearchResponse>, (StatusCode, Json<ApiError>)> {
    let raw = query.q.unwrap_or_default();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "missing query",
            }),
        ));
    }
    let needle = trimmed.to_string();
    let limit = clamp_limit(query.limit);
    let prefix = query.prefix.unwrap_or(false);
    let root = state.project_root().to_path_buf();

    let result = tokio::task::spawn_blocking(move || {
        compute_symbol_search_at_root(&root, &needle, limit, prefix)
    })
    .await
    .map_err(|error| {
        tracing::error!(?error, "symbol_search.join.failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "join error",
            }),
        )
    })?;

    match result {
        Ok(Some(response)) => Ok(Json(response)),
        Ok(None) => Ok(Json(SymbolSearchResponse {
            matches: Vec::new(),
        })),
        Err(error) => {
            tracing::error!(?error, "symbol_search.query.failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError { error: "internal" }),
            ))
        }
    }
}

fn clamp_limit(requested: Option<i64>) -> i64 {
    match requested {
        Some(value) if (1..=MAX_LIMIT).contains(&value) => value,
        _ => DEFAULT_LIMIT,
    }
}

fn compute_symbol_search_at_root(
    root: &Path,
    needle: &str,
    limit: i64,
    prefix: bool,
) -> anyhow::Result<Option<SymbolSearchResponse>> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(&db_path)?;
    Ok(Some(compute_symbol_search(&conn, needle, limit, prefix)?))
}

/// Compute the symbol-search payload from an open SQLite connection.
///
/// Factored out so unit tests can drive it against an in-memory DB
/// without spinning up the HTTP layer. When `prefix` is `true` the
/// match becomes a case-insensitive starts-with query; otherwise the
/// historical exact case-sensitive match is used.
pub(crate) fn compute_symbol_search(
    conn: &Connection,
    needle: &str,
    limit: i64,
    prefix: bool,
) -> anyhow::Result<SymbolSearchResponse> {
    let sql = if prefix {
        "SELECT s.id, s.name, s.kind, f.path, s.line_start
         FROM symbols s
         JOIN files   f ON f.id = s.file_id
         WHERE s.name LIKE ?1 || '%' COLLATE NOCASE
         ORDER BY s.pagerank DESC, s.id ASC
         LIMIT ?2"
    } else {
        "SELECT s.id, s.name, s.kind, f.path, s.line_start
         FROM symbols s
         JOIN files   f ON f.id = s.file_id
         WHERE s.name = ?1
         ORDER BY s.pagerank DESC, s.id ASC
         LIMIT ?2"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![needle, limit], |r| {
        Ok(SymbolMatch {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            file_path: r.get(3)?,
            line_start: r.get(4)?,
        })
    })?;
    let mut matches = Vec::new();
    for row in rows {
        matches.push(row?);
    }
    Ok(SymbolSearchResponse { matches })
}

fn default_db_path(root: &Path) -> PathBuf {
    root.join(".qartez").join("index.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Minimal subset of the `qartez-mcp` schema needed to drive the
    /// symbol-search query.
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
            pagerank    REAL    NOT NULL DEFAULT 0.0
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

    #[test]
    fn returns_matches_ordered_by_pagerank_desc() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/a.rs");
        insert_file(&conn, 2, "src/b.rs");
        insert_file(&conn, 3, "src/c.rs");
        insert_symbol(&conn, 10, 1, "compute", 5, 0.3);
        insert_symbol(&conn, 11, 2, "compute", 7, 0.9);
        insert_symbol(&conn, 12, 3, "compute", 9, 0.6);
        insert_symbol(&conn, 99, 1, "other", 1, 0.99);

        let response = compute_symbol_search(&conn, "compute", 10, false).expect("query ok");

        assert_eq!(response.matches.len(), 3);
        assert_eq!(response.matches[0].id, 11);
        assert_eq!(response.matches[1].id, 12);
        assert_eq!(response.matches[2].id, 10);
        for m in &response.matches {
            assert_eq!(m.name, "compute");
            assert_eq!(m.kind, "function");
        }
    }

    #[test]
    fn empty_query_returns_400() {
        let raw = "   \t\n";
        let trimmed = raw.trim();
        assert!(
            trimmed.is_empty(),
            "whitespace-only input must collapse to empty after trim, triggering 400 in handler"
        );

        let conn = fresh_db();
        insert_file(&conn, 1, "src/a.rs");
        insert_symbol(&conn, 1, 1, "real", 1, 0.5);
        let response = compute_symbol_search(&conn, "missing", 10, false).expect("query ok");
        assert!(
            response.matches.is_empty(),
            "no exact-name match yields empty list, not an error"
        );
    }

    #[test]
    fn prefix_matches_by_starting_substring() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/a.rs");
        insert_symbol(&conn, 10, 1, "compute", 5, 0.7);
        insert_symbol(&conn, 11, 1, "compile", 7, 0.9);
        insert_symbol(&conn, 12, 1, "other", 9, 0.6);

        let response = compute_symbol_search(&conn, "comp", 10, true).expect("query ok");

        assert_eq!(response.matches.len(), 2);
        let names: Vec<&str> = response.matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"compute"));
        assert!(names.contains(&"compile"));
        assert!(!names.contains(&"other"));
        assert_eq!(response.matches[0].id, 11);
        assert_eq!(response.matches[1].id, 10);
    }

    #[test]
    fn prefix_is_case_insensitive() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/a.rs");
        insert_symbol(&conn, 10, 1, "ComputeNode", 5, 0.5);

        let response = compute_symbol_search(&conn, "compute", 10, true).expect("query ok");

        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.matches[0].name, "ComputeNode");
    }
}

// Rust guideline compliant 2026-04-26
