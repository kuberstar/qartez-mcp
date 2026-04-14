//! Per-tool benchmark scenarios.
//!
//! Each scenario pairs one Qartez MCP tool invocation with the closest
//! non-MCP workflow (a sequence of `SimStep`s executed by `sim_runner`).
//! The MCP args and sim steps are function pointers because
//! `serde_json::json!` allocates and can't run in `const` context.
//!
//! Scenarios are language-agnostic: every hard-coded file path or symbol
//! name has been replaced with a field from [`ResolvedTargets`], and every
//! extension filter is drawn from the active [`LanguageProfile`]. The
//! Rust profile's `target_override` supplies the exact pre-refactor
//! values so the committed `reports/benchmark.{json,md}` baseline stays
//! byte-identical for `--lang rust`.

use serde_json::{Value, json};

use super::profiles::LanguageProfile;
use super::targets::ResolvedTargets;

/// One step in the non-MCP simulation pipeline.
///
/// Each step corresponds to a tool call a Claude Code agent would make when
/// working without the Qartez MCP server. The output of every step is
/// concatenated into a single buffer that represents everything the agent
/// would have had to read.
#[derive(Debug, Clone)]
pub enum SimStep {
    /// `Glob` equivalent: list files under the project root. When
    /// `ext_filter` is `Some`, only files whose extension matches one of
    /// the listed suffixes (without leading dot) are included.
    Glob { ext_filter: Option<Vec<String>> },
    /// `Grep` in `files_with_matches` mode: one line per matching file.
    GrepFiles {
        regex: String,
        ext_filter: Option<Vec<String>>,
    },
    /// `Grep` in `content` mode: one line per matching line, with
    /// `file:line:content` prefix.
    GrepContent {
        regex: String,
        ext_filter: Option<Vec<String>>,
    },
    /// `Read` equivalent: dump a file with `cat -n`-style line numbers.
    /// An optional inclusive 1-based `(start, end)` range restricts the
    /// slice.
    Read {
        path: String,
        range: Option<(usize, usize)>,
    },
    /// Shell out to `git log` for co-change / history-mining scenarios
    /// where a real non-MCP agent would run the same command.
    GitLog { file: String, limit: u32 },
    /// Representative byte padding used when the non-MCP path would
    /// require many more tool calls than the harness can faithfully
    /// reproduce (e.g. per-symbol follow-up greps in `qartez_unused`). The
    /// scenario verdict explains what the bytes stand for.
    BashOutput { bytes: usize },
    /// BFS over `use crate::*` imports: grep once for the seed symbol,
    /// then re-grep for each discovered importer's own crate stem,
    /// recursing up to `depth` levels. Used by the `qartez_impact` sim to
    /// faithfully reproduce what a non-MCP agent would do when chasing
    /// transitive importers.
    ImpactBfs {
        seed: String,
        depth: u32,
        ext_filter: Option<Vec<String>>,
    },
    /// `git log --name-only --pretty=format:%H -n{limit}` across the
    /// whole repo (no file filter), aggregated in-process into co-change
    /// pair counts for `target_file`, top `top_n` partners printed.
    /// Matches the "real" cost a non-MCP agent pays to answer co-change
    /// questions.
    GitCoChange {
        target_file: String,
        limit: u32,
        top_n: u32,
    },
}

impl SimStep {
    /// Short descriptor used in the JSON/Markdown report for each step.
    pub fn describe(&self) -> String {
        match self {
            Self::Glob { ext_filter } => match ext_filter.as_deref() {
                Some([]) | None => "Glob **/*".to_string(),
                Some([ext]) => format!("Glob **/*.{ext}"),
                Some(exts) => format!("Glob **/*.{{{}}}", exts.join(",")),
            },
            Self::GrepFiles { regex, ext_filter } => match ext_filter.as_deref() {
                Some([]) | None => format!("Grep --files-with-matches /{regex}/"),
                Some([ext]) => format!("Grep --files-with-matches /{regex}/ (*.{ext})"),
                Some(exts) => format!(
                    "Grep --files-with-matches /{regex}/ (*.{{{}}})",
                    exts.join(",")
                ),
            },
            Self::GrepContent { regex, ext_filter } => match ext_filter.as_deref() {
                Some([]) | None => format!("Grep /{regex}/"),
                Some([ext]) => format!("Grep /{regex}/ (*.{ext})"),
                Some(exts) => format!("Grep /{regex}/ (*.{{{}}})", exts.join(",")),
            },
            Self::Read { path, range } => match range {
                Some((s, e)) => format!("Read {path} lines {s}-{e}"),
                None => format!("Read {path}"),
            },
            Self::GitLog { file, limit } => format!("git log -n{limit} -- {file}"),
            Self::BashOutput { bytes } => format!("≈{bytes}B representative padding"),
            Self::ImpactBfs { seed, depth, .. } => {
                format!("BFS grep from '{seed}' (depth {depth})")
            }
            Self::GitCoChange {
                target_file,
                limit,
                top_n,
            } => format!(
                "git log -n{limit} --name-only + pair counts for {target_file} (top {top_n})"
            ),
        }
    }
}

/// One benchmark scenario: MCP tool + non-MCP equivalent + hand-authored
/// verdict.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub tool: &'static str,
    pub id: &'static str,
    pub description: &'static str,
    /// Builds the MCP tool args for this scenario. Takes the active
    /// [`ResolvedTargets`] so paths and symbol names can come from the
    /// language profile instead of being hard-coded.
    pub mcp_args: fn(&ResolvedTargets, &LanguageProfile) -> Value,
    /// Builds the non-MCP step sequence for this scenario. Takes the
    /// active [`ResolvedTargets`] and [`LanguageProfile`] so extension
    /// filters, file paths, and symbol names can come from the profile
    /// instead of being hard-coded.
    pub non_mcp_steps: fn(&ResolvedTargets, &LanguageProfile) -> Vec<SimStep>,
    pub pros: &'static [&'static str],
    pub cons: &'static [&'static str],
    pub verdict_summary: &'static str,
    /// Whether the non-MCP step sequence actually produces a semantically
    /// complete answer to the same question the MCP tool answers.
    ///
    /// When `false`, comparing raw token / byte counts is misleading
    /// because the non-MCP side is simply missing data (e.g.
    /// `qartez_cochange`'s non-MCP path emits a `git log --name-only`
    /// stream that contains the queried file repeated N times but no
    /// pair information at all). The winner selector in
    /// `report::pick_winner` awards MCP regardless of the token delta in
    /// this case, and the Markdown renderer annotates the row so a
    /// reader understands why the smaller non-MCP output is not a win.
    pub non_mcp_is_complete: bool,
    /// Golden reference answer for the judge rubric. When `Some`, the
    /// text is injected under `GOLDEN ANSWER` in `build_prompt` per
    /// `docs/benchmark/judge-core.md` §3; when `None`, the prompt
    /// falls back to "no golden answer was provided — judge against the
    /// rubric alone". Prometheus-style references lift judge-to-human
    /// Pearson correlation from 0.392 to 0.897 (research.md §6), which
    /// makes them the single biggest lever in rubric accuracy. Phase 4
    /// ships every scenario with `None`; hand-authoring references is
    /// a follow-up chase item per PLAN.md §2.2.
    pub reference_answer: Option<&'static str>,
    /// Scenario tier: 1 = default (ships with every run), 2+ = edge cases
    /// gated behind `--tier N`.
    pub tier: u8,
}

// -- Scenario arg/step functions --------------------------------------------
//
// Each tool gets a pair of `fn(…) -> …` functions rather than a single
// const value because `json!` and `vec!` both allocate. Every function
// now derives its file paths and symbol names from `ResolvedTargets` and
// its extension filter from `LanguageProfile::extensions`, so one set of
// scenarios covers every language the harness knows about.

/// Convenience helper: returns the profile's extensions as an owned
/// `Vec<String>` wrapped in `Some(..)`, or `None` when the profile lists
/// zero extensions (which currently never happens but is handled for
/// forward compatibility).
fn ext_filter_of(profile: &LanguageProfile) -> Option<Vec<String>> {
    if profile.extensions.is_empty() {
        None
    } else {
        Some(
            profile
                .extensions
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        )
    }
}

// 1. qartez_map -----------------------------------------------------------
fn map_args(_targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "top_n": 5, "format": "concise" })
}
fn map_steps(_targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    // To produce the "top 5 most important files" answer without
    // PageRank, a non-MCP agent can at best grep for all source files
    // and then read each to estimate connectivity. This represents step
    // 1 of that workflow.
    vec![SimStep::Glob {
        ext_filter: ext_filter_of(profile),
    }]
}

// 2. qartez_find ----------------------------------------------------------
fn find_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "name": targets.top_pagerank_symbol })
}
fn find_steps(targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    vec![
        SimStep::GrepContent {
            regex: format!(r"struct\s+{}", targets.top_pagerank_symbol),
            ext_filter: ext_filter_of(profile),
        },
        SimStep::Read {
            path: targets.top_pagerank_file.clone(),
            range: Some((1, 120)),
        },
    ]
}

// 3. qartez_read ----------------------------------------------------------
fn read_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "symbol_name": targets.smallest_exported_fn })
}
fn read_steps(targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    vec![
        SimStep::GrepContent {
            regex: format!(r"fn {}", targets.smallest_exported_fn),
            ext_filter: ext_filter_of(profile),
        },
        SimStep::Read {
            path: targets.outline_target_file.clone(),
            range: Some((260, 290)),
        },
    ]
}

// 4. qartez_grep ----------------------------------------------------------
fn grep_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "query": format!("{}*", targets.grep_prefix) })
}
fn grep_steps(targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    vec![SimStep::GrepContent {
        regex: targets.grep_prefix.clone(),
        ext_filter: ext_filter_of(profile),
    }]
}

// 5. qartez_outline -------------------------------------------------------
fn outline_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "file_path": targets.outline_target_file })
}
fn outline_steps(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Vec<SimStep> {
    vec![SimStep::Read {
        path: targets.outline_target_file.clone(),
        range: None,
    }]
}

// 6. qartez_deps ----------------------------------------------------------
fn deps_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "file_path": targets.deps_target_file })
}
fn deps_steps(_targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    vec![
        SimStep::GrepContent {
            regex: r"^use crate::".to_string(),
            ext_filter: ext_filter_of(profile),
        },
        SimStep::GrepContent {
            regex: r"use crate::server".to_string(),
            ext_filter: ext_filter_of(profile),
        },
    ]
}

// 7. qartez_refs ----------------------------------------------------------
fn refs_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "symbol": targets.most_referenced_symbol })
}
fn refs_steps(targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    vec![SimStep::GrepContent {
        regex: targets.most_referenced_symbol.clone(),
        ext_filter: ext_filter_of(profile),
    }]
}

// 8. qartez_impact --------------------------------------------------------
fn impact_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "file_path": targets.impact_target_file, "include_tests": false })
}
fn impact_steps(targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    vec![
        SimStep::ImpactBfs {
            seed: targets.impact_seed_stem.clone(),
            depth: 2,
            ext_filter: ext_filter_of(profile),
        },
        SimStep::GitCoChange {
            target_file: targets.impact_target_file.clone(),
            limit: 100,
            top_n: 10,
        },
    ]
}

// 9. qartez_cochange ------------------------------------------------------
fn cochange_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "file_path": targets.outline_target_file, "limit": 5, "max_commit_size": 30 })
}
fn cochange_steps(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Vec<SimStep> {
    vec![SimStep::GitCoChange {
        target_file: targets.outline_target_file.clone(),
        limit: 200,
        top_n: 5,
    }]
}

// 10. qartez_unused -------------------------------------------------------
fn unused_args(_targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({})
}
fn unused_steps(_targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    // Step 1 of the real non-MCP workflow: find all pub declarations.
    // A faithful emulation would then grep each name across the
    // codebase, which would amount to hundreds more tool calls —
    // represented by a BashOutput padding line in the verdict commentary
    // but not here, because inflating the baseline would be unfair. The
    // single grep is already the honest lower bound for a non-MCP
    // agent.
    vec![SimStep::GrepContent {
        regex: r"^pub (fn|struct|enum|trait|const)".to_string(),
        ext_filter: ext_filter_of(profile),
    }]
}

// 11. qartez_stats --------------------------------------------------------
fn stats_args(_targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({})
}
fn stats_steps(_targets: &ResolvedTargets, _profile: &LanguageProfile) -> Vec<SimStep> {
    vec![SimStep::Glob { ext_filter: None }]
}

// 12. qartez_calls --------------------------------------------------------
fn calls_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    // Default depth is now 1 (post-2026-04 compaction). depth=2 remains
    // opt-in and can still explode on hub functions — measuring default
    // behavior gives the honest steady-state cost.
    json!({ "name": targets.calls_target_symbol })
}
fn calls_steps(targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    vec![
        SimStep::GrepContent {
            regex: targets.calls_target_symbol.clone(),
            ext_filter: ext_filter_of(profile),
        },
        SimStep::Read {
            path: targets.outline_target_file.clone(),
            range: Some((46, 180)),
        },
    ]
}

// 13. qartez_context ------------------------------------------------------
fn context_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "files": [targets.outline_target_file.clone()], "limit": 5 })
}
fn context_steps(targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    // A non-MCP agent would reconstruct this by: finding imports,
    // finding importers, then mining cochange history. We emulate the
    // three sources.
    vec![
        SimStep::GrepContent {
            regex: r"use crate::".to_string(),
            ext_filter: ext_filter_of(profile),
        },
        SimStep::GrepContent {
            regex: r"use crate::server".to_string(),
            ext_filter: ext_filter_of(profile),
        },
        SimStep::GitLog {
            file: targets.outline_target_file.clone(),
            limit: 50,
        },
    ]
}

// 14. qartez_rename -------------------------------------------------------
fn rename_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({
        "old_name": targets.smallest_exported_fn,
        "new_name": targets.rename_new_name,
        "apply": false,
    })
}
fn rename_steps(targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    vec![SimStep::GrepContent {
        regex: format!(r"\b{}\b", targets.smallest_exported_fn),
        ext_filter: ext_filter_of(profile),
    }]
}

// 15. qartez_move ---------------------------------------------------------
fn move_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({
        "symbol": targets.move_symbol,
        "to_file": targets.move_destination,
        "apply": false,
    })
}
fn move_steps(targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    vec![
        SimStep::GrepContent {
            regex: format!(r"fn {}", targets.move_symbol),
            ext_filter: ext_filter_of(profile),
        },
        SimStep::Read {
            path: targets.outline_target_file.clone(),
            range: Some((2180, 2225)),
        },
        SimStep::GrepContent {
            regex: format!(r"\b{}\b", targets.move_symbol),
            ext_filter: ext_filter_of(profile),
        },
    ]
}

// 16. qartez_rename_file --------------------------------------------------
fn rename_file_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({
        "from": targets.rename_file_source,
        "to": targets.rename_file_destination,
        "apply": false,
    })
}
fn rename_file_steps(_targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    vec![SimStep::GrepContent {
        regex: r"crate::server".to_string(),
        ext_filter: ext_filter_of(profile),
    }]
}

// 17. qartez_project ------------------------------------------------------
fn project_args(_targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "action": "info" })
}
fn project_steps(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Vec<SimStep> {
    vec![SimStep::Read {
        path: targets.project_file.clone(),
        range: None,
    }]
}

// -- Tier-2 scenario arg/step functions -------------------------------------

// T2-1. qartez_find_nonexistent
fn find_nonexistent_args(_targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "name": "ThisSymbolDoesNotExist__xyz" })
}
fn find_nonexistent_steps(_targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    vec![SimStep::GrepContent {
        regex: "ThisSymbolDoesNotExist__xyz".to_string(),
        ext_filter: ext_filter_of(profile),
    }]
}

// T2-2. qartez_grep_regex
fn grep_regex_args(_targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "query": "^handle_.*request$", "regex": true })
}
fn grep_regex_steps(_targets: &ResolvedTargets, profile: &LanguageProfile) -> Vec<SimStep> {
    vec![SimStep::GrepContent {
        regex: r"^handle_.*request$".to_string(),
        ext_filter: ext_filter_of(profile),
    }]
}

// T2-3. qartez_read_whole_file
fn read_whole_file_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "file_path": targets.project_file })
}
fn read_whole_file_steps(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Vec<SimStep> {
    vec![SimStep::Read {
        path: targets.project_file.clone(),
        range: None,
    }]
}

// T2-4. qartez_outline_small_file
fn outline_small_file_args(targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "file_path": targets.impact_target_file })
}
fn outline_small_file_steps(
    targets: &ResolvedTargets,
    _profile: &LanguageProfile,
) -> Vec<SimStep> {
    vec![SimStep::Read {
        path: targets.impact_target_file.clone(),
        range: None,
    }]
}

// T2-5. qartez_unused_with_limit
fn unused_with_limit_args(_targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "limit": 5 })
}
fn unused_with_limit_steps(
    _targets: &ResolvedTargets,
    profile: &LanguageProfile,
) -> Vec<SimStep> {
    vec![SimStep::GrepContent {
        regex: r"^pub (fn|struct|enum|trait|const)".to_string(),
        ext_filter: ext_filter_of(profile),
    }]
}

// T2-6. qartez_impact_nonexistent
fn impact_nonexistent_args(_targets: &ResolvedTargets, _profile: &LanguageProfile) -> Value {
    json!({ "file_path": "src/this_file_does_not_exist.rs" })
}
fn impact_nonexistent_steps(
    _targets: &ResolvedTargets,
    profile: &LanguageProfile,
) -> Vec<SimStep> {
    vec![SimStep::GrepFiles {
        regex: "this_file_does_not_exist".to_string(),
        ext_filter: ext_filter_of(profile),
    }]
}

/// The full per-tool scenario matrix. 17 tier-1 entries + 6 tier-2 entries.
///
/// Pros/cons/verdicts are authored by hand — they encode judgment the
/// harness can't infer from raw bytes, and they're what makes the final
/// report useful beyond a numeric matrix.
pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        tool: "qartez_map",
        id: "qartez_map_top5_concise",
        description: "Rank the top 5 files by PageRank (concise format).",
        mcp_args: map_args,
        non_mcp_steps: map_steps,
        pros: &[
            "PageRank-based importance ranking",
            "Blast radius column in one call",
            "Elided source previews of exports",
            "Token-budgeted output",
            "`all_files: true` (or `top_n: 0`) returns every file PageRank-sorted",
        ],
        cons: &[
            "Cannot return raw file contents",
            "Requires .qartez/ to be built",
        ],
        verdict_summary: "MCP wins overwhelmingly: non-MCP path produces only a flat file list and still requires reading every file to approximate ranking.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_find",
        id: "qartez_find_struct_qartezserver",
        description: "Locate the QartezServer struct definition.",
        mcp_args: find_args,
        non_mcp_steps: find_steps,
        pros: &[
            "Pre-indexed exact line range (no brace counting)",
            "Signature pre-extracted",
            "Kind filter, export-status flag",
            "`regex: true` walks the indexed symbol table for pattern matches",
        ],
        cons: &[
            "Only the definition site, not usages",
            "Misses macro-synthesized symbols — tree-sitter opaque-tokens the macro body, so `lazy_static! { pub static ref FOO }` doesn't surface `FOO` in the index",
        ],
        verdict_summary: "MCP wins on precision and compactness, but the non-MCP path is viable for unique symbol names.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_read",
        id: "qartez_read_truncate_path",
        description: "Read the body of the `truncate_path` helper.",
        mcp_args: read_args,
        non_mcp_steps: read_steps,
        pros: &[
            "Jumps directly to the symbol by indexed line range",
            "No brace counting or over-reading",
            "Numbered output matches Read semantics",
            "`start_line` / `end_line` support raw line-range reads for non-symbol code (imports, file headers)",
            "`context_lines` opt-in surrounding lines (default 0)",
        ],
        cons: &["Line-range mode requires knowing the target file up-front"],
        verdict_summary: "MCP wins decisively — a non-MCP agent must over-read to guarantee body coverage.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_grep",
        id: "qartez_grep_find_symbol_prefix",
        description: "FTS5 prefix search for symbols starting with `find_symbol`.",
        mcp_args: grep_args,
        non_mcp_steps: grep_steps,
        pros: &[
            "Searches indexed symbols only — no comment/string noise",
            "Prefix matching via FTS5",
            "Returns kind + line range per hit",
            "`regex: true` falls back to in-memory regex over indexed symbol names",
            "`search_bodies: true` hits a pre-indexed FTS5 body table for text inside function bodies",
        ],
        cons: &[
            "Only indexed languages",
            "Body FTS storage grows ~1-2× the codebase when `search_bodies` is used",
        ],
        verdict_summary: "MCP wins on signal-to-noise; grep still wins when you need to find text inside bodies.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_outline",
        id: "qartez_outline_server_mod",
        description: "Outline all symbols in src/server/mod.rs grouped by kind.",
        mcp_args: outline_args,
        non_mcp_steps: outline_steps,
        pros: &[
            "Symbols pre-grouped by kind",
            "Signatures pre-parsed",
            "Token-budgeted — no full-file read",
            "Struct fields are emitted as child rows nested under their parent",
        ],
        cons: &[
            "Token budget truncates very large files",
            "Tuple-struct members are skipped — nothing meaningful to name",
        ],
        verdict_summary: "MCP wins by ~20x on a 2300-line file; non-MCP path reads the entire file.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_deps",
        id: "qartez_deps_server_mod",
        description: "Show incoming/outgoing file-level dependencies for src/server/mod.rs.",
        mcp_args: deps_args,
        non_mcp_steps: deps_steps,
        pros: &[
            "Edges pre-resolved at index time",
            "Bidirectional (imports + importers) in one call",
        ],
        cons: &[
            "Flattens to file-level — loses per-symbol specifier",
            "Doesn't show which items are imported",
        ],
        verdict_summary: "MCP wins on accuracy (resolved paths) and compactness.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_refs",
        id: "qartez_refs_find_symbol_by_name",
        description: "List references to find_symbol_by_name.",
        mcp_args: refs_args,
        non_mcp_steps: refs_steps,
        pros: &[
            "Specifier-aware filtering (vs raw text)",
            "Optional transitive BFS",
            "Def + uses in one response",
            "AST-resolved call sites now listed alongside file-edge importers",
        ],
        cons: &[
            "Misses dynamic dispatch: trait-object calls resolve at runtime and leave no static edge or call-site name the tree-sitter walker can anchor to",
            "Transitive BFS can balloon on hub symbols — a symbol whose file is imported by 50 crates yields 50 * avg-fanout rows",
        ],
        verdict_summary: "MCP now surfaces both file-level edges and AST call sites, closing the gap with grep while keeping the tree-sitter precision that skips strings and comments.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_impact",
        id: "qartez_impact_storage_read",
        description: "Blast radius and co-change partners for src/storage/read.rs.",
        mcp_args: impact_args,
        non_mcp_steps: impact_steps,
        pros: &[
            "Combines static blast radius + git co-change in one call",
            "Transitive BFS pre-computed",
            "`include_tests: false` by default excludes test modules from the blast radius",
        ],
        cons: &["Co-change is statistical, not causal"],
        verdict_summary: "MCP wins on correctness and output size: the non-MCP equivalent is a 2-level import BFS plus a full git-log mine with in-process pair counting — reproducible now that the sim matches what a real agent would actually have to do.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_cochange",
        id: "qartez_cochange_server_mod",
        description: "Top 5 co-change partners for src/server/mod.rs.",
        mcp_args: cochange_args,
        non_mcp_steps: cochange_steps,
        pros: &[
            "Pre-computed pair counts",
            "Instant response",
            "`max_commit_size` arg skips huge refactor commits (default 30) when recomputing from git",
        ],
        cons: &["Depends on git-history granularity"],
        verdict_summary: "MCP wins on tokens and latency: the non-MCP sim now faithfully reproduces the full git log mine + in-process pair counting that an agent would have to run by hand.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_unused",
        id: "qartez_unused_whole_repo",
        description: "Find all unused exported symbols across the repo.",
        mcp_args: unused_args,
        non_mcp_steps: unused_steps,
        pros: &[
            "Pre-materialized at index time — query is a single indexed SELECT",
            "`limit` / `offset` pagination (default 50) keeps default output small",
            "Trait-impl methods are excluded at parse time via the `unused_excluded` flag",
        ],
        cons: &[
            "Requires human filtering before action — dynamic dispatch callers don't register as static importers",
            "Pre-materialization is invalidated wholesale on re-index; a stale index may miss recently-added imports",
        ],
        verdict_summary: "MCP wins massively: the non-MCP step captured here is only the candidate list — a real agent would need hundreds more greps.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_stats",
        id: "qartez_stats_basic",
        description: "Overall codebase statistics and language breakdown.",
        mcp_args: stats_args,
        non_mcp_steps: stats_steps,
        pros: &[
            "Single-call summary",
            "Most-connected-files list included",
            "Optional `file_path` arg drills into per-file LOC / symbol / importer counts",
            "Aggregate output splits src and test files/LOC",
        ],
        cons: &[
            "Test/src split is filename-based (`tests/`, `_test.rs`, `benches/`) — not build-graph-aware, so integration tests wired via Cargo `test` target live in `src/` look like production code",
            "Language buckets count files-only; weighted metrics (bytes, symbols per language) aren't broken out",
        ],
        verdict_summary: "MCP wins — non-MCP glob dump requires external aggregation just to count files.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_calls",
        id: "qartez_calls_build_overview",
        description: "Call hierarchy for build_overview (default depth=1).",
        mcp_args: calls_args,
        non_mcp_steps: calls_steps,
        pros: &[
            "Tree-sitter AST distinguishes calls from references",
            "Callees resolved to definition file:line",
            "Transitive depth available (default 1, opt-in depth=2)",
            "Per-session parse cache — repeat invocations are in-memory",
        ],
        cons: &[
            "Misses dynamic dispatch — trait-object `Box<dyn Foo>` calls leave no static call-site name",
            "Depth=2 output can still balloon on hub functions; the grouping elision helps but the graph is inherently O(N^depth)",
        ],
        verdict_summary: "MCP wins on correctness (tree-sitter distinguishes call sites from type-position references) and tokens. A per-invocation parse cache and a textual pre-filter keep the AST walk from re-visiting files that cannot possibly contain the callee, so the cold-parse cost stays in the low milliseconds.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_context",
        id: "qartez_context_server_mod",
        description: "Top 5 related files for src/server/mod.rs.",
        mcp_args: context_args,
        non_mcp_steps: context_steps,
        pros: &[
            "Multi-signal scoring: deps + cochange + PageRank + task FTS",
            "Reason tags explain every row",
        ],
        cons: &[
            "Opaque composite score",
            "Cannot answer 'why was X excluded'",
        ],
        verdict_summary: "MCP wins — no single Grep/Read chain can approximate the composite ranking.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_rename",
        id: "qartez_rename_truncate_path_preview",
        description: "Preview rename of truncate_path → trunc_path (no apply).",
        mcp_args: rename_args,
        non_mcp_steps: rename_steps,
        pros: &[
            "Tree-sitter identifier matching skips strings/comments",
            "Atomic apply with word-boundary fallback",
            "Preview + apply in one API",
            "Correctly handles aliased imports (`use foo::bar as baz`) — enshrined by a unit test",
        ],
        cons: &["Only indexed languages"],
        verdict_summary: "MCP wins on tokens and safety. The AST-based identifier match on a 2300-line file runs in the low single-digit milliseconds — slower than a raw grep but the cost buys correct skipping of strings, comments, and same-spelled but unrelated identifiers.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_move",
        id: "qartez_move_capitalize_kind_preview",
        description: "Preview moving the `capitalize_kind` helper to a new file.",
        mcp_args: move_args,
        non_mcp_steps: move_steps,
        pros: &[
            "Atomic extraction + insertion + importer rewriting",
            "Refuses on ambiguous symbols",
            "Refuses when destination already defines a same-kind same-name symbol",
            "Importer rewriting uses regex word boundaries (no substring over-match)",
        ],
        cons: &[
            "Does not rewrite doc-comment references like `[`foo::bar`]` — the rewriter targets `use` paths and qualified call sites only",
            "Ambiguity check is by symbol name, not by fully-qualified path — a free function `foo` and a method `foo` on a struct both count as ambiguous even when only one matches the move target kind",
        ],
        verdict_summary: "MCP wins — non-MCP path is a 3-step sequence with several edit-time pitfalls.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_rename_file",
        id: "qartez_rename_file_server_mod_preview",
        description: "Preview renaming src/server/mod.rs → src/server/server.rs.",
        mcp_args: rename_file_args,
        non_mcp_steps: rename_file_steps,
        pros: &[
            "Atomic mv + import path rewriting",
            "Handles mod.rs → named-module transform",
            "Regex word-boundary matching prevents over-match on import stems",
        ],
        cons: &[
            "`mod.rs → named.rs` edge case: the parent module's `mod foo;` declaration is *not* rewritten because the rename tool only touches files that import via `use crate::foo::…`, not files that declare the module",
            "Doc links (`[`crate::foo`]` in `///` comments) aren't rewritten — the rewriter is limited to `use`-path tokens",
        ],
        verdict_summary: "MCP wins for single-shot refactors; non-MCP path produces correct results only with careful scoping.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    Scenario {
        tool: "qartez_project",
        id: "qartez_project_info",
        description: "Detect the toolchain and report its commands.",
        mcp_args: project_args,
        non_mcp_steps: project_steps,
        pros: &[
            "Auto-detection across 5+ ecosystems (Cargo, npm/bun, Go, Python, Make)",
            "Consistent output format",
            "`run` action resolves a subcommand to its shell form without executing",
        ],
        cons: &[
            "Detects toolchain by file presence only — `Cargo.toml` / `package.json` / `go.mod` existing is enough, the tool never runs a probe command to confirm the toolchain actually works",
            "No polyglot support — a Rust + Node monorepo resolves to the first detected toolchain (Cargo wins, Node is silent), so callers need to scope to a sub-directory to see the other",
        ],
        verdict_summary: "Near-tie: reading Cargo.toml is already cheap; MCP wins only on portability across ecosystems.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 1,
    },
    // -- Tier-2 edge-case scenarios -----------------------------------------
    Scenario {
        tool: "qartez_find",
        id: "qartez_find_nonexistent",
        description: "Search for a symbol that does not exist. Validates empty-result handling.",
        mcp_args: find_nonexistent_args,
        non_mcp_steps: find_nonexistent_steps,
        pros: &["Graceful empty result"],
        cons: &["No faster than grep for zero matches"],
        verdict_summary: "Validates error/empty path — both sides should return empty or a clear 'not found' message.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 2,
    },
    Scenario {
        tool: "qartez_grep",
        id: "qartez_grep_regex",
        description: "Regex search for `^handle_.*request$`. Tests regex mode correctness.",
        mcp_args: grep_regex_args,
        non_mcp_steps: grep_regex_steps,
        pros: &["Regex support against indexed symbol names"],
        cons: &["Regex anchors match symbol names, not full lines"],
        verdict_summary: "Tests regex handling — MCP searches indexed symbols while non-MCP greps raw lines.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 2,
    },
    Scenario {
        tool: "qartez_read",
        id: "qartez_read_whole_file",
        description: "Read a small file via file_path only (no symbol). Tests file-level read mode.",
        mcp_args: read_whole_file_args,
        non_mcp_steps: read_whole_file_steps,
        pros: &["Direct file read with no symbol resolution overhead"],
        cons: &["No advantage over plain Read for whole-file reads"],
        verdict_summary: "Near-tie: file-level reads are equivalent; MCP adds no semantic value.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 2,
    },
    Scenario {
        tool: "qartez_outline",
        id: "qartez_outline_small_file",
        description: "Outline a small file (~50 lines) instead of a 2300-line module. Tests proportional output.",
        mcp_args: outline_small_file_args,
        non_mcp_steps: outline_small_file_steps,
        pros: &["Still structured even for small files"],
        cons: &["Marginal benefit when the whole file fits in context"],
        verdict_summary: "MCP advantage shrinks for small files — the outline is nearly as large as reading the file.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 2,
    },
    Scenario {
        tool: "qartez_unused",
        id: "qartez_unused_with_limit",
        description: "List unused symbols with limit=5. Tests pagination.",
        mcp_args: unused_with_limit_args,
        non_mcp_steps: unused_with_limit_steps,
        pros: &["Pagination limits output size"],
        cons: &["Non-MCP has no equivalent pagination"],
        verdict_summary: "MCP wins — limit parameter keeps output bounded while non-MCP emits all matches.",
        non_mcp_is_complete: false,
        reference_answer: None,
        tier: 2,
    },
    Scenario {
        tool: "qartez_impact",
        id: "qartez_impact_nonexistent",
        description: "Impact analysis on a file that does not exist. Tests error handling.",
        mcp_args: impact_nonexistent_args,
        non_mcp_steps: impact_nonexistent_steps,
        pros: &["Clear error message for missing files"],
        cons: &["Error path, not a real use case"],
        verdict_summary: "Validates error handling — both sides should report 'file not found' gracefully.",
        non_mcp_is_complete: true,
        reference_answer: None,
        tier: 2,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_reference_answer_field_present() {
        // 17 tier-1 + 6 tier-2 = 23 total scenarios.
        assert_eq!(SCENARIOS.len(), 23);
        assert!(SCENARIOS[0].reference_answer.is_none());
        assert!(SCENARIOS.iter().all(|s| s.reference_answer.is_none()));
    }

    #[test]
    fn tier_1_scenarios_count() {
        let tier1 = SCENARIOS.iter().filter(|s| s.tier == 1).count();
        assert_eq!(tier1, 17);
    }

    #[test]
    fn tier_2_scenarios_count() {
        let tier2 = SCENARIOS.iter().filter(|s| s.tier == 2).count();
        assert_eq!(tier2, 6);
    }

    #[test]
    fn tier_2_scenarios_have_unique_ids() {
        let tier2_ids: Vec<&str> = SCENARIOS.iter().filter(|s| s.tier == 2).map(|s| s.id).collect();
        let unique: std::collections::BTreeSet<&str> = tier2_ids.iter().copied().collect();
        assert_eq!(tier2_ids.len(), unique.len());
    }
}
