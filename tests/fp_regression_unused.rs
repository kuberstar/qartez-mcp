// Rust guideline compliant 2026-04-26
//
// Regression coverage for the framework-convention entry-point filter
// added to `qartez_unused`. External runtimes (OpenCode plugin host,
// VS Code extension API, CLI script loaders, SvelteKit's route + hooks
// dispatcher) resolve exports by string name, so the static reference
// graph never records an edge to those symbols. Without the filter, the
// tool reports them as dead and hides real positives behind noise. The
// filter keys on:
//
//   1. directory prefix - `scripts/`, `plugins/`, `extensions/`
//   2. plugin basename  - `plugin.*`, `extension.*`, `*-plugin.*`,
//                         `*-extension.*`
//   3. SvelteKit route  - `+page.*`, `+page.server.*`, `+layout.*`,
//                         `+layout.server.*`, `+server.*`, `+error.*`,
//                         `hooks.server.*`, `hooks.client.*`
//   4. framework config - `svelte.config.*`, `vite.config.*`,
//                         `playwright.config.*`
//
// Fixture A exercises the path-prefix and basename branches with
// realistic OpenCode-style and SvelteKit-style entry points and asserts
// the tool does NOT flag them. Fixture B exercises the truly-dead case
// - an exported function in `src/` that no one calls - and asserts the
// tool DOES still flag it, protecting the tool's core detection signal.

use std::fs;
use std::path::Path;

use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use qartez_mcp::index;
use qartez_mcp::server::QartezServer;
use qartez_mcp::storage::schema;

// ---------------------------------------------------------------------------
// Shared fixture helpers
// ---------------------------------------------------------------------------

fn setup_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    conn
}

/// Build a project rooted at `dir`, drop the supplied fixture files onto
/// disk, run `full_index` against the project, and hand back a ready-to-
/// query `QartezServer`. A `.git` marker is added so downstream tools
/// recognise the TempDir as a real project root.
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
// Fixture A: plugin entry-point files are NOT flagged as unused
// ---------------------------------------------------------------------------

#[test]
fn qartez_unused_skips_scripts_dir_plugin_entry_points() {
    let dir = TempDir::new().unwrap();
    // OpenCode-shape plugin: exported top-level symbol the external host
    // loads by string name. No local importer exists, so without the
    // filter the indexer records zero in-edges and the tool flags it.
    let plugin_ts = r#"export interface Plugin {
  readonly name?: string;
}

export const Plugin = async () => {
  return {};
};
"#;
    let server = build_and_index(dir.path(), &[("scripts/my-plugin.ts", plugin_ts)]);

    let out = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 100 }))
        .expect("qartez_unused should succeed");

    assert!(
        !out.contains("scripts/my-plugin.ts"),
        "plugin entry-point file under scripts/ must be filtered out of \
         unused exports: got {out}"
    );
    assert!(
        !out.contains(" Plugin "),
        "the plugin-entry symbol itself must not appear in the output: got {out}"
    );
}

#[test]
fn qartez_unused_skips_plugins_dir_entry_points() {
    let dir = TempDir::new().unwrap();
    let plugin_ts = r#"export const Register = () => ({ name: "demo" });
"#;
    let server = build_and_index(dir.path(), &[("plugins/demo.ts", plugin_ts)]);

    let out = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 100 }))
        .expect("qartez_unused should succeed");

    assert!(
        !out.contains("plugins/demo.ts"),
        "plugin entry-point file under plugins/ must be filtered out: got {out}"
    );
}

#[test]
fn qartez_unused_skips_extensions_dir_entry_points() {
    let dir = TempDir::new().unwrap();
    let ext_ts = r#"export const Activate = () => ({});
"#;
    let server = build_and_index(dir.path(), &[("extensions/hello.ts", ext_ts)]);

    let out = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 100 }))
        .expect("qartez_unused should succeed");

    assert!(
        !out.contains("extensions/hello.ts"),
        "plugin entry-point file under extensions/ must be filtered out: got {out}"
    );
}

#[test]
fn qartez_unused_skips_dash_plugin_basename_anywhere() {
    // Basename branch: file lives outside the well-known directories but
    // its name still matches `*-plugin.*`, so the filter should fire.
    let dir = TempDir::new().unwrap();
    let plugin_ts = r#"export const Init = async () => ({});
"#;
    let server = build_and_index(dir.path(), &[("src/runtime-plugin.ts", plugin_ts)]);

    let out = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 100 }))
        .expect("qartez_unused should succeed");

    assert!(
        !out.contains("src/runtime-plugin.ts"),
        "file matching the *-plugin.* basename pattern must be filtered: got {out}"
    );
}

#[test]
fn qartez_unused_skips_sveltekit_layout_module_exports() {
    // SvelteKit reads `ssr`, `prerender`, `trailingSlash`, etc. by name
    // from `+layout.ts`. Without the framework-convention filter, the
    // tool would mark every one of those as a dead export.
    let dir = TempDir::new().unwrap();
    let layout_ts = r#"export const ssr = false;
export const prerender = true;
export const trailingSlash = "always";
"#;
    let server = build_and_index(dir.path(), &[("web/src/routes/+layout.ts", layout_ts)]);

    let out = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 100 }))
        .expect("qartez_unused should succeed");

    assert!(
        !out.contains("+layout.ts"),
        "SvelteKit `+layout.ts` exports must not be flagged as dead: got {out}"
    );
}

#[test]
fn qartez_unused_skips_sveltekit_page_server_actions() {
    // `+page.server.ts` exports `load` and `actions` for SvelteKit form
    // handling. These are resolved by name from the route table.
    let dir = TempDir::new().unwrap();
    let page_server_ts = r#"export const load = async () => ({ user: null });
export const actions = { default: async () => ({}) };
"#;
    let server = build_and_index(
        dir.path(),
        &[("web/src/routes/login/+page.server.ts", page_server_ts)],
    );

    let out = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 100 }))
        .expect("qartez_unused should succeed");

    assert!(
        !out.contains("+page.server.ts"),
        "SvelteKit `+page.server.ts` exports must not be flagged as dead: got {out}"
    );
}

#[test]
fn qartez_unused_skips_sveltekit_server_endpoint_handlers() {
    // `+server.ts` exports `GET` / `POST` / `PUT` / etc. as HTTP handlers.
    // SvelteKit's router dispatches by method name.
    let dir = TempDir::new().unwrap();
    let server_ts = r#"export const GET = async () => new Response("ok");
export const POST = async () => new Response("created", { status: 201 });
"#;
    let server = build_and_index(
        dir.path(),
        &[("web/src/routes/api/health/+server.ts", server_ts)],
    );

    let out = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 100 }))
        .expect("qartez_unused should succeed");

    assert!(
        !out.contains("+server.ts"),
        "SvelteKit `+server.ts` HTTP handler exports must not be flagged as dead: got {out}"
    );
}

#[test]
fn qartez_unused_skips_sveltekit_hooks_server() {
    // `hooks.server.ts` exports `handle`, `handleError`, `handleFetch`
    // that SvelteKit invokes by name from the request lifecycle.
    let dir = TempDir::new().unwrap();
    let hooks_ts = r#"export const handle = async ({ event, resolve }) => resolve(event);
export const handleError = ({ error }) => ({ message: String(error) });
"#;
    let server = build_and_index(dir.path(), &[("web/src/hooks.server.ts", hooks_ts)]);

    let out = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 100 }))
        .expect("qartez_unused should succeed");

    assert!(
        !out.contains("hooks.server.ts"),
        "SvelteKit `hooks.server.ts` exports must not be flagged as dead: got {out}"
    );
}

// ---------------------------------------------------------------------------
// Fixture B: truly-dead exports are STILL flagged
// ---------------------------------------------------------------------------

#[test]
fn qartez_unused_still_reports_real_dead_export() {
    // Rust TS-like example would be awkward here; use Rust so the
    // indexer's `is_exported` and unused-export materialisation paths
    // are exercised end-to-end with the same language-agnostic filter.
    let dir = TempDir::new().unwrap();
    let dead_rs = r#"pub fn dead_fn() -> u32 { 42 }
"#;
    let cargo_toml = r#"[package]
name = "fixture_b"
version = "0.0.0"
edition = "2024"

[lib]
path = "src/dead.rs"
"#;
    let server = build_and_index(
        dir.path(),
        &[("src/dead.rs", dead_rs), ("Cargo.toml", cargo_toml)],
    );

    let out = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 100 }))
        .expect("qartez_unused should succeed");

    assert!(
        out.contains("src/dead.rs"),
        "truly-dead export in a non-plugin path must still be flagged: got {out}"
    );
    assert!(
        out.contains("dead_fn"),
        "dead_fn must still appear in the unused-exports list: got {out}"
    );
}

#[test]
fn qartez_unused_mixed_project_keeps_dead_drops_plugin() {
    // Belt-and-suspenders: in a project that contains BOTH a plugin
    // entry-point AND a real dead export, the tool should surface only
    // the dead one. Guards against regressions where a too-broad filter
    // swallows the whole page.
    let dir = TempDir::new().unwrap();
    let plugin_ts = r#"export const Plugin = async () => ({});
"#;
    let dead_rs = r#"pub fn orphan_helper() -> &'static str { "nobody calls me" }
"#;
    let cargo_toml = r#"[package]
name = "fixture_mixed"
version = "0.0.0"
edition = "2024"

[lib]
path = "src/orphan.rs"
"#;
    let server = build_and_index(
        dir.path(),
        &[
            ("scripts/opencode-plugin.ts", plugin_ts),
            ("src/orphan.rs", dead_rs),
            ("Cargo.toml", cargo_toml),
        ],
    );

    let out = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 100 }))
        .expect("qartez_unused should succeed");

    assert!(
        !out.contains("scripts/opencode-plugin.ts"),
        "plugin entry-point must be filtered even alongside a real dead export: got {out}"
    );
    assert!(
        out.contains("orphan_helper"),
        "real dead export must survive the filter: got {out}"
    );
}
