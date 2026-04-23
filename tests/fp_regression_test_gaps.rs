// Rust guideline compliant 2026-04-22
//
// End-to-end regression for the test-gaps FP fix. FP1: non-testable file
// types (shell scripts, Cargo.toml, YAML, Dockerfile) were listed under
// "untested source files". The fix restricts the gap report to languages
// with first-class unit-test conventions.
//
// FP2: Rust crate-rooted imports (`use <crate>::<mod>`) and subprocess
// binary tests emit no import edge, so source modules covered through
// such tests were flagged as untested. The fix back-stops the edge-graph
// check with an FTS-body lookup keyed by the source-file's module stem.

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

fn write_test_gaps_fixture(dir: &Path) {
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    let tests = dir.join("tests");
    fs::create_dir_all(&tests).unwrap();

    // FP1: shell script. install.sh is indexed (bash language) but
    // cannot grow a unit test, so the gap report must filter it out.
    fs::write(dir.join("install.sh"), "#!/bin/bash\necho install\n").unwrap();

    // FP2: source module imported under the crate-rooted form.
    fs::write(
        src.join("foo.rs"),
        "pub fn touch() -> u32 { 42 }\npub fn add_two(x: u32) -> u32 { x + 2 }\n",
    )
    .unwrap();

    // Genuinely untested source.
    fs::write(
        src.join("bar.rs"),
        "pub fn neglected_helper(x: i64) -> i64 { x.saturating_mul(7) }\n",
    )
    .unwrap();

    fs::write(src.join("lib.rs"), "pub mod foo;\npub mod bar;\n").unwrap();

    // Crate-rooted import from a tests/*.rs file. The fixture crate is
    // named "fixture" via the manifest below.
    fs::write(
        tests.join("foo_test.rs"),
        "use fixture::foo;\n#[test]\nfn touch_returns_forty_two() {\n    assert_eq!(foo::touch(), 42);\n    assert_eq!(foo::add_two(3), 5);\n}\n",
    )
    .unwrap();
}

fn write_cargo_manifest(dir: &Path) {
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .unwrap();
}

#[test]
fn gaps_excludes_shell_scripts_and_manifests() {
    let dir = TempDir::new().unwrap();
    write_cargo_manifest(dir.path());
    write_test_gaps_fixture(dir.path());
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({ "mode": "gaps", "limit": 200, "format": "concise" }),
        )
        .expect("qartez_test_gaps gaps mode should succeed");

    assert!(
        !out.contains("install.sh"),
        "install.sh is a shell script and must not appear in test gaps: {out}"
    );
    assert!(
        !out.contains("Cargo.toml"),
        "Cargo.toml is not testable and must not appear in test gaps: {out}"
    );
}

#[test]
fn gaps_recognises_crate_rooted_imports_via_fts() {
    let dir = TempDir::new().unwrap();
    write_cargo_manifest(dir.path());
    write_test_gaps_fixture(dir.path());
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({ "mode": "gaps", "limit": 200, "format": "concise" }),
        )
        .expect("qartez_test_gaps gaps mode should succeed");

    assert!(
        !out.contains("src/foo.rs"),
        "src/foo.rs is exercised by tests/foo_test.rs via `use fixture::foo;` and must NOT appear in gaps: {out}"
    );
}

#[test]
fn gaps_still_flags_truly_untested_source() {
    let dir = TempDir::new().unwrap();
    write_cargo_manifest(dir.path());
    write_test_gaps_fixture(dir.path());
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({ "mode": "gaps", "limit": 200, "format": "concise" }),
        )
        .expect("qartez_test_gaps gaps mode should succeed");

    assert!(
        out.contains("src/bar.rs"),
        "src/bar.rs has no test coverage and must appear in gaps: {out}"
    );
}
