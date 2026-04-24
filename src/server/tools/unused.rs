// Rust guideline compliant 2026-04-22
#![allow(unused_imports)]

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::OnceLock;

use regex::RegexSet;
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

/// Directory prefixes whose exported symbols are almost always loaded by an
/// external runtime (plugin host, CLI extension loader, IDE extension API)
/// via string lookup rather than a static import edge. Matching the file's
/// relative path against any of these prefixes short-circuits the unused-
/// export check: the indexer cannot observe the dynamic caller, so the
/// symbol would otherwise be reported as dead even when it is a live entry
/// point. Paths are stored forward-slash-normalized (enforced by
/// `full_index_persists_forward_slash_keys`), so plain `str::starts_with`
/// suffices on all platforms.
const PLUGIN_ENTRY_DIR_PREFIXES: &[&str] = &["scripts/", "plugins/", "extensions/"];

/// Filename patterns that signal a plugin / extension entry-point module.
/// A symbol defined in a file whose basename matches any of these patterns
/// is skipped by `qartez_unused` for the same reason the directory prefixes
/// are skipped - the real caller is an external runtime that resolves
/// exports by string name, so the static reference graph cannot see the
/// edge. Compiled once via `OnceLock` so repeat invocations pay zero regex
/// build cost.
static PLUGIN_ENTRY_BASENAME_PATTERNS: OnceLock<RegexSet> = OnceLock::new();

fn plugin_entry_basename_patterns() -> &'static RegexSet {
    PLUGIN_ENTRY_BASENAME_PATTERNS.get_or_init(|| {
        // Anchored regexes matching the file basename (not the full path).
        // The extension is intentionally left free-form so `.ts`, `.tsx`,
        // `.js`, `.mjs`, `.py`, `.rs`, etc. all match without an explicit
        // allowlist. `[^.]+` forbids a second `.` so we do not over-match
        // unrelated multi-dotted filenames.
        RegexSet::new([
            r"^plugin\.[^.]+$",
            r"^extension\.[^.]+$",
            r"^.+-plugin\.[^.]+$",
            r"^.+-extension\.[^.]+$",
        ])
        .expect("plugin entry-point basename patterns must compile")
    })
}

/// Return `true` when `path` looks like a plugin or extension entry-point
/// file. The check is a cheap path-prefix scan followed by a single
/// `RegexSet::is_match` on the basename, so the cost is constant per row.
fn is_plugin_entry_point_path(path: &str) -> bool {
    if PLUGIN_ENTRY_DIR_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return true;
    }
    let basename = path.rsplit('/').next().unwrap_or(path);
    plugin_entry_basename_patterns().is_match(basename)
}

#[tool_router(router = qartez_unused_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_unused",
        description = "Find dead code: exported symbols with zero importers in the codebase. Safe candidates for removal or inlining. Pre-materialized at index time, so the whole-repo scan is a single indexed SELECT. Pass `limit` / `offset` to page through large result sets. `limit` must be > 0; omit `limit` to accept the 50-row default.",
        annotations(
            title = "Find Unused Exports",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_unused(
        &self,
        Parameters(params): Parameters<SoulUnusedParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;

        // `limit=0` is rejected rather than silently treated as a
        // "no cap" sentinel, matching `qartez_clones` / `qartez_cochange`
        // which already reject `limit=0` explicitly. Keeping the
        // semantics aligned across the tool surface removes the
        // silent divergence where `qartez_unused` alone returned
        // every remaining row for the zero value.
        if let Some(0) = params.limit {
            return Err(
                "limit must be > 0 (use a positive integer; there is no 'no-cap' mode).".into(),
            );
        }
        let limit = match params.limit {
            None => 50_i64,
            Some(n) => n as i64,
        };
        let offset = params.offset.unwrap_or(0) as i64;

        let total = read::count_unused_exports(&conn).map_err(|e| format!("DB error: {e}"))?;
        if total == 0 {
            return Ok("No unused exported symbols detected.".to_string());
        }

        // Plugin / extension entry-point files are loaded by external
        // runtimes via string lookup (e.g. OpenCode `Plugin` exports,
        // VS Code `activate` handlers, CLI script hooks). The static
        // reference graph cannot observe those callers, so the row
        // survives `NOT EXISTS (... symbol_refs ...)` and gets reported
        // as unused even when it is a live entry point. Over-sample
        // from the DB and drop those rows here so the caller-visible
        // page is always `limit` post-filter rows (unless the DB is
        // exhausted). Before oversampling, a page that happened to
        // contain a plugin entry produced off-by-one counters like
        // "10 unused; showing 9" or "limit=5 returns 4" that looked
        // like a pagination bug.
        const FETCH_PAGE_SIZE: i64 = 64;
        let mut page: Vec<(
            crate::storage::models::SymbolRow,
            crate::storage::models::FileRow,
        )> = Vec::new();
        let mut fetch_offset = offset;
        let mut plugin_filtered = 0i64;
        loop {
            let remaining_room = (limit - page.len() as i64).max(0);
            if remaining_room == 0 {
                break;
            }
            let batch_size = remaining_room.max(FETCH_PAGE_SIZE);
            let batch = read::get_unused_exports_page(&conn, batch_size, fetch_offset)
                .map_err(|e| format!("DB error: {e}"))?;
            if batch.is_empty() {
                break;
            }
            let batch_len = batch.len() as i64;
            fetch_offset += batch_len;
            for row in batch {
                if is_plugin_entry_point_path(&row.1.path) {
                    plugin_filtered += 1;
                    continue;
                }
                if page.len() as i64 >= limit {
                    break;
                }
                page.push(row);
            }
            if batch_len < batch_size {
                break;
            }
        }
        let next_offset = fetch_offset;

        if page.is_empty() {
            return Ok(format!(
                "No unused exports in page (total={total}, offset={offset}; {plugin_filtered} plugin-manifest entries hidden - they're intentional)."
            ));
        }

        let shown = page.len() as i64;
        // `N plugin-manifest entries hidden - they're intentional`
        // replaces the bare `plugin_entries_skipped=N` counter used
        // before. The old key/value pair looked like a pagination
        // bug to callers ("why is this counter non-zero? what did I
        // do wrong?") because the tool never documented that the
        // filter always runs. The new phrasing names the filter,
        // explains why entries are hidden, and places the counter
        // inside a parenthetical so it reads as a note instead of
        // a flag to investigate.
        let mut out = if next_offset < total {
            if plugin_filtered > 0 {
                format!(
                    "{total} unused export(s); showing {shown} from offset {offset} (next: offset={next_offset}; {plugin_filtered} plugin-manifest entries hidden - they're intentional).\n",
                )
            } else {
                format!(
                    "{total} unused export(s); showing {shown} from offset {offset} (next: offset={next_offset}).\n",
                )
            }
        } else if plugin_filtered > 0 {
            format!(
                "{total} unused export(s); showing {shown} of {total} ({plugin_filtered} plugin-manifest entries hidden - they're intentional).\n",
            )
        } else {
            format!("{total} unused export(s).\n")
        };

        // Compact per-file format: one header per file, one line per symbol
        // without the parenthesized kind (it's redundant with the kind-letter
        // prefix). Saves ~40% tokens vs the old `  - name (kind) [L-L]` shape.
        let mut current_path: &str = "";
        for (sym, file) in &page {
            if file.path != current_path {
                out.push_str(&format!("{}\n", file.path));
                current_path = file.path.as_str();
            }
            out.push_str(&format!(
                "  {} {} L{}\n",
                sym.kind.chars().next().unwrap_or(' '),
                sym.name,
                sym.line_start,
            ));
        }
        Ok(out)
    }
}
