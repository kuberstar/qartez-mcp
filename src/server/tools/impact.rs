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

#[tool_router(router = qartez_impact_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_impact",
        description = "MUST call before modifying any file with exports. Shows direct importers, transitive dependents, and co-change partners — the full set of files that could break.",
        annotations(
            title = "Impact Analysis",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_impact(
        &self,
        Parameters(params): Parameters<SoulImpactParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_impact")?;
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let concise = is_concise(&params.format);
        let include_tests = params.include_tests.unwrap_or(false);
        let file = read::get_file_by_path(&conn, &params.file_path)
            .map_err(|e| format!("DB error: {e}"))?
            .ok_or_else(|| format!("File '{}' not found in index", params.file_path))?;

        // Record that Claude has now reviewed the impact of editing this
        // file. The guard binary reads this as an acknowledgment and lets
        // subsequent Edit/Write calls on the same file through for a short
        // TTL window (see `qartez_mcp::guard`).
        guard::touch_ack(&self.project_root, &file.path);

        let blast_result =
            blast::blast_radius_for_file(&conn, file.id).map_err(|e| format!("Error: {e}"))?;

        let direct_names: Vec<String> = blast_result
            .direct_importers
            .iter()
            .filter_map(|&id| {
                read::get_file_by_id(&conn, id)
                    .ok()
                    .flatten()
                    .map(|f| f.path)
            })
            .filter(|p| include_tests || !is_test_path(p))
            .collect();

        let transitive_names: Vec<String> = blast_result
            .transitive_importers
            .iter()
            .filter_map(|&id| {
                read::get_file_by_id(&conn, id)
                    .ok()
                    .flatten()
                    .map(|f| f.path)
            })
            .filter(|p| include_tests || !is_test_path(p))
            .collect();

        let cochanges = read::get_cochanges(&conn, file.id, 10).unwrap_or_default();

        if concise {
            // Concise mode emits counts only; callers that want the actual
            // file lists drop the format flag. Prevents single-line spew
            // when a hub file has hundreds of importers.
            let out = format!(
                "Impact: {} | direct_importers={} transitive={} cochange={}\n",
                params.file_path,
                direct_names.len(),
                transitive_names.len(),
                cochanges.len(),
            );
            return Ok(out);
        }

        let mut out = format!("# Impact analysis: {}\n\n", params.file_path);
        out.push_str(&format!(
            "Direct importers ({}): {}\n",
            direct_names.len(),
            if direct_names.is_empty() {
                "none".to_string()
            } else {
                direct_names.join(", ")
            }
        ));

        out.push_str(&format!(
            "Transitive blast radius: {} file(s)\n",
            transitive_names.len(),
        ));
        for name in &transitive_names {
            out.push_str(&format!("  - {name}\n"));
        }

        if !cochanges.is_empty() {
            out.push_str(&format!("\nCo-change partners ({}):\n", cochanges.len()));
            for (cc, partner) in &cochanges {
                out.push_str(&format!(
                    "  {} (changed together {} times)\n",
                    partner.path, cc.count
                ));
            }
        }

        // Per-symbol breakdown: top symbols inside this file by
        // symbol-level PageRank. Helps Claude focus on the specific
        // symbols that matter when deciding how to edit safely. Only
        // emitted when there is actual signal (nonzero ranks) so legacy
        // DBs and languages without reference extraction do not produce
        // a confusing "all zeros" block.
        let hot_syms = read::get_symbols_ranked_for_file(&conn, file.id, 5).unwrap_or_default();
        let hot_syms_with_rank: Vec<&crate::storage::models::SymbolRow> =
            hot_syms.iter().filter(|s| s.pagerank > 0.0).collect();
        if !hot_syms_with_rank.is_empty() {
            out.push_str(&format!(
                "\nHot symbols in this file ({}):\n",
                hot_syms_with_rank.len()
            ));
            for sym in &hot_syms_with_rank {
                out.push_str(&format!(
                    "  {} ({}) pr={:.4} L{}-L{}\n",
                    sym.name, sym.kind, sym.pagerank, sym.line_start, sym.line_end,
                ));
            }
        }

        Ok(out)
    }
}
