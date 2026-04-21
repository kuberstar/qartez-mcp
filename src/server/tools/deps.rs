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

#[tool_router(router = qartez_deps_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_deps",
        description = "Show a file's dependency graph: what it imports (outgoing) and what imports it (incoming). Reveals coupling and helps plan safe changes.",
        annotations(
            title = "File Dependencies",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_deps(
        &self,
        Parameters(params): Parameters<SoulDepsParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let budget = params.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET as u32) as usize;
        let concise = is_concise(&params.format);
        let file = read::get_file_by_path(&conn, &params.file_path)
            .map_err(|e| format!("DB error: {e}"))?
            .ok_or_else(|| format!("File '{}' not found in index", params.file_path))?;

        let outgoing =
            read::get_edges_from(&conn, file.id).map_err(|e| format!("DB error: {e}"))?;
        let incoming = read::get_edges_to(&conn, file.id).map_err(|e| format!("DB error: {e}"))?;

        let blast_r = blast::blast_radius_for_file(&conn, file.id)
            .map(|r| r.transitive_count as i64)
            .unwrap_or(0);

        if is_mermaid(&params.format) {
            let center_id = helpers::mermaid_node_id(&params.file_path);
            let center_label = helpers::mermaid_label(&params.file_path);
            let mut out = format!("graph LR\n  {center_id}[\"{center_label}\"]\n");
            let max_nodes = 50;
            let mut count = 0usize;
            let mut seen_edges = HashSet::new();
            for edge in &outgoing {
                if count >= max_nodes {
                    out.push_str("  mermaid_truncated[\"... truncated\"]\n");
                    break;
                }
                if let Some(target) = read::get_file_by_id(&conn, edge.to_file).ok().flatten() {
                    let tid = helpers::mermaid_node_id(&target.path);
                    let edge_key = format!("{center_id}-->{tid}");
                    if !seen_edges.insert(edge_key) {
                        continue;
                    }
                    let tlabel = helpers::mermaid_label(&target.path);
                    out.push_str(&format!("  {center_id} --> {tid}[\"{tlabel}\"]\n"));
                    count += 1;
                }
            }
            for edge in &incoming {
                if count >= max_nodes {
                    out.push_str("  mermaid_truncated[\"... truncated\"]\n");
                    break;
                }
                if let Some(source) = read::get_file_by_id(&conn, edge.from_file).ok().flatten() {
                    let sid = helpers::mermaid_node_id(&source.path);
                    let edge_key = format!("{sid}-->{center_id}");
                    if !seen_edges.insert(edge_key) {
                        continue;
                    }
                    let slabel = helpers::mermaid_label(&source.path);
                    out.push_str(&format!("  {sid}[\"{slabel}\"] --> {center_id}\n"));
                    count += 1;
                }
            }
            return Ok(out);
        }

        if concise {
            let out_paths: Vec<String> = outgoing
                .iter()
                .filter_map(|e| {
                    read::get_file_by_id(&conn, e.to_file)
                        .ok()
                        .flatten()
                        .map(|f| f.path)
                })
                .collect();
            let in_paths: Vec<String> = incoming
                .iter()
                .filter_map(|e| {
                    read::get_file_by_id(&conn, e.from_file)
                        .ok()
                        .flatten()
                        .map(|f| f.path)
                })
                .collect();
            return Ok(format!(
                "{} (→{}): imports {} → [{}] | imported by {} ← [{}]",
                params.file_path,
                blast_r,
                out_paths.len(),
                out_paths.join(", "),
                in_paths.len(),
                in_paths.join(", "),
            ));
        }

        let mut out = format!(
            "# Dependencies: {} (blast →{})\n\n",
            params.file_path, blast_r
        );

        out.push_str(&format!("Imports from ({}):\n", outgoing.len()));
        if outgoing.is_empty() {
            out.push_str("  (no imports)\n");
        } else {
            for edge in &outgoing {
                let target_path = read::get_file_by_id(&conn, edge.to_file)
                    .ok()
                    .flatten()
                    .map(|f| f.path)
                    .unwrap_or_else(|| format!("file#{}", edge.to_file));
                let line = match edge.specifier.as_deref() {
                    Some(spec) if !spec.is_empty() => {
                        format!("  -> {} ({}: {})\n", target_path, edge.kind, spec)
                    }
                    _ => format!("  -> {} ({})\n", target_path, edge.kind),
                };
                if budget_exceeded(&mut out, &line, budget) {
                    break;
                }
                out.push_str(&line);
            }
        }

        out.push('\n');
        out.push_str(&format!("Imported by ({}):\n", incoming.len()));
        if incoming.is_empty() {
            out.push_str("  (no files import this file)\n");
        } else {
            for edge in &incoming {
                let source_path = read::get_file_by_id(&conn, edge.from_file)
                    .ok()
                    .flatten()
                    .map(|f| f.path)
                    .unwrap_or_else(|| format!("file#{}", edge.from_file));
                let line = match edge.specifier.as_deref() {
                    Some(spec) if !spec.is_empty() => {
                        format!("  <- {} ({}: {})\n", source_path, edge.kind, spec)
                    }
                    _ => format!("  <- {} ({})\n", source_path, edge.kind),
                };
                if budget_exceeded(&mut out, &line, budget) {
                    break;
                }
                out.push_str(&line);
            }
        }

        Ok(out)
    }
}
