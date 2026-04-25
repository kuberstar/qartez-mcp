// Rust guideline compliant 2026-04-25

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

/// Sections an `understand` call can request. Mirrors the `sections`
/// parameter on `SoulUnderstandParams`. Kept private to this module
/// because it is the only validator and consumer.
const VALID_SECTIONS: &[&str] = &["definition", "calls", "refs", "cochange"];

/// Default token budget for the compound view. Higher than the 4 000
/// default used by atomic query tools because the body shows multiple
/// sections; each one gets an equal slice once the header is rendered.
const DEFAULT_UNDERSTAND_BUDGET: u32 = 6_000;

/// Default per-section row cap forwarded to embedded `qartez_calls`
/// and `qartez_refs` invocations. Hub symbols can have hundreds of
/// importers - 10 keeps the compound output readable.
const DEFAULT_UNDERSTAND_LIMIT: u32 = 10;

#[tool_router(router = qartez_understand_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_understand",
        description = "Compound investigation of one symbol: resolves the definition (with kind/file_path disambiguation), then bundles signature + body + direct callers/callees + top references + co-change partners of the defining file in a single response. Replaces the qartez_find -> qartez_read -> qartez_calls -> qartez_refs -> qartez_cochange round-trip chain when you just want to know what a symbol is and who touches it. Pass `sections=['definition','calls']` to skip expensive sections; the per-section token budget is split equally across whatever sections remain.",
        annotations(
            title = "Understand Symbol",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_understand(
        &self,
        Parameters(params): Parameters<SoulUnderstandParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_understand")?;
        let name_trimmed = params.name.trim();
        if name_trimmed.is_empty() {
            return Err("`name` must be non-empty".into());
        }
        let name = name_trimmed.to_string();
        let concise = is_concise(&params.format);
        let total_budget = params.token_budget.unwrap_or(DEFAULT_UNDERSTAND_BUDGET) as usize;
        let per_section_limit = params.limit.unwrap_or(DEFAULT_UNDERSTAND_LIMIT);
        let include_tests = params.include_tests.unwrap_or(false);

        let kind_filter = params
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let file_filter = params
            .file_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        // Validate `sections` up front. An unknown name (typo like
        // 'definitions' plural) used to silently disappear into the
        // contains() check and produce a header-only response with no
        // body - the caller never learned why their section was empty.
        let sections: Vec<String> = match params.sections.as_ref() {
            Some(v) if !v.is_empty() => {
                let mut bad: Vec<String> = Vec::new();
                for s in v {
                    let t = s.trim().to_lowercase();
                    if !VALID_SECTIONS.contains(&t.as_str()) {
                        bad.push(s.clone());
                    }
                }
                if !bad.is_empty() {
                    return Err(format!(
                        "Unknown section(s): {}. Valid: {}.",
                        bad.join(", "),
                        VALID_SECTIONS.join(", "),
                    ));
                }
                v.iter().map(|s| s.trim().to_lowercase()).collect()
            }
            _ => VALID_SECTIONS.iter().map(|s| (*s).to_string()).collect(),
        };

        // Resolve the symbol. Reuse the same conn for kind/file_path
        // filtering so the disambiguation guard fires before any
        // section delegation work happens.
        let candidates = {
            let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
            read::find_symbol_by_name(&conn, &name).map_err(|e| format!("DB error: {e}"))?
        };

        if candidates.is_empty() {
            return Err(format!("No symbol found with name '{name}'"));
        }

        let filtered: Vec<_> = candidates
            .into_iter()
            .filter(|(s, _)| match kind_filter {
                Some(k) => s.kind.eq_ignore_ascii_case(k),
                None => true,
            })
            .filter(|(_, f)| match file_filter {
                Some(p) => f.path == p,
                None => true,
            })
            .filter(|(_, f)| include_tests || !helpers::is_test_path(&f.path))
            .collect();

        if filtered.is_empty() {
            return Err(format!(
                "'{name}' has no candidate matching kind={kind_filter:?} file_path={file_filter:?} include_tests={include_tests}. Drop a filter or set include_tests=true to widen the search.",
            ));
        }

        if filtered.len() > 1 {
            // Same disambiguation contract qartez_calls uses: refuse
            // up front when multi-match could cross-attribute results
            // across overloads. Print every survivor so the caller
            // sees which kind/file_path to pass.
            let mut banner = format!(
                "'{name}' resolves to {} candidate(s). Pass `file_path` or `kind` to pick one - per-candidate body/calls/refs are not attributable without disambiguation.\n\ncandidates:\n",
                filtered.len(),
            );
            for (sym, def_file) in &filtered {
                let owner = sym
                    .owner_type
                    .as_deref()
                    .map(|t| format!("{t}::"))
                    .unwrap_or_default();
                banner.push_str(&format!(
                    "  - {owner}{} ({}) @ {}:L{}-{}\n",
                    sym.name, sym.kind, def_file.path, sym.line_start, sym.line_end,
                ));
            }
            return Ok(banner);
        }

        let (sym, def_file) = filtered.into_iter().next().expect("len() == 1 above");

        // Header. Always rendered so even a one-section call produces a
        // useful response.
        let owner_prefix = sym
            .owner_type
            .as_deref()
            .map(|t| format!("{t}::"))
            .unwrap_or_default();
        let signature = sym.signature.clone().unwrap_or_else(|| "-".to_string());
        let exported = if sym.is_exported {
            "exported"
        } else {
            "private"
        };
        let mut out = if concise {
            format!(
                "{owner_prefix}{} ({}) @ {}:L{}-{} [{}]\n",
                sym.name, sym.kind, def_file.path, sym.line_start, sym.line_end, exported,
            )
        } else {
            format!(
                "# Symbol: {owner_prefix}{} ({})\nDefined in: {} [L{}-L{}] (visibility: {})\nSignature: {}\n",
                sym.name,
                sym.kind,
                def_file.path,
                sym.line_start,
                sym.line_end,
                exported,
                signature,
            )
        };

        let header_tokens = estimate_tokens(&out);
        let remaining = total_budget.saturating_sub(header_tokens);
        // Equal slice per active section. `max(1)` guards against the
        // edge case where the header alone consumed the budget; without
        // it `per_section` would be 0 and every embedded call would
        // hit the MIN_TOKEN_BUDGET clamp inside qartez_map / qartez_calls.
        let per_section = (remaining / sections.len().max(1)).max(256);

        for section in &sections {
            match section.as_str() {
                "definition" => {
                    out.push_str("\n## Definition\n");
                    let read_params = SoulReadParams {
                        symbol_name: Some(sym.name.clone()),
                        file_path: Some(def_file.path.clone()),
                        // Each token is roughly 4 bytes; leave headroom
                        // for the wrapper text by halving the per-section
                        // budget into bytes.
                        max_bytes: Some((per_section as u32).saturating_mul(2)),
                        ..Default::default()
                    };
                    match self.qartez_read(Parameters(read_params)) {
                        Ok(body) => append_capped(&mut out, &body, total_budget),
                        Err(e) => {
                            out.push_str(&format!("(definition unavailable: {e})\n"));
                        }
                    }
                }
                "calls" => {
                    out.push_str("\n## Calls (depth=1)\n");
                    let calls_params = SoulCallsParams {
                        name: sym.name.clone(),
                        depth: Some(1),
                        limit: Some(per_section_limit),
                        kind: Some(sym.kind.clone()),
                        file_path: Some(def_file.path.clone()),
                        token_budget: Some(per_section as u32),
                        include_tests: Some(include_tests),
                        // Forward the concise/detailed selector so
                        // sub-tool output matches the compound view's
                        // verbosity. Without this, a concise outer
                        // call still emitted detailed call/ref blocks
                        // that contradicted the compact header.
                        format: params.format,
                        ..Default::default()
                    };
                    match self.qartez_calls(Parameters(calls_params)) {
                        Ok(body) => append_capped(&mut out, &body, total_budget),
                        Err(e) => {
                            out.push_str(&format!("(calls unavailable: {e})\n"));
                        }
                    }
                }
                "refs" => {
                    out.push_str(&format!("\n## References (top {per_section_limit})\n"));
                    let refs_params = SoulRefsParams {
                        symbol: sym.name.clone(),
                        token_budget: Some(per_section as u32),
                        include_tests: Some(include_tests),
                        format: params.format,
                        ..Default::default()
                    };
                    match self.qartez_refs(Parameters(refs_params)) {
                        Ok(body) => {
                            // qartez_refs walks every same-named candidate
                            // and prints them in sequence. The compound
                            // tool already disambiguated to one symbol,
                            // so trim the `# N matches name 'X'` banner
                            // line that would otherwise contradict the
                            // single-symbol header above.
                            let trimmed = body
                                .lines()
                                .filter(|l| !l.starts_with("# ") || !l.contains("matches name"))
                                .collect::<Vec<_>>()
                                .join("\n");
                            append_capped(&mut out, &trimmed, total_budget);
                        }
                        Err(e) => {
                            out.push_str(&format!("(refs unavailable: {e})\n"));
                        }
                    }
                }
                "cochange" => {
                    out.push_str("\n## Co-change partners (defining file)\n");
                    let cochange_text = {
                        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
                        let pairs = read::get_cochanges(&conn, def_file.id, 5).unwrap_or_default();
                        if pairs.is_empty() {
                            "  (no indexed co-change partners)\n".to_string()
                        } else {
                            let mut s = String::new();
                            for (cc, partner) in &pairs {
                                s.push_str(&format!(
                                    "  {} (changed together {} times)\n",
                                    partner.path, cc.count,
                                ));
                            }
                            s
                        }
                    };
                    append_capped(&mut out, &cochange_text, total_budget);
                }
                _ => unreachable!("section list validated above"),
            }
        }

        Ok(out)
    }
}

/// Append `body` to `out` if the combined estimated tokens stay under
/// `budget`. When the body would overflow, emit the truncation marker
/// other compound surfaces use (`qartez_map`, `qartez_context`) so the
/// caller can recognise the same shape across tools.
fn append_capped(out: &mut String, body: &str, budget: usize) {
    if estimate_tokens(out) + estimate_tokens(body) > budget {
        out.push_str("(section truncated by token_budget)\n");
        return;
    }
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
}
