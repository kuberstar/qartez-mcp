// Rust guideline compliant 2026-04-25
//
// Final-pass audit fixes landed 2026-04-25:
//   - refactor_plan: hard CC=5 floor on god-step entries so the
//     "decision points past review/test budget" rationale is not
//     emitted for trivially-complex functions when a caller passes
//     `min_complexity=1`.
//   - diff_impact: co-change source labels promote `parent/basename`
//     when multiple sources in the same partner row share a basename
//     (e.g. `server/mod.rs` + `index/mod.rs`).
//   - calls: depth-clamp warning placed in the response footer to
//     match the `qartez_refactor_plan` / `qartez_trend` placement
//     convention.
//   - refs: surfaces an explicit "X test ref(s) hidden" footer when
//     `include_tests=false` actually suppressed any test-path importer.

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

fn build_and_index(dir: &Path) -> QartezServer {
    fs::create_dir_all(dir.join(".git")).unwrap();
    let conn = setup_db();
    index::full_index(&conn, dir, false).unwrap();
    QartezServer::new(conn, dir.to_path_buf(), 0)
}

fn write_cargo_manifest(dir: &Path) {
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
}

#[test]
fn refactor_plan_skips_trivial_cc_god_entries() {
    // Caller passing `min_complexity=1` with a tiny CC=1 function
    // would previously surface a god-step whose rationale claimed the
    // body was "past the usual review/test budget" - false for CC=1.
    // The hard floor of 5 keeps the rationale honest.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub mod tiny;\n").unwrap();
    // Single straight-line function with many lines but CC=1.
    let body: String = (0..80)
        .map(|i| format!("    let x{i} = {i};\n"))
        .collect::<Vec<_>>()
        .join("");
    let tiny = format!("pub fn straight_line() {{\n{body}}}\n");
    fs::write(src.join("tiny.rs"), tiny).unwrap();

    let server = build_and_index(root);
    let out = server
        .call_tool_by_name(
            "qartez_refactor_plan",
            json!({
                "file_path": "src/tiny.rs",
                "min_complexity": 1,
                "min_lines": 5,
                "limit": 0,
            }),
        )
        .expect("qartez_refactor_plan must succeed");
    assert!(
        !out.contains("1 decision points")
            && !out.contains("2 decision points")
            && !out.contains("3 decision points")
            && !out.contains("4 decision points"),
        "rationale must not claim CC<5 is past the review/test budget. Got:\n{out}"
    );
}

#[test]
fn refs_emits_test_refs_hidden_footer_when_filter_active() {
    // Hub symbol referenced from `tests/*.rs`. With include_tests=false
    // the test-path importer is dropped from the ref list AND the
    // response carries a footer reporting how many test rows were
    // suppressed so callers know to re-run with include_tests=true.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    let tests = root.join("tests");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&tests).unwrap();
    fs::write(src.join("lib.rs"), "pub mod hub;\n").unwrap();
    fs::write(src.join("hub.rs"), "pub fn hot() {}\n").unwrap();
    fs::write(
        tests.join("integration.rs"),
        "use x::hub::hot;\n#[test] fn t() { hot(); }\n",
    )
    .unwrap();

    let server = build_and_index(root);
    let out = server
        .call_tool_by_name(
            "qartez_refs",
            json!({
                "symbol": "hot",
                "include_tests": false,
                "token_budget": 20000,
            }),
        )
        .expect("qartez_refs must succeed");
    assert!(
        out.contains("test ref(s) hidden by include_tests=false"),
        "include_tests=false must report suppression count when test-path refs were dropped:\n{out}"
    );
}

#[test]
fn refs_default_does_not_emit_hidden_footer() {
    // Default include_tests=true must not introduce the suppression
    // footer because nothing was hidden.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub mod hub;\n").unwrap();
    fs::write(src.join("hub.rs"), "pub fn warm() {}\n").unwrap();
    let server = build_and_index(root);
    let out = server
        .call_tool_by_name(
            "qartez_refs",
            json!({
                "symbol": "warm",
                "token_budget": 20000,
            }),
        )
        .expect("qartez_refs must succeed");
    assert!(
        !out.contains("test ref(s) hidden by include_tests=false"),
        "default path must not advertise a suppression count:\n{out}"
    );
}

#[test]
fn calls_depth_clamp_warning_lives_in_footer() {
    // The clamp warning must follow the resolved symbol header so
    // qartez_calls matches the placement convention used by
    // qartez_refactor_plan and qartez_trend.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub mod a;\n").unwrap();
    fs::write(
        src.join("a.rs"),
        "pub fn root() { leaf(); }\npub fn leaf() {}\n",
    )
    .unwrap();
    let server = build_and_index(root);
    let out = server
        .call_tool_by_name(
            "qartez_calls",
            json!({
                "name": "root",
                "depth": 999,
                "direction": "callees",
            }),
        )
        .expect("qartez_calls must succeed");
    let warning_pos = out.find("was clamped").expect("warning must appear");
    let header_pos = out.find("root").expect("symbol header must appear");
    assert!(
        warning_pos > header_pos,
        "clamp warning must live in the footer, after the resolved symbol header:\n{out}"
    );
}
