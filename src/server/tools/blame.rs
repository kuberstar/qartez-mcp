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

#[tool_router(router = qartez_blame_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_blame",
        description = "Symbol-level git blame: who last touched this function/struct and what was their commit message? Resolves a symbol to its file and line range, then runs git blame scoped to those lines. Use aggregate=true for a per-author summary.",
        annotations(
            title = "Symbol Blame",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_blame(
        &self,
        Parameters(params): Parameters<SoulBlameParams>,
    ) -> Result<String, String> {
        if self.git_depth == 0 {
            return Err("Symbol blame requires git history. Re-index with --git-depth > 0.".into());
        }

        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let mut symbols = read::find_symbol_by_name(&conn, &params.symbol)
            .map_err(|e| format!("DB error: {e}"))?;
        drop(conn);

        if symbols.is_empty() {
            return Err(format!("Symbol '{}' not found in index", params.symbol));
        }

        if let Some(ref fp) = params.file_path {
            symbols.retain(|(_, f)| f.path.contains(fp.as_str()));
            if symbols.is_empty() {
                return Err(format!(
                    "Symbol '{}' not found in files matching '{}'",
                    params.symbol, fp
                ));
            }
        }

        if symbols.len() > 1 && params.file_path.is_none() {
            let files: Vec<&str> = symbols.iter().map(|(_, f)| f.path.as_str()).collect();
            return Err(format!(
                "Symbol '{}' found in {} files. Pass file_path to disambiguate:\n  {}",
                params.symbol,
                files.len(),
                files.join("\n  ")
            ));
        }

        let (sym, def_file) = &symbols[0];
        let file_path = &def_file.path;
        let line_start = sym.line_start;
        let line_end = sym.line_end;

        let hunks =
            crate::git::blame::symbol_blame(&self.project_root, file_path, line_start, line_end)
                .map_err(|e| format!("blame failed: {e}"))?;

        if hunks.is_empty() {
            return Ok(format!(
                "No blame data for {} @ {}:L{}-{}",
                params.symbol, file_path, line_start, line_end
            ));
        }

        let limit = params.limit.unwrap_or(20) as usize;
        let token_budget = params.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET as u32) as usize;
        let concise = matches!(params.format, Some(Format::Concise));
        let aggregate = params.aggregate.unwrap_or(false);

        if aggregate {
            let mut author_lines: HashMap<String, (u32, String)> = HashMap::new();
            for h in &hunks {
                let entry = author_lines
                    .entry(h.author.clone())
                    .or_insert((0, String::new()));
                entry.0 += h.lines;
                if entry.1.is_empty() {
                    entry.1 = format!("{} {}", h.commit_sha, h.commit_summary);
                }
            }
            let total_lines: u32 = hunks.iter().map(|h| h.lines).sum();
            let mut authors: Vec<(String, u32, String)> = author_lines
                .into_iter()
                .map(|(name, (lines, latest))| (name, lines, latest))
                .collect();
            authors.sort_by(|a, b| b.1.cmp(&a.1));
            authors.truncate(limit);

            let items: Vec<(f64, String)> = authors
                .iter()
                .enumerate()
                .map(|(i, (name, lines, latest))| {
                    let pct = if total_lines > 0 {
                        *lines as f64 / total_lines as f64 * 100.0
                    } else {
                        0.0
                    };
                    let line = if concise {
                        format!("{} {} {:.0}% {}\n", i + 1, name, pct, lines)
                    } else {
                        format!(
                            "{:>3} | {:<20} | {:>5} | {:>4.0}% | {}\n",
                            i + 1,
                            truncate_path(name, 20),
                            lines,
                            pct,
                            latest,
                        )
                    };
                    (*lines as f64, line)
                })
                .collect();

            let mut out = format!(
                "# blame (aggregate): {} @ {}:L{}-{}\n\n",
                params.symbol, file_path, line_start, line_end
            );
            if !concise {
                out.push_str("  # | Author               | Lines |    % | Sample Commit\n");
                out.push_str("----+----------------------+-------+------+--------------\n");
            }
            out.push_str(&budget_render(&items, token_budget));
            Ok(out)
        } else {
            let display_hunks: Vec<&crate::git::blame::BlameHunk> =
                hunks.iter().take(limit).collect();

            let items: Vec<(f64, String)> = display_hunks
                .iter()
                .map(|h| {
                    let line = if concise {
                        format!(
                            "L{}-{} {} {} {}\n",
                            h.line_start, h.line_end, h.author, h.commit_sha, h.commit_summary
                        )
                    } else {
                        format!(
                            "L{:<5}-{:<5} {:<20} {} {}\n",
                            h.line_start,
                            h.line_end,
                            truncate_path(&h.author, 20),
                            h.commit_sha,
                            h.commit_summary,
                        )
                    };
                    (-(h.line_start as f64), line)
                })
                .collect();

            let total_lines: u32 = hunks.iter().map(|h| h.lines).sum();
            let mut author_set: HashMap<&str, u32> = HashMap::new();
            for h in &hunks {
                *author_set.entry(&h.author).or_insert(0) += h.lines;
            }
            let mut authors_sorted: Vec<(&str, u32)> = author_set.into_iter().collect();
            authors_sorted.sort_by(|a, b| b.1.cmp(&a.1));

            let mut out = format!(
                "# blame: {} @ {}:L{}-{}\n\n",
                params.symbol, file_path, line_start, line_end
            );
            out.push_str(&budget_render(&items, token_budget));
            out.push_str(&format!(
                "\nauthors: {}\n",
                authors_sorted
                    .iter()
                    .map(|(name, lines)| {
                        let pct = *lines as f64 / total_lines as f64 * 100.0;
                        format!("{name} ({pct:.0}%)")
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            Ok(out)
        }
    }
}
