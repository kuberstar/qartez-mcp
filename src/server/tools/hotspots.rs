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

#[tool_router(router = qartez_hotspots_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_hotspots",
        description = "Find hotspot files or functions with a normalized 0-10 health score. Combines complexity, coupling (PageRank), and churn (git change frequency) into both a raw hotspot score and a health rating (10 = healthiest, 0 = worst). Use sort_by to rank by any individual factor; use threshold to filter unhealthy code (e.g. threshold=4 shows only files scoring 4 or below). Requires a prior index with git depth > 0.",
        annotations(
            title = "Hotspot Analysis",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_hotspots(
        &self,
        Parameters(params): Parameters<SoulHotspotsParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_hotspots")?;
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let limit = params.limit.unwrap_or(20) as usize;
        let concise = matches!(params.format, Some(Format::Concise));
        let level = params.level.unwrap_or(HotspotLevel::File);
        let sort_by = params.sort_by.unwrap_or_default();
        // `threshold=0` would require `health <= 0`, but the health
        // formula (`10 / (1 + x)`) is strictly positive, so zero
        // excludes every row. The previous build quietly clamped to
        // 1.0 which hid the misunderstanding behind surprising output.
        // Reject up front with an explanation of the value range so
        // callers see why their filter is empty.
        if let Some(0) = params.threshold {
            return Err(
                "threshold=0 excludes every file (only files with health score <= 0 would match, and health is always >= 0). Use threshold=10 to see all files, or the default for unhealthy-only view.".to_string(),
            );
        }
        let threshold = params.threshold.map(|t| t.min(10) as f64);
        let threshold_notice: &str = "";

        // Health score per factor: 10 / (1 + value / halflife).
        // The halflife is the value at which the factor score drops to 5.0.
        //   Complexity: halflife = 10 (CC 10 is the conventional warning threshold)
        //   Coupling:   halflife = 0.02 (top ~5% of files in a typical project)
        //   Churn:      halflife = 8 (moderate activity over the indexed git window)
        // Overall health = mean of the three factor scores, range [0, 10].
        let health_of = |max_cc: f64, coupling: f64, churn: i64| -> f64 {
            let cc_h = 10.0 / (1.0 + max_cc / 10.0);
            let coupling_h = 10.0 / (1.0 + coupling * 50.0);
            let churn_h = 10.0 / (1.0 + churn as f64 / 8.0);
            (cc_h + coupling_h + churn_h) / 3.0
        };

        match level {
            HotspotLevel::File => {
                let all_files = read::get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;

                // For each file, compute avg complexity of its functions.
                // Tuple: (path, score, avg_cc, max_cc, churn, coupling, health)
                let mut scored: Vec<(String, f64, f64, f64, i64, f64, f64)> = Vec::new();
                for file in &all_files {
                    let symbols = read::get_symbols_for_file(&conn, file.id).unwrap_or_default();
                    let complexities: Vec<u32> =
                        symbols.iter().filter_map(|s| s.complexity).collect();
                    if complexities.is_empty() {
                        continue;
                    }
                    let avg_cc = complexities.iter().copied().sum::<u32>() as f64
                        / complexities.len() as f64;
                    let max_cc = complexities.iter().copied().max().unwrap_or(1) as f64;
                    let coupling = file.pagerank;
                    let churn = file.change_count;
                    // Hotspot score: use max complexity (worst function in the
                    // file), weighted by coupling and change frequency. Adding
                    // 1 to churn avoids zeroing out files with no git history.
                    let score = max_cc * coupling * (1.0 + churn as f64);
                    let health = health_of(max_cc, coupling, churn);
                    if score > 0.0 {
                        scored.push((
                            file.path.clone(),
                            score,
                            avg_cc,
                            max_cc,
                            churn,
                            coupling,
                            health,
                        ));
                    }
                }

                if let Some(max_health) = threshold {
                    scored.retain(|entry| entry.6 <= max_health);
                }

                let cmp_f64 =
                    |a: &f64, b: &f64| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal);
                match sort_by {
                    HotspotSortBy::Score => scored.sort_by(|a, b| cmp_f64(&b.1, &a.1)),
                    HotspotSortBy::Health => scored.sort_by(|a, b| cmp_f64(&a.6, &b.6)),
                    HotspotSortBy::Complexity => scored.sort_by(|a, b| cmp_f64(&b.3, &a.3)),
                    HotspotSortBy::Coupling => scored.sort_by(|a, b| cmp_f64(&b.5, &a.5)),
                    HotspotSortBy::Churn => scored.sort_by(|a, b| b.4.cmp(&a.4)),
                }
                scored.truncate(limit);

                if scored.is_empty() {
                    let mut msg = String::new();
                    msg.push_str(threshold_notice);
                    msg.push_str("No hotspots found. Re-index with git history (--git-depth > 0) and imperative language files for complexity data.");
                    return Ok(msg);
                }

                let mut out = String::new();
                out.push_str(threshold_notice);
                // When the caller overrides the default composite sort
                // with `sort_by=complexity` (or another axis), the table
                // still shows the composite Score column. Annotate the
                // header so the reader understands the ordering does
                // not come from the Score column they see. Without the
                // note, the ranking looked arbitrary because the top
                // rows weren't highest by Score.
                if !matches!(sort_by, HotspotSortBy::Score) {
                    let axis = match sort_by {
                        HotspotSortBy::Score => "score",
                        HotspotSortBy::Health => "health",
                        HotspotSortBy::Complexity => "complexity",
                        HotspotSortBy::Coupling => "coupling",
                        HotspotSortBy::Churn => "churn",
                    };
                    out.push_str(&format!(
                        "// sorted by {axis}, Score column remains the composite\n"
                    ));
                }
                // Compact header when the visible output would be at
                // most three rows: the verbose banner (title + formula
                // lines + column header + ruler = 5 lines) otherwise
                // dwarfs the payload. The concise format always skips
                // the banner; the detailed format only keeps it when
                // the result is large enough to justify the explanation.
                let is_small = scored.len() <= 3 || limit <= 3;
                if concise {
                    out.push_str("# score health file avg_cc max_cc churn pagerank\n");
                    for (i, (path, score, avg, max, churn, pr, health)) in scored.iter().enumerate()
                    {
                        out.push_str(&format!(
                            "{} {:.2} {:.1} {} {:.1} {:.0} {} {:.4}\n",
                            i + 1,
                            score,
                            health,
                            path,
                            avg,
                            max,
                            churn,
                            pr,
                        ));
                    }
                } else {
                    if !is_small {
                        out.push_str("# Hotspot Analysis (file level)\n\n");
                        out.push_str(
                            "Health = mean of per-factor scores (0-10 scale, 10 = healthiest)\n",
                        );
                        out.push_str(
                            "Hotspot score = max_complexity x pagerank x (1 + change_count)\n\n",
                        );
                    }
                    out.push_str("  # | Score     | Health | File                               | AvgCC | MaxCC | Churn | PageRank\n");
                    out.push_str("----+-----------+--------+------------------------------------+-------+-------+-------+---------\n");
                    for (i, (path, score, avg, max, churn, pr, health)) in scored.iter().enumerate()
                    {
                        out.push_str(&format!(
                            "{:>3} | {:>9.2} | {:>6.1} | {:<34} | {:>5.1} | {:>5.0} | {:>5} | {:>8.4}\n",
                            i + 1,
                            score,
                            health,
                            truncate_path(path, 34),
                            avg,
                            max,
                            churn,
                            pr,
                        ));
                    }
                }
                Ok(out)
            }
            HotspotLevel::Symbol => {
                let all_symbols =
                    read::get_all_symbols_with_path(&conn).map_err(|e| format!("DB error: {e}"))?;

                // Pre-load file change counts.
                let all_files = read::get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;
                let file_churn: HashMap<i64, i64> =
                    all_files.iter().map(|f| (f.id, f.change_count)).collect();

                // Tuple: (name, kind, path, score, cc, pagerank, churn, health)
                let mut scored = Vec::<(String, String, String, f64, u32, f64, i64, f64)>::new();
                for (sym, file_path) in &all_symbols {
                    let cc = match sym.complexity {
                        Some(c) if c > 0 => c,
                        _ => continue,
                    };
                    let churn = file_churn.get(&sym.file_id).copied().unwrap_or(0);
                    let score = cc as f64 * sym.pagerank * (1.0 + churn as f64);
                    let health = health_of(cc as f64, sym.pagerank, churn);
                    if score > 0.0 {
                        scored.push((
                            sym.name.clone(),
                            sym.kind.clone(),
                            file_path.clone(),
                            score,
                            cc,
                            sym.pagerank,
                            churn,
                            health,
                        ));
                    }
                }

                if let Some(max_health) = threshold {
                    scored.retain(|entry| entry.7 <= max_health);
                }

                let cmp_f64 =
                    |a: &f64, b: &f64| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal);
                match sort_by {
                    HotspotSortBy::Score => scored.sort_by(|a, b| cmp_f64(&b.3, &a.3)),
                    HotspotSortBy::Health => scored.sort_by(|a, b| cmp_f64(&a.7, &b.7)),
                    HotspotSortBy::Complexity => {
                        scored.sort_by(|a, b| b.4.cmp(&a.4));
                    }
                    HotspotSortBy::Coupling => scored.sort_by(|a, b| cmp_f64(&b.5, &a.5)),
                    HotspotSortBy::Churn => scored.sort_by(|a, b| b.6.cmp(&a.6)),
                }
                scored.truncate(limit);

                if scored.is_empty() {
                    let mut msg = String::new();
                    msg.push_str(threshold_notice);
                    msg.push_str("No symbol hotspots found. Complexity data requires imperative language files (Rust, TS, Python, Go, etc.).");
                    return Ok(msg);
                }

                let mut out = String::new();
                out.push_str(threshold_notice);
                // See the file-level branch for the rationale: when the
                // caller sorts by a non-default axis we still render the
                // composite Score column, so callers need an explicit
                // note that the column they see is NOT the sort key.
                if !matches!(sort_by, HotspotSortBy::Score) {
                    let axis = match sort_by {
                        HotspotSortBy::Score => "score",
                        HotspotSortBy::Health => "health",
                        HotspotSortBy::Complexity => "complexity",
                        HotspotSortBy::Coupling => "coupling",
                        HotspotSortBy::Churn => "churn",
                    };
                    out.push_str(&format!(
                        "// sorted by {axis}, Score column remains the composite\n"
                    ));
                }
                // Compact header mirror of the file-level branch: drop
                // the explanatory banner when the result set is tiny
                // so the header block does not outweigh the payload.
                let is_small = scored.len() <= 3 || limit <= 3;
                if concise {
                    out.push_str("# score health name kind file cc pagerank churn\n");
                    for (i, (name, kind, path, score, cc, pr, churn, health)) in
                        scored.iter().enumerate()
                    {
                        out.push_str(&format!(
                            "{} {:.4} {:.1} {} {} {} {} {:.4} {}\n",
                            i + 1,
                            score,
                            health,
                            name,
                            kind,
                            path,
                            cc,
                            pr,
                            churn,
                        ));
                    }
                } else {
                    if !is_small {
                        out.push_str("# Hotspot Analysis (symbol level)\n\n");
                        out.push_str(
                            "Health = mean of per-factor scores (0-10 scale, 10 = healthiest)\n",
                        );
                        out.push_str("Hotspot score = complexity x symbol_pagerank x (1 + file_change_count)\n\n");
                    }
                    out.push_str("  # | Score    | Health | Symbol                    | Kind     | File                          | CC | PageRank | Churn\n");
                    out.push_str("----+----------+--------+---------------------------+----------+-------------------------------+----+----------+------\n");
                    for (i, (name, kind, path, score, cc, pr, churn, health)) in
                        scored.iter().enumerate()
                    {
                        out.push_str(&format!(
                            "{:>3} | {:>8.4} | {:>6.1} | {:<25} | {:<8} | {:<29} | {:>2} | {:>8.4} | {:>5}\n",
                            i + 1,
                            score,
                            health,
                            truncate_path(name, 25),
                            truncate_path(kind, 8),
                            truncate_path(path, 29),
                            cc,
                            pr,
                            churn,
                        ));
                    }
                }
                Ok(out)
            }
        }
    }
}
