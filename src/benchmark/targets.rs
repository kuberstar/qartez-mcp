//! Resolves scenario targets (file paths, symbol names) from either a
//! [`LanguageProfile::target_override`] hook or a live `.qartez/` database.
//!
//! Before this refactor the benchmark hard-coded `src/server/mod.rs`,
//! `QartezServer`, `truncate_path`, etc. directly in `scenarios.rs`; now
//! each language profile either supplies an override (Rust) or relies on
//! the auto-resolver below (all others) to pick sensible targets from the
//! live index.
//!
//! [`LanguageProfile::target_override`]: super::profiles::LanguageProfile::target_override

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::profiles::LanguageProfile;
use crate::storage::read::{
    get_all_files_ranked, get_most_imported_files, get_symbols_for_file, get_unused_exports_page,
};

/// Maximum PageRank difference at which two files are considered to share
/// the same tier.
///
/// Languages whose indexer cannot emit cross-file edges (Java's imports
/// are fully qualified package paths that the tree-sitter walker does not
/// resolve to local files) produce a degenerate PageRank distribution
/// where every file lands at `1/N`. The auto-resolver needs a way to
/// break that tie deterministically — see the symbol-count fallback
/// below — and this epsilon decides how close two ranks need to be for
/// the tie-break to kick in.
const PAGERANK_TIE_EPSILON: f64 = 1e-9;

/// Upper bound on how many top-ranked files the target picker inspects.
///
/// When PageRank is flat across the entire index the picker would
/// otherwise walk every file, loading each file's symbols to compute
/// the tie-break. On pathological fixtures (jackson-core has 454
/// files, many of them non-Java scripts/docs interleaved with the
/// library source) the scan cap has to be large enough that the
/// primary-extension filter still finds library files in the first
/// pass. `1000` is safe for every fixture we benchmark.
const TOP_FILE_SCAN_CAP: usize = 1000;

/// Base name used for the synthetic destination file in the `qartez_move`
/// preview scenario.
///
/// The extension is taken from the active [`LanguageProfile`] so Python
/// fixtures get `helpers_benchmark_tmp.py` rather than the Rust-flavoured
/// `.rs` that the pre-fix code hard-coded.
const MOVE_DEST_BASENAME: &str = "helpers_benchmark_tmp";

/// Kind discriminators used when filtering the indexed `symbols` table
/// down to callable units.
///
/// Must stay in lockstep with [`crate::index::symbols::SymbolKind::as_str`]
/// — kinds are stored as lowercase strings in the database, so comparing
/// against `"Function"` (capital F) silently matches zero rows. The
/// bug used to be hidden by the Rust profile's `target_override`, which
/// bypassed this resolver entirely; it surfaced on the Python fixture
/// as a -1766% `qartez_read` regression and inspired these named
/// constants to prevent the same pitfall from creeping back in.
///
/// Java and Kotlin index body-carrying class members as
/// [`crate::index::symbols::SymbolKind::Method`], so [`CALLABLE_KINDS`]
/// accepts both `"function"` and `"method"`. Filtering on `FUNCTION_KIND`
/// alone misses every Java method and makes the auto-resolver fall
/// back to picking classes, which then swamp `qartez_read`/`qartez_calls`.
const FUNCTION_KIND: &str = "function";
const METHOD_KIND: &str = "method";
const CALLABLE_KINDS: &[&str] = &[FUNCTION_KIND, METHOD_KIND];

fn is_callable_kind(kind: &str) -> bool {
    CALLABLE_KINDS.contains(&kind)
}

/// Concrete targets for every scenario in the per-tool benchmark.
///
/// The Rust profile supplies these verbatim via `target_override` so the
/// pre-refactor baseline stays byte-identical. Other profiles pick them
/// from the live `.qartez` database at benchmark start-up.
#[derive(Debug, Clone)]
pub struct ResolvedTargets {
    /// File used as the outline / dependency / cochange / context target.
    /// For Rust this is `src/server/mod.rs`; for auto-resolved profiles it
    /// is the top PageRank file with at least one exported symbol.
    pub top_pagerank_file: String,
    /// Largest exported symbol inside `top_pagerank_file`.
    pub top_pagerank_symbol: String,
    /// Heavily-referenced symbol used as the `qartez_refs` target.
    pub most_referenced_symbol: String,
    /// Small exported function used for the `qartez_read` / `qartez_rename`
    /// preview scenarios. Small is preferred so the sim's Grep path does
    /// not dominate the byte count.
    pub smallest_exported_fn: String,
    /// Alias for `top_pagerank_file` used by the `qartez_outline` scenario.
    pub outline_target_file: String,
    /// Alias for `top_pagerank_file` used by the `qartez_deps` scenario.
    pub deps_target_file: String,
    /// File used as the `qartez_impact` target — a heavily-imported utility
    /// file so the blast radius and git co-change output have something
    /// meaningful to measure.
    pub impact_target_file: String,
    /// Module-path stem used to seed the non-MCP impact BFS. Rust uses
    /// `"storage::read"`; other languages use their equivalent import
    /// stem (e.g. `./storage/read` stripped of extension).
    pub impact_seed_stem: String,
    /// Profile-wide project manifest file (copied from the profile).
    pub project_file: String,
    /// Prefix used by the `qartez_grep` scenario and its non-MCP `Grep`
    /// equivalent. Chosen so that multiple indexed symbols match.
    pub grep_prefix: String,
    /// Symbol name used by the `qartez_move` preview scenario.
    pub move_symbol: String,
    /// Destination file path used by the `qartez_move` preview.
    pub move_destination: String,
    /// Source file path used by the `qartez_rename_file` preview scenario.
    pub rename_file_source: String,
    /// Destination file path used by the `qartez_rename_file` preview.
    pub rename_file_destination: String,
    /// Function name used by the `qartez_calls` scenario.
    pub calls_target_symbol: String,
    /// New identifier used by the `qartez_rename` preview. Separate from
    /// `smallest_exported_fn` so the Rust override can reproduce the
    /// historic `truncate_path → trunc_path` pair byte-for-byte.
    pub rename_new_name: String,
}

/// Resolves scenario targets against the live qartez database.
///
/// Called only when the profile does not supply a `target_override`.
/// Falls back to sensible defaults when the database is thin, so the
/// harness produces a runnable benchmark even on a mostly-empty fixture.
pub fn resolve(conn: &Connection, profile: &LanguageProfile) -> Result<ResolvedTargets> {
    // Compile the profile's exclude globs once so we can filter the
    // indexed files table — the qartez indexer happily walks everything
    // under the project root (tests, generated code, docs, etc.),
    // whereas the benchmark scenarios should operate on library code.
    // Without this filter the Java fixture picks `NumberParsingTest`
    // from `src/test/java/...` as the top-symbol file, which swamps the
    // `qartez_read` scenario with a 12k-token test body.
    let compiled_excludes: Vec<glob::Pattern> = profile
        .exclude_globs
        .iter()
        .filter_map(|g| glob::Pattern::new(g).ok())
        .collect();
    let is_excluded = |path: &str| -> bool {
        compiled_excludes
            .iter()
            .any(|pat| pat.matches(path))
    };

    // 1. Top file picker with two important safeguards:
    //
    //    a. Files with zero symbols are skipped. The TypeScript indexer
    //       occasionally emits a phantom file row for an aliased
    //       re-export (`export { default as frCA } from "./fr-CA.js"`
    //       yields a `frCA.ts` row with no symbols); without this
    //       filter every downstream scenario that needs a symbol list
    //       falls over on the phantom row.
    //
    //    b. When PageRank is flat across the top tier — the Java
    //       indexer cannot resolve `import tools.jackson.core.*` to a
    //       local file and therefore emits zero cross-file edges, so
    //       every file lands at `1/N` — we pick the file with the
    //       most symbols instead of relying on SQLite's insertion
    //       order. On jackson-core this promotes `ParserMinimalBase`
    //       (140 symbols) over `create-test-report.sh` (zero symbols).
    let all_ranked = get_all_files_ranked(conn)
        .with_context(|| "load ranked files for target picker")?;
    let first_ranked = all_ranked.first().with_context(|| {
        format!(
            "no ranked files in qartez database for profile {}",
            profile.name
        )
    })?;
    let top_rank = first_ranked.pagerank;
    let primary_ext_early = profile.extensions.first().copied().unwrap_or("");
    let path_has_primary_ext = |path: &str| -> bool {
        if primary_ext_early.is_empty() {
            return true;
        }
        path.rsplit_once('.')
            .map(|(_, ext)| ext == primary_ext_early)
            .unwrap_or(false)
    };
    let mut tier: Vec<(crate::storage::models::FileRow, usize)> = Vec::new();
    for f in all_ranked.iter().take(TOP_FILE_SCAN_CAP) {
        let in_top_tier = (f.pagerank - top_rank).abs() <= PAGERANK_TIE_EPSILON;
        if !in_top_tier && !tier.is_empty() {
            break;
        }
        if is_excluded(&f.path) {
            continue;
        }
        // Enforce the profile's primary extension on the outline /
        // deps / context target so languages with non-source files in
        // the top tier (Java's `release.sh`, Rust's top-level shell
        // scripts on monorepo fixtures) do not hijack scenarios that
        // only make sense against real source files.
        if !path_has_primary_ext(&f.path) {
            continue;
        }
        let sym_count = get_symbols_for_file(conn, f.id)
            .map(|syms| syms.len())
            .unwrap_or(0);
        if sym_count == 0 {
            continue;
        }
        tier.push((f.clone(), sym_count));
    }
    let top_file = tier
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(f, _)| f)
        .with_context(|| {
            format!(
                "no files with symbols in qartez database for profile {}",
                profile.name
            )
        })?;
    let top_file_path = top_file.path.clone();
    let top_file_id = top_file.id;

    // 2. Largest exported symbol inside the top file, plus a separate
    //    "largest exported Function" that fallbacks below use for any
    //    scenario that requires a callable symbol. Keeping these
    //    separate is what fixes Python `httpx/_models.py`: the largest
    //    exported symbol is the `Response` class, but the largest
    //    exported Function (`send`, `stream_lines`, etc.) is what
    //    `qartez_calls` and `qartez_read` actually want.
    let top_symbols = get_symbols_for_file(conn, top_file_id)
        .with_context(|| format!("load symbols for {top_file_path}"))?;
    let top_symbol_name = top_symbols
        .iter()
        .filter(|s| s.is_exported)
        .max_by_key(|s| s.line_end.saturating_sub(s.line_start))
        .or_else(|| top_symbols.first())
        .map(|s| s.name.clone())
        .with_context(|| format!("top file {top_file_path} has no symbols"))?;
    let top_function_name: Option<String> = top_symbols
        .iter()
        .filter(|s| s.is_exported && is_callable_kind(&s.kind))
        .max_by_key(|s| s.line_end.saturating_sub(s.line_start))
        .or_else(|| top_symbols.iter().find(|s| is_callable_kind(&s.kind)))
        .map(|s| s.name.clone());

    // 3. Most-referenced symbol: take the first most-imported file
    //    that is NOT excluded by the profile, then pick its largest
    //    exported symbol. When no non-excluded file has any incoming
    //    edges, reuse the top-PageRank symbol.
    let most_imported = get_most_imported_files(conn, 50)
        .with_context(|| "load most-imported files for target picker")?;
    let most_referenced_symbol = most_imported
        .into_iter()
        .find(|(file, _)| !is_excluded(&file.path))
        .and_then(|(file, _)| {
            let syms = get_symbols_for_file(conn, file.id).ok()?;
            syms.into_iter()
                .filter(|s| s.is_exported)
                .max_by_key(|s| s.line_end.saturating_sub(s.line_start))
                .map(|s| s.name)
        })
        .unwrap_or_else(|| top_symbol_name.clone());

    // 4. Genuinely-smallest exported function across the whole index.
    //
    //    The prior iteration of this picker walked the materialized
    //    `unused_exports` table and kept the first-seen candidate,
    //    which meant the "smallest" label was a lie — it was whichever
    //    function happened to be inserted first. On Python's httpx
    //    fixture this picked `main` (60+ lines) and `qartez_read` ran up
    //    a -485% "regression" because the non-MCP sim reads a fixed
    //    30-line slice that's smaller than the function body.
    //
    //    We now scan every indexed symbol once, filter to exported
    //    functions WHOSE NAME IS UNIQUE (so `qartez_move`/`qartez_rename`
    //    do not trip over "Multiple definitions of 'main' found"
    //    disambiguation errors), and keep the row with the tightest
    //    line span. The "unused" heuristic is preserved as a secondary
    //    filter for `move_symbol` because zero importers is still the
    //    safest subject for a hypothetical apply-mode run.
    let all_symbols = crate::storage::read::get_all_symbols_with_path(conn)
        .with_context(|| "load all symbols for target picker")?;
    let function_name_counts: std::collections::HashMap<&str, usize> = all_symbols
        .iter()
        .filter(|(s, _)| is_callable_kind(&s.kind))
        .fold(std::collections::HashMap::new(), |mut acc, (s, _)| {
            *acc.entry(s.name.as_str()).or_insert(0) += 1;
            acc
        });
    let is_unique_fn = |name: &str| -> bool {
        function_name_counts
            .get(name)
            .copied()
            .unwrap_or(0)
            == 1
    };
    let smallest_fn: Option<(String, String)> = all_symbols
        .iter()
        .filter(|(s, path)| {
            s.is_exported
                && is_callable_kind(&s.kind)
                && is_unique_fn(&s.name)
                && !is_excluded(path)
        })
        .min_by_key(|(s, _)| {
            (
                s.line_end.saturating_sub(s.line_start),
                s.name.chars().count(),
            )
        })
        .map(|(s, path)| (s.name.clone(), path.clone()));
    // Picks the smallest exported function with a unique name that
    // also has no importers — guarantees `qartez_move` can extract it
    // without touching any caller site. Falls through to `smallest_fn`
    // below when every unused function has an ambiguous name.
    let smallest_unused_fn: Option<(String, String)> = {
        let unused_rows = get_unused_exports_page(conn, 10_000, 0)
            .with_context(|| "load unused exported symbols")?;
        unused_rows
            .into_iter()
            .filter(|(sym, file)| {
                is_callable_kind(&sym.kind)
                    && is_unique_fn(&sym.name)
                    && !is_excluded(&file.path)
            })
            .min_by_key(|(sym, _)| {
                (
                    sym.line_end.saturating_sub(sym.line_start),
                    sym.name.chars().count(),
                )
            })
            .map(|(sym, file)| (sym.name, file.path))
    };

    // Cascade for `smallest_exported_fn`: prefer the truly smallest
    // exported function (tightest span), then fall back to the largest
    // exported function in the top file, then to any top symbol.
    let smallest_exported_fn = smallest_fn
        .as_ref()
        .map(|(n, _)| n.clone())
        .or_else(|| top_function_name.clone())
        .unwrap_or_else(|| top_symbol_name.clone());

    // 5. Move symbol + destination. Prefer an unused fn (no importers
    //    to rewrite even in a hypothetical apply run), and put the
    //    destination file next to its current parent. The extension
    //    comes from the active profile so Python fixtures get
    //    `helpers_benchmark_tmp.py` instead of the Rust-flavored `.rs`.
    let move_dest_ext = profile.extensions.first().copied().unwrap_or("rs");
    let move_dest_basename = format!("{MOVE_DEST_BASENAME}.{move_dest_ext}");
    let (move_symbol, move_destination) = match smallest_unused_fn
        .as_ref()
        .or(smallest_fn.as_ref())
    {
        Some((name, path)) => {
            let parent = parent_dir(path);
            let dest = if parent.is_empty() {
                move_dest_basename.clone()
            } else {
                format!("{parent}/{move_dest_basename}")
            };
            (name.clone(), dest)
        }
        None => (
            top_function_name
                .clone()
                .unwrap_or_else(|| top_symbol_name.clone()),
            move_dest_basename,
        ),
    };

    // 6. Rename-file source/destination: the file with the fewest
    //    incoming edges among ranked files, so an apply-mode run would be
    //    the safest. We only use the preview so correctness is less
    //    critical than determinism.
    // Two-axis filter for secondary file pickers:
    //
    //   a. `is_primary_ext` enforces the profile's first extension so
    //      the rename/impact scenarios operate on real source code.
    //      Cobra's `.github/workflows/stale.yml` is the concrete
    //      offender — picking a YAML file as `rename_file_source`
    //      makes `qartez_rename_file` fail with "Source file does not
    //      exist" because the tool enforces the profile's extension.
    //
    //   b. `is_real_file` rejects phantom rows created by the indexer
    //      for unresolved external imports. Go's `cobra/cmd/*.go`
    //      import targets land in the files table with
    //      `size_bytes == 0` because they do not exist on disk; left
    //      unfiltered they poison the rename_file picker with paths
    //      that canonicalize to missing files.
    let primary_ext = profile.extensions.first().copied().unwrap_or("");
    let is_primary_ext = |path: &str| -> bool {
        if primary_ext.is_empty() {
            return true;
        }
        path.rsplit_once('.')
            .map(|(_, ext)| ext == primary_ext)
            .unwrap_or(false)
    };
    let is_real_file = |f: &crate::storage::models::FileRow| -> bool {
        f.size_bytes > 0 && is_primary_ext(&f.path) && !is_excluded(&f.path)
    };
    let rename_file_source = all_ranked
        .iter()
        .rev()
        .find(|f| is_real_file(f))
        .map(|f| f.path.clone())
        .unwrap_or_else(|| top_file_path.clone());
    let rename_file_destination = renamed_destination(&rename_file_source);

    // 7. Impact target: pick the top-ranked file that is _not_ the same
    //    as the outline target, so the two scenarios do not overlap.
    //    Reuses the same two-axis filter.
    let impact_target_file = all_ranked
        .iter()
        .find(|f| f.path != top_file_path && is_real_file(f))
        .map(|f| f.path.clone())
        .unwrap_or_else(|| top_file_path.clone());
    let impact_seed_stem = path_to_import_stem(&impact_target_file);

    // 8. Grep prefix: first 6 characters of the top symbol's name, lower
    //    bound clamped to at least 3 to avoid FTS5 prefix explosions.
    let grep_prefix = {
        let name = &top_symbol_name;
        // char-safe take: use `chars().take(n)` to avoid slicing mid-codepoint
        let want = name.chars().count().min(6).max(3.min(name.chars().count()));
        name.chars().take(want).collect::<String>()
    };

    // 9. Calls target: reuse `top_function_name`, which is already
    //    computed via the same "largest exported Function in the top
    //    file" selector. Falls back to the top symbol of any kind when
    //    the top file exports zero functions.
    let calls_target_symbol = top_function_name
        .clone()
        .unwrap_or_else(|| top_symbol_name.clone());

    // 10. Rename-preview target: append `_renamed` to the smallest
    //     exported fn. Auto-resolved profiles use a simple deterministic
    //     suffix so the preview scenario is reproducible; the Rust
    //     profile overrides this with the historic `trunc_path` value.
    let rename_new_name = format!("{smallest_exported_fn}_renamed");

    Ok(ResolvedTargets {
        top_pagerank_file: top_file_path.clone(),
        top_pagerank_symbol: top_symbol_name,
        most_referenced_symbol,
        smallest_exported_fn,
        outline_target_file: top_file_path.clone(),
        deps_target_file: top_file_path.clone(),
        impact_target_file,
        impact_seed_stem,
        project_file: profile.project_file.to_string(),
        grep_prefix,
        move_symbol,
        move_destination,
        rename_file_source,
        rename_file_destination,
        calls_target_symbol,
        rename_new_name,
    })
}

fn parent_dir(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((parent, _)) => parent.to_string(),
        None => String::new(),
    }
}

fn renamed_destination(path: &str) -> String {
    // Strip the last path component's extension and append `_renamed`.
    // Used by the `qartez_rename_file` preview, which never applies, so any
    // unique-looking destination is acceptable.
    let (parent, file) = match path.rsplit_once('/') {
        Some((p, f)) => (p.to_string(), f.to_string()),
        None => (String::new(), path.to_string()),
    };
    let (stem, ext) = match file.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (file.clone(), String::new()),
    };
    let renamed = format!("{stem}_renamed{ext}");
    if parent.is_empty() {
        renamed
    } else {
        format!("{parent}/{renamed}")
    }
}

fn path_to_import_stem(path: &str) -> String {
    // `src/storage/read.rs` -> `storage::read`. Used for the non-MCP
    // impact BFS seed. Non-Rust languages produce something similar
    // minus the extension.
    let without_src = path.strip_prefix("src/").unwrap_or(path);
    let base = without_src
        .rsplit_once('.')
        .map(|(b, _)| b)
        .unwrap_or(without_src);
    let stem = base.replace('/', "::");
    stem.strip_suffix("::mod")
        .map(str::to_string)
        .unwrap_or(stem)
}
