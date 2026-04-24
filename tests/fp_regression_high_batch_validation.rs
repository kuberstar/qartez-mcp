// Regression coverage for the HIGH-priority validation batch:
//
//   H1 replace_symbol rejects trailing content past the definition.
//   H2 replace_symbol rejects new_code whose identifier disagrees.
//   H3 replace_symbol strips a leading UTF-8 BOM before introducer check.
//   H4 replace_symbol accepts a leading `//` comment before the introducer.
//   H5 rename_file canonicalises the `to` arg (absolute / `..` / trailing
//      slash / nonexistent parent).
//   H6 qartez_move refuses `mod.rs` / `lib.rs` / `main.rs` destinations.
//   H6b qartez_move refuses cross-language `.ts` / unknown-extension targets.
//   H7 qartez_refs detailed-mode collapses duplicate importer paths.
//   H8 qartez_hotspots rejects format=mermaid with a precise error.
//   H9 qartez_project filter rejects shell metacharacters.
//   H10 qartez_find empty-name error names the `name` parameter.
//
// The harness mirrors the existing `tests/fp_regression_*.rs` files.

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
        ("src/lib.rs", "pub mod a;\n"),
        (
            "src/a.rs",
            "pub fn foo() -> u32 { 0 }\npub fn helper() {}\n",
        ),
    ]
}

// ---------------------------------------------------------------------------
// H1 replace_symbol refuses trailing content after the closing brace.
// ---------------------------------------------------------------------------

#[test]
fn replace_symbol_rejects_trailing_content() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    let err = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "foo",
                "new_code": "pub fn foo() -> u32 { 0 }\nstuff();\ngarbage;",
            }),
        )
        .expect_err("trailing junk after the definition must be rejected");
    assert!(
        err.contains("trailing content") || err.contains("one top-level item"),
        "rejection must mention trailing content, got: {err}",
    );
}

// ---------------------------------------------------------------------------
// H2 replace_symbol refuses when the defined identifier does not match.
// ---------------------------------------------------------------------------

#[test]
fn replace_symbol_rejects_identifier_mismatch() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    let err = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "foo",
                "new_code": "pub fn DIFFERENT_NAME() -> u32 { 0 }",
            }),
        )
        .expect_err("hidden rename must be rejected");
    assert!(
        err.contains("DIFFERENT_NAME") && err.contains("foo"),
        "error must name both the wrong and right identifier, got: {err}",
    );
    assert!(
        err.contains("qartez_rename"),
        "error must point to qartez_rename, got: {err}",
    );
}

// ---------------------------------------------------------------------------
// H3 replace_symbol strips a leading BOM before the introducer check.
// ---------------------------------------------------------------------------

#[test]
fn replace_symbol_accepts_leading_bom() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    // Preview-only (apply not set) so we just need the introducer check
    // to pass. A BOM-prefixed definition previously tripped the
    // introducer guard.
    let bom = '\u{FEFF}';
    let new_code = format!("{bom}pub fn foo() -> u32 {{ 1 }}");
    let out = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "foo",
                "new_code": new_code,
            }),
        )
        .expect("BOM-prefixed new_code must be accepted");
    assert!(out.contains("Preview"), "preview must render, got: {out}",);
}

// ---------------------------------------------------------------------------
// H4 replace_symbol accepts a leading `//` line comment before the
// definition introducer.
// ---------------------------------------------------------------------------

#[test]
fn replace_symbol_accepts_leading_line_comment() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    let out = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "foo",
                "new_code": "// note for reviewers\npub fn foo() -> u32 { 2 }",
            }),
        )
        .expect("leading line comment must be treated as prelude");
    assert!(out.contains("Preview"), "preview must render, got: {out}",);
}

// ---------------------------------------------------------------------------
// H5 rename_file canonicalisation: absolute, `..`, trailing slash,
// nonexistent parent are all rejected up front.
// ---------------------------------------------------------------------------

#[test]
fn rename_file_rejects_absolute_to() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({ "from": "src/a.rs", "to": "/tmp/other.rs" }),
        )
        .expect_err("absolute `to` must be rejected");
    assert!(
        err.contains("absolute path"),
        "rejection must mention absolute path, got: {err}",
    );
}

#[test]
fn rename_file_rejects_parent_traversal() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({ "from": "src/a.rs", "to": "src/../escape.rs" }),
        )
        .expect_err("parent traversal must be rejected");
    assert!(
        err.contains("`..`") || err.contains("parent-directory"),
        "rejection must mention `..`, got: {err}",
    );
}

#[test]
fn rename_file_rejects_trailing_slash() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({ "from": "src/a.rs", "to": "src/b/" }),
        )
        .expect_err("trailing slash must be rejected");
    assert!(
        err.contains("trailing slash"),
        "rejection must mention trailing slash, got: {err}",
    );
}

#[test]
fn rename_file_auto_creates_missing_parent_directory_on_apply() {
    // rename_file MAKES the destination subdirectory as part of apply
    // so refactor flows that move a file into a brand-new module
    // directory work in one step. `validate_rename_path_arg` already
    // rejects `..` traversal and malformed paths before this point,
    // so a "missing" parent just means the caller is creating it as
    // part of the rename.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    let ok = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({ "from": "src/a.rs", "to": "src/helpers/a.rs", "apply": true }),
        )
        .expect("missing parent must be auto-created on apply");
    assert!(
        ok.contains("renamed"),
        "apply path must confirm rename, got: {ok}",
    );
    assert!(
        dir.path().join("src/helpers/a.rs").exists(),
        "renamed file must land at the new path",
    );
}

// ---------------------------------------------------------------------------
// H6 qartez_move refuses to write into module/crate roots.
// ---------------------------------------------------------------------------

#[test]
fn move_refuses_mod_rs_destination() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    let err = server
        .call_tool_by_name(
            "qartez_move",
            json!({ "symbol": "foo", "to_file": "src/mod.rs" }),
        )
        .expect_err("mod.rs destination must be rejected");
    assert!(
        err.contains("mod.rs"),
        "rejection must mention mod.rs, got: {err}",
    );
}

#[test]
fn move_refuses_lib_rs_destination() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    let err = server
        .call_tool_by_name(
            "qartez_move",
            json!({ "symbol": "foo", "to_file": "src/lib.rs" }),
        )
        .expect_err("lib.rs destination must be rejected");
    assert!(
        err.contains("lib.rs") || err.contains("crate entry"),
        "rejection must mention lib.rs, got: {err}",
    );
}

// ---------------------------------------------------------------------------
// H6b qartez_move refuses cross-language and unknown extensions.
// ---------------------------------------------------------------------------

#[test]
fn move_refuses_cross_language_destination() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    let err = server
        .call_tool_by_name(
            "qartez_move",
            json!({ "symbol": "foo", "to_file": "src/b.ts" }),
        )
        .expect_err("cross-language destination must be rejected");
    assert!(
        err.contains("'.rs'") && err.contains("'.ts'"),
        "rejection must mention both extensions, got: {err}",
    );
}

#[test]
fn move_refuses_unknown_extension() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    let err = server
        .call_tool_by_name(
            "qartez_move",
            json!({ "symbol": "foo", "to_file": "src/notes.md" }),
        )
        .expect_err("unknown extension destination must be rejected");
    assert!(
        err.contains("'.md'") || err.contains("unsupported target extension"),
        "rejection must mention the unsupported extension, got: {err}",
    );
}

// ---------------------------------------------------------------------------
// H7 qartez_refs detailed mode collapses duplicate importer paths so one
// file with N call sites does not print N separate lines.
// ---------------------------------------------------------------------------

#[test]
fn refs_detailed_mode_collapses_duplicate_importer_paths() {
    let dir = TempDir::new().unwrap();
    let fixture = [
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        ("src/a.rs", "pub fn target_sym() -> u32 { 1 }\n"),
        (
            "src/b.rs",
            "use crate::a::target_sym;\n\
             pub fn one() -> u32 { target_sym() }\n\
             pub fn two() -> u32 { target_sym() }\n\
             pub fn three() -> u32 { target_sym() }\n",
        ),
    ];
    let server = build_and_index(dir.path(), &fixture);

    let out = server
        .call_tool_by_name("qartez_refs", json!({ "symbol": "target_sym" }))
        .expect("refs must succeed");

    // Importer file `src/b.rs` must appear only once in the detailed
    // Direct references block even though it references the target
    // from three call sites. The AST-resolved section below may still
    // list per-line hits, so restrict the count to the Direct
    // references block before the next section.
    let direct_header = out
        .find("Direct references")
        .expect("Direct references block must exist");
    let next_section = out[direct_header..]
        .find("\n\n")
        .map(|o| direct_header + o)
        .unwrap_or(out.len());
    let block = &out[direct_header..next_section];
    let b_line_count = block.matches("src/b.rs").count();
    assert!(
        b_line_count <= 1,
        "detailed mode must collapse duplicate importer paths, got {b_line_count} occurrences in:\n{block}",
    );
}

// ---------------------------------------------------------------------------
// H8 qartez_hotspots rejects format=mermaid with a clear error.
// ---------------------------------------------------------------------------

#[test]
fn hotspots_rejects_mermaid_format() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    let err = server
        .call_tool_by_name("qartez_hotspots", json!({ "format": "mermaid" }))
        .expect_err("format=mermaid must be rejected");
    assert!(
        err.contains("format=mermaid is not supported for qartez_hotspots"),
        "error must name the tool, got: {err}",
    );
    assert!(
        err.contains("qartez_deps")
            || err.contains("qartez_calls")
            || err.contains("qartez_hierarchy"),
        "error must point to graph-capable alternatives, got: {err}",
    );
}

// ---------------------------------------------------------------------------
// H9 qartez_project filter rejects shell metacharacters.
// ---------------------------------------------------------------------------

#[test]
fn project_filter_rejects_shell_metacharacters() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    for bad in ["'injected", "a;b", "x`y`", "a|b", "$(echo)"] {
        let err = server
            .call_tool_by_name("qartez_project", json!({ "action": "test", "filter": bad }))
            .expect_err("filter with shell metachars must be rejected");
        assert!(
            err.contains("unsupported character") || err.contains("start with '-'"),
            "rejection must mention filter rule, got: {err} for filter={bad}",
        );
    }
}

// ---------------------------------------------------------------------------
// H10 qartez_find empty-name error names the `name` parameter so the
// caller sees which field is wrong.
// ---------------------------------------------------------------------------

#[test]
fn find_empty_name_error_names_the_parameter() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());

    let err = server
        .call_tool_by_name("qartez_find", json!({ "name": "" }))
        .expect_err("empty name must be rejected");
    assert!(
        err.contains("`name`") && err.contains("non-empty"),
        "error must name the `name` parameter, got: {err}",
    );
}
