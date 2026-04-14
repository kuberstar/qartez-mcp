//! TypeScript language profile.
//!
//! Fixture: `colinhacks/zod` pinned at `499df780` (a pnpm monorepo with the
//! real schema-validation source under `packages/zod/src/**`). Scenario
//! targets are resolved live from the `.qartez/` database — no override —
//! so every Wave 2 rebuild of the fixture automatically picks whichever
//! top-PageRank file and smallest-unused function are indexed, without
//! hand-maintaining a pinned list.
//!
//! # Exclude globs
//!
//! The non-MCP sim walker respects the profile's `exclude_globs` so the
//! Glob/Grep baselines only see production TypeScript. Per the Wave 1B
//! notes, zod's `*.test.ts` files double the symbol surface without adding
//! benchmark signal, so they are filtered out alongside the usual
//! `node_modules/**`, build outputs, ambient `.d.ts` declarations, and
//! micro-benchmark `*.bench.ts` files.
//!
//! Note: the walker uses [`ignore::WalkBuilder`] with `.gitignore` honored,
//! so `node_modules/**` is already excluded for this fixture even without
//! the explicit glob. The glob is still listed to make the profile robust
//! against future fixtures that commit a `node_modules/` checkout.
//!
//! # Known gotchas for TypeScript
//!
//! - **Rust-specific `use crate::` regex**: the non-MCP sim steps for
//!   `qartez_deps`, `qartez_context`, and `qartez_impact::ImpactBfs` grep for
//!   `^use crate::` / `use crate::server` which are Rust idioms.
//!   TypeScript imports use `import … from "./util.js";`, so those regexes
//!   match zero lines and the non-MCP baseline for those scenarios is
//!   artificially small. The MCP side still runs correctly; the numbers
//!   just under-report the win.
//! - **Hard-coded Rust line ranges in `qartez_read` / `qartez_calls` /
//!   `qartez_move`**: those scenarios slice the top file with ranges like
//!   `(260, 290)` or `(2180, 2225)` that were chosen from Rust fixtures.
//!   On TypeScript the ranges still point inside the real top file (zod's
//!   `packages/zod/src/v4/core/errors.ts` is long enough), they just happen
//!   to land on unrelated code. This actually stays honest — a real agent
//!   without prior knowledge over-reads too — so we let the ranges stand.
//! - **Auto-picked `qartez_rename_file` source**: the live target resolver
//!   picks `README.md` (lowest-ranked file) as `rename_file_source`. The
//!   MCP `qartez_rename_file` tool operates on that file and the non-MCP
//!   sim's `crate::server` grep matches nothing, so both sides produce
//!   tiny outputs and the row lands as a tie.
//! - **`qartez_unused` on the TS index**: zod exports nearly everything so
//!   the unused-list is short. The non-MCP sim's `^pub …` grep matches
//!   zero lines (Rust syntax) and the row lands as a tie awarded to MCP
//!   on correctness.
//! - **Ghost file entries in the index**: the TypeScript indexer
//!   occasionally creates a phantom file row for aliased re-exports (e.g.
//!   `export { default as frCA } from "./fr-CA.js";` produces a `frCA.ts`
//!   row with no symbols). These zero-byte rows sometimes get picked by
//!   the auto-resolver on an older database; a full re-index usually
//!   clears them. This is a pre-existing indexer quirk and out of scope
//!   for this profile.
// Rust guideline compliant 2026-04-12

use super::LanguageProfile;

pub fn maybe_profile() -> Option<&'static LanguageProfile> {
    Some(profile())
}

pub fn profile() -> &'static LanguageProfile {
    static P: LanguageProfile = LanguageProfile {
        name: "typescript",
        extensions: &["ts", "tsx"],
        exclude_globs: &[
            "node_modules/**",
            "**/node_modules/**",
            "dist/**",
            "**/dist/**",
            "build/**",
            "**/build/**",
            "*.d.ts",
            "**/*.d.ts",
            "**/*.test.ts",
            "**/*.bench.ts",
        ],
        fixture_subdir: "typescript",
        project_file: "package.json",
        // Auto-resolved from the live `.qartez/` database. The Rust
        // profile keeps a hand-coded override so `reports/benchmark.{json,md}`
        // stays byte-identical post-refactor; TypeScript has no such
        // legacy baseline and benefits from a live-picked target set.
        target_override: None,
    };
    &P
}
