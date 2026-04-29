//! `GET /api/project` - returns a coarse summary of the current project's index.
//!
//! M1 scope: project root, file count, symbol count from
//! `<root>/.qartez/qartez.db` (the index file written by `qartez-mcp`).
//! Subsequent milestones will add the focused-file view, impact ring data,
//! and Project Pulse health metrics.

use std::path::PathBuf;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use rusqlite::Connection;
use serde::Serialize;

use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct ProjectSummary {
    pub root: PathBuf,
    pub files: u64,
    pub symbols: u64,
    pub indexed: bool,
}

pub async fn handler(
    State(state): State<AppState>,
) -> Result<Json<ProjectSummary>, (StatusCode, String)> {
    let root = state.project_root().to_path_buf();
    let summary = tokio::task::spawn_blocking(move || compute_summary(&root))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")))?
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("read index: {e}"),
            )
        })?;

    Ok(Json(summary))
}

fn compute_summary(root: &std::path::Path) -> anyhow::Result<ProjectSummary> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(ProjectSummary {
            root: root.to_path_buf(),
            files: 0,
            symbols: 0,
            indexed: false,
        });
    }
    let conn = Connection::open(&db_path)?;
    let files: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap_or(0);
    let symbols: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
        .unwrap_or(0);
    let files = u64::try_from(files).unwrap_or(0);
    let symbols = u64::try_from(symbols).unwrap_or(0);
    Ok(ProjectSummary {
        root: root.to_path_buf(),
        files,
        symbols,
        indexed: files > 0,
    })
}

/// Mirror of qartez-mcp's default DB layout: `<project_root>/.qartez/index.db`.
fn default_db_path(root: &std::path::Path) -> PathBuf {
    root.join(".qartez").join("index.db")
}
