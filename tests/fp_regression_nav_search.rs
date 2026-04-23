// Rust guideline compliant 2026-04-22
//
// End-to-end regressions for the navigation + search tool fixes.
//
// Covered bug classes:
//   - qartez_find kind aliases (`fn` maps to function+method, `class` maps to
//     class+struct, etc.)
//   - qartez_find / qartez_grep reject empty queries with a proper validation
//     error instead of silently falling through
//   - qartez_grep `search_bodies=true` emits line-level previews so callers
//     know where inside the body the match landed
//   - qartez_read returns a human-friendly "appears to be binary" error for
//     .png / NUL-byte files instead of the raw UTF-8 decoder error
//   - qartez_read emits an ambiguity warning when a symbol is defined in
//     two or more files and `file_path` is not set
//   - qartez_outline pagination: `offset=N` skips exactly N non-field
//     symbols in source order
//   - qartez_impact `include_tests=true` actually re-admits test files to
//     the reported blast radius

use std::fs;
use std::path::Path;

use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use qartez_mcp::index;
use qartez_mcp::server::QartezServer;
use qartez_mcp::storage::{schema, write};

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

fn write_cargo_manifest(dir: &Path, name: &str) {
    fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n"
        ),
    )
    .unwrap();
}

fn write_rust_fixture(dir: &Path) {
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub struct Foo;\n\
         impl Foo {\n    pub fn new() -> Self { Foo }\n    pub fn act(&self) -> u32 { 1 }\n}\n\
         pub fn greet() -> &'static str { \"hi\" }\n",
    )
    .unwrap();
}

#[test]
fn find_kind_alias_fn_matches_function_and_method() {
    let dir = TempDir::new().unwrap();
    write_cargo_manifest(dir.path(), "aliases");
    write_rust_fixture(dir.path());
    let server = build_and_index(dir.path());

    // `fn` must alias to the union of function + method so callers do not
    // have to know which bucket the indexer chose for free functions vs
    // methods.
    let out_fn = server
        .call_tool_by_name("qartez_find", json!({ "name": "greet", "kind": "fn" }))
        .expect("qartez_find kind=fn should succeed on a free function");
    assert!(
        out_fn.contains("greet"),
        "kind=fn must match free function `greet`: {out_fn}"
    );

    let out_method = server
        .call_tool_by_name("qartez_find", json!({ "name": "new", "kind": "fn" }))
        .expect("qartez_find kind=fn should succeed on a method");
    assert!(
        out_method.contains("new"),
        "kind=fn must match the method `new`: {out_method}"
    );

    // `function` alone should also find the method (indexed as `method`)
    // because the alias table maps `function` -> [function, method].
    let out_function = server
        .call_tool_by_name("qartez_find", json!({ "name": "new", "kind": "function" }))
        .expect("kind=function should alias to method too");
    assert!(
        out_function.contains("new"),
        "kind=function must match method `new`: {out_function}"
    );
}

#[test]
fn find_and_grep_reject_empty_query() {
    let dir = TempDir::new().unwrap();
    write_cargo_manifest(dir.path(), "empty_q");
    write_rust_fixture(dir.path());
    let server = build_and_index(dir.path());

    let err_find = server
        .call_tool_by_name("qartez_find", json!({ "name": "" }))
        .unwrap_err();
    assert!(
        err_find.contains("non-empty"),
        "empty qartez_find query must yield a validation error: {err_find}"
    );

    let err_grep = server
        .call_tool_by_name("qartez_grep", json!({ "query": "   " }))
        .unwrap_err();
    assert!(
        err_grep.contains("non-empty"),
        "whitespace-only qartez_grep query must yield a validation error: {err_grep}"
    );
}

#[test]
fn grep_search_bodies_emits_line_preview() {
    let dir = TempDir::new().unwrap();
    write_cargo_manifest(dir.path(), "preview");
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    // A function whose body embeds a distinctive sentinel token. The symbol
    // range is L1-L6; the match lives on L3. The preview must surface that
    // exact line so callers do not need a follow-up qartez_read.
    fs::write(
        src.join("lib.rs"),
        "pub fn find_me() -> u32 {\n    let a = 1;\n    let sentinel_token = 42;\n    let b = 2;\n    let _ = (a, b, sentinel_token);\n    sentinel_token\n}\n",
    )
    .unwrap();
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_grep",
            json!({ "query": "sentinel_token", "search_bodies": true }),
        )
        .expect("qartez_grep search_bodies should succeed");
    assert!(
        out.contains("find_me"),
        "symbol hit must still render: {out}"
    );
    assert!(
        out.contains("L3"),
        "line-level preview must cite L3 where the match landed: {out}"
    );
}

#[test]
fn read_binary_file_returns_human_friendly_error() {
    let dir = TempDir::new().unwrap();
    write_cargo_manifest(dir.path(), "binready");
    write_rust_fixture(dir.path());
    // Synthesize a tiny PNG-ish file: a few bytes with a NUL sentinel plus
    // the classic PNG magic header. Both the extension-based and the
    // content-based probe should flag it.
    let bin = dir.path().join("asset.png");
    fs::write(
        &bin,
        [
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0xFF, 0xD8,
        ],
    )
    .unwrap();
    let server = build_and_index(dir.path());

    let err = server
        .call_tool_by_name("qartez_read", json!({ "file_path": "asset.png" }))
        .unwrap_err();
    assert!(
        err.to_lowercase().contains("binary"),
        "binary file must produce a 'binary' error, not a UTF-8 decode error: {err}"
    );
    assert!(
        !err.to_lowercase()
            .contains("stream did not contain valid utf-8"),
        "raw UTF-8 decoder message must be hidden: {err}"
    );
}

#[test]
fn read_ambiguous_symbol_emits_warning() {
    let dir = TempDir::new().unwrap();
    write_cargo_manifest(dir.path(), "ambig");
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    // Two files define a symbol with the same name. Without file_path,
    // qartez_read should still return both bodies but prepend a warning
    // naming every defining file.
    fs::write(src.join("a.rs"), "pub fn shared() -> u32 { 1 }\n").unwrap();
    fs::write(src.join("b.rs"), "pub fn shared() -> u32 { 2 }\n").unwrap();
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name("qartez_read", json!({ "symbol_name": "shared" }))
        .expect("qartez_read should still return matches for an ambiguous symbol");
    assert!(
        out.contains("warning"),
        "ambiguous symbol must be preceded by a warning: {out}"
    );
    assert!(
        out.contains("a.rs") && out.contains("b.rs"),
        "warning must name every defining file: {out}"
    );
    assert!(
        out.contains("file_path"),
        "warning must prompt the caller to pin file_path: {out}"
    );
}

#[test]
fn outline_offset_skips_exactly_n_symbols() {
    let dir = TempDir::new().unwrap();
    write_cargo_manifest(dir.path(), "outline_offset");
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    // Eight top-level functions in source order. offset=3 must render
    // fn_3..fn_7 (five entries), and the first rendered row must be fn_3.
    let mut body = String::new();
    for i in 0..8 {
        body.push_str(&format!("pub fn fn_{i}() -> u32 {{ {i} }}\n"));
    }
    fs::write(src.join("lib.rs"), body).unwrap();
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_outline",
            json!({ "file_path": "src/lib.rs", "offset": 3, "format": "concise" }),
        )
        .expect("qartez_outline with offset should succeed");
    // The first three symbols must be skipped.
    assert!(
        !out.contains("fn_0") && !out.contains("fn_1") && !out.contains("fn_2"),
        "offset=3 must skip exactly 3 leading symbols: {out}"
    );
    // The remaining five must be present.
    for i in 3..8 {
        assert!(
            out.contains(&format!("fn_{i}")),
            "offset=3 must render fn_{i}: {out}"
        );
    }
}

#[test]
fn impact_include_tests_toggles_test_files_in_blast_radius() {
    // We drive the storage layer directly so the graph is deterministic
    // regardless of how the Rust indexer resolves crate-rooted imports in
    // temp fixtures. The goal is to prove the impact tool filters on
    // `is_test_path`, not to re-test import resolution.
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    let conn = setup_db();

    let target = write::upsert_file(&conn, "src/target.rs", 1000, 100, "rust", 10).unwrap();
    let consumer = write::upsert_file(&conn, "src/consumer.rs", 1000, 80, "rust", 8).unwrap();
    let test_file = write::upsert_file(&conn, "tests/target_test.rs", 1000, 40, "rust", 4).unwrap();

    // Both consumer.rs (production) and tests/target_test.rs (test)
    // import target.rs. Only the test-classified path should be filtered
    // when include_tests=false.
    write::insert_edge(&conn, consumer, target, "import", None).unwrap();
    write::insert_edge(&conn, test_file, target, "import", None).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    let without = server
        .call_tool_by_name(
            "qartez_impact",
            json!({ "file_path": "src/target.rs", "include_tests": false }),
        )
        .expect("qartez_impact without tests");
    let with = server
        .call_tool_by_name(
            "qartez_impact",
            json!({ "file_path": "src/target.rs", "include_tests": true }),
        )
        .expect("qartez_impact with tests");

    assert!(
        !without.contains("tests/target_test.rs"),
        "include_tests=false must suppress tests/target_test.rs: {without}"
    );
    assert!(
        with.contains("tests/target_test.rs"),
        "include_tests=true must re-admit tests/target_test.rs: {with}"
    );
    assert!(
        without.contains("src/consumer.rs"),
        "production importers must appear regardless of include_tests: {without}"
    );
}
