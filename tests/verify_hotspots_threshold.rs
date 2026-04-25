// Throwaway verification test for the hotspots threshold fix:
//   - threshold=0 → Err
//   - threshold > 10 → Err with the value echoed back
//   - threshold=10 boundary still allowed
//   - mid-range threshold values still allowed
//   - both file-level and symbol-level honour the new path

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

fn rust_fixture() -> [(&'static str, &'static str); 3] {
    [
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        ),
        (
            "src/lib.rs",
            "pub fn alpha(x: i32) -> i32 { if x > 0 { x } else { -x } }\n\
             pub fn beta(y: i32) -> i32 { y * 2 }\n",
        ),
        (
            "src/a.rs",
            "pub fn complex(x: i32) -> i32 {\n    \
                 if x > 10 { 1 } else if x > 5 { 2 } else if x > 0 { 3 } else { 0 }\n\
             }\n",
        ),
    ]
}

#[test]
fn hotspots_threshold_zero_is_rejected() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_hotspots", json!({"threshold": 0}))
        .expect_err("threshold=0 must error");
    assert!(
        err.contains("excludes every file"),
        "expected 'excludes every file' message, got: {err}"
    );
}

#[test]
fn hotspots_threshold_eleven_is_rejected() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_hotspots", json!({"threshold": 11}))
        .expect_err("threshold=11 must error");
    assert!(
        err.contains("outside the documented 0-10 range"),
        "expected 0-10 range message, got: {err}"
    );
    assert!(
        err.contains("11"),
        "error must echo the user's value 11, got: {err}"
    );
}

#[test]
fn hotspots_threshold_one_hundred_echoes_value() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_hotspots", json!({"threshold": 100}))
        .expect_err("threshold=100 must error");
    assert!(
        err.contains("outside the documented 0-10 range"),
        "expected 0-10 range message, got: {err}"
    );
    assert!(
        err.contains("100"),
        "error must echo the user's value 100, got: {err}"
    );
}

#[test]
fn hotspots_threshold_ten_is_accepted() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name("qartez_hotspots", json!({"threshold": 10}))
        .expect("threshold=10 must succeed");
    // Output is non-empty — either the table header or the
    // "no hotspots" stub. Both are valid (the fixture may or may
    // not have any hotspot data depending on git/PageRank state).
    assert!(!out.is_empty(), "threshold=10 must produce output");
}

#[test]
fn hotspots_threshold_one_five_nine_accepted() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    for t in [1u32, 5, 9] {
        server
            .call_tool_by_name("qartez_hotspots", json!({"threshold": t}))
            .unwrap_or_else(|e| panic!("threshold={t} must succeed, got Err: {e}"));
    }
}

#[test]
fn hotspots_threshold_none_default_path() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name("qartez_hotspots", json!({}))
        .expect("default call must succeed");
    assert!(!out.is_empty());
}

#[test]
fn hotspots_threshold_eleven_rejected_at_symbol_level() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_hotspots",
            json!({"threshold": 11, "level": "symbol"}),
        )
        .expect_err("symbol-level threshold=11 must error");
    assert!(
        err.contains("outside the documented 0-10 range"),
        "expected range error in symbol mode, got: {err}"
    );
}

#[test]
fn hotspots_threshold_ten_accepted_at_symbol_level() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    server
        .call_tool_by_name(
            "qartez_hotspots",
            json!({"threshold": 10, "level": "symbol"}),
        )
        .expect("symbol-level threshold=10 must succeed");
}

#[test]
fn hotspots_threshold_zero_rejected_at_symbol_level() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_hotspots",
            json!({"threshold": 0, "level": "symbol"}),
        )
        .expect_err("symbol-level threshold=0 must error");
    assert!(
        err.contains("excludes every file"),
        "expected 'excludes every file' in symbol mode, got: {err}"
    );
}
