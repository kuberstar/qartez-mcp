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
///
/// `mermaid` is ONLY honoured by `qartez_deps`, `qartez_calls`, and
/// `qartez_hierarchy`. Every other tool rejects it explicitly via
/// `reject_mermaid` - the enum is shared for schema economy, not because
/// every tool renders graphs.
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

/// Reject `format=mermaid` for tools that do not have a graph renderer.
///
/// The `Format` enum is shared across every tool for schema economy, so
/// every tool's JSON Schema advertises `"mermaid"` as a valid value. Only
/// `qartez_deps`, `qartez_calls`, and `qartez_hierarchy` actually emit
/// Mermaid output; the rest historically fell through to plain text
/// without any signal, which looked like a bug on the caller's end. Call
/// this at the top of any tool that does NOT implement a Mermaid
/// renderer so the caller gets a clear validation error instead of a
/// silent format downgrade.
pub(super) fn reject_mermaid(format: &Option<Format>, tool: &str) -> Result<(), String> {
    if is_mermaid(format) {
        Err(format!(
            "format=mermaid is not supported for {tool}. Use qartez_deps, qartez_calls, or qartez_hierarchy for graph visualisations, or omit `format` for the default text output."
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct QartezParams {
    #[schemars(
        description = "Number of top-ranked files to include. Default: 20. `top_n=0` follows the tool-wide no-cap convention (same as qartez_unused / qartez_context `limit=0`) and is equivalent to `all_files=true`. Watch for token-budget truncation on large repos; the response adds a `raise token_budget=` footer when the render is clipped."
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
        description = "Ranking axis. Must be one of: 'files' (default) shows top files by PageRank; 'symbols' shows top symbols by symbol-level PageRank + their defining file. Unknown values are rejected with a list of valid options.",
        extend("enum" = ["files", "symbols"])
    )]
    pub by: Option<String>,
    #[schemars(
        description = "When true, annotate each top-ranked file row with its max cyclomatic complexity (`CC=N`) and a smell tag when one fires (`god_function` / `long_params`). Same heuristics as qartez_health: CC>=15 with body>=50 lines = god_function, signature with >=5 params = long_params. Lets a single qartez_map call surface hotspot pressure on top of PageRank without a follow-up qartez_health round-trip. Default false."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub with_health: Option<bool>,
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
        description = "If true, interpret `name` as a regex applied to indexed symbol names. Uses `regex::Regex::is_match` (find anywhere, not anchored - prepend `^` for start-anchored, `$` for end-anchored, both for full-match). Case-insensitive by default; prepend `(?-i)` to force case-sensitive. Default false (exact name lookup)."
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
    #[schemars(
        description = "Write guard-ACK entries under `.qartez/acks/` for each indexed changed file (default: false). The tool is read-only by default; set `ack=true` only when running the pre-edit checkpoint flow that needs ACK side-effects."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub ack: Option<bool>,
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
        description = "Skip commits touching more than this many files when recomputing pair counts from git (default: 30). Guards against huge refactor commits inflating counts. Must be >= 1 (0 would skip every commit and produce no data).",
        range(min = 1)
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub max_commit_size: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulGrepParams {
    #[schemars(
        description = "FTS5 search query. Prefix matching is anchored to the symbol name column (e.g. `Parser*` only matches symbols whose name starts with `Parser`, not file paths that happen to contain `parser`). Accepts the alias `pattern` for parity with Grep. Interpreted as a regex when regex=true."
    )]
    #[serde(alias = "pattern")]
    pub query: String,
    #[schemars(
        description = "Max number of results (default: 200). The active governor is `token_budget`; raise `limit` only if you need more than 200 rows."
    )]
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
    #[schemars(
        description = "Filter results by symbol kind (e.g., 'function', 'struct', 'method', 'class'). Applied after the FTS/regex match so it narrows name-search results down to one category without changing the query grammar."
    )]
    pub kind: Option<String>,
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
        description = "'concise' = file paths only (duplicate paths collapsed to `path xN`), 'detailed' (default) = full import chain grouped per importer"
    )]
    pub format: Option<Format>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub token_budget: Option<u32>,
    #[schemars(
        description = "Include refs from test files in the listing. Default true for back-compat. Set false when investigating production usages of a hub symbol whose test imports would otherwise dominate the output."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub include_tests: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulRenameParams {
    #[schemars(description = "Current symbol name to rename")]
    pub old_name: String,
    #[schemars(description = "New name for the symbol")]
    pub new_name: String,
    #[schemars(
        description = "Disambiguate by symbol kind when the name is shared (e.g. 'function' vs 'method'). Required when the old_name matches multiple symbol kinds unless `file_path` is set."
    )]
    pub kind: Option<String>,
    #[schemars(
        description = "Disambiguate by file when the name is defined in multiple files. Relative path. Required when the old_name is defined in multiple files unless the matches collapse to a single symbol via `kind`."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: Option<String>,
    #[schemars(
        description = "If true, allow the rename even when `new_name` already exists as a defined symbol in one of the files that will be rewritten. Default false refuses to apply when a collision is detected."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub allow_collision: Option<bool>,
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
    #[schemars(
        description = "Timeout in seconds (default: 60). Must be >= 1. A value of 0 is rejected because it would produce an immediate timeout error before the toolchain command can run; use 1 for a near-instant check.",
        range(min = 1)
    )]
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
    #[schemars(
        description = "Disambiguate by file when the name is defined in multiple files. Relative path."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: Option<String>,
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
pub(super) struct SoulReplaceSymbolParams {
    #[schemars(description = "Symbol name to replace. Accepts aliases `name` and `symbol_name`.")]
    #[serde(alias = "name", alias = "symbol_name")]
    pub symbol: String,
    #[schemars(
        description = "Full new source for the symbol (replaces lines L[line_start..line_end] inclusive). Must include the signature - this is a whole-symbol replace, not a body-only splice."
    )]
    pub new_code: String,
    #[schemars(
        description = "Disambiguate by symbol kind when the name is shared (e.g. 'function' vs 'method')."
    )]
    pub kind: Option<String>,
    #[schemars(
        description = "Disambiguate by file when the name exists in multiple files. Relative path."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: Option<String>,
    #[schemars(description = "If true, apply the replace. If false (default), show a preview.")]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub apply: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulInsertSymbolParams {
    #[schemars(
        description = "Anchor symbol name. New code is inserted before (or after) its line range. Accepts aliases `name` and `symbol_name`."
    )]
    #[serde(alias = "name", alias = "symbol_name")]
    pub symbol: String,
    #[schemars(
        description = "Source text to insert. A trailing newline is added if missing so the anchor symbol stays on its own line."
    )]
    pub new_code: String,
    #[schemars(
        description = "Disambiguate by symbol kind when the name is shared (e.g. 'function' vs 'method')."
    )]
    pub kind: Option<String>,
    #[schemars(
        description = "Disambiguate by file when the name exists in multiple files. Relative path."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: Option<String>,
    #[schemars(description = "If true, apply the insert. If false (default), show a preview.")]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub apply: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulSafeDeleteParams {
    #[schemars(description = "Symbol name to delete. Accepts aliases `name` and `symbol_name`.")]
    #[serde(alias = "name", alias = "symbol_name")]
    pub symbol: String,
    #[schemars(
        description = "Disambiguate by symbol kind when the name is shared (e.g. 'function' vs 'method')."
    )]
    pub kind: Option<String>,
    #[schemars(
        description = "Disambiguate by file when the name exists in multiple files. Relative path."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: Option<String>,
    #[schemars(
        description = "If true, delete even when the symbol still has importers (they will be left dangling for the caller to fix). Default false refuses to apply when importers exist."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub force: Option<bool>,
    #[schemars(description = "If true, apply the delete. If false (default), show a preview.")]
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
        description = "Max depth for call chain traversal. Default 1, max 10. Values above 10 are clamped. `depth=0` is the seed-only mode: prints the resolved target symbol header without expanding callers or callees."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub depth: Option<u32>,
    #[schemars(
        description = "'concise' = names only, 'detailed' (default) = with file paths and lines, 'mermaid' = call graph as a Mermaid diagram (use only when the user asks for a visual). Mermaid output honours `token_budget`: nodes beyond the budget are replaced with a `truncated` marker."
    )]
    pub format: Option<Format>,
    #[schemars(
        description = "Max rows per caller/callee section (default: 50). Rows beyond the cap are truncated with a `... +N more, raise limit=` footer."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub token_budget: Option<u32>,
    #[schemars(
        description = "Include test-file callers in the listing (default: false). Tests are excluded by default so hub-function output stays focused on production callers."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub include_tests: Option<bool>,
    #[schemars(
        description = "Disambiguate by symbol kind when the name resolves to multiple definitions (e.g. 'function' vs 'method'). Required together with or instead of `file_path` when the name has multiple function-like candidates; without it the tool refuses to count callers or list callees because the counts would not be attributable to a single definition."
    )]
    pub kind: Option<String>,
    #[schemars(
        description = "Disambiguate by file when the name is defined in multiple files. Relative path. Required together with or instead of `kind` when the name has multiple function-like candidates."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulContextParams {
    #[schemars(
        description = "File paths to analyze context for (files you plan to modify). When empty, `task` must be set - the tool then derives the seed files from symbols matching the task terms via FTS search."
    )]
    #[serde(default, deserialize_with = "flexible::vec_string")]
    pub files: Vec<String>,
    #[schemars(
        description = "Optional task description to help prioritize relevant context. Also acts as the seed source when `files` is empty: task words longer than 3 characters are FTS-prefix-searched against symbol names and the matching files become the initial set."
    )]
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
    #[schemars(
        description = "When true, append a one-line blast-radius summary per input file (direct importers, transitive count, cochange partner count). Lets a single qartez_context call cover the prepare-change checklist without a follow-up qartez_impact round-trip. Default false."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub include_impact: Option<bool>,
    #[schemars(
        description = "When true, append a one-line test-coverage status per input file - either the test files that reach it through the file edge graph or `untested`. Same edge graph qartez_test_gaps `map` mode walks. Default false."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub include_test_gaps: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulUnderstandParams {
    #[schemars(
        description = "Symbol name to investigate. Accepts aliases `symbol` and `symbol_name`."
    )]
    #[serde(alias = "symbol", alias = "symbol_name")]
    pub name: String,
    #[schemars(
        description = "Disambiguate by symbol kind when the name resolves to multiple definitions (e.g. 'function' vs 'method' vs 'struct'). Required when the name has multiple matches and `file_path` alone does not narrow to one."
    )]
    pub kind: Option<String>,
    #[schemars(
        description = "Disambiguate by file when the name is defined in multiple files. Relative path. Required when the name has multiple matches and `kind` alone does not narrow to one."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: Option<String>,
    #[schemars(
        description = "Sections to include. Defaults to all four: 'definition' (signature + body), 'calls' (depth=1 callers/callees), 'refs' (top importers), 'cochange' (top co-change partners of the defining file). Pass a subset to skip expensive sections - 'refs' and 'calls' dominate the output for hub symbols.",
        extend("enum" = ["definition", "calls", "refs", "cochange"])
    )]
    #[serde(default, deserialize_with = "flexible::vec_string_opt")]
    pub sections: Option<Vec<String>>,
    #[schemars(
        description = "'concise' = headers + minimal lines, 'detailed' (default) = signature, body, full call/ref tables."
    )]
    pub format: Option<Format>,
    #[schemars(
        description = "Total token budget across all sections (default: 6000). Each active section receives an equal slice of the remaining budget after the header."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub token_budget: Option<u32>,
    #[schemars(
        description = "Per-section ref limit (default: 10). Forwarded to the embedded qartez_refs/qartez_calls calls so hub symbols stay readable. Pass 0 for no cap."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "Include test-file callers/refs in the calls/refs sections (default: false). Mirrors the `include_tests` flag on qartez_calls and qartez_refs."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub include_tests: Option<bool>,
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
        description = "Max number of clone groups to return (default: 20). Groups are sorted by size (most duplicates first). Must be >= 1 (no `no-cap` mode for clone groups).",
        range(min = 1)
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "Page offset for pagination - skip this many groups before returning (default: 0)."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub offset: Option<u32>,
    #[schemars(
        description = "Minimum number of source lines for a symbol to be considered (default: 8). Filters out trivial getters and short dispatch boilerplate. Pass `min_lines=5` for a more aggressive scan that also surfaces small near-duplicates. Must be >= 1 (0 matches every symbol and produces nothing useful after the duplicate-group filter).",
        range(min = 1)
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_lines: Option<u32>,
    #[schemars(
        description = "If true, include test/spec files and inline `#[cfg(test)]` modules in the scan (default: false). Parallel parser-fixture tests share AST shapes by design; excluding them keeps the report focused on production-code refactor candidates."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub include_tests: Option<bool>,
    #[schemars(
        description = "'concise' = compact list, 'detailed' (default) = grouped output with file paths and line ranges"
    )]
    pub format: Option<Format>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulSmellsParams {
    #[schemars(
        description = "Filter to specific smell kind(s). Comma-separated set of: 'god_function', 'long_params', 'feature_envy'. Unknown kinds in a mixed selection are warned and ignored; an all-unknown selection is rejected. Empty segments (e.g. 'god_function,') are trimmed. Omit to detect all."
    )]
    pub kind: Option<String>,
    #[schemars(description = "Scope detection to a single file path (relative to project root).")]
    pub file_path: Option<String>,
    #[schemars(
        description = "God Function: minimum cyclomatic complexity threshold (default: 15). Must be >= 1 (0 matches every function and produces no actionable signal).",
        range(min = 1)
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_complexity: Option<u32>,
    #[schemars(
        description = "God Function: minimum body line count threshold (default: 50). Must be >= 1.",
        range(min = 1)
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_lines: Option<u32>,
    #[schemars(
        description = "Long Parameter List: minimum parameter count threshold (default: 5). self/&self do not count. Must be >= 1.",
        range(min = 1)
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
        description = "Analysis mode. Must be one of: 'map' = test-to-source mapping, 'gaps' (default) = untested source files ranked by risk, 'suggest' = test files to run for a git diff range.",
        extend("enum" = ["map", "gaps", "suggest"])
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
        description = "In 'map' mode, annotate output with symbol info. With a source `file_path`, lists the intersection of symbols defined in the source file AND referenced by its mapped test files (empty when no indexed symbol edges resolve into the file - e.g. tests reach the source via crate-rooted FTS fallback). Without `file_path`, every row of the project-wide listing is annotated with its own indexed symbol count plus a short preview of symbol names in detailed mode. Default: false."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub include_symbols: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulWikiParams {
    #[schemars(
        description = "File path to write the wiki to. Accepts a path relative to the project root or an absolute path whose parent directory already exists. If omitted, returns the markdown inline capped by `token_budget`."
    )]
    pub write_to: Option<String>,
    #[schemars(
        description = "Leiden resolution parameter (default: 1.0). Larger values produce more, smaller clusters; smaller values merge clusters. Passing an explicit value forces a cluster recompute so the new resolution takes effect even when the clustering table is already populated."
    )]
    pub resolution: Option<f64>,
    #[schemars(
        description = "Minimum cluster size (default: 3). Clusters smaller than this are folded into the `misc` bucket. Passing an explicit value forces a cluster recompute."
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
    #[schemars(
        description = "Approximate token budget for inline output (default: 8000). Ignored when `write_to` is set. Output exceeding the budget is truncated with a footer pointing at `write_to`."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub token_budget: Option<u32>,
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
        description = "When `suggest` is true and `write_to` is set, write the generated TOML to this path. Accepts a path relative to the project root or an absolute path whose parent directory already exists. Ignored unless `suggest=true`: passing `write_to` with `suggest=false` is a validation error."
    )]
    pub write_to: Option<String>,
    #[schemars(
        description = "'concise' = one-line-per-violation summary, 'detailed' (default) = grouped output with rule text."
    )]
    pub format: Option<Format>,
    #[schemars(
        description = "When `suggest=true` and no cluster assignment is present, run the Leiden clustering on demand (default: true). Set to false to fail loudly with remediation instead."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub auto_cluster: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulTrendParams {
    #[schemars(
        description = "Relative file path to analyze complexity trend for. Aliases: `file`, `path`."
    )]
    #[serde(alias = "file", alias = "path")]
    pub file_path: String,
    #[schemars(
        description = "Optional symbol name to filter (e.g. a function name). When supplied, the filter is applied PRE-scan so `limit` caps the filtered commit set. Empty strings are treated as 'no filter'. When omitted, shows trends for all symbols in the file. Aliases: `name`, `symbol`."
    )]
    #[serde(alias = "name", alias = "symbol")]
    pub symbol_name: Option<String>,
    #[schemars(
        description = "Max number of commits to analyze (default: 10, max: 50, strictly clamped server-side). Only commits that actually changed the file are counted."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "Approximate token budget for the rendered report (default: 4000, floor: 512). Output exceeding the budget is truncated at a UTF-8 boundary with a footer pointing at `symbol_name` / `limit`."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub token_budget: Option<u32>,
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
        description = "Query direction. Must be one of: 'sub' (default) = what implements/extends this? 'super' = what does this implement/extend?",
        extend("enum" = ["sub", "super"])
    )]
    pub direction: Option<String>,
    #[schemars(
        description = "If true, follow the hierarchy transitively (e.g. A extends B extends C). Default false."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub transitive: Option<bool>,
    #[schemars(
        description = "Max depth for hierarchy traversal (default: 20). `max_depth=0` returns only the seed symbol itself with no children or parents, regardless of `transitive`. Positive values bound the transitive traversal."
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
        description = "Filter by vulnerability category: 'secrets', 'injection', 'crypto', 'unsafe', 'info-leak', 'review'. Validated against the active rule set (builtin + custom .qartez/security.toml)."
    )]
    pub category: Option<String>,
    #[schemars(
        description = "Minimum severity threshold. Must be one of: 'low' (default), 'medium', 'high', 'critical' (case-insensitive). Findings below this level are excluded.",
        extend("enum" = ["low", "medium", "high", "critical", "Low", "Medium", "High", "Critical"])
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

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub(super) enum WorkspaceAction {
    Add,
    Remove,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct SoulWorkspaceParams {
    #[schemars(description = "Action to perform: add or remove a project domain")]
    pub action: WorkspaceAction,
    #[schemars(description = "The alias (domain name) for the project")]
    pub alias: String,
    #[schemars(
        description = "The path to the project directory (required for 'add', optional for 'remove')"
    )]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct SoulAddRootParams {
    #[schemars(
        description = "Path to the directory to register as an additional root. Tilde (`~/`) is expanded; relative paths resolve against the primary project root."
    )]
    pub path: String,
    #[schemars(
        description = "Optional alias for the new root. When omitted, derived from the directory's basename and disambiguated with a numeric suffix on collision. Must be ASCII alphanumeric plus `-`, `_`, `.`."
    )]
    pub alias: Option<String>,
    #[schemars(
        description = "Persist the new root into `.qartez/workspace.toml` so it is reattached on the next start (default: true). Set to false for ephemeral, runtime-only registrations."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub persist: Option<bool>,
    #[schemars(
        description = "Attach a file watcher to the new root so incremental edits are reindexed live (default: true). Has no effect when the server was started with `--no-watch`."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    pub watch: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulListRootsParams {
    #[schemars(
        description = "'concise' = path + alias only, 'detailed' (default) = full row with source, watcher state, file count, and last-index timestamp"
    )]
    pub format: Option<Format>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulHealthParams {
    #[schemars(
        description = "Max number of files to surface across all severity buckets (default: 15)."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "Health-score cutoff (0-10). Files with health above this value are not surfaced (default: 5.0, i.e. only unhealthy files)."
    )]
    #[serde(default, deserialize_with = "flexible::f64_opt")]
    pub max_health: Option<f64>,
    #[schemars(
        description = "God Function: minimum cyclomatic complexity threshold (default: 15). Must be >= 1.",
        range(min = 1)
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_complexity: Option<u32>,
    #[schemars(
        description = "God Function: minimum body line count threshold (default: 50). Must be >= 1.",
        range(min = 1)
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_lines: Option<u32>,
    #[schemars(
        description = "Long Parameter List: minimum parameter count (default: 5). Must be >= 1.",
        range(min = 1)
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_params: Option<u32>,
    #[schemars(
        description = "'concise' = compact table, 'detailed' (default) = grouped output with per-file recommendations"
    )]
    pub format: Option<Format>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulRefactorPlanParams {
    #[schemars(description = "Target file path (relative to project root). Required.")]
    pub file_path: String,
    #[schemars(
        description = "Max number of steps to surface (default: 8, max: 50). `limit=0` follows the tool-wide no-cap convention but is still capped at 50 because larger plans blow the MCP response budget; the response notes the cap only when the pre-cap step count actually exceeded 50. Values above 50 are clamped server-side."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
    #[schemars(
        description = "God Function: minimum cyclomatic complexity threshold (default: 15). Must be >= 1.",
        range(min = 1)
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_complexity: Option<u32>,
    #[schemars(
        description = "God Function: minimum body line count threshold (default: 50). Must be >= 1.",
        range(min = 1)
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_lines: Option<u32>,
    #[schemars(
        description = "Long Parameter List: minimum parameter count (default: 5). Must be >= 1.",
        range(min = 1)
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub min_params: Option<u32>,
    #[schemars(
        description = "'concise' = one-line-per-step, 'detailed' (default) = full step cards with technique + safety + CC impact estimate"
    )]
    pub format: Option<Format>,
}

/// Action selector for `qartez_maintenance`.
///
/// `Stats` is the default so a bare `qartez_maintenance({})` reports DB
/// size and table breakdown without mutating anything. The destructive
/// actions (`Vacuum`, `VacuumIncremental`, `ConvertIncremental`) only
/// run when explicitly requested.
#[derive(Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(super) enum MaintenanceAction {
    /// Default: report DB / WAL sizes, top table row counts, current
    /// fingerprint, last full-reindex timestamp.
    #[default]
    Stats,
    /// `PRAGMA wal_checkpoint(TRUNCATE)`.
    Checkpoint,
    /// FTS5 segment-merge optimization on body and name FTS tables.
    OptimizeFts,
    /// `PRAGMA incremental_vacuum`. No-op when auto_vacuum is not
    /// `INCREMENTAL`; `stats` reports the current setting.
    VacuumIncremental,
    /// Full `VACUUM`. Slow on multi-GiB databases; only run when the
    /// operator has confirmed they want a full rewrite.
    Vacuum,
    /// `PRAGMA auto_vacuum=INCREMENTAL` followed by a full `VACUUM`.
    /// Use this once on a legacy bloated DB to enable cheap incremental
    /// page reclamation going forward. Idempotent: re-running on a DB
    /// that already reports `auto_vacuum=INCREMENTAL` is a fast no-op
    /// and never triggers a second multi-GiB rewrite.
    ConvertIncremental,
    /// Drop file rows whose root prefix is no longer in the live
    /// project root list. Companion to fingerprint-driven reindexing.
    PurgeStale,
    /// Drop file rows whose canonical disk path no longer exists.
    /// Catches ghost rows that `purge_stale` cannot reach because their
    /// prefix is still registered but the underlying directory was
    /// moved, deleted, or recorded under a previous working-directory
    /// layout.
    PurgeOrphaned,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(super) struct SoulMaintenanceParams {
    #[schemars(
        description = "Maintenance action to perform. Default: 'stats' (read-only). Other values: 'checkpoint', 'optimize_fts', 'vacuum_incremental', 'vacuum', 'convert_incremental', 'purge_stale', 'purge_orphaned'. Vacuum-class actions are destructive (rewrite the DB file) and may take minutes on a multi-GiB index."
    )]
    pub action: Option<MaintenanceAction>,
}
