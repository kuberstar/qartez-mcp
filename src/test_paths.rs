// Rust guideline compliant 2026-04-22

//! Path- and language-based classification for test reporting.
//!
//! Shared by `graph::security::scan` and `server::helpers`. Both classify
//! by file path; keeping one source of truth avoids drift.
//!
//! `is_testable_source_language` is the companion predicate used by the
//! test-gaps tool: shell scripts, manifests, CI YAML, and Dockerfiles
//! are indexed for dependency / security analysis but are not the kind
//! of file that can meaningfully grow a unit test, so listing them as
//! "untested source" is always a false positive.

/// True when `path` points at a file that should be treated as test code.
pub(crate) fn is_test_path(path: &str) -> bool {
    const TEST_DIR_PREFIXES: &[&str] = &["tests/", "test/", "benches/", "__tests__/", "spec/"];
    const TEST_DIR_SUBSTRINGS: &[&str] =
        &["/tests/", "/test/", "/benches/", "/__tests__/", "/spec/"];
    const TEST_FILE_EXACT: &[&str] = &["test.rs", "tests.rs"];
    const TEST_FILE_SUFFIXES: &[&str] = &[
        "_test.rs",
        "_tests.rs",
        "_test.go",
        "_test.dart",
        ".test.ts",
        ".spec.ts",
        ".test.tsx",
        ".spec.tsx",
        ".test.js",
        ".spec.js",
        ".test.jsx",
        ".spec.jsx",
        "_test.py",
        "Test.java",
        "Tests.java",
        "Test.kt",
        "Tests.kt",
        "_spec.rb",
        "Test.cs",
        "Tests.cs",
    ];

    if TEST_DIR_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return true;
    }
    if TEST_DIR_SUBSTRINGS.iter().any(|p| path.contains(p)) {
        return true;
    }
    let Some(name) = path.rsplit('/').next() else {
        return false;
    };
    if TEST_FILE_EXACT.contains(&name) {
        return true;
    }
    if TEST_FILE_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        return true;
    }
    name.starts_with("test_") && name.ends_with(".py")
}

/// True when `language` (as stored on `FileRow::language`) is a source
/// language for which writing unit tests is meaningful. The whitelist is
/// deliberately narrow and matches the exact strings emitted by each
/// `LanguageSupport` implementation's `language_name` method.
pub(crate) fn is_testable_source_language(language: &str) -> bool {
    matches!(
        language,
        "rust"
            | "typescript"
            | "python"
            | "go"
            | "java"
            | "kotlin"
            | "swift"
            | "csharp"
            | "ruby"
            | "php"
            | "cpp"
            | "c"
            | "scala"
            | "dart"
            | "elixir"
            | "lua"
            | "zig"
            | "haskell"
            | "ocaml"
            | "r"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_prefixes() {
        assert!(is_test_path("tests/foo.rs"));
        assert!(is_test_path("test/bar.go"));
        assert!(is_test_path("benches/baz.rs"));
        assert!(is_test_path("__tests__/snap.tsx"));
        assert!(is_test_path("spec/widget_spec.rb"));
    }

    #[test]
    fn dir_substrings() {
        assert!(is_test_path("src/tests/util.rs"));
        assert!(is_test_path("crate/test/helpers.go"));
        assert!(is_test_path("pkg/__tests__/index.ts"));
    }

    #[test]
    fn rust_file_names() {
        assert!(is_test_path("src/lib/test.rs"));
        assert!(is_test_path("src/lib/tests.rs"));
        assert!(is_test_path("src/server/quality_tests.rs"));
        assert!(is_test_path("src/foo_test.rs"));
    }

    #[test]
    fn js_ts_suffixes() {
        assert!(is_test_path("components/foo.test.ts"));
        assert!(is_test_path("components/foo.spec.tsx"));
        assert!(is_test_path("components/foo.test.js"));
        assert!(is_test_path("components/foo.spec.jsx"));
    }

    #[test]
    fn python_patterns() {
        assert!(is_test_path("pkg/mod_test.py"));
        assert!(is_test_path("pkg/test_mod.py"));
        assert!(!is_test_path("pkg/tester.py"));
    }

    #[test]
    fn production_paths() {
        assert!(!is_test_path("src/main.rs"));
        assert!(!is_test_path("src/server/mod.rs"));
        assert!(!is_test_path("src/graph/security.rs"));
        assert!(!is_test_path("test_data.json"));
    }

    #[test]
    fn narrow_predicate_was_missing_quality_tests_rs() {
        assert!(is_test_path("qartez-public/src/server/quality_tests.rs"));
    }

    #[test]
    fn testable_languages_accept_core_sources() {
        for lang in [
            "rust",
            "typescript",
            "python",
            "go",
            "java",
            "kotlin",
            "swift",
            "csharp",
            "ruby",
            "php",
            "cpp",
            "c",
            "scala",
            "dart",
            "elixir",
            "lua",
            "zig",
            "haskell",
            "ocaml",
            "r",
        ] {
            assert!(is_testable_source_language(lang), "{lang} must be testable");
        }
    }

    #[test]
    fn testable_languages_reject_config_and_scripts() {
        for lang in [
            "bash",
            "toml",
            "yaml",
            "json",
            "markdown",
            "dockerfile",
            "makefile",
            "jenkinsfile",
            "nginx",
            "helm",
            "sql",
            "protobuf",
            "hcl",
            "css",
            "caddyfile",
            "starlark",
            "jsonnet",
            "nix",
            "systemd",
            "",
        ] {
            assert!(
                !is_testable_source_language(lang),
                "{lang} must not be testable"
            );
        }
    }
}
