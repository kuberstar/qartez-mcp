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
        reject_mermaid(&params.format, "qartez_refs")?;
        let budget = params.token_budget.unwrap_or(4000) as usize;
        let concise = is_concise(&params.format);
        let transitive = params.transitive.unwrap_or(false);

        // All DB queries under one lock acquisition; the lock is dropped
        // before the tree-sitter / FS phase (cached_calls) so the watcher
        // and other handlers are not blocked during parsing.
        let (refs, reverse_graph, file_path_lookup) = {
            let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
            let refs = read::get_symbol_references(&conn, &params.symbol)
                .map_err(|e| format!("DB error: {e}"))?;
            if refs.is_empty() {
                return Ok(format!("No symbol found with name '{}'", params.symbol));
            }

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

            (refs, reverse_graph, file_path_lookup)
        };

        let mut out = String::new();

        for (sym, file, importers) in &refs {
            // Drop self-references only (a symbol whose `from_symbol_id`
            // equals the defining `sym.id`, i.e. `fn f() { f() }`) so the
            // `Direct references` list is not padded with the symbol
            // listing itself. Intra-file references from a DIFFERENT
            // symbol in the same file (e.g. a `pub(super)` helper called
            // through `.map(helper)`) are kept - they are legitimate
            // usages and previously disappeared behind a blanket
            // `importer.file == def.file` filter.
            let external_importers: Vec<&(
                crate::storage::models::EdgeRow,
                crate::storage::models::FileRow,
                i64,
            )> = importers
                .iter()
                .filter(|(_, _, from_sym)| *from_sym != sym.id)
                .collect();

            if concise {
                let paths: Vec<&str> = external_importers
                    .iter()
                    .map(|(_, f, _)| f.path.as_str())
                    .collect();
                out.push_str(&format!(
                    "{} ({}) in {} - {} ref(s): {}\n",
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

            if external_importers.is_empty() {
                out.push_str("  No direct references found.\n\n");
            } else {
                out.push_str(&format!(
                    "  Direct references ({}):\n",
                    external_importers.len()
                ));
                for (edge, importer_file, _from_sym) in &external_importers {
                    // Suppress the `imports via '...'` clause when the
                    // edge has no specifier: for symbol-level refs the
                    // specifier is routinely absent and the placeholder
                    // `(unspecified)` added one noise line per importer
                    // that carried no real information. When a real
                    // specifier is present (e.g. `use foo::Bar as Baz;`)
                    // it is kept so callers can still see the rename.
                    let line = match edge.specifier.as_deref() {
                        Some(spec) if !spec.is_empty() => format!(
                            "    {} - imports via '{}' ({})\n",
                            importer_file.path, spec, edge.kind,
                        ),
                        _ => format!("    {} ({})\n", importer_file.path, edge.kind),
                    };
                    if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                        out.push_str("    ... (truncated by token budget)\n");
                        break;
                    }
                    out.push_str(&line);
                }
                out.push('\n');
            }

            // Per-symbol scan set: the defining file plus every file whose
            // `symbol_refs` edge was resolved to THIS specific `sym.id`.
            // This is the only correctness-safe scope when the repo has
            // multiple same-named definitions (e.g. several `run` fns
            // across modules): including a blanket FTS-body-match union
            // would attribute every textual call to every same-named
            // symbol. Files that textually mention the name but have no
            // resolved edge to this sym are assumed to target a different
            // same-named sym and are left to that sym's iteration.
            let mut scan_paths: BTreeSet<String> = BTreeSet::new();
            scan_paths.insert(file.path.clone());
            for (_, importer_file, _from_sym) in importers {
                scan_paths.insert(importer_file.path.clone());
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
                    "  Direct call sites ({} - AST-resolved):\n",
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
                    if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                        out.push_str("    ... (truncated by token budget)\n");
                        break;
                    }
                    out.push_str(&line);
                }
                out.push('\n');
            }

            if transitive {
                // Walk per-symbol: seed the BFS from every direct
                // importer file (the files that actually use THIS sym),
                // not from the defining file's dependents. The pre-fix
                // behavior started at `file.id` so every symbol defined
                // in a hub module (e.g. `languages/mod.rs`) inherited
                // the module's full fan-out regardless of which specific
                // symbol was queried. Starting at per-symbol importers
                // keeps the transitive set bounded to files that
                // actually reach the symbol through the edge graph.
                let mut visited: HashSet<i64> = HashSet::new();
                let mut queue: VecDeque<(i64, u32)> = VecDeque::new();
                let mut by_depth: HashMap<u32, Vec<String>> = HashMap::new();

                let seeds: Vec<i64> = importers
                    .iter()
                    .filter(|(_, f, _)| f.id != file.id)
                    .map(|(_, f, _)| f.id)
                    .collect();
                for seed in &seeds {
                    if visited.insert(*seed) {
                        queue.push_back((*seed, 1));
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
                                if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                                    out.push_str("    ... (truncated by token budget)\n");
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
