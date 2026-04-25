// Rust guideline compliant 2026-04-25
//
// Regression coverage for the 2026-04-25 audit batch focused on
// user-visible error messages and analysis hints. Each test pins
// the new contract so a future regression to the older wording
// re-trips a CI signal.
//
//   M27 qartez_diff_impact - friendly hint for `not found` revspec
//   M28 qartez_test_gaps   - same friendly hint in `mode=suggest`
//   M29 qartez_impact      - on-disk-but-not-indexed reindex hint
//   M30 qartez_find        - FTS5-special chars route to `regex=true`
//   M31 qartez_outline     - tool-specific mermaid rejection
//   M32 qartez_trend       - no-CC message is not gaslighting
//   M33 qartez_test_gaps   - `min_pagerank` filter is named in empty result
//   M39 qartez_context     - non-code paths excluded from task_match too

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

fn seed_simple_repo(dir: &Path) -> QartezServer {
    let repo = git2::Repository::init(dir).unwrap();
    let path = dir.join("src/a.rs");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "pub fn a() {}\n").unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_path(Path::new("src/a.rs")).unwrap();
    idx.write().unwrap();
    let tree_oid = idx.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("audit", "audit@example.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
    let conn = setup_db();
    index::full_index(&conn, dir, false).unwrap();
    QartezServer::new(conn, dir.to_path_buf(), 5)
}

// ---------------------------------------------------------------------------
// M27 qartez_diff_impact: friendly hint for invalid revspec.
// ---------------------------------------------------------------------------

#[test]
fn diff_impact_invalid_revspec_routes_to_friendly_hint() {
    let dir = TempDir::new().unwrap();
    let server = seed_simple_repo(dir.path());
    let err = server
        .call_tool_by_name("qartez_diff_impact", json!({ "base": "invalidref" }))
        .expect_err("bogus revspec must error");
    assert!(
        err.contains("Cannot resolve revspec") || err.contains("not found"),
        "expected friendly wrapper, got: {err}"
    );
    // The trailing `(git: ...)` block preserves the libgit2 detail
    // for operators who grep for raw codes; the leak is wrapped, not
    // dropped on the floor.
    assert!(
        err.contains("(git:"),
        "raw libgit2 detail must be preserved as suffix, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// M28 qartez_test_gaps: same friendly hint in `mode=suggest`.
// ---------------------------------------------------------------------------

#[test]
fn test_gaps_suggest_invalid_base_routes_to_friendly_hint() {
    let dir = TempDir::new().unwrap();
    let server = seed_simple_repo(dir.path());
    let err = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({ "mode": "suggest", "base": "notarealbranch_xyz" }),
        )
        .expect_err("bogus base must error");
    assert!(
        err.contains("Cannot resolve revspec") || err.contains("not found"),
        "test_gaps suggest must use the same wrapper as diff_impact, got: {err}"
    );
    // Old contract leaked the raw `Git error: ...` envelope.
    assert!(
        !err.starts_with("Git error: revspec"),
        "raw libgit2 envelope must be replaced, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// M29 qartez_impact: on-disk-but-not-indexed reindex hint.
// ---------------------------------------------------------------------------

#[test]
fn impact_on_disk_but_not_indexed_tells_user_to_reindex() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "pub fn a() {}\n")]);
    fs::write(dir.path().join("src/b.rs"), "pub fn b() {}\n").unwrap();
    let err = server
        .call_tool_by_name("qartez_impact", json!({ "file_path": "src/b.rs" }))
        .expect_err("unindexed path must error");
    assert!(
        err.contains("not found in index") && err.contains("reindex"),
        "qartez_impact must give the same reindex hint as qartez_cochange, got: {err}"
    );
}

#[test]
fn impact_missing_on_disk_keeps_short_form() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "pub fn a() {}\n")]);
    let err = server
        .call_tool_by_name("qartez_impact", json!({ "file_path": "src/ghost.rs" }))
        .expect_err("missing-on-disk path must error");
    assert!(err.contains("not found in index"), "got: {err}");
    assert!(
        !err.contains("reindex"),
        "files absent from disk must not advertise the reindex remedy, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// M30 qartez_find: FTS5-special chars route to `regex=true`.
// ---------------------------------------------------------------------------

#[test]
fn find_fts_special_chars_recommend_regex_not_prefix() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "pub fn a() {}\n")]);
    let result = server
        .call_tool_by_name("qartez_find", json!({ "name": "Config!@#$" }))
        .expect("non-empty name should not error");
    // The old hint said `qartez_grep query=Config!@#$*` which would
    // explode on FTS5 syntax; the new hint must point at `regex=true`.
    assert!(
        !result.contains("query=Config!@#$"),
        "must not suggest a query that breaks FTS5, got: {result}"
    );
    assert!(
        result.contains("regex=true"),
        "must point callers at regex=true, got: {result}"
    );
}

#[test]
fn find_plain_name_keeps_prefix_hint() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "pub fn a() {}\n")]);
    let result = server
        .call_tool_by_name("qartez_find", json!({ "name": "Parser" }))
        .expect("non-empty name should not error");
    // Plain alphanumerics still get the existing prefix-search recovery.
    assert!(
        result.contains("query=Parser*"),
        "plain identifier names keep the prefix-search hint, got: {result}"
    );
}

// ---------------------------------------------------------------------------
// M31 qartez_outline: tool-specific mermaid rejection.
// ---------------------------------------------------------------------------

#[test]
fn outline_mermaid_rejection_explains_per_file_shape_not_graph() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "pub fn a() {}\n")]);
    let err = server
        .call_tool_by_name(
            "qartez_outline",
            json!({ "file_path": "src/a.rs", "format": "mermaid" }),
        )
        .expect_err("mermaid must be rejected");
    // Outline is a per-file table; the rejection must lead with the
    // shape mismatch instead of just redirecting to graph tools as if
    // they could give a per-file outline. Graph tools may still be
    // mentioned, but only as alternatives for callers who actually
    // wanted a Mermaid graph view (deps / calls / hierarchy) rather
    // than a symbol table.
    assert!(
        err.contains("symbol table") || err.contains("not a graph"),
        "outline rejection must explain the per-file shape mismatch, got: {err}"
    );
    assert!(
        err.contains("concise") || err.contains("default"),
        "outline rejection must point at the working symbol-table formats, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// M32 qartez_trend: no-CC message is not gaslighting.
// ---------------------------------------------------------------------------

#[test]
fn trend_no_complexity_message_does_not_gaslight() {
    let dir = TempDir::new().unwrap();
    // A short Rust file with only declarations / re-exports has no
    // measurable per-symbol CC. The old wording said the file "may
    // have been non-code"; the new wording must name the actual
    // condition.
    let repo = git2::Repository::init(dir.path()).unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    let lib = "pub mod a;\npub mod b;\npub use a::*;\n";
    fs::write(dir.path().join("src/lib.rs"), lib).unwrap();
    fs::write(dir.path().join("src/a.rs"), "pub const X: u32 = 1;\n").unwrap();
    fs::write(dir.path().join("src/b.rs"), "pub const Y: u32 = 2;\n").unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_path(Path::new("src/lib.rs")).unwrap();
    idx.add_path(Path::new("src/a.rs")).unwrap();
    idx.add_path(Path::new("src/b.rs")).unwrap();
    idx.write().unwrap();
    let tree_oid = idx.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("audit", "audit@example.com").unwrap();
    let c1 = repo
        .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
    // Second commit so trend has at least 2 points to consider.
    fs::write(
        dir.path().join("src/lib.rs"),
        "pub mod a;\npub mod b;\npub use a::*;\npub use b::*;\n",
    )
    .unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_path(Path::new("src/lib.rs")).unwrap();
    idx.write().unwrap();
    let tree_oid = idx.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let parent = repo.find_commit(c1).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&parent])
        .unwrap();

    let conn = setup_db();
    index::full_index(&conn, dir.path(), false).unwrap();
    // The trend "no per-symbol complexity" branch only fires when
    // `change_count >= 2`. `index::full_index` does not back-fill the
    // git change_count column (cochange analysis is a separate pass),
    // so for a focused test of the wording we set it directly.
    conn.execute(
        "UPDATE files SET change_count = 2 WHERE path = 'src/lib.rs'",
        [],
    )
    .unwrap();
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 5);

    let result = server
        .call_tool_by_name("qartez_trend", json!({ "file_path": "src/lib.rs" }))
        .expect("declaration-only file should not panic the trend tool");
    assert!(
        !result.contains("non-code"),
        "must not gaslight: file is real Rust code, got: {result}"
    );
    assert!(
        result.contains("declaration-only")
            || result.contains("no function or method")
            || result.contains("re-exports"),
        "must name the actual condition the analyzer measured, got: {result}"
    );
}

// ---------------------------------------------------------------------------
// M33 qartez_test_gaps: `min_pagerank` filter is named in empty result.
// ---------------------------------------------------------------------------

#[test]
fn test_gaps_min_pagerank_filtered_does_not_claim_all_covered() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            ("src/lib.rs", "pub fn entry() {}\n"),
            ("src/util.rs", "pub fn helper() {}\n"),
        ],
    );
    let result = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({ "mode": "gaps", "min_pagerank": 9999.0 }),
        )
        .expect("min_pagerank too high must produce a filtered message, not error");
    assert!(
        !result.contains("All source files are covered"),
        "must not claim coverage when filter dropped every candidate, got: {result}"
    );
    assert!(
        result.contains("min_pagerank") || result.contains("rank below the filter"),
        "must mention the filter that emptied the result set, got: {result}"
    );
}

// ---------------------------------------------------------------------------
// M39 qartez_context: non-code paths excluded from task_match too.
// ---------------------------------------------------------------------------

#[test]
fn context_task_match_skips_non_code_files() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "src/parser.rs",
                "pub fn parse_rust_source() {}\npub fn analyze_rust_source() {}\n",
            ),
            ("src/lib.rs", "pub mod parser;\npub fn rust_entry() {}\n"),
            // Non-code seeds: TS plugin and CSS file mentioning `parse`.
            (
                "opencode-plugin.ts",
                "export function parseConfig() {}\nexport function analyzeStyle() {}\n",
            ),
            ("style.css", ".parser-rust-source { color: red; }\n"),
        ],
    );
    let result = server
        .call_tool_by_name(
            "qartez_context",
            json!({
                "files": ["src/parser.rs"],
                "task": "parse and analyze the rust source code",
            }),
        )
        .expect("context with task should not error");
    // Non-code files must NOT bubble into the context list via the
    // task FTS scoring loop. Asserting absence rather than the exact
    // shape of the rendered table so the test stays robust to format
    // changes.
    assert!(
        !result.contains("opencode-plugin.ts"),
        "non-code TS plugin must not be credited via task_match, got: {result}"
    );
    assert!(
        !result.contains("style.css"),
        "CSS file must not be credited via task_match, got: {result}"
    );
}
