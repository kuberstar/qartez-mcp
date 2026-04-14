//! Per-language benchmark profiles.
//!
//! Each profile describes the knobs needed to run the per-tool benchmark
//! against a different language ecosystem: which file extensions count,
//! which directories to exclude from the non-MCP Glob/Grep simulation,
//! which file identifies the project for [`crate::server`]'s project
//! detection, and an optional override for the scenario targets
//! (file/symbol names) so the hand-authored Rust baseline can be preserved
//! byte-identically.
//!
//! Only the Rust profile is fully implemented today. TypeScript, Python,
//! Go, and Java are stub files waiting for a Wave 2 agent to fill in the
//! fixture layout, extension list, exclude globs, and either a live
//! target resolver or a `target_override`. Until then, [`by_name`]
//! returns `None` for those languages so the CLI can produce a clean
//! error before any panic can reach the user.

pub mod go;
pub mod java;
pub mod python;
pub mod rust;
pub mod typescript;

use super::targets::ResolvedTargets;

/// Immutable description of one language's benchmark knobs.
///
/// Every field is `'static` because the profile table is stored in a
/// `static` slot inside each language module and must be cheap to return
/// by reference.
#[derive(Debug)]
pub struct LanguageProfile {
    /// Short identifier used on the CLI (`--lang rust`, `--lang typescript`).
    pub name: &'static str,
    /// File extensions that belong to this language, without the leading dot.
    /// Multi-extension languages (e.g. `ts` and `tsx`) list every variant.
    pub extensions: &'static [&'static str],
    /// Glob patterns (matched against the path relative to the fixture root)
    /// that should be excluded from the non-MCP file walk. TypeScript uses
    /// this to skip `node_modules/**`, `dist/**`, and generated `.d.ts`
    /// files that would otherwise swamp the Glob/Grep baselines.
    pub exclude_globs: &'static [&'static str],
    /// Directory (relative to `--fixture-root`, or to the project root when
    /// no fixture root was provided) that contains the fixture code.
    /// Rust uses `""` because it benchmarks against the qartez-mcp repo
    /// itself. TypeScript uses e.g. `"typescript"` inside a shared fixture
    /// root.
    pub fixture_subdir: &'static str,
    /// The file that identifies the project for `qartez_project` and any
    /// other tool that needs a project-root manifest — `Cargo.toml` for
    /// Rust, `package.json` for TS/JS, etc.
    pub project_file: &'static str,
    /// Optional override for scenario targets. When `Some`, the benchmark
    /// harness calls this function instead of running
    /// [`super::targets::resolve`] against the live qartez database. The
    /// Rust profile uses this to keep the committed
    /// `reports/benchmark.{json,md}` baseline byte-identical to the
    /// hand-authored scenarios that existed before the multi-language
    /// refactor.
    pub target_override: Option<fn() -> ResolvedTargets>,
}

/// Resolves a `--lang` CLI string to the corresponding profile.
///
/// Returns `None` for unknown languages AND for languages whose profile
/// is still a stub, so the CLI caller can produce a context-rich error
/// message (listing implemented languages) instead of panicking deep
/// inside the harness.
pub fn by_name(name: &str) -> Option<&'static LanguageProfile> {
    match name {
        "rust" => Some(rust::profile()),
        "typescript" => typescript::maybe_profile(),
        "python" => python::maybe_profile(),
        "go" => go::maybe_profile(),
        "java" => java::maybe_profile(),
        _ => None,
    }
}

/// All language identifiers the CLI is aware of, in the order they should
/// appear in help text. Stub languages are included so Wave 2 agents do
/// not have to touch the CLI string; [`by_name`] is the ground truth for
/// which of these are actually usable today.
pub const KNOWN_LANGUAGES: &[&str] = &["rust", "typescript", "python", "go", "java"];

/// The subset of [`KNOWN_LANGUAGES`] that currently resolves to a real
/// profile via [`by_name`]. The CLI uses this to render a friendly error
/// when the user picks a language whose profile has not been written yet.
pub fn implemented_languages() -> Vec<&'static str> {
    KNOWN_LANGUAGES
        .iter()
        .copied()
        .filter(|n| by_name(n).is_some())
        .collect()
}
