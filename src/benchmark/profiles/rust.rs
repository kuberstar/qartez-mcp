//! Rust language profile.
//!
//! The only fully-implemented profile. Its [`target_override`] returns the
//! exact scenario targets that the pre-refactor `scenarios.rs` hard-coded,
//! so the committed `reports/benchmark.{json,md}` baseline remains
//! byte-identical for `--lang rust` after the multi-language refactor.
//!
//! [`target_override`]: super::LanguageProfile::target_override

use super::super::targets::ResolvedTargets;
use super::LanguageProfile;

pub fn profile() -> &'static LanguageProfile {
    static P: LanguageProfile = LanguageProfile {
        name: "rust",
        extensions: &["rs"],
        exclude_globs: &[],
        fixture_subdir: "",
        project_file: "Cargo.toml",
        target_override: Some(rust_targets),
    };
    &P
}

/// Returns the hard-coded Rust targets that the pre-refactor scenarios
/// used. These are chosen to keep the benchmark output byte-identical:
/// every file/symbol name here is taken verbatim from the old `*_args`
/// / `*_steps` function pairs in `scenarios.rs` prior to the refactor.
fn rust_targets() -> ResolvedTargets {
    ResolvedTargets {
        top_pagerank_file: "src/server/mod.rs".to_string(),
        top_pagerank_symbol: "QartezServer".to_string(),
        most_referenced_symbol: "find_symbol_by_name".to_string(),
        smallest_exported_fn: "truncate_path".to_string(),
        outline_target_file: "src/server/mod.rs".to_string(),
        deps_target_file: "src/server/mod.rs".to_string(),
        impact_target_file: "src/storage/read.rs".to_string(),
        impact_seed_stem: "storage::read".to_string(),
        project_file: "Cargo.toml".to_string(),
        grep_prefix: "find_symbol".to_string(),
        move_symbol: "capitalize_kind".to_string(),
        move_destination: "src/server/helpers.rs".to_string(),
        rename_file_source: "src/server/mod.rs".to_string(),
        rename_file_destination: "src/server/server.rs".to_string(),
        calls_target_symbol: "build_overview".to_string(),
        rename_new_name: "trunc_path".to_string(),
        hierarchy_target_symbol: "LanguageSupport".to_string(),
    }
}
