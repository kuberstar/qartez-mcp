//! `GET /api/project-health` - composite project-wide health view that
//! cross-references hotspot pressure with code-smell incidence.
//!
//! Surfaces the same per-file score the `qartez_hotspots` MCP tool emits,
//! then annotates each row with the count of god-functions and
//! long-parameter signatures from `qartez_smells`. Severity:
//!
//! - **critical** - hotspot AND `smell_count > 0`
//! - **medium**   - `smell_count > 0` and not a hotspot
//! - **low**      - hotspot with no smells (high pressure but no obvious
//!   refactor target)
//!
//! The `summary` block exposes aggregate counts so the dashboard can
//! render a single-glance project verdict without re-scanning the rows.
//!
//! Lives at `/api/project-health` because `/api/health` is already
//! claimed by the daemon liveness probe.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::api::hotspots;
use crate::api::smells;
use crate::state::AppState;

const HOTSPOT_PRESSURE_TOP_N: usize = 30;
const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 500;

#[derive(Debug, Deserialize)]
pub struct ProjectHealthQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct HealthFile {
    pub path: String,
    pub language: String,
    pub health: f64,
    pub max_cc: i64,
    pub pagerank: f64,
    pub churn: i64,
    pub smell_count: usize,
    pub severity: &'static str,
}

#[derive(Debug, Serialize)]
pub struct HealthSummary {
    pub avg_health: f64,
    pub critical_count: usize,
    pub medium_count: usize,
    pub low_count: usize,
    pub file_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ProjectHealthResponse {
    pub files: Vec<HealthFile>,
    pub summary: HealthSummary,
    pub indexed: bool,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: &'static str,
}

pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<ProjectHealthQuery>,
) -> Result<Json<ProjectHealthResponse>, (StatusCode, Json<ApiError>)> {
    let limit = clamp_limit(query.limit);
    let root = state.project_root().to_path_buf();

    let result = tokio::task::spawn_blocking(move || compute_at_root(&root, limit))
        .await
        .map_err(|error| {
            tracing::error!(?error, "project_health.join.failed");
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
            tracing::error!(?error, "project_health.query.failed");
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

fn compute_at_root(root: &Path, limit: i64) -> anyhow::Result<ProjectHealthResponse> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(ProjectHealthResponse {
            files: Vec::new(),
            summary: HealthSummary {
                avg_health: 0.0,
                critical_count: 0,
                medium_count: 0,
                low_count: 0,
                file_count: 0,
            },
            indexed: false,
        });
    }
    let conn = Connection::open(&db_path)?;
    let response = compute(&conn, limit)?;
    Ok(ProjectHealthResponse {
        indexed: true,
        ..response
    })
}

pub(crate) fn compute(conn: &Connection, limit: i64) -> anyhow::Result<ProjectHealthResponse> {
    // Pull every file from the hotspot scan so smell-only files still get
    // a real health number instead of falling back to 0.0. The user's
    // `limit` only constrains the rendered output below.
    let hotspots = hotspots::compute_hotspots(conn, i64::MAX)?;
    let god = smells::load_god_functions(conn, i64::MAX)?;
    let long = smells::load_long_params(conn, i64::MAX)?;

    let mut smells_per_file: HashMap<String, usize> = HashMap::new();
    for entry in &god {
        *smells_per_file.entry(entry.path.clone()).or_insert(0) += 1;
    }
    for entry in &long {
        *smells_per_file.entry(entry.path.clone()).or_insert(0) += 1;
    }

    let hotspot_paths: std::collections::HashSet<String> = hotspots
        .iter()
        .take(HOTSPOT_PRESSURE_TOP_N)
        .map(|h| h.path.clone())
        .collect();

    let mut by_path: HashMap<String, HealthFile> = HashMap::new();
    for h in &hotspots {
        let smell_count = smells_per_file.get(&h.path).copied().unwrap_or(0);
        let is_hotspot = hotspot_paths.contains(&h.path);
        let severity = classify(is_hotspot, smell_count);
        by_path.insert(
            h.path.clone(),
            HealthFile {
                path: h.path.clone(),
                language: h.language.clone(),
                health: h.health,
                max_cc: h.max_cc,
                pagerank: h.pagerank,
                churn: h.churn,
                smell_count,
                severity,
            },
        );
    }

    for entry in &god {
        if by_path.contains_key(&entry.path) {
            continue;
        }
        let smell_count = smells_per_file.get(&entry.path).copied().unwrap_or(0);
        let severity = classify(false, smell_count);
        by_path.insert(
            entry.path.clone(),
            HealthFile {
                path: entry.path.clone(),
                language: entry.language.clone(),
                health: 0.0,
                max_cc: entry.complexity,
                pagerank: 0.0,
                churn: 0,
                smell_count,
                severity,
            },
        );
    }
    for entry in &long {
        if by_path.contains_key(&entry.path) {
            continue;
        }
        let smell_count = smells_per_file.get(&entry.path).copied().unwrap_or(0);
        let severity = classify(false, smell_count);
        by_path.insert(
            entry.path.clone(),
            HealthFile {
                path: entry.path.clone(),
                language: entry.language.clone(),
                health: 0.0,
                max_cc: 0,
                pagerank: 0.0,
                churn: 0,
                smell_count,
                severity,
            },
        );
    }

    let mut files: Vec<HealthFile> = by_path.into_values().collect();
    files.sort_by(|a, b| {
        let rank = |s: &str| match s {
            "critical" => 0_u8,
            "medium" => 1,
            "low" => 2,
            _ => 3,
        };
        rank(a.severity)
            .cmp(&rank(b.severity))
            .then_with(|| b.smell_count.cmp(&a.smell_count))
            .then_with(|| {
                a.health
                    .partial_cmp(&b.health)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    // Summary counters describe the whole project, not the page the table
    // renders, so they are computed across the full ranked vector before the
    // display-cap truncation. A previous version ran them after `truncate`,
    // which made the summary collapse to the page size and contradicted the
    // earlier fix that lifted the internal compute to unbounded.
    let critical_count = files.iter().filter(|f| f.severity == "critical").count();
    let medium_count = files.iter().filter(|f| f.severity == "medium").count();
    let low_count = files.iter().filter(|f| f.severity == "low").count();
    let file_count = files.len();
    let avg_health = if files.is_empty() {
        0.0
    } else {
        let sum: f64 = files.iter().map(|f| f.health).sum();
        #[expect(
            clippy::cast_precision_loss,
            reason = "file_count is the length of a Vec produced in this function and stays well under f64 precision"
        )]
        let denom = file_count as f64;
        sum / denom
    };

    let cap = usize::try_from(limit).unwrap_or(usize::MAX);
    files.truncate(cap);

    Ok(ProjectHealthResponse {
        files,
        summary: HealthSummary {
            avg_health,
            critical_count,
            medium_count,
            low_count,
            file_count,
        },
        indexed: true,
    })
}

fn classify(is_hotspot: bool, smell_count: usize) -> &'static str {
    if is_hotspot && smell_count > 0 {
        "critical"
    } else if smell_count > 0 {
        "medium"
    } else if is_hotspot {
        "low"
    } else {
        "ok"
    }
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
            mtime_ns     INTEGER NOT NULL DEFAULT 0,
            size_bytes   INTEGER NOT NULL DEFAULT 0,
            language     TEXT    NOT NULL,
            line_count   INTEGER NOT NULL DEFAULT 0,
            pagerank     REAL    NOT NULL DEFAULT 0.0,
            indexed_at   INTEGER NOT NULL DEFAULT 0,
            change_count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE symbols (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            name        TEXT    NOT NULL,
            kind        TEXT    NOT NULL,
            line_start  INTEGER NOT NULL,
            line_end    INTEGER NOT NULL,
            signature   TEXT,
            complexity  INTEGER,
            is_exported INTEGER NOT NULL DEFAULT 0
        );
    ";

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(TEST_SCHEMA).unwrap();
        conn
    }

    #[test]
    fn empty_db_yields_zero_summary() {
        let conn = db();
        let r = compute(&conn, 100).expect("ok");
        assert!(r.files.is_empty());
        assert_eq!(r.summary.file_count, 0);
        assert_eq!(r.summary.critical_count, 0);
    }

    #[test]
    fn marks_critical_when_hotspot_overlaps_smell() {
        let conn = db();
        conn.execute(
            "INSERT INTO files (id, path, language, pagerank, change_count)
             VALUES (1, 'src/big.rs', 'rust', 0.5, 10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, line_start, line_end, complexity, signature)
             VALUES (1, 'monster', 'function', 1, 100, 30, 'fn monster()')",
            [],
        )
        .unwrap();

        let r = compute(&conn, 100).expect("ok");
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].severity, "critical");
        assert_eq!(r.summary.critical_count, 1);
    }

    #[test]
    fn summary_counts_full_project_when_files_truncated() {
        // Insert five files where each has a god function (high complexity),
        // so all five qualify as `medium` severity. Then ask for `limit=2` so
        // the rendered list is truncated to two rows. The summary must still
        // describe all five files, not the two-row page.
        let conn = db();
        for i in 1..=5 {
            conn.execute(
                "INSERT INTO files (id, path, language, pagerank, change_count)
                 VALUES (?1, ?2, 'rust', 0.0, 0)",
                rusqlite::params![i, format!("src/f{i}.rs")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO symbols (file_id, name, kind, line_start, line_end, complexity, signature)
                 VALUES (?1, ?2, 'function', 1, 100, 30, 'fn big()')",
                rusqlite::params![i, format!("big{i}")],
            )
            .unwrap();
        }

        let r = compute(&conn, 2).expect("ok");
        assert_eq!(r.files.len(), 2, "rendered list respects the cap");
        assert_eq!(
            r.summary.file_count, 5,
            "summary reports the full project, not just the page",
        );
        assert_eq!(
            r.summary.medium_count, 5,
            "all five medium files are counted regardless of truncation",
        );
    }
}
