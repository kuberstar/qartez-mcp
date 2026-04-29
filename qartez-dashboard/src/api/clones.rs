//! `GET /api/clones` - groups of duplicate symbols by AST shape hash.
//!
//! Reads `symbols.shape_hash` (populated by the indexer's clone detector)
//! and groups by hash where at least two symbols share the same shape.
//! Each group includes the per-member file path / line range so the UI
//! can link straight to the duplicates.
//!
//! `min_lines` filters out trivial 1-3 line getters / setters that match
//! by accident; the default of 8 mirrors `qartez_clones`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

const DEFAULT_MIN_LINES: i64 = 8;
const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 500;

#[derive(Debug, Deserialize)]
pub struct ClonesQuery {
    pub min_lines: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct CloneMember {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line_start: i64,
    pub line_end: i64,
}

#[derive(Debug, Serialize)]
pub struct CloneGroup {
    pub shape_hash: String,
    pub member_count: i64,
    pub avg_lines: f64,
    pub members: Vec<CloneMember>,
}

#[derive(Debug, Serialize)]
pub struct ClonesResponse {
    pub groups: Vec<CloneGroup>,
    pub indexed: bool,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: &'static str,
}

pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<ClonesQuery>,
) -> Result<Json<ClonesResponse>, (StatusCode, Json<ApiError>)> {
    let min_lines = query
        .min_lines
        .filter(|v| *v >= 1)
        .unwrap_or(DEFAULT_MIN_LINES);
    let limit = clamp_limit(query.limit);
    let root = state.project_root().to_path_buf();

    let result =
        tokio::task::spawn_blocking(move || compute_clones_at_root(&root, min_lines, limit))
            .await
            .map_err(|error| {
                tracing::error!(?error, "clones.join.failed");
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
            tracing::error!(?error, "clones.query.failed");
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

fn compute_clones_at_root(
    root: &Path,
    min_lines: i64,
    limit: i64,
) -> anyhow::Result<ClonesResponse> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(ClonesResponse {
            groups: Vec::new(),
            indexed: false,
        });
    }
    let conn = Connection::open(&db_path)?;
    let groups = compute_clones(&conn, min_lines, limit)?;
    Ok(ClonesResponse {
        groups,
        indexed: true,
    })
}

pub(crate) fn compute_clones(
    conn: &Connection,
    min_lines: i64,
    limit: i64,
) -> anyhow::Result<Vec<CloneGroup>> {
    let group_sql = "SELECT shape_hash, COUNT(*) AS cnt,
                            AVG(line_end - line_start + 1) AS avg_lines
                     FROM symbols
                     WHERE shape_hash IS NOT NULL
                       AND shape_hash <> ''
                       AND (line_end - line_start + 1) >= ?1
                     GROUP BY shape_hash
                     HAVING cnt >= 2
                     ORDER BY cnt DESC, avg_lines DESC
                     LIMIT ?2";
    let mut stmt = conn.prepare(group_sql)?;
    let rows = stmt.query_map(rusqlite::params![min_lines, limit], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, f64>(2)?,
        ))
    })?;

    let mut hashes: Vec<(String, i64, f64)> = Vec::new();
    for row in rows {
        hashes.push(row?);
    }

    let mut members_by_hash = load_members(conn, &hashes, min_lines)?;
    let mut groups = Vec::with_capacity(hashes.len());
    for (shape_hash, member_count, avg_lines) in hashes {
        let members = members_by_hash.remove(&shape_hash).unwrap_or_default();
        groups.push(CloneGroup {
            shape_hash,
            member_count,
            avg_lines,
            members,
        });
    }
    Ok(groups)
}

fn load_members(
    conn: &Connection,
    hashes: &[(String, i64, f64)],
    min_lines: i64,
) -> anyhow::Result<HashMap<String, Vec<CloneMember>>> {
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = std::iter::repeat_n("?", hashes.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT s.shape_hash, s.id, s.name, s.kind, f.path, s.line_start, s.line_end
         FROM symbols s
         JOIN files f ON f.id = s.file_id
         WHERE s.shape_hash IN ({placeholders})
           AND (s.line_end - s.line_start + 1) >= ?
         ORDER BY f.path, s.line_start"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<rusqlite::types::Value> = hashes
        .iter()
        .map(|(h, _, _)| rusqlite::types::Value::Text(h.clone()))
        .collect();
    params.push(rusqlite::types::Value::Integer(min_lines));
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
        Ok((
            r.get::<_, String>(0)?,
            CloneMember {
                id: r.get(1)?,
                name: r.get(2)?,
                kind: r.get(3)?,
                path: r.get(4)?,
                line_start: r.get(5)?,
                line_end: r.get(6)?,
            },
        ))
    })?;

    let mut out: HashMap<String, Vec<CloneMember>> = HashMap::new();
    for row in rows {
        let (hash, member) = row?;
        out.entry(hash).or_default().push(member);
    }
    Ok(out)
}

fn default_db_path(root: &Path) -> PathBuf {
    root.join(".qartez").join("index.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SCHEMA: &str = "
        CREATE TABLE files (
            id   INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT    NOT NULL UNIQUE
        );
        CREATE TABLE symbols (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            name       TEXT    NOT NULL,
            kind       TEXT    NOT NULL,
            line_start INTEGER NOT NULL,
            line_end   INTEGER NOT NULL,
            shape_hash TEXT
        );
    ";

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(TEST_SCHEMA).expect("create schema");
        conn
    }

    fn insert_file(conn: &Connection, id: i64, path: &str) {
        conn.execute(
            "INSERT INTO files (id, path) VALUES (?1, ?2)",
            rusqlite::params![id, path],
        )
        .unwrap();
    }

    fn insert_symbol(
        conn: &Connection,
        file_id: i64,
        name: &str,
        line_start: i64,
        line_end: i64,
        shape_hash: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, line_start, line_end, shape_hash)
             VALUES (?1, ?2, 'function', ?3, ?4, ?5)",
            rusqlite::params![file_id, name, line_start, line_end, shape_hash],
        )
        .unwrap();
    }

    #[test]
    fn groups_symbols_with_same_hash() {
        let conn = fresh_db();
        insert_file(&conn, 1, "a.rs");
        insert_file(&conn, 2, "b.rs");
        insert_symbol(&conn, 1, "foo", 1, 20, Some("HASH_X"));
        insert_symbol(&conn, 2, "bar", 5, 25, Some("HASH_X"));
        insert_symbol(&conn, 1, "lone", 30, 50, Some("HASH_Y"));

        let groups = compute_clones(&conn, 8, 100).expect("query ok");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].shape_hash, "HASH_X");
        assert_eq!(groups[0].member_count, 2);
        assert_eq!(groups[0].members.len(), 2);
    }

    #[test]
    fn skips_short_bodies() {
        let conn = fresh_db();
        insert_file(&conn, 1, "a.rs");
        insert_symbol(&conn, 1, "tiny1", 1, 3, Some("H"));
        insert_symbol(&conn, 1, "tiny2", 5, 7, Some("H"));

        let groups = compute_clones(&conn, 8, 100).expect("query ok");
        assert!(groups.is_empty());
    }

    #[test]
    fn ignores_null_or_empty_shape_hash() {
        let conn = fresh_db();
        insert_file(&conn, 1, "a.rs");
        insert_symbol(&conn, 1, "n1", 1, 20, None);
        insert_symbol(&conn, 1, "n2", 25, 45, None);
        insert_symbol(&conn, 1, "e1", 50, 70, Some(""));
        insert_symbol(&conn, 1, "e2", 75, 95, Some(""));
        let groups = compute_clones(&conn, 8, 100).expect("query ok");
        assert!(groups.is_empty());
    }
}
