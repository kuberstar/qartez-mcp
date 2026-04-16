//! Programmatic grounding verification for MCP / non-MCP tool outputs.
//!
//! Parses file paths, line numbers, line ranges, and symbol names out of
//! response text and verifies each claim against the real project: file
//! existence via `std::fs`, line counts via the qartez index, symbol
//! existence via `storage::read::find_symbol_by_name`. Produces a
//! [`GroundingScores`] with a scalar `score: f64 ∈ [0.0, 1.0]` plus an
//! itemized breakdown, plus a rendered prompt block that slice A's
//! `build_prompt` embeds between `ANSWERS TO GRADE` and `OUTPUT FORMAT`.
//!
//! No LLM calls; runs entirely in-process. Budget: < 50 ms per side on
//! the 17-scenario Rust self-bench. Degrades gracefully when
//! `.qartez/index.db` is missing: file checks still run, symbol checks
//! are excluded from the denominator, and a `degraded: true` flag is
//! surfaced to the judge prompt.
//!
//! # Design
//!
//! Follows FActScore (Min et al., EMNLP 2023) / SAFE (Wei et al., 2024) /
//! RAGAS faithfulness (Es et al., 2023) - decompose the generated answer
//! into atomic claims and verify each against a trusted knowledge source.
//! Here the knowledge source is the local filesystem plus the qartez
//! SQLite index, rather than a web search. See
//! `docs/benchmark-v2/verifiable-grounding.md` for the full design.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use regex::Regex;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use crate::storage::read;

// ---------------------------------------------------------------------------
// Scope caps - guard against pathological inputs.
// ---------------------------------------------------------------------------

/// Hard cap on input size. Beyond this the extractor short-circuits to
/// zero claims + `degraded = true`, rather than risk pathological regex
/// scan time on adversarial or corrupted output.
const MAX_INPUT_BYTES: usize = 1_024 * 1_024;

/// Hard cap on number of claims. Any extractor that produces more than
/// this is treated as a parser failure (likely a malformed regex
/// capturing too aggressively).
const MAX_CLAIMS: usize = 50_000;

/// Hard cap on path segment depth. Defends against `a/b/c/.../z` that
/// would blow up the extractor without providing useful signal.
const MAX_PATH_DEPTH: usize = 10;

/// Maximum number of unverified-claim examples carried back to the
/// judge. Keeps the prompt block under ~5 lines regardless of how many
/// claims failed verification.
const MAX_UNVERIFIED_EXAMPLES: usize = 5;

// ---------------------------------------------------------------------------
// Claim shape
// ---------------------------------------------------------------------------

/// One extracted factual claim about the project. Deliberately not
/// serialized - this is a transient parsing artifact; only the
/// aggregated [`GroundingScores`] crosses the report / prompt boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Claim {
    /// A bare file path, e.g. ``src/server/mod.rs`` or ``Cargo.toml``.
    FilePath { path: String },
    /// A file path with a single line number, e.g. ``src/foo.rs:42``.
    FileLine { path: String, line: u32 },
    /// A file path with an inclusive line range, e.g.
    /// ``src/foo.rs [L42-L58]``.
    FileRange { path: String, start: u32, end: u32 },
    /// A bare symbol name, with an optional kind hint like ``fn`` /
    /// ``struct`` / ``trait`` / ``impl``.
    Symbol { name: String, kind: Option<String> },
}

// ---------------------------------------------------------------------------
// Scores + context
// ---------------------------------------------------------------------------

/// Claim-level grounding scores for one side of one scenario. Carried on
/// `SideReport::grounding` so both the judge prompt and the Markdown
/// report can surface the verified fraction alongside the LLM judge's
/// subjective rating.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GroundingScores {
    /// Total number of claims extracted from the response.
    pub total_claims: usize,
    /// Number of claims that verified against the live project.
    pub verified_claims: usize,
    /// Breakdown - file-existence claims (raw `FilePath`).
    pub file_claims: usize,
    /// Breakdown - line / range claims (counted separately from plain
    /// file claims because a ranged claim also verifies a file).
    pub line_claims: usize,
    /// Breakdown - symbol claims (excluded from denominator when
    /// `degraded == true`).
    pub symbol_claims: usize,
    /// Verified subset of [`file_claims`](Self::file_claims).
    pub verified_files: usize,
    /// Verified subset of [`line_claims`](Self::line_claims).
    pub verified_lines: usize,
    /// Verified subset of [`symbol_claims`](Self::symbol_claims).
    pub verified_symbols: usize,
    /// Up to [`MAX_UNVERIFIED_EXAMPLES`] of the shortest unverified
    /// claim strings, for the judge prompt.
    pub unverified: Vec<String>,
    /// `verified / total`, clamped to `[0.0, 1.0]`. Zero when
    /// `total_claims == 0`.
    pub score: f64,
    /// Wall-clock extraction + verification time in microseconds.
    /// Reported so a Phase 5 sanity check can assert the < 50 ms soft
    /// budget held.
    pub elapsed_us: u64,
    /// True when symbol verification was unavailable (missing index),
    /// a parser panic was caught, or the input hit the hard caps. The
    /// judge prompt appends a `note: symbol check skipped (missing index)`
    /// line when this is set.
    pub degraded: bool,
}

/// Facts about a single file, cached to avoid re-statting or re-querying
/// the index for the same path across scenarios.
#[derive(Debug, Clone, Copy)]
pub struct FileFacts {
    /// Last line number reachable in the file (1-based). Used to reject
    /// `FileLine` / `FileRange` claims beyond the file's end.
    pub max_line: u32,
}

/// Borrowed context handed to [`verify_side`]. Lives on
/// `BenchmarkRunner` so caches survive a full `run_all` and both sides
/// of a scenario share hits on the same file.
pub struct GroundingContext<'a> {
    /// Absolute path to the project being benchmarked.
    pub project_root: &'a Path,
    /// Connection to `.qartez/index.db`, when available. When `None`,
    /// file existence still verifies but symbol lookups are excluded
    /// from the denominator and `degraded` is set.
    pub conn: Option<&'a Connection>,
    /// Cache of `(normalized_path, Option<FileFacts>)`. Negative
    /// (`None`) entries are cached too so phantom paths are not
    /// re-statted.
    pub file_cache: &'a mut HashMap<String, Option<FileFacts>>,
    /// Cache of `(symbol_name, exists_in_index)`. Negative entries are
    /// cached.
    pub symbol_cache: &'a mut HashMap<String, bool>,
    /// Lazily-initialized basename → candidate path index. Built on the
    /// first bare-basename claim that triggers a lookup.
    pub basename_index: &'a mut Option<HashMap<String, Vec<PathBuf>>>,
}

// ---------------------------------------------------------------------------
// Regex set (ordered - first match wins per span)
// ---------------------------------------------------------------------------
//
// Patterns taken verbatim from `docs/benchmark-v2/verifiable-grounding.md`
// §1. Compiled once via `OnceLock` so repeat invocations pay zero regex
// build cost. Each pattern targets a specific surface shape observed in
// real benchmark output - adding a new tool shape only requires
// appending a pattern here.

/// A file-extension whitelist baked into the bare-path regex. Kept
/// permissive on purpose: recall > precision per design doc §1.
const FILE_EXT_GROUP: &str = r"rs|ts|tsx|js|jsx|py|go|java|toml|json|md|yml|yaml|sql|sh|css|html|kt|swift|cs|php|rb|c|h|cpp|hpp";

struct RegexSet {
    range_bracketed: Regex,
    range_colon: Regex,
    range_at: Regex,
    line_colon: Regex,
    lines_verb: Regex,
    read_lines: Regex,
    path_backtick: Regex,
    path_bare: Regex,
    symbol_backtick: Regex,
    symbol_kind: Regex,
    /// Leading identifier form observed in `qartez_find` concise and
    /// `qartez_grep` detailed output. Shapes:
    ///   `  + QartezServer - src/server/mod.rs [L116-L122]`
    ///   `find_symbol_by_name    function     src/storage/read.rs`
    /// The leading identifier is the claim; the trailing kind / path
    /// are handled by other regexes. Anchored to line start after
    /// optional whitespace / bullet / `+` / `-` to avoid matching
    /// prose mid-sentence.
    symbol_leading: Regex,
}

fn regex_set() -> &'static RegexSet {
    static CELL: OnceLock<RegexSet> = OnceLock::new();
    CELL.get_or_init(|| {
        // Path character class: letters, digits, underscore, slash,
        // dash, dot. Kept conservative so random prose identifiers do
        // not get captured as paths.
        let path_chars = r"[A-Za-z0-9_./-]";

        // Range with bracketed `[L42-L58]` or `[42-58]` suffix,
        // optionally separated by whitespace.
        let range_bracketed = Regex::new(&format!(
            r"({path_chars}+\.(?:{FILE_EXT_GROUP}))\s*\[L?(\d+)\s*[-\u{{2013}}]\s*L?(\d+)\]"
        ))
        .expect("range_bracketed regex compiles");

        // Range with colon form `src/foo.rs:42-58`.
        let range_colon = Regex::new(&format!(
            r"({path_chars}+\.(?:{FILE_EXT_GROUP})):L?(\d+)\s*[-\u{{2013}}]\s*L?(\d+)"
        ))
        .expect("range_colon regex compiles");

        // `@ src/server/mod.rs:L553-559` - qartez_read cat-n header form.
        let range_at = Regex::new(&format!(
            r"@\s*({path_chars}+\.(?:{FILE_EXT_GROUP})):L?(\d+)-(\d+)"
        ))
        .expect("range_at regex compiles");

        // Single-line colon form `src/foo.rs:42` with a negative
        // lookahead for a trailing digit or dash so we don't swallow
        // ranges or file names like `foo.rs:42.5`.
        let line_colon = Regex::new(&format!(
            r"({path_chars}+\.(?:{FILE_EXT_GROUP})):(\d+)(?:[^\d-]|$)"
        ))
        .expect("line_colon regex compiles");

        // Verbose form: `lines 260 to 290 of src/foo.rs`.
        let lines_verb = Regex::new(&format!(
            r"\blines?\s+(\d+)\s*[-\u{{2013}}]\s*(\d+)\s+(?:of|in|from)?\s*({path_chars}+\.(?:{FILE_EXT_GROUP}))"
        ))
        .expect("lines_verb regex compiles");

        // `Read src/foo.rs lines 260-290` / `read src/foo.rs`.
        let read_lines = Regex::new(&format!(
            r"(?i)\bread\s+({path_chars}+\.(?:{FILE_EXT_GROUP}))(?:\s+lines?\s+(\d+)\s*[-\u{{2013}}]\s*(\d+))?"
        ))
        .expect("read_lines regex compiles");

        // Backtick-fenced path inside prose.
        let path_backtick = Regex::new(&format!(
            r"`({path_chars}+\.(?:{FILE_EXT_GROUP}))`"
        ))
        .expect("path_backtick regex compiles");

        // Bare path with at least one directory segment. Excludes URLs
        // and trailing dots.
        let path_bare = Regex::new(&format!(
            r"(?:^|[^`./A-Za-z0-9:])((?:[A-Za-z0-9_-]+/)+[A-Za-z0-9_.-]+\.(?:{FILE_EXT_GROUP}))"
        ))
        .expect("path_bare regex compiles");

        // Backtick-fenced symbol. Rust-flavored: accepts `foo::Bar`.
        let symbol_backtick =
            Regex::new(r"`([A-Z_a-z][A-Za-z0-9_]*(?:::[A-Z_a-z][A-Za-z0-9_]*)*)`")
                .expect("symbol_backtick regex compiles");

        // Kind-prefixed symbol: `fn foo`, `struct Bar`, `impl Baz`, ...
        let symbol_kind =
            Regex::new(r"\b(fn|function|struct|enum|trait|type|class|method|impl)\s+([A-Z_a-z][A-Za-z0-9_]*)")
                .expect("symbol_kind regex compiles");

        // Leading identifier: start-of-line identifier optionally
        // followed by whitespace and a kind word. Captures two groups:
        // (name, optional kind). The `(?m)` flag makes `^` match the
        // start of every line. Anchored forms (`+`, `-`, digits, `|`)
        // are stripped upstream by `strip_cat_n_prefix` and
        // `strip_markdown_bullet`, so we only need to tolerate leading
        // whitespace here.
        let symbol_leading = Regex::new(
            r"(?m)^\s*([A-Z_a-z][A-Za-z0-9_]{1,63})(?:\s+(fn|function|struct|enum|trait|type|class|method|impl))?(?:\s|$|\u{2014}|\u{2013}|-)",
        )
        .expect("symbol_leading regex compiles");

        RegexSet {
            range_bracketed,
            range_colon,
            range_at,
            line_colon,
            lines_verb,
            read_lines,
            path_backtick,
            path_bare,
            symbol_backtick,
            symbol_kind,
            symbol_leading,
        }
    })
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// Extracts all factual claims from `text`. Returns an empty vec for
/// empty or over-cap input.
///
/// Follows the state machine from `docs/benchmark-v2/verifiable-grounding.md`
/// §1.extractor: per-line, ordered regex scan, first-match wins per
/// span, dedup by canonical form. Symbol extraction is suppressed
/// inside triple-fenced code blocks (cat-n bodies, grep dumps); ranged
/// and line claims still fire there because the fences typically carry
/// a header line like `@ src/foo.rs:L10-20` that we want to verify.
pub fn extract_claims(text: &str) -> Vec<Claim> {
    if text.is_empty() || text.len() > MAX_INPUT_BYTES {
        return Vec::new();
    }
    let rx = regex_set();

    let mut out: Vec<Claim> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut in_fence = false;

    for raw_line in text.lines() {
        if out.len() >= MAX_CLAIMS {
            break;
        }

        // Triple-fence toggle. Lines that start with ``` open or close
        // a fenced block. Symbol extraction is suppressed inside.
        let trimmed_fence_check = raw_line.trim_start();
        if trimmed_fence_check.starts_with("```") {
            in_fence = !in_fence;
            // The fence line itself may still carry a path after the
            // language hint (e.g. ```rust src/foo.rs); fall through
            // to normal extraction below.
        }

        let body = strip_cat_n_prefix(raw_line);
        let body = strip_markdown_bullet(body);

        // Track which byte spans have already been consumed by a
        // higher-precedence regex on this line. Used to prevent the
        // bare-path regex from re-firing on a path that already landed
        // as the path half of a range claim.
        let mut consumed: Vec<(usize, usize)> = Vec::new();

        // Ranged claims first (highest precedence).
        for cap in rx.range_bracketed.captures_iter(body) {
            let m = cap.get(0).expect("regex match has group 0");
            if overlaps(&consumed, m.start(), m.end()) {
                continue;
            }
            if is_url_context(body, m.start()) {
                continue;
            }
            let path = cap.get(1).map(|g| g.as_str().to_string());
            let start = cap.get(2).and_then(|g| g.as_str().parse::<u32>().ok());
            let end = cap.get(3).and_then(|g| g.as_str().parse::<u32>().ok());
            if let (Some(path), Some(start), Some(end)) = (path, start, end)
                && path_ok(&path)
                && push_claim(&mut out, &mut seen, Claim::FileRange { path, start, end })
            {
                consumed.push((m.start(), m.end()));
            }
        }

        for cap in rx.range_colon.captures_iter(body) {
            let m = cap.get(0).expect("regex match has group 0");
            if overlaps(&consumed, m.start(), m.end()) {
                continue;
            }
            if is_url_context(body, m.start()) {
                continue;
            }
            let path = cap.get(1).map(|g| g.as_str().to_string());
            let start = cap.get(2).and_then(|g| g.as_str().parse::<u32>().ok());
            let end = cap.get(3).and_then(|g| g.as_str().parse::<u32>().ok());
            if let (Some(path), Some(start), Some(end)) = (path, start, end)
                && path_ok(&path)
                && push_claim(&mut out, &mut seen, Claim::FileRange { path, start, end })
            {
                consumed.push((m.start(), m.end()));
            }
        }

        for cap in rx.range_at.captures_iter(body) {
            let m = cap.get(0).expect("regex match has group 0");
            if overlaps(&consumed, m.start(), m.end()) {
                continue;
            }
            if is_url_context(body, m.start()) {
                continue;
            }
            let path = cap.get(1).map(|g| g.as_str().to_string());
            let start = cap.get(2).and_then(|g| g.as_str().parse::<u32>().ok());
            let end = cap.get(3).and_then(|g| g.as_str().parse::<u32>().ok());
            if let (Some(path), Some(start), Some(end)) = (path, start, end)
                && path_ok(&path)
                && push_claim(&mut out, &mut seen, Claim::FileRange { path, start, end })
            {
                consumed.push((m.start(), m.end()));
            }
        }

        for cap in rx.line_colon.captures_iter(body) {
            let m = cap.get(0).expect("regex match has group 0");
            if overlaps(&consumed, m.start(), m.end()) {
                continue;
            }
            if is_url_context(body, m.start()) {
                continue;
            }
            let path = cap.get(1).map(|g| g.as_str().to_string());
            let line = cap.get(2).and_then(|g| g.as_str().parse::<u32>().ok());
            if let (Some(path), Some(line)) = (path, line)
                && path_ok(&path)
                && push_claim(&mut out, &mut seen, Claim::FileLine { path, line })
            {
                consumed.push((m.start(), m.end()));
            }
        }

        for cap in rx.lines_verb.captures_iter(body) {
            let m = cap.get(0).expect("regex match has group 0");
            if overlaps(&consumed, m.start(), m.end()) {
                continue;
            }
            let start = cap.get(1).and_then(|g| g.as_str().parse::<u32>().ok());
            let end = cap.get(2).and_then(|g| g.as_str().parse::<u32>().ok());
            let path = cap.get(3).map(|g| g.as_str().to_string());
            if let (Some(path), Some(start), Some(end)) = (path, start, end)
                && path_ok(&path)
                && push_claim(&mut out, &mut seen, Claim::FileRange { path, start, end })
            {
                consumed.push((m.start(), m.end()));
            }
        }

        for cap in rx.read_lines.captures_iter(body) {
            let m = cap.get(0).expect("regex match has group 0");
            if overlaps(&consumed, m.start(), m.end()) {
                continue;
            }
            let path = cap.get(1).map(|g| g.as_str().to_string());
            let start = cap.get(2).and_then(|g| g.as_str().parse::<u32>().ok());
            let end = cap.get(3).and_then(|g| g.as_str().parse::<u32>().ok());
            if let Some(path) = path
                && path_ok(&path)
            {
                let claim = match (start, end) {
                    (Some(s), Some(e)) => Claim::FileRange {
                        path,
                        start: s,
                        end: e,
                    },
                    _ => Claim::FilePath { path },
                };
                if push_claim(&mut out, &mut seen, claim) {
                    consumed.push((m.start(), m.end()));
                }
            }
        }

        // Path-only claims (lower precedence than ranges).
        for cap in rx.path_backtick.captures_iter(body) {
            let m = cap.get(0).expect("regex match has group 0");
            if overlaps(&consumed, m.start(), m.end()) {
                continue;
            }
            if is_url_context(body, m.start()) {
                continue;
            }
            if let Some(path) = cap.get(1).map(|g| g.as_str().to_string())
                && path_ok(&path)
                && push_claim(&mut out, &mut seen, Claim::FilePath { path })
            {
                consumed.push((m.start(), m.end()));
            }
        }

        for cap in rx.path_bare.captures_iter(body) {
            let m = cap.get(1).expect("bare-path regex has path group");
            if overlaps(&consumed, m.start(), m.end()) {
                continue;
            }
            if is_url_context(body, m.start()) {
                continue;
            }
            let path = m.as_str().to_string();
            if path_ok(&path) && push_claim(&mut out, &mut seen, Claim::FilePath { path }) {
                consumed.push((m.start(), m.end()));
            }
        }

        // Symbol claims - suppressed inside triple-fenced blocks.
        if !in_fence {
            // Leading-identifier form comes FIRST so tabular output from
            // `qartez_find` / `qartez_grep` is captured before the generic
            // `symbol_kind` regex starts matching kind-first fragments
            // mid-line. Only fires once per line (the first match).
            if let Some(cap) = rx.symbol_leading.captures(body) {
                let name_match = cap.get(1);
                if let Some(name_m) = name_match
                    && !overlaps(&consumed, name_m.start(), name_m.end())
                {
                    let name = name_m.as_str().to_string();
                    let kind = cap.get(2).map(|g| g.as_str().to_string());
                    if is_leading_symbol_candidate(&name)
                        && push_claim(&mut out, &mut seen, Claim::Symbol { name, kind })
                    {
                        consumed.push((name_m.start(), name_m.end()));
                    }
                }
            }

            for cap in rx.symbol_backtick.captures_iter(body) {
                let m = cap.get(0).expect("regex match has group 0");
                if overlaps(&consumed, m.start(), m.end()) {
                    continue;
                }
                if let Some(full) = cap.get(1).map(|g| g.as_str()) {
                    // `foo::Bar` - take the final segment as the claim.
                    let name = full.rsplit("::").next().unwrap_or(full).to_string();
                    if symbol_ok(&name)
                        && push_claim(&mut out, &mut seen, Claim::Symbol { name, kind: None })
                    {
                        consumed.push((m.start(), m.end()));
                    }
                }
            }

            for cap in rx.symbol_kind.captures_iter(body) {
                let m = cap.get(0).expect("regex match has group 0");
                if overlaps(&consumed, m.start(), m.end()) {
                    continue;
                }
                let kind = cap.get(1).map(|g| g.as_str().to_string());
                let name = cap.get(2).map(|g| g.as_str().to_string());
                if let Some(name) = name
                    && symbol_ok(&name)
                    && push_claim(&mut out, &mut seen, Claim::Symbol { name, kind })
                {
                    consumed.push((m.start(), m.end()));
                }
            }
        }
    }

    out
}

fn overlaps(consumed: &[(usize, usize)], start: usize, end: usize) -> bool {
    consumed.iter().any(|&(s, e)| start < e && end > s)
}

/// Returns `true` if position `at` in `haystack` is preceded by `://`,
/// indicating a URL-embedded path that should not be treated as a local
/// file reference.
fn is_url_context(haystack: &str, at: usize) -> bool {
    if at < 3 {
        return false;
    }
    // Scan up to ~20 characters back for the `://` marker.
    let lookback_start = at.saturating_sub(20);
    let slice = &haystack.as_bytes()[lookback_start..at];
    // Look for `://` terminating at a byte in the slice.
    for i in 0..slice.len().saturating_sub(2) {
        if slice[i] == b':' && slice[i + 1] == b'/' && slice[i + 2] == b'/' {
            return true;
        }
    }
    false
}

fn strip_cat_n_prefix(line: &str) -> &str {
    // Pattern: optional whitespace, 1-6 digits, optional whitespace, `|`,
    // optional whitespace - matches `  553 | ` in `qartez_read` output.
    let trimmed = line.trim_start();
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 || i > 6 {
        return line;
    }
    // Skip whitespace, look for `|`.
    let mut j = i;
    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
        j += 1;
    }
    if j >= bytes.len() || bytes[j] != b'|' {
        return line;
    }
    j += 1;
    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
        j += 1;
    }
    &trimmed[j..]
}

fn strip_markdown_bullet(line: &str) -> &str {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("- ") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("* ") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("+ ") {
        rest
    } else {
        trimmed
    }
}

fn path_ok(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') {
        return false;
    }
    let segments = path.split('/').count();
    if segments > MAX_PATH_DEPTH {
        return false;
    }
    // Reject obviously bogus tails like trailing dots.
    if path.ends_with('.') || path.ends_with("/.") {
        return false;
    }
    true
}

fn symbol_ok(name: &str) -> bool {
    // A symbol is useful only if it is at least 2 characters long and
    // not a common English word that happens to match the identifier
    // shape. The longer list lives upstream in the regex pattern; here
    // we only block the truly trivial cases.
    if name.len() < 2 {
        return false;
    }
    // Reject pure-lowercase short words that look like prose. Keep
    // anything capitalized, anything with an underscore, anything with
    // a digit, anything with more than 4 characters.
    if name.len() <= 4
        && name.chars().all(|c| c.is_ascii_lowercase())
        && matches!(
            name,
            "the" | "and" | "for" | "not" | "you" | "but" | "use" | "are" | "was" | "has"
        )
    {
        return false;
    }
    true
}

/// Stricter admission test for the line-leading symbol regex. That
/// regex matches any identifier at the start of a line and would false-
/// positive on prose lines ("The ... is ...") unless we require a
/// code-shaped identifier.
///
/// Accepts:
///   - snake_case with at least one underscore (`find_symbol_by_name`),
///   - CamelCase / PascalCase with an internal uppercase (`QartezServer`),
///   - ALL_CAPS constants.
///
/// Rejects plain lowercase prose words and short title-case English words.
fn is_leading_symbol_candidate(name: &str) -> bool {
    if !symbol_ok(name) {
        return false;
    }
    if name.contains('_') {
        return true;
    }
    let has_upper = name.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = name.chars().any(|c| c.is_ascii_lowercase());
    if has_upper && !has_lower {
        return true;
    }
    // PascalCase: starts with uppercase AND contains a second uppercase
    // after a lowercase (or is long enough to be clearly an identifier
    // rather than a sentence-initial word).
    if has_upper && has_lower {
        let chars: Vec<char> = name.chars().collect();
        if chars.len() >= 6 {
            for i in 1..chars.len() {
                if chars[i].is_ascii_uppercase() && chars[i - 1].is_ascii_lowercase() {
                    return true;
                }
            }
        }
    }
    false
}

fn canonical(c: &Claim) -> String {
    match c {
        Claim::FilePath { path } => format!("P:{}", normalize_path(path)),
        Claim::FileLine { path, line } => format!("L:{}:{}", normalize_path(path), line),
        Claim::FileRange { path, start, end } => {
            format!("R:{}:{}-{}", normalize_path(path), start, end)
        }
        Claim::Symbol { name, .. } => format!("S:{name}"),
    }
}

fn normalize_path(path: &str) -> String {
    path.trim_start_matches("./").to_string()
}

fn push_claim(out: &mut Vec<Claim>, seen: &mut HashSet<String>, claim: Claim) -> bool {
    if out.len() >= MAX_CLAIMS {
        return false;
    }
    let key = canonical(&claim);
    if seen.insert(key) {
        out.push(claim);
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// Verify every claim in `output` against the context. Returns `None`
/// if zero claims were extracted (the judge prompt then shows
/// `score N/A` for that side rather than a bogus `0.0`).
///
/// Wraps extraction + verification in `std::panic::catch_unwind` so a
/// parser bug does not crash the benchmark run - on panic this returns
/// a `degraded: true` sentinel with a single `"parser panic: ..."`
/// example in `unverified`.
pub fn verify_side(output: &str, ctx: &mut GroundingContext<'_>) -> Option<GroundingScores> {
    let start = Instant::now();

    // Short-circuit on over-cap input before extraction.
    if output.len() > MAX_INPUT_BYTES {
        return Some(GroundingScores {
            total_claims: 0,
            degraded: true,
            elapsed_us: start.elapsed().as_micros() as u64,
            unverified: vec![format!(
                "input over cap ({} bytes > {} bytes)",
                output.len(),
                MAX_INPUT_BYTES
            )],
            ..Default::default()
        });
    }

    // Panic recovery for the whole extract+verify pipeline. Extraction
    // is the high-risk site (regex backtracking, arithmetic on parsed
    // line numbers); verification is straightforward but cheap to guard.
    //
    // Note: `catch_unwind` requires `UnwindSafe`. `&mut` refs are not,
    // so we use `AssertUnwindSafe` - the invariants we care about
    // (caches left consistent) are upheld by push-only operations on
    // the cache hash maps, which cannot get corrupted by a mid-write
    // panic in safe Rust.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        verify_side_inner(output, ctx)
    }));

    match result {
        Ok(inner) => inner,
        Err(_) => Some(GroundingScores {
            total_claims: 0,
            degraded: true,
            elapsed_us: start.elapsed().as_micros() as u64,
            unverified: vec!["parser panic: extractor aborted".to_string()],
            ..Default::default()
        }),
    }
}

fn verify_side_inner(output: &str, ctx: &mut GroundingContext<'_>) -> Option<GroundingScores> {
    let start = Instant::now();
    let claims = extract_claims(output);
    if claims.is_empty() {
        return None;
    }

    let mut scores = GroundingScores {
        total_claims: claims.len(),
        ..Default::default()
    };
    let conn_available = ctx.conn.is_some();
    // When there is no index available, symbol claims cannot be
    // verified; they are excluded from the denominator and `degraded`
    // is set so the judge prompt discloses the reduced-capability run.
    scores.degraded = !conn_available;

    let mut unverified_buf: Vec<String> = Vec::new();
    let mut effective_total: usize = 0;
    let mut verified_total: usize = 0;

    for claim in &claims {
        match claim {
            Claim::FilePath { path } => {
                scores.file_claims += 1;
                effective_total += 1;
                if verify_file_exists(path, ctx).is_some() {
                    scores.verified_files += 1;
                    verified_total += 1;
                } else {
                    unverified_buf.push(path.clone());
                }
            }
            Claim::FileLine { path, line } => {
                scores.line_claims += 1;
                effective_total += 1;
                match verify_file_exists(path, ctx) {
                    Some(facts) if *line <= facts.max_line => {
                        scores.verified_lines += 1;
                        verified_total += 1;
                    }
                    _ => {
                        unverified_buf.push(format!("{path}:{line}"));
                    }
                }
            }
            Claim::FileRange { path, start, end } => {
                scores.line_claims += 1;
                effective_total += 1;
                match verify_file_exists(path, ctx) {
                    Some(facts)
                        if *start <= *end && *start <= facts.max_line && *end <= facts.max_line =>
                    {
                        scores.verified_lines += 1;
                        verified_total += 1;
                    }
                    _ => {
                        unverified_buf.push(format!("{path}:{start}-{end}"));
                    }
                }
            }
            Claim::Symbol { name, .. } => {
                scores.symbol_claims += 1;
                // Symbols are excluded from the denominator when the
                // index is unavailable so a missing `.qartez` does not
                // falsely depress the score.
                if !conn_available {
                    continue;
                }
                effective_total += 1;
                if verify_symbol_exists(name, ctx) {
                    scores.verified_symbols += 1;
                    verified_total += 1;
                } else {
                    unverified_buf.push(name.clone());
                }
            }
        }
    }

    scores.verified_claims = verified_total;
    scores.score = if effective_total == 0 {
        0.0
    } else {
        verified_total as f64 / effective_total as f64
    };

    // Keep the shortest few unverified examples so the judge prompt
    // stays compact regardless of corpus size.
    unverified_buf.sort_by_key(|s| s.len());
    unverified_buf.truncate(MAX_UNVERIFIED_EXAMPLES);
    scores.unverified = unverified_buf;

    scores.elapsed_us = start.elapsed().as_micros() as u64;
    Some(scores)
}

fn verify_file_exists(path: &str, ctx: &mut GroundingContext<'_>) -> Option<FileFacts> {
    let normalized = normalize_path(path);
    if let Some(hit) = ctx.file_cache.get(&normalized) {
        return *hit;
    }

    // Resolve bare basenames via a lazy project walk.
    let has_slash = normalized.contains('/');
    let facts = if has_slash {
        verify_file_direct(&normalized, ctx)
    } else {
        verify_file_basename(&normalized, ctx)
    };

    ctx.file_cache.insert(normalized, facts);
    facts
}

fn verify_file_direct(path: &str, ctx: &mut GroundingContext<'_>) -> Option<FileFacts> {
    // Index path first - the qartez index carries `line_count` pre-
    // computed, no file I/O needed.
    if let Some(conn) = ctx.conn
        && let Ok(Some(file)) = read::get_file_by_path(conn, path)
    {
        let max_line = i64::max(file.line_count, 0) as u32;
        let abs = ctx.project_root.join(path);
        if abs.exists() {
            return Some(FileFacts { max_line });
        }
    }

    // Filesystem fallback: stat + count lines. Cheap for the tiny
    // fraction of claims that point at files the index does not know
    // about (Cargo.toml, dotfiles).
    let abs = ctx.project_root.join(path);
    let meta = std::fs::metadata(&abs).ok()?;
    if !meta.is_file() {
        return None;
    }
    let contents = std::fs::read_to_string(&abs).ok()?;
    let max_line = contents.lines().count() as u32;
    Some(FileFacts { max_line })
}

fn verify_file_basename(basename: &str, ctx: &mut GroundingContext<'_>) -> Option<FileFacts> {
    // Lazy-init the basename index. Walks the project tree once.
    if ctx.basename_index.is_none() {
        *ctx.basename_index = Some(build_basename_index(ctx.project_root));
    }
    let index = ctx.basename_index.as_ref().expect("just initialized");
    let hits = index.get(basename)?;
    if hits.len() != 1 {
        // Ambiguous basename → unverified. Explicit design decision in
        // §1.edge_cases: rewarding bare basenames only when unambiguous.
        return None;
    }
    let abs = hits[0].clone();
    let rel = abs.strip_prefix(ctx.project_root).ok()?;
    let rel_str = rel.to_string_lossy().to_string();
    verify_file_direct(&rel_str, ctx)
}

fn build_basename_index(root: &Path) -> HashMap<String, Vec<PathBuf>> {
    let mut index: HashMap<String, Vec<PathBuf>> = HashMap::new();
    // Bounded walk: at most 20000 entries so a pathological repo does
    // not stall the benchmark. Uses the `ignore` crate when available,
    // falls back to a manual recurse on failure.
    let walker = ignore::WalkBuilder::new(root)
        .max_depth(Some(MAX_PATH_DEPTH))
        .standard_filters(true)
        .add_custom_ignore_filename(".qartezignore")
        .build();
    let mut count: usize = 0;
    for entry in walker.flatten() {
        if count >= 20_000 {
            break;
        }
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let abs = entry.path().to_path_buf();
        if let Some(name) = abs.file_name().and_then(|n| n.to_str()) {
            index.entry(name.to_string()).or_default().push(abs);
            count += 1;
        }
    }
    index
}

fn verify_symbol_exists(name: &str, ctx: &mut GroundingContext<'_>) -> bool {
    if let Some(&hit) = ctx.symbol_cache.get(name) {
        return hit;
    }
    let conn = match ctx.conn {
        Some(c) => c,
        None => return false,
    };
    let result = read::find_symbol_by_name(conn, name)
        .map(|rows| !rows.is_empty())
        .unwrap_or(false);
    ctx.symbol_cache.insert(name.to_string(), result);
    result
}

// ---------------------------------------------------------------------------
// Prompt block rendering
// ---------------------------------------------------------------------------

/// Renders the multi-line grounding block embedded in `build_prompt`
/// between `ANSWERS TO GRADE` and `OUTPUT FORMAT` (PLAN.md §2.3).
///
/// Three shapes are produced:
/// 1. Normal - `claims=... verified=... unverified=...`.
/// 2. No-claims - `no verifiable claims extracted - score N/A`
///    (when `g` is `None` or `total_claims == 0`).
/// 3. Degraded - the normal body plus a trailing
///    `note: symbol check skipped (missing index)` line.
pub fn render_prompt_block(label: &str, g: Option<&GroundingScores>) -> String {
    match g {
        None => format!(
            "Programmatic grounding for {label}:\n  no verifiable claims extracted - score N/A"
        ),
        Some(scores) if scores.total_claims == 0 => format!(
            "Programmatic grounding for {label}:\n  no verifiable claims extracted - score N/A"
        ),
        Some(scores) => {
            let unverified_list = if scores.unverified.is_empty() {
                "[]".to_string()
            } else {
                let quoted: Vec<String> = scores
                    .unverified
                    .iter()
                    .map(|u| format!("\"{u}\""))
                    .collect();
                format!("[{}]", quoted.join(", "))
            };
            let mut out = format!(
                "Programmatic grounding for {label}:\n  claims={total} (files={files}, lines={lines}, symbols={symbols})\n  verified={verified}/{total} (score={score:.3})\n  unverified={unverified}",
                total = scores.total_claims,
                files = scores.file_claims,
                lines = scores.line_claims,
                symbols = scores.symbol_claims,
                verified = scores.verified_claims,
                score = scores.score,
                unverified = unverified_list,
            );
            if scores.degraded {
                out.push_str("\n  note: symbol check skipped (missing index)");
            }
            out
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_ctx<'a>(
        project_root: &'a Path,
        conn: Option<&'a Connection>,
        file_cache: &'a mut HashMap<String, Option<FileFacts>>,
        symbol_cache: &'a mut HashMap<String, bool>,
        basename_index: &'a mut Option<HashMap<String, Vec<PathBuf>>>,
    ) -> GroundingContext<'a> {
        GroundingContext {
            project_root,
            conn,
            file_cache,
            symbol_cache,
            basename_index,
        }
    }

    #[test]
    fn extract_claims_qartez_find_shape() {
        let input = " + QartezServer - src/server/mod.rs [L116-L122] →6";
        let claims = extract_claims(input);
        assert!(
            claims.contains(&Claim::FileRange {
                path: "src/server/mod.rs".to_string(),
                start: 116,
                end: 122,
            }),
            "expected FileRange, got {claims:?}"
        );
        assert!(
            claims
                .iter()
                .any(|c| matches!(c, Claim::Symbol { name, .. } if name == "QartezServer")),
            "expected QartezServer symbol, got {claims:?}"
        );
    }

    #[test]
    fn extract_claims_qartez_grep_shape() {
        let input = "find_symbol_by_name    function     src/storage/read.rs  [L148-L193]";
        let claims = extract_claims(input);
        assert!(
            claims.contains(&Claim::FileRange {
                path: "src/storage/read.rs".to_string(),
                start: 148,
                end: 193,
            }),
            "expected FileRange, got {claims:?}"
        );
        assert!(
            claims.iter().any(
                |c| matches!(c, Claim::Symbol { name, kind } if name == "find_symbol_by_name" && kind.as_deref() == Some("function"))
            ),
            "expected find_symbol_by_name symbol with kind `function`, got {claims:?}"
        );
    }

    #[test]
    fn extract_claims_qartez_read_cat_n() {
        let input = "@ src/server/mod.rs:L553-559\n```rust\n  553 | pub fn helper() -> Foo {\n  554 |     Foo::default()\n  555 | }\n```";
        let claims = extract_claims(input);
        // Range claim must be extracted (the outer header).
        assert!(
            claims.contains(&Claim::FileRange {
                path: "src/server/mod.rs".to_string(),
                start: 553,
                end: 559,
            }),
            "expected range claim from header, got {claims:?}"
        );
        // Symbols inside the fenced body must be skipped.
        let symbol_count = claims
            .iter()
            .filter(|c| matches!(c, Claim::Symbol { .. }))
            .count();
        assert_eq!(
            symbol_count, 0,
            "expected no symbol claims from fenced body, got {claims:?}"
        );
    }

    #[test]
    fn extract_claims_backtick_path() {
        let input = "the file `src/foo.rs` contains the helper";
        let claims = extract_claims(input);
        assert!(
            claims.contains(&Claim::FilePath {
                path: "src/foo.rs".to_string()
            }),
            "expected backtick path claim, got {claims:?}"
        );
    }

    #[test]
    fn extract_claims_url_rejected() {
        let input = "visit https://rust-lang.org/main.rs for info";
        let claims = extract_claims(input);
        let has_main = claims.iter().any(|c| match c {
            Claim::FilePath { path } => path.ends_with("main.rs"),
            _ => false,
        });
        assert!(
            !has_main,
            "URL-embedded main.rs should not be extracted: {claims:?}"
        );
    }

    #[test]
    fn extract_claims_basename_only() {
        let input = "the config.json is corrupt";
        let claims = extract_claims(input);
        // The bare-path regex requires at least one slash, so bare
        // basenames without a directory prefix do not land as
        // `FilePath` claims by design. The verifier then has nothing
        // to do - bare prose basenames are genuinely ambiguous.
        // This test documents the decision instead of flipping it.
        assert!(
            !claims
                .iter()
                .any(|c| matches!(c, Claim::FilePath { path } if path == "config.json")),
            "bare basenames are not extracted as FilePath claims: {claims:?}"
        );
    }

    #[test]
    fn extract_claims_backtick_basename() {
        // Backtick-fenced basenames are intentional author claims, so
        // they DO land as FilePath claims.
        let input = "the `config.json` file";
        let claims = extract_claims(input);
        assert!(
            claims.contains(&Claim::FilePath {
                path: "config.json".to_string()
            }),
            "expected backtick-fenced config.json: {claims:?}"
        );
    }

    #[test]
    fn extract_claims_dedup() {
        let input = "src/foo.rs and again src/foo.rs and `src/foo.rs` three times";
        let claims = extract_claims(input);
        let foo_count = claims
            .iter()
            .filter(|c| matches!(c, Claim::FilePath { path } if path == "src/foo.rs"))
            .count();
        assert_eq!(foo_count, 1, "dedup failed: {claims:?}");
    }

    #[test]
    fn extract_claims_empty_input() {
        let claims = extract_claims("");
        assert!(claims.is_empty());
    }

    #[test]
    fn extract_claims_caps_respected() {
        // Build a 2 MB input that would otherwise produce many claims.
        let line = "see src/foo.rs for details\n";
        let repeat = (MAX_INPUT_BYTES / line.len()) + 100;
        let giant = line.repeat(repeat);
        assert!(giant.len() > MAX_INPUT_BYTES);
        let claims = extract_claims(&giant);
        assert!(
            claims.is_empty(),
            "over-cap input must short-circuit, got {} claims",
            claims.len()
        );
    }

    #[test]
    fn verify_side_file_exists() {
        // Point project_root at this very worktree, assert Cargo.toml
        // lands as verified.
        let project_root = find_project_root();
        let mut file_cache: HashMap<String, Option<FileFacts>> = HashMap::new();
        let mut symbol_cache: HashMap<String, bool> = HashMap::new();
        let mut basename_index: Option<HashMap<String, Vec<PathBuf>>> = None;
        let mut ctx = make_ctx(
            &project_root,
            None,
            &mut file_cache,
            &mut symbol_cache,
            &mut basename_index,
        );
        // `Cargo.toml` alone would not be extracted (no slash); use a
        // backtick form the regex recognizes.
        let input = "the file `Cargo.toml` defines the workspace";
        let result = verify_side(input, &mut ctx).expect("one claim extracted");
        assert_eq!(result.total_claims, 1);
        assert_eq!(result.verified_claims, 1);
        assert!((result.score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn verify_side_missing_file() {
        let project_root = find_project_root();
        let mut file_cache: HashMap<String, Option<FileFacts>> = HashMap::new();
        let mut symbol_cache: HashMap<String, bool> = HashMap::new();
        let mut basename_index: Option<HashMap<String, Vec<PathBuf>>> = None;
        let mut ctx = make_ctx(
            &project_root,
            None,
            &mut file_cache,
            &mut symbol_cache,
            &mut basename_index,
        );
        let input = "see `src/this_file_does_not_exist.rs`";
        let result = verify_side(input, &mut ctx).expect("one claim extracted");
        assert!(result.score < 1.0);
        assert!(!result.unverified.is_empty());
        assert!(
            result
                .unverified
                .iter()
                .any(|u| u.contains("this_file_does_not_exist")),
            "unverified list should contain phantom: {:?}",
            result.unverified
        );
    }

    #[test]
    fn verify_side_degraded_no_conn() {
        let project_root = find_project_root();
        let mut file_cache: HashMap<String, Option<FileFacts>> = HashMap::new();
        let mut symbol_cache: HashMap<String, bool> = HashMap::new();
        let mut basename_index: Option<HashMap<String, Vec<PathBuf>>> = None;
        let mut ctx = make_ctx(
            &project_root,
            None,
            &mut file_cache,
            &mut symbol_cache,
            &mut basename_index,
        );
        // Mix a real file and a symbol. Without a conn, the symbol is
        // excluded from the denominator and degraded is set.
        let input = "`Cargo.toml` defines `QartezServer`";
        let result = verify_side(input, &mut ctx).expect("claims extracted");
        assert!(result.degraded, "degraded flag should be set");
        assert_eq!(result.symbol_claims, 1);
        // Denominator = 1 (Cargo.toml alone), symbol excluded.
        assert!(
            (result.score - 1.0).abs() < f64::EPSILON,
            "symbol exclusion should not depress the score: {result:?}"
        );
    }

    #[test]
    fn verify_side_none_on_zero_claims() {
        let project_root = find_project_root();
        let mut file_cache: HashMap<String, Option<FileFacts>> = HashMap::new();
        let mut symbol_cache: HashMap<String, bool> = HashMap::new();
        let mut basename_index: Option<HashMap<String, Vec<PathBuf>>> = None;
        let mut ctx = make_ctx(
            &project_root,
            None,
            &mut file_cache,
            &mut symbol_cache,
            &mut basename_index,
        );
        let input = "just prose, no paths at all";
        let result = verify_side(input, &mut ctx);
        assert!(result.is_none(), "zero-claim input should return None");
    }

    #[test]
    fn render_prompt_block_with_scores() {
        let scores = GroundingScores {
            total_claims: 12,
            verified_claims: 11,
            file_claims: 7,
            line_claims: 2,
            symbol_claims: 3,
            verified_files: 7,
            verified_lines: 2,
            verified_symbols: 2,
            unverified: vec!["src/missing.rs".to_string()],
            score: 11.0 / 12.0,
            elapsed_us: 100,
            degraded: false,
        };
        let rendered = render_prompt_block("ANSWER A (MCP)", Some(&scores));
        assert!(rendered.contains("Programmatic grounding for ANSWER A (MCP):"));
        assert!(rendered.contains("verified=11/12"));
        assert!(rendered.contains("score=0.917"));
        assert!(!rendered.contains("note: symbol check skipped"));
    }

    #[test]
    fn render_prompt_block_none() {
        let rendered = render_prompt_block("ANSWER A (MCP)", None);
        assert!(rendered.contains("no verifiable claims extracted - score N/A"));
    }

    #[test]
    fn render_prompt_block_zero_claims() {
        let scores = GroundingScores::default();
        let rendered = render_prompt_block("ANSWER A (MCP)", Some(&scores));
        assert!(rendered.contains("no verifiable claims extracted - score N/A"));
    }

    #[test]
    fn render_prompt_block_degraded_appends_note() {
        let scores = GroundingScores {
            total_claims: 2,
            verified_claims: 2,
            file_claims: 2,
            line_claims: 0,
            symbol_claims: 0,
            verified_files: 2,
            verified_lines: 0,
            verified_symbols: 0,
            unverified: vec![],
            score: 1.0,
            elapsed_us: 50,
            degraded: true,
        };
        let rendered = render_prompt_block("ANSWER A (MCP)", Some(&scores));
        assert!(rendered.ends_with("note: symbol check skipped (missing index)"));
    }

    // -- helpers -------------------------------------------------------

    /// Locate the worktree root so the file-existence tests can point
    /// at a known-good `Cargo.toml`. Walks up from CARGO_MANIFEST_DIR
    /// until it finds a `Cargo.toml`.
    fn find_project_root() -> PathBuf {
        // `CARGO_MANIFEST_DIR` is the crate root at compile time.
        let manifest = env!("CARGO_MANIFEST_DIR");
        PathBuf::from(manifest)
    }
}
