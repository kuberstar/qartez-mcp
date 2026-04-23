// Rust guideline compliant 2026-04-22

#![allow(unused_imports)]

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};

use super::super::QartezServer;
use super::super::helpers::{self, *};
use super::super::params::*;
use super::super::tiers;
use super::super::treesitter::*;

use crate::graph::blast;
use crate::guard;
use crate::storage::read;
use crate::storage::read::sanitize_fts_query;
use crate::toolchain;

/// Resolve a user-supplied write target for `qartez_boundaries`.
///
/// Accepts either a path relative to the project root (delegates to
/// `safe_resolve`) or an absolute path whose parent directory already
/// exists. The "absolute + existing parent" contract matches the
/// policy shared with `qartez_wiki`.
fn resolve_write_target(server: &QartezServer, user_path: &str) -> Result<PathBuf, String> {
    let trimmed = user_path.trim();
    if trimmed.is_empty() {
        return Err("write_to must not be empty".to_string());
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        let parent = candidate
            .parent()
            .ok_or_else(|| format!("Path '{trimmed}' has no parent directory"))?;
        if !parent.exists() {
            return Err(format!(
                "Parent directory '{}' does not exist. Create it first or use a path relative to the project root.",
                parent.display()
            ));
        }
        return Ok(candidate.to_path_buf());
    }
    server.safe_resolve(trimmed)
}

#[tool_router(router = qartez_boundaries_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_boundaries",
        description = "Check architecture boundary rules defined in `.qartez/boundaries.toml` against the import graph. Each rule says files matching `from` must not import files matching any `deny` pattern (with optional `allow` overrides). Returns the list of violating edges. Pass `suggest=true` to emit a starter config derived from the current Leiden clustering instead of running the checker. When `suggest=true` and the clustering table is empty, `auto_cluster=true` (default) runs the clustering on demand; set `auto_cluster=false` to fail loudly instead. `write_to` is only honored with `suggest=true`.",
        annotations(
            title = "Architecture Boundaries",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_boundaries(
        &self,
        Parameters(params): Parameters<SoulBoundariesParams>,
    ) -> Result<String, String> {
        use crate::graph::boundaries::{
            check_boundaries, load_config, render_config_toml, suggest_boundaries,
        };
        use crate::graph::leiden::{LeidenConfig, compute_clusters};
        use crate::storage::read::{
            boundaries_all_files, boundaries_edge_pairs, boundaries_file_cluster_pairs,
            wiki_cluster_row_count,
        };

        let rel_path = params
            .config_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(".qartez/boundaries.toml");
        let abs_path = self.safe_resolve(rel_path)?;
        let concise = is_concise(&params.format);

        let suggest = params.suggest.unwrap_or(false);
        let write_to_trimmed = params.write_to.as_deref().map(str::trim).unwrap_or("");

        if !suggest && !write_to_trimmed.is_empty() {
            return Err(
                "`write_to` is only valid when `suggest=true`. Pass `suggest=true` to emit a starter config, or remove `write_to`."
                    .to_string(),
            );
        }

        if suggest {
            let auto_cluster = params.auto_cluster.unwrap_or(true);

            {
                let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
                let cluster_rows =
                    wiki_cluster_row_count(&conn).map_err(|e| format!("DB error: {e}"))?;
                if cluster_rows == 0 {
                    if !auto_cluster {
                        return Err(
                            "No cluster assignment found. Run `qartez_wiki` first, or pass `auto_cluster=true` (default) to run the clustering on demand."
                                .to_string(),
                        );
                    }
                    let leiden = LeidenConfig::default();
                    compute_clusters(&conn, &leiden)
                        .map_err(|e| format!("Auto-cluster failed: {e}"))?;
                }
            }

            let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
            let files = boundaries_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;
            let clusters =
                boundaries_file_cluster_pairs(&conn).map_err(|e| format!("DB error: {e}"))?;
            let edges = boundaries_edge_pairs(&conn).map_err(|e| format!("DB error: {e}"))?;
            drop(conn);

            if clusters.is_empty() {
                return Err(
                    "Clustering ran but produced no assignments; the import graph is empty. Index the project first (`qartez_workspace refresh=true`) and retry."
                        .to_string(),
                );
            }

            let cfg = suggest_boundaries(&files, &clusters, &edges);
            let toml = render_config_toml(&cfg);

            if !write_to_trimmed.is_empty() {
                let write_abs = resolve_write_target(self, write_to_trimmed)?;
                if let Some(parent) = write_abs.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
                }
                std::fs::write(&write_abs, &toml)
                    .map_err(|e| format!("Cannot write {}: {e}", write_abs.display()))?;
                return Ok(format!(
                    "Wrote {} rule(s) to {} ({} bytes).",
                    cfg.boundary.len(),
                    write_abs.display(),
                    toml.len(),
                ));
            }

            if cfg.boundary.is_empty() {
                return Ok(
                    "No candidate rules: the current clustering has no directory-aligned \
                     partitions to derive rules from. Try `qartez_wiki recompute=true` first."
                        .to_string(),
                );
            }

            return Ok(toml);
        }

        if !abs_path.exists() {
            return Ok(format!(
                "No boundary config at {rel_path}. Run `qartez_boundaries suggest=true write_to={rel_path}` to generate a starter file."
            ));
        }
        let config = load_config(&abs_path).map_err(|e| format!("Config error: {e}"))?;

        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let files = boundaries_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;
        let edges = boundaries_edge_pairs(&conn).map_err(|e| format!("DB error: {e}"))?;
        drop(conn);

        let violations = check_boundaries(&config, &files, &edges);
        if violations.is_empty() {
            return Ok(format!(
                "No boundary violations across {} rule(s) and {} edges.",
                config.boundary.len(),
                edges.len(),
            ));
        }

        let mut out = String::new();
        out.push_str(&format!(
            "{} violation(s) across {} rule(s):\n",
            violations.len(),
            config.boundary.len(),
        ));

        if concise {
            for v in &violations {
                out.push_str(&format!(
                    "{} -> {} (rule #{}: deny {})\n",
                    v.from_file, v.to_file, v.rule_index, v.deny_pattern,
                ));
            }
            return Ok(out);
        }

        let mut current_rule: Option<usize> = None;
        for v in &violations {
            if current_rule != Some(v.rule_index) {
                current_rule = Some(v.rule_index);
                let rule = &config.boundary[v.rule_index];
                out.push_str(&format!(
                    "\nRule #{}: {} !-> {}\n",
                    v.rule_index,
                    rule.from,
                    rule.deny.join(" | "),
                ));
            }
            out.push_str(&format!(
                "  {} -> {} (matched deny pattern: {})\n",
                v.from_file, v.to_file, v.deny_pattern,
            ));
        }
        Ok(out)
    }
}
