//! `GET /api/hotspots` - file ranking by composite hotspot score.
//!
//! Mirrors the formula used by the `qartez_hotspots` MCP tool:
//!
//! - `score = max_complexity * pagerank * (1 + change_count)`
//! - `health = mean(cc_h, coupling_h, churn_h)` where each factor is
//!   `10 / (1 + value / halflife)` with halflives 10 (cc), 0.02 (pagerank),
//!   and 8 (churn). Range `[0, 10]`, 10 = healthiest.
//!
//! Files with no symbols carrying complexity are excluded - the score
//! defaults to zero for them, which matches MCP behavior. Old DBs that
//! predate the `complexity` / `change_count` migrations get treated as
//! `NULL -> 0`.

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 500;

#[derive(Debug, Deserialize)]
pub struct HotspotsQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct HotspotItem {
    pub path: String,
    pub language: String,
    pub pagerank: f64,
    pub churn: i64,
    pub max_cc: i64,
    pub avg_cc: f64,
    pub score: f64,
    pub health: f64,
}

#[derive(Debug, Serialize)]
pub struct HotspotsResponse {
    pub items: Vec<HotspotItem>,
    pub indexed: bool,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: &'static str,
}

pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<HotspotsQuery>,
) -> Result<Json<HotspotsResponse>, (StatusCode, Json<ApiError>)> {
    let limit = clamp_limit(query.limit);
    let root = state.project_root().to_path_buf();

    let result = tokio::task::spawn_blocking(move || compute_hotspots_at_root(&root, limit))
        .await
        .map_err(|error| {
            tracing::error!(?error, "hotspots.join.failed");
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
            tracing::error!(?error, "hotspots.query.failed");
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

fn compute_hotspots_at_root(root: &Path, limit: i64) -> anyhow::Result<HotspotsResponse> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(HotspotsResponse {
            items: Vec::new(),
            indexed: false,
        });
    }
    let conn = Connection::open(&db_path)?;
    let items = compute_hotspots(&conn, limit)?;
    Ok(HotspotsResponse {
        items,
        indexed: true,
    })
}

pub(crate) fn compute_hotspots(conn: &Connection, limit: i64) -> anyhow::Result<Vec<HotspotItem>> {
    let has_complexity = column_exists(conn, "symbols", "complexity")?;
    let has_change_count = column_exists(conn, "files", "change_count")?;

    let cc_expr = if has_complexity {
        "s.complexity"
    } else {
        "NULL"
    };
    let churn_expr = if has_change_count {
        "f.change_count"
    } else {
        "0"
    };

    let sql = format!(
        "SELECT f.path, f.language, f.pagerank, {churn_expr} AS churn,
                MAX({cc_expr}) AS max_cc, AVG({cc_expr}) AS avg_cc
         FROM files f
         LEFT JOIN symbols s ON s.file_id = f.id
         GROUP BY f.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        let path: String = r.get(0)?;
        let language: String = r.get(1)?;
        let pagerank: f64 = r.get(2)?;
        let churn: i64 = r.get(3)?;
        let max_cc: Option<i64> = r.get(4)?;
        let avg_cc: Option<f64> = r.get(5)?;
        Ok((path, language, pagerank, churn, max_cc, avg_cc))
    })?;

    let mut items: Vec<HotspotItem> = Vec::new();
    for row in rows {
        let (path, language, pagerank, churn, max_cc_opt, avg_cc_opt) = row?;
        let Some(max_cc) = max_cc_opt else {
            continue;
        };
        if max_cc <= 0 {
            continue;
        }
        let avg_cc = avg_cc_opt.unwrap_or(0.0);
        #[expect(
            clippy::cast_precision_loss,
            reason = "max_cc and churn are bounded small integers"
        )]
        let score = max_cc as f64 * pagerank * (1.0 + churn as f64);
        if score <= 0.0 {
            continue;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "max_cc is bounded; precision loss inside the health formula is negligible"
        )]
        let health = health_score(max_cc as f64, pagerank, churn);
        items.push(HotspotItem {
            path,
            language,
            pagerank,
            churn,
            max_cc,
            avg_cc,
            score,
            health,
        });
    }

    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let cap = usize::try_from(limit).unwrap_or(usize::MAX);
    items.truncate(cap);
    Ok(items)
}

fn health_score(max_cc: f64, coupling: f64, churn: i64) -> f64 {
    let cc_h = 10.0 / (1.0 + max_cc / 10.0);
    let coupling_h = 10.0 / (1.0 + coupling * 50.0);
    #[expect(
        clippy::cast_precision_loss,
        reason = "churn is a small int bounded by git history depth"
    )]
    let churn_h = 10.0 / (1.0 + churn as f64 / 8.0);
    (cc_h + coupling_h + churn_h) / 3.0
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
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            path         TEXT    NOT NULL UNIQUE,
            mtime_ns     INTEGER NOT NULL,
            size_bytes   INTEGER NOT NULL,
            language     TEXT    NOT NULL,
            line_count   INTEGER NOT NULL,
            pagerank     REAL    NOT NULL DEFAULT 0.0,
            indexed_at   INTEGER NOT NULL,
            change_count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE symbols (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            name       TEXT    NOT NULL,
            kind       TEXT    NOT NULL,
            line_start INTEGER NOT NULL,
            line_end   INTEGER NOT NULL,
            complexity INTEGER
        );
    ";

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(TEST_SCHEMA).expect("create schema");
        conn
    }

    fn insert_file(conn: &Connection, id: i64, path: &str, pagerank: f64, change_count: i64) {
        conn.execute(
            "INSERT INTO files
             (id, path, mtime_ns, size_bytes, language, line_count, pagerank, indexed_at, change_count)
             VALUES (?1, ?2, 0, 0, 'rust', 100, ?3, 0, ?4)",
            rusqlite::params![id, path, pagerank, change_count],
        )
        .unwrap();
    }

    fn insert_symbol(conn: &Connection, file_id: i64, complexity: i64) {
        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, line_start, line_end, complexity)
             VALUES (?1, 'sym', 'function', 1, 10, ?2)",
            rusqlite::params![file_id, complexity],
        )
        .unwrap();
    }

    #[test]
    fn empty_db_returns_no_items() {
        let conn = fresh_db();
        let items = compute_hotspots(&conn, 10).expect("query ok");
        assert!(items.is_empty());
    }

    #[test]
    fn ranks_files_by_score_desc() {
        let conn = fresh_db();
        insert_file(&conn, 1, "low.rs", 0.1, 0);
        insert_symbol(&conn, 1, 5);
        insert_file(&conn, 2, "high.rs", 0.5, 10);
        insert_symbol(&conn, 2, 20);

        let items = compute_hotspots(&conn, 10).expect("query ok");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].path, "high.rs");
        assert_eq!(items[0].max_cc, 20);
        assert_eq!(items[0].churn, 10);
        assert_eq!(items[1].path, "low.rs");
    }

    #[test]
    fn excludes_files_without_complexity_data() {
        let conn = fresh_db();
        insert_file(&conn, 1, "code.rs", 0.5, 1);
        insert_symbol(&conn, 1, 10);
        insert_file(&conn, 2, "data.json", 0.3, 1);

        let items = compute_hotspots(&conn, 10).expect("query ok");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, "code.rs");
    }

    #[test]
    fn health_is_within_bounds() {
        let conn = fresh_db();
        insert_file(&conn, 1, "a.rs", 0.001, 0);
        insert_symbol(&conn, 1, 1);
        let items = compute_hotspots(&conn, 10).expect("query ok");
        let h = items[0].health;
        assert!(h.is_finite());
        assert!((0.0..=10.0).contains(&h), "health {h} out of range");
    }

    #[test]
    fn limit_truncates_result() {
        let conn = fresh_db();
        for i in 1_i64..=5 {
            insert_file(&conn, i, &format!("f{i}.rs"), 0.1 * i as f64, i);
            insert_symbol(&conn, i, i + 1);
        }
        let items = compute_hotspots(&conn, 2).expect("query ok");
        assert_eq!(items.len(), 2);
    }
}
