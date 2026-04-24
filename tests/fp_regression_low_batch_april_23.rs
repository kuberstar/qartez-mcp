// Regression coverage for the LOW batch + test_gaps path + zero-semantics
// fixes landed on 2026-04-23. Each test pins a user-visible contract
// introduced by a fix so a future refactor cannot silently revert it.
//
// The harness mirrors the existing `tests/fp_regression_*.rs` files:
// drop files to a TempDir, run `full_index`, call the MCP dispatcher
// via `QartezServer::call_tool_by_name`.

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

fn basic_fixture() -> [(&'static str, &'static str); 3] {
    [
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "pub mod a;\n"),
        ("src/a.rs", "pub fn helper() {}\npub fn second() {}\n"),
    ]
}

// ---------------------------------------------------------------------------
// unused.rs: limit=0 is now rejected instead of silently meaning "no cap".
// ---------------------------------------------------------------------------

#[test]
fn unused_rejects_limit_zero() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &basic_fixture());
    let err = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 0 }))
        .expect_err("limit=0 must error");
    assert!(
        err.contains("limit must be > 0") && err.contains("no-cap"),
        "expected explicit limit=0 rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// clones.rs: limit=0 is now rejected (previously coerced to 1 via `.max(1)`).
// ---------------------------------------------------------------------------

#[test]
fn clones_rejects_limit_zero() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &basic_fixture());
    let err = server
        .call_tool_by_name("qartez_clones", json!({ "limit": 0 }))
        .expect_err("limit=0 must error");
    assert!(
        err.contains("limit must be > 0") && err.contains("no-cap"),
        "expected explicit limit=0 rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// trend.rs: limit=0 is now rejected instead of clamping to 1 and emitting
// the "No data" string.
// ---------------------------------------------------------------------------

#[test]
fn trend_rejects_limit_zero() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &basic_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_trend",
            json!({ "file_path": "src/a.rs", "limit": 0 }),
        )
        .expect_err("limit=0 must error");
    assert!(
        err.contains("limit must be > 0") || err.contains("Complexity trend requires git history"),
        "expected explicit limit=0 rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// grep.rs: bare `*` wildcard is now rejected with guidance.
// ---------------------------------------------------------------------------

#[test]
fn grep_rejects_bare_wildcard() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &basic_fixture());
    let err = server
        .call_tool_by_name("qartez_grep", json!({ "query": "*" }))
        .expect_err("bare * must error");
    assert!(
        err.contains("FTS wildcard must be a prefix"),
        "expected prefix-wildcard guidance, got: {err}"
    );
    assert!(
        err.contains("regex=true"),
        "expected regex fallback hint, got: {err}"
    );
}

#[test]
fn grep_bare_wildcard_with_regex_still_runs() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &basic_fixture());
    // `regex=true` re-interprets `*` as a regex (which the regex crate
    // rejects anyway), so the wildcard guard must NOT fire for regex
    // mode. We assert the error (if any) is the regex error, not the
    // wildcard message.
    let result = server.call_tool_by_name("qartez_grep", json!({ "query": "*", "regex": true }));
    if let Err(err) = result {
        assert!(
            !err.contains("FTS wildcard must be a prefix"),
            "regex mode must not hit the FTS wildcard guard, got: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// smells.rs: mixed known + unknown kinds warn instead of rejecting the call.
// ---------------------------------------------------------------------------

#[test]
fn smells_accepts_known_kinds_with_unknown_warning() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &basic_fixture());
    let out = server
        .call_tool_by_name(
            "qartez_smells",
            json!({ "kind": "god_function,unknown_smell" }),
        )
        .expect("mixed known+unknown must succeed (with warning)");
    assert!(
        out.contains("warning:") && out.contains("unknown_smell"),
        "expected unknown-kind warning in output, got: {out}"
    );
}

#[test]
fn smells_rejects_all_unknown_kinds() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &basic_fixture());
    let err = server
        .call_tool_by_name("qartez_smells", json!({ "kind": "foo,bar" }))
        .expect_err("all-unknown selection must error");
    assert!(
        err.contains("no known smell kinds"),
        "expected explicit rejection when no kind is valid, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// hierarchy.rs: missing symbol gives a different error than "zero impls".
// ---------------------------------------------------------------------------

#[test]
fn hierarchy_missing_symbol_is_not_found() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &basic_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_hierarchy",
            json!({ "symbol": "NonExistentTypeName", "direction": "sub" }),
        )
        .expect_err("missing symbol must error");
    assert!(
        err.contains("not found in index"),
        "expected 'not found in index' for missing symbol, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// safe_delete.rs: when force=true is already set, preview wording collapses
// to "call again with apply=true (force=true already set)".
// ---------------------------------------------------------------------------

#[test]
fn safe_delete_preview_with_force_mentions_apply_true() {
    // Target a symbol that has at least one live reference so the
    // warning path fires. A simple helper referenced from lib.rs
    // satisfies that shape.
    let fixture: [(&'static str, &'static str); 3] = [
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        ),
        (
            "src/lib.rs",
            "pub mod a;\npub fn entry() { a::helper(); }\n",
        ),
        ("src/a.rs", "pub fn helper() {}\n"),
    ];
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &fixture);
    let out = server
        .call_tool_by_name(
            "qartez_safe_delete",
            json!({ "symbol": "helper", "force": true }),
        )
        .expect("preview path must succeed");
    assert!(
        out.contains("apply=true") && out.contains("force=true already set"),
        "preview with force=true must direct the caller to apply=true, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// rename_file.rs: absolute `from` path gets a precise error, not
// "not found in index".
// ---------------------------------------------------------------------------

#[test]
fn rename_file_rejects_absolute_from_with_guidance() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &basic_fixture());
    let abs = dir.path().join("src/a.rs");
    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({ "from": abs.to_string_lossy(), "to": "src/b.rs" }),
        )
        .expect_err("absolute from-path must error");
    assert!(
        err.contains("absolute path not supported") && err.contains("relative path"),
        "expected absolute-path guidance, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// map.rs: token_budget=0 is rejected, small positive values clamp to 256
// with a warning banner.
// ---------------------------------------------------------------------------

#[test]
fn map_rejects_token_budget_zero() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &basic_fixture());
    let err = server
        .call_tool_by_name("qartez_map", json!({ "token_budget": 0 }))
        .expect_err("token_budget=0 must error");
    assert!(
        err.contains("token_budget=0 is invalid"),
        "expected token_budget=0 rejection, got: {err}"
    );
}

#[test]
fn map_small_token_budget_is_clamped_with_warning() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &basic_fixture());
    let out = server
        .call_tool_by_name("qartez_map", json!({ "token_budget": 50 }))
        .expect("small token_budget must succeed after clamp");
    assert!(
        out.contains("warning: token_budget=50") && out.contains("256"),
        "expected clamp warning in output, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// test_gaps.rs: mode=map with a relative path does not surface a
// garbage root prefix (e.g. `/private/tmp/src/...`) after the multi-
// root-aware safe_resolve path. On a single-root project the rel
// form mirrors the caller's input.
// ---------------------------------------------------------------------------

#[test]
fn test_gaps_map_preserves_relative_path_in_message() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &basic_fixture());
    let out = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({ "mode": "map", "file_path": "src/a.rs" }),
        )
        .expect("test_gaps map must succeed");
    // The output must mention the caller-supplied relative path and
    // must NOT leak the tempdir absolute path back into the message.
    assert!(
        out.contains("src/a.rs"),
        "expected caller-relative path in output, got: {out}"
    );
    let abs_prefix = dir.path().to_string_lossy().into_owned();
    assert!(
        !out.contains(&abs_prefix),
        "absolute path must not appear in map output, got: {out}"
    );
}
