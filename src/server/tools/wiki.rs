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
/// `safe_resolve`) or an absolute path rooted in the project, `/tmp`,
/// or the user's `$HOME`. The sandbox prefix list mirrors the rule
/// callers reach for most often: "a scratch file next to my work" or
/// "a throwaway under /tmp". An audit found the previous check only
/// required the parent directory to exist, which let a stray absolute
/// path land ARCHITECTURE.md anywhere on disk.
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
        let home = std::env::var("HOME").ok().map(PathBuf::from);
        // macOS resolves `std::env::temp_dir()` to `/var/folders/<hash>/T/...`
        // (via `TMPDIR`), not `/tmp`. Accept the OS-reported tempdir as
        // well so `TempDir`-based tests and platform-sensible scratch
        // writes do not hit the sandbox guard. `/tmp` stays explicit for
        // Linux parity.
        let os_tmp = std::env::temp_dir();
        let inside_project = candidate.starts_with(&server.project_root);
        let inside_tmp = candidate.starts_with("/tmp") || candidate.starts_with(&os_tmp);
        let inside_home = home.as_ref().is_some_and(|h| candidate.starts_with(h));
        if !(inside_project || inside_tmp || inside_home) {
            return Err(format!(
                "write_to absolute path '{}' is outside project root, tmpdir ({}), and $HOME. Use a relative path or one of those prefixes.",
                candidate.display(),
                os_tmp.display(),
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

        // Louvain-style modularity is only defined for strictly positive
        // resolutions. A value of 0 collapses every node onto the zero
        // axis so the optimiser returns nonsense modularity, and
        // negative resolutions (e.g. -1.0) produced a modularity of
        // 1.17 which callers mistook for a score above the theoretical
        // 1.0 ceiling. Reject values outside (0.0, 10.0] with an
        // explicit range so the caller sees their input was wrong
        // rather than trusting an impossible score. The 10.0 ceiling
        // matches the Leiden literature recommendation: higher values
        // fragment every node into its own cluster, which produces a
        // meaningless wiki.
        if let Some(r) = params.resolution {
            if !r.is_finite() || r <= 0.0 {
                return Err(format!(
                    "resolution must be > 0.0 (got {r}). Valid range: (0.0, 10.0]. Typical values: 0.5 (coarse clusters), 1.0 (default), 2.0-5.0 (fine-grained)."
                ));
            }
            if r > 10.0 {
                return Err(format!(
                    "resolution must be <= 10.0 (got {r}). Valid range: (0.0, 10.0]. Values above 10 fragment every file into its own cluster and defeat the summary."
                ));
            }
        }

        let leiden = LeidenConfig {
            resolution: params.resolution.unwrap_or(1.0),
            min_cluster_size: params.min_cluster_size.unwrap_or(3),
            ..Default::default()
        };

        // Cache-key parity: the cluster assignments stored in
        // `file_clusters` are computed from a specific
        // (resolution, min_cluster_size) pair. A previous call with
        // `min_cluster_size=999` collapsed the project to a single
        // cluster and persisted that; a follow-up default call then
        // reused the cached 1-cluster result, making the knob appear
        // sticky across invocations. Track the last config in
        // `.qartez/wiki-cluster-key` and force a recompute whenever
        // the current fingerprint differs, so subsequent calls with
        // different parameters observe their configured granularity.
        let config_fingerprint = format!("{:.6}:{}", leiden.resolution, leiden.min_cluster_size,);
        let key_path = self.project_root.join(".qartez").join("wiki-cluster-key");
        let stored_fingerprint = std::fs::read_to_string(&key_path).ok();
        let fingerprint_mismatch = stored_fingerprint.as_deref().map(str::trim).unwrap_or("")
            != config_fingerprint.as_str();
        let recompute = params.recompute.unwrap_or(false)
            || resolution_explicit
            || min_cluster_size_explicit
            || fingerprint_mismatch;

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

        // Stamp the config fingerprint so the NEXT call can tell
        // whether its requested params still match the cached
        // clusters. Only update when we actually recomputed; otherwise
        // the stored key would drift from the real on-disk state.
        if recompute {
            if let Some(parent) = key_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&key_path, &config_fingerprint);
        }

        let mod_line = modularity
            .map(|q| format!(", modularity {q:.2}"))
            .unwrap_or_default();
        let cluster_count = markdown
            .lines()
            .filter(|l| l.starts_with("## ") && !l.starts_with("## Table of contents"))
            .count();

        // Inject the modularity score into the on-disk markdown so the
        // file written to disk matches the header shape rendered
        // inline. Without this, readers of ARCHITECTURE.md saw a
        // cluster count but no quality signal for the partition, while
        // the inline response included "modularity Q.QQ". We insert
        // after the second-level `## Table of contents` heading if
        // present, otherwise prepend so the note never gets lost.
        let markdown_for_disk = if let Some(q) = modularity {
            let banner = format!("Modularity: {q:.2}\n");
            let anchor = "## Table of contents";
            if let Some(pos) = markdown.find(anchor) {
                let (head, tail) = markdown.split_at(pos);
                format!("{head}{banner}\n{tail}")
            } else {
                format!("{banner}\n{markdown}")
            }
        } else {
            markdown.clone()
        };

        let write_to_trimmed = params.write_to.as_deref().map(str::trim).unwrap_or("");
        if !write_to_trimmed.is_empty() {
            let abs = resolve_write_target(self, write_to_trimmed)?;
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
            }
            std::fs::write(&abs, &markdown_for_disk)
                .map_err(|e| format!("Cannot write {}: {e}", abs.display()))?;
            return Ok(format!(
                "Wrote {} bytes to {} ({} clusters{})",
                markdown_for_disk.len(),
                abs.display(),
                cluster_count,
                mod_line,
            ));
        }

        // Leiden resolution semantics: larger = more clusters. The
        // optimizer penalizes merges harder with a higher gamma in the
        // `louvain_local_move` gain formula, so nodes stay split. If
        // you ever observe the inverse (e.g. resolution=0.5 produces
        // MORE clusters than resolution=2.0), audit the modularity
        // objective in `compute_modularity` -- not this validation.
        if let Some(budget) = params.token_budget {
            if (budget as usize) < 1024 {
                return Err(format!(
                    "token_budget must be >= 1024 to produce a useful wiki (got {budget}). Pass write_to=<path> to bypass the budget entirely."
                ));
            }
        }
        let token_budget = params
            .token_budget
            .map(|v| v as usize)
            .unwrap_or(WIKI_DEFAULT_TOKEN_BUDGET);
        Ok(truncate_to_budget(&markdown, cluster_count, token_budget))
    }
}
