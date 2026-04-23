// Rust guideline compliant 2026-04-22
//
// End-to-end regressions for the five structural-tool bugs filed in the
// 2026-04-22 follow-up triage. Each test indexes qartez-public itself,
// asserts the specific bug is gone, and (where relevant) that the paired
// true-positive path still works.
//
// Report scope:
//   E1 qartez_boundaries  - auto_cluster fallback + write_to validation
//   E2 qartez_hierarchy   - max_depth=0 returns only the seed
//   E3 qartez_wiki        - resolution actually changes cluster count
//   E4 qartez_wiki/bounds - absolute path consistency
//   E5 qartez_wiki        - inline output respects token cap

use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use qartez_mcp::index;
use qartez_mcp::server::QartezServer;
use qartez_mcp::storage::schema;

fn qartez_public_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn build_server() -> QartezServer {
    let root = qartez_public_root();
    assert!(
        root.join("src/lib.rs").exists(),
        "qartez-public/src/lib.rs missing at {root:?}"
    );
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    index::full_index(&conn, &root, false).unwrap();
    QartezServer::new(conn, root, 0)
}

// --------------------------------------------------------------------------
// E1: qartez_boundaries
//
// Reported: `suggest=true` failed with "No cluster assignment found" unless
// qartez_wiki had run first; `write_to` was silently ignored when
// `suggest=false`.
//
// Fix: new `auto_cluster` parameter (default true) runs the clustering on
// demand; passing `write_to` without `suggest=true` is a validation error.
// --------------------------------------------------------------------------

#[test]
fn selftest_boundaries_suggest_auto_clusters_without_prior_wiki_run() {
    let server = build_server();
    let out = server
        .call_tool_by_name("qartez_boundaries", json!({ "suggest": true }))
        .expect("suggest=true must succeed via auto_cluster fallback");
    assert!(
        !out.contains("No cluster assignment found"),
        "auto_cluster should have filled the clustering table; got: {out}"
    );
}

#[test]
fn selftest_boundaries_suggest_auto_cluster_off_fails_cleanly() {
    let server = build_server();
    let err = server
        .call_tool_by_name(
            "qartez_boundaries",
            json!({ "suggest": true, "auto_cluster": false }),
        )
        .expect_err("auto_cluster=false with empty clusters must fail");
    assert!(
        err.contains("auto_cluster=true") || err.contains("qartez_wiki"),
        "error must mention the remediation path; got: {err}"
    );
}

#[test]
fn selftest_boundaries_write_to_without_suggest_is_rejected() {
    let server = build_server();
    let err = server
        .call_tool_by_name(
            "qartez_boundaries",
            json!({ "write_to": "tmp/out.toml", "suggest": false }),
        )
        .expect_err("write_to without suggest must fail with a validation error");
    assert!(
        err.contains("`write_to` is only valid when `suggest=true`"),
        "error must mention the suggest requirement; got: {err}"
    );
}

#[test]
fn selftest_boundaries_suggest_write_to_absolute_path_is_honored() {
    let server = build_server();
    let tmp = TempDir::new().unwrap();
    let abs = tmp.path().join("boundaries.toml");
    let abs_str = abs.to_string_lossy().into_owned();
    let out = server
        .call_tool_by_name(
            "qartez_boundaries",
            json!({ "suggest": true, "write_to": abs_str.clone() }),
        )
        .expect("absolute write_to must be accepted");
    assert!(out.starts_with("Wrote"), "expected 'Wrote ...' got: {out}");
    assert!(
        abs.exists(),
        "boundaries file must exist at {abs:?} after the call"
    );
}

// --------------------------------------------------------------------------
// E2: qartez_hierarchy
//
// Reported: `max_depth=0` was ignored and the tool returned the full list of
// direct sub/supertypes.
//
// Fix: `max_depth=0` short-circuits to a "seed only" response regardless of
// `transitive`, for both the text and mermaid output paths.
// --------------------------------------------------------------------------

#[test]
fn selftest_hierarchy_max_depth_zero_returns_only_seed() {
    let server = build_server();
    let out = server
        .call_tool_by_name(
            "qartez_hierarchy",
            json!({ "symbol": "LanguageSupport", "max_depth": 0 }),
        )
        .expect("max_depth=0 must succeed");

    assert!(
        out.contains("Seed symbol only") || out.contains("max_depth=0"),
        "max_depth=0 must surface the seed-only marker; got: {out}"
    );
    assert!(
        !out.contains("Types implementing/extending"),
        "max_depth=0 must not enumerate children; got: {out}"
    );
}

#[test]
fn selftest_hierarchy_positive_depth_still_lists_children() {
    let server = build_server();
    let zero = server
        .call_tool_by_name(
            "qartez_hierarchy",
            json!({ "symbol": "LanguageSupport", "max_depth": 0 }),
        )
        .unwrap();
    let positive = server
        .call_tool_by_name(
            "qartez_hierarchy",
            json!({ "symbol": "LanguageSupport", "max_depth": 5 }),
        )
        .unwrap();
    assert_ne!(
        zero, positive,
        "max_depth=0 and max_depth>0 must produce different output"
    );
}

// --------------------------------------------------------------------------
// E3: qartez_wiki
//
// Reported: `resolution` had no observable effect; low and high values
// returned practically the same cluster count because the cached clustering
// was reused regardless of the new resolution.
//
// Fix: passing an explicit `resolution` forces a cluster recompute.
// --------------------------------------------------------------------------

fn count_clusters_in_wiki(markdown: &str) -> usize {
    markdown
        .lines()
        .filter(|l| l.starts_with("## ") && !l.starts_with("## Table of contents"))
        .count()
}

#[test]
fn selftest_wiki_resolution_changes_cluster_count() {
    let server = build_server();
    // Use write_to so the output isn't truncated by the token cap before
    // we can count sections. The tmp dir is dropped at the end of the
    // test, so there's no pollution of the project tree.
    let tmp = TempDir::new().unwrap();
    let low_path = tmp.path().join("low.md");
    let high_path = tmp.path().join("high.md");

    let low_str = low_path.to_string_lossy().into_owned();
    let high_str = high_path.to_string_lossy().into_owned();

    let _ = server
        .call_tool_by_name(
            "qartez_wiki",
            json!({
                "resolution": 0.1,
                "min_cluster_size": 2,
                "write_to": low_str,
            }),
        )
        .expect("low-resolution wiki must render");
    let low_md = fs::read_to_string(&low_path).unwrap();
    let low_count = count_clusters_in_wiki(&low_md);

    let _ = server
        .call_tool_by_name(
            "qartez_wiki",
            json!({
                "resolution": 5.0,
                "min_cluster_size": 2,
                "write_to": high_str,
            }),
        )
        .expect("high-resolution wiki must render");
    let high_md = fs::read_to_string(&high_path).unwrap();
    let high_count = count_clusters_in_wiki(&high_md);

    assert!(
        high_count > low_count,
        "higher resolution must yield strictly more clusters (low={low_count}, high={high_count})"
    );
}

// --------------------------------------------------------------------------
// E4: qartez_wiki
//
// Reported: `write_to` rejected absolute paths, even though boundaries
// accepts various paths.
//
// Fix: both tools now share a "project-relative or absolute + existing
// parent" policy.
// --------------------------------------------------------------------------

#[test]
fn selftest_wiki_write_to_absolute_path_is_honored() {
    let server = build_server();
    let tmp = TempDir::new().unwrap();
    let abs = tmp.path().join("wiki.md");
    let abs_str = abs.to_string_lossy().into_owned();
    let out = server
        .call_tool_by_name(
            "qartez_wiki",
            json!({ "write_to": abs_str.clone(), "min_cluster_size": 3 }),
        )
        .expect("absolute write_to must be accepted");
    assert!(out.starts_with("Wrote"), "expected 'Wrote ...' got: {out}");
    assert!(abs.exists(), "wiki file must exist at {abs:?}");
}

#[test]
fn selftest_wiki_write_to_absolute_missing_parent_is_rejected() {
    let server = build_server();
    let err = server
        .call_tool_by_name(
            "qartez_wiki",
            json!({ "write_to": "/__nonexistent_parent_9823742/wiki.md" }),
        )
        .expect_err("missing parent directory must fail");
    assert!(
        err.contains("does not exist"),
        "error must explain the missing parent dir; got: {err}"
    );
}

// --------------------------------------------------------------------------
// E5: qartez_wiki
//
// Reported: inline output with `min_cluster_size=1` dumped a 102-cluster
// wiki into the response with no cap.
//
// Fix: inline responses are capped at `token_budget` (default 8000) with a
// footer pointing at `write_to=<path>` for the full wiki.
// --------------------------------------------------------------------------

#[test]
fn selftest_wiki_inline_output_respects_token_cap() {
    let server = build_server();
    let out = server
        .call_tool_by_name(
            "qartez_wiki",
            json!({
                "min_cluster_size": 1,
                "resolution": 2.0,
                "token_budget": 1000,
            }),
        )
        .expect("inline wiki must render");

    // Approximate Claude tokens at 3 chars/token (see
    // `helpers::estimate_tokens`). The cap is a soft budget; allow a
    // small overshoot from the truncation footer.
    let approx_tokens = out.chars().count() / 3;
    assert!(
        approx_tokens <= 1200,
        "inline output must respect token_budget=1000; got ~{approx_tokens} tokens"
    );
    assert!(
        out.contains("write_to=<path>") || out.contains("Set token_budget="),
        "truncation footer must point at write_to and/or suggest a token_budget; got: {out}"
    );
}

#[test]
fn selftest_wiki_inline_below_cap_has_no_truncation_footer() {
    let server = build_server();
    let out = server
        .call_tool_by_name(
            "qartez_wiki",
            json!({ "token_budget": 200000, "min_cluster_size": 3 }),
        )
        .expect("inline wiki with huge budget must render in full");
    assert!(
        !out.contains("write_to=<path> to write the full wiki"),
        "below-cap output must not carry the truncation footer"
    );
    assert!(
        !out.contains("Set token_budget="),
        "below-cap output must not carry the token_budget suggestion"
    );
}
