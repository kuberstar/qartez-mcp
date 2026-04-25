// Coverage for the runtime root-management tools introduced for #29:
// `qartez_add_root` and `qartez_list_roots`. Together they let an MCP
// client register a sibling repository, plugin folder, or attached
// workspace after the server is already running, without forcing a
// restart or a manual TOML edit.
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
    let conn = setup_db();
    index::full_index(&conn, dir, false).unwrap();
    QartezServer::new(conn, dir.to_path_buf(), 0)
}

#[test]
fn add_root_without_alias_derives_one_from_basename() {
    // The runtime tool must accept a path-only call: an agent that
    // discovers a sibling repo should be able to register it without
    // first inventing an alias. The derivation rule is
    // `sanitize(basename)`, with a numeric suffix on collision.
    let main = TempDir::new().unwrap();
    let server = build_and_index(main.path());

    let extra = TempDir::new().unwrap();
    fs::write(extra.path().join("y.rs"), "pub fn y() {}\n").unwrap();

    let out = server
        .call_tool_by_name(
            "qartez_add_root",
            json!({ "path": extra.path().to_str().unwrap() }),
        )
        .expect("qartez_add_root must accept a path-only call");

    assert!(
        out.contains("Added domain"),
        "expected success message, got: {out}"
    );
    let basename = extra
        .path()
        .file_name()
        .and_then(|n| n.to_str())
        .expect("temp dir basename");
    assert!(
        out.contains(basename),
        "derived alias must echo the basename, got: {out}"
    );
}

#[test]
fn list_roots_reports_runtime_source_after_add() {
    // Adds via `qartez_add_root` should be tagged `source=runtime` so
    // automation can distinguish them from cli/config roots without
    // having to diff against startup state.
    let main = TempDir::new().unwrap();
    let server = build_and_index(main.path());

    let extra = TempDir::new().unwrap();
    fs::write(extra.path().join("y.rs"), "pub fn y() {}\n").unwrap();

    server
        .call_tool_by_name(
            "qartez_add_root",
            json!({
                "path": extra.path().to_str().unwrap(),
                "alias": "demo-runtime",
                "persist": false,
            }),
        )
        .expect("add must succeed");

    let listing = server
        .call_tool_by_name("qartez_list_roots", json!({}))
        .expect("list must succeed");

    assert!(
        listing.contains("demo-runtime"),
        "listing must mention the new alias: {listing}"
    );
    assert!(
        listing.contains("runtime"),
        "listing must label the source as runtime: {listing}"
    );
}

#[test]
fn add_root_persist_false_does_not_touch_workspace_toml() {
    // Ephemeral roots are an explicit feature: an agent should be
    // able to attach a sibling folder for the duration of a session
    // without leaving a stale entry in `.qartez/workspace.toml`.
    let main = TempDir::new().unwrap();
    let server = build_and_index(main.path());

    let extra = TempDir::new().unwrap();
    fs::write(extra.path().join("y.rs"), "pub fn y() {}\n").unwrap();

    server
        .call_tool_by_name(
            "qartez_add_root",
            json!({
                "path": extra.path().to_str().unwrap(),
                "alias": "ephemeral",
                "persist": false,
                "watch": false,
            }),
        )
        .expect("ephemeral add must succeed");

    let toml_path = main.path().join(".qartez/workspace.toml");
    if toml_path.exists() {
        let content = fs::read_to_string(&toml_path).unwrap();
        assert!(
            !content.contains("ephemeral"),
            "persist=false must leave `.qartez/workspace.toml` untouched, got: {content}"
        );
    }
}

#[test]
fn add_root_persist_true_writes_workspace_toml() {
    // The default behaviour matches `qartez_workspace add`: the new
    // root should round-trip across restarts via workspace.toml.
    let main = TempDir::new().unwrap();
    let server = build_and_index(main.path());

    let extra = TempDir::new().unwrap();
    fs::write(extra.path().join("y.rs"), "pub fn y() {}\n").unwrap();

    server
        .call_tool_by_name(
            "qartez_add_root",
            json!({
                "path": extra.path().to_str().unwrap(),
                "alias": "persisted",
                "watch": false,
            }),
        )
        .expect("persisted add must succeed");

    let toml_path = main.path().join(".qartez/workspace.toml");
    let content = fs::read_to_string(&toml_path).expect("workspace.toml must exist after persist");
    assert!(
        content.contains("persisted"),
        "TOML must include the new alias, got: {content}"
    );
}

#[test]
fn list_roots_concise_format_drops_the_table() {
    // The concise format is meant for quick agent-side checks: just
    // a bullet list with `alias -> path`. Detailed mode is the
    // default and renders the full markdown table.
    let main = TempDir::new().unwrap();
    let server = build_and_index(main.path());

    let listing = server
        .call_tool_by_name("qartez_list_roots", json!({"format": "concise"}))
        .expect("list must succeed");

    assert!(
        listing.contains("# Project Roots"),
        "header must always render: {listing}"
    );
    assert!(
        !listing.contains("| alias | path |"),
        "concise format must not emit the markdown table: {listing}"
    );
}

#[test]
fn add_root_rejects_empty_path() {
    let main = TempDir::new().unwrap();
    let server = build_and_index(main.path());

    let err = server
        .call_tool_by_name("qartez_add_root", json!({"path": "   "}))
        .expect_err("blank path must be rejected");
    assert!(
        err.to_lowercase().contains("path"),
        "error must mention the path argument: {err}"
    );
}

#[test]
fn list_roots_after_add_includes_new_root_in_table() {
    // The detailed (default) format renders a markdown table; the
    // newly added root should appear as its own row with the alias
    // in the first column.
    let main = TempDir::new().unwrap();
    let server = build_and_index(main.path());

    let extra = TempDir::new().unwrap();
    fs::write(extra.path().join("y.rs"), "pub fn y() {}\n").unwrap();

    server
        .call_tool_by_name(
            "qartez_add_root",
            json!({
                "path": extra.path().to_str().unwrap(),
                "alias": "table-row",
                "persist": false,
                "watch": false,
            }),
        )
        .expect("add must succeed");

    let listing = server
        .call_tool_by_name("qartez_list_roots", json!({}))
        .expect("list must succeed");

    assert!(
        listing.contains("| alias |"),
        "default format must render the markdown table header: {listing}"
    );
    assert!(
        listing.contains("table-row"),
        "new root must appear in the listing: {listing}"
    );
}

#[test]
fn add_root_alias_collision_appends_numeric_suffix() {
    // When the derived alias collides with an existing one, the
    // tool must disambiguate with a `-2`, `-3`, ... suffix so the
    // call still succeeds without forcing the caller to hand-craft
    // a unique name.
    let main = TempDir::new().unwrap();
    let server = build_and_index(main.path());

    let extra1 = TempDir::new().unwrap();
    let shared = extra1.path().join("plugin");
    fs::create_dir_all(&shared).unwrap();
    fs::write(shared.join("a.rs"), "pub fn a() {}\n").unwrap();

    let extra2 = TempDir::new().unwrap();
    let shared2 = extra2.path().join("plugin");
    fs::create_dir_all(&shared2).unwrap();
    fs::write(shared2.join("b.rs"), "pub fn b() {}\n").unwrap();

    server
        .call_tool_by_name(
            "qartez_add_root",
            json!({"path": shared.to_str().unwrap(), "watch": false}),
        )
        .expect("first add must succeed and take alias 'plugin'");

    let second = server
        .call_tool_by_name(
            "qartez_add_root",
            json!({"path": shared2.to_str().unwrap(), "watch": false}),
        )
        .expect("second add must succeed via numeric suffix");

    assert!(
        second.contains("plugin-2"),
        "second add should resolve to `plugin-2`, got: {second}"
    );
}
