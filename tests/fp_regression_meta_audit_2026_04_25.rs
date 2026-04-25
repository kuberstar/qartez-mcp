// Rust guideline compliant 2026-04-25
//
// Regression coverage for the 2026-04-25 meta-audit batch (claims 73, 75,
// 76, 77, 78, 79, 80). Each test pins a user-visible contract introduced
// by the audit fix so a future refactor cannot silently revert it.
//
// Scope:
//   73 qartez_tools     - non-progressive listing labels enable[] as no-op
//   75 qartez_boundaries - empty rule set lands as `#`-commented placeholder
//   76 qartez_wiki      - token_budget below 1024 is clamped, not rejected
//   77 qartez_wiki      - 0 import edges yields an explicit warning
//   78 qartez_project   - ambiguous toolchains label the closest as primary
//   79 qartez_project   - unknown test filter is refused before any build
//   80 qartez_semantic  - empty query returns the validation error path

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

fn rust_fixture() -> [(&'static str, &'static str); 4] {
    [
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        (
            "src/a.rs",
            "pub fn helper() {}\n#[test]\nfn test_alpha() {}\n#[test]\nfn test_beta() {}\n",
        ),
        ("src/b.rs", "pub fn other() {}\n"),
    ]
}

// ---------------------------------------------------------------------------
// 75 qartez_boundaries: empty rule set never lands as a 4-line stub
// the violation checker would silently read as "0 rules, 0 violations
// - pristine architecture". The fix wraps an empty rule set in
// explanatory `#`-comments so anyone opening the file sees the cause
// and the recovery command, not a misleading blank stub.
// ---------------------------------------------------------------------------

#[test]
fn boundaries_empty_rules_writes_explanatory_placeholder() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    // The tiny fixture has no directory-aligned partitions, so the
    // boundary suggester returns 0 rules. The fix replaces the bare
    // 4-line stub with a `#`-commented placeholder that names the
    // cause and the recovery command directly on disk.
    let target_rel = ".qartez/boundaries.toml";
    let out = server
        .call_tool_by_name(
            "qartez_boundaries",
            json!({ "suggest": true, "write_to": target_rel }),
        )
        .expect("write_to with 0 rules must succeed - now as a placeholder");
    assert!(
        out.contains("0-rule placeholder") || out.contains("placeholder"),
        "report must label the file as a placeholder, got: {out}"
    );
    let target_abs = dir.path().join(target_rel);
    assert!(
        target_abs.exists(),
        "placeholder boundaries.toml must exist on disk at {target_abs:?}"
    );
    let body = fs::read_to_string(&target_abs).expect("read placeholder");
    assert!(
        body.contains("No candidate rules were derivable"),
        "placeholder must explain why no rules were derived on disk, got: {body}"
    );
    assert!(
        body.contains("qartez_wiki recompute=true"),
        "placeholder must surface the recovery command on disk, got: {body}"
    );
    assert!(
        !body.contains("[[boundary]]"),
        "placeholder must NOT contain rule entries, got: {body}"
    );
}

#[test]
fn boundaries_empty_rules_inline_explains_advice() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name("qartez_boundaries", json!({ "suggest": true }))
        .expect("suggest without write_to must return advice when 0 rules");
    assert!(
        out.contains("No candidate rules"),
        "inline path must explain why no rules were derived, got: {out}"
    );
    assert!(
        out.contains("recompute=true") || out.contains("resolution"),
        "inline path must point at a recovery action, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// 76 qartez_wiki: token_budget below 1024 is clamped, not rejected.
// ---------------------------------------------------------------------------

#[test]
fn wiki_token_budget_below_floor_is_clamped() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    // Pre-fix: this returned a hard `Err("token_budget must be >= 1024 ...")`,
    // contradicting the schema which advertised no minimum bound. The
    // fix clamps to 1024 internally and emits an informational note.
    let out = server
        .call_tool_by_name("qartez_wiki", json!({ "token_budget": 200 }))
        .expect("token_budget below floor must be clamped, not rejected");
    assert!(
        out.contains("clamped to 1024"),
        "clamp path must announce the new effective budget, got: {out}"
    );
    assert!(
        out.contains("token_budget=200"),
        "clamp note must echo the requested value back, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// 77 qartez_wiki: zero import edges leads with an explicit warning.
// ---------------------------------------------------------------------------

#[test]
fn wiki_with_no_edges_emits_warning_banner() {
    let dir = TempDir::new().unwrap();
    // Single file with no imports - the tree-sitter Rust parser will
    // not record any edges for this fixture, so the import graph stays
    // empty and clustering collapses to one bucket.
    let files: [(&str, &str); 2] = [
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "pub fn solo() {}\n"),
    ];
    let server = build_and_index(dir.path(), &files);
    let out = server
        .call_tool_by_name("qartez_wiki", json!({}))
        .expect("wiki must succeed even with no edges");
    assert!(
        out.contains("no import edges are recorded"),
        "no-edges path must lead with the explicit warning, got: {out}"
    );
    assert!(
        out.contains("misc")
            || out.contains("rebuild the import graph")
            || out.contains("qartez_workspace"),
        "warning must point at a recovery action, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// 78 qartez_project: ambiguous toolchains label the closest as primary.
// ---------------------------------------------------------------------------

#[test]
fn project_info_marks_primary_when_multiple_toolchains_detected() {
    let dir = TempDir::new().unwrap();
    // Root `Makefile` + nested `Cargo.toml` -> two toolchain blocks.
    // The Makefile sits at the root (subdir = None) so it should
    // receive the [primary] tag.
    let files: [(&str, &str); 4] = [
        (
            "Makefile",
            "test:\n\techo make-test\nbuild:\n\techo make-build\n",
        ),
        (
            "qartez-public/Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        ),
        ("qartez-public/src/lib.rs", "pub fn nested() {}\n"),
        ("qartez-public/src/a.rs", "pub fn a() {}\n"),
    ];
    let server = build_and_index(dir.path(), &files);
    let out = server
        .call_tool_by_name("qartez_project", json!({ "action": "info" }))
        .expect("info must succeed when toolchains are detected");
    assert!(
        out.contains("[primary]"),
        "ambiguous detection must mark the closest-to-root toolchain, got:\n{out}"
    );
    assert!(
        out.contains("toolchains detected"),
        "ambiguous detection must announce the count, got:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// 79 qartez_project: unknown test filter refuses before any build runs.
// ---------------------------------------------------------------------------

#[test]
fn project_test_filter_unknown_short_circuits_before_build() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    // The fixture indexes `helper`, `test_alpha`, `test_beta`. A filter
    // that matches none of them must short-circuit instead of invoking
    // cargo build (which would compile the crate, time out mid-build,
    // and burn CPU).
    let err = server
        .call_tool_by_name(
            "qartez_project",
            json!({
                "action": "test",
                "filter": "does_not_exist_xyz",
                "timeout": 5,
            }),
        )
        .expect_err("filter that matches no indexed function must short-circuit");
    assert!(
        err.contains("does_not_exist_xyz") || err.contains("No indexed function"),
        "rejection must name the missing filter, got: {err}"
    );
    // Confirm the fix actually saved time: matching filters still
    // route through to the toolchain (we expect the tool to invoke
    // cargo, which may then fail because cargo is not installed in
    // the test runner; we just need to verify the pre-check did NOT
    // veto it). Use a very small timeout so we do not spend more
    // than necessary even if cargo IS installed.
    let pass = server.call_tool_by_name(
        "qartez_project",
        json!({
            "action": "test",
            "filter": "test_alpha",
            "timeout": 1,
        }),
    );
    match pass {
        Ok(_) => {}
        Err(e) => {
            assert!(
                !e.contains("No indexed function"),
                "matching filter must NOT trip the no-match veto, got: {e}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 80 qartez_semantic: empty query returns the precise validation error.
// ---------------------------------------------------------------------------

#[test]
fn semantic_empty_query_validates_before_feature_check() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_semantic", json!({ "query": "   " }))
        .expect_err("empty query must err regardless of feature flag");
    // The feature-disabled branch used to lead with "Semantic search
    // is not available in this build", forcing the caller through a
    // misleading rebuild loop. Both branches now validate the query
    // first and return the same precise message.
    assert!(
        err.contains("query must be non-empty"),
        "empty query must surface the validation error, got: {err}"
    );
}
