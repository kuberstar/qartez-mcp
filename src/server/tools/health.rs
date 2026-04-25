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

#[tool_router(router = qartez_health_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_health",
        description = "Prioritized, actionable health report that cross-references qartez_hotspots (complexity x coupling x churn) with qartez_smells (god functions, long parameter lists, feature envy). Files that score badly in both signals are surfaced first as 'critical' with a concrete suggested refactor technique. Use `qartez_refactor_plan file_path=<X>` to expand a recommendation into a step-by-step plan. Filter semantics: `max_health` is the upper bound on the 0-10 health scale (default 5.0); only files with score <= max_health that ALSO match a hotspot OR smell heuristic are listed. `max_health=10` therefore means \"include every file with at least one of max_cc > 0 or a named smell\", not \"every indexed file\". Pass `limit=0` to remove the row cap; the default is 15.",
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
        // Reject max_health outside [0.0, 10.0] outright. The scale is
        // a 0-to-10 health score, so values above 10 are meaningless -
        // previously the tool silently clamped them to 10, which hid
        // typos (`100` vs `10`) behind an identical response. Negative
        // values never match any row and used to surface the "Avg
        // health 10.0/10" stub, which read like success.
        if let Some(m) = params.max_health
            && (!m.is_finite() || !(0.0..=10.0).contains(&m))
        {
            return Err(format!(
                "max_health must be in range 0..=10 (got {m}). Use a value in [0.0, 10.0]."
            ));
        }
        let max_health = params.max_health.unwrap_or(5.0);
        // `min_complexity=0` labelled every CC=1 helper as a
        // `god_function`, which is nonsense: the "god function"
        // heuristic describes a function whose branch count alone
        // makes it hard to understand. Reject 0 outright with a
        // clear message instead of silently producing a wall of
        // false positives. Wording mirrors `qartez_clones` /
        // `qartez_smells` which already reject the same value so
        // callers see a consistent remediation hint across tools.
        if let Some(0) = params.min_complexity {
            return Err(
                "min_complexity must be >= 1 (0 matches every function). Use the default or a positive integer.".into(),
            );
        }
        let min_cc_explicit = params.min_complexity;
        let min_cc = params.min_complexity.unwrap_or(15).max(1);
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
                    // Inherit the same flat-dispatcher classification
                    // qartez_smells uses so health doesn't recommend
                    // "Extract Method" on a flat match/switch table.
                    // Dispatchers carry their CC budget in arm count, so
                    // extracting a single arm rarely helps - the smell
                    // emits a different recommendation, and so should
                    // health. Using `super::smells::fetch_symbol_body`
                    // (already `pub(super)`) plus a local arm counter
                    // avoids duplicating the FTS query and stays in
                    // sync with the smells classifier's CC slack /
                    // dominant-arm rules. When the body is unavailable
                    // (older index, language without body extraction),
                    // we fall back to `god_function` - same downgrade
                    // policy as qartez_smells.
                    let (kind, arm_count) = classify_function_shape(&conn, sym.id, cc);
                    smells_by_file
                        .entry(path.clone())
                        .or_default()
                        .push(SmellEntry::God {
                            name: sym.name.clone(),
                            cc,
                            lines: body,
                            line_start: sym.line_start,
                            kind,
                            arm_count,
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
        // Per-severity cap, NOT a whole-list truncate. Before, the
        // list was sorted Critical -> High -> Medium and then sliced
        // at `limit`, which dropped entire severity buckets whenever
        // Critical alone met the cap. Callers then saw "max_health=10
        // limit=5 -> 5 files" while "max_health=10 limit=0 -> 6 files"
        // even though the two queries should differ only in whether
        // the display is paginated. Applying `limit` per severity
        // bucket keeps at least one row from each bucket in view and
        // makes the "critical-first, then high, then medium" ordering
        // visible on small caps. Round-robin across buckets so the
        // cap divides evenly even when Critical dominates.
        if limit != usize::MAX && limit < rows.len() {
            let mut buckets: [VecDeque<FileHealthRow>; 3] =
                [VecDeque::new(), VecDeque::new(), VecDeque::new()];
            for r in rows.drain(..) {
                let slot = r.severity.rank() as usize;
                buckets[slot].push_back(r);
            }
            let mut kept: Vec<FileHealthRow> = Vec::with_capacity(limit);
            'fill: loop {
                let before = kept.len();
                for bucket in buckets.iter_mut() {
                    if let Some(row) = bucket.pop_front() {
                        kept.push(row);
                        if kept.len() >= limit {
                            break 'fill;
                        }
                    }
                }
                if kept.len() == before {
                    break;
                }
            }
            // Re-sort into canonical severity+health order so the
            // per-severity sections below render in sorted order.
            kept.sort_by(|a, b| {
                a.severity.rank().cmp(&b.severity.rank()).then(
                    a.health
                        .partial_cmp(&b.health)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
            });
            rows = kept;
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
        /// Sub-kind matching `qartez_smells`'s classifier output.
        /// `"god_function"` is the default; `"flat_dispatcher"` marks a
        /// function whose CC is dominated by a flat match/switch table
        /// over many trivial arms. Recommendations branch on this so
        /// the report does not advise "Extract Method" on a structure
        /// that responds poorly to it.
        kind: &'static str,
        /// Detected match-arm count when `kind` is `"flat_dispatcher"`,
        /// zero otherwise. Echoes the same field qartez_smells exposes
        /// so the two tools speak the same vocabulary.
        arm_count: u32,
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
                    kind,
                    arm_count,
                } => {
                    if *kind == "flat_dispatcher" {
                        out.push_str(&format!(
                            "  - flat_dispatcher `{name}` @ L{line_start} (CC={cc}, lines={lines}, arms={arm_count})\n"
                        ));
                    } else {
                        out.push_str(&format!(
                            "  - god_function `{name}` @ L{line_start} (CC={cc}, lines={lines})\n"
                        ));
                    }
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
    let has_god_plain = r
        .smells
        .iter()
        .any(|s| matches!(s, SmellEntry::God { kind, .. } if *kind != "flat_dispatcher"));
    let has_dispatcher = r
        .smells
        .iter()
        .any(|s| matches!(s, SmellEntry::God { kind, .. } if *kind == "flat_dispatcher"));
    let has_long = r
        .smells
        .iter()
        .any(|s| matches!(s, SmellEntry::LongParams { .. }));

    if has_god_plain {
        recs.push(
            "Extract Method on the largest branches of the god function - each extracted branch typically drops parent CC by 1-4.".to_string(),
        );
    }
    if has_dispatcher {
        // Mirrors the flat_dispatcher note `qartez_smells` already
        // emits below its god-function table. CC of a flat dispatch
        // table grows linearly with arm count, so "Extract Method"
        // rarely helps - the actionable refactor is to split the
        // table by variant into purpose-built handlers, not to
        // pull one arm into its own function.
        recs.push(
            "Flat dispatcher: avoid Extract Method on individual arms - CC grows linearly with arm count. Prefer splitting the dispatch table into per-variant handlers, or accept the shape unless arms grow non-trivial.".to_string(),
        );
    }
    if has_long {
        recs.push(
            "Introduce Parameter Object for the long param lists - groups correlated args, simplifies all call sites in one atomic change.".to_string(),
        );
    }
    if r.severity == Severity::High && !has_god_plain && !has_dispatcher && !has_long {
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

/// Arm-count threshold below which the flat-dispatcher classifier won't
/// fire. Mirrors `FLAT_DISPATCHER_MIN_ARMS` in `qartez_smells` so the two
/// tools agree on what qualifies as a dispatch table.
const HEALTH_FLAT_DISPATCHER_MIN_ARMS: u32 = 6;
/// Maximum slack between raw CC and arm count for the tight-dispatcher
/// path. Mirrors `FLAT_DISPATCHER_CC_SLACK` in `qartez_smells`.
const HEALTH_FLAT_DISPATCHER_CC_SLACK: u32 = 5;
/// Arm count above which the looser dominant-arm path qualifies a function
/// as a dispatcher. Mirrors `FLAT_DISPATCHER_MIN_ARMS_DOMINANT`.
const HEALTH_FLAT_DISPATCHER_MIN_ARMS_DOMINANT: u32 = 12;
/// Arm-fraction-of-CC threshold for the dominant-arm path. Mirrors
/// `FLAT_DISPATCHER_ARM_FRACTION`.
const HEALTH_FLAT_DISPATCHER_ARM_FRACTION: f64 = 0.4;

/// Re-classify a god function as a flat dispatcher when its CC is
/// dominated by a flat match/switch table. Returns the same
/// `(kind, arm_count)` shape `qartez_smells::classify_function_shape`
/// produces so the two tools surface identical sub-kinds for the same
/// function. Reuses `super::smells::fetch_symbol_body` (already
/// `pub(super)`) for the FTS lookup; the arm counter is duplicated
/// locally rather than exposing the smells private helper, since
/// keeping the smells module untouched is a hard constraint of this
/// audit pass.
fn classify_function_shape(
    conn: &rusqlite::Connection,
    symbol_id: i64,
    cc: u32,
) -> (&'static str, u32) {
    let Some(body) = super::smells::fetch_symbol_body(conn, symbol_id) else {
        return ("god_function", 0);
    };
    let arms = count_match_arms(&body);
    if arms < HEALTH_FLAT_DISPATCHER_MIN_ARMS {
        return ("god_function", 0);
    }
    if cc <= arms.saturating_add(HEALTH_FLAT_DISPATCHER_CC_SLACK) {
        return ("flat_dispatcher", arms);
    }
    if arms >= HEALTH_FLAT_DISPATCHER_MIN_ARMS_DOMINANT
        && (arms as f64) >= (cc as f64) * HEALTH_FLAT_DISPATCHER_ARM_FRACTION
    {
        return ("flat_dispatcher", arms);
    }
    ("god_function", 0)
}

/// Line-level proxy for `=>` arrow occurrences in a function body.
/// Skips characters inside `//` line comments and string literals so
/// in-arm doc strings don't inflate the count. Mirrors the parser used
/// by `qartez_smells::count_match_arms` so both tools agree on arm
/// detection.
fn count_match_arms(body: &str) -> u32 {
    let mut count: u32 = 0;
    for raw_line in body.lines() {
        let line = match raw_line.find("//") {
            Some(i) => &raw_line[..i],
            None => raw_line,
        };
        let bytes = line.as_bytes();
        let mut i = 0;
        let mut in_string = false;
        let mut escape = false;
        while i + 1 < bytes.len() {
            let b = bytes[i];
            if escape {
                escape = false;
                i += 1;
                continue;
            }
            match b {
                b'\\' if in_string => {
                    escape = true;
                }
                b'"' => {
                    in_string = !in_string;
                }
                b'=' if !in_string
                    && i + 1 < bytes.len()
                    && bytes[i + 1] == b'>'
                    && (i == 0 || (bytes[i - 1] != b'=' && bytes[i - 1] != b'>')) =>
                {
                    count = count.saturating_add(1);
                    i += 2;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
    }
    count
}
