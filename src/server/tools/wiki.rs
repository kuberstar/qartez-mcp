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

/// Default token budget for inline `qartez_wiki` output.
///
/// Keeps the response within a single Claude turn on projects with
/// hundreds of clusters. Callers can override with `token_budget=<N>`
/// or bypass the cap entirely by writing to disk with `write_to=<path>`.
const WIKI_DEFAULT_TOKEN_BUDGET: usize = 8000;

/// Resolve a user-supplied write target for `qartez_wiki`.
///
/// Accepts either a path relative to the project root (delegates to
/// `safe_resolve`) or an absolute path whose parent directory already
/// exists. Keeps the policy aligned with `qartez_boundaries`.
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

/// Cap `markdown` at `token_budget` approximate tokens, preserving the
/// header and cluster-count summary so callers can still see overall
/// scope. When truncation fires, a footer points at `write_to` as the
/// escape hatch for the full wiki and suggests a concrete
/// `token_budget` value large enough to render every cluster.
fn truncate_to_budget(markdown: &str, cluster_count: usize, token_budget: usize) -> String {
    let estimated = helpers::estimate_tokens(markdown);
    if estimated <= token_budget {
        return markdown.to_string();
    }

    // 3 chars/token matches `estimate_tokens`; use the same ratio when
    // converting the budget back to bytes so the cap is consistent.
    const CHARS_PER_TOKEN: usize = 3;
    let char_budget = token_budget.saturating_mul(CHARS_PER_TOKEN);
    let mut truncated = String::with_capacity(char_budget + 256);
    let mut char_count = 0usize;
    let mut clusters_rendered = 0usize;
    let mut last_section_break = 0usize;
    for line in markdown.lines() {
        let line_chars = line.chars().count() + 1;
        if char_count + line_chars > char_budget {
            break;
        }
        truncated.push_str(line);
        truncated.push('\n');
        char_count += line_chars;
        if line.starts_with("## ") && !line.starts_with("## Table of contents") {
            clusters_rendered = clusters_rendered.saturating_add(1);
            last_section_break = truncated.len();
        }
    }

    // Snap back to the most recent cluster boundary so the truncated
    // markdown always ends at a complete section.
    if last_section_break > 0 && last_section_break < truncated.len() {
        truncated.truncate(last_section_break);
    }

    let remaining = cluster_count.saturating_sub(clusters_rendered);
    // Suggest a concrete `token_budget` large enough to render every
    // cluster: take the full markdown estimate and round up to the
    // next 1000-token multiple so the suggested value is easy to read
    // and leaves a small headroom over the raw size. We floor the
    // suggestion at `token_budget + 1000` so the recommendation is
    // always strictly larger than the current cap.
    let suggested_budget = {
        let next_k = estimated.saturating_add(999) / 1000 * 1000;
        next_k.max(token_budget.saturating_add(1000))
    };
    truncated.push('\n');
    truncated.push_str(&format!(
        "Showing {clusters_rendered}/{cluster_count} clusters ({remaining} truncated). Set token_budget={suggested_budget} to see all, or pass write_to=<path> to write the full wiki to disk.\n"
    ));
    truncated
}

#[tool_router(router = qartez_wiki_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_wiki",
        description = "Generate a markdown architecture wiki from Leiden-style community detection on the import graph. Partitions files into clusters, names each by the shared path prefix or top-PageRank file, and emits ARCHITECTURE.md with per-cluster file lists, top exported symbols, and inter-cluster edges. Use write_to=null to return the markdown as a string (capped by `token_budget`, default 8000), or write_to=<path> to save the full wiki to disk. `write_to` accepts a project-relative or absolute path whose parent exists. `resolution` controls cluster granularity (default 1.0; higher = more clusters). Passing an explicit `resolution` or `min_cluster_size` forces a cluster recompute so the new value takes effect.",
        annotations(
            title = "Architecture Wiki",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_wiki(
        &self,
        Parameters(params): Parameters<SoulWikiParams>,
    ) -> Result<String, String> {
        use crate::graph::boundaries::{check_boundaries, load_config};
        use crate::graph::leiden::LeidenConfig;
        use crate::graph::wiki::{WikiConfig, render_wiki};
        use crate::storage::read::{get_all_edges, get_all_files};

        // Force a cluster recompute whenever the caller explicitly
        // passes `resolution` or `min_cluster_size`. Without this, the
        // wiki silently reuses cached assignments and the new knob has
        // no observable effect (cf. `render_wiki`'s cache check on
        // `get_file_clusters_count`).
        let resolution_explicit = params.resolution.is_some();
        let min_cluster_size_explicit = params.min_cluster_size.is_some();
        let recompute =
            params.recompute.unwrap_or(false) || resolution_explicit || min_cluster_size_explicit;

        let leiden = LeidenConfig {
            resolution: params.resolution.unwrap_or(1.0),
            min_cluster_size: params.min_cluster_size.unwrap_or(3),
            ..Default::default()
        };
        let mut wiki_cfg = WikiConfig {
            project_name: self.project_name(),
            max_files_per_section: params
                .max_files_per_section
                .map(|v| v as usize)
                .unwrap_or(20),
            recompute,
            leiden,
            ..Default::default()
        };

        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;

        let boundary_config_path = self.project_root.join(".qartez/boundaries.toml");
        if boundary_config_path.exists() {
            match load_config(&boundary_config_path) {
                Ok(cfg) => {
                    let files = get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;
                    let edges = get_all_edges(&conn).map_err(|e| format!("DB error: {e}"))?;
                    wiki_cfg.boundary_violations = Some(check_boundaries(&cfg, &files, &edges));
                }
                Err(e) => {
                    tracing::warn!("boundary config parse failed: {e}");
                }
            }
        }

        let (markdown, modularity) =
            render_wiki(&conn, &wiki_cfg).map_err(|e| format!("Wiki render error: {e}"))?;
        drop(conn);

        let mod_line = modularity
            .map(|q| format!(", modularity {q:.2}"))
            .unwrap_or_default();
        let cluster_count = markdown
            .lines()
            .filter(|l| l.starts_with("## ") && !l.starts_with("## Table of contents"))
            .count();

        let write_to_trimmed = params.write_to.as_deref().map(str::trim).unwrap_or("");
        if !write_to_trimmed.is_empty() {
            let abs = resolve_write_target(self, write_to_trimmed)?;
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
            }
            std::fs::write(&abs, &markdown)
                .map_err(|e| format!("Cannot write {}: {e}", abs.display()))?;
            return Ok(format!(
                "Wrote {} bytes to {} ({} clusters{})",
                markdown.len(),
                abs.display(),
                cluster_count,
                mod_line,
            ));
        }

        let token_budget = params
            .token_budget
            .map(|v| v as usize)
            .unwrap_or(WIKI_DEFAULT_TOKEN_BUDGET);
        Ok(truncate_to_budget(&markdown, cluster_count, token_budget))
    }
}
