// Edge-case verification coverage for the cluster-d validation and UX
// batch pinned by `fp_regression_validation_uiux.rs`. The primary file
// pins the user-visible contract; this one probes the boundary cases
// that the bug fixes implicitly have to handle.
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
// read.rs edge cases
// ---------------------------------------------------------------------------

#[test]
fn read_max_bytes_one_is_accepted() {
    // `max_bytes=0` rejects, but any positive value - including 1 -
    // must flow through to the renderer. The response will be
    // heavily truncated, but the tool must not short-circuit.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name(
            "qartez_read",
            json!({"symbol_name": "helper", "max_bytes": 1}),
        )
        .expect("max_bytes=1 must be accepted (only 0 is rejected)");
    assert!(!out.is_empty(), "renderer must emit at least one byte");
}

#[test]
fn read_start_line_zero_defaults_not_rejected() {
    // `start_line=0` is now explicitly rejected. The API is 1-based so
    // the zero value carries no useful meaning and the lenient "treat
    // 0 as unset" behaviour used to mask off-by-one bugs in callers.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_read",
            json!({"file_path": "src/a.rs", "start_line": 0, "end_line": 2}),
        )
        .expect_err("start_line=0 is now rejected with a 1-based hint");
    assert!(
        err.contains("1-based") && err.contains("start_line"),
        "rejection must explain the 1-based contract, got: {err}"
    );
}

#[test]
fn read_symbols_empty_list_alone_errors() {
    // `symbols=[]` without `symbol_name` is not a batch-miss (there
    // is nothing to miss); it is an unspecified query and must err
    // via the shared `parse_symbol_queries` path.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_read", json!({"symbols": []}))
        .expect_err("empty symbols list with no symbol_name must err");
    assert!(
        err.contains("symbol_name") || err.contains("required"),
        "error must describe the missing query, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// context.rs edge cases
// ---------------------------------------------------------------------------

#[test]
fn context_whitespace_only_task_is_rejected_like_empty() {
    // A `task` of only whitespace is functionally empty; the fix
    // must hit the same "task-only seed" error branch instead of
    // passing through to an FTS run that cannot possibly match.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_context", json!({"task": "   "}))
        .expect_err("whitespace-only task must err");
    assert!(
        err.contains("file") || err.contains("task"),
        "error must point at the seeding options, got: {err}"
    );
}

#[test]
fn context_task_with_only_short_words_yields_zero_seed_error() {
    // Task words of three characters or fewer are filtered out
    // before the FTS query runs. A task made of only short tokens
    // therefore seeds zero files and must emit the dedicated
    // "seeded 0 files" error rather than falling through.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_context", json!({"task": "a b c de"}))
        .expect_err("task with only short words must err");
    assert!(
        err.contains("seeded 0") || err.contains("files"),
        "error must report that seeding failed, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// security.rs edge cases
// ---------------------------------------------------------------------------

#[test]
fn security_mixed_case_severity_accepted() {
    // Mixed case is a common copy/paste artefact (e.g. `Critical`
    // from a docs table). Case-insensitive parsing has to cover
    // any capitalisation, not only the two extreme shout-cases.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name("qartez_security", json!({"severity": "Critical"}))
        .expect("mixed-case severity must be accepted");
    assert!(
        !out.to_lowercase().contains("unknown severity"),
        "mixed case must not trigger unknown-severity, got: {out}"
    );
}

#[test]
fn security_unknown_severity_still_rejected() {
    // The fix widens the accepted set but the rejection path must
    // still fire for truly bogus values so typos do not silently
    // scan at the default Low floor.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_security", json!({"severity": "urgent"}))
        .expect_err("bogus severity must err");
    assert!(
        err.to_lowercase().contains("unknown severity"),
        "error must mention unknown severity, got: {err}"
    );
}

#[test]
fn security_config_path_whitespace_treated_as_unset() {
    // A whitespace-only `config_path` is morally equivalent to the
    // unset case; the trimmer strips it and the tool must fall
    // back to the builtin rule set without raising the "does not
    // exist" error that real missing paths trigger.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name("qartez_security", json!({"config_path": "   "}))
        .expect("whitespace-only config_path must act like unset");
    assert!(
        !out.contains("does not exist"),
        "whitespace config_path must not raise the missing-file error, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// workspace.rs edge cases
// ---------------------------------------------------------------------------

#[test]
fn workspace_rejects_path_that_is_a_file() {
    // `canonicalize` succeeds on regular files, so the code path
    // specifically has to reject non-directory targets. Regression
    // guard against the old "happily register a file" behaviour.
    let main = TempDir::new().unwrap();
    fs::create_dir_all(main.path().join(".git")).unwrap();
    fs::create_dir_all(main.path().join(".qartez")).unwrap();
    let file_root = TempDir::new().unwrap();
    let file_path = file_root.path().join("plain.rs");
    fs::write(&file_path, "fn x() {}\n").unwrap();

    let conn = setup_db();
    index::full_index(&conn, main.path(), false).unwrap();
    let server = QartezServer::new(conn, main.path().to_path_buf(), 0);

    let err = server
        .call_tool_by_name(
            "qartez_workspace",
            json!({
                "action": "add",
                "alias": "demo",
                "path": file_path.to_str().unwrap(),
            }),
        )
        .expect_err("a regular file must not register as a workspace root");
    assert!(
        err.contains("not a directory"),
        "error must explain why the path was refused, got: {err}"
    );
}

#[test]
fn workspace_add_same_alias_same_path_is_idempotent() {
    // The fix rejects alias reuse across different paths. The same
    // alias at the same path must still succeed so re-running a
    // setup script stays an idempotent no-op.
    let main = TempDir::new().unwrap();
    fs::create_dir_all(main.path().join(".git")).unwrap();
    fs::create_dir_all(main.path().join(".qartez")).unwrap();
    let side = TempDir::new().unwrap();
    fs::write(side.path().join("x.rs"), "fn x() {}\n").unwrap();

    let conn = setup_db();
    index::full_index(&conn, main.path(), false).unwrap();
    let server = QartezServer::new(conn, main.path().to_path_buf(), 0);

    let path_str = side.path().to_str().unwrap();
    server
        .call_tool_by_name(
            "qartez_workspace",
            json!({"action": "add", "alias": "demo", "path": path_str}),
        )
        .expect("first add succeeds");
    let second = server
        .call_tool_by_name(
            "qartez_workspace",
            json!({"action": "add", "alias": "demo", "path": path_str}),
        )
        .expect("same alias + same path must stay Ok");
    assert!(
        second.to_lowercase().contains("no-op") || second.to_lowercase().contains("already"),
        "repeat add must surface an idempotent marker, got: {second}"
    );
}

#[test]
fn workspace_alias_rejects_path_separators() {
    // The alias becomes a DB path prefix under
    // `delete_files_by_prefix`, so a slash in the alias would
    // shred the purge semantics. Keep the reject working.
    let main = TempDir::new().unwrap();
    fs::create_dir_all(main.path().join(".git")).unwrap();
    fs::create_dir_all(main.path().join(".qartez")).unwrap();
    let side = TempDir::new().unwrap();
    fs::write(side.path().join("x.rs"), "fn x() {}\n").unwrap();

    let conn = setup_db();
    index::full_index(&conn, main.path(), false).unwrap();
    let server = QartezServer::new(conn, main.path().to_path_buf(), 0);

    let err = server
        .call_tool_by_name(
            "qartez_workspace",
            json!({
                "action": "add",
                "alias": "bad/alias",
                "path": side.path().to_str().unwrap(),
            }),
        )
        .expect_err("alias with '/' must be rejected");
    assert!(
        err.contains("Invalid alias") || err.contains("ASCII"),
        "error must describe the alias charset, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// tools_meta.rs edge cases
// ---------------------------------------------------------------------------

#[test]
fn tools_meta_unknown_enable_target_errors() {
    // `qartez_tools` is async-only, so sync dispatch always returns
    // Err (either the async-only marker or the real validation
    // error). The positive assertion is therefore "never Ok": an
    // Ok would mean the bogus target was silently accepted.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let result = server.call_tool_by_name("qartez_tools", json!({"enable": ["bogus_tier"]}));
    assert!(
        result.is_err(),
        "unknown tier must not produce Ok, got: {result:?}"
    );
}

#[test]
fn tools_meta_mixed_known_and_unknown_enable_errors() {
    // Same async-only constraint applies: the unknown half of the
    // list must never reach the apply path under any dispatch
    // mode. Ok here would indicate the validator regressed.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let result = server.call_tool_by_name(
        "qartez_tools",
        json!({"enable": ["analysis", "bogus_tier"]}),
    );
    assert!(
        result.is_err(),
        "mixed known + unknown must not produce Ok, got: {result:?}"
    );
}

#[test]
fn tools_meta_disable_all_rejected_targets_errors() {
    // Disabling only structurally protected targets (`core`,
    // `qartez_tools`) must not succeed; sync dispatch already
    // returns the async-only err, but an Ok would indicate the
    // guard silently allowed the no-op path.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let result = server.call_tool_by_name("qartez_tools", json!({"disable": ["core"]}));
    assert!(
        result.is_err(),
        "disable=[core] must not produce Ok, got: {result:?}"
    );
}

// ---------------------------------------------------------------------------
// boundaries.rs edge cases
// ---------------------------------------------------------------------------

#[test]
fn boundaries_write_to_without_suggest_errors() {
    // Passing `write_to` without `suggest=true` used to overwrite
    // the existing config with the checker's output; the fix
    // rejects that combination.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name("qartez_boundaries", json!({"write_to": ".qartez/b.toml"}))
        .expect_err("write_to without suggest=true must err");
    assert!(
        err.contains("write_to") && err.contains("suggest"),
        "error must name both flags, got: {err}"
    );
}

#[test]
fn boundaries_empty_write_to_with_suggest_renders_inline() {
    // Trimmed-empty `write_to` is "unset"; the tool must render
    // the starter TOML inline instead of writing to disk or
    // rejecting the call.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name(
            "qartez_boundaries",
            json!({"suggest": true, "write_to": "  "}),
        )
        .expect("suggest=true with whitespace write_to must succeed");
    assert!(
        !out.is_empty(),
        "suggest mode must still render output when write_to is whitespace"
    );
}

// ---------------------------------------------------------------------------
// project.rs edge cases
// ---------------------------------------------------------------------------

#[test]
fn project_run_defaults_to_build_not_test() {
    // action=run without a filter must default to build, not test.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name("qartez_project", json!({"action": "run"}))
        .expect("action=run must succeed with a cargo project");
    // The dry-run banner embeds the subcommand label; confirm it is "build".
    assert!(
        out.contains(" build "),
        "action=run default must be build, got: {out}"
    );
    assert!(
        !out.contains(" test ") || out.contains(" build "),
        "default must not pick test over build, got: {out}"
    );
}

#[test]
fn project_run_unknown_subcommand_errors() {
    // The router hard-lists four subcommands; anything else must
    // reject so callers do not silently receive a no-op response.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let err = server
        .call_tool_by_name(
            "qartez_project",
            json!({"action": "run", "filter": "install"}),
        )
        .expect_err("unknown subcommand must err");
    assert!(
        err.to_lowercase().contains("unknown"),
        "error must list the supported subcommands, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// find.rs edge cases
// ---------------------------------------------------------------------------

#[test]
fn find_regex_miss_has_no_grep_hint() {
    // The fallback hint is only useful for exact-name lookups.
    // In regex mode, the caller is already pattern-matching, so
    // the hint is both irrelevant and actively confusing.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &rust_fixture());
    let out = server
        .call_tool_by_name(
            "qartez_find",
            json!({"name": "^DoesNotExist$", "regex": true}),
        )
        .expect("regex miss must be Ok");
    assert!(
        !out.contains("qartez_grep"),
        "regex miss must not offer the prefix hint, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// refactor_plan.rs edge cases
// ---------------------------------------------------------------------------

#[test]
fn refactor_plan_limit_above_cap_clamps_distinctly() {
    // `limit=0` is the no-cap sentinel and `limit>50` is the clamp;
    // the two cases must produce distinguishable notices so the
    // caller can tell which path they hit.
    let dir = TempDir::new().unwrap();
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
    let clamped = server
        .call_tool_by_name(
            "qartez_refactor_plan",
            json!({"file_path": "src/lib.rs", "limit": 999}),
        )
        .expect("limit=999 must succeed and clamp");
    assert!(
        clamped.contains("was clamped") || clamped.contains("Refactor Plan"),
        "clamped limit must emit the clamp notice, got: {clamped}"
    );
    assert!(
        !clamped.contains("limit=0 requested no cap"),
        "the clamp path must not borrow the no-cap wording, got: {clamped}"
    );
}

// ---------------------------------------------------------------------------
// workspace primary-root wipe guard
// ---------------------------------------------------------------------------

#[test]
fn workspace_add_rejects_alias_colliding_with_primary_subdir() {
    // Scenario that wiped the entire index before the guard:
    // primary root has a subdirectory 'qartez-public', so indexed paths
    // start with 'qartez-public/...'. Registering an alias named
    // 'qartez-public' (even pointing somewhere unrelated) would put
    // primary-owned files on the kill list of `remove_workspace`
    // because delete_files_by_prefix deletes by path prefix alone.
    let main = TempDir::new().unwrap();
    fs::create_dir_all(main.path().join(".git")).unwrap();
    fs::create_dir_all(main.path().join(".qartez")).unwrap();
    fs::create_dir_all(main.path().join("qartez-public/src")).unwrap();
    fs::write(
        main.path().join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.0.1\"\nedition=\"2021\"\n",
    )
    .unwrap();
    fs::write(
        main.path().join("qartez-public/src/lib.rs"),
        "pub fn x() {}\n",
    )
    .unwrap();

    let other = TempDir::new().unwrap();
    fs::write(other.path().join("y.rs"), "pub fn y() {}\n").unwrap();

    let conn = setup_db();
    index::full_index(&conn, main.path(), false).unwrap();
    let server = QartezServer::new(conn, main.path().to_path_buf(), 0);

    let err = server
        .call_tool_by_name(
            "qartez_workspace",
            json!({
                "action": "add",
                "alias": "qartez-public",
                "path": other.path().to_str().unwrap(),
            }),
        )
        .expect_err("alias colliding with an existing path prefix must be rejected");
    assert!(
        err.contains("Refusing to add") && err.contains("qartez-public"),
        "error must name the refused alias, got: {err}"
    );
    assert!(
        err.to_lowercase().contains("prefix") || err.to_lowercase().contains("collid"),
        "error must reference the prefix collision, got: {err}"
    );
}

#[test]
fn workspace_remove_rejects_path_inside_primary_root() {
    // Belt-and-suspenders coverage: simulate a legacy workspace.toml
    // loaded at startup whose alias points at a subdirectory of the
    // primary root. The add-time guard rejects new registrations like
    // this, but older TOML files can still carry the mapping. A
    // remove call would trigger `delete_files_by_prefix` and purge
    // primary-owned files. The guard refuses up front with guidance.
    let main = TempDir::new().unwrap();
    fs::create_dir_all(main.path().join(".git")).unwrap();
    fs::create_dir_all(main.path().join(".qartez")).unwrap();
    fs::create_dir_all(main.path().join("sub/src")).unwrap();
    fs::write(
        main.path().join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.0.1\"\nedition=\"2021\"\n",
    )
    .unwrap();
    fs::write(main.path().join("sub/src/lib.rs"), "pub fn x() {}\n").unwrap();

    let conn = setup_db();
    index::full_index(&conn, main.path(), false).unwrap();

    // Seed root_aliases with an entry that maps the primary subdir
    // `sub` to the alias name. This is what a stale workspace.toml
    // would produce on startup.
    let canonical_primary = main.path().canonicalize().unwrap();
    let sub_path = canonical_primary.join("sub");
    let mut aliases = std::collections::HashMap::new();
    aliases.insert(sub_path.clone(), "legacy-sub".to_string());
    let roots = vec![canonical_primary.clone(), sub_path];

    let server = QartezServer::with_roots(conn, canonical_primary, roots, aliases, 0);

    let err = server
        .call_tool_by_name(
            "qartez_workspace",
            json!({ "action": "remove", "alias": "legacy-sub" }),
        )
        .expect_err("remove of an inside-primary alias must be refused");
    assert!(
        err.to_lowercase().contains("refusing to remove"),
        "error must explain the refusal, got: {err}"
    );
    assert!(
        err.contains("primary project root"),
        "error must name the primary root guard, got: {err}"
    );
}

#[test]
fn workspace_add_unique_alias_still_succeeds_after_guard() {
    // The collision guard must stay narrow: a non-colliding alias into
    // an external directory continues to work the same as before.
    let main = TempDir::new().unwrap();
    fs::create_dir_all(main.path().join(".git")).unwrap();
    fs::create_dir_all(main.path().join(".qartez")).unwrap();
    fs::write(
        main.path().join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.0.1\"\nedition=\"2021\"\n",
    )
    .unwrap();
    fs::create_dir_all(main.path().join("src")).unwrap();
    fs::write(main.path().join("src/lib.rs"), "pub fn x() {}\n").unwrap();

    let extra = TempDir::new().unwrap();
    fs::write(extra.path().join("y.rs"), "pub fn y() {}\n").unwrap();

    let conn = setup_db();
    index::full_index(&conn, main.path(), false).unwrap();
    let server = QartezServer::new(conn, main.path().to_path_buf(), 0);

    server
        .call_tool_by_name(
            "qartez_workspace",
            json!({
                "action": "add",
                "alias": "demo-ext",
                "path": extra.path().to_str().unwrap(),
            }),
        )
        .expect("unique alias pointing outside primary must still register");
}
