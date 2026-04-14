//! Go language profile.
//!
//! Fixture: [`spf13/cobra`] pinned at `6dec1ae26659a130bdb4c985768d1853b0e1bc06`
//! via `benchmarks/fixtures.toml`. Cobra is a small (~3 MB, ~36 non-test
//! `.go` files) CLI-framework package with a flat layout: every `*.go`
//! file at the repo root belongs to `package cobra`, with a secondary
//! `doc/` sub-package for the man-page / markdown / rest / yaml doc
//! generators. There is no `vendor/` tree in this commit.
//!
//! Go has no `pub` keyword — package visibility is driven entirely by
//! identifier capitalization — so the qartez language backend owns the
//! `is_exported` decision and this profile does not need any
//! Go-specific visibility wiring. The non-MCP sim, however, still uses
//! the `use crate::`-style regex that the Rust scenarios baked into
//! `scenarios::deps_steps` / `context_steps` / `impact_steps`; those
//! will produce empty or near-empty output on a Go fixture. This is an
//! intentional asymmetry — patching `scenarios.rs` would drift the Rust
//! baseline — and the gotcha is documented in `reports/benchmark-go.md`.
//!
//! Targets are resolved live against `.qartez/index.db` via
//! [`super::super::targets::resolve`]; no `target_override` is needed.
//!
//! [`spf13/cobra`]: https://github.com/spf13/cobra

use super::LanguageProfile;

pub fn maybe_profile() -> Option<&'static LanguageProfile> {
    Some(profile())
}

pub fn profile() -> &'static LanguageProfile {
    static P: LanguageProfile = LanguageProfile {
        name: "go",
        extensions: &["go"],
        exclude_globs: &[
            // Vendored deps are absent in this commit but other Go
            // fixtures (and most real-world Go repos) keep a vendored
            // module cache here, so we exclude it preemptively to keep
            // the non-MCP baseline comparable across fixture bumps.
            "vendor/**",
            // Test files double the Glob/Grep file count in a way that
            // would inflate the non-MCP baseline without adding any
            // product-code signal to the scenarios.
            "**/*_test.go",
            // Cobra's Hugo-based documentation site lives under `site/`
            // and is almost entirely Markdown / asset content, not Go.
            "site/**",
            // `doc/` contains the man/md/rest/yaml docs generators; they
            // are a separate Go package (`doc`) with their own tests and
            // are noise for the blast-radius / deps scenarios which
            // target the top-level `cobra` package.
            "doc/**",
        ],
        fixture_subdir: "go",
        project_file: "go.mod",
        target_override: None,
    };
    &P
}
