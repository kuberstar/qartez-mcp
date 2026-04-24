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
        // Default true preserves back-compat for callers that do not
        // know about the filter. Hub symbols whose test-only usage
        // dominates production call sites (e.g. `new` in server/mod.rs
        // with 200+ refs from tests/tools.rs) can flip the flag off to
        // focus on production usages.
        let include_tests = params.include_tests.unwrap_or(true);

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

        // Track candidate count once so we can render a header and a
        // "N candidate(s) truncated" footer. Without this, `qartez_refs
        // new` (7 function candidates in the index) silently burned the
        // whole token budget inside the second candidate and the caller
        // saw no indication that 5 more existed.
        let total_candidates = refs.len();
        if !concise && total_candidates > 1 {
            out.push_str(&format!(
                "# {} matches name '{}'. Pass `kind` / `file_path` (on refactor tools) to narrow. Reporting each candidate in order.\n\n",
                total_candidates, params.symbol,
            ));
        }

        for (candidates_emitted, (sym, file, importers)) in refs.iter().enumerate() {
            // Drop self-references only (a symbol whose `from_symbol_id`
            // equals the defining `sym.id`, i.e. `fn f() { f() }`) so the
            // `Direct references` list is not padded with the symbol
            // listing itself. Intra-file references from a DIFFERENT
            // symbol in the same file (e.g. a `pub(super)` helper called
            // through `.map(helper)`) are kept - they are legitimate
            // usages and previously disappeared behind a blanket
            // `importer.file == def.file` filter.
            //
            // When `include_tests=false`, also drop importers whose
            // file path is classified as a test path. This is the same
            // predicate `qartez_calls` uses so both tools report a
            // consistent production-only view of a hub symbol.
            let external_importers: Vec<&(
                crate::storage::models::EdgeRow,
                crate::storage::models::FileRow,
                i64,
            )> = importers
                .iter()
                .filter(|(_, _, from_sym)| *from_sym != sym.id)
                .filter(|(_, f, _)| include_tests || !helpers::is_test_path(&f.path))
                .collect();

            // Over-budget: stop before emitting the next candidate block
            // so `qartez_refs new` (7 fn candidates) signals how many
            // were held back instead of silently truncating mid-symbol.
            if !concise && estimate_tokens(&out) > budget {
                let remaining = total_candidates - candidates_emitted;
                out.push_str(&format!(
                    "\n... {remaining} candidate(s) truncated by token_budget. Pass `kind` / `file_path` to narrow, or raise `token_budget=`.\n",
                ));
                break;
            }

            if concise {
                // Collapse duplicate file paths: a single importer file
                // often supplies multiple edges (separate test fns, N
                // call sites sharing one `use` import). Before this
                // fix, concise mode printed the same path once per
                // edge (e.g. 6x the defining file of LanguageSupport),
                // wasting the caller's token budget on noise. Each
                // path is now emitted once with a trailing `xN` count
                // when N > 1, matching the detailed-mode convention.
                let mut counts: std::collections::BTreeMap<&str, usize> =
                    std::collections::BTreeMap::new();
                for (_, f, _) in &external_importers {
                    *counts.entry(f.path.as_str()).or_insert(0) += 1;
                }
                let rendered: Vec<String> = counts
                    .iter()
                    .map(|(path, count)| {
                        if *count > 1 {
                            format!("{path} x{count}")
                        } else {
                            (*path).to_string()
                        }
                    })
                    .collect();
                out.push_str(&format!(
                    "{} ({}) in {} - {} ref(s): {}\n",
                    sym.name,
                    sym.kind,
                    file.path,
                    external_importers.len(),
                    if rendered.is_empty() {
                        "none".to_string()
                    } else {
                        rendered.join(", ")
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
                // Dedup by file path so detailed mode does not print
                // the same importer ~50x when N distinct specifiers or
                // N call sites resolve to the same file. Before this
                // collapse, a hub symbol referenced in a test module
                // through many named imports emitted one line per
                // import; callers wasted their token budget reading
                // the same path over and over. Group by path, keep
                // the set of distinct specifiers and kinds, and emit
                // one line per file with `xN` when N > 1.
                let mut by_path: std::collections::BTreeMap<
                    String,
                    (
                        std::collections::BTreeSet<String>,
                        std::collections::BTreeSet<String>,
                        usize,
                    ),
                > = std::collections::BTreeMap::new();
                for (edge, importer_file, _from_sym) in &external_importers {
                    let entry = by_path.entry(importer_file.path.clone()).or_insert((
                        std::collections::BTreeSet::new(),
                        std::collections::BTreeSet::new(),
                        0,
                    ));
                    if let Some(s) = edge.specifier.as_deref()
                        && !s.is_empty()
                    {
                        entry.0.insert(s.to_string());
                    }
                    entry.1.insert(edge.kind.clone());
                    entry.2 += 1;
                }

                out.push_str(&format!(
                    "  Direct references ({} importer(s), {} total ref(s)):\n",
                    by_path.len(),
                    external_importers.len(),
                ));
                for (path, (specs, kinds, count)) in &by_path {
                    let count_tag = if *count > 1 {
                        format!(" x{count}")
                    } else {
                        String::new()
                    };
                    let kind_label = kinds.iter().cloned().collect::<Vec<_>>().join(",");
                    let line = if specs.is_empty() {
                        format!("    {path} ({kind_label}){count_tag}\n")
                    } else {
                        let specs_joined = specs.iter().cloned().collect::<Vec<_>>().join(", ");
                        format!(
                            "    {path} - imports via '{specs_joined}' ({kind_label}){count_tag}\n",
                        )
                    };
                    if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                        out.push_str("    ... (truncated by token budget)\n");
                        break;
                    }
                    out.push_str(&line);
                }
                out.push('\n');
            }

            // For trait / interface / class symbols, also surface rows
            // from `type_hierarchy`: an `impl LanguageSupport for Foo`
            // block does NOT emit a `symbol_refs` edge to the trait
            // definition, so the naive ref list would miss 37 of 37
            // implementing files. The per-symbol refactor tools
            // (qartez_rename, qartez_safe_delete) still run off
            // `symbol_refs`, so this report section is a visibility
            // boost for the caller - rewrite/delete must still scan
            // the impl sites manually.
            if matches!(sym.kind.as_str(), "trait" | "interface" | "class") {
                let impl_files: Vec<String> = {
                    let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
                    let mut stmt = match conn.prepare_cached(
                        "SELECT DISTINCT f.path
                         FROM type_hierarchy h
                         JOIN files f ON f.id = h.file_id
                         WHERE h.super_name = ?1
                         ORDER BY f.path",
                    ) {
                        Ok(s) => s,
                        Err(_) => {
                            out.push('\n');
                            continue;
                        }
                    };
                    let rows = stmt
                        .query_map([&sym.name], |row| row.get::<_, String>(0))
                        .map_err(|e| format!("DB error: {e}"))?;
                    rows.flatten().collect()
                };

                if !impl_files.is_empty() {
                    out.push_str(&format!(
                        "  Trait implementations ({} file(s)):\n",
                        impl_files.len(),
                    ));
                    for p in &impl_files {
                        let line = format!("    {p}\n");
                        if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                            out.push_str("    ... (truncated by token budget)\n");
                            break;
                        }
                        out.push_str(&line);
                    }
                    out.push('\n');
                }
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
            //
            // `include_tests=false` drops test-path importers from this
            // scan set as well so the AST-resolved call sites section
            // stays consistent with the Direct-references list above.
            let mut scan_paths: BTreeSet<String> = BTreeSet::new();
            if include_tests || !helpers::is_test_path(&file.path) {
                scan_paths.insert(file.path.clone());
            }
            for (_, importer_file, _from_sym) in importers {
                if !include_tests && helpers::is_test_path(&importer_file.path) {
                    continue;
                }
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
                    .filter(|(_, f, _)| include_tests || !helpers::is_test_path(&f.path))
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
