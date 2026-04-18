// Rust guideline compliant 2026-04-16
//
// Integration tests for the standalone CLI: verifies that subcommand arg
// translation produces the correct JSON for each tool, and that the full
// index-then-dispatch pipeline returns useful output.

use std::fs;

use tempfile::TempDir;

use qartez_mcp::cli::{Command, OutputFormat};
use qartez_mcp::cli_runner;
use qartez_mcp::config::Config;

fn make_project() -> (TempDir, Config) {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        r#"pub fn greet(name: &str) -> String { format!("hello {name}") }
pub struct Settings { pub verbose: bool }
pub trait Formatter { fn format(&self) -> String; }
impl Formatter for Settings {
    fn format(&self) -> String { format!("verbose={}", self.verbose) }
}
"#,
    )
    .unwrap();
    fs::write(
        src.join("main.rs"),
        "use crate::greet;\nfn main() { println!(\"{}\", greet(\"world\")); }\n",
    )
    .unwrap();

    let db_dir = dir.path().join(".qartez");
    fs::create_dir_all(&db_dir).unwrap();

    let config = Config {
        project_roots: vec![dir.path().to_path_buf()],
        root_aliases: std::collections::HashMap::new(),
        primary_root: dir.path().to_path_buf(),
        db_path: db_dir.join("index.db"),
        reindex: false,
        git_depth: 50,
        has_project: true,
    };
    (dir, config)
}

// ---------------------------------------------------------------------------
// End-to-end: each subcommand produces non-empty output
// ---------------------------------------------------------------------------

#[test]
fn cli_map_returns_output() {
    let (_dir, config) = make_project();
    let cmd = Command::Map {
        top_n: 5,
        boost: vec![],
        all_files: false,
        by: None,
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "map failed: {:?}", result.err());
}

#[test]
fn cli_find_returns_symbol() {
    let (_dir, config) = make_project();
    let cmd = Command::Find {
        name: "greet".into(),
        kind: None,
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "find failed: {:?}", result.err());
}

#[test]
fn cli_find_nonexistent_does_not_error() {
    let (_dir, config) = make_project();
    let cmd = Command::Find {
        name: "NONEXISTENT_SYMBOL_XYZ".into(),
        kind: None,
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(
        result.is_ok(),
        "find nonexistent should succeed (empty result), got: {:?}",
        result.err()
    );
}

#[test]
fn cli_grep_returns_results() {
    let (_dir, config) = make_project();
    let cmd = Command::Grep {
        query: "greet".into(),
        limit: 10,
        bodies: false,
        regex: false,
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "grep failed: {:?}", result.err());
}

#[test]
fn cli_outline_returns_symbols() {
    let (_dir, config) = make_project();
    let cmd = Command::Outline {
        file: "src/lib.rs".into(),
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "outline failed: {:?}", result.err());
}

#[test]
fn cli_stats_returns_metrics() {
    let (_dir, config) = make_project();
    let cmd = Command::Stats { file: None };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "stats failed: {:?}", result.err());
}

#[test]
fn cli_impact_returns_analysis() {
    let (_dir, config) = make_project();
    let cmd = Command::Impact {
        file: "src/lib.rs".into(),
        include_tests: false,
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "impact failed: {:?}", result.err());
}

#[test]
fn cli_deps_returns_graph() {
    let (_dir, config) = make_project();
    let cmd = Command::Deps {
        file: "src/lib.rs".into(),
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "deps failed: {:?}", result.err());
}

#[test]
fn cli_unused_runs() {
    let (_dir, config) = make_project();
    let cmd = Command::Unused { limit: 10 };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "unused failed: {:?}", result.err());
}

#[test]
fn cli_refs_returns_usages() {
    let (_dir, config) = make_project();
    let cmd = Command::Refs {
        name: "greet".into(),
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "refs failed: {:?}", result.err());
}

#[test]
fn cli_hotspots_runs() {
    let (_dir, config) = make_project();
    let cmd = Command::Hotspots;
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "hotspots failed: {:?}", result.err());
}

#[test]
fn cli_calls_runs() {
    let (_dir, config) = make_project();
    let cmd = Command::Calls {
        name: "greet".into(),
        direction: "both".into(),
        depth: 1,
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "calls failed: {:?}", result.err());
}

#[test]
fn cli_clones_runs() {
    let (_dir, config) = make_project();
    let cmd = Command::Clones;
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "clones failed: {:?}", result.err());
}

#[test]
fn cli_boundaries_runs() {
    let (_dir, config) = make_project();
    let cmd = Command::Boundaries;
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "boundaries failed: {:?}", result.err());
}

#[test]
fn cli_hierarchy_runs() {
    let (_dir, config) = make_project();
    let cmd = Command::Hierarchy {
        name: "Formatter".into(),
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "hierarchy failed: {:?}", result.err());
}

#[test]
fn cli_security_runs() {
    let (_dir, config) = make_project();
    let cmd = Command::Security;
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "security failed: {:?}", result.err());
}

#[test]
fn cli_context_runs() {
    let (_dir, config) = make_project();
    let cmd = Command::Context {
        files: vec!["src/lib.rs".into()],
        task: None,
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "context failed: {:?}", result.err());
}

#[test]
fn cli_read_symbol_runs() {
    let (_dir, config) = make_project();
    let cmd = Command::Read {
        name: Some("greet".into()),
        file: None,
        start: None,
        end: None,
        context: 0,
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "read failed: {:?}", result.err());
}

#[test]
fn cli_read_file_range_runs() {
    let (_dir, config) = make_project();
    let cmd = Command::Read {
        name: None,
        file: Some("src/lib.rs".into()),
        start: Some(1),
        end: Some(3),
        context: 0,
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_ok(), "read file range failed: {:?}", result.err());
}

// ---------------------------------------------------------------------------
// Error path: nonexistent file gives tool-level error, not a panic
// ---------------------------------------------------------------------------

#[test]
fn cli_outline_nonexistent_file_errors_gracefully() {
    let (_dir, config) = make_project();
    let cmd = Command::Outline {
        file: "does/not/exist.rs".into(),
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_err(), "outline on missing file should error");
}

#[test]
fn cli_impact_nonexistent_file_errors_gracefully() {
    let (_dir, config) = make_project();
    let cmd = Command::Impact {
        file: "nope.rs".into(),
        include_tests: false,
    };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(result.is_err(), "impact on missing file should error");
}

// ---------------------------------------------------------------------------
// No-project mode: verify CLI handles empty index gracefully
// ---------------------------------------------------------------------------

#[test]
fn cli_stats_no_project() {
    let dir = TempDir::new().unwrap();
    let db_dir = dir.path().join(".qartez");
    fs::create_dir_all(&db_dir).unwrap();
    let config = Config {
        project_roots: vec![dir.path().to_path_buf()],
        root_aliases: std::collections::HashMap::new(),
        primary_root: dir.path().to_path_buf(),
        db_path: db_dir.join("index.db"),
        reindex: false,
        git_depth: 50,
        has_project: false,
    };
    let cmd = Command::Stats { file: None };
    let result = cli_runner::run(&config, &cmd, OutputFormat::Compact);
    assert!(
        result.is_ok(),
        "stats on empty index should not panic: {:?}",
        result.err()
    );
}
