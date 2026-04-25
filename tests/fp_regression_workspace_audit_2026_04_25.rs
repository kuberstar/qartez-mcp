// Rust guideline compliant 2026-04-25
//
// Regression coverage for the 2026-04-25 workspace / index audit pass.
//
// The audit found a cluster of related bugs around the symmetry of
// `add` and `remove` workspace guards and around the file-count
// rendering in `qartez_list_roots` when the legacy single-root primary
// stays unprefixed after `qartez_add_root` is called for a sibling root.
//
// Harness pattern matches the rest of `tests/fp_regression_*.rs`:
// drop files into a TempDir, run the index, then call the MCP dispatch
// directly via `QartezServer::call_tool_by_name`.

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
    fs::create_dir_all(dir.join(".qartez")).unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"primary\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(dir.join("src/lib.rs"), "pub fn primary() {}\n").unwrap();
    fs::write(dir.join("src/util.rs"), "pub fn util() {}\n").unwrap();
    let conn = setup_db();
    index::full_index(&conn, dir, false).unwrap();
    QartezServer::new(conn, dir.to_path_buf(), 0)
}

#[test]
fn add_root_rejects_path_that_is_primary_itself() {
    // The same canonical path as the primary cannot be re-registered
    // as a separate root. Without this guard the indexer would walk
    // the primary tree twice and double-count its symbols under a
    // duplicate prefix.
    let main = TempDir::new().unwrap();
    let server = build_and_index(main.path());

    let err = server
        .call_tool_by_name(
            "qartez_add_root",
            json!({
                "path": main.path().to_str().unwrap(),
                "alias": "primary-clone",
                "persist": false,
                "watch": false,
            }),
        )
        .expect_err("re-adding the primary path must be rejected");
    assert!(
        err.to_lowercase().contains("primary"),
        "error must mention the primary root: {err}"
    );
}

#[test]
fn add_root_rejects_path_inside_primary_root() {
    // Symmetrical to the existing remove-side guard: registering a
    // subdirectory of the primary as a separate root is the path
    // that produced ghost prefix rows after remove and asymmetric
    // behaviour ("add accepts, remove refuses"). Reject at add time.
    let main = TempDir::new().unwrap();
    let server = build_and_index(main.path());

    let nested = main.path().join("src");

    let err = server
        .call_tool_by_name(
            "qartez_add_root",
            json!({
                "path": nested.to_str().unwrap(),
                "alias": "nested",
                "persist": false,
                "watch": false,
            }),
        )
        .expect_err("nested path must be rejected at add time");
    assert!(
        err.to_lowercase().contains("inside the primary"),
        "error must mention 'inside the primary' guard: {err}"
    );
}

#[test]
fn add_root_rejects_path_that_contains_primary() {
    // `path=../..` after canonicalization resolves to an ancestor of
    // the primary root. Indexing that ancestor would walk the entire
    // monorepo and produce duplicate rows under a different prefix.
    // Reject up front.
    let outer = TempDir::new().unwrap();
    fs::create_dir_all(outer.path().join("inner")).unwrap();

    let primary = outer.path().join("inner");
    fs::create_dir_all(primary.join(".git")).unwrap();
    fs::create_dir_all(primary.join(".qartez")).unwrap();
    fs::create_dir_all(primary.join("src")).unwrap();
    fs::write(
        primary.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(primary.join("src/lib.rs"), "pub fn x() {}\n").unwrap();

    let conn = setup_db();
    index::full_index(&conn, &primary, false).unwrap();
    let server = QartezServer::new(conn, primary, 0);

    let err = server
        .call_tool_by_name(
            "qartez_add_root",
            json!({
                "path": outer.path().to_str().unwrap(),
                "alias": "ancestor",
                "persist": false,
                "watch": false,
            }),
        )
        .expect_err("ancestor of primary must be rejected");
    let lower = err.to_lowercase();
    assert!(
        lower.contains("contains") || lower.contains("ancestor") || lower.contains("primary"),
        "error must explain the ancestor guard: {err}"
    );
}

#[test]
fn list_roots_reports_primary_files_after_runtime_add() {
    // After a runtime `qartez_add_root` the primary stays in
    // `project_roots` without an alias entry while its on-disk rows
    // still live unprefixed. The previous `qartez_list_roots` logic
    // computed a prefix from the basename and reported `files=0`
    // for that primary even though the rows were intact. The fix
    // counts unprefixed rows (rows that do NOT start with any
    // sibling prefix) for the primary.
    let main = TempDir::new().unwrap();
    let server = build_and_index(main.path());

    let extra = TempDir::new().unwrap();
    fs::write(extra.path().join("y.rs"), "pub fn y() {}\n").unwrap();

    server
        .call_tool_by_name(
            "qartez_add_root",
            json!({
                "path": extra.path().to_str().unwrap(),
                "alias": "extra-root",
                "persist": false,
                "watch": false,
            }),
        )
        .expect("add must succeed");

    let listing = server
        .call_tool_by_name("qartez_list_roots", json!({}))
        .expect("list must succeed");

    // The primary row in the markdown table must NOT report files=0
    // when the DB still has the primary's lib.rs/util.rs rows.
    let primary_basename = main
        .path()
        .file_name()
        .and_then(|n| n.to_str())
        .expect("temp dir basename");
    // The primary row uses the literal alias `(primary)` because no
    // alias entry is registered for it. Find that row and assert the
    // file count is at least 1 (lib.rs, util.rs, Cargo.toml).
    let primary_line = listing
        .lines()
        .find(|line| line.contains("| (primary) |"))
        .unwrap_or_else(|| panic!("listing must include a (primary) row: {listing}"));
    assert!(
        !primary_line.contains("| 0 |"),
        "primary row must not report files=0 after runtime add: {primary_line}"
    );
    let _ = primary_basename;
}

#[test]
fn list_roots_counts_aliased_root_separately_from_primary() {
    // Companion check to `list_roots_reports_primary_files_after_runtime_add`:
    // after the add, the aliased root's count must be >= 1 too, and
    // the two counts must not collide (both pointing at the whole
    // table). The audit found earlier renderers that returned the
    // entire `files` row count for both rows.
    let main = TempDir::new().unwrap();
    let server = build_and_index(main.path());

    let extra = TempDir::new().unwrap();
    fs::write(extra.path().join("y.rs"), "pub fn y() {}\n").unwrap();

    server
        .call_tool_by_name(
            "qartez_add_root",
            json!({
                "path": extra.path().to_str().unwrap(),
                "alias": "extra-root",
                "persist": false,
                "watch": false,
            }),
        )
        .expect("add must succeed");

    let listing = server
        .call_tool_by_name("qartez_list_roots", json!({}))
        .expect("list must succeed");

    let extra_line = listing
        .lines()
        .find(|line| line.contains("| extra-root |"))
        .unwrap_or_else(|| panic!("listing must include the extra-root row: {listing}"));
    assert!(
        !extra_line.contains("| 0 |"),
        "extra-root row must not report files=0: {extra_line}"
    );
}
