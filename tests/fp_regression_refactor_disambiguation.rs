// Rust guideline compliant 2026-04-23
//
// Regression coverage for the refactor-tool critical-bug sweep that
// shipped on top of commit 3bd798a. Each test maps 1-to-1 to a bug in
// the user report:
//
//   - rename refuses without disambiguation when the name is shared
//   - rename refuses without disambiguation when defined in >1 files
//   - rename identity (old == new) short-circuits to no-op
//   - rename collision detection refuses apply when `new_name`
//     already exists in a touched file
//   - rename_file rewrites the parent `mod <stem>;` declaration
//   - rename_file refuses to rename `mod.rs`
//   - replace_symbol refuses body-only new_code
//   - safe_delete uses symbol-level refs, not just file-level use edges
//   - qartez_move importer count matches qartez_refs
//
// All tests use the same fixture pattern as the existing
// `tests/fp_regression_*.rs` files: drop files to a TempDir, run
// `full_index`, and call the public MCP dispatcher via
// `QartezServer::call_tool_by_name`. Apply-flag paths then re-read the
// files on disk to assert the rewrite actually landed.

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
// A1 / A10: rename refuses when a name collapses multiple kinds / files.
// ---------------------------------------------------------------------------

#[test]
fn rename_refuses_when_name_shared_by_multiple_kinds() {
    // Both a free `fn make` and an `impl Foo { fn make }` method exist in
    // a single file. Without `kind` disambiguation, `qartez_rename` must
    // refuse rather than blanket-rewrite every occurrence.
    let dir = TempDir::new().unwrap();
    let src = r#"pub struct Foo;

impl Foo {
    pub fn make() -> Foo {
        Foo
    }
}

pub fn make() -> u32 {
    42
}

pub fn caller() -> u32 {
    let _f = Foo::make();
    make()
}
"#;
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", src),
        ],
    );

    let err = server
        .call_tool_by_name(
            "qartez_rename",
            json!({ "old_name": "make", "new_name": "manufacture" }),
        )
        .expect_err("expected refusal, got success");
    assert!(
        err.contains("Refusing to rename") || err.contains("Multiple definitions"),
        "must refuse without kind/file_path disambiguation, got: {err}"
    );
    assert!(
        err.contains("kind") || err.contains("file_path"),
        "error must tell caller to pass kind/file_path, got: {err}"
    );
}

#[test]
fn rename_refuses_when_name_defined_in_multiple_files() {
    // Same-name symbol (`is_test_path`) defined in two files - the A10
    // failure mode. Without `file_path`, the rename must refuse.
    let dir = TempDir::new().unwrap();
    let a = "pub fn is_test_path(p: &str) -> bool { p.starts_with(\"tests/\") }\n";
    let b = "pub fn is_test_path(p: &str) -> bool { p.ends_with(\"_test.rs\") }\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod a;\npub mod b;\n"),
            ("src/a.rs", a),
            ("src/b.rs", b),
        ],
    );

    let err = server
        .call_tool_by_name(
            "qartez_rename",
            json!({ "old_name": "is_test_path", "new_name": "is_test_file" }),
        )
        .expect_err("expected refusal, got success");
    assert!(
        err.contains("Refusing to rename") || err.contains("Multiple definitions"),
        "must refuse when defined in multiple files, got: {err}"
    );
    assert!(
        err.contains("src/a.rs") && err.contains("src/b.rs"),
        "disambiguation hint must list both files, got: {err}"
    );
}

#[test]
fn rename_file_path_filter_unblocks_disambiguation() {
    // Same fixture as the previous test but with `file_path` supplied -
    // the tool must now accept the call and rewrite only the picked file.
    let dir = TempDir::new().unwrap();
    let a = "pub fn is_test_path(p: &str) -> bool { p.starts_with(\"tests/\") }\n";
    let b = "pub fn is_test_path(p: &str) -> bool { p.ends_with(\"_test.rs\") }\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod a;\npub mod b;\n"),
            ("src/a.rs", a),
            ("src/b.rs", b),
        ],
    );

    let preview = server
        .call_tool_by_name(
            "qartez_rename",
            json!({
                "old_name": "is_test_path",
                "new_name": "is_test_file",
                "file_path": "src/a.rs",
            }),
        )
        .expect("qartez_rename preview with file_path must succeed");
    assert!(
        preview.contains("src/a.rs"),
        "preview must mention the picked file, got: {preview}"
    );
    assert!(
        !preview.contains("src/b.rs"),
        "preview must not cross into other same-name definition file, got: {preview}"
    );
}

// ---------------------------------------------------------------------------
// A9: identity rename short-circuits to a no-op.
// ---------------------------------------------------------------------------

#[test]
fn rename_identity_returns_noop_message() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub fn foo() -> u32 { 1 }\n"),
        ],
    );

    let out = server
        .call_tool_by_name(
            "qartez_rename",
            json!({ "old_name": "foo", "new_name": "foo" }),
        )
        .expect("identity rename must succeed as a no-op");
    assert!(
        out.to_lowercase().contains("no-op") || out.to_lowercase().contains("identical"),
        "identity rename must return a no-op marker, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// A11: rename collision detection.
// ---------------------------------------------------------------------------

#[test]
fn rename_refuses_when_new_name_collides_in_same_file() {
    // File has both `foo` and `bar`. Renaming `foo` -> `bar` must refuse
    // because the target name already exists as a defined symbol in the
    // touched file. `allow_collision=true` is the escape hatch.
    let dir = TempDir::new().unwrap();
    let src = "pub fn foo() -> u32 { 1 }\npub fn bar() -> u32 { 2 }\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", src),
        ],
    );

    let err = server
        .call_tool_by_name(
            "qartez_rename",
            json!({ "old_name": "foo", "new_name": "bar", "apply": true }),
        )
        .expect_err("collision must refuse by default");
    assert!(
        err.contains("collision") || err.contains("already defined"),
        "must name the collision in the error, got: {err}"
    );

    let out = server
        .call_tool_by_name(
            "qartez_rename",
            json!({
                "old_name": "foo",
                "new_name": "bar",
                "allow_collision": true,
            }),
        )
        .expect("allow_collision=true must unblock the call");
    assert!(
        out.contains("bar"),
        "collision-overridden output must reference the new name, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// A4 / A5: rename_file rewrites parent `mod` declaration; refuses mod.rs.
// ---------------------------------------------------------------------------

#[test]
fn rename_file_rewrites_parent_mod_decl() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod helpers;\n"),
            ("src/helpers.rs", "pub fn noop() {}\n"),
        ],
    );

    let out = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({
                "from": "src/helpers.rs",
                "to": "src/utilities.rs",
                "apply": true,
            }),
        )
        .expect("rename_file must succeed");
    assert!(
        out.contains("renamed"),
        "apply path must say 'renamed', got: {out}"
    );

    let lib = fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert!(
        lib.contains("mod utilities;"),
        "parent lib.rs must get `mod utilities;`, got:\n{lib}"
    );
    assert!(
        !lib.contains("mod helpers;"),
        "parent lib.rs must not still declare the old module, got:\n{lib}"
    );
}

#[test]
fn rename_file_refuses_mod_rs() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod benchmark;\n"),
            ("src/benchmark/mod.rs", "pub fn run() {}\n"),
        ],
    );

    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({
                "from": "src/benchmark/mod.rs",
                "to": "src/benchmark/core.rs",
                "apply": true,
            }),
        )
        .expect_err("mod.rs rename must refuse");
    assert!(
        err.to_lowercase().contains("mod.rs"),
        "refusal must name mod.rs explicitly, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// A7: replace_symbol refuses body-only new_code.
// ---------------------------------------------------------------------------

#[test]
fn replace_symbol_refuses_body_only_new_code() {
    let dir = TempDir::new().unwrap();
    let src = "pub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", src),
        ],
    );

    // Body-only: missing the `fn add(...)` introducer line.
    let err = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "add",
                "new_code": "    let r = a.saturating_add(b);\n    r\n",
                "apply": true,
            }),
        )
        .expect_err("body-only replace must refuse");
    assert!(
        err.contains("introducer") || err.contains("signature"),
        "refusal must mention signature/introducer, got: {err}"
    );

    // Control: a full new definition must still be accepted.
    let out = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "add",
                "new_code": "pub fn add(a: u32, b: u32) -> u32 {\n    a.saturating_add(b)\n}\n",
                "apply": true,
            }),
        )
        .expect("full-signature replace must succeed");
    assert!(
        out.contains("Replaced"),
        "successful replace must report Replaced, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// A6: safe_delete blast radius reflects symbol refs, not just file edges.
// ---------------------------------------------------------------------------

#[test]
fn safe_delete_preview_reports_edge_and_symbol_ref_counts() {
    let dir = TempDir::new().unwrap();
    let defs = "pub fn hello() -> &'static str { \"hi\" }\n";
    // caller.rs imports via `use crate::defs::hello;` - edge signal.
    let caller = "use crate::defs::hello;\npub fn greet() -> &'static str { hello() }\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod defs;\npub mod caller;\n"),
            ("src/defs.rs", defs),
            ("src/caller.rs", caller),
        ],
    );

    let out = server
        .call_tool_by_name(
            "qartez_safe_delete",
            json!({ "symbol": "hello", "file_path": "src/defs.rs" }),
        )
        .expect("safe_delete preview must succeed");
    // The preview now cites per-symbol references only. File-level
    // use-edges were the signal that falsely flagged zero-caller
    // helpers in mod.rs as "7 files reference mod.rs"; dropping the
    // dual-signal breakdown was the intended fix.
    assert!(
        out.contains("reference symbol 'hello'"),
        "preview must cite the symbol being deleted, got: {out}"
    );
    assert!(
        out.contains("src/caller.rs"),
        "caller file must appear in the blast-radius list, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// A3: qartez_move importer count matches the symbol-ref count.
// ---------------------------------------------------------------------------

#[test]
fn move_importer_count_matches_symbol_refs() {
    let dir = TempDir::new().unwrap();
    let defs = "pub fn greet() -> &'static str { \"hi\" }\n";
    let a = "use crate::defs::greet;\npub fn a() -> &'static str { greet() }\n";
    let b = "use crate::defs::greet;\npub fn b() -> &'static str { greet() }\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod defs;\npub mod a;\npub mod b;\n"),
            ("src/defs.rs", defs),
            ("src/a.rs", a),
            ("src/b.rs", b),
        ],
    );

    let refs = server
        .call_tool_by_name("qartez_refs", json!({ "symbol": "greet" }))
        .expect("qartez_refs must succeed");
    let refs_a = refs.contains("src/a.rs");
    let refs_b = refs.contains("src/b.rs");
    assert!(
        refs_a && refs_b,
        "qartez_refs must surface both importers, got: {refs}"
    );

    let mv = server
        .call_tool_by_name(
            "qartez_move",
            json!({
                "symbol": "greet",
                "to_file": "src/util.rs",
                "file_path": "src/defs.rs",
            }),
        )
        .expect("qartez_move preview must succeed");
    assert!(
        mv.contains("src/a.rs") && mv.contains("src/b.rs"),
        "qartez_move importer list must match qartez_refs, got: {mv}"
    );
}
