// Regression coverage for the 2026-04-25 refactor/mutation tools audit sweep.
// Each test pins a user-visible safety or UX contract tightened in this batch
// so a future refactor cannot silently revert them.
//
// The harness mirrors the existing `tests/fp_regression_*.rs` files: drop
// files to a `TempDir`, run `full_index`, then call the MCP dispatcher via
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

// ---------------------------------------------------------------------------
// Item 95: rename_file refuses to rename build-system manifests.
// Cargo.toml, package.json, go.mod, pyproject.toml, etc. are discovered by
// exact basename by their respective toolchains. A rename detaches the
// package from its resolver and the build fails with a cryptic error rather
// than the precise refusal callers already get for `mod.rs` / `lib.rs`.
// ---------------------------------------------------------------------------

#[test]
fn rename_file_refuses_cargo_toml_source() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub fn f() {}\n"),
        ],
    );
    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({"from": "Cargo.toml", "to": "Cargo.renamed.toml"}),
        )
        .expect_err("Cargo.toml rename must be refused");
    assert!(
        err.contains("Cargo.toml"),
        "refusal must name the manifest, got: {err}"
    );
    assert!(
        err.contains("manifest"),
        "refusal must explain that the file is a manifest, got: {err}"
    );
}

#[test]
fn rename_file_refuses_cargo_toml_target() {
    // Renaming TO a manifest basename clobbers the toolchain's discovery
    // path even when the source is an ordinary file. Symmetric guard.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub fn f() {}\n"),
            ("docs/extra.toml", "key = 1\n"),
        ],
    );
    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({"from": "docs/extra.toml", "to": "docs/Cargo.toml"}),
        )
        .expect_err("rename targeting a manifest basename must be refused");
    assert!(
        err.contains("Cargo.toml") && err.contains("manifest"),
        "refusal must name the manifest target, got: {err}"
    );
}

#[test]
fn rename_file_refuses_package_json_and_go_mod() {
    // Spot-check non-Rust manifests so the protection list is not silently
    // narrowed back to Cargo only.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            ("package.json", "{\"name\": \"x\"}\n"),
            ("go.mod", "module x\n"),
            ("src/lib.rs", "pub fn f() {}\n"),
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
        ],
    );
    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({"from": "package.json", "to": "package.renamed.json"}),
        )
        .expect_err("package.json rename must be refused");
    assert!(err.contains("package.json"), "got: {err}");

    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({"from": "go.mod", "to": "go.renamed.mod"}),
        )
        .expect_err("go.mod rename must be refused");
    assert!(err.contains("go.mod"), "got: {err}");
}

#[test]
fn rename_file_allows_non_manifest_files_with_manifest_like_basenames() {
    // The protection is basename-exact, not substring. A file named
    // `Cargo.toml.bak` or `my_package.json` is not a manifest and must
    // remain renameable.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub fn f() {}\n"),
            ("scripts/Cargo.toml.bak", "old contents\n"),
        ],
    );
    let result = server.call_tool_by_name(
        "qartez_rename_file",
        json!({"from": "scripts/Cargo.toml.bak", "to": "scripts/Cargo.toml.old"}),
    );
    // The rename may or may not succeed depending on indexing of the .bak
    // file, but it MUST NOT be refused on the manifest-protection grounds.
    if let Err(e) = result {
        assert!(
            !e.contains("manifest"),
            "Cargo.toml.bak must not trip the manifest guard, got: {e}"
        );
    }
}

// ---------------------------------------------------------------------------
// Item 112: rename info-disclosure cap on available_files listing.
// A `file_path` typo on a heavily-imported symbol previously dumped every
// indexed file that defines the symbol (potentially hundreds of paths from
// other roots). Cap to 20 entries and append an overflow count.
// ---------------------------------------------------------------------------

#[test]
fn rename_caps_available_files_listing_when_filter_excludes_all() {
    // Index 30 files that all define a symbol named `helper`. A bogus
    // file_path then forces the disambiguator-miss path. The error message
    // must list at most 20 paths and announce how many were elided.
    let dir = TempDir::new().unwrap();
    let mut files: Vec<(String, String)> = vec![
        (
            "Cargo.toml".to_string(),
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n".to_string(),
        ),
        (
            "src/lib.rs".to_string(),
            (0..30)
                .map(|i| format!("pub mod m{i};"))
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        ),
    ];
    for i in 0..30 {
        files.push((format!("src/m{i}.rs"), "pub fn helper() {}\n".to_string()));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let server = build_and_index(dir.path(), &refs);
    let err = server
        .call_tool_by_name(
            "qartez_rename",
            json!({
                "old_name": "helper",
                "new_name": "helper2",
                "file_path": "src/does_not_exist.rs",
            }),
        )
        .expect_err("nonexistent file_path must err");
    // The error names the bogus filter and surfaces the overflow count.
    assert!(
        err.contains("file_path='src/does_not_exist.rs'"),
        "error must echo the bogus filter, got: {err}"
    );
    assert!(
        err.contains("more)"),
        "error must include the overflow count footer, got: {err}"
    );
    // No more than 20 file paths from the available_files set are emitted.
    let listed = (0..30)
        .filter(|i| err.contains(&format!("src/m{i}.rs")))
        .count();
    assert!(
        listed <= 20,
        "available_files listing must be capped at 20, got {listed} paths",
    );
}

#[test]
fn rename_keeps_full_listing_when_below_cap() {
    // With only a few defining files, the full set must still be shown so
    // the cap does not regress small workspaces.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod a;\npub mod b;\npub mod c;\n"),
            ("src/a.rs", "pub fn helper() {}\n"),
            ("src/b.rs", "pub fn helper() {}\n"),
            ("src/c.rs", "pub fn helper() {}\n"),
        ],
    );
    let err = server
        .call_tool_by_name(
            "qartez_rename",
            json!({
                "old_name": "helper",
                "new_name": "helper2",
                "file_path": "src/missing.rs",
            }),
        )
        .expect_err("nonexistent file_path must err");
    assert!(
        err.contains("src/a.rs") && err.contains("src/b.rs") && err.contains("src/c.rs"),
        "small available_files set must be shown in full, got: {err}"
    );
    assert!(
        !err.contains("more)"),
        "no overflow footer when listing fits under the cap, got: {err}"
    );
}
