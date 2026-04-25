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
use crate::test_paths::{is_testable_source_language, is_testable_source_path};
use crate::toolchain;

/// Extract the module-stem used by crate-rooted `use` imports in Rust.
///
/// Rust's `use <crate>::<mod>` refers to a module by its file stem with
/// hyphens already normalised to underscores; the test-gaps FTS fallback
/// uses that stem to probe whether any test file body references the
/// source module even though the edge resolver dropped the import.
///
/// Returns `None` for paths the predicate cannot meaningfully probe:
/// non-file paths, `mod.rs` / `lib.rs` / `main.rs` (their identifier is
/// the parent directory, not the file), and stems that are not valid
/// Rust identifiers.
fn rust_module_stem(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let stem = name.strip_suffix(".rs")?;
    if matches!(stem, "mod" | "lib" | "main") {
        return None;
    }
    if stem.is_empty() {
        return None;
    }
    if !stem.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    if stem.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(stem.to_string())
}

/// Returns true when any test-file symbol body indexed in the FTS table
/// mentions `stem` as a token. Used as a fallback coverage signal for
/// Rust crate-rooted imports (`use <crate>::<stem>`) and subprocess-
/// style binary tests that the edge graph cannot observe. FTS failures
/// (missing index, malformed query) are non-fatal and treated as "no
/// mention" so the caller falls back to the normal edge-based gap rule.
fn stem_mentioned_in_any_test(
    conn: &rusqlite::Connection,
    stem: &str,
    test_paths: &HashSet<&str>,
) -> bool {
    if test_paths.is_empty() {
        return false;
    }
    let Ok(paths) = read::find_file_paths_by_body_text(conn, stem) else {
        return false;
    };
    paths.iter().any(|p| test_paths.contains(p.as_str()))
}

#[tool_router(router = qartez_test_gaps_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_test_gaps",
        description = "Test-to-code mapping, coverage gap detection, and test suggestion for changes. Three modes: 'map' shows which test files cover which source files via import edges. 'gaps' (default) finds untested source files ranked by risk score (health * blast radius). 'suggest' takes a git diff range and returns which existing tests to run plus which changed files lack test coverage.",
        annotations(
            title = "Test Coverage Gaps",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_test_gaps(
        &self,
        Parameters(params): Parameters<SoulTestGapsParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_test_gaps")?;
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let limit = params.limit.unwrap_or(30) as usize;
        let concise = is_concise(&params.format);
        let mode = params.mode.as_deref().unwrap_or("gaps");

        let all_files = read::get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;
        let all_edges = read::get_all_edges(&conn).map_err(|e| format!("DB error: {e}"))?;
        let ctx = TestGapsCtx::build(&all_files, &all_edges);

        match mode {
            "map" => self.test_gaps_map(&params, &ctx, &conn, limit, concise),
            "gaps" => self.test_gaps_find(&params, &ctx, &conn, limit, concise),
            "suggest" => self.test_gaps_suggest(&params, &ctx, &conn, limit, concise),
            _ => Err(format!(
                "Unknown mode '{mode}'. Use 'map', 'gaps', or 'suggest'."
            )),
        }
    }
}

struct TestGapsCtx<'a> {
    all_files: &'a [crate::storage::models::FileRow],
    id_to_file: HashMap<i64, &'a crate::storage::models::FileRow>,
    path_to_id: HashMap<&'a str, i64>,
    forward: HashMap<i64, Vec<i64>>,
    reverse: HashMap<i64, Vec<i64>>,
}

impl<'a> TestGapsCtx<'a> {
    fn build(all_files: &'a [crate::storage::models::FileRow], all_edges: &[(i64, i64)]) -> Self {
        let id_to_file: HashMap<i64, &'a crate::storage::models::FileRow> =
            all_files.iter().map(|f| (f.id, f)).collect();
        let path_to_id: HashMap<&'a str, i64> =
            all_files.iter().map(|f| (f.path.as_str(), f.id)).collect();

        let mut forward: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut reverse: HashMap<i64, Vec<i64>> = HashMap::new();
        for &(from, to) in all_edges {
            if from != to {
                forward.entry(from).or_default().push(to);
                reverse.entry(to).or_default().push(from);
            }
        }

        Self {
            all_files,
            id_to_file,
            path_to_id,
            forward,
            reverse,
        }
    }
}

impl QartezServer {
    fn test_gaps_map(
        &self,
        params: &SoulTestGapsParams,
        ctx: &TestGapsCtx<'_>,
        conn: &rusqlite::Connection,
        limit: usize,
        concise: bool,
    ) -> Result<String, String> {
        let mut source_to_tests: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut test_to_sources: HashMap<&str, Vec<&str>> = HashMap::new();

        for file in ctx.all_files {
            if !is_test_path(&file.path) {
                continue;
            }
            let imports = ctx.forward.get(&file.id).cloned().unwrap_or_default();
            for imp_id in imports {
                if let Some(imp_file) = ctx.id_to_file.get(&imp_id)
                    && !is_test_path(&imp_file.path)
                {
                    source_to_tests
                        .entry(imp_file.path.as_str())
                        .or_default()
                        .push(file.path.as_str());
                    test_to_sources
                        .entry(file.path.as_str())
                        .or_default()
                        .push(imp_file.path.as_str());
                }
            }
        }

        // Dispatcher-table fallback: tests that exercise a source file
        // via a string-keyed dispatcher (e.g.
        // `server.call_tool_by_name("qartez_find", args)`) never emit
        // an `import` edge for `tools/find.rs`. Scan every test file
        // source for string-literal tool names and, when a literal
        // matches a source file stem (with an optional `<prefix>_`
        // namespace), credit that source with coverage from this
        // test. Each test file is read at most once per call.
        augment_map_with_dispatcher_calls(
            &self.project_root,
            ctx,
            &mut source_to_tests,
            &mut test_to_sources,
        );

        if let Some(ref fp) = params.file_path {
            // Resolve relative to the project root via `safe_resolve`
            // (which handles multi-root prefixes and path-escape
            // rejection) and prefer the user-supplied relative form
            // for display when `strip_prefix` against the primary
            // project root does not match. Under multi-root setups,
            // `safe_resolve` returns a path anchored to the ALIAS
            // root (e.g. `qartez-public/...`) which is not a child
            // of `self.project_root`; the old `unwrap_or(&resolved)`
            // fallback then surfaced the absolute path in messages
            // like `/private/tmp/src/...` after a spurious prefix
            // strip. Falling back to the caller-supplied relative
            // path keeps the displayed form stable across root
            // configurations.
            let resolved = self.safe_resolve(fp).map_err(|e| e.to_string())?;
            let rel = match resolved.strip_prefix(&self.project_root) {
                Ok(stripped) => {
                    crate::index::to_forward_slash(stripped.to_string_lossy().into_owned())
                }
                Err(_) => crate::index::to_forward_slash(fp.clone()),
            };

            if is_test_path(&rel) {
                let sources = test_to_sources
                    .get(rel.as_str())
                    .cloned()
                    .unwrap_or_default();
                if sources.is_empty() {
                    let mut msg = format!("Test file '{rel}' has no indexed source imports.");
                    if params.include_symbols.unwrap_or(false) {
                        // Make the no-op status of `include_symbols`
                        // observable. Silently ignoring the flag read
                        // as a bug to callers who passed it expecting
                        // a symbol-level breakdown.
                        msg.push_str(
                            " include_symbols=true had no effect: the flag needs at least one mapped source file on the test->sources side.",
                        );
                    }
                    return Ok(msg);
                }
                let mut out = format!(
                    "# Test coverage: {rel}\n\nImports {} source file(s):\n",
                    sources.len(),
                );
                for src in sources.iter().take(limit) {
                    out.push_str(&format!("  - {src}\n"));
                }
                return Ok(out);
            }

            let tests = source_to_tests
                .get(rel.as_str())
                .cloned()
                .unwrap_or_default();
            if tests.is_empty() {
                let mut msg = format!("Source file '{rel}' has no test files importing it.");
                if params.include_symbols.unwrap_or(false) {
                    // Surface the remediation instead of a bare
                    // "had no effect" note. The previous wording
                    // made the flag look broken; this version
                    // explains exactly why it did nothing (no test
                    // file imports the source) and points at the
                    // right mode to answer the bigger question.
                    msg.push_str(&format!(
                        " include_symbols=true had no effect: no test file imports '{rel}'. Use mode=gaps to find files without tests.",
                    ));
                }
                return Ok(msg);
            }
            let mut out = format!("# Test coverage: {rel}\n\n{} test file(s):\n", tests.len(),);
            for t in tests.iter().take(limit) {
                out.push_str(&format!("  - {t}\n"));
            }
            if params.include_symbols.unwrap_or(false)
                && let Some(&file_id) = ctx.path_to_id.get(rel.as_str())
            {
                // Intersection of `def(file)` and `ref(by test files)`:
                // show the symbols from the source file that are actually
                // exercised by its mapped test files. The old behaviour
                // listed all exports, which misled callers into thinking
                // every export was covered.
                let test_ids: Vec<i64> = tests
                    .iter()
                    .filter_map(|t| ctx.path_to_id.get(*t).copied())
                    .collect();
                if test_ids.is_empty() {
                    out.push_str(
                        "\nReferenced symbols: none - tests reach this file via path / FTS mapping, not indexed symbol edges.\n",
                    );
                } else {
                    let referenced = read::test_gaps_referenced_by_tests(conn, file_id, &test_ids)
                        .map_err(|e| format!("DB error: {e}"))?;
                    if referenced.is_empty() {
                        out.push_str(
                            "\nReferenced symbols: none - no indexed symbol edges from the mapped test files resolve into this source.\n",
                        );
                    } else {
                        out.push_str(&format!(
                            "\n{} symbol(s) referenced by tests:\n",
                            referenced.len(),
                        ));
                        for sym in referenced.iter().take(20) {
                            out.push_str(&format!("  - {} ({})\n", sym.name, sym.kind));
                        }
                    }
                }
            }
            return Ok(out);
        }

        let mut entries: Vec<(&str, &Vec<&str>)> =
            source_to_tests.iter().map(|(&k, v)| (k, v)).collect();
        entries.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));

        let total_covered = entries.len();
        // Source-file counts exclude file types that cannot
        // meaningfully hold unit tests (shell scripts, TOML manifests,
        // YAML, Dockerfile). Keeping them in the denominator would
        // understate the coverage ratio the tool reports to the user.
        let total_source = ctx
            .all_files
            .iter()
            .filter(|f| !is_test_path(&f.path) && is_testable_source_language(&f.language))
            .count();
        let total_test = ctx
            .all_files
            .iter()
            .filter(|f| is_test_path(&f.path))
            .count();

        let mut out = format!(
            "# Test-to-source mapping\n\n{total_covered}/{total_source} source files covered by {total_test} test files\n\n",
        );

        // When include_symbols is set on the project-wide listing,
        // annotate every source row with the count of its own indexed
        // symbols AND, in detailed mode, show up to a handful of them.
        // The flag used to only affect the single-file branch above,
        // so callers who set it here got no signal at all - now the
        // output grows with whatever qartez actually knows about each
        // source file.
        let include_symbols_project = params.include_symbols.unwrap_or(false);

        if concise {
            for (src, tests) in entries.iter().take(limit) {
                if include_symbols_project {
                    let sym_count = ctx
                        .path_to_id
                        .get(src)
                        .and_then(|id| read::get_symbols_for_file(conn, *id).ok())
                        .map(|v| v.iter().filter(|s| s.kind != "field").count())
                        .unwrap_or(0);
                    out.push_str(&format!(
                        "  {} ({} tests, {} symbols)\n",
                        src,
                        tests.len(),
                        sym_count,
                    ));
                } else {
                    out.push_str(&format!("  {} ({})\n", src, tests.len()));
                }
            }
        } else {
            for (src, tests) in entries.iter().take(limit) {
                out.push_str(&format!("- {} ({} tests)\n", src, tests.len()));
                for t in tests.iter().take(5) {
                    out.push_str(&format!("    - {t}\n"));
                }
                if tests.len() > 5 {
                    out.push_str(&format!("    ... and {} more\n", tests.len() - 5,));
                }
                if include_symbols_project
                    && let Some(&file_id) = ctx.path_to_id.get(src)
                    && let Ok(syms) = read::get_symbols_for_file(conn, file_id)
                {
                    let visible: Vec<_> = syms.iter().filter(|s| s.kind != "field").collect();
                    if !visible.is_empty() {
                        out.push_str(&format!("    symbols ({}):\n", visible.len()));
                        for sym in visible.iter().take(5) {
                            out.push_str(&format!("      - {} ({})\n", sym.name, sym.kind));
                        }
                        if visible.len() > 5 {
                            out.push_str(&format!("      ... and {} more\n", visible.len() - 5,));
                        }
                    }
                }
            }
        }
        if entries.len() > limit {
            out.push_str(&format!(
                "\n... and {} more (use limit= to see more)\n",
                entries.len() - limit,
            ));
        }

        Ok(out)
    }

    fn test_gaps_find(
        &self,
        params: &SoulTestGapsParams,
        ctx: &TestGapsCtx<'_>,
        conn: &rusqlite::Connection,
        limit: usize,
        concise: bool,
    ) -> Result<String, String> {
        let min_pagerank = params.min_pagerank.unwrap_or(0.0);

        // Resolve the optional scope filter. Without this, `gaps` ignored
        // `file_path` entirely and always walked the whole project, which
        // contradicts the documented "scope to a single file path" contract.
        // Two modes are supported:
        //   - exact file path match (e.g. `src/foo.rs`)
        //   - directory prefix match (e.g. `src/` or `src/server`)
        // The prefix form lets callers narrow gaps to a subtree without
        // listing every file.
        let scope_rel: Option<String> = match params.file_path.as_ref() {
            None => None,
            Some(raw) => {
                let resolved = self.safe_resolve(raw).map_err(|e| e.to_string())?;
                let rel = crate::index::to_forward_slash(
                    resolved
                        .strip_prefix(&self.project_root)
                        .unwrap_or(&resolved)
                        .to_string_lossy()
                        .into_owned(),
                );
                Some(rel)
            }
        };

        let all_syms =
            read::get_all_symbols_with_path(conn).map_err(|e| format!("DB error: {e}"))?;
        let mut max_cc_by_path: HashMap<&str, u32> = HashMap::new();
        for (sym, path) in &all_syms {
            if let Some(cc) = sym.complexity {
                let entry = max_cc_by_path.entry(path.as_str()).or_insert(0);
                if cc > *entry {
                    *entry = cc;
                }
            }
        }

        let health_of = |max_cc: f64, coupling: f64, churn: i64| -> f64 {
            let cc_h = 10.0 / (1.0 + max_cc / 10.0);
            let coupling_h = 10.0 / (1.0 + coupling * 50.0);
            let churn_h = 10.0 / (1.0 + churn as f64 / 8.0);
            (cc_h + coupling_h + churn_h) / 3.0
        };

        // Precompute the set of test-file paths so the FTS-body fallback
        // can classify a lookup hit as "mentioned by a test" without a
        // second pass over `all_files`.
        let test_paths: HashSet<&str> = ctx
            .all_files
            .iter()
            .filter(|f| is_test_path(&f.path))
            .map(|f| f.path.as_str())
            .collect();

        let mut gaps: Vec<(&crate::storage::models::FileRow, f64)> = Vec::new();

        for file in ctx.all_files {
            if is_test_path(&file.path) || file.pagerank < min_pagerank {
                continue;
            }
            // Filter out indexed-but-not-testable file types (shell
            // scripts, Cargo manifests, Dockerfile, YAML, etc.). They
            // enter the DB for dependency / security analysis but do
            // not support unit tests, so flagging them as coverage
            // gaps is always a false positive.
            if !is_testable_source_language(&file.language) {
                continue;
            }
            // Honour the optional `file_path` scope. Accept either an
            // exact path match or a directory prefix so callers can
            // narrow to a subtree (e.g. `src/server`) without listing
            // every leaf file.
            if let Some(ref scope) = scope_rel {
                let fp = file.path.as_str();
                let under_dir = {
                    let dir = scope.trim_end_matches('/');
                    !dir.is_empty()
                        && (fp == dir
                            || fp.starts_with(&format!("{dir}/"))
                            || scope.ends_with('/') && fp.starts_with(scope.as_str()))
                };
                if fp != scope.as_str() && !under_dir {
                    continue;
                }
            }

            let has_test_importer = ctx.reverse.get(&file.id).is_some_and(|importers| {
                importers.iter().any(|&imp_id| {
                    ctx.id_to_file
                        .get(&imp_id)
                        .is_some_and(|f| is_test_path(&f.path))
                })
            });

            let mut covered =
                has_test_importer || has_inline_rust_tests(&self.project_root, &file.path);

            // FTS-body fallback for Rust crate-rooted imports. The
            // edge resolver only recognises `crate::`, `super::`, and
            // `self::` prefixes, so `use <crate_name>::<module>` in a
            // tests/*.rs file never emits an import edge. Before
            // flagging the source as untested, probe whether any test
            // file body mentions the module stem - this catches both
            // crate-rooted imports and subprocess-style tests that
            // spawn `cargo run --bin <name>`.
            if !covered
                && let Some(stem) = rust_module_stem(&file.path)
                && stem_mentioned_in_any_test(conn, &stem, &test_paths)
            {
                covered = true;
            }

            if !covered {
                let max_cc = max_cc_by_path.get(file.path.as_str()).copied().unwrap_or(0) as f64;
                let health = health_of(max_cc, file.pagerank, file.change_count);
                let blast_count = ctx.reverse.get(&file.id).map(|v| v.len()).unwrap_or(0);
                let score = (10.0 - health) * (1.0 + blast_count as f64 / 10.0);
                gaps.push((file, score));
            }
        }

        gaps.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        if gaps.is_empty() {
            // When `min_pagerank` filtered every candidate out of the
            // scan, the empty result is "filtered, not actually
            // covered". Surface that distinction so callers do not
            // mistake an aggressive filter for full coverage. Compare
            // against `f64::EPSILON` rather than `> 0.0` so a
            // deliberate `min_pagerank=0.0` from the schema default
            // does not trigger the filtered-out branch.
            let candidates_above_filter = ctx
                .all_files
                .iter()
                .filter(|f| !is_test_path(&f.path) && is_testable_source_language(&f.language))
                .filter(|f| match scope_rel.as_deref() {
                    None => true,
                    Some(scope) => {
                        let dir = scope.trim_end_matches('/');
                        f.path == scope
                            || (!dir.is_empty()
                                && (f.path.as_str() == dir
                                    || f.path.starts_with(&format!("{dir}/"))))
                    }
                })
                .count();
            let any_above_filter = ctx.all_files.iter().any(|f| {
                !is_test_path(&f.path)
                    && is_testable_source_language(&f.language)
                    && f.pagerank >= min_pagerank
            });
            if min_pagerank > f64::EPSILON && !any_above_filter && candidates_above_filter > 0 {
                let msg = match scope_rel.as_deref() {
                    Some(scope) => format!(
                        "No source files met `min_pagerank={min_pagerank}` under scope `{scope}` ({candidates_above_filter} testable file(s) exist in the scope but all rank below the filter). Lower `min_pagerank` or omit it.",
                    ),
                    None => format!(
                        "No source files met `min_pagerank={min_pagerank}` ({candidates_above_filter} testable file(s) exist but all rank below the filter). Lower `min_pagerank` or omit it.",
                    ),
                };
                return Ok(msg);
            }
            let msg = match scope_rel.as_deref() {
                Some(scope) => format!(
                    "No untested source files found under scope `{scope}`. All files within the scope are covered by an external test file import or inline Rust tests (`#[cfg(test)]` / `#[test]`)."
                ),
                None => "No untested source files found. All source files are covered by an external test file import or inline Rust tests (`#[cfg(test)]` / `#[test]`).".to_string(),
            };
            return Ok(msg);
        }

        // Denominator tracks the same scope the gap list reports against.
        // Without this, scoping to `src/server` would still show the
        // project-wide count of testable source files, which is
        // misleading.
        let total_source = ctx
            .all_files
            .iter()
            .filter(|f| !is_test_path(&f.path) && is_testable_source_language(&f.language))
            .filter(|f| match scope_rel.as_deref() {
                None => true,
                Some(scope) => {
                    let dir = scope.trim_end_matches('/');
                    f.path == scope
                        || (!dir.is_empty()
                            && (f.path.as_str() == dir || f.path.starts_with(&format!("{dir}/"))))
                }
            })
            .count();
        let gap_count = gaps.len();
        let shown = gap_count.min(limit);

        let mut out =
            format!("# Test coverage gaps ({gap_count}/{total_source} source files untested)\n\n",);
        if shown < gap_count {
            out.push_str(&format!(
                "Showing {shown} of {gap_count} (use limit= to see more).\n\n",
            ));
        }

        if concise {
            for (file, score) in gaps.iter().take(limit) {
                let blast = ctx.reverse.get(&file.id).map(|v| v.len()).unwrap_or(0);
                out.push_str(&format!(
                    "  {} PR={:.4} blast={} score={:.1}\n",
                    file.path, file.pagerank, blast, score,
                ));
            }
        } else {
            out.push_str("| File | PageRank | Blast | Churn | Score |\n");
            out.push_str("|------|----------|-------|-------|-------|\n");
            for (file, score) in gaps.iter().take(limit) {
                let blast = ctx.reverse.get(&file.id).map(|v| v.len()).unwrap_or(0);
                out.push_str(&format!(
                    "| {} | {:.4} | {} | {} | {:.1} |\n",
                    truncate_path(&file.path, 40),
                    file.pagerank,
                    blast,
                    file.change_count,
                    score,
                ));
            }
        }

        Ok(out)
    }

    fn test_gaps_suggest(
        &self,
        params: &SoulTestGapsParams,
        ctx: &TestGapsCtx<'_>,
        conn: &rusqlite::Connection,
        limit: usize,
        concise: bool,
    ) -> Result<String, String> {
        let base = params.base.as_deref().ok_or(
            "The 'suggest' mode requires a 'base' parameter (git diff range, e.g., 'main' or 'HEAD~3').",
        )?;

        let changed = crate::git::diff::changed_files_in_range(&self.project_root, base)
            .map_err(|e| super::diff_impact::friendly_git_error(base, &e))?;

        if changed.is_empty() {
            return Ok(format!("No files changed in range '{base}'."));
        }

        let changed_source: Vec<&str> = changed
            .iter()
            .map(|s| s.as_str())
            .filter(|p| !is_test_path(p))
            .filter(|p| {
                // Skip non-testable file types. Indexed paths use
                // the precise `FileRow::language` signal; unindexed
                // paths (CHANGELOG.md, Cargo.lock, install.ps1,
                // SKILL.md) now fall back to a path-based classifier
                // instead of the old `.unwrap_or(true)` which flagged
                // every non-source file as "needs new tests" and
                // diverged from qartez_diff_impact on the same input.
                match ctx.path_to_id.get(p).and_then(|id| ctx.id_to_file.get(id)) {
                    Some(f) => is_testable_source_language(&f.language),
                    None => is_testable_source_path(p),
                }
            })
            .collect();
        let changed_tests: Vec<&str> = changed
            .iter()
            .map(|s| s.as_str())
            .filter(|p| is_test_path(p))
            .collect();

        let test_paths: HashSet<&str> = ctx
            .all_files
            .iter()
            .filter(|f| is_test_path(&f.path))
            .map(|f| f.path.as_str())
            .collect();

        let mut tests_to_run: HashMap<String, Vec<String>> = HashMap::new();
        let mut untested_sources: Vec<&str> = Vec::new();

        for &src_path in &changed_source {
            let file_id = match ctx.path_to_id.get(src_path) {
                Some(&id) => id,
                None => {
                    untested_sources.push(src_path);
                    continue;
                }
            };

            guard::touch_ack(&self.project_root, src_path);

            let mut found_tests: Vec<String> = Vec::new();

            if let Some(importers) = ctx.reverse.get(&file_id) {
                for &imp_id in importers {
                    if let Some(imp_file) = ctx.id_to_file.get(&imp_id)
                        && is_test_path(&imp_file.path)
                    {
                        found_tests.push(imp_file.path.clone());
                    }
                }
            }

            let cochanges = read::get_cochanges(conn, file_id, 10).unwrap_or_default();
            for (_, partner) in &cochanges {
                if is_test_path(&partner.path) && !found_tests.contains(&partner.path) {
                    found_tests.push(partner.path.clone());
                }
            }

            // Crate-rooted import fallback (see `test_gaps_find`): a
            // tests/*.rs calling `qartez_mcp::cli_runner::run(...)`
            // emits no import edge, so back-stop with the FTS body
            // index over the full test-file set.
            let has_fts_mention = rust_module_stem(src_path)
                .map(|stem| stem_mentioned_in_any_test(conn, &stem, &test_paths))
                .unwrap_or(false);

            if found_tests.is_empty()
                && !has_inline_rust_tests(&self.project_root, src_path)
                && !has_fts_mention
            {
                untested_sources.push(src_path);
            } else if !found_tests.is_empty() {
                for t in &found_tests {
                    tests_to_run
                        .entry(t.clone())
                        .or_default()
                        .push(src_path.to_string());
                }
            }
        }

        for &test_path in &changed_tests {
            if !tests_to_run.contains_key(test_path) {
                tests_to_run
                    .entry(test_path.to_string())
                    .or_default()
                    .push("(directly changed)".into());
            }
            guard::touch_ack(&self.project_root, test_path);
        }

        let mut test_entries: Vec<(&String, &Vec<String>)> = tests_to_run.iter().collect();
        test_entries.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));

        let mut out = format!(
            "# Test suggestion for {base}\n\n{} changed files ({} source, {} test)\n\n",
            changed.len(),
            changed_source.len(),
            changed_tests.len(),
        );

        if test_entries.is_empty() && untested_sources.is_empty() {
            out.push_str("No test files found for the changed source files.\n");
            return Ok(out);
        }

        if !test_entries.is_empty() {
            out.push_str(&format!(
                "## Tests to run ({} test files)\n",
                test_entries.len(),
            ));
            if concise {
                for (test, sources) in test_entries.iter().take(limit) {
                    out.push_str(&format!("  {} (covers {})\n", test, sources.len(),));
                }
            } else {
                for (test, sources) in test_entries.iter().take(limit) {
                    out.push_str(&format!("- {test}\n"));
                    for src in sources.iter().take(5) {
                        out.push_str(&format!("    covers: {src}\n"));
                    }
                    if sources.len() > 5 {
                        out.push_str(&format!("    ... and {} more\n", sources.len() - 5,));
                    }
                }
            }
            out.push('\n');
        }

        if !untested_sources.is_empty() {
            out.push_str(&format!(
                "## Untested changes ({} source files need new tests)\n",
                untested_sources.len(),
            ));
            for src in untested_sources.iter().take(limit) {
                out.push_str(&format!("  - {src}\n"));
            }
        }

        Ok(out)
    }
}

/// Regex source for the string-literal arguments of `call_tool_by_name`
/// style dispatchers. Keep this open to whitespace after the opener and
/// between the literal and the comma so clippy-formatted callers match.
/// The captured literal's trailing quote is dropped by the match end.
const DISPATCHER_CALL_REGEX: &str = r#"call_tool_by_name\s*\(\s*"([A-Za-z_][A-Za-z0-9_]*)""#;

/// Augment the import-based `source_to_tests` / `test_to_sources` maps
/// with a dispatcher-table fallback. When a test file body contains a
/// `call_tool_by_name("X", ...)` style pattern and `X` (or a
/// `<prefix>_<stem>` shape) matches a non-test source file stem in the
/// index, credit that source with coverage from the test file.
///
/// `test_paths` are the tests to scan (already filtered from
/// `ctx.all_files`); `source_stems` maps lowercased source stems to
/// the `&str` slice inside `ctx.all_files` so the borrowed lifetimes
/// of the resulting HashMaps stay consistent with the import-based
/// entries populated earlier.
fn augment_map_with_dispatcher_calls<'a>(
    project_root: &std::path::Path,
    ctx: &'a TestGapsCtx<'a>,
    source_to_tests: &mut HashMap<&'a str, Vec<&'a str>>,
    test_to_sources: &mut HashMap<&'a str, Vec<&'a str>>,
) {
    let Ok(re) = regex::Regex::new(DISPATCHER_CALL_REGEX) else {
        return;
    };

    // Build `stem -> source file path slice` index once up front. A
    // given stem can resolve to multiple files (e.g. two languages
    // defining the same module name); every match is credited.
    let mut stem_to_sources: HashMap<String, Vec<&'a str>> = HashMap::new();
    for file in ctx.all_files {
        if is_test_path(&file.path) {
            continue;
        }
        let Some(name) = file.path.rsplit('/').next() else {
            continue;
        };
        let stem = match name.rfind('.') {
            Some(dot) => &name[..dot],
            None => name,
        };
        if stem.is_empty() {
            continue;
        }
        stem_to_sources
            .entry(stem.to_ascii_lowercase())
            .or_default()
            .push(file.path.as_str());
    }

    // Track existing edges so dispatcher-only coverage never
    // double-credits a source that already has an import edge.
    let mut seen: HashSet<(&'a str, &'a str)> = HashSet::new();
    for (&src, tests) in source_to_tests.iter() {
        for &t in tests {
            seen.insert((src, t));
        }
    }

    for file in ctx.all_files {
        if !is_test_path(&file.path) {
            continue;
        }
        let abs = project_root.join(&file.path);
        let Ok(body) = std::fs::read_to_string(&abs) else {
            continue;
        };
        let mut literals: HashSet<String> = HashSet::new();
        for cap in re.captures_iter(&body) {
            if let Some(m) = cap.get(1) {
                literals.insert(m.as_str().to_ascii_lowercase());
            }
        }
        if literals.is_empty() {
            continue;
        }
        let test_path: &'a str = file.path.as_str();
        for literal in &literals {
            // Accept direct-stem and `<prefix>_<stem>` shapes so
            // dispatcher keys that namespace tool names (e.g.
            // `qartez_find` -> `find.rs`) resolve to the right file.
            let candidates: Vec<&str> = std::iter::once(literal.as_str())
                .chain(literal.split('_').skip(1).take(1))
                .collect();
            for cand in candidates {
                if let Some(sources) = stem_to_sources.get(cand) {
                    for &src_path in sources {
                        if seen.insert((src_path, test_path)) {
                            source_to_tests.entry(src_path).or_default().push(test_path);
                            test_to_sources.entry(test_path).or_default().push(src_path);
                        }
                    }
                }
            }
        }
    }
}

/// Coverage summary for a single source file. Drives the per-file
/// annotation surfaced by `qartez_context include_test_gaps=true` so
/// the compound flag uses the same coverage signal `qartez_test_gaps`
/// applies in `mode=gaps`.
#[derive(Debug, Default)]
pub(in crate::server) struct FileCoverage {
    /// Test files that directly import this source via the indexed
    /// edge graph.
    pub direct_test_paths: Vec<String>,
    /// True when the source file declares its own `#[cfg(test)]`
    /// or `#[test]` block. Inline-tested files are considered covered
    /// even when no external test file imports them.
    pub inline_rust_tests: bool,
    /// Test files whose body text mentions the source's Rust module
    /// stem. Catches Rust crate-rooted imports
    /// (`use <crate>::<stem>`) that the edge resolver cannot see.
    /// May overlap with `direct_test_paths`; the caller is responsible
    /// for deduping if a single line listing is desired.
    pub stem_mentioned_in_tests: Vec<String>,
}

impl FileCoverage {
    /// True when any of the three signals fire. Mirrors the coverage
    /// rule used by `test_gaps_find` so the answers stay consistent
    /// across both surfaces.
    pub(in crate::server) fn is_covered(&self) -> bool {
        !self.direct_test_paths.is_empty()
            || self.inline_rust_tests
            || !self.stem_mentioned_in_tests.is_empty()
    }
}

/// Compute coverage for `source_path` against the project. Public so
/// `qartez_context include_test_gaps=true` and other compound surfaces
/// can reuse the canonical signal without re-implementing it. The
/// caller is expected to have already locked the DB.
pub(in crate::server) fn coverage_for_source(
    conn: &rusqlite::Connection,
    project_root: &std::path::Path,
    source_path: &str,
) -> FileCoverage {
    let all_files = read::get_all_files(conn).unwrap_or_default();
    // Resolve the source row by path. A miss is reported as
    // FileCoverage::default() (untested) - same shape `qartez_context`
    // already uses for missing input rows in the ranked listing.
    let Some(file) = all_files.iter().find(|f| f.path == source_path) else {
        return FileCoverage::default();
    };

    let test_paths: HashSet<&str> = all_files
        .iter()
        .filter(|f| is_test_path(&f.path))
        .map(|f| f.path.as_str())
        .collect();

    // Direct edge importers that are themselves test files.
    let mut direct: Vec<String> = Vec::new();
    if let Ok(rows) = read::get_edges_to(conn, file.id) {
        for edge in rows {
            if let Ok(Some(importer)) = read::get_file_by_id(conn, edge.from_file)
                && is_test_path(&importer.path)
            {
                direct.push(importer.path);
            }
        }
    }
    direct.sort();
    direct.dedup();

    let inline = has_inline_rust_tests(project_root, &file.path);

    // FTS body-text fallback for crate-rooted Rust imports. Same
    // helper test_gaps_find uses; the answer is the list of test
    // file paths whose body contains the module stem.
    let mut stem_mentioned: Vec<String> = Vec::new();
    if let Some(stem) = rust_module_stem(&file.path)
        && let Ok(paths) = read::find_file_paths_by_body_text(conn, &stem)
    {
        for p in paths {
            if test_paths.contains(p.as_str()) {
                stem_mentioned.push(p);
            }
        }
    }
    stem_mentioned.sort();
    stem_mentioned.dedup();

    FileCoverage {
        direct_test_paths: direct,
        inline_rust_tests: inline,
        stem_mentioned_in_tests: stem_mentioned,
    }
}
