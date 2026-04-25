// Throwaway verification test file for the read.rs and grep.rs param-fix
// audit run. Drives the public `call_tool_by_name` JSON dispatch since
// the per-tool methods are `pub(in crate::server)`.

use qartez_mcp::server::QartezServer;
use qartez_mcp::storage::models::SymbolInsert;
use qartez_mcp::storage::{schema, write};
use rusqlite::Connection;
use serde_json::json;
use std::fs;
use tempfile::TempDir;

fn setup_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    conn
}

fn write_test_files(dir: &std::path::Path) {
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("utils.rs"),
        "pub fn helper(name: &str) -> String {\n\
             let sum = compute(1, 2);\n\
             format!(\"Hello, {} ({})\", name, sum)\n\
         }\n\
         \n\
         pub fn compute(x: i32, y: i32) -> i32 {\n\
             x + y\n\
         }\n\
         \n\
         pub fn long_one() -> i32 {\n\
             let line_a = 1;\n\
             let line_b = 2;\n\
             let line_c = 3;\n\
             let line_d = 4;\n\
             line_a + line_b + line_c + line_d\n\
         }\n",
    )
    .unwrap();
    fs::write(src.join("lib.rs"), "pub mod utils;\n").unwrap();
}

fn populate_db(conn: &Connection) {
    let f_utils = write::upsert_file(conn, "src/utils.rs", 1000, 200, "rust", 16).unwrap();
    write::insert_symbols(
        conn,
        f_utils,
        &[
            SymbolInsert {
                name: "helper".into(),
                kind: "function".into(),
                line_start: 1,
                line_end: 4,
                signature: Some("pub fn helper(name: &str) -> String".into()),
                is_exported: true,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            },
            SymbolInsert {
                name: "compute".into(),
                kind: "function".into(),
                line_start: 6,
                line_end: 8,
                signature: Some("pub fn compute(x: i32, y: i32) -> i32".into()),
                is_exported: true,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            },
            SymbolInsert {
                name: "long_one".into(),
                kind: "function".into(),
                line_start: 10,
                line_end: 16,
                signature: Some("pub fn long_one() -> i32".into()),
                is_exported: true,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            },
        ],
    )
    .unwrap();
    write::sync_fts(conn).unwrap();
}

fn setup() -> (QartezServer, TempDir) {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    write_test_files(dir.path());
    let conn = setup_db();
    populate_db(&conn);
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    (server, dir)
}

fn read_ok(server: &QartezServer, args: serde_json::Value) -> String {
    server
        .call_tool_by_name("qartez_read", args)
        .expect("qartez_read must succeed")
}

fn read_err(server: &QartezServer, args: serde_json::Value) -> String {
    server
        .call_tool_by_name("qartez_read", args)
        .expect_err("qartez_read should error")
}

fn grep_ok(server: &QartezServer, args: serde_json::Value) -> String {
    server
        .call_tool_by_name("qartez_grep", args)
        .expect("qartez_grep must succeed")
}

fn grep_err(server: &QartezServer, args: serde_json::Value) -> String {
    server
        .call_tool_by_name("qartez_grep", args)
        .expect_err("qartez_grep should error")
}

// =========================================================================
// 1. context_lines clamp boundary
// =========================================================================

#[test]
fn context_lines_eq_50_does_not_clamp() {
    let (server, _dir) = setup();
    let result = read_ok(
        &server,
        json!({"symbol_name": "helper", "context_lines": 50}),
    );
    assert!(
        !result.contains("clamped"),
        "context_lines=50 (boundary) must NOT clamp, got: {result}"
    );
}

#[test]
fn context_lines_eq_51_clamps_with_note() {
    let (server, _dir) = setup();
    let result = read_ok(
        &server,
        json!({"symbol_name": "helper", "context_lines": 51}),
    );
    assert!(
        result.contains("context_lines=51 clamped to 50"),
        "context_lines=51 must clamp with the original value visible, got: {result}"
    );
}

#[test]
fn context_lines_9999_clamps_with_note() {
    let (server, _dir) = setup();
    let result = read_ok(
        &server,
        json!({"symbol_name": "helper", "context_lines": 9999}),
    );
    assert!(
        result.contains("context_lines=9999 clamped to 50"),
        "context_lines=9999 must clamp and surface the 9999, got: {result}"
    );
}

#[test]
fn context_lines_zero_does_not_warn() {
    let (server, _dir) = setup();
    let result = read_ok(
        &server,
        json!({"symbol_name": "helper", "context_lines": 0}),
    );
    assert!(
        !result.contains("clamped"),
        "context_lines=0 must not produce a clamp warning, got: {result}"
    );
}

#[test]
fn context_lines_none_no_warning() {
    let (server, _dir) = setup();
    let result = read_ok(&server, json!({"symbol_name": "helper"}));
    assert!(
        !result.contains("clamped"),
        "context_lines=None must produce no clamp warning, got: {result}"
    );
}

// =========================================================================
// 2. start_line / end_line / limit + symbol rejection
// =========================================================================

#[test]
fn start_line_plus_symbol_name_rejected() {
    let (server, _dir) = setup();
    let err = read_err(&server, json!({"symbol_name": "helper", "start_line": 1}));
    assert!(
        err.contains("only apply to file-slice mode"),
        "expected file-slice rejection, got: {err}"
    );
}

#[test]
fn end_line_plus_symbol_name_rejected() {
    let (server, _dir) = setup();
    let err = read_err(&server, json!({"symbol_name": "helper", "end_line": 10}));
    assert!(
        err.contains("only apply to file-slice mode"),
        "expected file-slice rejection, got: {err}"
    );
}

#[test]
fn limit_plus_symbol_name_rejected() {
    let (server, _dir) = setup();
    let err = read_err(&server, json!({"symbol_name": "helper", "limit": 5}));
    assert!(
        err.contains("only apply to file-slice mode"),
        "expected file-slice rejection, got: {err}"
    );
}

#[test]
fn start_line_plus_symbols_list_rejected() {
    let (server, _dir) = setup();
    let err = read_err(&server, json!({"symbols": ["helper"], "start_line": 1}));
    assert!(
        err.contains("only apply to file-slice mode"),
        "symbols=[..] + start_line should also reject, got: {err}"
    );
}

#[test]
fn file_slice_mode_with_start_line_works() {
    let (server, _dir) = setup();
    let result = read_ok(
        &server,
        json!({"file_path": "src/utils.rs", "start_line": 1, "end_line": 3}),
    );
    assert!(
        result.contains("L1-3"),
        "expected L1-3 header in file-slice output, got: {result}"
    );
}

#[test]
fn file_path_alone_reads_whole_file() {
    let (server, _dir) = setup();
    let result = read_ok(&server, json!({"file_path": "src/utils.rs"}));
    assert!(
        result.contains("helper") && result.contains("compute"),
        "whole-file read should include both symbols, got: {result}"
    );
}

// =========================================================================
// 3. symbols dedup
// =========================================================================

#[test]
fn symbols_with_three_repeats_resolves_once() {
    let (server, _dir) = setup();
    let result = read_ok(&server, json!({"symbols": ["helper", "helper", "helper"]}));
    let header_pat = "// + helper function";
    let occurrences = result.matches(header_pat).count();
    assert_eq!(
        occurrences, 1,
        "expected single rendered section after dedup, got {occurrences}: {result}"
    );
}

#[test]
fn symbols_repeated_with_other_preserves_order() {
    let (server, _dir) = setup();
    let result = read_ok(&server, json!({"symbols": ["helper", "compute", "helper"]}));
    let helper_pos = result.find("// + helper function");
    let compute_pos = result.find("// + compute function");
    assert!(
        helper_pos.is_some() && compute_pos.is_some(),
        "both symbols must render, got: {result}"
    );
    assert!(
        helper_pos.unwrap() < compute_pos.unwrap(),
        "first-seen order must be preserved (helper before compute)"
    );
    let helper_count = result.matches("// + helper function").count();
    assert_eq!(
        helper_count, 1,
        "helper should be deduped, got {helper_count}"
    );
}

#[test]
fn symbols_with_whitespace_dedupes_against_clean() {
    let (server, _dir) = setup();
    let result = read_ok(&server, json!({"symbols": ["  helper  ", "helper"]}));
    let count = result.matches("// + helper function").count();
    assert_eq!(
        count, 1,
        "whitespace-trimmed input must dedupe against clean form, got {count}"
    );
}

#[test]
fn symbols_case_sensitive_keeps_both() {
    let (server, _dir) = setup();
    let result = read_ok(&server, json!({"symbols": ["HELPER", "helper"]}));
    assert!(
        result.contains("// + helper function"),
        "lowercase helper should resolve, got: {result}"
    );
    assert!(
        result.contains("HELPER") && result.contains("not found"),
        "uppercase HELPER should be in missing list (case-sensitive dedup), got: {result}"
    );
}

#[test]
fn symbols_empty_and_whitespace_dropped() {
    let (server, _dir) = setup();
    let result = read_ok(&server, json!({"symbols": ["", "  ", "helper"]}));
    assert!(
        result.contains("// + helper function"),
        "empty/whitespace entries must be silently dropped, got: {result}"
    );
}

#[test]
fn symbols_all_empty_errors() {
    let (server, _dir) = setup();
    let err = read_err(&server, json!({"symbols": ["", "  "]}));
    assert!(
        err.contains("non-empty") || err.contains("required"),
        "all-empty input must error, got: {err}"
    );
}

// =========================================================================
// 4. grep regex+search_bodies rejection
// =========================================================================

#[test]
fn grep_regex_plus_search_bodies_rejected() {
    let (server, _dir) = setup();
    let err = grep_err(
        &server,
        json!({"query": "foo", "regex": true, "search_bodies": true}),
    );
    assert!(
        err.contains("cannot be combined"),
        "regex+search_bodies should be rejected, got: {err}"
    );
}

#[test]
fn grep_regex_alone_works() {
    let (server, _dir) = setup();
    let result = grep_ok(
        &server,
        json!({"query": "help.*", "regex": true, "search_bodies": false}),
    );
    assert!(
        result.contains("helper"),
        "regex over names should match 'helper', got: {result}"
    );
}

#[test]
fn grep_search_bodies_alone_does_not_trip_combination_guard() {
    let (server, _dir) = setup();
    let r = server.call_tool_by_name(
        "qartez_grep",
        json!({"query": "foo", "regex": false, "search_bodies": true}),
    );
    assert!(
        r.is_ok(),
        "search_bodies alone must reach the FTS path, got: {r:?}"
    );
}

// =========================================================================
// 5. Hint conditional on regex
// =========================================================================

#[test]
fn grep_zero_rows_search_bodies_includes_regex_hint() {
    let (server, _dir) = setup();
    let result = grep_ok(
        &server,
        json!({"query": "zzznotfoundXYZ", "search_bodies": true, "regex": false}),
    );
    assert!(
        result.contains("try regex=true"),
        "0-row body FTS with regex=false should suggest regex, got: {result}"
    );
}

// =========================================================================
// 6. Other grep regression checks
// =========================================================================

#[test]
fn grep_prefix_query_still_works() {
    let (server, _dir) = setup();
    let result = grep_ok(&server, json!({"query": "help*"}));
    assert!(
        result.contains("helper"),
        "FTS prefix should still match, got: {result}"
    );
}

#[test]
fn grep_regex_pinned_case_sensitive_behaviour() {
    // qartez_grep regex branch uses regex::Regex::new(&query) directly without
    // a (?i) wrapper. Pin: HELPER does NOT match indexed `helper`.
    let (server, _dir) = setup();
    let lower = grep_ok(&server, json!({"query": "helper", "regex": true}));
    let upper = grep_ok(&server, json!({"query": "HELPER", "regex": true}));
    assert!(
        lower.contains("helper") && lower.starts_with("Found"),
        "regex over names must hit lowercase 'helper', got: {lower}"
    );
    assert!(
        upper.starts_with("No symbols matching"),
        "qartez_grep regex IS case-sensitive (unlike qartez_find); pin docs got: {upper}"
    );
}
