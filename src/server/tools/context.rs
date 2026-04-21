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

#[tool_router(router = qartez_context_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_context",
        description = "Smart context builder: given files you plan to modify, returns the optimal set of related files to read first. Combines dependency graph, co-change history, and PageRank to prioritize what matters.",
        annotations(
            title = "Smart Context",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_context(
        &self,
        Parameters(params): Parameters<SoulContextParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let budget = params.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET as u32) as usize;
        let concise = is_concise(&params.format);
        let explain = params.explain.unwrap_or(false);
        let limit = params.limit.unwrap_or(15) as usize;

        if params.files.is_empty() {
            return Err("Provide at least one file path in 'files' parameter".to_string());
        }

        // Per-reason breakdown. Keyed by path, each entry tracks the
        // contribution of every signal so `explain=true` can surface the
        // decomposition instead of only the final score.
        let mut scored: HashMap<String, ScoreBreakdown> = HashMap::new();
        let mut input_file_ids: Vec<i64> = Vec::new();

        for file_path in &params.files {
            let file = match read::get_file_by_path(&conn, file_path)
                .map_err(|e| format!("DB error: {e}"))?
            {
                Some(f) => f,
                None => continue,
            };
            input_file_ids.push(file.id);

            let outgoing = read::get_edges_from(&conn, file.id).unwrap_or_default();
            for edge in &outgoing {
                if let Ok(Some(dep)) = read::get_file_by_id(&conn, edge.to_file)
                    && !params.files.contains(&dep.path)
                {
                    scored.entry(dep.path.clone()).or_default().imports +=
                        3.0 + dep.pagerank * 10.0;
                }
            }

            let incoming = read::get_edges_to(&conn, file.id).unwrap_or_default();
            for edge in &incoming {
                if let Ok(Some(imp)) = read::get_file_by_id(&conn, edge.from_file)
                    && !params.files.contains(&imp.path)
                {
                    scored.entry(imp.path.clone()).or_default().importer +=
                        2.0 + imp.pagerank * 5.0;
                }
            }

            let cochanges = read::get_cochanges(&conn, file.id, 10).unwrap_or_default();
            for (cc, partner) in &cochanges {
                if !params.files.contains(&partner.path) {
                    scored.entry(partner.path.clone()).or_default().cochange +=
                        cc.count as f64 * 1.5;
                }
            }

            let blast = blast::blast_radius_for_file(&conn, file.id).unwrap_or_else(|_| {
                blast::BlastResult {
                    file_id: file.id,
                    direct_importers: Vec::new(),
                    transitive_importers: Vec::new(),
                    transitive_count: 0,
                }
            });
            for &imp_id in &blast.transitive_importers {
                if input_file_ids.contains(&imp_id) {
                    continue;
                }
                if let Ok(Some(f)) = read::get_file_by_id(&conn, imp_id)
                    && !params.files.contains(&f.path)
                {
                    scored.entry(f.path.clone()).or_default().transitive += 0.5;
                }
            }
        }

        if let Some(ref task) = params.task {
            let words: Vec<&str> = task.split_whitespace().filter(|w| w.len() > 3).collect();
            for word in &words {
                let fts = if word.contains('*') {
                    word.to_string()
                } else {
                    format!("{word}*")
                };
                if let Ok(results) = read::search_symbols_fts(&conn, &fts, 10) {
                    for (sym, file_path) in &results {
                        if !params.files.contains(file_path) {
                            scored.entry(file_path.clone()).or_default().task_match += 1.0;
                        }
                        let _ = sym;
                    }
                }
            }
        }

        let mut ranked: Vec<(String, ScoreBreakdown)> = scored.into_iter().collect();
        ranked.sort_by(|a, b| {
            b.1.total()
                .partial_cmp(&a.1.total())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let total_candidates = ranked.len();
        let dropped_by_limit = total_candidates.saturating_sub(limit);
        ranked.truncate(limit);

        if ranked.is_empty() {
            return Ok(
                "No related context files found. The specified files may be isolated.".to_string(),
            );
        }

        let mut out = format!(
            "# Context for: {}\n{} related file(s) found:\n\n",
            params.files.join(", "),
            ranked.len(),
        );

        let mut dropped_by_budget: usize = 0;
        for (i, (path, breakdown)) in ranked.iter().enumerate() {
            let line = if concise {
                format!("  {} {}\n", i + 1, path)
            } else if explain {
                format!(
                    "{:>2}. {} (score: {:.1}) — {}\n",
                    i + 1,
                    path,
                    breakdown.total(),
                    breakdown.explain(),
                )
            } else {
                format!(
                    "{:>2}. {} (score: {:.1}) — {}\n",
                    i + 1,
                    path,
                    breakdown.total(),
                    breakdown.reasons().join(", "),
                )
            };
            if budget_exceeded(&mut out, &line, budget) {
                dropped_by_budget = ranked.len() - i;
                break;
            }
            out.push_str(&line);
        }

        if explain && (dropped_by_limit > 0 || dropped_by_budget > 0) {
            out.push_str(&format!(
                "\nExcluded: {dropped_by_limit} by limit, {dropped_by_budget} by token budget (candidates={total_candidates}, limit={limit}, budget={budget})\n",
            ));
        }

        Ok(out)
    }
}
