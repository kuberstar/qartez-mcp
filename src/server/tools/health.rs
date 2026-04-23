#![allow(unused_imports)]

use std::collections::{BTreeSet, HashMap, HashSet};

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

#[tool_router(router = qartez_health_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_health",
        description = "Prioritized, actionable health report that cross-references qartez_hotspots (complexity x coupling x churn) with qartez_smells (god functions, long parameter lists, feature envy). Files that score badly in both signals are surfaced first as 'critical' with a concrete suggested refactor technique. Use `qartez_refactor_plan file_path=<X>` to expand a recommendation into a step-by-step plan.",
        annotations(
            title = "Codebase Health Report",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_health(
        &self,
        Parameters(params): Parameters<SoulHealthParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_health")?;
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let concise = is_concise(&params.format);
        // `limit=0` means "no cap" project-wide convention; `None` keeps the
        // historical default of 15.
        let limit = match params.limit {
            None => 15,
            Some(0) => usize::MAX,
            Some(n) => n as usize,
        };
        // Negative max_health is rejected outright: silently clamping a
        // negative to 0 produced the "Avg health 10.0/10" stub on empty
        // results, which read like success. Values above 10 clamp to 10
        // for deterministic output (a 0-to-10 health scale cannot exceed
        // 10, and callers using "999" as an "unbounded ceiling" idiom
        // expect the same report as max_health=10).
        if let Some(m) = params.max_health
            && m < 0.0
        {
            return Err(format!(
                "max_health must be >= 0.0, got {m}. Use a value in [0.0, 10.0]."
            ));
        }
        let max_health = params.max_health.unwrap_or(5.0).min(10.0);
        let min_cc_explicit = params.min_complexity;
        let min_cc = params.min_complexity.unwrap_or(15);
        let min_lines = params.min_lines.unwrap_or(50);
        let min_params = params.min_params.unwrap_or(5) as usize;

        let all_files = read::get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;
        let all_symbols =
            read::get_all_symbols_with_path(&conn).map_err(|e| format!("DB error: {e}"))?;

        // When the caller EXPLICITLY raised min_complexity above every
        // indexed function's CC, no file can ever qualify. Surface that
        // as a clear signal rather than falling through to the generic
        // "Review with qartez_outline" stub. We restrict this to
        // explicit caller intent so a small clean repo using the default
        // threshold still hits the standard "no unhealthy files" path.
        let max_cc_seen = all_symbols
            .iter()
            .filter_map(|(s, _)| s.complexity)
            .max()
            .unwrap_or(0);
        if let Some(explicit) = min_cc_explicit
            && explicit > max_cc_seen
        {
            return Ok(format!(
                "No files with min_complexity >= {explicit} found (max observed CC = {max_cc_seen}). Lower min_complexity to widen the search.",
            ));
        }

        let mut smells_by_file: HashMap<String, Vec<SmellEntry>> = HashMap::new();
        for (sym, path) in &all_symbols {
            if !matches!(sym.kind.as_str(), "function" | "method") {
                continue;
            }
            if let Some(cc) = sym.complexity {
                let body = sym.line_end.saturating_sub(sym.line_start) + 1;
                if cc >= min_cc && body >= min_lines {
                    smells_by_file
                        .entry(path.clone())
                        .or_default()
                        .push(SmellEntry::God {
                            name: sym.name.clone(),
                            cc,
                            lines: body,
                            line_start: sym.line_start,
                        });
                }
            }
            if let Some(ref sig) = sym.signature {
                let count = super::smells::count_signature_params(sig);
                if count >= min_params {
                    smells_by_file
                        .entry(path.clone())
                        .or_default()
                        .push(SmellEntry::LongParams {
                            name: sym.name.clone(),
                            count,
                            line_start: sym.line_start,
                        });
                }
            }
        }

        let mut rows: Vec<FileHealthRow> = Vec::new();
        for file in &all_files {
            if helpers::is_test_path(&file.path) {
                continue;
            }
            let symbols = read::get_symbols_for_file(&conn, file.id).unwrap_or_default();
            let complexities: Vec<u32> = symbols.iter().filter_map(|s| s.complexity).collect();
            let max_cc = complexities.iter().copied().max().unwrap_or(0);
            let smell_entries = smells_by_file.remove(&file.path).unwrap_or_default();
            if max_cc == 0 && smell_entries.is_empty() {
                continue;
            }
            let health = health_score(max_cc as f64, file.pagerank, file.change_count);
            if health > max_health {
                continue;
            }
            let is_hotspot =
                max_cc as f64 >= min_cc as f64 && file.pagerank > 0.0 && file.change_count > 0;
            let severity = classify(is_hotspot, !smell_entries.is_empty(), health);
            rows.push(FileHealthRow {
                path: file.path.clone(),
                health,
                max_cc,
                pagerank: file.pagerank,
                churn: file.change_count,
                smells: smell_entries,
                severity,
            });
        }

        if rows.is_empty() {
            return Ok(format!(
                "No unhealthy files found at max_health={max_health:.1}, min_cc={min_cc}, min_lines={min_lines}, min_params={min_params}. Raise thresholds or lower max_health to widen the search."
            ));
        }

        rows.sort_by(|a, b| {
            a.severity.rank().cmp(&b.severity.rank()).then(
                a.health
                    .partial_cmp(&b.health)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        let total = rows.len();
        if limit != usize::MAX {
            rows.truncate(limit);
        }

        let avg_health = if rows.is_empty() {
            10.0
        } else {
            rows.iter().map(|r| r.health).sum::<f64>() / rows.len() as f64
        };

        let mut out = String::new();
        out.push_str(&format!(
            "# Codebase Health Report\n\nShowing {}/{} unhealthy files. Avg health of surfaced files: {:.1}/10 (lower = worse).\nHealth = mean(complexity, coupling, churn) on a 0-10 scale.\n\n",
            rows.len(),
            total,
            avg_health,
        ));

        if concise {
            out.push_str("# severity health file max_cc churn pagerank smells\n");
            for r in &rows {
                out.push_str(&format!(
                    "{} {:.1} {} {} {} {:.4} {}\n",
                    r.severity.tag(),
                    r.health,
                    r.path,
                    r.max_cc,
                    r.churn,
                    r.pagerank,
                    r.smells.len(),
                ));
            }
            return Ok(out);
        }

        for sev in [Severity::Critical, Severity::High, Severity::Medium] {
            let bucket: Vec<&FileHealthRow> = rows.iter().filter(|r| r.severity == sev).collect();
            if bucket.is_empty() {
                continue;
            }
            out.push_str(&format!("## {} ({})\n\n", sev.header(), bucket.len()));
            out.push_str(sev.blurb());
            out.push('\n');
            for r in bucket {
                format_file_block(&mut out, r);
            }
        }

        out.push_str("## Next steps\n");
        out.push_str(
            "- `qartez_refactor_plan file_path=<file>` for an ordered, safety-annotated plan on a single file.\n",
        );
        out.push_str("- `qartez_impact file_path=<file>` before editing any load-bearing file.\n");
        out.push_str(
            "- Tune `min_complexity`, `min_lines`, `min_params`, or `max_health` to widen or narrow the report.\n",
        );

        Ok(out)
    }
}

#[derive(Clone)]
enum SmellEntry {
    God {
        name: String,
        cc: u32,
        lines: u32,
        line_start: u32,
    },
    LongParams {
        name: String,
        count: usize,
        line_start: u32,
    },
}

struct FileHealthRow {
    path: String,
    health: f64,
    max_cc: u32,
    pagerank: f64,
    churn: i64,
    smells: Vec<SmellEntry>,
    severity: Severity,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Severity {
    Critical,
    High,
    Medium,
}

impl Severity {
    fn rank(self) -> u8 {
        match self {
            Severity::Critical => 0,
            Severity::High => 1,
            Severity::Medium => 2,
        }
    }
    fn tag(self) -> &'static str {
        match self {
            Severity::Critical => "C",
            Severity::High => "H",
            Severity::Medium => "M",
        }
    }
    fn header(self) -> &'static str {
        match self {
            Severity::Critical => "Critical",
            Severity::High => "High",
            Severity::Medium => "Medium",
        }
    }
    fn blurb(self) -> &'static str {
        match self {
            Severity::Critical => {
                "Hotspot AND at least one smell. Fix these first - a change here breaks callers and regresses quickly.\n\n"
            }
            Severity::High => {
                "Hotspot without a named smell. High complexity, coupling, and churn - a magnet for future smells.\n\n"
            }
            Severity::Medium => {
                "Smells without hotspot pressure. Safe to defer, but easy wins when you touch the file.\n\n"
            }
        }
    }
}

fn classify(is_hotspot: bool, has_smell: bool, _health: f64) -> Severity {
    match (is_hotspot, has_smell) {
        (true, true) => Severity::Critical,
        (true, false) => Severity::High,
        (false, _) => Severity::Medium,
    }
}

/// Same three-factor health formula used by `qartez_hotspots`. Duplicated here
/// because `qartez_health` is a pure aggregator over hotspots + smells; pulling
/// hotspots through its public entry point would reformat through a table and
/// discard the raw numbers we need for the combined report.
fn health_score(max_cc: f64, coupling: f64, churn: i64) -> f64 {
    let cc_h = 10.0 / (1.0 + max_cc / 10.0);
    let coupling_h = 10.0 / (1.0 + coupling * 50.0);
    let churn_h = 10.0 / (1.0 + churn as f64 / 8.0);
    (cc_h + coupling_h + churn_h) / 3.0
}

fn format_file_block(out: &mut String, r: &FileHealthRow) {
    out.push_str(&format!("### `{}` (Health: {:.1}/10)\n", r.path, r.health,));
    out.push_str(&format!(
        "- MaxCC={} PageRank={:.4} Churn={}\n",
        r.max_cc, r.pagerank, r.churn,
    ));
    if !r.smells.is_empty() {
        out.push_str("- Smells:\n");
        for s in &r.smells {
            match s {
                SmellEntry::God {
                    name,
                    cc,
                    lines,
                    line_start,
                } => {
                    out.push_str(&format!(
                        "  - god_function `{name}` @ L{line_start} (CC={cc}, lines={lines})\n"
                    ));
                }
                SmellEntry::LongParams {
                    name,
                    count,
                    line_start,
                } => {
                    out.push_str(&format!(
                        "  - long_params `{name}` @ L{line_start} ({count} params)\n"
                    ));
                }
            }
        }
    }
    out.push_str("- Recommended:\n");
    for rec in recommendations(r) {
        out.push_str(&format!("  - {rec}\n"));
    }
    out.push('\n');
}

fn recommendations(r: &FileHealthRow) -> Vec<String> {
    let mut recs = Vec::new();
    let has_god = r.smells.iter().any(|s| matches!(s, SmellEntry::God { .. }));
    let has_long = r
        .smells
        .iter()
        .any(|s| matches!(s, SmellEntry::LongParams { .. }));

    if has_god {
        recs.push(
            "Extract Method on the largest branches of the god function - each extracted branch typically drops parent CC by 1-4.".to_string(),
        );
    }
    if has_long {
        recs.push(
            "Introduce Parameter Object for the long param lists - groups correlated args, simplifies all call sites in one atomic change.".to_string(),
        );
    }
    if r.severity == Severity::High && !has_god && !has_long {
        recs.push(
            "No named smell, but high hotspot pressure. Start with `qartez_outline` to find the fattest function, then `qartez_refactor_plan` on this file.".to_string(),
        );
    }
    if r.churn >= 10 && r.pagerank > 0.01 {
        recs.push(
            "High churn + high coupling: stabilize the public surface first (narrow exports, add tests) before deeper refactors.".to_string(),
        );
    }
    if recs.is_empty() {
        recs.push(
            "Review the file with `qartez_outline` and `qartez_impact` before changes.".to_string(),
        );
    }
    recs
}
