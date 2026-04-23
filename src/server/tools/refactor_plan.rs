#![allow(unused_imports)]

use std::collections::{HashMap, HashSet};

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

#[tool_router(router = qartez_refactor_plan_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_refactor_plan",
        description = "Ordered, safety-annotated refactor plan for a single file. Pulls the file's code smells (god functions, long param lists) and orders them by estimated cyclomatic-complexity reduction and safety (does the file have tests? is the symbol exported? how many callers?). Each step names a concrete refactor technique (Extract Method, Introduce Parameter Object, Replace Conditional) and categorizes the expected CC impact (High/Medium/Low). The tool is intentionally conservative about exact CC prediction: it emits ranges, not single numbers.",
        annotations(
            title = "Refactor Plan",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_refactor_plan(
        &self,
        Parameters(params): Parameters<SoulRefactorPlanParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_refactor_plan")?;
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let concise = is_concise(&params.format);
        // Default 8 matches the tool description; an upper cap of 50 keeps
        // the rendered plan inside the MCP transport budget even when a
        // caller passes a very large `limit`. The floor of 1 guarantees
        // we still show at least the highest-impact step.
        const MAX_REFACTOR_STEPS: u32 = 50;
        let limit = params.limit.unwrap_or(8).clamp(1, MAX_REFACTOR_STEPS) as usize;
        let min_cc = params.min_complexity.unwrap_or(15);
        let min_lines = params.min_lines.unwrap_or(50);
        let min_params = params.min_params.unwrap_or(5) as usize;

        let resolved = self
            .safe_resolve(&params.file_path)
            .map_err(|e| e.to_string())?;
        let rel = crate::index::to_forward_slash(
            resolved
                .strip_prefix(&self.project_root)
                .unwrap_or(&resolved)
                .to_string_lossy()
                .into_owned(),
        );
        let file = read::get_file_by_path(&conn, &rel)
            .map_err(|e| format!("DB error: {e}"))?
            .ok_or_else(|| format!("File not found: {}", params.file_path))?;
        let symbols =
            read::get_symbols_for_file(&conn, file.id).map_err(|e| format!("DB error: {e}"))?;

        let mut steps: Vec<Step> = Vec::new();
        for sym in &symbols {
            if !matches!(sym.kind.as_str(), "function" | "method") {
                continue;
            }
            if let Some(cc) = sym.complexity {
                let body = sym.line_end.saturating_sub(sym.line_start) + 1;
                if cc >= min_cc && body >= min_lines {
                    steps.push(Step::god(sym, cc, body));
                }
            }
            if let Some(ref sig) = sym.signature {
                let count = super::smells::count_signature_params(sig);
                if count >= min_params {
                    steps.push(Step::long_params(sym, count));
                }
            }
        }

        if steps.is_empty() {
            return Ok(format!(
                "No smells detected in `{rel}` at min_complexity={min_cc}, min_lines={min_lines}, min_params={min_params}. Lower thresholds to widen, or inspect the file with `qartez_outline file_path={rel}`."
            ));
        }

        // Two coverage signals: cross-file (`tests/foo.rs -> src/foo.rs`
        // import edge) and inline (`#[cfg(test)] fn test_*` colocated
        // with production). The old report used only the import-edge
        // signal, so files like `src/index/mod.rs` with ~80 inline
        // `test_*` functions AND an external `tests/business_logic.rs`
        // importer were flagged "Tests covering file: none detected".
        // Report both to give callers an accurate safety picture.
        let (cross_file_count, has_cross_file_tests) =
            cross_file_test_stats(&conn, &rel).unwrap_or((0, false));
        let has_inline = helpers::has_inline_rust_tests(&self.project_root, &rel);
        let inline_test_count = if has_inline {
            symbols
                .iter()
                .filter(|s| {
                    matches!(s.kind.as_str(), "function" | "method") && s.name.starts_with("test_")
                })
                .count()
        } else {
            0
        };
        let has_tests = has_cross_file_tests || has_inline;

        let symbol_caller_counts =
            caller_counts(&conn, symbols.iter().map(|s| s.id)).unwrap_or_default();

        for step in &mut steps {
            step.callers = symbol_caller_counts
                .get(&step.symbol_id)
                .copied()
                .unwrap_or(0);
            step.has_tests = has_tests;
        }

        steps.sort_by(|a, b| {
            b.impact_rank()
                .cmp(&a.impact_rank())
                .then_with(|| b.safety_rank().cmp(&a.safety_rank()))
        });
        let total = steps.len();
        steps.truncate(limit);

        let max_cc = symbols
            .iter()
            .filter_map(|s| s.complexity)
            .max()
            .unwrap_or(0);
        let current_health = health_score(max_cc as f64, file.pagerank, file.change_count);
        let health_tag = if current_health < 3.0 {
            "Critical"
        } else if current_health < 6.0 {
            "Unhealthy"
        } else {
            "Moderate"
        };

        let tests_summary = match (inline_test_count, cross_file_count, has_tests) {
            (0, 0, _) => "none detected".to_string(),
            (n, 0, _) if n > 0 => format!("{n} inline"),
            (0, m, _) if m > 0 => format!("{m} cross-file file(s)"),
            (n, m, _) => format!("{n} inline + {m} cross-file file(s)"),
        };

        let mut out = String::new();
        out.push_str(&format!("# Refactor Plan: `{rel}`\n\n"));
        out.push_str(&format!(
            "- Current health: **{current_health:.1}/10 ({health_tag})**\n- MaxCC={max_cc}  PageRank={:.4}  Churn={}\n- Tests covering file: {tests_summary}\n- Steps surfaced: {}/{}\n\n",
            file.pagerank,
            file.change_count,
            steps.len(),
            total,
        ));

        if concise {
            out.push_str("# n impact technique symbol line cc_delta safety\n");
            for (i, s) in steps.iter().enumerate() {
                out.push_str(&format!(
                    "{} {} {} {} L{} {} {}\n",
                    i + 1,
                    s.impact_label(),
                    s.technique(),
                    s.name,
                    s.line_start,
                    s.cc_delta_range(),
                    s.safety_label(),
                ));
            }
            return Ok(out);
        }

        for (i, s) in steps.iter().enumerate() {
            out.push_str(&format!(
                "## Step {}: {} on `{}` (L{})\n",
                i + 1,
                s.technique(),
                s.name,
                s.line_start,
            ));
            match s.kind {
                StepKind::God { cc, lines } => {
                    out.push_str(&format!("- Smell: god_function (CC={cc}, lines={lines})\n",));
                }
                StepKind::LongParams { count } => {
                    out.push_str(&format!("- Smell: long_params ({count} params)\n"));
                }
            }
            out.push_str(&format!(
                "- Estimated CC impact: **{}** ({} to parent fn)\n",
                s.impact_label(),
                s.cc_delta_range(),
            ));
            out.push_str(&format!(
                "- Safety: {} (tests: {}, callers: {}, exported: {})\n",
                s.safety_label(),
                if s.has_tests { "yes" } else { "missing" },
                s.callers,
                if s.is_exported { "yes" } else { "no" },
            ));
            out.push_str(&format!("- Rationale: {}\n", s.rationale()));
            out.push('\n');
        }

        out.push_str("## Ordering & safety notes\n");
        out.push_str("- Steps ordered by estimated CC impact desc, then safety desc.\n");
        out.push_str("- CC estimates are heuristic ranges, not measurements. Re-index after each step to see the real delta.\n");
        out.push_str(&format!(
            "- Before applying any step: `qartez_impact file_path={rel}` to review blast radius.\n",
        ));
        if !has_tests {
            out.push_str(
                "- WARNING: no tests mapped to this file. Add characterization tests before refactoring.\n",
            );
        }
        Ok(out)
    }
}

enum StepKind {
    God { cc: u32, lines: u32 },
    LongParams { count: usize },
}

struct Step {
    kind: StepKind,
    name: String,
    symbol_id: i64,
    line_start: u32,
    is_exported: bool,
    callers: usize,
    has_tests: bool,
}

impl Step {
    fn god(sym: &crate::storage::models::SymbolRow, cc: u32, body_lines: u32) -> Self {
        Self {
            kind: StepKind::God {
                cc,
                lines: body_lines,
            },
            name: sym.name.clone(),
            symbol_id: sym.id,
            line_start: sym.line_start,
            is_exported: sym.is_exported,
            callers: 0,
            has_tests: false,
        }
    }
    fn long_params(sym: &crate::storage::models::SymbolRow, count: usize) -> Self {
        Self {
            kind: StepKind::LongParams { count },
            name: sym.name.clone(),
            symbol_id: sym.id,
            line_start: sym.line_start,
            is_exported: sym.is_exported,
            callers: 0,
            has_tests: false,
        }
    }

    fn technique(&self) -> &'static str {
        match self.kind {
            StepKind::God { .. } => "Extract Method",
            StepKind::LongParams { .. } => "Introduce Parameter Object",
        }
    }

    fn impact_label(&self) -> &'static str {
        match self.kind {
            StepKind::God { cc, .. } if cc >= 30 => "HIGH",
            StepKind::God { cc, .. } if cc >= 20 => "MEDIUM",
            StepKind::God { .. } => "LOW",
            StepKind::LongParams { .. } => "LOW",
        }
    }

    fn impact_rank(&self) -> u8 {
        match self.impact_label() {
            "HIGH" => 2,
            "MEDIUM" => 1,
            _ => 0,
        }
    }

    /// Heuristic ranges, NOT measurements. Extract Method for a god function
    /// removes roughly one decision point per extracted branch; the parent
    /// typically drops by 20-50% of its CC when 3-5 independent branches move
    /// out. Introduce Parameter Object does not change control flow.
    fn cc_delta_range(&self) -> String {
        match self.kind {
            StepKind::God { cc, .. } => {
                let low = (cc as f32 * 0.2).round() as i32;
                let high = (cc as f32 * 0.5).round() as i32;
                format!("-{low} to -{high} CC")
            }
            StepKind::LongParams { .. } => "0 CC (cognitive-load only)".to_string(),
        }
    }

    fn safety_rank(&self) -> u8 {
        let mut score = 0u8;
        if self.has_tests {
            score += 2;
        }
        if !self.is_exported {
            score += 1;
        }
        if self.callers <= 3 {
            score += 1;
        }
        score
    }

    fn safety_label(&self) -> &'static str {
        match self.safety_rank() {
            0..=1 => "RISKY",
            2..=3 => "OK",
            _ => "SAFE",
        }
    }

    fn rationale(&self) -> String {
        match self.kind {
            StepKind::God { cc, lines } => format!(
                "{cc} decision points in a {lines}-line body is past the usual review/test budget. Split the largest independent branches into helpers first; keep the public signature stable so callers stay untouched."
            ),
            StepKind::LongParams { count } => format!(
                "{count} positional args is past what call sites can read at a glance. Group correlated params into a small struct; update all call sites atomically in one commit."
            ),
        }
    }
}

/// Copy of the hotspot health formula. See `tools/hotspots.rs`.
fn health_score(max_cc: f64, coupling: f64, churn: i64) -> f64 {
    let cc_h = 10.0 / (1.0 + max_cc / 10.0);
    let coupling_h = 10.0 / (1.0 + coupling * 50.0);
    let churn_h = 10.0 / (1.0 + churn as f64 / 8.0);
    (cc_h + coupling_h + churn_h) / 3.0
}

/// True when any indexed file importing the target looks like a test file.
/// Count test files that import `rel` and whether at least one exists.
/// The boolean is a slim compatibility shim for callers that only care
/// about yes/no; the count drives the refactor-plan report so the user
/// sees "M cross-file test file(s)" instead of a bare "yes".
fn cross_file_test_stats(
    conn: &rusqlite::Connection,
    rel: &str,
) -> rusqlite::Result<(usize, bool)> {
    let mut stmt = conn.prepare_cached(
        "SELECT f.path FROM edges e
         JOIN files f ON f.id = e.from_file
         JOIN files t ON t.id = e.to_file
         WHERE t.path = ?1",
    )?;
    let rows = stmt.query_map([rel], |row| row.get::<_, String>(0))?;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for path in rows.flatten() {
        if helpers::is_test_path(&path) {
            seen.insert(path);
        }
    }
    let has = !seen.is_empty();
    Ok((seen.len(), has))
}

/// For each of the given symbol ids, how many other symbols reference them.
fn caller_counts<I: IntoIterator<Item = i64>>(
    conn: &rusqlite::Connection,
    ids: I,
) -> rusqlite::Result<HashMap<i64, usize>> {
    let mut stmt =
        conn.prepare_cached("SELECT COUNT(*) FROM symbol_refs WHERE to_symbol_id = ?1")?;
    let mut out = HashMap::new();
    for id in ids {
        let n: i64 = stmt.query_row([id], |row| row.get(0))?;
        out.insert(id, n as usize);
    }
    Ok(out)
}
