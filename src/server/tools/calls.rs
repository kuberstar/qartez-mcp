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

/// Hard ceiling on traversal depth. Hub-function call graphs can explode
/// combinatorially, so even with an explicit user request the walker will
/// never recurse more than this many levels.
const MAX_CALL_DEPTH: usize = 10;
/// Default per-section row cap when the caller does not pass `limit`.
const DEFAULT_LIMIT: usize = 50;
/// Default output token budget when the caller does not pass `token_budget`.
const DEFAULT_TOKEN_BUDGET: usize = 4000;

/// One call site extracted from a caller body with its full prefix path
/// (e.g. `Foo::new`, `self.method`, `obj.method`). The prefix is used to
/// disambiguate same-named callees that belong to different types.
#[derive(Clone)]
struct CallSite {
    name: String,
    line: usize,
    /// The left-hand qualifier before the final call name, if any. For
    /// `Foo::bar()` this is `Some("Foo")`; for `self.bar()` this is
    /// `Some("self")`; for bare `bar()` this is `None`.
    qualifier: Option<String>,
    /// True when the call uses `receiver.method(...)` syntax. Callees
    /// of this shape cannot safely bind to cross-file free functions
    /// that are not reachable through the caller's imports - those are
    /// almost always same-named unrelated symbols (e.g. a field named
    /// `filter` colliding with `Iterator::filter`).
    via_method_syntax: bool,
}

#[tool_router(router = qartez_calls_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_calls",
        description = "Show call hierarchy for a function: who calls it (callers) and what it calls (callees). Uses tree-sitter AST analysis. Distinguishes actual calls from type annotations, unlike qartez_refs.",
        annotations(
            title = "Call Hierarchy",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_calls(
        &self,
        Parameters(params): Parameters<SoulCallsParams>,
    ) -> Result<String, String> {
        let concise = is_concise(&params.format);
        let direction = params.direction.unwrap_or_default();
        let want_callers = matches!(direction, CallDirection::Callers | CallDirection::Both);
        let want_callees = matches!(direction, CallDirection::Callees | CallDirection::Both);
        // Depth=1 is the default after the 2026-04 compaction: depth=2 can
        // explode on hub functions, so callers opt in explicitly. Clamp to
        // MAX_CALL_DEPTH so pathological requests do not recurse forever.
        let requested_depth = params.depth.unwrap_or(1) as usize;
        let max_depth = requested_depth.clamp(1, MAX_CALL_DEPTH);
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT as u32) as usize;
        let token_budget = params.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET as u32) as usize;
        let include_tests = params.include_tests.unwrap_or(false);

        // Lock 1: resolve the target symbol and fetch the file list.
        let (symbols, all_files) = {
            let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
            let symbols = read::find_symbol_by_name(&conn, &params.name)
                .map_err(|e| format!("DB error: {e}"))?;
            let all_files = if want_callers {
                read::get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?
            } else {
                Vec::new()
            };
            (symbols, all_files)
        };

        if symbols.is_empty() {
            return Err(format!("No symbol '{}' found in index", params.name));
        }

        let func_symbols: Vec<_> = symbols
            .iter()
            .filter(|(s, _)| matches!(s.kind.as_str(), "function" | "method" | "constructor"))
            .collect();

        if func_symbols.is_empty() {
            return Err(format!(
                "'{}' exists but is not a function/method",
                params.name
            ));
        }

        if is_mermaid(&params.format) {
            return self.qartez_calls_mermaid(
                &params.name,
                &func_symbols,
                &all_files,
                want_callers,
                want_callees,
            );
        }

        let mut out = String::new();
        // Per-invocation caches. Both sets overlap heavily inside a single
        // tool call, so memoizing avoids re-running SQL.
        let mut resolve_cache: HashMap<
            String,
            Vec<(
                crate::storage::models::SymbolRow,
                crate::storage::models::FileRow,
            )>,
        > = HashMap::new();
        let mut file_syms_cache: HashMap<i64, Vec<crate::storage::models::SymbolRow>> =
            HashMap::new();

        for (sym, def_file) in &func_symbols {
            out.push_str(&format!(
                "{} ({}) @ {}:L{}-{}\n",
                sym.name, sym.kind, def_file.path, sym.line_start, sym.line_end,
            ));

            if want_callers {
                self.append_callers(
                    &params.name,
                    &all_files,
                    &mut file_syms_cache,
                    &mut out,
                    concise,
                    limit,
                    token_budget,
                    include_tests,
                )?;
            }

            if want_callees {
                self.append_callees(
                    sym,
                    def_file,
                    &mut resolve_cache,
                    &mut out,
                    concise,
                    limit,
                    token_budget,
                )?;
            }

            if max_depth > 1 && want_callees {
                self.append_deep_callees(
                    sym,
                    def_file,
                    &mut resolve_cache,
                    &mut out,
                    max_depth,
                    limit,
                    token_budget,
                )?;
            }
        }

        Ok(out)
    }
}

impl QartezServer {
    #[allow(clippy::too_many_arguments)]
    fn append_callers(
        &self,
        name: &str,
        all_files: &[crate::storage::models::FileRow],
        file_syms_cache: &mut HashMap<i64, Vec<crate::storage::models::SymbolRow>>,
        out: &mut String,
        concise: bool,
        limit: usize,
        token_budget: usize,
        include_tests: bool,
    ) -> Result<(), String> {
        // Scan phase (no lock): FS reads + tree-sitter parsing for every
        // file. This is the heaviest phase and must not hold the db mutex.
        let mut raw_sites: Vec<(i64, String, Vec<usize>)> = Vec::new();
        for file in all_files {
            if !include_tests && helpers::is_test_path(&file.path) {
                continue;
            }
            let source = match self.cached_source(&file.path) {
                Some(s) => s,
                None => continue,
            };
            if !source.contains(name) {
                continue;
            }
            let calls = self.cached_calls(&file.path);
            let matching: Vec<usize> = calls
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, l)| *l)
                .collect();
            if !matching.is_empty() {
                raw_sites.push((file.id, file.path.clone(), matching));
            }
        }

        // Resolve phase (lock 2): fetch per-file symbol lists to find the
        // enclosing function for each call site. Also augment with
        // `symbol_refs` edges so callers that arrive through non-call
        // syntax (e.g. `.map(helper)` passes `helper` as a callback, not
        // a call_expression) are still surfaced.
        let mut sites: Vec<(String, usize, Option<String>)> = Vec::new();
        {
            let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
            for (file_id, file_path, matching) in &raw_sites {
                let file_syms = file_syms_cache.entry(*file_id).or_insert_with(|| {
                    read::get_symbols_for_file(&conn, *file_id).unwrap_or_default()
                });
                for line in matching {
                    let enclosing = file_syms
                        .iter()
                        .filter(|s| {
                            s.line_start as usize <= *line
                                && *line <= s.line_end as usize
                                && matches!(s.kind.as_str(), "function" | "method" | "constructor")
                        })
                        .max_by_key(|s| s.line_start)
                        .map(|s| s.name.clone());
                    sites.push((file_path.clone(), *line, enclosing));
                }
            }

            // Reverse lookup through `symbol_refs`: any symbol whose body
            // records an edge to a same-named target is treated as a
            // caller. This catches callback-style usages
            // (`.map(helper)`) that never produce a call_expression for
            // `helper` and therefore do not surface through the
            // cached_calls-based text scan above. Merged with the text-
            // scan sites and deduplicated by `(path, line_start)` so we
            // do not double-count syntactic calls.
            let refs =
                read::get_symbol_references(&conn, name).map_err(|e| format!("DB error: {e}"))?;
            let mut existing: std::collections::HashSet<(String, usize)> =
                sites.iter().map(|(p, l, _)| (p.clone(), *l)).collect();
            for (target_sym, _target_file, importers) in &refs {
                for (_, importer_file, from_symbol_id) in importers {
                    if *from_symbol_id == target_sym.id {
                        continue;
                    }
                    if !include_tests && helpers::is_test_path(&importer_file.path) {
                        continue;
                    }
                    let caller_syms =
                        file_syms_cache.entry(importer_file.id).or_insert_with(|| {
                            read::get_symbols_for_file(&conn, importer_file.id).unwrap_or_default()
                        });
                    if let Some(caller_sym) = caller_syms.iter().find(|s| s.id == *from_symbol_id) {
                        let line = caller_sym.line_start as usize;
                        if existing.insert((importer_file.path.clone(), line)) {
                            sites.push((
                                importer_file.path.clone(),
                                line,
                                Some(caller_sym.name.clone()),
                            ));
                        }
                    }
                }
            }
        }

        if sites.is_empty() {
            out.push_str("callers: none\n");
        } else {
            out.push_str(&format!("callers: {}\n", sites.len()));
            if !concise {
                let shown = sites.len().min(limit);
                for (idx, (path, line, encl)) in sites.iter().take(shown).enumerate() {
                    let row = match encl {
                        Some(fn_name) => format!("  {fn_name} @ {path}:L{line}\n"),
                        None => format!("  (top) @ {path}:L{line}\n"),
                    };
                    if estimate_tokens(out) + estimate_tokens(&row) > token_budget {
                        let remaining = sites.len() - idx;
                        out.push_str(&format!("  ... +{remaining} more, raise token_budget=\n"));
                        return Ok(());
                    }
                    out.push_str(&row);
                }
                if sites.len() > shown {
                    let remaining = sites.len() - shown;
                    out.push_str(&format!("  ... +{remaining} more, raise limit=\n"));
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn append_callees(
        &self,
        sym: &crate::storage::models::SymbolRow,
        def_file: &crate::storage::models::FileRow,
        resolve_cache: &mut HashMap<
            String,
            Vec<(
                crate::storage::models::SymbolRow,
                crate::storage::models::FileRow,
            )>,
        >,
        out: &mut String,
        concise: bool,
        limit: usize,
        token_budget: usize,
    ) -> Result<(), String> {
        let start = sym.line_start as usize;
        let end = sym.line_end as usize;
        let sites = self.collect_call_sites_with_qualifiers(&def_file.path, start, end);
        // Dedup by (name, qualifier, via_method_syntax); keep the first-seen
        // line per pair so `Foo::new` and `Bar::new` stay distinct. Tracking
        // via_method_syntax lets the renderer drop cross-file unrelated
        // candidates for `.filter()`-style method calls.
        let mut seen_order: Vec<(String, Option<String>, bool)> = Vec::new();
        let mut first_line: HashMap<(String, Option<String>, bool), usize> = HashMap::new();
        for site in &sites {
            let key = (
                site.name.clone(),
                site.qualifier.clone(),
                site.via_method_syntax,
            );
            if !first_line.contains_key(&key) {
                first_line.insert(key.clone(), site.line);
                seen_order.push(key);
            }
        }

        if seen_order.is_empty() {
            out.push_str("callees: none\n");
            return Ok(());
        }

        out.push_str(&format!("callees: {}\n", seen_order.len()));
        if concise {
            return Ok(());
        }

        {
            let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
            for (callee_name, _, _) in &seen_order {
                resolve_cache.entry(callee_name.clone()).or_insert_with(|| {
                    read::find_symbol_by_name(&conn, callee_name).unwrap_or_default()
                });
            }
        }

        let shown = seen_order.len().min(limit);
        for (idx, (callee_name, qualifier, via_method)) in seen_order.iter().take(shown).enumerate()
        {
            let resolved = resolve_cache.get(callee_name).unwrap();
            let row = render_callee_row(
                callee_name,
                qualifier.as_deref(),
                resolved,
                *via_method,
                &def_file.path,
            );
            if estimate_tokens(out) + estimate_tokens(&row) > token_budget {
                let remaining = seen_order.len() - idx;
                out.push_str(&format!("  ... +{remaining} more, raise token_budget=\n"));
                return Ok(());
            }
            out.push_str(&row);
        }
        if seen_order.len() > shown {
            let remaining = seen_order.len() - shown;
            out.push_str(&format!("  ... +{remaining} more, raise limit=\n"));
        }
        Ok(())
    }

    /// Walk callees up to `max_depth` BFS levels. Replaces the old
    /// `append_depth2`: depth=3+ used to be silently clamped to depth 2
    /// because only a single extra level was ever walked.
    #[allow(clippy::too_many_arguments)]
    fn append_deep_callees(
        &self,
        sym: &crate::storage::models::SymbolRow,
        def_file: &crate::storage::models::FileRow,
        resolve_cache: &mut HashMap<
            String,
            Vec<(
                crate::storage::models::SymbolRow,
                crate::storage::models::FileRow,
            )>,
        >,
        out: &mut String,
        max_depth: usize,
        limit: usize,
        token_budget: usize,
    ) -> Result<(), String> {
        let start = sym.line_start as usize;
        let end = sym.line_end as usize;
        let root_sites = self.collect_call_sites_with_qualifiers(&def_file.path, start, end);

        // BFS frontier: each entry is (callee_name, parent_chain) at a
        // given depth. `parent_chain` is the `A -> B -> C` string used to
        // render the path in the output.
        //
        // A global `visited` set seeded with the root symbol and every
        // direct callee guards against cycles and keeps hub blow-up in
        // check: reaching X from two distinct roots prints it only under
        // the first one. Same invariant as the pre-fix `depth2` walker,
        // just generalized to N levels.
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(sym.name.clone());
        let mut direct: Vec<String> = Vec::new();
        {
            let mut seen_direct = HashSet::new();
            for site in &root_sites {
                if seen_direct.insert(site.name.clone()) {
                    direct.push(site.name.clone());
                }
            }
        }
        for d in &direct {
            visited.insert(d.clone());
        }

        // Depth-keyed output buckets. Level 2 is "targets reached via a
        // direct callee", level 3 via a depth-2 callee, and so on.
        let mut by_depth: HashMap<usize, Vec<(String, String)>> = HashMap::new();

        let mut frontier: Vec<(String, String)> =
            direct.iter().map(|n| (n.clone(), n.clone())).collect();
        for depth in 2..=max_depth {
            if frontier.is_empty() {
                break;
            }
            let mut next_frontier: Vec<(String, String)> = Vec::new();
            {
                let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
                for (name, _) in &frontier {
                    resolve_cache.entry(name.clone()).or_insert_with(|| {
                        read::find_symbol_by_name(&conn, name).unwrap_or_default()
                    });
                }
            }
            for (name, chain) in &frontier {
                let resolved = resolve_cache.get(name).cloned().unwrap_or_default();
                for (s2, f2) in resolved.iter() {
                    if !matches!(s2.kind.as_str(), "function" | "method") {
                        continue;
                    }
                    let s2start = s2.line_start as usize;
                    let s2end = s2.line_end as usize;
                    let child_sites =
                        self.collect_call_sites_with_qualifiers(&f2.path, s2start, s2end);
                    for child in &child_sites {
                        if visited.insert(child.name.clone()) {
                            let new_chain = format!("{chain} -> {}", child.name);
                            by_depth
                                .entry(depth)
                                .or_default()
                                .push((child.name.clone(), new_chain.clone()));
                            next_frontier.push((child.name.clone(), new_chain));
                        }
                    }
                }
            }
            frontier = next_frontier;
        }

        if by_depth.is_empty() {
            out.push_str("deeper: none\n");
            return Ok(());
        }

        out.push_str("deeper:\n");
        let mut depths: Vec<usize> = by_depth.keys().copied().collect();
        depths.sort();
        let total_available: usize = by_depth.values().map(|v| v.len()).sum();
        let mut total_emitted = 0usize;
        'outer: for depth in depths {
            if let Some(entries) = by_depth.get(&depth) {
                for (_, chain) in entries {
                    if total_emitted >= limit {
                        break 'outer;
                    }
                    let row = format!("  [depth {depth}] {chain}\n");
                    if estimate_tokens(out) + estimate_tokens(&row) > token_budget {
                        let remaining = total_available - total_emitted;
                        out.push_str(&format!("  ... +{remaining} more, raise token_budget=\n"));
                        return Ok(());
                    }
                    out.push_str(&row);
                    total_emitted += 1;
                }
            }
        }
        if total_emitted < total_available {
            let over = total_available - total_emitted;
            out.push_str(&format!("  ... +{over} more, raise limit=\n"));
        }
        Ok(())
    }

    /// Walk the cached AST of `file_path`, collecting every call inside
    /// the inclusive line range `[start, end]` together with the prefix
    /// path before the call name. The prefix is used by
    /// `append_callees` to distinguish `Foo::new` from `Bar::new` when
    /// resolving a same-named candidate set.
    fn collect_call_sites_with_qualifiers(
        &self,
        file_path: &str,
        start: usize,
        end: usize,
    ) -> Vec<CallSite> {
        let Some((source_arc, tree_arc)) = self.cached_tree(file_path) else {
            return self
                .cached_calls(file_path)
                .iter()
                .filter(|(_, l)| *l >= start && *l <= end)
                .map(|(n, l)| CallSite {
                    name: n.clone(),
                    line: *l,
                    qualifier: None,
                    via_method_syntax: false,
                })
                .collect();
        };
        let source_bytes = source_arc.as_bytes();
        let mut results: Vec<CallSite> = Vec::new();
        collect_call_sites_with_qualifiers_tree(
            &mut tree_arc.walk(),
            source_bytes,
            &mut results,
            start,
            end,
        );
        results
    }
}

/// Render one callee row, disambiguating same-named candidates.
///
/// - Zero matches: emit `<name> (extern)`.
/// - Single match: emit `<name> @ <file>`.
/// - Multiple matches with a qualifier that matches `owner_type` on exactly
///   one candidate: pick that candidate.
/// - Multiple matches, no usable qualifier or no unique owner-type hit:
///   emit every candidate prefixed with `ambiguous (N candidates)`.
fn render_callee_row(
    callee_name: &str,
    qualifier: Option<&str>,
    resolved: &[(
        crate::storage::models::SymbolRow,
        crate::storage::models::FileRow,
    )],
    via_method_syntax: bool,
    caller_file_path: &str,
) -> String {
    let func_like: Vec<&(
        crate::storage::models::SymbolRow,
        crate::storage::models::FileRow,
    )> = resolved
        .iter()
        .filter(|(s, _)| matches!(s.kind.as_str(), "function" | "method" | "constructor"))
        .collect();
    if func_like.is_empty() {
        // A method-syntax call (`.filter(...)`) whose only same-named
        // candidates are fields, variables, or constants in an unrelated
        // file is almost always the stdlib (Iterator::filter) colliding
        // with a user symbol. Report as extern rather than binding to
        // the spurious match.
        if via_method_syntax
            && let Some((_, f)) = resolved.first()
            && f.path != caller_file_path
        {
            return format!("  {callee_name} (extern)\n");
        }
        if let Some((_, f)) = resolved.first() {
            return format!("  {callee_name} @ {}\n", f.path);
        }
        return format!("  {callee_name} (extern)\n");
    }
    if func_like.len() == 1 {
        let (_, f) = func_like[0];
        // Same protection for a single cross-file func-like match: a
        // method-syntax call should not silently bind to a free fn that
        // is not reachable from the caller's file.
        if via_method_syntax && f.path != caller_file_path {
            return format!("  {callee_name} (extern)\n");
        }
        return format!("  {callee_name} @ {}\n", f.path);
    }
    if let Some(qual) = qualifier
        && qual != "self"
        && qual != "Self"
    {
        let matching: Vec<&(
            crate::storage::models::SymbolRow,
            crate::storage::models::FileRow,
        )> = func_like
            .iter()
            .copied()
            .filter(|(s, _)| s.owner_type.as_deref() == Some(qual))
            .collect();
        if matching.len() == 1 {
            let (_, f) = matching[0];
            return format!("  {qual}::{callee_name} @ {}\n", f.path);
        }
    }
    let mut buf = format!(
        "  {callee_name} ambiguous ({} candidates)\n",
        func_like.len()
    );
    for (s, f) in &func_like {
        let label = match s.owner_type.as_deref() {
            Some(t) => format!("{t}::{callee_name}"),
            None => callee_name.to_string(),
        };
        buf.push_str(&format!("    - {label} @ {}\n", f.path));
    }
    buf
}

/// Tree-walking version of [`collect_call_names`] that also records the
/// left-hand qualifier of every call. Only emits sites whose source line
/// falls inside `[start, end]` so callers can scope to one function body.
fn collect_call_sites_with_qualifiers_tree(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    results: &mut Vec<CallSite>,
    start: usize,
    end: usize,
) {
    let mut reached_root = false;
    while !reached_root {
        let node = cursor.node();
        if CALL_NODE_KINDS.contains(&node.kind()) {
            let line = node.start_position().row + 1;
            if line >= start && line <= end {
                let mut found = None;
                for field in CALLEE_FIELD_NAMES {
                    if let Some(callee) = node.child_by_field_name(field) {
                        let via_method = matches!(
                            callee.kind(),
                            "field_expression" | "member_expression" | "attribute"
                        );
                        let (name, qual) = split_callee(callee, source);
                        if !name.is_empty() {
                            found = Some(CallSite {
                                name,
                                line,
                                qualifier: qual,
                                via_method_syntax: via_method,
                            });
                        }
                        break;
                    }
                }
                if found.is_none()
                    && let Some(first_child) = node.child(0)
                {
                    let via_method = matches!(
                        first_child.kind(),
                        "field_expression" | "member_expression" | "attribute"
                    );
                    let (name, qual) = split_callee(first_child, source);
                    if !name.is_empty() {
                        found = Some(CallSite {
                            name,
                            line,
                            qualifier: qual,
                            via_method_syntax: via_method,
                        });
                    }
                }
                if let Some(site) = found {
                    results.push(site);
                }
            }
        }

        if cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                reached_root = true;
                break;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Decompose a callee AST node into `(name, qualifier)`. For a
/// field/scoped expression the qualifier is the text left of the final
/// `.` / `::`, with a best-effort fallback to the first identifier
/// child when no `object`/`value`/`scope` field is exposed by the grammar.
fn split_callee(node: tree_sitter::Node, source: &[u8]) -> (String, Option<String>) {
    match node.kind() {
        "identifier" | "simple_identifier" | "property_identifier" => {
            (node.utf8_text(source).unwrap_or("").to_string(), None)
        }
        "field_expression" | "member_expression" | "scoped_identifier" | "attribute" => {
            let name = node
                .child_by_field_name("field")
                .or_else(|| node.child_by_field_name("property"))
                .or_else(|| node.child_by_field_name("name"))
                .map(|f| f.utf8_text(source).unwrap_or("").to_string())
                .unwrap_or_else(|| {
                    let count = node.child_count();
                    if count == 0 {
                        return String::new();
                    }
                    node.child((count - 1) as u32)
                        .and_then(|n| n.utf8_text(source).ok())
                        .unwrap_or("")
                        .to_string()
                });
            let qual = node
                .child_by_field_name("object")
                .or_else(|| node.child_by_field_name("value"))
                .or_else(|| node.child_by_field_name("scope"))
                .or_else(|| node.child_by_field_name("path"))
                .or_else(|| node.child(0))
                .and_then(|n| n.utf8_text(source).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && s != &name);
            (name, qual)
        }
        _ => (node.utf8_text(source).unwrap_or("").to_string(), None),
    }
}

impl QartezServer {
    /// Render call hierarchy as a Mermaid flowchart.
    fn qartez_calls_mermaid(
        &self,
        target_name: &str,
        func_symbols: &[&(
            crate::storage::models::SymbolRow,
            crate::storage::models::FileRow,
        )],
        all_files: &[crate::storage::models::FileRow],
        want_callers: bool,
        want_callees: bool,
    ) -> Result<String, String> {
        let max_nodes = 50;
        let mut out = String::from("graph TD\n");
        let target_id = helpers::mermaid_node_id(target_name);
        let target_label = helpers::mermaid_label(target_name);
        out.push_str(&format!("  {target_id}[\"{target_label}\"]\n"));

        let mut count = 0usize;
        let mut seen_edges = HashSet::new();

        for (sym, def_file) in func_symbols {
            if want_callers {
                for file in all_files {
                    if count >= max_nodes {
                        break;
                    }
                    let source = match self.cached_source(&file.path) {
                        Some(s) => s,
                        None => continue,
                    };
                    if !source.contains(target_name) {
                        continue;
                    }
                    let calls = self.cached_calls(&file.path);
                    let has_call = calls.iter().any(|(name, _)| name == target_name);
                    if !has_call {
                        continue;
                    }
                    let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
                    let file_syms = read::get_symbols_for_file(&conn, file.id).unwrap_or_default();
                    drop(conn);
                    let matching_lines: Vec<usize> = calls
                        .iter()
                        .filter(|(name, _)| name == target_name)
                        .map(|(_, l)| *l)
                        .collect();
                    for line in &matching_lines {
                        if count >= max_nodes {
                            break;
                        }
                        let enclosing = file_syms
                            .iter()
                            .filter(|s| {
                                s.line_start as usize <= *line
                                    && *line <= s.line_end as usize
                                    && matches!(
                                        s.kind.as_str(),
                                        "function" | "method" | "constructor"
                                    )
                            })
                            .max_by_key(|s| s.line_start)
                            .map(|s| s.name.clone());
                        let caller = enclosing.as_deref().unwrap_or("(top-level)");
                        let cid = helpers::mermaid_node_id(caller);
                        let edge_key = format!("{cid}-->{target_id}");
                        if !seen_edges.insert(edge_key) {
                            continue;
                        }
                        let clabel = helpers::mermaid_label(caller);
                        out.push_str(&format!("  {cid}[\"{clabel}\"] --> {target_id}\n"));
                        count += 1;
                    }
                }
            }

            if want_callees {
                let all_calls = self.cached_calls(&def_file.path);
                let start = sym.line_start as usize;
                let end = sym.line_end as usize;
                let mut seen = HashSet::new();
                for (name, line) in all_calls.iter() {
                    if count >= max_nodes {
                        break;
                    }
                    if *line < start || *line > end {
                        continue;
                    }
                    if !seen.insert(name.clone()) {
                        continue;
                    }
                    // Count function-like candidates for this callee
                    // so the mermaid edge can surface ambiguous
                    // resolutions. Text output prints
                    // `ambiguous (N candidates)` plus a listing; the
                    // mermaid path used to collapse every candidate
                    // into one solid edge, hiding the fact that the
                    // resolver could not pick a unique target. A
                    // dashed edge (`-.->`) with a `|?N|` label echoes
                    // the same signal in graph form.
                    let candidates = {
                        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
                        read::find_symbol_by_name(&conn, name).unwrap_or_default()
                    };
                    let func_like = candidates
                        .iter()
                        .filter(|(s, _)| {
                            matches!(s.kind.as_str(), "function" | "method" | "constructor")
                        })
                        .count();
                    let cid = helpers::mermaid_node_id(name);
                    let clabel = helpers::mermaid_label(name);
                    if func_like >= 2 {
                        out.push_str(&format!(
                            "  {target_id} -.->|?{func_like}| {cid}[\"{clabel}\"]\n",
                        ));
                    } else {
                        out.push_str(&format!("  {target_id} --> {cid}[\"{clabel}\"]\n"));
                    }
                    count += 1;
                }
            }
        }

        if count >= max_nodes {
            out.push_str("  truncated[\"... truncated\"]\n");
        }
        Ok(out)
    }
}
