// Rust guideline compliant 2026-04-25

#![allow(unused_imports)]

//! `qartez_maintenance` MCP tool: operator-driven `.qartez/index.db`
//! upkeep. Hosts six actions (`stats`, `checkpoint`, `optimize_fts`,
//! `vacuum_incremental`, `vacuum`, `convert_incremental`,
//! `purge_stale`) backed by [`crate::storage::maintenance`].
//!
//! Designed to be called rarely and explicitly: vacuum-class actions
//! rewrite the database file and can take minutes on a multi-GiB index.
//! The default `stats` action is read-only and cheap; surface it first
//! so callers see the current state before they trigger anything heavy.

use std::collections::HashSet;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};

use super::super::QartezServer;
use super::super::params::*;
use super::super::tiers;

use crate::index::fingerprint;
use crate::storage::maintenance::{
    self, ConvertIncrementalOutcome, IndexStats, checkpoint_truncate,
    convert_to_incremental_auto_vacuum, optimize_fts, purge_orphaned_files, purge_stale_roots,
    stats, vacuum_full, vacuum_incremental,
};
use crate::storage::read;

#[tool_router(router = qartez_maintenance_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_maintenance",
        description = "Inspect and compact the `.qartez/index.db` file. Default action 'stats' is read-only and reports DB / WAL sizes, top tables by row count, current workspace fingerprint, last full-reindex timestamp, and any derived-table coverage gaps that point to a stale pagerank or body-FTS state. Other actions: 'checkpoint' (truncate WAL), 'optimize_fts' (merge FTS5 segments), 'vacuum_incremental' (free pages back to OS), 'vacuum' (full rewrite, slow on multi-GiB DBs), 'convert_incremental' (one-shot conversion to auto_vacuum=INCREMENTAL; idempotent), 'purge_stale' (drop file rows for roots no longer in the workspace), 'purge_orphaned' (drop file rows whose on-disk path no longer exists).",
        annotations(
            title = "Database Maintenance",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_maintenance(
        &self,
        Parameters(params): Parameters<SoulMaintenanceParams>,
    ) -> Result<String, String> {
        let action = params.action.unwrap_or_default();
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let db_path = self.project_root.join(".qartez").join("index.db");

        match action {
            MaintenanceAction::Stats => Ok(render_stats(
                &stats(&conn, &db_path).map_err(|e| format!("stats error: {e}"))?,
            )),
            MaintenanceAction::Checkpoint => {
                let (busy, log_pages, ckpt) =
                    checkpoint_truncate(&conn).map_err(|e| format!("checkpoint error: {e}"))?;
                Ok(format!(
                    "checkpoint(TRUNCATE) -> busy={busy}, log_pages={log_pages}, checkpointed={ckpt}\n"
                ))
            }
            MaintenanceAction::OptimizeFts => {
                optimize_fts(&conn).map_err(|e| format!("optimize_fts error: {e}"))?;
                Ok(
                    "FTS5 segment-merge optimization complete (symbols_body_fts, symbols_fts).\n"
                        .to_string(),
                )
            }
            MaintenanceAction::VacuumIncremental => {
                let freed = vacuum_incremental(&conn)
                    .map_err(|e| format!("vacuum_incremental error: {e}"))?;
                Ok(format!(
                    "incremental_vacuum complete: {freed} page(s) released to free list.\n"
                ))
            }
            MaintenanceAction::Vacuum => {
                tracing::info!("qartez_maintenance: starting full VACUUM (may take minutes)");
                vacuum_full(&conn).map_err(|e| format!("vacuum error: {e}"))?;
                Ok("VACUUM complete. Database file rewritten.\n".to_string())
            }
            MaintenanceAction::ConvertIncremental => {
                tracing::info!(
                    "qartez_maintenance: convert_incremental requested (full VACUUM only if not already INCREMENTAL)"
                );
                let outcome = convert_to_incremental_auto_vacuum(&conn)
                    .map_err(|e| format!("convert_incremental error: {e}"))?;
                Ok(match outcome {
                    ConvertIncrementalOutcome::AlreadyConfigured => {
                        "auto_vacuum=INCREMENTAL already configured; no VACUUM performed.\n"
                            .to_string()
                    }
                    ConvertIncrementalOutcome::Converted => {
                        "auto_vacuum=INCREMENTAL set; VACUUM complete. Future runs will reclaim pages incrementally.\n"
                            .to_string()
                    }
                })
            }
            MaintenanceAction::PurgeStale => {
                let roots = self
                    .project_roots
                    .read()
                    .map_err(|e| format!("project_roots lock: {e}"))?
                    .clone();
                let aliases = self
                    .root_aliases
                    .read()
                    .map_err(|e| format!("root_aliases lock: {e}"))?
                    .clone();
                let live: HashSet<String> = fingerprint::live_root_prefixes(&roots, &aliases)
                    .into_iter()
                    .collect();
                let removed = purge_stale_roots(&conn, &live)
                    .map_err(|e| format!("purge_stale error: {e}"))?;
                Ok(format!(
                    "purge_stale complete: {removed} file row(s) removed from prefixes no longer in the workspace.\n"
                ))
            }
            MaintenanceAction::PurgeOrphaned => {
                let roots = self
                    .project_roots
                    .read()
                    .map_err(|e| format!("project_roots lock: {e}"))?
                    .clone();
                let aliases = self
                    .root_aliases
                    .read()
                    .map_err(|e| format!("root_aliases lock: {e}"))?
                    .clone();
                let removed = purge_orphaned_files(&conn, &self.project_root, &roots, &aliases)
                    .map_err(|e| format!("purge_orphaned error: {e}"))?;
                Ok(format!(
                    "purge_orphaned complete: {removed} file row(s) removed because their on-disk path no longer exists.\n"
                ))
            }
        }
    }
}

/// Render an [`IndexStats`] snapshot as a compact human-readable block.
///
/// Designed for direct LLM consumption: one section per concern, no
/// padding, no decorative borders.
fn render_stats(s: &IndexStats) -> String {
    let av_label = match s.auto_vacuum {
        0 => "NONE",
        1 => "FULL",
        2 => "INCREMENTAL",
        _ => "UNKNOWN",
    };
    let mut out = String::new();
    out.push_str("# qartez_maintenance: stats\n\n");
    out.push_str(&format!("db_path: {}\n", s.db_path));
    out.push_str(&format!(
        "db_size: {} ({} bytes)\nwal_size: {} ({} bytes)\nshm_size: {} ({} bytes)\n",
        maintenance::human_bytes(s.db_bytes),
        s.db_bytes,
        maintenance::human_bytes(s.wal_bytes),
        s.wal_bytes,
        maintenance::human_bytes(s.shm_bytes),
        s.shm_bytes,
    ));
    out.push_str(&format!(
        "page_size: {} | page_count: {} | freelist: {} | auto_vacuum: {av_label} | journal_mode: {}\n",
        s.page_size, s.page_count, s.freelist_count, s.journal_mode,
    ));
    out.push_str(&format!(
        "fingerprint: {} | last_full_reindex: {} | last_index: {}\n",
        s.fingerprint.as_deref().unwrap_or("<unset>"),
        s.last_full_reindex
            .map(|t| t.to_string())
            .unwrap_or_else(|| "<unset>".to_string()),
        s.last_index
            .map(|t| t.to_string())
            .unwrap_or_else(|| "<unset>".to_string()),
    ));
    out.push('\n');
    out.push_str("## top tables\n");
    for t in &s.top_tables {
        out.push_str(&format!("{}={}\n", t.name, t.row_count));
    }

    // Derived-table coverage gaps. Surfaced after the top-tables block
    // so operators see them immediately without paging. These rows
    // expose silent degradation following a `qartez_workspace add` or
    // `remove` cycle: pagerank values mid-recompute, body_fts wipes
    // that the per-file rebuild has not yet healed, etc.
    let gaps = &s.derived_gaps;
    if gaps.total_files > 0
        && (gaps.files_with_zero_pagerank > 0 || gaps.files_missing_body_fts > 0)
    {
        out.push_str("\n## derived-table gaps\n");
        if gaps.files_with_zero_pagerank > 0 {
            out.push_str(&format!(
                "files_with_zero_pagerank={}/{} (run a full reindex or `qartez_workspace add`/`remove` to recompute)\n",
                gaps.files_with_zero_pagerank, gaps.total_files,
            ));
        }
        if gaps.files_missing_body_fts > 0 {
            out.push_str(&format!(
                "files_missing_body_fts={} (qartez_grep search_bodies=true will under-report; trigger a reindex to heal)\n",
                gaps.files_missing_body_fts,
            ));
        }
    }

    if s.auto_vacuum == 0 && s.db_bytes > 1024 * 1024 * 1024 {
        out.push_str(
            "\nhint: auto_vacuum=NONE on a >1 GiB DB. Run `qartez_maintenance action=convert_incremental` once to enable incremental page reclamation.\n",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_server(root: &std::path::Path) -> QartezServer {
        let db_path = root.join(".qartez").join("index.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let conn = crate::storage::open_db(&db_path).unwrap();
        QartezServer::new(conn, root.to_path_buf(), 0)
    }

    #[test]
    fn maintenance_stats_returns_summary() {
        let tmp = TempDir::new().unwrap();
        let server = make_server(tmp.path());
        let result = server.qartez_maintenance(Parameters(SoulMaintenanceParams {
            action: Some(MaintenanceAction::Stats),
        }));
        let s = result.expect("stats must succeed on fresh DB");
        assert!(s.contains("qartez_maintenance: stats"));
        assert!(s.contains("db_path"));
        assert!(s.contains("auto_vacuum"));
        assert!(s.contains("top tables"));
    }

    #[test]
    fn maintenance_checkpoint_returns_tuple() {
        let tmp = TempDir::new().unwrap();
        let server = make_server(tmp.path());
        let result = server
            .qartez_maintenance(Parameters(SoulMaintenanceParams {
                action: Some(MaintenanceAction::Checkpoint),
            }))
            .expect("checkpoint must succeed");
        assert!(result.contains("checkpoint(TRUNCATE)"));
        assert!(result.contains("busy="));
    }

    #[test]
    fn maintenance_optimize_fts_runs_on_empty_tables() {
        let tmp = TempDir::new().unwrap();
        let server = make_server(tmp.path());
        let result = server
            .qartez_maintenance(Parameters(SoulMaintenanceParams {
                action: Some(MaintenanceAction::OptimizeFts),
            }))
            .expect("optimize_fts must succeed on empty FTS tables");
        assert!(result.contains("FTS5"));
    }

    #[test]
    fn maintenance_purge_stale_no_op_on_clean_db() {
        let tmp = TempDir::new().unwrap();
        let server = make_server(tmp.path());
        let result = server
            .qartez_maintenance(Parameters(SoulMaintenanceParams {
                action: Some(MaintenanceAction::PurgeStale),
            }))
            .expect("purge_stale must succeed when no rows exist");
        assert!(result.contains("purge_stale complete"));
        assert!(result.contains("0 file row"));
    }

    #[test]
    fn maintenance_default_action_is_stats() {
        let tmp = TempDir::new().unwrap();
        let server = make_server(tmp.path());
        let result = server
            .qartez_maintenance(Parameters(SoulMaintenanceParams { action: None }))
            .expect("default action must succeed");
        assert!(result.contains("qartez_maintenance: stats"));
    }
}
