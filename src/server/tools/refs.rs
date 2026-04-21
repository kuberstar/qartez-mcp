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

#[tool_router(router = qartez_refs_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_refs",
        description = "Trace all usages of a symbol: which files import it, and (with transitive=true) the full dependency chain. Essential before renaming, moving, or deleting a symbol.",
        annotations(
            title = "Symbol References",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_refs(
        &self,
        Parameters(params): Parameters<SoulRefsParams>,
    ) -> Result<String, String> {
        let budget = params.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET as u32) as usize;
        let concise = is_concise(&params.format);
        let transitive = params.transitive.unwrap_or(false);

        // All DB queries under one lock acquisition; the lock is dropped
        // before the tree-sitter / FS phase (cached_calls) so the watcher
        // and other handlers are not blocked during parsing.
        let (refs, fts_fallback_paths, reverse_graph, file_path_lookup) = {
            let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
            let refs = read::get_symbol_references(&conn, &params.symbol)
                .map_err(|e| format!("DB error: {e}"))?;
            if refs.is_empty() {
                return Ok(format!("No symbol found with name '{}'", params.symbol));
            }

            // FTS fallback: files whose symbol bodies mention the target
            // identifier. Supplements the edge graph because not every caller
            // shows up as a direct importer — external-crate `use` lines are
            // dropped at index time, `use crate::a::sub;` resolves to `a/mod.rs`
            // not `a/sub.rs`, and child modules pulled in via `use super::*;`
            // carry a wildcard specifier that the old importer filter dropped.
            // Failures are non-fatal: if FTS is missing we still have the
            // edge-based scan set below.
            let fts_fallback_paths: Vec<String> =
                read::find_file_paths_by_body_text(&conn, &params.symbol).unwrap_or_default();

            let (reverse_graph, file_path_lookup) = if transitive {
                let all_edges = read::get_all_edges(&conn).map_err(|e| format!("DB error: {e}"))?;
                let mut reverse: HashMap<i64, Vec<i64>> = HashMap::new();
                for &(from, to) in &all_edges {
                    reverse.entry(to).or_default().push(from);
                }
                let lookup: HashMap<i64, String> = read::get_all_files(&conn)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|f| (f.id, f.path))
                    .collect();
                (reverse, lookup)
            } else {
                (HashMap::new(), HashMap::new())
            };

            (refs, fts_fallback_paths, reverse_graph, file_path_lookup)
        };

        let mut out = String::new();

        for (sym, file, importers) in &refs {
            if concise {
                let paths: Vec<&str> = importers.iter().map(|(_, f)| f.path.as_str()).collect();
                out.push_str(&format!(
                    "{} ({}) in {} — {} ref(s): {}\n",
                    sym.name,
                    sym.kind,
                    file.path,
                    paths.len(),
                    if paths.is_empty() {
                        "none".to_string()
                    } else {
                        paths.join(", ")
                    },
                ));
                continue;
            }

            out.push_str(&format!(
                "# Symbol: {} ({})\n  Defined in: {} [L{}-L{}]\n\n",
                sym.name, sym.kind, file.path, sym.line_start, sym.line_end,
            ));

            if importers.is_empty() {
                out.push_str("  No direct references found.\n\n");
            } else {
                out.push_str(&format!("  Direct references ({}):\n", importers.len()));
                for (edge, importer_file) in importers {
                    let line = format!(
                        "    {} — imports via '{}' ({})\n",
                        importer_file.path,
                        edge.specifier.as_deref().unwrap_or("(unspecified)"),
                        edge.kind,
                    );
                    if budget_exceeded(&mut out, &line, budget) {
                        break;
                    }
                    out.push_str(&line);
                }
                out.push('\n');
            }

            // Union the def file, every importer, and every FTS-body-match
            // into the scan set. BTreeSet gives us dedup plus stable order
            // for reproducible output.
            let mut scan_paths: BTreeSet<String> = BTreeSet::new();
            scan_paths.insert(file.path.clone());
            for (_, importer_file) in importers {
                scan_paths.insert(importer_file.path.clone());
            }
            for path in &fts_fallback_paths {
                scan_paths.insert(path.clone());
            }
            let mut call_sites: Vec<(String, usize)> = Vec::new();
            for scan_path in &scan_paths {
                let calls = self.cached_calls(scan_path);
                for (name, line_no) in calls.iter() {
                    if name == &params.symbol {
                        call_sites.push((scan_path.clone(), *line_no));
                    }
                }
            }
            call_sites.sort();
            if !call_sites.is_empty() {
                out.push_str(&format!(
                    "  Direct call sites ({} — AST-resolved):\n",
                    call_sites.len()
                ));
                let mut last_path = String::new();
                for (path, line_no) in &call_sites {
                    let line = if path == &last_path {
                        format!("        L{line_no}\n")
                    } else {
                        last_path = path.clone();
                        format!("    {path} [L{line_no}]\n")
                    };
                    if budget_exceeded(&mut out, &line, budget) {
                        break;
                    }
                    out.push_str(&line);
                }
                out.push('\n');
            }

            if transitive {
                let mut visited: HashSet<i64> = HashSet::new();
                let mut queue: VecDeque<(i64, u32)> = VecDeque::new();
                let mut by_depth: HashMap<u32, Vec<String>> = HashMap::new();

                if let Some(neighbors) = reverse_graph.get(&file.id) {
                    for &n in neighbors {
                        if visited.insert(n) {
                            queue.push_back((n, 1));
                        }
                    }
                }

                while let Some((current, depth)) = queue.pop_front() {
                    if let Some(path) = file_path_lookup.get(&current) {
                        by_depth.entry(depth).or_default().push(path.clone());
                    }
                    if let Some(neighbors) = reverse_graph.get(&current) {
                        for &n in neighbors {
                            if n != file.id && visited.insert(n) {
                                queue.push_back((n, depth + 1));
                            }
                        }
                    }
                }

                if by_depth.is_empty() {
                    out.push_str("  No transitive dependents.\n\n");
                } else {
                    let total: usize = by_depth.values().map(|v| v.len()).sum();
                    out.push_str(&format!("  Transitive dependents ({total} total):\n"));
                    let mut depths: Vec<u32> = by_depth.keys().copied().collect();
                    depths.sort();
                    let mut truncated = false;
                    'trans: for depth in depths {
                        if let Some(files) = by_depth.get(&depth) {
                            for f in files {
                                let line = format!("    [depth {depth}] {f}\n");
                                if budget_exceeded(&mut out, &line, budget) {
                                    truncated = true;
                                    break 'trans;
                                }
                                out.push_str(&line);
                            }
                        }
                    }
                    if !truncated {
                        out.push('\n');
                    }
                }
            }
        }

        Ok(out)
    }
}
