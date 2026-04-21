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
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let limit = params.limit.unwrap_or(20) as i64;
        let budget = params.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET as u32) as usize;
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
            if budget_exceeded(&mut out, &line, budget) {
                break;
            }
            out.push_str(&line);
        }
        Ok(out)
    }
}
