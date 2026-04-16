// Rust guideline compliant 2026-04-15

//! MCP tool parameter structs and tolerant deserializers.
//!
//! Every `Soul*Params` / `QartezParams` struct lives here together with the
//! `flexible` deserialization helpers that let them accept both native JSON
//! types and stringified variants forwarded by some MCP clients.

use schemars::JsonSchema;
use serde::Deserialize;

/// Tolerant deserializers for MCP tool parameters.
///
/// Some MCP clients (notably Claude Code as of 2026-04) serialize numeric
/// and boolean tool arguments as JSON strings before forwarding them over
/// the JSON-RPC bridge, e.g. `{"limit":"30"}` instead of `{"limit":30}`.
/// Strict serde then rejects the string where a `u32` or `bool` is expected,
/// which surfaces to the user as a cryptic `invalid type: string "30"` error.
///
/// These helpers accept either the native JSON form or the stringified form
/// and produce the same value. `schemars` still emits `{"type":"integer"}`
/// / `{"type":"boolean"}` in the tool schema, so well-behaved clients are
/// unaffected.
mod flexible {
    use serde::{Deserialize, Deserializer, de::Error};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum U32OrStr {
        Num(u32),
        Str(String),
    }

    pub(super) fn u32_opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
        match Option::<U32OrStr>::deserialize(d)? {
            None => Ok(None),
            Some(U32OrStr::Num(n)) => Ok(Some(n)),
            Some(U32OrStr::Str(s)) => s
                .parse::<u32>()
                .map(Some)
                .map_err(|e| D::Error::custom(format!("expected u32, got \"{s}\": {e}"))),
        }
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BoolOrStr {
        Bool(bool),
        Str(String),
    }

    pub(super) fn bool_opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
        match Option::<BoolOrStr>::deserialize(d)? {
            None => Ok(None),
            Some(BoolOrStr::Bool(b)) => Ok(Some(b)),
            Some(BoolOrStr::Str(s)) => match s.as_str() {
                "true" | "True" | "TRUE" | "1" => Ok(Some(true)),
                "false" | "False" | "FALSE" | "0" => Ok(Some(false)),
                _ => Err(D::Error::custom(format!("expected bool, got \"{s}\""))),
            },
        }
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum VecOrStr {
        Vec(Vec<String>),
        Str(String),
    }

    fn split_csv(s: &str) -> Vec<String> {
        s.split(',')
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect()
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum F64OrStr {
        Num(f64),
        Str(String),
    }

    pub(super) fn f64_opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
        match Option::<F64OrStr>::deserialize(d)? {
            None => Ok(None),
            Some(F64OrStr::Num(n)) => Ok(Some(n)),
            Some(F64OrStr::Str(s)) => s
                .parse::<f64>()
                .map(Some)
                .map_err(|e| D::Error::custom(format!("expected f64, got \"{s}\": {e}"))),
        }
    }

    pub(super) fn vec_string_opt<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Vec<String>>, D::Error> {
        match Option::<VecOrStr>::deserialize(d)? {
            None => Ok(None),
            Some(VecOrStr::Vec(v)) => Ok(Some(v)),
            Some(VecOrStr::Str(s)) => Ok(Some(split_csv(&s))),
        }
    }

    pub(super) fn vec_string<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
        match VecOrStr::deserialize(d) {
            Ok(VecOrStr::Vec(v)) => Ok(v),
            Ok(VecOrStr::Str(s)) => Ok(split_csv(&s)),
            Err(_) => Ok(Vec::new()),
        }
    }
}

/// Output verbosity for query tools. Encoded as a proper JSON Schema enum so
/// clients see the allowed values at tool-listing time instead of having to
/// try-and-fail on a free-form string.
#[derive(Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub(super) enum Format {
    #[default]
    Detailed,
    Concise,
    Mermaid,
}

/// Toolchain command selector for `qartez_project`. `Info` is the default so a
/// caller can probe the detected toolchain with a bare `qartez_project({})`.
#[derive(Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub(super) enum ProjectAction {
    #[default]
    Info,
    Run,
    Test,
    Build,
    Lint,
    Typecheck,
}

/// Call hierarchy direction for `qartez_calls`. `Both` is the default because it
/// is the most useful on a cold exploration.
#[derive(Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub(super) enum CallDirection {
    Callers,
    Callees,
    #[default]
    Both,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum HotspotLevel {
    File,
    Symbol,
}

/// Sorting axis for hotspot results.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum HotspotSortBy {
    /// Composite hotspot score (default).
    #[default]
    Score,
    /// Normalized 0-10 health rating (ascending: worst first).
    Health,
    /// Maximum cyclomatic complexity.
    Complexity,
    /// PageRank coupling weight.
    Coupling,
    /// Git change count.
    Churn,
}

pub(super) fn is_concise(format: &Option<Format>) -> bool {
    matches!(format, Some(Format::Concise))
}

pub(super) fn is_mermaid(format: &Option<Format>) -> bool {
    matches!(format, Some(Format::Mermaid))
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct QartezParams {
    #[schemars(
        description = "Number of top files to show (default: 20). Pass 0 or set all_files=true to return every file PageRank-sorted."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub top_n: Option<u32>,
    #[schemars(
        description = "If true, return all files sorted by PageRank (ignores top_n). Watch for token-budget truncation on large repos."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub all_files: Option<bool>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub token_budget: Option<u32>,
    #[schemars(
        description = "File paths to boost in ranking (e.g., recently edited or mentioned files)"
    )]
    #[serde(default, deserialize_with = "flexible::vec_string_opt")]
    pub boost_files: Option<Vec<String>>,
    #[schemars(description = "Search terms to boost files containing matching symbols")]
    #[serde(default, deserialize_with = "flexible::vec_string_opt")]
    pub boost_terms: Option<Vec<String>>,
    #[schemars(
        description = "'concise' = file list only, 'detailed' (default) = files + exported symbols"
    )]
    pub format: Option<Format>,
    #[schemars(
        description = "Ranking axis: 'files' (default) shows top files by PageRank; 'symbols' shows top symbols by symbol-level PageRank + their defining file."
    )]
    pub by: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulFindParams {
    #[schemars(
        description = "Exact symbol name to search for (or regex when regex=true). Accepts aliases `symbol`, `symbol_name`, and `query`."
    )]
    #[serde(alias = "symbol", alias = "symbol_name", alias = "query")]
    pub name: String,
    #[schemars(description = "Filter by symbol kind (function, struct, class, etc.)")]
    pub kind: Option<String>,
    #[schemars(
        description = "'concise' = name + file only, 'detailed' (default) = full info with signatures"
    )]
    pub format: Option<Format>,
    #[schemars(
        description = "If true, interpret `name` as a regex applied to indexed symbol names (anchored match semantics: `is_match`). Default false (exact)."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub regex: Option<bool>,
    #[schemars(
        description = "Maximum number of results to return in regex mode. Default 100. Ignored for exact-name lookups."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulReadParams {
    #[schemars(
        description = "Name of a single symbol to read. For batch reads, use `symbols` instead. Accepts the aliases `symbol` and `name`."
    )]
    #[serde(alias = "symbol", alias = "name")]
    pub symbol_name: Option<String>,
    #[schemars(
        description = "Batch mode: list of symbol names to read in one call. Results are concatenated in order. Cheaper than multiple qartez_read calls. Either `symbols` or `symbol_name` must be set."
    )]
    #[serde(default, deserialize_with = "flexible::vec_string_opt")]
    pub symbols: Option<Vec<String>>,
    #[schemars(
        description = "Filter all symbols to a specific file path. When set without any symbol, reads the raw file contents - the whole file by default, or the slice defined by start_line/end_line/limit. max_bytes still bounds the output. Aliases: `file`, `path`."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: Option<String>,
    #[schemars(
        description = "Max response size in bytes (default: 25000). Symbols past the cap are omitted with a truncation marker."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub max_bytes: Option<u32>,
    #[schemars(
        description = "Lines of source context to include BEFORE the symbol's own range (default: 0). Use when you need to see surrounding use-blocks or comments."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub context_lines: Option<u32>,
    #[schemars(
        description = "Read partial body: 1-based start line. Combined with `end_line` or `limit`. When set together with `file_path` but without any symbol, dumps that raw line range from the file - lets you read non-symbol code (imports, module headers) without falling back to Read. Alias: `offset`."
    )]
    #[serde(default, alias = "offset", deserialize_with = "flexible::u32_opt")]
    pub start_line: Option<u32>,
    #[schemars(description = "Partial-body end line (inclusive). Pairs with `start_line`.")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub end_line: Option<u32>,
    #[schemars(
        description = "Partial-body line count. Alternative to `end_line`: when set, reads `limit` lines starting at `start_line` (defaults to 1). Mirrors the built-in Read tool's `limit` parameter so callers can copy-paste the same shape."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulImpactParams {
    #[schemars(
        description = "Relative file path to analyze blast radius for. Aliases: `file`, `path`, `name`."
    )]
    #[serde(alias = "file", alias = "path", alias = "name")]
    pub file_path: String,
    #[schemars(description = "'concise' = counts only, 'detailed' (default) = full file lists")]
    pub format: Option<Format>,
    #[schemars(description = "Include test files in the transitive blast radius (default: false)")]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub include_tests: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulDiffImpactParams {
    #[schemars(
        description = "Git revspec for the range to analyze, e.g. 'main..HEAD', 'HEAD~3..HEAD', or just 'main' (implies main..HEAD). Aliases: `range`, `revspec`."
    )]
    #[serde(alias = "range", alias = "revspec")]
    pub base: String,
    #[schemars(description = "'concise' = summary only, 'detailed' (default) = full report")]
    pub format: Option<Format>,
    #[schemars(description = "Include test files in the blast radius (default: false)")]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub include_tests: Option<bool>,
    #[schemars(
        description = "Add per-file risk scoring: health, boundary violations, test coverage, and composite risk (default: false)"
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub risk: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulCochangeParams {
    #[schemars(
        description = "Relative file path to find co-change partners for. Aliases: `file`, `path`."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: String,
    #[schemars(description = "Max number of results (default: 10)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "'concise' = file paths + counts only, 'detailed' (default) = table format"
    )]
    pub format: Option<Format>,
    #[schemars(
        description = "Skip commits touching more than this many files when recomputing pair counts from git (default: 30). Guards against huge refactor commits inflating counts."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub max_commit_size: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulGrepParams {
    #[schemars(
        description = "FTS5 search query (supports prefix* matching) or regex when regex=true. Accepts the alias `pattern` for parity with Grep."
    )]
    #[serde(alias = "pattern")]
    pub query: String,
    #[schemars(description = "Max number of results (default: 20)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "'concise' = names only, 'detailed' (default) = names + kind + file + lines"
    )]
    pub format: Option<Format>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub token_budget: Option<u32>,
    #[schemars(
        description = "If true, interpret query as a regex applied to indexed symbol names (not bodies). Default false (FTS5 prefix)."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub regex: Option<bool>,
    #[schemars(
        description = "If true, search pre-indexed function bodies via FTS5 instead of symbol names. Useful for finding strings/comments/identifiers that don't appear in declarations. Default false."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub search_bodies: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulRefsParams {
    #[schemars(
        description = "Symbol name to find references for. Accepts aliases `name` and `symbol_name`."
    )]
    #[serde(alias = "name", alias = "symbol_name")]
    pub symbol: String,
    #[schemars(description = "Include transitive dependents (default: false)")]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub transitive: Option<bool>,
    #[schemars(
        description = "'concise' = file paths only, 'detailed' (default) = full import chain"
    )]
    pub format: Option<Format>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub token_budget: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulRenameParams {
    #[schemars(description = "Current symbol name to rename")]
    pub old_name: String,
    #[schemars(description = "New name for the symbol")]
    pub new_name: String,
    #[schemars(
        description = "If true, apply the rename. If false (default), show a preview of changes."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub apply: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulProjectParams {
    #[schemars(
        description = "Which project command to run. Defaults to `info`, which prints the detected toolchain without executing anything. `run` dry-prints the resolved command; the other variants execute it."
    )]
    pub action: Option<ProjectAction>,
    #[schemars(description = "Optional filter (e.g., test name pattern, specific package)")]
    pub filter: Option<String>,
    #[schemars(description = "Timeout in seconds (default: 60)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub timeout: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulMoveParams {
    #[schemars(description = "Symbol name to move. Accepts aliases `name` and `symbol_name`.")]
    #[serde(alias = "name", alias = "symbol_name")]
    pub symbol: String,
    #[schemars(
        description = "Target file path (relative to project root). Created if it doesn't exist."
    )]
    pub to_file: String,
    #[schemars(description = "If true, apply the move. If false (default), show a preview.")]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub apply: Option<bool>,
    #[schemars(
        description = "Disambiguate by symbol kind when the name is shared (e.g. 'function' vs 'method'). Accepts the kinds returned by qartez_find."
    )]
    pub kind: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulRenameFileParams {
    #[schemars(description = "Current file path (relative to project root)")]
    pub from: String,
    #[schemars(description = "New file path (relative to project root)")]
    pub to: String,
    #[schemars(description = "If true, apply the rename. If false (default), show a preview.")]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub apply: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulOutlineParams {
    #[schemars(description = "Relative file path to get outline for. Aliases: `file`, `path`.")]
    #[serde(alias = "file", alias = "path")]
    pub file_path: String,
    #[schemars(
        description = "'concise' = names + lines only, 'detailed' (default) = grouped by kind with signatures"
    )]
    pub format: Option<Format>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub token_budget: Option<u32>,
    #[schemars(
        description = "Skip the first N non-field symbols before rendering. Pair with token_budget to page through very large files."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub offset: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulDepsParams {
    #[schemars(
        description = "Relative file path to show dependencies for. Aliases: `file`, `path`."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: String,
    #[schemars(
        description = "'concise' = file paths only, 'detailed' (default) = paths + edge kinds, 'mermaid' = dependency graph as a Mermaid diagram (use only when the user asks for a visual)"
    )]
    pub format: Option<Format>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub token_budget: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulStatsParams {
    #[schemars(
        description = "Optional relative file path for per-file stats: LOC, symbol count, imports, importers. Aliases: `file`, `path`."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulUnusedParams {
    #[schemars(
        description = "Max number of unused exports to return (default: 50). Pre-materialized at index time; paging through this list is O(1)."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(description = "Pagination offset into the unused-exports list (default: 0)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub offset: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulCallsParams {
    #[schemars(
        description = "Function/method name to analyze call hierarchy for. Accepts aliases `symbol` and `symbol_name`."
    )]
    #[serde(alias = "symbol", alias = "symbol_name")]
    pub name: String,
    #[schemars(
        description = "Which edges to walk. `both` is the default and shows callers and callees."
    )]
    pub direction: Option<CallDirection>,
    #[schemars(
        description = "Max depth for call chain traversal (default: 1). Pass 2 to also see transitive chains - this can emit many lines on hub functions."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub depth: Option<u32>,
    #[schemars(
        description = "'concise' = names only, 'detailed' (default) = with file paths and lines, 'mermaid' = call graph as a Mermaid diagram (use only when the user asks for a visual)"
    )]
    pub format: Option<Format>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulContextParams {
    #[schemars(description = "File paths to analyze context for (files you plan to modify)")]
    #[serde(default, deserialize_with = "flexible::vec_string")]
    pub files: Vec<String>,
    #[schemars(description = "Optional task description to help prioritize relevant context")]
    pub task: Option<String>,
    #[schemars(description = "Max number of context files to return (default: 15)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "'concise' = file list only, 'detailed' (default) = files with reasons"
    )]
    pub format: Option<Format>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub token_budget: Option<u32>,
    #[schemars(
        description = "When true, show score breakdown per component (imports, importer, cochange, transitive, task-match) and count of files excluded by limit / budget. Use to diagnose why a file was or was not surfaced."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub explain: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulHotspotsParams {
    #[schemars(
        description = "Max number of hotspot results to return (default: 20). Hotspots are sorted by composite score = complexity x coupling x change_frequency."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "Granularity: 'file' (default) ranks whole files, 'symbol' ranks individual functions/methods."
    )]
    pub level: Option<HotspotLevel>,
    #[schemars(
        description = "'concise' = compact table, 'detailed' (default) = full breakdown with per-metric scores"
    )]
    pub format: Option<Format>,
    #[schemars(
        description = "Sort results by a specific factor instead of the default composite score. One of: 'score' (default), 'health', 'complexity', 'coupling', 'churn'."
    )]
    pub sort_by: Option<HotspotSortBy>,
    #[schemars(
        description = "Only show results with health score at or below this threshold (0-10 scale, 10 = healthiest). For example, threshold=4 shows only unhealthy code."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub threshold: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulClonesParams {
    #[schemars(
        description = "Max number of clone groups to return (default: 20). Groups are sorted by size (most duplicates first)."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "Page offset for pagination - skip this many groups before returning (default: 0)."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub offset: Option<u32>,
    #[schemars(
        description = "Minimum number of source lines for a symbol to be considered (default: 5). Filters out trivial getters."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_lines: Option<u32>,
    #[schemars(
        description = "'concise' = compact list, 'detailed' (default) = grouped output with file paths and line ranges"
    )]
    pub format: Option<Format>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulSmellsParams {
    #[schemars(
        description = "Filter to specific smell kind(s): 'god_function', 'long_params', 'feature_envy', or comma-separated combination. Omit to detect all."
    )]
    pub kind: Option<String>,
    #[schemars(description = "Scope detection to a single file path (relative to project root).")]
    pub file_path: Option<String>,
    #[schemars(
        description = "God Function: minimum cyclomatic complexity threshold (default: 15)."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_complexity: Option<u32>,
    #[schemars(description = "God Function: minimum body line count threshold (default: 50).")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_lines: Option<u32>,
    #[schemars(
        description = "Long Parameter List: minimum parameter count threshold (default: 5). self/&self do not count."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_params: Option<u32>,
    #[schemars(
        description = "Feature Envy: ratio of external-type calls to own-type calls that triggers the smell (default: 2.0). Only reliable for Rust and Java where owner_type is populated."
    )]
    #[serde(default, deserialize_with = "flexible::f64_opt")]
    pub envy_ratio: Option<f64>,
    #[schemars(description = "Max results to return (default: 30).")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "'concise' = compact one-line-per-smell, 'detailed' (default) = grouped output with context"
    )]
    pub format: Option<Format>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulTestGapsParams {
    #[schemars(
        description = "Analysis mode: 'map' = test-to-source mapping, 'gaps' (default) = untested source files ranked by risk, 'suggest' = test files to run for a git diff range."
    )]
    pub mode: Option<String>,
    #[schemars(
        description = "Scope to a single file path (relative to project root). In 'map' mode shows tests for this source file or sources for this test file."
    )]
    pub file_path: Option<String>,
    #[schemars(
        description = "Git diff range for 'suggest' mode (e.g., 'main', 'HEAD~3'). Same format as qartez_diff_impact base parameter."
    )]
    pub base: Option<String>,
    #[schemars(description = "Max results to return (default: 30).")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "'concise' = compact one-line-per-entry, 'detailed' (default) = grouped output with context"
    )]
    pub format: Option<Format>,
    #[schemars(
        description = "In 'gaps' mode, only show files with PageRank above this threshold (default: 0.0)."
    )]
    #[serde(default, deserialize_with = "flexible::f64_opt")]
    pub min_pagerank: Option<f64>,
    #[schemars(
        description = "In 'map' mode, include which symbols from source files are referenced by test files (default: false)."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub include_symbols: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulWikiParams {
    #[schemars(
        description = "File path to write the wiki to (relative to project root). If omitted, returns the markdown inline."
    )]
    pub write_to: Option<String>,
    #[schemars(
        description = "Leiden resolution parameter (default: 1.0). Larger values produce more, smaller clusters; smaller values merge clusters."
    )]
    pub resolution: Option<f64>,
    #[schemars(
        description = "Minimum cluster size (default: 3). Clusters smaller than this are folded into the `misc` bucket."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_cluster_size: Option<u32>,
    #[schemars(
        description = "Max files listed per cluster section (default: 20). Remaining files are summarised as `... and N more`."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub max_files_per_section: Option<u32>,
    #[schemars(
        description = "Recompute clusters even if the file_clusters table is already populated (default: false)."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub recompute: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulBoundariesParams {
    #[schemars(
        description = "Path to the boundary config (TOML), relative to the project root. Defaults to `.qartez/boundaries.toml`."
    )]
    pub config_path: Option<String>,
    #[schemars(
        description = "If true, skip the checker and emit a starter config derived from the current Leiden clustering instead."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub suggest: Option<bool>,
    #[schemars(
        description = "When `suggest` is true and `write_to` is set, write the generated TOML to this path (relative to the project root) instead of returning it inline."
    )]
    pub write_to: Option<String>,
    #[schemars(
        description = "'concise' = one-line-per-violation summary, 'detailed' (default) = grouped output with rule text."
    )]
    pub format: Option<Format>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulTrendParams {
    #[schemars(
        description = "Relative file path to analyze complexity trend for. Aliases: `file`, `path`."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: String,
    #[schemars(
        description = "Optional symbol name to filter (e.g. a function name). When omitted, shows trends for all symbols in the file. Aliases: `name`, `symbol`."
    )]
    #[serde(alias = "name", alias = "symbol")]
    pub symbol_name: Option<String>,
    #[schemars(
        description = "Max number of commits to analyze (default: 10, max: 50). Only commits that actually changed the file are counted."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "'concise' = compact table, 'detailed' (default) = full breakdown with delta percentages"
    )]
    pub format: Option<Format>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct SoulHierarchyParams {
    #[schemars(
        description = "Type or trait name to query (e.g. 'Display', 'LanguageSupport', 'Serializable'). Aliases: `name`, `type`, `trait`."
    )]
    #[serde(alias = "name", alias = "type", alias = "trait")]
    pub symbol: String,
    #[schemars(
        description = "Query direction: 'sub' (default) = what implements/extends this? 'super' = what does this implement/extend?"
    )]
    pub direction: Option<String>,
    #[schemars(
        description = "If true, follow the hierarchy transitively (e.g. A extends B extends C). Default false."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub transitive: Option<bool>,
    #[schemars(
        description = "Max depth for transitive traversal (default: 20). Only applies when transitive=true."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub max_depth: Option<u32>,
    #[schemars(
        description = "'concise' = names only, 'detailed' (default) = full info with file paths and line numbers, 'mermaid' = inheritance graph as a Mermaid diagram (use only when the user asks for a visual)"
    )]
    pub format: Option<Format>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulSecurityParams {
    #[schemars(
        description = "Max number of findings to return (default: 50). Findings are sorted by risk score (severity x pagerank x export status)."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(description = "Skip the first N findings (for pagination). Combine with limit.")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub offset: Option<u32>,
    #[schemars(
        description = "Filter by vulnerability category: 'secrets', 'injection', 'crypto', 'unsafe', 'info-leak', 'review'."
    )]
    pub category: Option<String>,
    #[schemars(
        description = "Minimum severity threshold: 'low' (default), 'medium', 'high', 'critical'. Findings below this level are excluded."
    )]
    pub severity: Option<String>,
    #[schemars(
        description = "Scan only files whose path contains this substring. Omit to scan the entire project."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: Option<String>,
    #[schemars(description = "If true, include test/spec files in the scan (default: false).")]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub include_tests: Option<bool>,
    #[schemars(
        description = "Path to a custom rules TOML file, relative to the project root. Defaults to `.qartez/security.toml` if it exists."
    )]
    pub config_path: Option<String>,
    #[schemars(
        description = "'concise' = compact table, 'detailed' (default) = full table with snippets."
    )]
    pub format: Option<Format>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[allow(
    dead_code,
    reason = "fields read only when `semantic` feature is active"
)]
pub(super) struct SemanticParams {
    #[schemars(
        description = "Natural language query describing what you are looking for (e.g. 'authentication handler', 'database connection pooling', 'error retry logic')."
    )]
    pub query: String,
    #[schemars(description = "Max number of results (default: 10)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "'concise' = symbol + file only, 'detailed' (default) = full info with scores and snippets"
    )]
    pub format: Option<Format>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum KnowledgeLevel {
    /// Per-file authorship breakdown (default).
    #[default]
    File,
    /// Per-module (directory) bus factor summary.
    Module,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulKnowledgeParams {
    #[schemars(
        description = "Scope analysis to a single file or directory prefix (relative to project root). Omit to analyze the entire project."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: Option<String>,
    #[schemars(
        description = "Granularity: 'file' (default) = per-file author breakdown, 'module' = per-directory bus factor summary."
    )]
    pub level: Option<KnowledgeLevel>,
    #[schemars(
        description = "Filter results to files touched by this author (case-insensitive substring match)."
    )]
    pub author: Option<String>,
    #[schemars(description = "Max results to return (default: 20).")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "'concise' = compact one-line-per-entry, 'detailed' (default) = full table with author percentages"
    )]
    pub format: Option<Format>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ToolsParams {
    #[schemars(
        description = "Tiers or tool names to enable. Use tier names ('analysis', 'refactor', 'meta') or individual tool names ('qartez_refs', 'qartez_calls'). Pass 'all' to enable everything."
    )]
    pub enable: Option<Vec<String>>,
    #[schemars(
        description = "Tiers or tool names to disable. Same format as enable. 'core' and 'qartez_tools' cannot be disabled."
    )]
    pub disable: Option<Vec<String>>,
}
