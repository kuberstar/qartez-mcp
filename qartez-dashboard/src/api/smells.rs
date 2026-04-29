//! `GET /api/smells` - god functions and long-parameter signatures.
//!
//! Two complementary detectors over `symbols`:
//!
//! - **God functions:** `complexity >= MIN_CC` and body length
//!   `>= MIN_LINES` (defaults 15 / 50, matching `qartez_smells`).
//! - **Long params:** signature parses to `>= MIN_PARAMS` parameters
//!   (default 5). The parser mirrors `count_signature_params` from
//!   `qartez_smells` so the dashboard reports the same offenders.
//!
//! When `complexity` is missing on the index DB (very old build) the
//! god-function detector returns an empty list rather than 500. The
//! long-params detector relies only on `signature`, which has been
//! present since the earliest schema.

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

const MIN_CC: i64 = 15;
const MIN_LINES: i64 = 50;
const MIN_PARAMS: usize = 5;
const DEFAULT_LIMIT: i64 = 200;
const MAX_LIMIT: i64 = 1000;

#[derive(Debug, Deserialize)]
pub struct SmellsQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct GodFunction {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub language: String,
    pub line_start: i64,
    pub line_end: i64,
    pub lines: i64,
    pub complexity: i64,
}

#[derive(Debug, Serialize)]
pub struct LongParams {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub language: String,
    pub line_start: i64,
    pub param_count: usize,
    pub signature: String,
}

#[derive(Debug, Serialize)]
pub struct SmellsResponse {
    pub god_functions: Vec<GodFunction>,
    pub long_params: Vec<LongParams>,
    pub indexed: bool,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: &'static str,
}

pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<SmellsQuery>,
) -> Result<Json<SmellsResponse>, (StatusCode, Json<ApiError>)> {
    let limit = clamp_limit(query.limit);
    let root = state.project_root().to_path_buf();

    let result = tokio::task::spawn_blocking(move || compute_smells_at_root(&root, limit))
        .await
        .map_err(|error| {
            tracing::error!(?error, "smells.join.failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError {
                    error: "join error",
                }),
            )
        })?;

    match result {
        Ok(response) => Ok(Json(response)),
        Err(error) => {
            tracing::error!(?error, "smells.query.failed");
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

fn compute_smells_at_root(root: &Path, limit: i64) -> anyhow::Result<SmellsResponse> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(SmellsResponse {
            god_functions: Vec::new(),
            long_params: Vec::new(),
            indexed: false,
        });
    }
    let conn = Connection::open(&db_path)?;
    let god_functions = load_god_functions(&conn, limit)?;
    let long_params = load_long_params(&conn, limit)?;
    Ok(SmellsResponse {
        god_functions,
        long_params,
        indexed: true,
    })
}

pub(crate) fn load_god_functions(
    conn: &Connection,
    limit: i64,
) -> anyhow::Result<Vec<GodFunction>> {
    if !column_exists(conn, "symbols", "complexity")? {
        return Ok(Vec::new());
    }
    let sql = "SELECT s.name, s.kind, f.path, f.language,
                      s.line_start, s.line_end, s.complexity
               FROM symbols s
               JOIN files f ON f.id = s.file_id
               WHERE s.complexity >= ?1
                 AND (s.line_end - s.line_start + 1) >= ?2
                 AND s.kind IN ('function', 'method')
               ORDER BY s.complexity DESC, (s.line_end - s.line_start + 1) DESC
               LIMIT ?3";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(rusqlite::params![MIN_CC, MIN_LINES, limit], |r| {
        let line_start: i64 = r.get(4)?;
        let line_end: i64 = r.get(5)?;
        Ok(GodFunction {
            name: r.get(0)?,
            kind: r.get(1)?,
            path: r.get(2)?,
            language: r.get(3)?,
            line_start,
            line_end,
            lines: line_end - line_start + 1,
            complexity: r.get(6)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub(crate) fn load_long_params(conn: &Connection, limit: i64) -> anyhow::Result<Vec<LongParams>> {
    let sql = "SELECT s.name, s.kind, f.path, f.language, s.line_start, s.signature
               FROM symbols s
               JOIN files f ON f.id = s.file_id
               WHERE s.signature IS NOT NULL
                 AND s.kind IN ('function', 'method')
               ORDER BY s.line_start";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| {
        let signature: String = r.get(5)?;
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            signature,
        ))
    })?;

    let cap = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut out: Vec<LongParams> = Vec::new();
    for row in rows {
        let (name, kind, path, language, line_start, signature) = row?;
        let count = count_signature_params(&signature);
        if count >= MIN_PARAMS {
            out.push(LongParams {
                name,
                kind,
                path,
                language,
                line_start,
                param_count: count,
                signature,
            });
            if out.len() >= cap {
                break;
            }
        }
    }
    out.sort_by(|a, b| b.param_count.cmp(&a.param_count));
    Ok(out)
}

/// Count top-level parameters in a function signature.
///
/// Mirrors `qartez_smells::count_signature_params` so the dashboard
/// reports the same offenders the MCP tool does. Strips Rust / Python
/// receivers (`self`, `&self`, `&mut self`, `mut self`, `cls`) and
/// respects nested `<>` / `()` so generic / tuple types do not
/// inflate the count.
fn count_signature_params(sig: &str) -> usize {
    let start = match sig.find('(') {
        Some(i) => i + 1,
        None => return 0,
    };
    let mut depth: u32 = 1;
    let mut end = start;
    for (i, &byte) in sig.as_bytes().iter().enumerate().skip(start) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    let params_str = sig[start..end].trim();
    if params_str.is_empty() {
        return 0;
    }
    let mut params: Vec<&str> = Vec::new();
    let mut angle_depth: u32 = 0;
    let mut paren_depth: u32 = 0;
    let mut seg_start = 0;
    for (i, ch) in params_str.char_indices() {
        match ch {
            '<' => angle_depth += 1,
            '>' if angle_depth > 0 => angle_depth -= 1,
            '(' => paren_depth += 1,
            ')' if paren_depth > 0 => paren_depth -= 1,
            ',' if angle_depth == 0 && paren_depth == 0 => {
                params.push(params_str[seg_start..i].trim());
                seg_start = i + 1;
            }
            _ => {}
        }
    }
    params.push(params_str[seg_start..].trim());
    params
        .into_iter()
        .filter(|p| {
            if p.is_empty() {
                return false;
            }
            let base = p.split(':').next().unwrap_or(p).trim();
            !matches!(base, "self" | "&self" | "&mut self" | "mut self" | "cls")
        })
        .count()
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

    const TEST_SCHEMA: &str = "
        CREATE TABLE files (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            path       TEXT    NOT NULL UNIQUE,
            language   TEXT    NOT NULL DEFAULT 'rust',
            line_count INTEGER NOT NULL DEFAULT 0,
            mtime_ns   INTEGER NOT NULL DEFAULT 0,
            size_bytes INTEGER NOT NULL DEFAULT 0,
            indexed_at INTEGER NOT NULL DEFAULT 0,
            pagerank   REAL    NOT NULL DEFAULT 0.0
        );
        CREATE TABLE symbols (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            name       TEXT    NOT NULL,
            kind       TEXT    NOT NULL,
            line_start INTEGER NOT NULL,
            line_end   INTEGER NOT NULL,
            signature  TEXT,
            complexity INTEGER
        );
    ";

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(TEST_SCHEMA).expect("create schema");
        conn.execute(
            "INSERT INTO files (id, path, language) VALUES (1, 'src/lib.rs', 'rust')",
            [],
        )
        .unwrap();
        conn
    }

    fn insert_symbol(
        conn: &Connection,
        name: &str,
        kind: &str,
        line_start: i64,
        line_end: i64,
        signature: Option<&str>,
        complexity: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, line_start, line_end, signature, complexity)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![name, kind, line_start, line_end, signature, complexity],
        )
        .unwrap();
    }

    #[test]
    fn god_functions_pick_up_complex_long_bodies() {
        let conn = fresh_db();
        insert_symbol(&conn, "tiny", "function", 1, 5, None, Some(20));
        insert_symbol(&conn, "shallow", "function", 10, 100, None, Some(5));
        insert_symbol(&conn, "monster", "function", 200, 300, None, Some(40));

        let g = load_god_functions(&conn, 10).expect("query ok");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].name, "monster");
        assert_eq!(g[0].complexity, 40);
        assert_eq!(g[0].lines, 101);
    }

    #[test]
    fn long_params_strip_receivers_and_respect_threshold() {
        let conn = fresh_db();
        insert_symbol(
            &conn,
            "few",
            "function",
            1,
            2,
            Some("fn few(a: i32, b: i32)"),
            None,
        );
        insert_symbol(
            &conn,
            "five",
            "function",
            3,
            4,
            Some("fn five(a: i32, b: i32, c: i32, d: i32, e: i32)"),
            None,
        );
        insert_symbol(
            &conn,
            "method",
            "method",
            5,
            6,
            Some("fn method(&self, a: i32, b: i32, c: i32, d: i32, e: i32)"),
            None,
        );

        let lp = load_long_params(&conn, 10).expect("query ok");
        assert_eq!(lp.len(), 2);
        let names: Vec<&str> = lp.iter().map(|x| x.name.as_str()).collect();
        assert!(names.contains(&"five"));
        assert!(names.contains(&"method"));
        for entry in &lp {
            assert_eq!(entry.param_count, 5);
        }
    }

    #[test]
    fn count_params_handles_generics_and_tuples() {
        assert_eq!(count_signature_params("fn x(a: HashMap<K, V>, b: i32)"), 2);
        assert_eq!(count_signature_params("fn x(a: (i32, i32), b: i32)"), 2);
        assert_eq!(count_signature_params("fn x()"), 0);
        assert_eq!(count_signature_params(""), 0);
    }

    #[test]
    fn returns_empty_when_complexity_column_missing() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT, language TEXT);
             CREATE TABLE symbols (
                id INTEGER PRIMARY KEY, file_id INTEGER, name TEXT, kind TEXT,
                line_start INTEGER, line_end INTEGER, signature TEXT
             );",
        )
        .unwrap();
        let g = load_god_functions(&conn, 10).expect("query ok");
        assert!(g.is_empty());
    }
}
