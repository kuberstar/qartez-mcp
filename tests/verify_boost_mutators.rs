// Verification tests for two boost mutator correctness fixes:
//   QW1 - check_trailing_content in src/server/tools/replace.rs, exercised
//         end-to-end through qartez_replace_symbol on real temp files.
//   C2  - CRLF line-ending preservation across replace / insert_after /
//         safe_delete (src/server/tools/refactor_common.rs + tools).
//
// All tests build a real index over temp files and drive the tools via
// `call_tool_by_name`, mirroring tests/verify_replace_comment_only_edge_cases.rs.

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
        // Write bytes exactly as given so CRLF fixtures are preserved.
        fs::write(&path, content.as_bytes()).unwrap();
    }
    let conn = setup_db();
    index::full_index(&conn, dir, false).unwrap();
    QartezServer::new(conn, dir.to_path_buf(), 0)
}

const CARGO: (&str, &str) = (
    "Cargo.toml",
    "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
);

// ---------------------------------------------------------------------------
// QW1: check_trailing_content, ACCEPT + REJECT via qartez_replace_symbol
// ---------------------------------------------------------------------------

// One fixture file whose symbols exactly match (kind + name + signature +
// visibility) the new_code used in each accept/reject case, so earlier guards
// (signature-shape, identifier-match) pass and control reaches
// check_trailing_content. Preview mode (apply=false) is used so structural
// warnings do not turn into refusals - a preview Ok means the trailing-content
// gate accepted, an Err containing "trailing content" means it rejected.
fn trailing_server() -> (QartezServer, TempDir) {
    let dir = TempDir::new().unwrap();
    let fixture = concat!(
        "const CONFIG: Config = Config { timeout: 1 };\n",
        "static R: Map = Map { };\n",
        "const X: u32 = { 0 + 0 };\n",
        "fn f() {}\n",
        "struct Foo;\n",
        "const F: fn() = || {};\n",
        "fn g() { let s = \"z\"; }\n",
        "fn h() {}\n",
    );
    let server = build_and_index(dir.path(), &[CARGO, ("src/fixture.rs", fixture)]);
    (server, dir)
}

fn replace_preview(server: &QartezServer, symbol: &str, new_code: &str) -> Result<String, String> {
    server.call_tool_by_name(
        "qartez_replace_symbol",
        json!({
            "symbol": symbol,
            "file_path": "src/fixture.rs",
            "new_code": new_code,
            "apply": false,
        }),
    )
}

fn assert_accept(server: &QartezServer, symbol: &str, new_code: &str, label: &str) {
    let res = replace_preview(server, symbol, new_code);
    match res {
        Ok(out) => assert!(
            out.starts_with("Preview: replace"),
            "[{label}] expected Preview, got: {out}"
        ),
        Err(e) => panic!("[{label}] expected ACCEPT (Ok preview) for `{new_code}`, got Err: {e}"),
    }
}

fn assert_reject_trailing(server: &QartezServer, symbol: &str, new_code: &str, label: &str) {
    let res = replace_preview(server, symbol, new_code);
    match res {
        Ok(out) => panic!("[{label}] expected REJECT for `{new_code}`, got Ok: {out}"),
        Err(e) => assert!(
            e.contains("trailing content"),
            "[{label}] expected trailing-content rejection, got a different error: {e}"
        ),
    }
}

#[test]
fn qw1_accept_const_struct_literal_initializer() {
    let (server, _dir) = trailing_server();
    assert_accept(
        &server,
        "CONFIG",
        "const CONFIG: Config = Config { timeout: 30 };",
        "const-struct-literal",
    );
}

// NOTE: the static-with-map-literal ACCEPT case cannot be exercised through
// qartez_replace_symbol end-to-end, because the Rust indexer classifies a
// top-level `static` as kind "variable" (src/index/languages/rust_lang.rs
// static_item -> SymbolKind::Variable), and replace.rs refuses "variable" at
// the NON_DEFINITION_KINDS guard *before* check_trailing_content runs. This
// is pre-existing behaviour, not the QW1 fix. The check_trailing_content
// logic for a brace-terminated item with a trailing `;` (the exact shape of
// `static R: Map = Map { };`) is identical to, and is verified by, the
// const-struct-literal ACCEPT case above. This test pins the actual
// end-to-end outcome so the limitation is documented.
#[test]
fn qw1_static_is_refused_as_variable_kind_preexisting() {
    let (server, _dir) = trailing_server();
    let err = replace_preview(&server, "R", "static R: Map = Map { };").unwrap_err();
    assert!(
        err.contains("is not a standalone definition") && err.contains("variable"),
        "expected the pre-existing variable-kind refusal, got: {err}"
    );
}

#[test]
fn qw1_accept_const_block_expr() {
    let (server, _dir) = trailing_server();
    assert_accept(
        &server,
        "X",
        "const X: u32 = { 1 + 2 };",
        "const-block-expr",
    );
}

#[test]
fn qw1_accept_plain_fn() {
    let (server, _dir) = trailing_server();
    assert_accept(&server, "f", "fn f() { let _x = 1; }", "plain-fn");
}

#[test]
fn qw1_accept_unit_struct() {
    let (server, _dir) = trailing_server();
    assert_accept(&server, "Foo", "struct Foo;", "unit-struct");
}

#[test]
fn qw1_accept_trailing_line_comment() {
    let (server, _dir) = trailing_server();
    assert_accept(
        &server,
        "h",
        "fn h() {}\n// trailing explanation only",
        "trailing-comment",
    );
}

#[test]
fn qw1_accept_braces_and_semis_inside_string() {
    let (server, _dir) = trailing_server();
    assert_accept(
        &server,
        "g",
        "fn g() { let s = \"a;b{c}\"; }",
        "string-with-braces-semis",
    );
}

#[test]
fn qw1_accept_closure_initializer_const() {
    let (server, _dir) = trailing_server();
    assert_accept(
        &server,
        "F",
        "const F: fn() = || {};",
        "closure-initializer",
    );
}

#[test]
fn qw1_reject_two_items_struct_then_fn() {
    let (server, _dir) = trailing_server();
    assert_reject_trailing(
        &server,
        "Foo",
        "struct Foo;\npub fn bar() {}",
        "two-items-struct-fn",
    );
}

#[test]
fn qw1_reject_const_block_then_fn() {
    let (server, _dir) = trailing_server();
    assert_reject_trailing(
        &server,
        "X",
        "const X: u32 = { 1 + 2 };\npub fn evil() {}",
        "const-block-then-fn",
    );
}

#[test]
fn qw1_reject_fn_then_stray_fn() {
    let (server, _dir) = trailing_server();
    assert_reject_trailing(&server, "f", "fn f() {}\nfn stray() {}", "fn-then-stray-fn");
}

// ---------------------------------------------------------------------------
// C2: CRLF preservation
// ---------------------------------------------------------------------------

/// True when every `\n` in `s` is immediately preceded by `\r` (i.e. pure
/// CRLF, no stray bare LF introduced by a whole-file flip).
fn all_lf_are_crlf(s: &str) -> bool {
    !s.replace("\r\n", "").contains('\n')
}

fn crlf_three_fns() -> String {
    "pub fn alpha() -> u32 { 1 }\r\npub fn beta() -> u32 { 2 }\r\npub fn gamma() -> u32 { 3 }\r\n"
        .to_string()
}

#[test]
fn c2_replace_preserves_crlf_on_untouched_lines() {
    let dir = TempDir::new().unwrap();
    let content = crlf_three_fns();
    let server = build_and_index(dir.path(), &[CARGO, ("src/crlf_rep.rs", &content)]);

    let out = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "beta",
                "file_path": "src/crlf_rep.rs",
                "new_code": "pub fn beta() -> u32 { 20 }",
                "apply": true,
            }),
        )
        .expect("replace should apply");
    assert!(out.starts_with("Replaced"), "got: {out}");

    let after = fs::read_to_string(dir.path().join("src/crlf_rep.rs")).unwrap();
    assert!(after.contains("\r\n"), "CRLF must survive: {after:?}");
    assert!(
        !after.contains("\r\r\n"),
        "no doubled CR at seam: {after:?}"
    );
    assert!(all_lf_are_crlf(&after), "no bare LF flip: {after:?}");
    assert!(
        after.contains("pub fn alpha() -> u32 { 1 }\r\n"),
        "untouched alpha keeps CRLF: {after:?}"
    );
    assert!(
        after.contains("pub fn gamma() -> u32 { 3 }\r\n"),
        "untouched gamma keeps CRLF: {after:?}"
    );
    assert!(after.contains("{ 20 }"), "body changed: {after:?}");
}

#[test]
fn c2_replace_crlf_new_code_no_double_cr_at_seam() {
    let dir = TempDir::new().unwrap();
    let content = crlf_three_fns();
    let server = build_and_index(dir.path(), &[CARGO, ("src/crlf_seam.rs", &content)]);

    // new_code itself carries CRLF; the splice must strip the trailing \r
    // so no \r\r\n appears.
    server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "beta",
                "file_path": "src/crlf_seam.rs",
                "new_code": "pub fn beta() -> u32 { 21 }\r\n",
                "apply": true,
            }),
        )
        .expect("replace should apply");

    let after = fs::read_to_string(dir.path().join("src/crlf_seam.rs")).unwrap();
    assert!(
        !after.contains("\r\r\n"),
        "no doubled CR at seam: {after:?}"
    );
    assert!(all_lf_are_crlf(&after), "no bare LF: {after:?}");
    assert!(after.contains("{ 21 }"), "body changed: {after:?}");
}

#[test]
fn c2_replace_noop_does_not_corrupt_crlf() {
    let dir = TempDir::new().unwrap();
    let content = crlf_three_fns();
    let server = build_and_index(dir.path(), &[CARGO, ("src/crlf_noop.rs", &content)]);
    let before = fs::read(dir.path().join("src/crlf_noop.rs")).unwrap();

    let out = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "beta",
                "file_path": "src/crlf_noop.rs",
                // Identical to the existing definition.
                "new_code": "pub fn beta() -> u32 { 2 }",
                "apply": true,
            }),
        )
        .expect("noop replace should be Ok");
    assert!(out.contains("No changes"), "expected no-op message: {out}");

    let after = fs::read(dir.path().join("src/crlf_noop.rs")).unwrap();
    assert_eq!(before, after, "no-op edit must not touch bytes");
}

#[test]
fn c2_insert_after_preserves_crlf() {
    let dir = TempDir::new().unwrap();
    let content = crlf_three_fns();
    let server = build_and_index(dir.path(), &[CARGO, ("src/crlf_ins.rs", &content)]);

    server
        .call_tool_by_name(
            "qartez_insert_after_symbol",
            json!({
                "symbol": "alpha",
                "file_path": "src/crlf_ins.rs",
                "new_code": "pub fn inserted() -> u32 { 9 }",
                "apply": true,
            }),
        )
        .expect("insert should apply");

    let after = fs::read_to_string(dir.path().join("src/crlf_ins.rs")).unwrap();
    assert!(
        after.contains("pub fn inserted() -> u32 { 9 }"),
        "inserted: {after:?}"
    );
    assert!(
        !after.contains("\r\r\n"),
        "no doubled CR at seam: {after:?}"
    );
    assert!(all_lf_are_crlf(&after), "no bare LF flip: {after:?}");
    assert!(
        after.contains("pub fn alpha() -> u32 { 1 }\r\n"),
        "alpha keeps CRLF: {after:?}"
    );
    assert!(
        after.contains("pub fn beta() -> u32 { 2 }\r\n"),
        "beta keeps CRLF: {after:?}"
    );
    // The inserted line must itself terminate with CRLF, not LF.
    assert!(
        after.contains("pub fn inserted() -> u32 { 9 }\r\n"),
        "inserted line terminates CRLF: {after:?}"
    );
}

#[test]
fn c2_safe_delete_preserves_crlf() {
    let dir = TempDir::new().unwrap();
    let content = crlf_three_fns();
    let server = build_and_index(dir.path(), &[CARGO, ("src/crlf_del.rs", &content)]);

    server
        .call_tool_by_name(
            "qartez_safe_delete",
            json!({
                "symbol": "beta",
                "file_path": "src/crlf_del.rs",
                "apply": true,
                "force": true,
            }),
        )
        .expect("delete should apply");

    let after = fs::read_to_string(dir.path().join("src/crlf_del.rs")).unwrap();
    assert!(!after.contains("beta"), "beta removed: {after:?}");
    assert!(!after.contains("\r\r\n"), "no doubled CR: {after:?}");
    assert!(all_lf_are_crlf(&after), "no bare LF flip: {after:?}");
    assert!(
        after.contains("pub fn alpha() -> u32 { 1 }\r\n"),
        "alpha keeps CRLF: {after:?}"
    );
    assert!(
        after.contains("pub fn gamma() -> u32 { 3 }\r\n"),
        "gamma keeps CRLF: {after:?}"
    );
}

#[test]
fn c2_lf_file_stays_lf() {
    let dir = TempDir::new().unwrap();
    let content = "pub fn a() -> u32 { 1 }\npub fn b() -> u32 { 2 }\n";
    let server = build_and_index(dir.path(), &[CARGO, ("src/lf.rs", content)]);

    server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "b",
                "file_path": "src/lf.rs",
                "new_code": "pub fn b() -> u32 { 22 }",
                "apply": true,
            }),
        )
        .expect("replace should apply");

    let after = fs::read_to_string(dir.path().join("src/lf.rs")).unwrap();
    assert!(!after.contains('\r'), "LF file must not gain CR: {after:?}");
    assert!(after.contains("{ 22 }"), "body changed: {after:?}");
}

#[test]
fn c2_crlf_no_trailing_newline_round_trips() {
    let dir = TempDir::new().unwrap();
    // No trailing newline after the last line.
    let content =
        "pub fn one() -> u32 { 1 }\r\npub fn two() -> u32 { 2 }\r\npub fn three() -> u32 { 3 }";
    let server = build_and_index(dir.path(), &[CARGO, ("src/crlf_notrail.rs", content)]);

    server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "two",
                "file_path": "src/crlf_notrail.rs",
                "new_code": "pub fn two() -> u32 { 22 }",
                "apply": true,
            }),
        )
        .expect("replace should apply");

    let after = fs::read_to_string(dir.path().join("src/crlf_notrail.rs")).unwrap();
    assert!(
        !after.ends_with('\n'),
        "missing trailing newline must be preserved: {after:?}"
    );
    assert!(!after.contains("\r\r\n"), "no doubled CR: {after:?}");
    assert!(all_lf_are_crlf(&after), "no bare LF flip: {after:?}");
    assert!(
        after.contains("pub fn one() -> u32 { 1 }\r\n"),
        "one keeps CRLF: {after:?}"
    );
    assert!(
        after.contains("pub fn three() -> u32 { 3 }"),
        "three intact: {after:?}"
    );
    assert!(after.contains("{ 22 }"), "body changed: {after:?}");
}
