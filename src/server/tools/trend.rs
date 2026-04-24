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

        // Distinguish "file absent from the index" from "file present
        // but without per-symbol complexity data". Previously both
        // cases collapsed onto the same "No complexity trend data"
        // message, so callers chasing a typo in `file_path` could not
        // tell whether to fix the path or widen the filter. Scope the
        // lookup to its own DB lock so the rest of the analysis does
        // not hold onto the handle while walking git history.
        let file_row = {
            let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
            read::get_file_by_path(&conn, &params.file_path)
                .map_err(|e| format!("DB error: {e}"))?
        };
        let Some(file_row) = file_row else {
            return Err(format!(
                "File '{}' not found in index. Check the path (must be project-relative) or re-index with `qartez index`.",
                params.file_path,
            ));
        };
        // Cache the index-time commit count so the empty-result path
        // can distinguish "fewer than 2 commits touched this file"
        // from "symbol not found" / "complexity unmeasurable" without
        // a second DB round-trip.
        let file_commit_count = file_row.change_count;

        // Strict server-side clamp. The tool description promises a 50-commit
        // cap; clamping here keeps the documented contract visible in the
        // tool layer instead of relying on a sibling crate's private
        // constant.
        const MAX_COMMIT_LIMIT: u32 = 50;
        // `limit=0` previously flowed through `clamp(1, 50)` and
        // silently became `1`, which emitted the same "No data"
        // message the real no-history path produces and hid the
        // caller's bad input. Reject explicitly so the distinction
        // between "no trend data" and "invalid limit" is visible.
        if let Some(0) = params.limit {
            return Err(
                "limit must be > 0 (use a positive integer; there is no 'no-cap' mode).".into(),
            );
        }
        let requested_limit = params.limit.unwrap_or(10);
        let limit = requested_limit.clamp(1, MAX_COMMIT_LIMIT);
        let limit_was_clamped = requested_limit != limit;
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
            // Disambiguate the three reasons a trend can come back
            // empty. Emit exactly one cause so the caller doesn't
            // need to reverse-engineer which of the three to fix.
            //   1) explicit symbol_name that doesn't resolve to any
            //      trend point - the caller picked the wrong name.
            //   2) fewer than 2 commits touched the file - git
            //      history is too shallow to measure a delta.
            //   3) file is indexed with commits but no function had
            //      measurable complexity (non-code, generated, or
            //      the parser skipped every body).
            // The clamp notice still runs on every path so callers
            // always see that their requested limit was rewritten,
            // regardless of which cause fired.
            let mut msg = if let Some(sym) = symbol_filter {
                format!("Symbol '{sym}' not found in file '{}'.", params.file_path,)
            } else if file_commit_count < 2 {
                format!(
                    "Only {file_commit_count} commit(s) touched '{}'. Need at least 2 to measure a trend.",
                    params.file_path,
                )
            } else {
                format!(
                    "Complexity not computable for '{}'. The file may have been non-code.",
                    params.file_path,
                )
            };
            if limit_was_clamped {
                msg.push_str(&format!(
                    "\nnote: limit={requested_limit} was clamped to {MAX_COMMIT_LIMIT} (server-side commit cap).",
                ));
            }
            return Ok(msg);
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

            // When the caller did not pin a symbol, skip the per-commit
            // table for STABLE trends (|delta| <= 10%). Those rows
            // carried no signal and, on a 20+ symbol file, consumed the
            // full token budget before the GROWING/SHRINKING rows
            // rendered. Callers who want the per-commit view for a
            // stable symbol pass `symbol_name=` explicitly, and the
            // table is rendered as usual.
            let summarise_stable = symbol_filter.is_none();
            let mut stable_summarised = 0usize;

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

                // `(commits=N)` replaces the bare `(N)` suffix so the
                // number is self-describing - previously the count
                // visually collided with CC values on either side.
                out.push_str(&format!(
                    "## {} (commits={}) CC {} -> {} ({:+.0}% {})\n\n",
                    t.symbol_name,
                    t.points.len(),
                    first_cc,
                    last_cc,
                    delta,
                    direction,
                ));

                if summarise_stable && direction == "STABLE" {
                    stable_summarised += 1;
                    continue;
                }

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

            if stable_summarised > 0 {
                out.push_str(&format!(
                    "// {stable_summarised} STABLE symbol(s) shown as header only (no CC change). Pass `symbol_name=` to see the per-commit table.\n",
                ));
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

        if limit_was_clamped {
            out.push_str(&format!(
                "\nnote: limit={requested_limit} was clamped to {MAX_COMMIT_LIMIT} (server-side commit cap). Per-symbol series may still be shorter when the underlying file has fewer indexed commits.\n",
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
