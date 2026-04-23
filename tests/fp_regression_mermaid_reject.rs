// Rust guideline compliant 2026-04-22
//
// Regression coverage for format=mermaid rejection on tools that do not
// implement a mermaid rendering path.
//
//   R1  qartez_find  - returns a validation error when format=mermaid
//   R2  qartez_smells - returns a validation error when format=mermaid
//
// Both tools are representative samples of the 16 tools that received the
// reject_mermaid guard in the same patch. The error message must name the
// tool and point callers at the graph-capable alternatives.

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

// ---------------------------------------------------------------------------
// R1 qartez_find: format=mermaid must return a validation error
// ---------------------------------------------------------------------------

#[test]
fn find_rejects_mermaid_format() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/lib.rs", "pub fn hello() {}\n")]);

    let err = server
        .call_tool_by_name("qartez_find", json!({"name": "hello", "format": "mermaid"}))
        .expect_err("qartez_find with format=mermaid must return Err");

    assert!(
        err.contains("format=mermaid is not supported for qartez_find"),
        "error must name the tool, got: {err}"
    );
    assert!(
        err.contains("qartez_deps")
            || err.contains("qartez_calls")
            || err.contains("qartez_hierarchy"),
        "error must point to graph-capable alternatives, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// R2 qartez_smells: format=mermaid must return a validation error
// ---------------------------------------------------------------------------

#[test]
fn smells_rejects_mermaid_format() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/lib.rs", "pub fn hello() {}\n")]);

    let err = server
        .call_tool_by_name("qartez_smells", json!({"format": "mermaid"}))
        .expect_err("qartez_smells with format=mermaid must return Err");

    assert!(
        err.contains("format=mermaid is not supported for qartez_smells"),
        "error must name the tool, got: {err}"
    );
    assert!(
        err.contains("qartez_deps")
            || err.contains("qartez_calls")
            || err.contains("qartez_hierarchy"),
        "error must point to graph-capable alternatives, got: {err}"
    );
}
