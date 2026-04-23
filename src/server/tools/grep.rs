#![allow(unused_imports)]

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

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

#[tool_router(router = qartez_grep_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_grep",
        description = "Search indexed symbols by name, kind, or file path using FTS5. Use prefix matching (e.g., 'Config*') for fuzzy search. Returns symbol locations with export status. Faster than Grep — searches the index, not disk.",
        annotations(
            title = "Search Symbols",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_grep(
        &self,
        Parameters(params): Parameters<SoulGrepParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_grep")?;
        if params.query.trim().is_empty() {
            return Err("query must be non-empty".to_string());
        }
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        // Default is large so the token_budget stays the active governor for
        // output volume. Callers that need a hard cap still set `limit` or
        // `token_budget` explicitly.
        let limit = params.limit.unwrap_or(200) as i64;
        let budget = params.token_budget.unwrap_or(4000) as usize;
        let concise = is_concise(&params.format);
        let use_regex = params.regex.unwrap_or(false);
        let search_bodies = params.search_bodies.unwrap_or(false);

        let results: Vec<(crate::storage::models::SymbolRow, String)> = if search_bodies {
            let fts_query = sanitize_fts_query(&params.query);
            read::search_symbol_bodies_fts(&conn, &fts_query, limit).map_err(|e| {
                format!(
                    "body FTS error: {e}. Try regex=true or drop search_bodies for symbol-name search.",
                )
            })?
        } else if use_regex {
            let re = regex::Regex::new(&params.query).map_err(|e| format!("regex error: {e}"))?;
            let all =
                read::get_all_symbols_with_path(&conn).map_err(|e| format!("DB error: {e}"))?;
            all.into_iter()
                .filter(|(s, _)| re.is_match(&s.name))
                .take(limit as usize)
                .collect()
        } else {
            let fts_query = sanitize_fts_query(&params.query);
            read::search_symbols_fts(&conn, &fts_query, limit).map_err(|e| {
                format!(
                    "FTS error: {e}. Try regex=true for source-code patterns like `#[tool` or `Foo::bar`.",
                )
            })?
        };

        if results.is_empty() {
            return Ok(format!("No symbols matching '{}'", params.query));
        }

        let mut out = format!(
            "Found {} result(s) for '{}':\n\n",
            results.len(),
            params.query,
        );
        for (sym, file_path) in &results {
            let line = if concise {
                let marker = if sym.is_exported { "+" } else { " " };
                format!(
                    " {marker} {} — {} [L{}]\n",
                    sym.name, file_path, sym.line_start
                )
            } else {
                let exported = if sym.is_exported { "+" } else { " " };
                format!(
                    " {exported} {:<30} {:<12} {}  [L{}-L{}]\n",
                    sym.name, sym.kind, file_path, sym.line_start, sym.line_end,
                )
            };
            if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                out.push_str("  ... (truncated by token budget)\n");
                break;
            }
            out.push_str(&line);

            // When search_bodies=true, the symbol header only reports the
            // enclosing range. Add per-line snippets that show which lines
            // inside the body actually matched, so callers do not have to
            // follow up with qartez_read just to locate the hit.
            if search_bodies {
                let preview =
                    self.body_match_preview(file_path, sym, &params.query, use_regex, budget);
                if estimate_tokens(&out) + estimate_tokens(&preview) > budget {
                    out.push_str("  ... (truncated by token budget)\n");
                    break;
                }
                out.push_str(&preview);
            }
        }
        Ok(out)
    }

    /// Render up to a few concrete line-level matches inside a symbol's
    /// body. Best-effort: on read failure we simply skip the preview so
    /// the caller still sees the symbol-level hit.
    fn body_match_preview(
        &self,
        file_path: &str,
        sym: &crate::storage::models::SymbolRow,
        query: &str,
        use_regex: bool,
        budget: usize,
    ) -> String {
        const MAX_PREVIEW_LINES: usize = 5;
        const MAX_SNIPPET_LEN: usize = 120;

        let Ok(abs_path) = self.safe_resolve(file_path) else {
            return String::new();
        };
        let Ok(source) = std::fs::read_to_string(&abs_path) else {
            return String::new();
        };
        let lines: Vec<&str> = source.lines().collect();
        let start_idx = (sym.line_start as usize).saturating_sub(1);
        let end_idx = (sym.line_end as usize).min(lines.len());
        if start_idx >= end_idx {
            return String::new();
        }

        let re = if use_regex {
            regex::Regex::new(query).ok()
        } else {
            None
        };
        let needle_lower = query.to_lowercase();

        let mut out = String::new();
        let mut shown = 0usize;
        for (offset, raw_line) in lines[start_idx..end_idx].iter().enumerate() {
            let line_no = start_idx + offset + 1;
            let hit = match (&re, use_regex) {
                (Some(pat), true) => pat.is_match(raw_line),
                _ => raw_line.to_lowercase().contains(&needle_lower),
            };
            if !hit {
                continue;
            }
            let trimmed = raw_line.trim();
            let snippet = if trimmed.chars().count() > MAX_SNIPPET_LEN {
                let cut: String = trimmed.chars().take(MAX_SNIPPET_LEN).collect();
                format!("{cut}...")
            } else {
                trimmed.to_string()
            };
            let row = format!("      L{line_no}: {snippet}\n");
            if estimate_tokens(&out) + estimate_tokens(&row) > budget {
                break;
            }
            out.push_str(&row);
            shown += 1;
            if shown >= MAX_PREVIEW_LINES {
                break;
            }
        }
        out
    }
}
