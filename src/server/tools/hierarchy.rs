// Rust guideline compliant 2026-04-22

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

#[tool_router(router = qartez_hierarchy_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_hierarchy",
        description = "Query the type hierarchy: find all types that implement a trait/interface, or all traits/interfaces a type implements. Works across Rust (impl Trait for Type), TypeScript/Java (extends/implements), Python (base classes), and Go (interface embedding). `max_depth=0` returns only the seed symbol with no children or parents."
    )]
    pub(in crate::server) fn qartez_hierarchy(
        &self,
        Parameters(params): Parameters<SoulHierarchyParams>,
    ) -> Result<String, String> {
        let concise = is_concise(&params.format);
        let direction = params.direction.as_deref().unwrap_or("sub").to_lowercase();
        let transitive = params.transitive.unwrap_or(false);
        const DEFAULT_MAX_DEPTH: u32 = 20;
        let max_depth = params.max_depth.unwrap_or(DEFAULT_MAX_DEPTH);

        if is_mermaid(&params.format) {
            return self.qartez_hierarchy_mermaid(
                &params.symbol,
                &direction,
                transitive,
                max_depth,
            );
        }

        if max_depth == 0 {
            // max_depth=0 is the documented "seed-only" shortcut: the
            // caller just wants to confirm the symbol itself is in scope
            // without paying for either a direct or a transitive walk.
            match direction.as_str() {
                "sub" | "down" | "implementors" | "super" | "up" | "supertypes" => {
                    return Ok(format!(
                        "# Seed symbol only (max_depth=0): '{}'\n\nNo children or parents traversed. Increase max_depth to walk the hierarchy.\n",
                        params.symbol
                    ));
                }
                _ => {
                    return Err(format!(
                        "Invalid direction '{direction}'. Use 'sub' (what implements this?) or 'super' (what does this implement?)."
                    ));
                }
            }
        }

        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;

        let mut out = String::new();

        match direction.as_str() {
            "sub" | "down" | "implementors" => {
                let rows = read::get_subtypes(&conn, &params.symbol)
                    .map_err(|e| format!("DB error: {e}"))?;
                if rows.is_empty() {
                    return Ok(format!(
                        "No types found that implement or extend '{}'.",
                        params.symbol
                    ));
                }
                out.push_str(&format!(
                    "# Types implementing/extending '{}' ({} found)\n\n",
                    params.symbol,
                    rows.len()
                ));
                for (rel, file) in &rows {
                    if concise {
                        out.push_str(&format!(
                            "{} {} {} ({}:L{})\n",
                            rel.sub_name, rel.kind, rel.super_name, file.path, rel.line
                        ));
                    } else {
                        out.push_str(&format!(
                            "  {} {} {}\n    {} [L{}]\n",
                            rel.sub_name, rel.kind, rel.super_name, file.path, rel.line
                        ));
                    }
                }

                if transitive {
                    let mut visited: HashSet<String> = HashSet::new();
                    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
                    for (rel, _) in &rows {
                        if visited.insert(rel.sub_name.clone()) {
                            queue.push_back((rel.sub_name.clone(), 1));
                        }
                    }
                    let mut transitive_rows = Vec::new();
                    while let Some((name, depth)) = queue.pop_front() {
                        if depth > max_depth {
                            break;
                        }
                        let sub_rows = read::get_subtypes(&conn, &name)
                            .map_err(|e| format!("DB error: {e}"))?;
                        for (rel, file) in sub_rows {
                            if visited.insert(rel.sub_name.clone()) {
                                queue.push_back((rel.sub_name.clone(), depth + 1));
                                transitive_rows.push((rel, file, depth));
                            }
                        }
                    }
                    if !transitive_rows.is_empty() {
                        out.push_str(&format!(
                            "\n  Transitive subtypes ({}):\n",
                            transitive_rows.len()
                        ));
                        for (rel, file, depth) in &transitive_rows {
                            out.push_str(&format!(
                                "    [depth {}] {} {} {} ({}:L{})\n",
                                depth, rel.sub_name, rel.kind, rel.super_name, file.path, rel.line
                            ));
                        }
                    }
                }
            }
            "super" | "up" | "supertypes" => {
                let rows = read::get_supertypes(&conn, &params.symbol)
                    .map_err(|e| format!("DB error: {e}"))?;
                if rows.is_empty() {
                    return Ok(format!("No supertypes found for '{}'.", params.symbol));
                }
                out.push_str(&format!(
                    "# Supertypes of '{}' ({} found)\n\n",
                    params.symbol,
                    rows.len()
                ));
                for (rel, file) in &rows {
                    if concise {
                        out.push_str(&format!(
                            "{} {} {} ({}:L{})\n",
                            rel.sub_name, rel.kind, rel.super_name, file.path, rel.line
                        ));
                    } else {
                        out.push_str(&format!(
                            "  {} {} {}\n    {} [L{}]\n",
                            rel.sub_name, rel.kind, rel.super_name, file.path, rel.line
                        ));
                    }
                }

                if transitive {
                    let mut visited: HashSet<String> = HashSet::new();
                    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
                    for (rel, _) in &rows {
                        if visited.insert(rel.super_name.clone()) {
                            queue.push_back((rel.super_name.clone(), 1));
                        }
                    }
                    let mut transitive_rows = Vec::new();
                    while let Some((name, depth)) = queue.pop_front() {
                        if depth > max_depth {
                            break;
                        }
                        let sup_rows = read::get_supertypes(&conn, &name)
                            .map_err(|e| format!("DB error: {e}"))?;
                        for (rel, file) in sup_rows {
                            if visited.insert(rel.super_name.clone()) {
                                queue.push_back((rel.super_name.clone(), depth + 1));
                                transitive_rows.push((rel, file, depth));
                            }
                        }
                    }
                    if !transitive_rows.is_empty() {
                        out.push_str(&format!(
                            "\n  Transitive supertypes ({}):\n",
                            transitive_rows.len()
                        ));
                        for (rel, file, depth) in &transitive_rows {
                            out.push_str(&format!(
                                "    [depth {}] {} {} {} ({}:L{})\n",
                                depth, rel.sub_name, rel.kind, rel.super_name, file.path, rel.line
                            ));
                        }
                    }
                }
            }
            _ => {
                return Err(format!(
                    "Invalid direction '{direction}'. Use 'sub' (what implements this?) or 'super' (what does this implement?)."
                ));
            }
        }

        Ok(out)
    }
}

impl QartezServer {
    /// Render type hierarchy as a Mermaid flowchart.
    ///
    /// Honors the "seed-only" shortcut: `max_depth=0` renders a single
    /// node representing the requested symbol, matching the textual
    /// tool's contract so the mermaid path doesn't silently return a
    /// full traversal.
    fn qartez_hierarchy_mermaid(
        &self,
        symbol: &str,
        direction: &str,
        transitive: bool,
        max_depth: u32,
    ) -> Result<String, String> {
        if max_depth == 0 {
            match direction {
                "sub" | "down" | "implementors" | "super" | "up" | "supertypes" => {
                    let root_id = helpers::mermaid_node_id(symbol);
                    let root_label = helpers::mermaid_label(symbol);
                    let dir_tag = if matches!(direction, "super" | "up" | "supertypes") {
                        "BT"
                    } else {
                        "TD"
                    };
                    return Ok(format!("graph {dir_tag}\n  {root_id}[\"{root_label}\"]\n"));
                }
                _ => {
                    return Err(format!(
                        "Invalid direction '{direction}'. Use 'sub' or 'super'."
                    ));
                }
            }
        }

        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let max_nodes = 50;
        let mut count = 0usize;

        match direction {
            "sub" | "down" | "implementors" => {
                let rows =
                    read::get_subtypes(&conn, symbol).map_err(|e| format!("DB error: {e}"))?;
                if rows.is_empty() {
                    return Ok(format!(
                        "No types found that implement or extend '{symbol}'."
                    ));
                }
                let mut out = String::from("graph TD\n");
                let root_id = helpers::mermaid_node_id(symbol);
                let root_label = helpers::mermaid_label(symbol);
                out.push_str(&format!("  {root_id}[\"{root_label}\"]\n"));

                for (rel, _) in &rows {
                    if count >= max_nodes {
                        out.push_str("  truncated[\"... truncated\"]\n");
                        break;
                    }
                    let sid = helpers::mermaid_node_id(&rel.sub_name);
                    let slabel = helpers::mermaid_label(&rel.sub_name);
                    out.push_str(&format!(
                        "  {sid}[\"{slabel}\"] -->|{kind}| {root_id}\n",
                        kind = rel.kind
                    ));
                    count += 1;
                }

                if transitive {
                    let mut visited: HashSet<String> = HashSet::new();
                    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
                    for (rel, _) in &rows {
                        if visited.insert(rel.sub_name.clone()) {
                            queue.push_back((rel.sub_name.clone(), 1));
                        }
                    }
                    while let Some((name, depth)) = queue.pop_front() {
                        if depth > max_depth || count >= max_nodes {
                            break;
                        }
                        let sub_rows = read::get_subtypes(&conn, &name)
                            .map_err(|e| format!("DB error: {e}"))?;
                        for (rel, _) in sub_rows {
                            if count >= max_nodes {
                                out.push_str("  truncated[\"... truncated\"]\n");
                                break;
                            }
                            if visited.insert(rel.sub_name.clone()) {
                                queue.push_back((rel.sub_name.clone(), depth + 1));
                                let sid = helpers::mermaid_node_id(&rel.sub_name);
                                let slabel = helpers::mermaid_label(&rel.sub_name);
                                let pid = helpers::mermaid_node_id(&name);
                                out.push_str(&format!(
                                    "  {sid}[\"{slabel}\"] -->|{kind}| {pid}\n",
                                    kind = rel.kind
                                ));
                                count += 1;
                            }
                        }
                    }
                }

                Ok(out)
            }
            "super" | "up" | "supertypes" => {
                let rows =
                    read::get_supertypes(&conn, symbol).map_err(|e| format!("DB error: {e}"))?;
                if rows.is_empty() {
                    return Ok(format!("No supertypes found for '{symbol}'."));
                }
                let mut out = String::from("graph BT\n");
                let root_id = helpers::mermaid_node_id(symbol);
                let root_label = helpers::mermaid_label(symbol);
                out.push_str(&format!("  {root_id}[\"{root_label}\"]\n"));

                for (rel, _) in &rows {
                    if count >= max_nodes {
                        out.push_str("  truncated[\"... truncated\"]\n");
                        break;
                    }
                    let sid = helpers::mermaid_node_id(&rel.super_name);
                    let slabel = helpers::mermaid_label(&rel.super_name);
                    out.push_str(&format!(
                        "  {root_id} -->|{kind}| {sid}[\"{slabel}\"]\n",
                        kind = rel.kind
                    ));
                    count += 1;
                }

                if transitive {
                    let mut visited: HashSet<String> = HashSet::new();
                    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
                    for (rel, _) in &rows {
                        if visited.insert(rel.super_name.clone()) {
                            queue.push_back((rel.super_name.clone(), 1));
                        }
                    }
                    while let Some((name, depth)) = queue.pop_front() {
                        if depth > max_depth || count >= max_nodes {
                            break;
                        }
                        let sup_rows = read::get_supertypes(&conn, &name)
                            .map_err(|e| format!("DB error: {e}"))?;
                        for (rel, _) in sup_rows {
                            if count >= max_nodes {
                                out.push_str("  truncated[\"... truncated\"]\n");
                                break;
                            }
                            if visited.insert(rel.super_name.clone()) {
                                queue.push_back((rel.super_name.clone(), depth + 1));
                                let sid = helpers::mermaid_node_id(&rel.super_name);
                                let slabel = helpers::mermaid_label(&rel.super_name);
                                let pid = helpers::mermaid_node_id(&name);
                                out.push_str(&format!(
                                    "  {pid} -->|{kind}| {sid}[\"{slabel}\"]\n",
                                    kind = rel.kind
                                ));
                                count += 1;
                            }
                        }
                    }
                }

                Ok(out)
            }
            _ => Err(format!(
                "Invalid direction '{direction}'. Use 'sub' or 'super'."
            )),
        }
    }
}
