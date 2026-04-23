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

#[tool_router(router = qartez_trend_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_trend",
        description = "Show how a symbol's cyclomatic complexity changed over recent commits. Unlike qartez_hotspots (point-in-time), this reveals whether code is actively getting more complex (e.g. 'function grew from CC 8 to CC 39 over 5 commits'). Pass a file_path and optionally a symbol_name to focus on one function.",
        annotations(
            title = "Complexity Trend",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_trend(
        &self,
        Parameters(params): Parameters<SoulTrendParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_trend")?;
        if self.git_depth == 0 {
            return Err(
                "Complexity trend requires git history. Re-index with --git-depth > 0.".into(),
            );
        }

        // Strict server-side clamp. The tool description promises a 50-commit
        // cap; clamping here keeps the documented contract visible in the
        // tool layer instead of relying on a sibling crate's private
        // constant.
        const MAX_COMMIT_LIMIT: u32 = 50;
        let limit = params.limit.unwrap_or(10).clamp(1, MAX_COMMIT_LIMIT);
        let concise = matches!(params.format, Some(Format::Concise));

        // Token-budget truncation caps the rendered report so a big file
        // (dozens of symbols x 50 commits) cannot overflow the MCP
        // transport. 512 is the floor so degenerate values do not emit an
        // empty payload.
        const DEFAULT_TOKEN_BUDGET: u32 = 4_000;
        let token_budget = params.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET).max(512) as usize;

        // Normalise the symbol filter so trimmed / empty forms collapse to
        // the "no filter" branch. Without this, `symbol_name=""` was a
        // valid `Some("")` that never matched anything and produced an
        // empty trend list instead of the documented "all symbols" fall
        // back.
        let symbol_filter: Option<&str> = params
            .symbol_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let mut trends = crate::git::trend::complexity_trend(
            &self.project_root,
            &params.file_path,
            symbol_filter,
            limit,
        )
        .map_err(|e| format!("trend analysis failed: {e}"))?;

        // Belt-and-braces post-filter: keep exactly the requested symbol
        // even if the underlying parser produced homonyms. Without this,
        // a file with two `fn foo()` in separate `impl` blocks would
        // return both trends when the caller asked for one.
        if let Some(filter) = symbol_filter {
            trends.retain(|t| t.symbol_name == filter);
        }

        // When no symbol is targeted, sort by absolute delta-percent
        // descending so GROWING / SHRINKING trends surface first and the
        // STABLE majority falls to the bottom where `token_budget` can
        // truncate it. Without this, a file with 20 stable and 1
        // growing symbol buried the signal: 100 commit lines of "STABLE
        // +0%" filled the budget before the growing function was
        // rendered. Ties break on symbol name so the rendering is
        // deterministic across reruns against the same HEAD.
        if symbol_filter.is_none() {
            trends.sort_by(|a, b| {
                let abs_a = trend_abs_delta(a);
                let abs_b = trend_abs_delta(b);
                abs_b
                    .partial_cmp(&abs_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.symbol_name.cmp(&b.symbol_name))
            });
        }

        if trends.is_empty() {
            return Ok(format!(
                "No complexity trend data for `{}`. Possible reasons: file has fewer than 2 commits, no functions with measurable complexity, or symbol not found.",
                params.file_path
            ));
        }

        // Approximate char-to-token ratio; the MCP server elsewhere uses
        // the same 4:1 heuristic so the limits stay consistent across
        // tools.
        const CHARS_PER_TOKEN: usize = 4;
        let max_chars = token_budget.saturating_mul(CHARS_PER_TOKEN);

        let mut out = String::new();

        if concise {
            out.push_str("# symbol commits first_cc last_cc delta% file\n");
            for t in &trends {
                let first_cc = t.points.first().map(|p| p.complexity).unwrap_or(0);
                let last_cc = t.points.last().map(|p| p.complexity).unwrap_or(0);
                let delta = if first_cc > 0 {
                    ((last_cc as f64 - first_cc as f64) / first_cc as f64 * 100.0) as i64
                } else {
                    0
                };
                out.push_str(&format!(
                    "{} {} {} {} {}% {}\n",
                    t.symbol_name,
                    t.points.len(),
                    first_cc,
                    last_cc,
                    delta,
                    t.file_path,
                ));
            }
        } else {
            out.push_str(&format!("# Complexity Trend: {}\n\n", params.file_path));

            for t in &trends {
                let first_cc = t.points.first().map(|p| p.complexity).unwrap_or(0);
                let last_cc = t.points.last().map(|p| p.complexity).unwrap_or(0);
                let delta = if first_cc > 0 {
                    (last_cc as f64 - first_cc as f64) / first_cc as f64 * 100.0
                } else {
                    0.0
                };

                let direction = if delta > 10.0 {
                    "GROWING"
                } else if delta < -10.0 {
                    "SHRINKING"
                } else {
                    "STABLE"
                };

                out.push_str(&format!(
                    "## {} ({}) CC {} -> {} ({:+.0}% {})\n\n",
                    t.symbol_name,
                    t.points.len(),
                    first_cc,
                    last_cc,
                    delta,
                    direction,
                ));

                out.push_str("  Commit  | CC | Lines | Summary\n");
                out.push_str("  --------+----+-------+--------\n");

                for (i, p) in t.points.iter().enumerate() {
                    let marker = if i > 0 {
                        let prev = t.points[i - 1].complexity;
                        if p.complexity > prev {
                            " (+)"
                        } else if p.complexity < prev {
                            " (-)"
                        } else {
                            ""
                        }
                    } else {
                        ""
                    };

                    out.push_str(&format!(
                        "  {} | {:>2}{:<4} | {:>5} | {}\n",
                        p.commit_sha, p.complexity, marker, p.line_count, p.commit_summary,
                    ));
                }
                out.push('\n');
            }
        }

        // Token-budget enforcement: if the rendered report exceeds the
        // cap, truncate at a safe UTF-8 boundary and append a footer so
        // the caller knows the response is partial. Without this, a 50 x
        // N rendering could exceed 300k chars and get rejected by the
        // transport.
        if out.len() > max_chars {
            let mut cut = max_chars;
            while cut > 0 && !out.is_char_boundary(cut) {
                cut -= 1;
            }
            out.truncate(cut);
            out.push_str(&format!(
                "\n... output truncated at ~{token_budget} tokens. Narrow with `symbol_name=` or lower `limit=`.\n",
            ));
        }

        Ok(out)
    }
}

/// Absolute delta-percent between first and last commit-point of a
/// trend. Returns `0.0` when `first_cc == 0` so "0 -> 0" ties with
/// other stable symbols instead of becoming infinite. Used to rank
/// trends so GROWING / SHRINKING signal surfaces above STABLE noise.
fn trend_abs_delta(trend: &crate::git::trend::SymbolTrend) -> f64 {
    let first = trend.points.first().map(|p| p.complexity).unwrap_or(0) as f64;
    let last = trend.points.last().map(|p| p.complexity).unwrap_or(0) as f64;
    if first <= 0.0 {
        return 0.0;
    }
    ((last - first) / first * 100.0).abs()
}
