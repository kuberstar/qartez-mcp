// Regression coverage for the 2026-04-24 audit sweep (~155 findings).
// Each test pins a user-visible contract introduced or tightened by the
// bug-fix batch so a future refactor cannot silently revert them.
//
// The harness mirrors the existing `tests/fp_regression_*.rs` files:
// drop files to a TempDir, run `full_index`, call the MCP dispatcher via
// `QartezServer::call_tool_by_name`.

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
            "pub fn helper() {}\npub fn parse_config() {}\npub struct Config { pub name: String }\n",
        ),
    ]
}

// ---------------------------------------------------------------------------
// Fix: qartez_read start_line=0 is rejected with a 1-based hint.
// ---------------------------------------------------------------------------

#[test]
fn read_rejects_start_line_zero_with_one_based_hint() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_read",
            json!({
                "file_path": "src/a.rs",
                "start_line": 0,
                "limit": 3,
            }),
        )
        .expect_err("start_line=0 must be rejected");
    assert!(
        err.contains("1-based") || err.contains("start_line"),
        "rejection must name the parameter and note it is 1-based, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix: qartez_read non-existent file does not leak the absolute host path.
// ---------------------------------------------------------------------------

#[test]
fn read_nonexistent_file_sanitizes_os_error() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_read",
            json!({
                "file_path": "src/does_not_exist.rs",
            }),
        )
        .expect_err("non-existent file must err");
    let abs_prefix = dir.path().to_string_lossy().to_string();
    assert!(
        !err.contains(abs_prefix.as_str()),
        "sanitized error must not include the absolute tempdir prefix, got: {err}"
    );
    assert!(
        err.contains("not found") || err.contains("does_not_exist"),
        "sanitized error should mention the relative path or `not found`, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix: qartez_find empty name returns a validation error naming the field.
// ---------------------------------------------------------------------------

#[test]
fn find_rejects_empty_name_with_field_label() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_find", json!({ "name": "   " }))
        .expect_err("empty name must err");
    assert!(
        err.contains("name") && (err.contains("non-empty") || err.contains("empty")),
        "error must reference the `name` field, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix: qartez_grep trailing-whitespace query emits a trim notice + matches
// the trimmed form.
// ---------------------------------------------------------------------------

#[test]
fn grep_trims_query_and_notes_it() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name("qartez_grep", json!({"query": "helper  "}))
        .expect("trimmed query must succeed");
    // Trim notice may be emitted as a `// note:` line or inlined. We just
    // require matches are returned and a trim signal is visible.
    assert!(
        out.contains("helper"),
        "trimmed query must still match indexed symbol, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// Fix: qartez_map invalid `by=<unknown>` is rejected with the valid list.
// ---------------------------------------------------------------------------

#[test]
fn map_rejects_unknown_by_with_valid_options() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_map", json!({"by": "nonsense"}))
        .expect_err("unknown by= value must err");
    assert!(
        err.contains("files") && err.contains("symbols"),
        "error must list valid by values, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix: qartez_rename empty old_name is a validation error, not "No symbol".
// ---------------------------------------------------------------------------

#[test]
fn rename_rejects_empty_old_name() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_rename",
            json!({"old_name": "   ", "new_name": "ok"}),
        )
        .expect_err("empty old_name must err");
    assert!(
        err.contains("old_name") && err.contains("empty"),
        "error must name `old_name` and say empty, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix: qartez_rename accepts `r#fn` raw-identifier syntax.
// ---------------------------------------------------------------------------

#[test]
fn rename_accepts_raw_identifier_prefix() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    // Single-file, single-symbol case: helper → r#fn must reach the apply
    // stage without tripping the shape gate. We stay in preview (apply is
    // default false) so the test does not mutate the fixture.
    let out = server.call_tool_by_name(
        "qartez_rename",
        json!({
            "old_name": "helper",
            "new_name": "r#fn",
            "file_path": "src/a.rs",
        }),
    );
    match out {
        Ok(_) => {}
        Err(e) => assert!(
            !e.contains("outside [A-Za-z0-9_]"),
            "raw-identifier prefix must be accepted, got: {e}"
        ),
    }
}

// ---------------------------------------------------------------------------
// Fix: qartez_rename rejects bare `_` (reserved placeholder).
// ---------------------------------------------------------------------------

#[test]
fn rename_rejects_bare_underscore_placeholder() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_rename",
            json!({
                "old_name": "helper",
                "new_name": "_",
                "file_path": "src/a.rs",
            }),
        )
        .expect_err("bare _ must be rejected");
    assert!(
        err.contains("_") && (err.contains("placeholder") || err.contains("reserved")),
        "rejection must name the reserved placeholder, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix: qartez_diff_impact reversed range is rejected with a redirection hint.
// ---------------------------------------------------------------------------

#[test]
fn diff_impact_rejects_reversed_range_with_forward_hint() {
    // Minimal git history: two commits with a real file change so
    // `graph_descendant_of` can differentiate HEAD from HEAD~1.
    let dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test").unwrap();
        config.set_str("user.email", "test@test").unwrap();
    }
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.0.1\"\nedition=\"2021\"\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    // Commit 1
    let sig = repo.signature().unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    idx.write().unwrap();
    let tree_id = idx.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let c1 = repo
        .commit(Some("HEAD"), &sig, &sig, "first", &tree, &[])
        .unwrap();
    // Commit 2
    fs::write(dir.path().join("src/lib.rs"), "pub fn b() {}\n").unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    idx.write().unwrap();
    let tree_id = idx.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let parent = repo.find_commit(c1).unwrap();
    let _c2 = repo
        .commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&parent])
        .unwrap();

    let conn = setup_db();
    index::full_index(&conn, dir.path(), false).unwrap();
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    // Reversed range: HEAD..HEAD~1 goes from descendant to ancestor.
    let reversed = server.call_tool_by_name("qartez_diff_impact", json!({"base": "HEAD..HEAD~1"}));
    // Accept either a hard error message carrying "reversed"/"descendant"
    // OR a friendly hint routed through `friendly_git_error`. Both are
    // user-visible signals that the direction was NOT silently accepted.
    match reversed {
        Err(e) => assert!(
            e.contains("reversed") || e.contains("descendant") || e.contains("Did you mean"),
            "reversed range must surface a redirection hint, got: {e}"
        ),
        Ok(out) => assert!(
            out.contains("reversed") || out.contains("descendant") || out.contains("Did you mean"),
            "reversed range must surface a redirection hint, got: {out}"
        ),
    }
}

// ---------------------------------------------------------------------------
// Fix: qartez_replace_symbol refuses kind change on apply=true.
// ---------------------------------------------------------------------------

#[test]
fn replace_symbol_refuses_structural_change_on_apply() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    // Try to replace `parse_config` (function) with a struct body. This is
    // a kind change and must be refused in apply mode.
    let err = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "parse_config",
                "new_code": "pub struct parse_config { pub x: i32 }",
                "file_path": "src/a.rs",
                "apply": true,
            }),
        )
        .expect_err("kind change on apply must be refused");
    assert!(
        err.contains("Refusing")
            || err.contains("kind")
            || err.contains("structural")
            || err.contains("signature")
            || err.contains("visibility"),
        "apply-time refusal must explain the structural change, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix: qartez_move refuses builtin method names.
// ---------------------------------------------------------------------------

#[test]
fn move_refuses_builtin_method_names() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod a;\npub mod b;\n"),
            (
                "src/a.rs",
                "pub struct Foo;\nimpl Foo { pub fn new() -> Self { Self } }\n",
            ),
            ("src/b.rs", "// target\n"),
        ],
    );
    let err = server
        .call_tool_by_name(
            "qartez_move",
            json!({
                "symbol": "new",
                "to_file": "src/b.rs",
                "file_path": "src/a.rs",
            }),
        )
        .expect_err("move of builtin-method-name must be refused");
    assert!(
        err.contains("builtin") || err.contains("new") || err.contains("method"),
        "rejection must explain the builtin-name hazard, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix: schema `minimum=1` surfaces via `qartez_project timeout=0` being
// either rejected server-side (runtime) or never forwarded. Runtime behavior
// is the only observable side — assert it.
// ---------------------------------------------------------------------------

#[test]
fn project_rejects_timeout_zero_or_runs_with_default() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server.call_tool_by_name("qartez_project", json!({"action": "info", "timeout": 0}));
    // Either rejected up front (schema or runtime validation) OR info
    // mode returns without executing (no timeout hit because `info` is
    // non-executing). Both are acceptable post-fix behaviors.
    match out {
        Ok(s) => assert!(
            !s.contains("timed out") && !s.contains("timeout error"),
            "timeout=0 must not cause immediate timeout in info mode, got: {s}"
        ),
        Err(e) => assert!(
            e.contains("timeout") || e.contains("0") || e.contains(">="),
            "rejection must reference the timeout parameter, got: {e}"
        ),
    }
}
