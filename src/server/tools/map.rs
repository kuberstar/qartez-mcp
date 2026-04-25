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

#[tool_router(router = qartez_map_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_map",
        description = "Start here. Returns the codebase skeleton: files ranked by importance (PageRank), their exports, and blast radii. Use boost_files/boost_terms to focus on areas relevant to your current task.",
        annotations(
            title = "Project Map",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_map(
        &self,
        Parameters(params): Parameters<QartezParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_map")?;
        let requested_top = params.top_n.unwrap_or(20);
        let all_files = params.all_files.unwrap_or(false) || requested_top == 0;
        let top_n = if all_files {
            i64::MAX
        } else {
            requested_top as i64
        };
        // `token_budget=0` would disable every output line because
        // `estimate_tokens(&line) > 0` is true for any non-empty row,
        // leaving callers with a header-only skeleton that read like
        // an empty-index response. Reject the zero value explicitly
        // and clamp small values into a usable floor so the response
        // still carries enough rows to be meaningful while preserving
        // the caller's "tight budget" intent.
        const MIN_TOKEN_BUDGET: u32 = 256;
        if let Some(0) = params.token_budget {
            return Err(
                "token_budget=0 is invalid (no output is possible); pass a positive value of at least 256 or omit to accept the 4000-token default.".into(),
            );
        }
        let requested_budget = params.token_budget.unwrap_or(4000);
        let mut token_budget_warning = String::new();
        let token_budget = if requested_budget < MIN_TOKEN_BUDGET {
            token_budget_warning = format!(
                "// warning: token_budget={requested_budget} clamped to {MIN_TOKEN_BUDGET} (minimum for a meaningful response)\n",
            );
            MIN_TOKEN_BUDGET as usize
        } else {
            requested_budget as usize
        };
        let concise = is_concise(&params.format);

        // Validate the `by` axis up front. The only accepted values are
        // `files` (default) and `symbols`; anything else used to be
        // silently coerced into the file-ranked path, masking typos
        // like `by=symbol` (singular) behind an unexpected output
        // shape. Warn rather than hard-error so legacy callers that
        // leaned on the silent-coerce behaviour still get their
        // file-ranked view, just with the typo surfaced.
        let mut warnings: Vec<String> = Vec::new();
        if !token_budget_warning.is_empty() {
            let trimmed = token_budget_warning.trim_end().to_string();
            if !trimmed.is_empty() {
                warnings.push(trimmed);
            }
        }
        let by_raw = params
            .by
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        // Hard-reject unknown `by` values. A typo like `by=symbol`
        // (singular) used to silently coerce to the file-ranked path
        // and return an unexpected shape; callers then chased a
        // non-bug. Only `files` (default) and `symbols` are valid.
        let by_symbols = match by_raw {
            None => false,
            Some(v) if v.eq_ignore_ascii_case("files") => false,
            Some(v) if v.eq_ignore_ascii_case("symbols") => true,
            Some(other) => {
                return Err(format!(
                    "Unknown `by` value '{other}'. Valid: 'files' (default) or 'symbols'.",
                ));
            }
        };

        // Validate boost_terms: a term that matches zero indexed
        // symbols is almost always a typo and produced no observable
        // effect on the ranking. Surface the miss the same way we
        // already surface bad boost_files entries so the caller can
        // fix their prompt.
        if let Some(terms) = params.boost_terms.as_deref()
            && !terms.is_empty()
        {
            let unmatched: Vec<String> = {
                let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
                let mut missed: Vec<String> = Vec::new();
                for term in terms {
                    let trimmed = term.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let fts_query = if trimmed.contains('*') {
                        trimmed.to_string()
                    } else {
                        format!("{trimmed}*")
                    };
                    let hits = read::search_file_ids_by_fts(&conn, &fts_query).unwrap_or_default();
                    if hits.is_empty() {
                        missed.push(term.clone());
                    }
                }
                missed
            };
            if !unmatched.is_empty() {
                warnings.push(format!(
                    "// warning: {} boost_terms entry(ies) matched no indexed symbol: {}",
                    unmatched.len(),
                    unmatched.join(", "),
                ));
            }
        }

        let combined_warning = if warnings.is_empty() {
            String::new()
        } else {
            format!("{}\n", warnings.join("\n"))
        };

        if by_symbols {
            let body = self.build_symbol_overview(top_n, token_budget, concise);
            return Ok(prepend_warning(&combined_warning, body));
        }
        let with_health = params.with_health.unwrap_or(false);
        let body = self.build_overview(
            top_n,
            token_budget,
            params.boost_files.as_deref(),
            params.boost_terms.as_deref(),
            concise,
            all_files,
            with_health,
        );
        // Coercion annotation: `top_n=0` is the documented no-cap
        // path (treated as `all_files=true`). Surface the coercion so
        // the caller sees that their "give me everything" intent was
        // honoured and not silently falling into a default page.
        let top_n_zero_note = if requested_top == 0 && params.all_files != Some(true) {
            "// top_n=0 treated as all_files=true per the no-cap convention\n"
        } else {
            ""
        };
        // Rewrite `build_overview`'s boost_files warning from a
        // buried `// warning:` line (which read as tool output
        // metadata) into a markdown blockquote hoisted above the
        // body. Callers scanning the response land on the NOTE
        // immediately instead of missing it between the stats
        // table and the exports section.
        let (body_hoisted, boost_note) = hoist_boost_files_warning(body);

        let mut out = prepend_warning(&combined_warning, body_hoisted);
        if !boost_note.is_empty() {
            out.insert_str(0, &boost_note);
        }
        if !top_n_zero_note.is_empty() {
            out.insert_str(0, top_n_zero_note);
        }
        // Surface a clear hint when `top_n=0` asked for the full list
        // but `token_budget` will truncate the response. The body text
        // already says "truncated: X/Y files shown"; append a concrete
        // token_budget suggestion (2x current) so the caller has a
        // ready-to-paste knob instead of guessing.
        if requested_top == 0 && out.contains("// truncated") {
            let suggested = token_budget
                .saturating_mul(2)
                .max(token_budget.saturating_add(2_000));
            out.push_str(&format!(
                "// hint: top_n=0 requested the full list but token_budget={token_budget} truncated it. Raise `token_budget={suggested}` (or higher) or narrow with `boost_files=`/`boost_terms=` to see every file.\n",
            ));
        }
        Ok(out)
    }
}

/// Pull the `// warning: N boost_files entry(ies) matched no indexed
/// file: ...` line emitted by `build_overview` and convert it into a
/// markdown blockquote. Keeps the body text free of a buried warning
/// that otherwise rendered like tool metadata mid-response. Returns
/// `(body_without_warning, hoisted_note)` so the caller can prepend
/// the note wherever it wants.
fn hoist_boost_files_warning(body: String) -> (String, String) {
    const PREFIX: &str = "// warning: ";
    const NEEDLE: &str = "boost_files entry(ies) matched no indexed file";
    let mut kept = String::with_capacity(body.len());
    let mut note = String::new();
    for line in body.split_inclusive('\n') {
        if note.is_empty() && line.starts_with(PREFIX) && line.contains(NEEDLE) {
            let stripped = line.trim_start_matches(PREFIX).trim_end();
            note = format!("> NOTE: {stripped}\n");
            continue;
        }
        kept.push_str(line);
    }
    (kept, note)
}

/// Prepend a warning banner (produced by the `token_budget` clamp
/// path) to a built overview body. Kept as a free function so the
/// two overview variants above share the same prefixing logic
/// without duplicating the empty-warning fast path.
fn prepend_warning(warning: &str, body: String) -> String {
    if warning.is_empty() {
        body
    } else {
        format!("{warning}{body}")
    }
}
