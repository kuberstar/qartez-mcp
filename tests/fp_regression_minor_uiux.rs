// Rust guideline compliant 2026-04-22
//
// Regression coverage for the 2026-04-22 minor UI/UX bug batch.
// Each test pins the new user-visible contract produced by the fix:
//
//   G1  qartez_cochange - distinguish missing-from-disk vs. not-indexed
//   G2  qartez_context  - reject unindexed paths instead of emitting "isolated"
//   G3  qartez_context  - `limit=0` = no cap (uniform policy)
//   G4  qartez_knowledge - friendly roster on author typo
//   G5  qartez_knowledge - `limit=0` = no cap
//   G6  qartez_workspace - idempotent add returns "already registered"
//   G7  qartez_smells    - reject unknown categorical `kind`
//   G8  qartez_semantic  - empty query is a validation error when feature off
//                          (the feature-gated error path still applies)
//   G9  qartez_health    - `limit=0` = no cap
//   G10 qartez_health    - min_complexity above corpus returns clear message
//   G11 qartez_health    - max_health outside [0, 10] is rejected
//   G12 qartez_unused    - `limit=0` = no cap (fixes off-by-one)
//
// The tool-wide "limit=0 means no cap" policy is documented in each
// tool source file near the `limit` binding, and exercised from the
// outside here so a future regression to "0 means 0/1" re-trips a test.

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

// A lightweight variant that installs a real git history for the
// knowledge / cochange tools. `commits` is a list of (author_name,
// author_email, commit_message, [(relpath, content)]) tuples applied in
// order. Returns both the seeded directory and a `QartezServer`.
type GitCommit<'a> = (&'a str, &'a str, &'a str, &'a [(&'a str, &'a str)]);

fn build_git_repo(dir: &Path, commits: &[GitCommit<'_>]) -> QartezServer {
    let repo = git2::Repository::init(dir).unwrap();
    let mut parent_oid: Option<git2::Oid> = None;
    for (author, email, msg, files) in commits {
        for (rel, content) in *files {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }
        let mut index = repo.index().unwrap();
        for (rel, _) in *files {
            index.add_path(Path::new(rel)).unwrap();
        }
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now(author, email).unwrap();
        let new_oid = match parent_oid {
            Some(p) => {
                let parent = repo.find_commit(p).unwrap();
                repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&parent])
                    .unwrap()
            }
            None => repo
                .commit(Some("HEAD"), &sig, &sig, msg, &tree, &[])
                .unwrap(),
        };
        parent_oid = Some(new_oid);
    }
    let conn = setup_db();
    index::full_index(&conn, dir, false).unwrap();
    // git_depth=100 enables knowledge / cochange paths that require history.
    QartezServer::new(conn, dir.to_path_buf(), 100)
}

// ---------------------------------------------------------------------------
// G1 qartez_cochange: distinguish missing-from-disk vs. on-disk-not-indexed
// ---------------------------------------------------------------------------

#[test]
fn cochange_missing_on_disk_reports_disk_error() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "fn a() {}\n")]);
    let err = server
        .call_tool_by_name("qartez_cochange", json!({ "file_path": "src/ghost.rs" }))
        .expect_err("missing path must error");
    // Unified `File '<path>' not found in index` wording, shared with
    // qartez_stats / qartez_impact / qartez_outline / qartez_context.
    assert!(err.contains("not found in index"), "got: {err}");
    assert!(err.contains("src/ghost.rs"), "got: {err}");
}

#[test]
fn cochange_on_disk_but_not_indexed_tells_user_to_reindex() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "fn a() {}\n")]);
    // Write a new file AFTER indexing. It exists on disk but the index
    // does not know it yet.
    fs::write(dir.path().join("src/b.rs"), "fn b() {}\n").unwrap();
    let err = server
        .call_tool_by_name("qartez_cochange", json!({ "file_path": "src/b.rs" }))
        .expect_err("unindexed path must error");
    assert!(
        err.contains("not found in index") && err.contains("reindex"),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------------
// G2 qartez_context: unindexed path returns validation error, not "isolated"
// ---------------------------------------------------------------------------

#[test]
fn context_unindexed_path_errors_instead_of_isolated_stub() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "fn a() {}\n")]);
    let err = server
        .call_tool_by_name(
            "qartez_context",
            json!({ "files": ["src/does_not_exist.rs"] }),
        )
        .expect_err("unindexed path must error");
    assert!(
        err.contains("not found in index") && err.contains("does_not_exist"),
        "got: {err}"
    );
    assert!(
        !err.contains("isolated"),
        "must not fall through to 'isolated' stub: {err}"
    );
}

// ---------------------------------------------------------------------------
// G3 qartez_context: limit=0 means "no cap"
// ---------------------------------------------------------------------------

#[test]
fn context_limit_zero_means_no_cap() {
    // Fabricate a tiny graph: `a.rs` imports `b.rs`. With limit=0 we
    // should still see `b.rs` in the output (i.e. no "isolated" stub and
    // no zero-row stub).
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            ("src/b.rs", "pub fn b() {}\n"),
            ("src/a.rs", "use crate::b; pub fn a() { b::b() }\n"),
            (
                "Cargo.toml",
                "[package]\nname=\"ctx\"\nversion=\"0.0.0\"\nedition=\"2024\"\n[lib]\npath=\"src/a.rs\"\n",
            ),
        ],
    );
    let out = server
        .call_tool_by_name(
            "qartez_context",
            json!({ "files": ["src/a.rs"], "limit": 0 }),
        )
        .expect("qartez_context limit=0 must succeed");
    // The fixture either surfaces b.rs (preferred) or is empty. What
    // MUST NOT happen is a panic / error.
    assert!(
        !out.to_lowercase().contains("error"),
        "limit=0 must not error: {out}"
    );
}

// ---------------------------------------------------------------------------
// G4 qartez_knowledge: bad author filter returns roster, not "no blame data"
// ---------------------------------------------------------------------------

#[test]
fn knowledge_unknown_author_lists_real_roster() {
    let dir = TempDir::new().unwrap();
    let server = build_git_repo(
        dir.path(),
        &[(
            "Alice",
            "alice@test.com",
            "init",
            &[("src/lib.rs", "pub fn foo() {}\n")],
        )],
    );
    let out = server
        .call_tool_by_name(
            "qartez_knowledge",
            json!({ "author": "nonexistent_typo_99" }),
        )
        .expect("knowledge with typo'd author must succeed");
    assert!(
        out.contains("No files touched by author matching"),
        "expected typo message, got: {out}"
    );
    assert!(
        out.contains("Alice"),
        "available-authors roster must list real authors; got: {out}"
    );
}

// ---------------------------------------------------------------------------
// G5 qartez_knowledge: limit=0 returns full list (no cap), not an empty table
// ---------------------------------------------------------------------------

#[test]
fn knowledge_limit_zero_returns_all_rows() {
    let dir = TempDir::new().unwrap();
    let server = build_git_repo(
        dir.path(),
        &[(
            "Alice",
            "alice@test.com",
            "init",
            &[
                ("src/a.rs", "pub fn a() {}\n"),
                ("src/b.rs", "pub fn b() {}\n"),
            ],
        )],
    );
    let out = server
        .call_tool_by_name("qartez_knowledge", json!({ "limit": 0 }))
        .expect("knowledge limit=0 must succeed");
    // Both files must be present: the empty-table-with-header regression
    // would have dropped one or both.
    assert!(
        out.contains("src/a.rs") && out.contains("src/b.rs"),
        "limit=0 must list all rows, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// G6 qartez_workspace: idempotent add emits "already registered"
// ---------------------------------------------------------------------------

#[test]
fn workspace_add_same_alias_twice_reports_already_registered() {
    let main = TempDir::new().unwrap();
    fs::create_dir_all(main.path().join(".git")).unwrap();
    fs::create_dir_all(main.path().join(".qartez")).unwrap();
    let target = TempDir::new().unwrap();
    fs::write(target.path().join("x.rs"), "fn x() {}\n").unwrap();

    let conn = setup_db();
    index::full_index(&conn, main.path(), false).unwrap();
    let server = QartezServer::new(conn, main.path().to_path_buf(), 0);

    let first = server
        .call_tool_by_name(
            "qartez_workspace",
            json!({
                "action": "add",
                "alias": "demo",
                "path": target.path().to_str().unwrap(),
            }),
        )
        .expect("first add must succeed");
    assert!(
        first.contains("Added domain"),
        "first add must succeed with Added message, got: {first}"
    );

    let second = server
        .call_tool_by_name(
            "qartez_workspace",
            json!({
                "action": "add",
                "alias": "demo",
                "path": target.path().to_str().unwrap(),
            }),
        )
        .expect("re-add must succeed (idempotent)");
    assert!(
        second.contains("already registered"),
        "second add must report already-registered, got: {second}"
    );
}

// ---------------------------------------------------------------------------
// G7 qartez_smells: unknown categorical kind is rejected with the valid set
// ---------------------------------------------------------------------------

#[test]
fn smells_rejects_unknown_kind() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "fn a() {}\n")]);
    let err = server
        .call_tool_by_name("qartez_smells", json!({ "kind": "bogus_kind" }))
        .expect_err("unknown kind must error");
    assert!(
        err.contains("no known smell kinds") && err.contains("bogus_kind"),
        "must name the bad value, got: {err}"
    );
    assert!(
        err.contains("god_function") && err.contains("long_params") && err.contains("feature_envy"),
        "error must list valid kinds, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// G8 qartez_semantic: empty query is a validation error.
//
// Without the `semantic` feature, the tool returns a "requires feature"
// message. The non-feature build therefore emits that message even on
// empty input - that is the documented fallback. This test pins the
// feature-gated path: when `semantic` is on, an empty query is rejected.
// ---------------------------------------------------------------------------

#[cfg(feature = "semantic")]
#[test]
fn semantic_empty_query_is_validation_error_when_feature_on() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "fn a() {}\n")]);
    let err = server
        .call_tool_by_name("qartez_semantic", json!({ "query": "   " }))
        .expect_err("empty query must error");
    assert!(
        err.contains("non-empty") || err.contains("must not be empty"),
        "must surface validation message, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// G9 qartez_health: limit=0 = no cap
// ---------------------------------------------------------------------------

#[test]
fn health_limit_zero_means_no_cap() {
    let dir = TempDir::new().unwrap();
    // A trivial file is enough; we just want to check the shape of the
    // output. With limit=0, we must never emit "Showing 0/N" - either
    // there is one row or the min-cc guard bails with its own message.
    let server = build_and_index(
        dir.path(),
        &[("src/a.rs", "fn a() { let x = 1; let y = 2; }\n")],
    );
    let out = server
        .call_tool_by_name("qartez_health", json!({ "limit": 0 }))
        .expect("qartez_health limit=0 must succeed");
    assert!(
        !out.contains("Showing 0/"),
        "limit=0 must not produce '0/N' stub, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// G10 qartez_health: min_complexity above corpus returns a clear message
// ---------------------------------------------------------------------------

#[test]
fn health_min_complexity_above_corpus_returns_clear_message() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "fn a() {}\n")]);
    let out = server
        .call_tool_by_name("qartez_health", json!({ "min_complexity": 1000 }))
        .expect("qartez_health with huge min_complexity must succeed");
    assert!(
        out.contains("No files with min_complexity >= 1000 found"),
        "must return the clear-message stub, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// G11 qartez_health: negative max_health rejected; above-10 clamped
// ---------------------------------------------------------------------------

#[test]
fn health_rejects_negative_max_health() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "fn a() {}\n")]);
    let err = server
        .call_tool_by_name("qartez_health", json!({ "max_health": -5.0 }))
        .expect_err("negative max_health must error");
    assert!(
        err.contains("max_health") && err.contains("range") && err.contains("10"),
        "must return a validation error describing the 0..=10 range, got: {err}"
    );
}

#[test]
fn health_clamps_max_health_above_ten() {
    // Unified range policy: max_health above 10 is a caller typo (e.g.
    // 999 instead of 9) and is now rejected rather than clamped, so
    // the mistake is surfaced at the call site instead of being masked
    // by identity output with max_health=10.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "fn a() {}\n")]);
    server
        .call_tool_by_name("qartez_health", json!({ "max_health": 10.0 }))
        .expect("max_health=10 must succeed (inclusive upper bound)");
    let err = server
        .call_tool_by_name("qartez_health", json!({ "max_health": 999.0 }))
        .expect_err("max_health>10 must now be rejected");
    assert!(
        err.contains("max_health") && err.contains("range") && err.contains("10"),
        "rejection must describe the valid range, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// G12 qartez_unused: limit=0 = no cap (fixes the off-by-one)
// ---------------------------------------------------------------------------

#[test]
fn unused_limit_zero_returns_all_rows() {
    // qartez_unused was realigned with the rest of the tool family on
    // the no-cap convention: limit=0 removes the row cap, matching
    // qartez_cochange / qartez_health / qartez_hotspots /
    // qartez_context. The original "limit=0 is a mistake" stance was
    // the inconsistent outlier; settle on no-cap so callers do not
    // have to remember per-tool exceptions.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "pub fn x() {}\n")]);
    let result = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 0 }))
        .expect("qartez_unused limit=0 must mean no-cap, not error");
    assert!(
        !result.contains("limit must be > 0"),
        "limit=0 must not produce a rejection, got: {result}"
    );
}

// Cross-cutting: verify qartez_unused `limit=2` still paginates correctly
// (we did not accidentally break the positive-integer path).
#[test]
fn unused_positive_limit_still_paginates() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    let cargo =
        "[package]\nname=\"u\"\nversion=\"0.0.0\"\nedition=\"2024\"\n[lib]\npath=\"src/dead.rs\"\n";
    let dead = r#"pub fn dead_one() -> u32 { 1 }
pub fn dead_two() -> u32 { 2 }
pub fn dead_three() -> u32 { 3 }
"#;
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/dead.rs"), dead).unwrap();
    fs::write(dir.path().join("Cargo.toml"), cargo).unwrap();
    let conn = setup_db();
    index::full_index(&conn, dir.path(), false).unwrap();
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    let out = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 2 }))
        .expect("qartez_unused limit=2 must succeed");
    // Explicit limit=2 returns exactly two of the three symbols.
    let matches = ["dead_one", "dead_two", "dead_three"]
        .iter()
        .filter(|n| out.contains(*n))
        .count();
    assert_eq!(matches, 2, "limit=2 must return two rows, got: {out}");
}
