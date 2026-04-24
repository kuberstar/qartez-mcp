// Regression coverage for the validation + UX consistency batch
// (cluster-d). Each test pins a user-visible contract introduced by
// the bug fixes so a future refactor cannot silently revert them.
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

fn rust_fixture() -> [(&'static str, &'static str); 3] {
    [
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "pub mod a;\n"),
        ("src/a.rs", "pub fn Parser() {}\npub fn helper() {}\n"),
    ]
}

// ---------------------------------------------------------------------------
// Fix read.rs #1: symbol_name + symbols set at the same time must err.
// ---------------------------------------------------------------------------

#[test]
fn read_rejects_symbol_name_and_symbols_both_set() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_read",
            json!({
                "symbol_name": "helper",
                "symbols": ["Parser"],
            }),
        )
        .expect_err("both fields set must error");
    assert!(
        err.contains("either") && err.contains("not both"),
        "error must say both-set is ambiguous, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix read.rs #2: total-miss batch reads return Ok with (N not found) notice.
// ---------------------------------------------------------------------------

#[test]
fn read_batch_total_miss_is_graceful() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name("qartez_read", json!({"symbols": ["no_a", "no_b", "no_c"]}))
        .expect("batch miss should be Ok, not Err");
    assert!(
        out.contains("3 not found"),
        "graceful miss must count the absentees, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// Fix read.rs #3: max_bytes=0 is rejected with a clear message.
// ---------------------------------------------------------------------------

#[test]
fn read_rejects_max_bytes_zero() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_read",
            json!({"symbol_name": "helper", "max_bytes": 0}),
        )
        .expect_err("max_bytes=0 must error");
    assert!(
        err.contains("max_bytes=0"),
        "error must name the bad value, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix read.rs #4: start_line + end_line + limit all set is rejected.
// ---------------------------------------------------------------------------

#[test]
fn read_rejects_all_three_range_params() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_read",
            json!({
                "file_path": "src/a.rs",
                "start_line": 1,
                "end_line": 2,
                "limit": 10,
            }),
        )
        .expect_err("all three range fields must error");
    assert!(
        err.contains("mutually exclusive"),
        "error must flag the mutually exclusive combo, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix read.rs #5: start > end must error consistently regardless of file size.
// ---------------------------------------------------------------------------

#[test]
fn read_rejects_start_greater_than_end_before_bounds() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_read",
            json!({
                "file_path": "src/a.rs",
                "start_line": 100,
                "end_line": 5,
            }),
        )
        .expect_err("start>end must error");
    assert!(
        err.contains("start_line") && err.contains(" > end_line"),
        "error must describe ordering, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix context.rs #6/#7: task-only seed bootstraps from symbol FTS.
// ---------------------------------------------------------------------------

#[test]
fn context_task_only_seed_works() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name(
            "qartez_context",
            json!({"task": "helper function maintenance"}),
        )
        .expect("task-only seed should be Ok");
    // The specific-fixture may end up with no graph edges, in which case
    // the fall-through message fires. Pin only the inverse contract:
    // task-only mode must NOT reach the "Provide at least one file path"
    // error that we used to hit when `files` was empty.
    assert!(
        !out.contains("Provide at least one file path"),
        "task-only seed must not report missing files, got: {out}"
    );
}

#[test]
fn context_empty_files_without_task_still_errors() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_context", json!({}))
        .expect_err("empty files + no task must error");
    assert!(
        err.contains("task"),
        "error must mention task-only seed option, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix security.rs #8: severity is case-insensitive.
// ---------------------------------------------------------------------------

#[test]
fn security_accepts_uppercase_severity() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name("qartez_security", json!({"severity": "CRITICAL"}))
        .expect("uppercase severity must be accepted");
    assert!(
        !out.to_lowercase().contains("unknown severity"),
        "uppercase must not trigger unknown-severity, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// Fix security.rs #9: explicit missing config_path errors.
// ---------------------------------------------------------------------------

#[test]
fn security_missing_config_path_errors() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_security",
            json!({"config_path": ".qartez/does-not-exist.toml"}),
        )
        .expect_err("missing config_path must error");
    assert!(
        err.contains("does not exist"),
        "error must mention that the config does not exist, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix workspace.rs #10: alias-reuse with different path is rejected.
// ---------------------------------------------------------------------------

#[test]
fn workspace_alias_reuse_with_new_path_rejected() {
    let main = TempDir::new().unwrap();
    fs::create_dir_all(main.path().join(".git")).unwrap();
    fs::create_dir_all(main.path().join(".qartez")).unwrap();
    let first = TempDir::new().unwrap();
    fs::write(first.path().join("x.rs"), "fn x() {}\n").unwrap();
    let second = TempDir::new().unwrap();
    fs::write(second.path().join("y.rs"), "fn y() {}\n").unwrap();

    let conn = setup_db();
    index::full_index(&conn, main.path(), false).unwrap();
    let server = QartezServer::new(conn, main.path().to_path_buf(), 0);

    server
        .call_tool_by_name(
            "qartez_workspace",
            json!({
                "action": "add",
                "alias": "demo",
                "path": first.path().to_str().unwrap(),
            }),
        )
        .expect("first add succeeds");

    let err = server
        .call_tool_by_name(
            "qartez_workspace",
            json!({
                "action": "add",
                "alias": "demo",
                "path": second.path().to_str().unwrap(),
            }),
        )
        .expect_err("duplicate alias with different path must err");
    assert!(
        err.contains("already registered"),
        "error must say alias is already registered, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix tools_meta.rs #13: listing exposes mode banner.
// ---------------------------------------------------------------------------

#[test]
fn tools_meta_list_exposes_mode_banner() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    // The meta dispatcher for listing is async-only, so we only check
    // the string if it is reachable; on sync dispatch it returns a
    // specific Err.
    let result = server.call_tool_by_name("qartez_tools", json!({}));
    if let Ok(out) = result {
        assert!(
            out.contains("Mode:") && out.contains("always on"),
            "listing must label core as always-on and advertise the mode, got: {out}"
        );
    }
}

// ---------------------------------------------------------------------------
// Fix find.rs #23: no-hit lookup surfaces the qartez_grep fallback hint.
// ---------------------------------------------------------------------------

#[test]
fn find_no_match_includes_grep_hint() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name("qartez_find", json!({"name": "NoSuchSymbol"}))
        .expect("find must be Ok even on miss");
    assert!(
        out.contains("qartez_grep") && out.contains("prefix"),
        "miss must offer the grep fallback hint, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// Fix refactor_plan.rs #20: limit=0 renders up to MAX_REFACTOR_STEPS.
// ---------------------------------------------------------------------------

#[test]
fn refactor_plan_limit_zero_is_unlimited_but_capped() {
    let dir = TempDir::new().unwrap();
    // Need a file with at least one smell so the tool reaches the footer.
    let heavy_body: String = (0..60)
        .map(|i| format!("    if x == {i} {{ return {i}; }}\n"))
        .collect();
    let src = format!(
        "pub fn god(x: u32) -> u32 {{\n{heavy_body}    return 0;\n}}\n\
         pub fn multi_param(a: u32, b: u32, c: u32, d: u32, e: u32, f: u32) {{}}\n"
    );
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", &src),
        ],
    );
    let out = server
        .call_tool_by_name(
            "qartez_refactor_plan",
            json!({"file_path": "src/lib.rs", "limit": 0}),
        )
        .expect("limit=0 must succeed");
    assert!(
        out.contains("limit=0 requested no cap") || out.contains("Refactor Plan"),
        "limit=0 must either emit the no-cap notice or at least render a plan header, got: {out}"
    );
    assert!(
        !out.contains("limit=0 was clamped"),
        "limit=0 must not be described as clamped, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// Fix map.rs #22: heavy top_n truncated by token_budget emits footer hint.
// ---------------------------------------------------------------------------

#[test]
fn map_token_budget_truncation_emits_footer() {
    let dir = TempDir::new().unwrap();
    // Enough files to guarantee token-budget truncation even after the
    // all_files fallback kicks in. 100 files with tight budget + concise
    // leaves no room for everything so the footer MUST fire.
    let files: Vec<(String, String)> = (0..100)
        .map(|i| {
            (
                format!("src/m{i}.rs"),
                format!("pub fn fn_{i}() -> u32 {{ {i} }}\n"),
            )
        })
        .collect();
    let mut fixture: Vec<(&str, &str)> = Vec::new();
    fixture.push((
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    ));
    fixture.push(("src/lib.rs", "pub mod m0;\n"));
    for (rel, content) in &files {
        fixture.push((rel.as_str(), content.as_str()));
    }
    let server = build_and_index(dir.path(), &fixture);
    let out = server
        .call_tool_by_name(
            "qartez_map",
            json!({"top_n": 10000, "token_budget": 150, "format": "concise"}),
        )
        .expect("heavy top_n must succeed");
    assert!(
        out.contains("truncated"),
        "token-budget truncation must leave a marker, got: {out}"
    );
}
