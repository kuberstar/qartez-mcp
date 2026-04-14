//! Python language profile.
//!
//! Fixture: encode/httpx pinned at `4fb9528c2f5ac000441c3634d297e77da23067cd`.
//! Standard layout: `pyproject.toml` at the repo root, the importable
//! package under `httpx/`, tests under `tests/`. The exclude globs strip
//! the usual Python build artefacts (`__pycache__/`, `.venv/`, `build/`,
//! `dist/`, `.tox/`) and the `docs/` directory, which holds prose-only
//! Markdown that the indexer would otherwise walk for nothing.
//!
//! There is no `target_override` because the auto-resolver in
//! [`super::super::targets::resolve`] picks sensible defaults from the
//! live `.qartez/index.db`. The Rust profile uses an override only to
//! keep its committed baseline byte-identical to the pre-refactor
//! scenarios; Python is starting fresh, so the live resolver wins.

use super::LanguageProfile;

pub fn maybe_profile() -> Option<&'static LanguageProfile> {
    Some(profile())
}

pub fn profile() -> &'static LanguageProfile {
    static P: LanguageProfile = LanguageProfile {
        name: "python",
        extensions: &["py"],
        // Standard Python build / virtualenv / cache directories plus the
        // httpx-specific `docs/` tree. `*.pyc` is included so a stray
        // committed bytecode file does not pollute the Glob baseline.
        exclude_globs: &[
            "__pycache__/**",
            ".venv/**",
            "venv/**",
            "*.pyc",
            "build/**",
            "dist/**",
            ".tox/**",
            "docs/**",
        ],
        fixture_subdir: "python",
        project_file: "pyproject.toml",
        target_override: None,
    };
    &P
}
// Rust guideline compliant 2026-04-12
