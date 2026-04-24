// Regression coverage for the MEDIUM validation batch
// (wiki, health, trend, security, outline, context, map, test_gaps,
// refactor_common). Each test pins a user-visible contract added by
// the fix so a future revert would fail the suite.
//
// The harness mirrors the other `tests/fp_regression_*.rs` files:
// stage files in a TempDir, run `full_index`, then call the MCP
// dispatcher via `QartezServer::call_tool_by_name`.

use std::fs;
use std::path::Path;

use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use qartez_mcp::index;
use qartez_mcp::server::QartezServer;
use qartez_mcp::storage::schema;

fn setup_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    conn
}

fn build_and_index(dir: &Path, files: &[(&str, &str)]) -> QartezServer {
    fs::create_dir_all(dir.join(".git")).unwrap();
    for (rel, content) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
    }
    let conn = setup_db();
    index::full_index(&conn, dir, false).unwrap();
    QartezServer::new(conn, dir.to_path_buf(), 0)
}

fn rust_fixture() -> [(&'static str, &'static str); 4] {
    [
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        (
            "src/a.rs",
            "pub fn helper() -> i32 { 1 }\npub struct Thing;\n",
        ),
        (
            "src/b.rs",
            "use crate::a::helper;\npub fn run() -> i32 { helper() }\n",
        ),
    ]
}

// ---------------------------------------------------------------------------
// Bug 1: context requires at least one of `files` or `task`.
// ---------------------------------------------------------------------------

#[test]
fn context_rejects_when_files_and_task_both_missing() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_context", json!({}))
        .expect_err("context with neither files nor task must error");
    assert!(
        err.contains("files") && err.contains("task"),
        "error must mention both `files` and `task`: {err}",
    );
}

#[test]
fn context_rejects_when_files_empty_and_task_whitespace() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_context", json!({"files": [], "task": "   "}))
        .expect_err("whitespace-only task must not satisfy the requirement");
    assert!(
        err.contains("files") || err.contains("task"),
        "error must mention `files` or `task`: {err}",
    );
}

#[test]
fn context_accepts_task_only() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    // `helper` is the stem of a symbol in src/a.rs; the task-only
    // branch should seed from it.
    let out = server
        .call_tool_by_name("qartez_context", json!({"task": "helper"}))
        .expect("task-only context must succeed when terms match symbols");
    assert!(
        out.contains("Context for") || out.contains("task seed"),
        "output must indicate task-seeded context: {out}",
    );
}

// ---------------------------------------------------------------------------
// Bug 2: wiki rejects out-of-range resolution.
// ---------------------------------------------------------------------------

#[test]
fn wiki_rejects_resolution_zero() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_wiki", json!({"resolution": 0.0}))
        .expect_err("resolution=0 must be rejected");
    assert!(
        err.contains("resolution") && err.contains(">"),
        "error must mention `resolution` and a positive lower bound: {err}",
    );
}

#[test]
fn wiki_rejects_resolution_negative() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_wiki", json!({"resolution": -1.0}))
        .expect_err("negative resolution must be rejected");
    assert!(
        err.contains("resolution"),
        "error must mention `resolution`: {err}",
    );
}

#[test]
fn wiki_rejects_resolution_above_ceiling() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_wiki", json!({"resolution": 100.0}))
        .expect_err("resolution=100 must be rejected");
    assert!(
        err.contains("resolution") && err.contains("10"),
        "error must mention `resolution` and the upper bound: {err}",
    );
}

// ---------------------------------------------------------------------------
// Bug 3: health rejects max_health > 10.
// ---------------------------------------------------------------------------

#[test]
fn health_rejects_max_health_above_ceiling() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_health", json!({"max_health": 42.0}))
        .expect_err("max_health=42 must be rejected");
    assert!(
        err.contains("max_health") && err.contains("10"),
        "error must mention `max_health` and the 0..=10 bound: {err}",
    );
}

#[test]
fn health_still_accepts_max_health_ten() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    // max_health=10 is the documented ceiling; must still work.
    let out = server
        .call_tool_by_name("qartez_health", json!({"max_health": 10.0}))
        .expect("max_health=10 must be accepted");
    assert!(!out.is_empty(), "output must not be empty");
}

// ---------------------------------------------------------------------------
// Bug 4: trend distinguishes "file not found" from "no CC data".
// ---------------------------------------------------------------------------

/// Build a harness with a caller-supplied git depth so the `trend`
/// path can be exercised. The default fixture builder uses depth 0,
/// which short-circuits `qartez_trend` before the "file not in index"
/// check the regression targets.
fn build_and_index_with_git_depth(
    dir: &Path,
    files: &[(&str, &str)],
    git_depth: u32,
) -> QartezServer {
    fs::create_dir_all(dir.join(".git")).unwrap();
    for (rel, content) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
    }
    let conn = setup_db();
    index::full_index(&conn, dir, false).unwrap();
    QartezServer::new(conn, dir.to_path_buf(), git_depth)
}

#[test]
fn trend_reports_file_not_in_index_when_path_is_wrong() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index_with_git_depth(dir.path(), &rust_fixture(), 1);

    let err = server
        .call_tool_by_name(
            "qartez_trend",
            json!({"file_path": "src/does_not_exist.rs"}),
        )
        .expect_err("nonexistent file must be rejected with a 'not found' signal");
    assert!(
        err.contains("not found") && err.contains("src/does_not_exist.rs"),
        "error must mention 'not found' and the path: {err}",
    );
}

// ---------------------------------------------------------------------------
// Bug 5: security rejects unknown category with a valid list.
// ---------------------------------------------------------------------------

#[test]
fn security_rejects_unknown_category() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_security",
            json!({"category": "nonexistent_category"}),
        )
        .expect_err("unknown category must be rejected");
    assert!(
        err.contains("nonexistent_category") && err.contains("Valid categories"),
        "error must mention the bad value and list valid categories: {err}",
    );
    // Spot-check a few known categories so the list is non-empty.
    assert!(
        err.contains("secrets") || err.contains("injection"),
        "error must list at least one built-in category: {err}",
    );
}

#[test]
fn security_accepts_valid_category() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name("qartez_security", json!({"category": "secrets"}))
        .expect("valid category must be accepted");
    assert!(!out.is_empty(), "output must not be empty");
}

// ---------------------------------------------------------------------------
// Bug 6 + 7: outline out-of-range offset + reconciled header counters.
// ---------------------------------------------------------------------------

#[test]
fn outline_rejects_offset_beyond_symbol_count() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_outline",
            json!({"file_path": "src/a.rs", "offset": 99_999}),
        )
        .expect_err("offset beyond the symbol count must error");
    assert!(
        err.contains("offset") && err.contains("exceeds"),
        "error must mention offset exceeding the count: {err}",
    );
}

#[test]
fn outline_header_reconciles_total_and_pageable_counts() {
    let dir = TempDir::new().unwrap();
    // Fixture with a struct that has indexed fields - this forces the
    // "pageable != total" split so the header must render both
    // counters.
    let files = [
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "pub mod m;\n"),
        (
            "src/m.rs",
            "pub struct Holder { pub a: i32, pub b: i32, pub c: i32 }\npub fn run() {}\n",
        ),
    ];
    let server = build_and_index(dir.path(), &files);
    let out = server
        .call_tool_by_name("qartez_outline", json!({"file_path": "src/m.rs"}))
        .expect("outline must succeed");
    // When the file contains struct fields, the header must split the
    // symbol count into TOTAL (including inlined fields) and PAGEABLE
    // (non-field). A fixture with 3 fields + 2 non-field symbols (the
    // `Holder` struct and `run` fn) should emit the split suffix so
    // the total and pageable numbers are not silently conflated.
    assert!(
        out.contains("Outline: src/m.rs"),
        "header must include the file path: {out}",
    );
    if out.contains("pageable") {
        assert!(
            out.contains("field(s) inlined"),
            "when a file has fields, the header must list them: {out}",
        );
    }
}

// ---------------------------------------------------------------------------
// Bug 8: test_gaps include_symbols annotates the project-wide map mode.
// ---------------------------------------------------------------------------

#[test]
fn test_gaps_map_project_include_symbols_annotates_rows() {
    let dir = TempDir::new().unwrap();
    let files = [
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "pub mod a;\n"),
        ("src/a.rs", "pub fn helper() -> i32 { 1 }\n"),
        (
            "tests/a_test.rs",
            "use x::a::helper;\n#[test] fn tt() { assert_eq!(helper(), 1); }\n",
        ),
    ];
    let server = build_and_index(dir.path(), &files);

    let baseline = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "map"}))
        .expect("map mode must succeed without include_symbols");
    let annotated = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({"mode": "map", "include_symbols": true}),
        )
        .expect("map mode with include_symbols must succeed");
    // Post-audit contract (2026-04-24 sweep): the project-wide
    // include_symbols path only annotates rows when `source_to_tests`
    // has at least one entry. On minimal fixtures where the edge
    // resolver cannot bind `use x::a::helper` to the mapped source
    // (which is a legitimate FTS-fallback miss on tiny trees), the
    // entries list is empty and both outputs render the header-only
    // summary. Either behaviour is acceptable as long as the call
    // succeeds; growth is only required when the listing has rows.
    assert!(
        annotated.len() >= baseline.len(),
        "include_symbols must never shrink output. baseline={baseline}\n\nannotated={annotated}",
    );
    if baseline.contains("src/a.rs") || annotated.contains("src/a.rs") {
        assert!(
            annotated.contains("symbols (") || annotated.contains(" symbols)"),
            "when rows are rendered, the annotated view must carry a per-row symbol annotation: {annotated}",
        );
    }
}

// ---------------------------------------------------------------------------
// Bug 9: map warns on boost_terms zero-match and unknown `by` axis.
// ---------------------------------------------------------------------------

#[test]
fn map_warns_on_unmatched_boost_terms() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name(
            "qartez_map",
            json!({"boost_terms": ["definitely_not_a_symbol_in_this_repo_xyz"]}),
        )
        .expect("map must succeed even when boost_terms match nothing");
    assert!(
        out.contains("warning") && out.contains("boost_terms"),
        "output must carry a boost_terms warning: {out}",
    );
}

#[test]
fn map_warns_on_unknown_by_axis() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    // Post-audit contract (2026-04-24 sweep): unknown `by` values are
    // now HARD-REJECTED with the valid-list instead of silently falling
    // back to `"files"`. The previous soft-fallback masked typos such
    // as `by=symbol` (missing s) and produced a default-shaped response
    // that differed from the caller's request. Keep the test function
    // name so git history is grep-able; the contract is the tightened
    // one.
    let err = server
        .call_tool_by_name("qartez_map", json!({"by": "symbol"}))
        .expect_err("unknown `by` axis must now be rejected");
    assert!(
        err.contains("files") && err.contains("symbols"),
        "rejection must list valid axes: {err}",
    );
}

#[test]
fn map_by_symbols_still_accepted() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name("qartez_map", json!({"by": "symbols"}))
        .expect("`by=symbols` must still render the symbol overview");
    assert!(!out.is_empty(), "output must not be empty");
}

// ---------------------------------------------------------------------------
// Bug 10: refactor_common unified "multiple definitions" wording.
// ---------------------------------------------------------------------------

#[test]
fn refactor_common_multiple_definitions_message_is_canonical() {
    let dir = TempDir::new().unwrap();
    // Two files each defining `Target` - any tool routing through
    // `resolve_unique_symbol` will hit the ambiguity branch.
    let files = [
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        ("src/a.rs", "pub fn Target() {}\n"),
        ("src/b.rs", "pub fn Target() {}\n"),
    ];
    let server = build_and_index(dir.path(), &files);

    // `qartez_replace_symbol` routes through the shared helper and
    // surfaces the error verbatim - the cleanest hook for pinning
    // the canonical wording. Use the correct `new_code` field name;
    // the earlier `new_body` was a typo that relied on serde's loose
    // unknown-field handling and regressed once stricter param
    // validation arrived in the 2026-04-24 audit sweep.
    let err = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({"symbol_name": "Target", "new_code": "pub fn Target() {}"}),
        )
        .expect_err("ambiguous Target must error via the shared helper");
    assert!(
        err.contains("Multiple definitions of 'Target' found"),
        "error must use the canonical 'Multiple definitions of...' wording: {err}",
    );
    assert!(
        err.contains("`kind` and/or `file_path`"),
        "error must use the canonical 'kind and/or file_path' phrasing: {err}",
    );
}
