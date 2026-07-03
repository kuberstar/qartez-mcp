// Verification of three /boost features, exercised end-to-end over a REAL
// index (full_index over on-disk fixtures) and dispatched through the public
// `QartezServer::call_tool_by_name` entry point - not the tools' own unit
// tests.
//
//   1. qartez_path            - shortest call/reference path between symbols
//   2. qartez_unused reachable - whole-program dead-code reachability
//   3. qartez_security taint   - opt-in source->sink reachability annotation

use std::fs;

use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use qartez_mcp::index;
use qartez_mcp::server::QartezServer;
use qartez_mcp::storage::schema;

/// Build a QartezServer whose DB is a real index of `root`.
fn server_for(root: &std::path::Path) -> QartezServer {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    index::full_index(&conn, root, false).unwrap();
    QartezServer::new(conn, root.to_path_buf(), 0)
}

// ---------------------------------------------------------------------------
// Feature 1: qartez_path
// ---------------------------------------------------------------------------

/// Three files wired through `use` imports so the symbol-ref resolver links
/// them: func_a -> func_b -> func_c. `lonely` is an isolated exported fn.
fn path_project() -> TempDir {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub mod a;\npub mod b;\npub mod c;\n").unwrap();
    fs::write(
        src.join("a.rs"),
        "use crate::b::func_b;\npub fn func_a() { func_b(); }\npub fn lonely() {}\n",
    )
    .unwrap();
    fs::write(
        src.join("b.rs"),
        "use crate::c::func_c;\npub fn func_b() { func_c(); }\n",
    )
    .unwrap();
    fs::write(src.join("c.rs"), "pub fn func_c() {}\n").unwrap();
    dir
}

#[test]
fn path_orders_chain_a_to_c() {
    let dir = path_project();
    let server = server_for(dir.path());

    let out = server
        .call_tool_by_name("qartez_path", json!({"from": "func_a", "to": "func_c"}))
        .expect("qartez_path dispatch should succeed");

    assert!(out.contains("shortest path: 2 hop(s)"), "output:\n{out}");

    // The rendered rows must be ordered func_a (1) -> func_b (2) -> func_c (3).
    // Anchor on the numbered row markers, not bare names (the header line
    // `func_a -> func_c` also mentions the endpoints).
    assert!(out.contains("1. func_a"), "output:\n{out}");
    assert!(out.contains("2. func_b"), "output:\n{out}");
    assert!(out.contains("3. func_c"), "output:\n{out}");
    let ia = out.find("1. func_a").unwrap();
    let ib = out.find("2. func_b").unwrap();
    let ic = out.find("3. func_c").unwrap();
    assert!(ia < ib && ib < ic, "path rows not ordered a->b->c:\n{out}");
}

#[test]
fn path_reports_no_path_for_disconnected_and_reverse() {
    let dir = path_project();
    let server = server_for(dir.path());

    // Reverse direction: the graph is directed, func_c has no edge to func_a.
    let rev = server
        .call_tool_by_name("qartez_path", json!({"from": "func_c", "to": "func_a"}))
        .unwrap();
    assert!(rev.contains("No path found"), "reverse output:\n{rev}");

    // Fully disconnected target.
    let iso = server
        .call_tool_by_name("qartez_path", json!({"from": "func_a", "to": "lonely"}))
        .unwrap();
    assert!(iso.contains("No path found"), "isolated output:\n{iso}");
}

#[test]
fn path_same_symbol_is_zero_length() {
    let dir = path_project();
    let server = server_for(dir.path());

    let out = server
        .call_tool_by_name("qartez_path", json!({"from": "func_a", "to": "func_a"}))
        .unwrap();
    assert!(out.contains("path length is 0"), "output:\n{out}");
}

// ---------------------------------------------------------------------------
// Feature 2: qartez_unused reachable mode
// ---------------------------------------------------------------------------

/// A live exported chain (public_api -> helper) plus an isolated private dead
/// subgraph (dead_a -> dead_b). dead_b has an importer (dead_a) so the
/// one-hop scan cannot see it as dead, but reachability can.
fn unused_project() -> TempDir {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub fn public_api() { helper(); }\n\
         fn helper() {}\n\
         fn dead_a() { dead_b(); }\n\
         fn dead_b() {}\n",
    )
    .unwrap();
    dir
}

#[test]
fn unused_default_one_hop_does_not_report_dead_b() {
    let dir = unused_project();
    let server = server_for(dir.path());

    // Default (one-hop) mode reports only exported symbols with zero
    // importers; the private dead_a/dead_b never appear.
    let out = server
        .call_tool_by_name("qartez_unused", json!({"limit": 0}))
        .unwrap();
    assert!(
        !out.contains("dead_b"),
        "one-hop mode must not report dead_b (its importer dead_a hides it):\n{out}"
    );
    assert!(
        !out.contains("dead_a"),
        "one-hop mode reports exports only; private dead_a must not appear:\n{out}"
    );
}

#[test]
fn unused_reachable_reports_whole_dead_subgraph() {
    let dir = unused_project();
    let server = server_for(dir.path());

    let out = server
        .call_tool_by_name("qartez_unused", json!({"reachable": true, "limit": 0}))
        .unwrap();

    assert!(
        out.contains("dead_a"),
        "reachable must report dead_a:\n{out}"
    );
    assert!(
        out.contains("dead_b"),
        "reachable must report dead_b:\n{out}"
    );
    // The live exported chain must NOT be reported.
    assert!(
        !out.contains("public_api"),
        "exported root must be live:\n{out}"
    );
    assert!(
        !out.contains("helper"),
        "symbol reachable from an exported root must be live:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// Feature 3: qartez_security taint_reachability
// ---------------------------------------------------------------------------

/// A user-input SOURCE (`handle_request`) that calls a SQL-injection SINK
/// (`execute_statement`, SEC003 via `format!("SELECT ...")`).
fn security_project() -> TempDir {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("db.rs"),
        "pub fn handle_request() { execute_statement(1); }\n\
         pub fn execute_statement(id: i64) {\n\
        \x20   let q = format!(\"SELECT * FROM users WHERE id = {}\", id);\n\
        \x20   let _ = q;\n\
         }\n",
    )
    .unwrap();
    fs::write(src.join("lib.rs"), "pub mod db;\n").unwrap();
    dir
}

#[test]
fn security_default_omits_reachability_and_is_byte_identical() {
    let dir = security_project();
    let server = server_for(dir.path());

    let out_default = server
        .call_tool_by_name("qartez_security", json!({}))
        .unwrap();
    let out_false = server
        .call_tool_by_name("qartez_security", json!({"taint_reachability": false}))
        .unwrap();

    // Sanity: the SEC003 sink is actually found on the default path.
    assert!(
        out_default.contains("sql-injection") || out_default.contains("SEC003"),
        "expected the SQL-injection sink in default output:\n{out_default}"
    );
    // Default path carries no reachability section.
    assert!(
        !out_default.contains("Source -> Sink Reachability"),
        "default output must have no reachability section:\n{out_default}"
    );
    // Explicit false == default: byte-identical.
    assert_eq!(
        out_default, out_false,
        "taint_reachability=false must be byte-identical to the default"
    );
}

#[test]
fn security_taint_true_annotates_sink_reachable_from_source() {
    let dir = security_project();
    let server = server_for(dir.path());

    let out_false = server
        .call_tool_by_name("qartez_security", json!({"taint_reachability": false}))
        .unwrap();
    let out_true = server
        .call_tool_by_name("qartez_security", json!({"taint_reachability": true}))
        .unwrap();

    assert!(
        out_true.contains("Source -> Sink Reachability"),
        "taint_reachability=true must add the reachability section:\n{out_true}"
    );
    assert!(
        out_true.contains("reachable from source `handle_request`"),
        "sink must be annotated as reachable from handle_request:\n{out_true}"
    );
    assert!(
        out_true.contains("handle_request -> execute_statement"),
        "the source->sink path must be rendered:\n{out_true}"
    );
    // The regex finding table is unchanged; the section is purely additive,
    // so the reachability output is a strict superset prefixed by the
    // byte-identical default report.
    assert!(
        out_true.starts_with(&out_false),
        "reachability output must be additive over the default report.\n\
         --- false ---\n{out_false}\n--- true ---\n{out_true}"
    );
}
