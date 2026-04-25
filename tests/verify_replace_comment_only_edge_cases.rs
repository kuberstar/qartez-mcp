// Throwaway verification tests for the replace_symbol comment-only guard.
// Exercises 13 edge cases: positive cases must succeed, all-prelude inputs
// must fail with the new guard message pointing at qartez_safe_delete.

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

fn fresh_server() -> (QartezServer, TempDir) {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            (
                "src/lib.rs",
                "pub fn target_fn() -> u32 { 42 }\npub struct TargetStruct { pub x: u32 }\n",
            ),
        ],
    );
    (server, dir)
}

fn replace_target_fn(server: &QartezServer, new_code: &str) -> Result<String, String> {
    server.call_tool_by_name(
        "qartez_replace_symbol",
        json!({
            "symbol": "target_fn",
            "file_path": "src/lib.rs",
            "new_code": new_code,
            "apply": false,
        }),
    )
}

// ---------------------------------------------------------------------------
// Case 1: Pure happy path. Real fn replacement must succeed.
// ---------------------------------------------------------------------------
#[test]
fn case01_happy_path_real_fn_replacement_succeeds() {
    let (server, _dir) = fresh_server();
    let result = replace_target_fn(&server, "pub fn target_fn() -> u32 { 100 }").unwrap();
    assert!(
        result.starts_with("Preview: replace 'target_fn'"),
        "expected preview, got: {result}"
    );
}

// ---------------------------------------------------------------------------
// Case 2: Comment-only must Err with safe_delete pointer.
// ---------------------------------------------------------------------------
#[test]
fn case02_comment_only_rejected_with_safe_delete_pointer() {
    let (server, _dir) = fresh_server();
    let err = replace_target_fn(&server, "// just a comment").unwrap_err();
    assert!(
        err.contains("no definition introducer"),
        "expected guard rejection, got: {err}"
    );
    assert!(
        err.contains("qartez_safe_delete"),
        "expected qartez_safe_delete pointer, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Case 3: Block comment only.
// ---------------------------------------------------------------------------
#[test]
fn case03_block_comment_only_rejected() {
    let (server, _dir) = fresh_server();
    let err = replace_target_fn(&server, "/* block comment */").unwrap_err();
    assert!(
        err.contains("no definition introducer"),
        "expected guard rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Case 4: Doc comment only (///).
// ---------------------------------------------------------------------------
#[test]
fn case04_doc_comment_only_rejected() {
    let (server, _dir) = fresh_server();
    let err = replace_target_fn(&server, "/// doc comment").unwrap_err();
    assert!(
        err.contains("no definition introducer"),
        "expected guard rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Case 5: Inner doc comment only (//!).
// ---------------------------------------------------------------------------
#[test]
fn case05_inner_doc_comment_only_rejected() {
    let (server, _dir) = fresh_server();
    let err = replace_target_fn(&server, "//! inner").unwrap_err();
    assert!(
        err.contains("no definition introducer"),
        "expected guard rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Case 6: Attribute only.
// ---------------------------------------------------------------------------
#[test]
fn case06_attribute_only_rejected() {
    let (server, _dir) = fresh_server();
    let err = replace_target_fn(&server, "#[derive(Debug)]").unwrap_err();
    assert!(
        err.contains("no definition introducer"),
        "expected guard rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Case 7: Blank lines + comments mixed.
// ---------------------------------------------------------------------------
#[test]
fn case07_blank_lines_and_comments_rejected() {
    let (server, _dir) = fresh_server();
    let err = replace_target_fn(&server, "\n\n// comment\n\n").unwrap_err();
    assert!(
        err.contains("no definition introducer"),
        "expected guard rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Case 8: BOM + comment-only. The BOM should be stripped before the guard.
// ---------------------------------------------------------------------------
#[test]
fn case08_bom_plus_comment_only_rejected() {
    let (server, _dir) = fresh_server();
    let err = replace_target_fn(&server, "\u{FEFF}// just a comment").unwrap_err();
    assert!(
        err.contains("no definition introducer"),
        "expected guard rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Case 9: LEGITIMATE - mixed prelude + real code (struct).
// ---------------------------------------------------------------------------
#[test]
fn case09_mixed_prelude_plus_struct_succeeds() {
    let (server, _dir) = fresh_server();
    // Replace TargetStruct - must not be blocked by the prelude guard.
    let result = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "TargetStruct",
                "file_path": "src/lib.rs",
                "new_code": "#[derive(Debug)]\npub struct TargetStruct { pub x: u32 }",
                "apply": false,
            }),
        )
        .unwrap();
    assert!(
        result.starts_with("Preview: replace 'TargetStruct'"),
        "expected preview, got: {result}"
    );
}

// ---------------------------------------------------------------------------
// Case 10: LEGITIMATE - doc + real fn.
// ---------------------------------------------------------------------------
#[test]
fn case10_doc_plus_real_fn_succeeds() {
    let (server, _dir) = fresh_server();
    let result = replace_target_fn(
        &server,
        "/// docs for target_fn\npub fn target_fn() -> u32 { 0 }",
    )
    .unwrap();
    assert!(
        result.starts_with("Preview: replace 'target_fn'"),
        "expected preview, got: {result}"
    );
}

// ---------------------------------------------------------------------------
// Case 11: Whitespace-only must hit the existing empty-check (not the new guard).
// ---------------------------------------------------------------------------
#[test]
fn case11_whitespace_only_hits_empty_check() {
    let (server, _dir) = fresh_server();
    // The existing empty check uses `trim_end_matches('\n').is_empty()`,
    // which DOES NOT trim leading spaces. So `"   \n\t\n"` after
    // trim_end_matches('\n') -> "   \n\t" which is not empty. The NEW
    // guard catches this case via first_real_introducer_line returning
    // None (whitespace-only lines are skipped as blank).
    let err = replace_target_fn(&server, "   \n\t\n").unwrap_err();
    // Either the empty-check or the new guard catches it; both produce
    // an Err. We accept either path.
    assert!(
        err.contains("Empty `new_code`") || err.contains("no definition introducer"),
        "expected empty-check or guard rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Case 12: Mixed prelude + whitespace, no code.
// ---------------------------------------------------------------------------
#[test]
fn case12_prelude_plus_whitespace_no_code_rejected() {
    let (server, _dir) = fresh_server();
    let err = replace_target_fn(&server, "#[derive(Debug)]\n  \n// comment").unwrap_err();
    assert!(
        err.contains("no definition introducer"),
        "expected guard rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Case 13: `let x = 5;` - not a top-level Rust definition. Document behavior.
// ---------------------------------------------------------------------------
#[test]
fn case13_let_statement_behavior_documented() {
    let (server, _dir) = fresh_server();
    // `let` is NOT in the Rust introducer table. The new guard's
    // `first_real_introducer_line` returns the line itself (not prelude),
    // so the new guard does NOT block it. The downstream
    // `check_signature_shape` should reject it because the introducer
    // table for kind=function does not contain `let`.
    let err = replace_target_fn(&server, "let x = 5;").unwrap_err();
    // Confirm SOMETHING rejected it - either the new guard, the
    // signature-shape check, or the identifier check.
    assert!(
        !err.is_empty(),
        "expected some rejection of `let x = 5;`, got Ok"
    );
    // Print outcome for documentation.
    eprintln!("[case13] outcome for `let x = 5;`: {err}");
}

// ---------------------------------------------------------------------------
// Case 14 (BONUS): Empty string - existing empty check.
// ---------------------------------------------------------------------------
#[test]
fn case14_empty_string_hits_existing_empty_check() {
    let (server, _dir) = fresh_server();
    let err = replace_target_fn(&server, "").unwrap_err();
    assert!(
        err.contains("Empty `new_code`"),
        "expected empty-check rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Case 15 (BONUS): Apply=true with comment-only must NOT write the file.
// ---------------------------------------------------------------------------
#[test]
fn case15_apply_true_comment_only_does_not_modify_file() {
    let (server, dir) = fresh_server();
    let path = dir.path().join("src/lib.rs");
    let original = fs::read_to_string(&path).unwrap();

    let err = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "target_fn",
                "file_path": "src/lib.rs",
                "new_code": "// just a comment, no def",
                "apply": true,
            }),
        )
        .unwrap_err();
    assert!(
        err.contains("no definition introducer"),
        "expected guard rejection, got: {err}"
    );

    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(
        original, after,
        "file must NOT be modified when guard rejects apply=true"
    );
}
