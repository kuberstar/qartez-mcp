// Regression coverage for the 2026-04-23 tool-semantic bug batch.
// Each test pins the user-visible contract produced by the fix:
//
//   I6   qartez_map all_files=true honours token_budget
//   I7   qartez_project detects first-level subdir toolchains
//   I15  qartez_diff_impact suppresses origin hint when origin missing
//   I19  qartez_security find_cfg_test_blocks catches standalone #[cfg(test)] fn

use std::fs;
use std::path::Path;

use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use qartez_mcp::index;
use qartez_mcp::server::QartezServer;
use qartez_mcp::storage::schema;
use qartez_mcp::toolchain;

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

// -- I6 ----------------------------------------------------------------
// `qartez_map all_files=true token_budget=...` must stop growing the
// output once the budget is exhausted. The old code exempted the
// `all_files=true` branch from the token-budget loop, so a tight cap
// still produced thousands of characters.
#[test]
fn qartez_map_all_files_honours_token_budget() {
    let dir = TempDir::new().unwrap();
    // Seed 60 files so the unbounded listing overflows a tight budget.
    let mut files: Vec<(String, String)> = Vec::new();
    for i in 0..60 {
        files.push((format!("src/f{i:03}.rs"), "pub fn hello() {}\n".into()));
    }
    let borrow: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let server = build_and_index(dir.path(), &borrow);

    let out = server
        .call_tool_by_name(
            "qartez_map",
            json!({ "all_files": true, "token_budget": 300, "format": "concise" }),
        )
        .unwrap();
    // Rough upper bound: 4 chars per token * 300 tokens = ~1200 chars.
    // Allow a margin for the footer.
    assert!(
        out.len() < 2400,
        "all_files token_budget=300 produced {} chars (too many rows, budget ignored)",
        out.len(),
    );
    assert!(
        out.contains("truncated") || out.lines().count() < 60,
        "expected truncation marker or fewer than 60 rows; got:\n{out}"
    );
}

// -- I7 ----------------------------------------------------------------
// Monorepo layouts with only a top-level Makefile and a per-crate
// `Cargo.toml` under `qartez-public/` previously reported `(not
// configured)` for both test and build. The subdir detector must
// surface these.
#[test]
fn qartez_project_detects_subdir_cargo_toml() {
    let dir = TempDir::new().unwrap();
    let crate_dir = dir.path().join("qartez-public");
    fs::create_dir_all(&crate_dir).unwrap();
    fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"t\"\n").unwrap();

    let subdir_tcs = toolchain::detect_subdir_toolchains(dir.path(), 8);
    let rust = subdir_tcs
        .iter()
        .find(|t| t.name == "rust" && t.subdir.as_deref() == Some("qartez-public"));
    assert!(
        rust.is_some(),
        "expected subdir Cargo.toml to surface as rust toolchain; got {:?}",
        subdir_tcs
            .iter()
            .map(|t| (&t.name, &t.subdir))
            .collect::<Vec<_>>(),
    );
    assert_eq!(rust.unwrap().test_cmd, vec!["cargo", "test"]);
}

// -- I15 ---------------------------------------------------------------
// `diff_impact_worktree_hint` must NOT emit the origin suggestion when
// the repo has no `origin` remote. Previously any `base=main` with no
// delta printed "Did you mean origin/main?" unconditionally, misleading
// callers chasing an upstream divergence that did not exist.
#[test]
fn diff_impact_hint_suppressed_without_origin_remote() {
    let dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    fs::write(dir.path().join("a.txt"), "hi").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.txt")).unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("t", "t@e").unwrap();
    let main_oid = repo
        .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
    repo.branch("main", &repo.find_commit(main_oid).unwrap(), false)
        .unwrap();

    assert!(
        repo.find_remote("origin").is_err(),
        "fresh repo should not have an origin remote",
    );
    // Sanity: server constructs on this repo (we cannot invoke the
    // hint helper directly because it is private; the outer
    // `qartez_diff_impact` call with `base=main` exercises it).
    let server = QartezServer::new(setup_db(), dir.path().to_path_buf(), 0);
    let out = server
        .call_tool_by_name("qartez_diff_impact", json!({ "base": "main" }))
        .unwrap();
    assert!(
        !out.contains("origin/main"),
        "no origin remote - output must not suggest origin/main; got:\n{out}"
    );
}

// -- I19 ---------------------------------------------------------------
// A production file with a lone `#[cfg(test)] fn` (no wrapping mod)
// must still be scoped like a `#[cfg(test)] mod` block so the security
// scanner can skip it on the default `include_tests=false` path. This
// exercises the filter end-to-end via `qartez_security`.
#[test]
fn security_skips_standalone_cfg_test_function_by_default() {
    let source = r#"pub fn prod_ok() -> i32 { 0 }

#[cfg(test)]
fn helper() {
    let p = "../../etc/passwd";
    let _ = p;
}
"#;
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/lib.rs", source)]);
    let out = server
        .call_tool_by_name(
            "qartez_security",
            json!({ "include_tests": false, "severity": "low" }),
        )
        .unwrap();
    // The traversal literal lives inside the #[cfg(test)] fn, so the
    // scanner must suppress the finding on the default path. Before
    // this fix, the standalone fn slipped past the mod-only filter.
    assert!(
        !out.contains("helper"),
        "standalone #[cfg(test)] fn must be suppressed on include_tests=false; got:\n{out}"
    );
}
