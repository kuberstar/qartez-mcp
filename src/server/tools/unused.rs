// Rust guideline compliant 2026-04-22
#![allow(unused_imports)]

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::OnceLock;

use regex::RegexSet;
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

/// Directory prefixes whose exported symbols are almost always loaded by an
/// external runtime (plugin host, CLI extension loader, IDE extension API)
/// via string lookup rather than a static import edge. Matching the file's
/// relative path against any of these prefixes short-circuits the unused-
/// export check: the indexer cannot observe the dynamic caller, so the
/// symbol would otherwise be reported as dead even when it is a live entry
/// point. Paths are stored forward-slash-normalized (enforced by
/// `full_index_persists_forward_slash_keys`), so plain `str::starts_with`
/// suffices on all platforms.
const PLUGIN_ENTRY_DIR_PREFIXES: &[&str] = &["scripts/", "plugins/", "extensions/"];

/// Filename patterns that signal a plugin / extension entry-point module.
/// A symbol defined in a file whose basename matches any of these patterns
/// is skipped by `qartez_unused` for the same reason the directory prefixes
/// are skipped - the real caller is an external runtime that resolves
/// exports by string name, so the static reference graph cannot see the
/// edge. Compiled once via `OnceLock` so repeat invocations pay zero regex
/// build cost.
static PLUGIN_ENTRY_BASENAME_PATTERNS: OnceLock<RegexSet> = OnceLock::new();

fn plugin_entry_basename_patterns() -> &'static RegexSet {
    PLUGIN_ENTRY_BASENAME_PATTERNS.get_or_init(|| {
        // Anchored regexes matching the file basename (not the full path).
        // The extension is intentionally left free-form so `.ts`, `.tsx`,
        // `.js`, `.mjs`, `.py`, `.rs`, etc. all match without an explicit
        // allowlist. `[^.]+` forbids a second `.` so we do not over-match
        // unrelated multi-dotted filenames.
        RegexSet::new([
            r"^plugin\.[^.]+$",
            r"^extension\.[^.]+$",
            r"^.+-plugin\.[^.]+$",
            r"^.+-extension\.[^.]+$",
        ])
        .expect("plugin entry-point basename patterns must compile")
    })
}

/// Filename patterns for SvelteKit and adjacent meta-framework convention
/// entry-points. SvelteKit discovers route handlers, layouts, hooks, and
/// build configuration by filename, then loads exports (`load`, `actions`,
/// `ssr`, `prerender`, `GET`, `POST`, ...) by string name. The static
/// reference graph cannot observe the dynamic caller, so the symbols would
/// otherwise be reported as dead. The basename anchors keep the match
/// tight: a stray file called `+page.bak` is rejected, while `+page.ts`,
/// `+page.svelte`, `+page.server.ts`, `+layout.ts`, `+server.ts`,
/// `hooks.client.ts`, `hooks.server.ts`, `svelte.config.js`, etc. all hit.
static FRAMEWORK_CONVENTION_BASENAME_PATTERNS: OnceLock<RegexSet> = OnceLock::new();

fn framework_convention_basename_patterns() -> &'static RegexSet {
    FRAMEWORK_CONVENTION_BASENAME_PATTERNS.get_or_init(|| {
        RegexSet::new([
            // SvelteKit route conventions - the leading `+` is unique to
            // SvelteKit so we accept any single trailing extension or the
            // `.server.<ext>` / `.client.<ext>` shape.
            r"^\+page\.[^.]+$",
            r"^\+page\.(server|client)\.[^.]+$",
            r"^\+layout\.[^.]+$",
            r"^\+layout\.(server|client)\.[^.]+$",
            r"^\+server\.[^.]+$",
            r"^\+error\.[^.]+$",
            // Hooks are conventionally at the project root; SvelteKit
            // resolves them by filename.
            r"^hooks\.(server|client)\.[^.]+$",
            // Build / framework configs picked up by tooling at startup.
            r"^svelte\.config\.[^.]+$",
            r"^vite\.config\.[^.]+$",
            r"^playwright\.config\.[^.]+$",
        ])
        .expect("framework convention basename patterns must compile")
    })
}

/// Return `true` when `path` looks like a plugin, extension, or
/// meta-framework convention entry-point file. The check is a cheap
/// path-prefix scan followed by two `RegexSet::is_match` calls on the
/// basename, so the cost is constant per row.
fn is_framework_runtime_entry_path(path: &str) -> bool {
    if PLUGIN_ENTRY_DIR_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return true;
    }
    let basename = path.rsplit('/').next().unwrap_or(path);
    plugin_entry_basename_patterns().is_match(basename)
        || framework_convention_basename_patterns().is_match(basename)
}

/// Symbol kinds that the reachability scan treats as dead-code candidates.
/// Restricting to functions and methods keeps the signal focused on the
/// classic dead-code target and avoids flooding the report with every
/// unreferenced field, struct, or type - type-level relationships are not
/// fully represented as `symbol_refs` edges, so reporting them would produce
/// false positives.
fn is_function_like(kind: &str) -> bool {
    matches!(kind, "function" | "method")
}

/// Heuristic: does this symbol look like a test entry-point invoked by a test
/// harness rather than by a static call edge? Test runners discover and call
/// tests by attribute/convention, so the reference graph has no incoming edge
/// and the test - plus everything it exercises - would look unreachable.
/// Over-matching here is safe: an extra root can only *shrink* the reported
/// dead set, never invent a false positive.
fn is_test_symbol(sym: &crate::storage::models::SymbolRow, path: &str) -> bool {
    // Dedicated test locations across ecosystems: Rust `tests/` integration
    // dir, `*_test.*` (Go/Rust), `*.test.*` / `*.spec.*` (JS/TS).
    if path.starts_with("tests/") || path.contains("/tests/") {
        return true;
    }
    let basename = path.rsplit('/').next().unwrap_or(path);
    if basename.contains(".test.") || basename.contains(".spec.") {
        return true;
    }
    if let Some(stem) = basename.split('.').next() {
        if stem.ends_with("_test") {
            return true;
        }
    }
    // Name-based conventions, restricted to callables so type names like
    // `TestConfig` are not swept in: pytest/Rust `test_*`, Go `TestXxx`.
    is_function_like(&sym.kind)
        && (sym.name.starts_with("test_") || (sym.name.starts_with("Test") && sym.name.len() > 4))
}

/// Is this symbol a reachability *root* - an entry point whose caller lives
/// outside the observable reference graph? Roots seed the forward BFS in
/// `reachable` mode: binary `main`s, every exported public-API symbol
/// (any downstream crate may call it), framework-convention entry points
/// (loaded by string name), and test functions.
fn is_reachability_root(sym: &crate::storage::models::SymbolRow, path: &str) -> bool {
    sym.name == "main"
        || sym.is_exported
        || is_framework_runtime_entry_path(path)
        || is_test_symbol(sym, path)
}

/// Compute the symbols that are **not reachable** from any entry-point root,
/// walking the `symbol_refs` graph forward (`from_symbol_id -> to_symbol_id`).
///
/// This is the `reachable=true` dead-code model. Unlike the default one-hop
/// check (an exported symbol with zero *direct* importers), a function called
/// only by another dead function is still reported here, because the whole
/// chain is unreachable from a live root - the one-hop scan sees the incoming
/// edge and wrongly treats it as live.
///
/// Rows are returned in input order (path, then line, as produced by
/// `get_all_symbols_with_path`) so the caller can page and group them
/// directly. Only function/method symbols are returned - see
/// [`is_function_like`].
fn unreachable_symbols<'a>(
    symbols: &'a [(crate::storage::models::SymbolRow, String)],
    refs: &[(i64, i64)],
) -> Vec<&'a (crate::storage::models::SymbolRow, String)> {
    let mut adjacency: HashMap<i64, Vec<i64>> = HashMap::new();
    for &(from, to) in refs {
        adjacency.entry(from).or_default().push(to);
    }

    let mut visited: HashSet<i64> = HashSet::new();
    let mut queue: VecDeque<i64> = VecDeque::new();
    for (sym, path) in symbols {
        if is_reachability_root(sym, path) && visited.insert(sym.id) {
            queue.push_back(sym.id);
        }
    }
    while let Some(id) = queue.pop_front() {
        if let Some(neighbors) = adjacency.get(&id) {
            for &to in neighbors {
                if visited.insert(to) {
                    queue.push_back(to);
                }
            }
        }
    }

    symbols
        .iter()
        .filter(|(sym, _)| !visited.contains(&sym.id) && is_function_like(&sym.kind))
        .collect()
}

/// Render the `reachable=true` dead-code page, mirroring the compact per-file
/// layout of the default one-hop output (one header per file, one line per
/// symbol). `limit` is already normalized (`i64::MAX` means no cap) and
/// `offset` is a raw row offset into the full unreachable list.
fn render_reachable_dead(
    dead: &[&(crate::storage::models::SymbolRow, String)],
    limit: i64,
    offset: i64,
) -> String {
    let total = dead.len();
    if total == 0 {
        return "No unreachable symbols detected (whole-program reachability).".to_string();
    }
    let start = (offset.max(0) as usize).min(total);
    let end = if limit == i64::MAX {
        total
    } else {
        start.saturating_add(limit.max(0) as usize).min(total)
    };
    let page = &dead[start..end];
    if page.is_empty() {
        return format!("No unreachable symbols in page (total={total}, offset={offset}).");
    }

    let shown = page.len();
    let mut out = if end < total {
        format!(
            "{total} unreachable symbol(s) (whole-program reachability); showing {shown} from offset {offset} (next: offset={end}).\n",
        )
    } else {
        format!("{total} unreachable symbol(s) (whole-program reachability).\n")
    };

    let mut current_path: &str = "";
    for (sym, path) in page {
        if path != current_path {
            out.push_str(&format!("{path}\n"));
            current_path = path.as_str();
        }
        out.push_str(&format!(
            "  {} {} L{}\n",
            sym.kind.chars().next().unwrap_or(' '),
            sym.name,
            sym.line_start,
        ));
    }
    out
}

#[tool_router(router = qartez_unused_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_unused",
        description = "Find dead code: exported symbols with zero importers in the codebase. Safe candidates for removal or inlining. Pre-materialized at index time, so the whole-repo scan is a single indexed SELECT. Pass `limit` / `offset` to page through large result sets. `limit=0` removes the row cap (project-wide convention); omit `limit` to accept the 50-row default. Pass `reachable=true` for a deeper whole-program reachability scan: it seeds roots from `main`, exported public API, framework entry-points, and tests, then forward-walks the reference graph, so a function reachable only from other dead code is also reported (the default one-hop scan reports it as live because it still has a direct importer).",
        annotations(
            title = "Find Unused Exports",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_unused(
        &self,
        Parameters(params): Parameters<SoulUnusedParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;

        // `limit=0` means "no cap" project-wide convention, matching
        // `qartez_cochange` / `qartez_health`. `limit=None` keeps the
        // historical default of 50. The previous build rejected
        // `limit=0` outright, which made `qartez_unused` the only
        // page-able tool that did NOT accept the no-cap sentinel and
        // forced callers to pick an arbitrary upper bound.
        let limit = match params.limit {
            None => 50_i64,
            Some(0) => i64::MAX,
            Some(n) => n as i64,
        };
        let offset = params.offset.unwrap_or(0) as i64;

        // Opt-in whole-program reachability mode. The default path below
        // relies on the pre-materialized one-hop `unused_exports` view; the
        // reachable scan instead loads the full symbol/ref graph and walks it
        // forward from entry-point roots, so it also catches functions that
        // are referenced only by other dead code.
        if params.reachable.unwrap_or(false) {
            let symbols =
                read::get_all_symbols_with_path(&conn).map_err(|e| format!("DB error: {e}"))?;
            let refs = read::get_all_symbol_refs(&conn).map_err(|e| format!("DB error: {e}"))?;
            let dead = unreachable_symbols(&symbols, &refs);
            return Ok(render_reachable_dead(&dead, limit, offset));
        }

        let total = read::count_unused_exports(&conn).map_err(|e| format!("DB error: {e}"))?;
        if total == 0 {
            return Ok("No unused exported symbols detected.".to_string());
        }

        // Framework-convention entry-point files are loaded by external
        // runtimes via string lookup (e.g. OpenCode `Plugin` exports,
        // VS Code `activate` handlers, CLI script hooks, SvelteKit
        // `+page.ts` / `+server.ts` / `hooks.server.ts` route handlers).
        // The static reference graph cannot observe those callers, so the
        // row survives `NOT EXISTS (... symbol_refs ...)` and gets
        // reported as unused even when it is a live entry point. Over-
        // sample from the DB and drop those rows here so the caller-
        // visible page is always `limit` post-filter rows (unless the DB
        // is exhausted). Before oversampling, a page that happened to
        // contain a framework entry produced off-by-one counters like
        // "10 unused; showing 9" or "limit=5 returns 4" that looked
        // like a pagination bug.
        const FETCH_PAGE_SIZE: i64 = 64;
        let mut page: Vec<(
            crate::storage::models::SymbolRow,
            crate::storage::models::FileRow,
        )> = Vec::new();
        let mut fetch_offset = offset;
        // `consumed_offset` tracks the DB-row position right AFTER the
        // last row we either kept or skipped as a plugin entry. It is
        // the cursor a caller should pass as `offset` for the next
        // page so they neither skip rows (when a batch was over-
        // sampled past the kept rows) nor revisit rows already shown
        // (when plugin-filtered rows sit between kept rows). Before
        // this fix, the "next: offset=" hint reported `fetch_offset`
        // which advanced in FETCH_PAGE_SIZE chunks, so e.g. limit=5
        // produced "next: offset=64" and clicking through the pages
        // skipped 59 rows on every step.
        let mut consumed_offset = offset;
        let mut plugin_filtered = 0i64;
        let mut db_exhausted = false;
        'outer: loop {
            let remaining_room = (limit - page.len() as i64).max(0);
            if remaining_room == 0 {
                break;
            }
            let batch_size = remaining_room.max(FETCH_PAGE_SIZE);
            let batch = read::get_unused_exports_page(&conn, batch_size, fetch_offset)
                .map_err(|e| format!("DB error: {e}"))?;
            if batch.is_empty() {
                db_exhausted = true;
                break;
            }
            let batch_len = batch.len() as i64;
            fetch_offset += batch_len;
            for row in batch {
                if is_framework_runtime_entry_path(&row.1.path) {
                    plugin_filtered += 1;
                    consumed_offset += 1;
                    continue;
                }
                if page.len() as i64 >= limit {
                    break 'outer;
                }
                page.push(row);
                consumed_offset += 1;
            }
            if batch_len < batch_size {
                db_exhausted = true;
                break;
            }
        }
        let next_offset = consumed_offset;

        if page.is_empty() {
            return Ok(format!(
                "No unused exports in page (total={total}, offset={offset}; {plugin_filtered} framework-convention entries hidden - they're intentional)."
            ));
        }

        let shown = page.len() as i64;
        // `N framework-convention entries hidden - they're intentional`
        // replaces the bare `plugin_entries_skipped=N` counter used
        // before. The old key/value pair looked like a pagination
        // bug to callers ("why is this counter non-zero? what did I
        // do wrong?") because the tool never documented that the
        // filter always runs. The new phrasing names the filter,
        // explains why entries are hidden, and places the counter
        // inside a parenthetical so it reads as a note instead of
        // a flag to investigate.
        let mut out = if !db_exhausted && next_offset < total {
            if plugin_filtered > 0 {
                format!(
                    "{total} unused export(s); showing {shown} from offset {offset} (next: offset={next_offset}; {plugin_filtered} framework-convention entries hidden - they're intentional).\n",
                )
            } else {
                format!(
                    "{total} unused export(s); showing {shown} from offset {offset} (next: offset={next_offset}).\n",
                )
            }
        } else if plugin_filtered > 0 {
            format!(
                "{total} unused export(s); showing {shown} of {total} ({plugin_filtered} framework-convention entries hidden - they're intentional).\n",
            )
        } else {
            format!("{total} unused export(s).\n")
        };

        // Compact per-file format: one header per file, one line per symbol
        // without the parenthesized kind (it's redundant with the kind-letter
        // prefix). Saves ~40% tokens vs the old `  - name (kind) [L-L]` shape.
        let mut current_path: &str = "";
        for (sym, file) in &page {
            if file.path != current_path {
                out.push_str(&format!("{}\n", file.path));
                current_path = file.path.as_str();
            }
            out.push_str(&format!(
                "  {} {} L{}\n",
                sym.kind.chars().next().unwrap_or(' '),
                sym.name,
                sym.line_start,
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod reachable_tests {
    use super::*;
    use crate::storage::models::SymbolRow;

    /// Build a minimal function/method `SymbolRow` at a synthetic path for the
    /// pure BFS tests.
    fn sym(id: i64, name: &str, is_exported: bool) -> (SymbolRow, String) {
        (
            SymbolRow {
                id,
                file_id: 1,
                name: name.to_string(),
                kind: "function".to_string(),
                line_start: (id as u32) * 10,
                line_end: (id as u32) * 10 + 5,
                signature: None,
                is_exported,
                shape_hash: None,
                parent_id: None,
                pagerank: 0.0,
                complexity: None,
                owner_type: None,
            },
            "src/lib.rs".to_string(),
        )
    }

    fn dead_names(symbols: &[(SymbolRow, String)], refs: &[(i64, i64)]) -> Vec<String> {
        unreachable_symbols(symbols, refs)
            .into_iter()
            .map(|(s, _)| s.name.clone())
            .collect()
    }

    #[test]
    fn reachable_flags_dead_subgraph_that_one_hop_misses() {
        // `public_api` (exported root) -> `helper` is a live chain.
        // `dead_a` -> `dead_b` is an isolated dead chain: neither is exported,
        // `main`, a test, or a framework entry, so no root reaches them.
        let symbols = vec![
            sym(10, "public_api", true),
            sym(11, "helper", false),
            sym(1, "dead_a", false),
            sym(2, "dead_b", false),
        ];
        let refs = vec![(10_i64, 11_i64), (1_i64, 2_i64)];

        let dead = dead_names(&symbols, &refs);

        // The default one-hop scan sees `dead_b` has an incoming edge from
        // `dead_a` and reports it as live; reachability catches both because
        // the whole chain is unreachable from any root.
        assert!(dead.contains(&"dead_a".to_string()), "dead: {dead:?}");
        assert!(dead.contains(&"dead_b".to_string()), "dead: {dead:?}");
        assert!(!dead.contains(&"public_api".to_string()));
        assert!(!dead.contains(&"helper".to_string()));
        assert_eq!(dead.len(), 2);
    }

    #[test]
    fn reachable_treats_exported_root_chain_as_live() {
        // Once `a` is exported it becomes a root, so `b` is reachable and
        // neither is reported - the reachability walk follows the edge.
        let symbols = vec![sym(1, "a", true), sym(2, "b", false)];
        let refs = vec![(1_i64, 2_i64)];
        assert!(unreachable_symbols(&symbols, &refs).is_empty());
    }

    #[test]
    fn reachable_seeds_main_and_test_roots() {
        // `main` and test functions are roots even when not exported.
        let symbols = vec![
            sym(1, "main", false),
            sym(2, "boot", false),
            sym(3, "test_thing", false),
            sym(4, "checked_by_test", false),
            sym(5, "orphan", false),
        ];
        let refs = vec![(1_i64, 2_i64), (3_i64, 4_i64)];

        let dead = dead_names(&symbols, &refs);

        assert_eq!(dead, vec!["orphan".to_string()], "dead: {dead:?}");
    }

    #[test]
    fn render_reachable_dead_paginates_and_reports_next_offset() {
        let rows = vec![sym(1, "a", false), sym(2, "b", false), sym(3, "c", false)];
        let dead: Vec<&(SymbolRow, String)> = rows.iter().collect();

        let page = render_reachable_dead(&dead, 2, 0);
        assert!(page.contains("3 unreachable symbol(s)"));
        assert!(page.contains("next: offset=2"));
        assert!(page.contains(" a L"));
        assert!(page.contains(" b L"));
        assert!(!page.contains(" c L"));

        // No-cap sentinel dumps everything with no "next" hint.
        let all = render_reachable_dead(&dead, i64::MAX, 0);
        assert!(all.contains(" c L"));
        assert!(!all.contains("next: offset"));
    }
}
