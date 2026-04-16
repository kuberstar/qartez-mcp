//! Java language profile.
//!
//! Fixture: FasterXML/jackson-core pinned at e6f1276. Standard Maven
//! src/main/java layout, package prefix `tools.jackson.core.*`. Test
//! sources and generated output are excluded from the non-MCP walk
//! because jackson-core's src/test/java nearly doubles the file count
//! without adding library-surface signal - an agent trying to understand
//! the public API would never grep through the test tree first.
//!
//! ## Known gotchas for Java
//!
//! - **Rust-specific `use crate::` regex**: the non-MCP sim steps for
//!   `qartez_deps`, `qartez_context`, and `impact_bfs` grep for `^use crate::`
//!   which is a Rust idiom. Java imports are of the form
//!   `import tools.jackson.core.*;` so those regexes match zero lines and
//!   the non-MCP baseline is artificially small for those three
//!   scenarios. The MCP side still runs correctly; the numbers just
//!   under-report the win.
//! - **Rust-specific line ranges in `qartez_read` / `qartez_calls` /
//!   `qartez_move`**: the sim steps slice the outline target file with
//!   hard-coded ranges (e.g. `(260, 290)` or `(2180, 2225)`) chosen for
//!   the pre-refactor Rust fixture. Those ranges over-read on Java,
//!   which is actually more honest - a real agent exploring the codebase
//!   without prior knowledge also over-reads - so we let the ranges
//!   stand instead of patching `scenarios.rs`.
//! - **Zero file-level edges**: Java imports reference fully-qualified
//!   package paths rather than relative paths, so the tree-sitter-based
//!   walker emits no cross-file edges and PageRank flattens to `1/N`.
//!   The profile used to ship a hand-coded `jackson_core_targets`
//!   override so `get_files_ranked` would not hand back whichever
//!   non-Java file SQLite happened to insert first; the auto-resolver
//!   now handles this itself via a symbol-count tiebreaker combined
//!   with the profile's primary-extension and `exclude_globs` filters,
//!   so the override is no longer needed.

use super::LanguageProfile;

pub fn maybe_profile() -> Option<&'static LanguageProfile> {
    Some(profile())
}

pub fn profile() -> &'static LanguageProfile {
    static P: LanguageProfile = LanguageProfile {
        name: "java",
        extensions: &["java"],
        exclude_globs: &[
            // jackson-core's test tree nearly doubles the file count with
            // content that an agent exploring the public API would not
            // touch. Excluding it keeps the non-MCP baseline honest.
            "src/test/**",
            // Maven build output directory.
            "target/**",
            // Gradle build output directories, defensive for downstream
            // Java fixtures that may use Gradle instead of Maven.
            "build/**",
            ".gradle/**",
            // Annotation-processor / code-generator output.
            "generated/**",
            "**/generated-sources/**",
        ],
        fixture_subdir: "java",
        project_file: "pom.xml",
        target_override: None,
    };
    &P
}
