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

/// Identifier denylist used by the callee renderer. These names are dominated
/// by stdlib / language-builtin methods (`Option::map`, `Result::unwrap`,
/// iterator adapters, common `new` constructors, etc.). When a call resolves
/// to one of them without a concrete owner_type the candidate pool is almost
/// always a noise swarm of same-named user symbols that the resolver cannot
/// disambiguate. Suppressing them keeps the callee listing focused on the
/// user code the caller is actually wiring together. Rows that DO resolve
/// through the owner_type branch still pass because those name a concrete
/// user symbol the caller depends on (e.g. `QartezServer::new` is kept).
const STDLIB_STUBS: &[&str] = &[
    "parse",
    "init",
    "Ok",
    "Err",
    "Some",
    "None",
    "clone",
    "new",
    "into",
    "from",
    "as_ref",
    "as_mut",
    "unwrap",
    "expect",
    "map",
    "map_err",
    "and_then",
    "or_else",
    "ok_or",
    "iter",
    "collect",
    "to_string",
    "to_owned",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "len",
    "is_empty",
    "push",
    "pop",
    "insert",
    "remove",
    "get",
    "contains",
    "starts_with",
    "ends_with",
];

/// Language names (matching `FileRow::language`) that carry call graphs the
/// `qartez_calls` tool can analyse. Shell, config, and markup files frequently
/// contain identifiers that collide with real function names (`main` in a
/// benchmark `.sh`, `handle` in a Dockerfile, etc.); letting them into the
/// candidate pool pollutes the tool output with results that cannot be
/// reached from the user's Rust / JS / Python call graph. Any language not
/// listed here is dropped from the seed-symbol candidate set in
/// `qartez_calls` before the multi-candidate refusal, so the refusal itself
/// never fires on "one real function plus five shell-script noise rows".
const CALLABLE_SOURCE_LANGUAGES: &[&str] = &[
    "rust",
    "typescript",
    "javascript",
    "python",
    "go",
    "java",
    "kotlin",
    "scala",
    "swift",
    "csharp",
    "cpp",
    "c",
    "ruby",
    "php",
    "lua",
    "dart",
    "elixir",
    "haskell",
    "ocaml",
    "zig",
    "r",
];

fn is_callable_source_language(language: &str) -> bool {
    CALLABLE_SOURCE_LANGUAGES
        .iter()
        .any(|l| l.eq_ignore_ascii_case(language))
}

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
        // Depth semantics after the 2026-04-23 fix:
        //   depth=0         -> seed-only mode (resolved header, no graph walk)
        //                      mirrors qartez_hierarchy max_depth=0
        //   depth=1 default -> direct callers + direct callees
        //   depth>=2        -> multi-level BFS, clamped to MAX_CALL_DEPTH
        // Before the fix, depth=0 was silently treated as 1. Now both
        // tools honour the seed-only contract.
        let requested_depth = params.depth.unwrap_or(1) as usize;
        let seed_only = requested_depth == 0;
        let max_depth = if seed_only {
            0
        } else {
            requested_depth.clamp(1, MAX_CALL_DEPTH)
        };
        let depth_was_clamped = requested_depth > MAX_CALL_DEPTH;
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT as u32) as usize;
        let token_budget = params.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET as u32) as usize;
        let include_tests = params.include_tests.unwrap_or(false);
        let kind_filter = params
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let file_filter = params
            .file_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

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
            // Unified wording with qartez_refs / qartez_find / qartez_read
            // so callers can grep the same string across tools.
            return Err(format!("No symbol found with name '{}'", params.name));
        }

        let func_symbols: Vec<_> = symbols
            .iter()
            .filter(|(s, _)| matches!(s.kind.as_str(), "function" | "method" | "constructor"))
            // Drop candidates whose defining file is not a callable source
            // language. Shell scripts, Makefiles, dockerfiles, YAML, etc.
            // can index function-like rows (e.g. `main` in a benchmark
            // `.sh`) that collide with real Rust/JS/Python names. Without
            // this filter the multi-candidate refusal below fires on
            // "one real function plus N shell-noise rows" and the caller
            // is asked to disambiguate against names that were never
            // reachable from their code in the first place.
            .filter(|(_, f)| is_callable_source_language(&f.language))
            // Honour `include_tests=false` on the seed lookup too. The
            // caller's intent is "analyse production code"; resolving
            // `index_project` to a `tests/fp_regression_security_refs.rs`
            // test-only helper violated that contract because the
            // include_tests filter was only applied inside
            // `append_callers`. Dropping test-path definitions here
            // keeps the two axes consistent.
            .filter(|(_, f)| include_tests || !helpers::is_test_path(&f.path))
            // User-supplied disambiguation. Applied before the
            // multi-candidate guard so callers can pick one overload
            // of a shared name (e.g. `new`) without tripping the
            // ambiguity refusal.
            .filter(|(s, _)| match kind_filter {
                Some(k) => s.kind.eq_ignore_ascii_case(k),
                None => true,
            })
            .filter(|(_, f)| match file_filter {
                Some(p) => f.path == p,
                None => true,
            })
            .collect();

        if func_symbols.is_empty() {
            // Give distinct signals for "not a function", "only test
            // definitions exist", "only non-callable language definitions
            // exist (shell `main`, Makefile targets, ...)" and "no
            // candidate matches the user's kind/file_path filter".
            // Without the branches, every empty-set case collapsed into
            // the same message and the caller could not tell which knob
            // to adjust.
            //
            // NOTE: we branch on `symbols` (pre-filter) on purpose. When
            // `name="handle"` resolves only to structs/macros, the outer
            // `if symbols.is_empty()` guard above never fires because
            // SymbolRow rows DO exist - the filter chain just removes
            // all of them. That is the case this block translates into
            // a caller-visible "exists but is not a function" message.
            if kind_filter.is_some() || file_filter.is_some() {
                return Err(format!(
                    "'{}' has no function/method candidate matching kind={:?} file_path={:?}. Drop the filter to see the full candidate list.",
                    params.name, kind_filter, file_filter,
                ));
            }
            if !include_tests
                && symbols.iter().any(|(s, f)| {
                    matches!(s.kind.as_str(), "function" | "method" | "constructor")
                        && helpers::is_test_path(&f.path)
                })
            {
                return Err(format!(
                    "'{}' is only defined in test files. Pass `include_tests=true` to analyse them, or narrow by `file_path`.",
                    params.name,
                ));
            }
            if symbols.iter().any(|(s, f)| {
                matches!(s.kind.as_str(), "function" | "method" | "constructor")
                    && !is_callable_source_language(&f.language)
            }) {
                return Err(format!(
                    "'{}' is only defined in non-callable languages (shell/makefile/config). qartez_calls analyses Rust/JS/Python/Go/Java/... call graphs only.",
                    params.name,
                ));
            }
            // `symbols` is non-empty (outer guard passed) but none are
            // function-like after filtering. This is the "handle exists
            // as struct/macro but not as fn" path the auditor flagged.
            return Err(format!(
                "'{}' exists but is not a function/method",
                params.name
            ));
        }

        // Multi-candidate disambiguation guard. Before this branch
        // landed, `append_callers` counted every textual occurrence of
        // the bare name across the repo (e.g. `callers: 1357` for every
        // `new`), and `render_callee_row` emitted the
        // `ambiguous (N candidates)` block INSIDE the callees listing
        // when the target body re-invoked its own name through a
        // different owner type (HashMap::new in QartezServer::new).
        // Both were incorrect per-candidate: a single identifier-scan
        // cannot attribute call sites to one overload. We now refuse
        // up front and ask the caller to narrow via `file_path` or
        // `kind`.
        if func_symbols.len() > 1 {
            let mut banner = format!(
                "'{}' resolves to {} function-like candidate(s). Pass `file_path` or `kind` to pick one - per-candidate callers/callees counts are not attributable without disambiguation.\n\ncandidates:\n",
                params.name,
                func_symbols.len(),
            );
            for (sym, def_file) in &func_symbols {
                let owner = sym
                    .owner_type
                    .as_deref()
                    .map(|t| format!("{t}::"))
                    .unwrap_or_default();
                banner.push_str(&format!(
                    "  - {owner}{} ({}) @ {}:L{}-{}\n",
                    sym.name, sym.kind, def_file.path, sym.line_start, sym.line_end,
                ));
            }
            return Ok(banner);
        }

        if is_mermaid(&params.format) {
            return self.qartez_calls_mermaid(
                &params.name,
                &func_symbols,
                &all_files,
                want_callers,
                want_callees,
                token_budget,
            );
        }

        let mut out = String::new();
        // Surface the depth clamp up front as a `!warning:` line so a
        // caller skimming the top of the output immediately sees that
        // the graph was capped. The old trailing `note:` was easy to
        // miss when output ran into the limit truncation footer.
        if depth_was_clamped {
            out.push_str(&format!(
                "!warning: depth={requested_depth} was clamped to {MAX_CALL_DEPTH} (server-side hard cap to prevent hub-function blow-up).\n\n",
            ));
        }
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

            // Seed-only mode: depth=0 prints the resolved symbol and
            // stops before any graph walk. Matches qartez_hierarchy
            // max_depth=0 behavior so both tools speak the same
            // "cheap existence probe" vocabulary.
            if seed_only {
                out.push_str(
                    "  (depth=0 seed-only: no callers/callees expanded. Raise depth to walk the graph.)\n",
                );
                continue;
            }

            if want_callers {
                // Hand the resolved symbol's owner_type (e.g.
                // "QartezServer") to the caller scanner so a name like
                // `new` does not sweep up every unrelated `X::new` call
                // across the repo. Without this filter `QartezServer::new`
                // over-counted ~6x (1387 vs ~223) because the text scan
                // matched any `new` token regardless of receiver type.
                self.append_callers(
                    &params.name,
                    sym.owner_type.as_deref(),
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
                    concise,
                )?;
            }
        }

        // Depth-clamp warning is now emitted at the head of `out` above,
        // not here - a trailing note was easy to miss when output hit
        // the token-budget truncation footer.

        Ok(out)
    }
}

impl QartezServer {
    #[allow(clippy::too_many_arguments)]
    fn append_callers(
        &self,
        name: &str,
        owner_filter: Option<&str>,
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
        //
        // When `owner_filter` is set, switch from the bare `cached_calls`
        // (name, line) pool to the qualifier-aware walker so a call to
        // `HashMap::new()` inside a file that also invokes
        // `QartezServer::new()` is not counted as a caller of the latter.
        // Sites whose indexed qualifier is `None` (the AST failed to
        // record a receiver type, e.g. trait-object dynamic dispatch) are
        // kept but counted toward `unqualified_count` so the header can
        // warn the caller that the number is a best-effort upper bound.
        // This is a best-effort heuristic - without a full type-resolver
        // we cannot prove `self.new()` binds to the target's owner_type,
        // so `self`/`Self` qualifiers are also routed through the
        // unqualified bucket rather than silently dropped.
        let mut raw_sites: Vec<(i64, String, Vec<usize>)> = Vec::new();
        let mut unqualified_count: usize = 0;
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
            let matching: Vec<usize> = if let Some(owner) = owner_filter {
                // Qualifier-aware pass: walk the whole file and keep
                // only call sites whose receiver qualifier matches the
                // target's owner_type, plus unqualified/`self` sites
                // that we cannot prove one way or the other.
                let line_count = source.lines().count().max(1);
                let sites = self.collect_call_sites_with_qualifiers(&file.path, 1, line_count);
                let mut lines = Vec::new();
                for site in &sites {
                    if site.name != name {
                        continue;
                    }
                    match site.qualifier.as_deref() {
                        Some(q) if q == owner => lines.push(site.line),
                        Some("self") | Some("Self") => {
                            // `self.new()` / `Self::new()` carry the
                            // caller impl-block's receiver type, which
                            // we cannot resolve without a type checker.
                            // Keep the site but count it as unqualified.
                            unqualified_count += 1;
                            lines.push(site.line);
                        }
                        Some(_) => {
                            // Qualifier that belongs to a different
                            // type (`HashMap::new`, `Vec::new`, ...).
                            // Drop - this is the over-count fix.
                        }
                        None => {
                            // Free-standing `name(...)`: could be a
                            // re-export or a same-named free function.
                            // Preserve the site but flag it.
                            unqualified_count += 1;
                            lines.push(site.line);
                        }
                    }
                }
                lines
            } else {
                let calls = self.cached_calls(&file.path);
                calls
                    .iter()
                    .filter(|(n, _)| n == name)
                    .map(|(_, l)| *l)
                    .collect()
            };
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
            // Owner-filtering is a best-effort heuristic: the tree-sitter
            // walker cannot always recover the receiver type (trait
            // objects, `self.new()` without file-scoped impl resolution,
            // re-exports). Surface the residual when it is non-zero so
            // the caller knows the headline number is an upper bound.
            if owner_filter.is_some() && unqualified_count > 0 {
                out.push_str(&format!(
                    "  (note: {unqualified_count} unqualified site(s) could not be attributed to a concrete owner_type; count may be over-inclusive)\n",
                ));
            }
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
        // Caller's owner_type lets `self.<method>()` / `Self::<method>()`
        // calls disambiguate against the caller's own impl block. Before
        // this, `QartezServer::new()` calling `self.X()` could not bind
        // to `QartezServer::X` because the qualifier was the literal
        // `self` string, which the filter below deliberately skips to
        // avoid false matches on siblings in other impl blocks.
        let caller_owner_type: Option<&str> = sym.owner_type.as_deref();
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

        // Render rows lazily so the denylist inside `render_callee_row`
        // can suppress noise rows (stdlib stubs like `clone`, `map`,
        // `unwrap` that could not be resolved to a concrete owner_type)
        // without burning a `limit` slot on an empty row. `suppressed`
        // surfaces the residual in the output footer so the reader
        // knows the header count included a few builtins the listing
        // below deliberately elides.
        let shown = seen_order.len().min(limit);
        let mut suppressed: usize = 0;
        for (idx, (callee_name, qualifier, via_method)) in seen_order.iter().take(shown).enumerate()
        {
            let resolved = resolve_cache.get(callee_name).unwrap();
            let row = render_callee_row(
                callee_name,
                qualifier.as_deref(),
                resolved,
                *via_method,
                &def_file.path,
                caller_owner_type,
            );
            if row.is_empty() {
                suppressed += 1;
                continue;
            }
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
        if suppressed > 0 {
            out.push_str(&format!(
                "  (suppressed {suppressed} stdlib-stub callee(s): parse/init/Ok/clone/map/unwrap/...)\n",
            ));
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
        concise: bool,
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
                    // Drop candidates that live in a different language
                    // than the seed. Without this the deep walker happily
                    // resolved a Rust `fmt`/`ok` identifier through a
                    // JavaScript file (e.g. qartez-website/app.js) and
                    // emitted spurious `... -> fmt ambiguous` rows
                    // because `find_symbol_by_name` is language-agnostic.
                    // The call graph is strictly intra-language, so the
                    // cross-language hit is always a false positive.
                    if !f2.language.eq_ignore_ascii_case(&def_file.language) {
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

        let mut depths: Vec<usize> = by_depth.keys().copied().collect();
        depths.sort();

        // Concise-mode roll-up: when `format=concise` and a given
        // depth carries more than `ROLLUP_THRESHOLD` chain rows, emit
        // a single "depth N: K callees" summary line instead of
        // dumping every chain. This keeps the output scannable on
        // hub functions (e.g. `qartez_calls name=QartezServer::new
        // depth=2 format=concise` that previously produced 500+
        // chain lines) while preserving the full-tree rendering for
        // the default verbose mode. Depth-buckets that stay under
        // the threshold still render inline so small graphs look
        // the same as before.
        const ROLLUP_THRESHOLD: usize = 20;
        if concise {
            out.push_str("deeper:\n");
            let total_available: usize = by_depth.values().map(|v| v.len()).sum();
            let mut total_emitted = 0usize;
            for depth in &depths {
                let Some(entries) = by_depth.get(depth) else {
                    continue;
                };
                if entries.len() > ROLLUP_THRESHOLD {
                    let summary = format!("  depth {depth}: {} callees\n", entries.len());
                    if estimate_tokens(out) + estimate_tokens(&summary) > token_budget {
                        let remaining = total_available - total_emitted;
                        out.push_str(&format!("  ... +{remaining} more, raise token_budget=\n",));
                        return Ok(());
                    }
                    out.push_str(&summary);
                    total_emitted += entries.len();
                    continue;
                }
                for (_, chain) in entries {
                    if total_emitted >= limit {
                        let remaining = total_available - total_emitted;
                        out.push_str(&format!("  ... +{remaining} more, raise limit=\n"));
                        return Ok(());
                    }
                    let row = format!("  [depth {depth}] {chain}\n");
                    if estimate_tokens(out) + estimate_tokens(&row) > token_budget {
                        let remaining = total_available - total_emitted;
                        out.push_str(&format!("  ... +{remaining} more, raise token_budget=\n",));
                        return Ok(());
                    }
                    out.push_str(&row);
                    total_emitted += 1;
                }
            }
            out.push_str(
                "// note: concise mode rolls up depth buckets > 20 callees into a summary line; drop `format=concise` to see the full chains.\n",
            );
            return Ok(());
        }

        out.push_str("deeper:\n");
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
    caller_owner_type: Option<&str>,
) -> String {
    let func_like: Vec<&(
        crate::storage::models::SymbolRow,
        crate::storage::models::FileRow,
    )> = resolved
        .iter()
        .filter(|(s, _)| matches!(s.kind.as_str(), "function" | "method" | "constructor"))
        .collect();
    // Stdlib-stub denylist: identifiers like `clone`, `unwrap`, `map`,
    // `new`, etc. are dominated by language builtins whose real
    // implementation lives outside the index. Suppressing them unless
    // the call site ties to a concrete user-owned type keeps the
    // callee listing honest: a row like `clone (extern)` every ten
    // lines taught the reader nothing, and an `ambiguous (N candidates)`
    // row listing unrelated user structs was actively misleading. The
    // gate only skips rows we CANNOT resolve uniquely - rows that DO
    // resolve through the owner_type branch below pass through because
    // those name a concrete user symbol the caller actually depends on.
    if STDLIB_STUBS.contains(&callee_name) {
        let effective_qual: Option<&str> = match qualifier {
            Some(q) if q == "self" || q == "Self" => caller_owner_type,
            Some(q) => Some(q),
            None => None,
        };
        let resolves_uniquely = match effective_qual {
            Some(qual) => {
                func_like
                    .iter()
                    .filter(|(s, _)| s.owner_type.as_deref() == Some(qual))
                    .count()
                    == 1
            }
            None => false,
        };
        if !resolves_uniquely {
            return String::new();
        }
    }
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
    // `self.X(...)` / `Self::X(...)` calls carry the caller's impl-
    // block receiver type semantically, not the literal `self` token.
    // Fall through to the caller's owner_type when that context is
    // available so `QartezServer::new` calling `self.safe_resolve`
    // binds to `QartezServer::safe_resolve` instead of reporting
    // "ambiguous (7 candidates)".
    let effective_qual: Option<&str> = match qualifier {
        Some(q) if q == "self" || q == "Self" => caller_owner_type,
        Some(q) => Some(q),
        None => None,
    };
    if let Some(qual) = effective_qual {
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
    ///
    /// Honours `token_budget`: nodes beyond the budget are dropped and
    /// the graph is terminated with a dashed `truncated` marker edge.
    /// Before the 2026-04-23 fix, mermaid rendering bypassed the budget
    /// entirely and could emit 10x the requested volume on hub
    /// functions - the guard now mirrors the textual renderer's
    /// truncation contract.
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
        token_budget: usize,
    ) -> Result<String, String> {
        let max_nodes = 50;
        let mut out = String::from("graph TD\n");
        let target_id = helpers::mermaid_node_id(target_name);
        let target_label = helpers::mermaid_label(target_name);
        out.push_str(&format!("  {target_id}[\"{target_label}\"]\n"));
        // Shared predicate: a new edge row fits into the budget only if
        // its estimated tokens plus the current buffer stay under the
        // cap. Declared here so both caller and callee loops consult
        // the same rule without code duplication.
        let fits_budget =
            |buf: &str, row: &str| estimate_tokens(buf) + estimate_tokens(row) <= token_budget;
        let mut budget_exhausted = false;

        // Track nodes whose label has already been declared so each
        // id's bracketed label only appears once. Strict Mermaid
        // renderers (e.g. `@mermaid-js/mermaid-cli` >= 10) error on
        // duplicate id-with-label declarations like
        //   run["run"]
        //   run["run"]
        // even when the labels are identical. Emitting the subsequent
        // occurrences as bare `run` keeps the graph well-formed.
        let mut declared_nodes: HashSet<String> = HashSet::new();
        declared_nodes.insert(target_id.clone());
        let mut count = 0usize;
        let mut seen_edges = HashSet::new();

        'outer: for (sym, def_file) in func_symbols {
            if want_callers {
                for file in all_files {
                    if count >= max_nodes || budget_exhausted {
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
                        if count >= max_nodes || budget_exhausted {
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
                        let already_declared = declared_nodes.contains(&cid);
                        let row = if !already_declared {
                            format!("  {cid}[\"{clabel}\"] --> {target_id}\n")
                        } else {
                            format!("  {cid} --> {target_id}\n")
                        };
                        if !fits_budget(&out, &row) {
                            budget_exhausted = true;
                            break;
                        }
                        declared_nodes.insert(cid);
                        out.push_str(&row);
                        count += 1;
                    }
                }
            }

            if budget_exhausted {
                break 'outer;
            }

            if want_callees {
                let all_calls = self.cached_calls(&def_file.path);
                let start = sym.line_start as usize;
                let end = sym.line_end as usize;
                let mut seen = HashSet::new();
                for (name, line) in all_calls.iter() {
                    if count >= max_nodes || budget_exhausted {
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
                    let first_time = !declared_nodes.contains(&cid);
                    let row = if func_like >= 2 {
                        if first_time {
                            format!("  {target_id} -.->|?{func_like}| {cid}[\"{clabel}\"]\n")
                        } else {
                            format!("  {target_id} -.->|?{func_like}| {cid}\n")
                        }
                    } else if first_time {
                        format!("  {target_id} --> {cid}[\"{clabel}\"]\n")
                    } else {
                        format!("  {target_id} --> {cid}\n")
                    };
                    if !fits_budget(&out, &row) {
                        budget_exhausted = true;
                        break;
                    }
                    declared_nodes.insert(cid);
                    out.push_str(&row);
                    count += 1;
                }
            }

            if budget_exhausted {
                break 'outer;
            }
        }

        if count >= max_nodes || budget_exhausted {
            // Connect the truncation marker to the target so it is not
            // a dangling node. Strict Mermaid renderers otherwise warn
            // on orphan declarations. Using a fixed id keeps the
            // output deterministic across max-nodes and token-budget
            // truncation paths.
            let marker = format!("  {target_id} -.-> truncated[\"... truncated\"]\n");
            // Append the marker even if it slightly overshoots budget:
            // the caller must SEE that the output was cut rather than
            // silently reading a subset.
            out.push_str(&marker);
        }
        Ok(out)
    }
}
