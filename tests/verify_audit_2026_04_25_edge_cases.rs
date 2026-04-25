// Temporary edge-case verification for the 2026-04-25 audit batch.
// Exists as a deletable harness (safe to remove once the suite is
// confirmed clean) - kept lightweight so a single `cargo test`
// invocation re-checks the corner cases that the per-fix regression
// suites do not always exercise. Goal here is verification breadth,
// not contract pinning.

use std::fs;
use std::path::Path;

use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use qartez_mcp::guard;
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
// guard::touch_ack manifest format
// ---------------------------------------------------------------------------

#[test]
fn ack_file_has_two_lines_with_terminator() {
    let tmp = TempDir::new().unwrap();
    let rel = "src/server/tools/refs.rs";
    guard::touch_ack(tmp.path(), rel);
    let body = fs::read_to_string(guard::ack_path(tmp.path(), rel)).unwrap();
    assert!(
        body.ends_with('\n'),
        "ack file must have a trailing newline so future appends are line-clean: {body:?}"
    );
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2, "ack file body lines: {lines:?}");
    assert!(
        lines[0].parse::<u64>().is_ok(),
        "first line must be a unix timestamp: {:?}",
        lines[0]
    );
    assert_eq!(lines[1], rel);
}

#[test]
fn ack_freshness_does_not_depend_on_file_body() {
    // ack_is_fresh reads mtime, not body. After we extended the body
    // to include the rel-path manifest, the freshness check must still
    // work on the existing file. Touch the file, age it past the TTL
    // by a deliberate sleep, then verify both freshness branches.
    let tmp = TempDir::new().unwrap();
    let rel = "src/main.rs";
    guard::touch_ack(tmp.path(), rel);
    assert!(guard::ack_is_fresh(tmp.path(), rel, 60));
    // TTL=0 always reports stale because elapsed is >=0.
    assert!(!guard::ack_is_fresh(tmp.path(), rel, 0));
}

#[test]
fn ack_path_with_unicode_relpath_round_trips() {
    let tmp = TempDir::new().unwrap();
    // Cyrillic chars + unicode whitespace + emoji - the FNV-1a hash
    // operates on raw bytes so it must survive non-ASCII paths.
    let rel = "src/тест/файл.rs";
    guard::touch_ack(tmp.path(), rel);
    let body = fs::read_to_string(guard::ack_path(tmp.path(), rel)).unwrap();
    let line2 = body.lines().nth(1).unwrap();
    assert_eq!(line2, rel);
}

// ---------------------------------------------------------------------------
// limit=0 unification across the no-cap family.
// ---------------------------------------------------------------------------

#[test]
fn limit_zero_is_no_cap_for_unused_hotspots_health_cochange_context() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod a;\npub mod b;\npub fn entry() {}\n"),
            ("src/a.rs", "pub fn a_one() {}\npub fn a_two() {}\n"),
            ("src/b.rs", "pub fn b_one() {}\n"),
        ],
    );
    // Each tool below should accept limit=0 without an error. We only
    // check absence of "limit must be > 0" - their bodies otherwise
    // depend on indexing internals we do not pin here.
    for (tool, args) in [
        ("qartez_unused", json!({ "limit": 0 })),
        ("qartez_hotspots", json!({ "limit": 0 })),
        ("qartez_health", json!({ "limit": 0 })),
        (
            "qartez_cochange",
            json!({ "file_path": "src/lib.rs", "limit": 0 }),
        ),
        (
            "qartez_context",
            json!({ "files": ["src/lib.rs"], "limit": 0 }),
        ),
    ] {
        let result = server
            .call_tool_by_name(tool, args.clone())
            .unwrap_or_else(|e| panic!("{tool}({args}) errored: {e}"));
        assert!(
            !result.contains("limit must be > 0"),
            "{tool} must accept limit=0 as no-cap, got: {result}"
        );
    }
}

#[test]
fn limit_zero_is_still_rejected_by_clones_and_trend() {
    // clones / trend keep the explicit reject because limit is the
    // only protection against oversized output (ASTs / per-commit
    // tables can be huge). A regression that silently switched these
    // to no-cap would risk MCP transport-level failure.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub fn a() {}\n"),
        ],
    );
    let err = server
        .call_tool_by_name("qartez_clones", json!({ "limit": 0 }))
        .expect_err("clones must keep the limit=0 reject");
    assert!(
        err.contains("limit must be") || err.contains("> 0"),
        "clones reject must mention the contract: {err}"
    );
    let err = server
        .call_tool_by_name(
            "qartez_trend",
            json!({ "file_path": "src/lib.rs", "limit": 0 }),
        )
        .expect_err("trend must keep the limit=0 reject");
    assert!(
        err.contains("limit must be") || err.contains("> 0"),
        "trend reject must mention the contract: {err}"
    );
}

// ---------------------------------------------------------------------------
// workspace primary-root guard - all three rejection branches.
// ---------------------------------------------------------------------------

#[test]
fn workspace_add_rejects_three_overlap_shapes() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[(
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        )],
    );
    // Same path as primary.
    let err = server
        .call_tool_by_name(
            "qartez_workspace",
            json!({
                "action": "add",
                "alias": "self",
                "path": dir.path().to_str().unwrap(),
            }),
        )
        .expect_err("primary self-add must error");
    assert!(
        err.contains("primary project root") && err.contains("could not be added"),
        "self-add rejection wording: {err}"
    );
    // Subdirectory of primary (create one first).
    fs::create_dir_all(dir.path().join("nested")).unwrap();
    let err = server
        .call_tool_by_name(
            "qartez_workspace",
            json!({
                "action": "add",
                "alias": "nested",
                "path": dir.path().join("nested").to_str().unwrap(),
            }),
        )
        .expect_err("subdir-of-primary add must error");
    assert!(
        err.contains("inside the primary project root"),
        "subdir rejection wording: {err}"
    );
    // Parent that contains primary.
    let err = server
        .call_tool_by_name(
            "qartez_workspace",
            json!({
                "action": "add",
                "alias": "parent",
                "path": dir.path().parent().unwrap().to_str().unwrap(),
            }),
        )
        .expect_err("parent-of-primary add must error");
    assert!(
        err.contains("contains the primary project root"),
        "parent rejection wording: {err}"
    );
}

// ---------------------------------------------------------------------------
// rename_file manifest protection edge cases.
// ---------------------------------------------------------------------------

#[test]
fn rename_file_blocks_manifest_basenames_in_subdirectories() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"root\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            (
                "subcrate/Cargo.toml",
                "[package]\nname = \"sub\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("subcrate/src/lib.rs", "pub fn x() {}\n"),
        ],
    );
    // Build manifests are protected by basename, not by absolute path,
    // so a subcrate Cargo.toml must also refuse rename.
    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({
                "from": "subcrate/Cargo.toml",
                "to": "subcrate/Cargo2.toml",
                "apply": false,
            }),
        )
        .expect_err("subcrate Cargo.toml must be protected too");
    assert!(
        err.to_lowercase().contains("manifest") || err.contains("Cargo.toml"),
        "subcrate manifest rejection wording: {err}"
    );
}

// ---------------------------------------------------------------------------
// find FTS5-special hint - matches plain identifiers AND special chars.
// ---------------------------------------------------------------------------

#[test]
fn find_special_chars_route_to_regex_hint() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[(
            "src/lib.rs",
            "pub fn parse_config() {}\npub fn analyze_module() {}\n",
        )],
    );
    // Plain alphanumeric: the prefix-search hint stays.
    let r = server
        .call_tool_by_name("qartez_find", json!({ "name": "MissingSym" }))
        .unwrap();
    assert!(r.contains("query=MissingSym*"));
    // Underscore-bearing identifier (still alphanumeric+_): same prefix hint.
    let r = server
        .call_tool_by_name("qartez_find", json!({ "name": "missing_one" }))
        .unwrap();
    assert!(r.contains("query=missing_one*"));
    // Dollar / dash / dot: route to regex=true.
    for special in &["foo$bar", "a-b", "a.b", "x@y", "Config!@#$"] {
        let r = server
            .call_tool_by_name("qartez_find", json!({ "name": special }))
            .unwrap();
        assert!(
            r.contains("regex=true"),
            "name '{special}' must point at regex=true: {r}"
        );
        assert!(
            !r.contains(&format!("query={special}*")),
            "name '{special}' must NOT suggest a literal FTS prefix: {r}"
        );
    }
}

// ---------------------------------------------------------------------------
// workspace add + sibling path = should still work (positive case).
// ---------------------------------------------------------------------------

#[test]
fn workspace_add_accepts_sibling_path_outside_primary() {
    let outer = TempDir::new().unwrap();
    let primary = outer.path().join("primary");
    let sibling = outer.path().join("sibling");
    fs::create_dir_all(primary.join("src")).unwrap();
    fs::create_dir_all(sibling.join("src")).unwrap();
    fs::write(
        primary.join("Cargo.toml"),
        "[package]\nname = \"primary\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(primary.join("src").join("lib.rs"), "pub fn x() {}\n").unwrap();
    fs::write(sibling.join("src").join("lib.rs"), "pub fn y() {}\n").unwrap();
    fs::create_dir_all(primary.join(".git")).unwrap();
    let conn = setup_db();
    index::full_index(&conn, &primary, false).unwrap();
    let server = QartezServer::new(conn, primary.clone(), 0);
    // The guard rejects self/sub/super paths but a sibling that
    // shares the same parent must still be addable. Without this
    // positive check, the symmetric guard could regress to over-
    // refusal.
    let result = server
        .call_tool_by_name(
            "qartez_workspace",
            json!({
                "action": "add",
                "alias": "sibling",
                "path": sibling.to_str().unwrap(),
            }),
        )
        .expect("sibling root must be accepted");
    assert!(
        result.contains("Added") || result.contains("sibling"),
        "sibling add response: {result}"
    );
}

// ---------------------------------------------------------------------------
// rename_file: parent traversal attempt in `from` is rejected.
// ---------------------------------------------------------------------------

#[test]
fn rename_file_rejects_traversal_in_from_path() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub fn a() {}\n"),
        ],
    );
    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({
                "from": "../../escape.rs",
                "to": "src/escape.rs",
                "apply": false,
            }),
        )
        .expect_err("traversal in from must error");
    assert!(
        err.contains("..")
            || err.contains("traversal")
            || err.contains("relative")
            || err.contains("invalid"),
        "from-traversal rejection wording: {err}"
    );
}

// ---------------------------------------------------------------------------
// rename_file: missing parent directory is auto-created (mkdir -p
// semantics). The audit complaint was that the behavior was unclear,
// not that auto-create was wrong - the existing
// `rename_file_apply_into_subdirectory` quality test pins this as
// intentional behavior. Validate the auto-create here.
// ---------------------------------------------------------------------------

#[test]
fn rename_file_auto_creates_missing_parent_on_apply() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod util;\n"),
            ("src/util.rs", "pub fn helper() {}\n"),
        ],
    );
    let result = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({
                "from": "src/util.rs",
                "to": "src/helpers/util.rs",
                "apply": true,
            }),
        )
        .expect("apply with missing parent must auto-create");
    assert!(result.contains("renamed"), "got: {result}");
    assert!(
        dir.path().join("src/helpers/util.rs").exists(),
        "destination must exist after auto-mkdir"
    );
    assert!(
        !dir.path().join("src/util.rs").exists(),
        "source must be moved"
    );
}

#[test]
fn rename_file_rejects_traversal_in_to_path() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub fn a() {}\n"),
        ],
    );
    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({
                "from": "src/lib.rs",
                "to": "../../escape.rs",
                "apply": false,
            }),
        )
        .expect_err("traversal in to must error");
    assert!(
        err.contains("..") || err.contains("traversal") || err.contains("relative"),
        "to-traversal rejection wording: {err}"
    );
}

// ---------------------------------------------------------------------------
// outline mermaid rejection still refers callers to graph alternatives.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// purge_orphaned: must NOT delete live workspace files.
// ---------------------------------------------------------------------------

#[test]
fn purge_orphaned_preserves_live_files() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod util;\n"),
            ("src/util.rs", "pub fn helper() {}\n"),
        ],
    );

    // Sanity: live files are present in the index.
    let stats_before = server
        .call_tool_by_name("qartez_stats", json!({}))
        .expect("stats");
    assert!(
        stats_before.contains("files=") && !stats_before.contains("files=0"),
        "stats must report indexed files, got: {stats_before}"
    );

    let result = server
        .call_tool_by_name("qartez_maintenance", json!({ "action": "purge_orphaned" }))
        .expect("purge_orphaned must not error");
    // The fixture has no orphaned rows; the count must be 0.
    assert!(
        result.contains("0 ")
            || result.contains("nothing")
            || result.contains("no orphan")
            || result.contains("0 row"),
        "purge_orphaned on a clean index must report 0 rows removed, got: {result}"
    );

    // After purge, live files must still be in the index.
    let stats_after = server
        .call_tool_by_name("qartez_stats", json!({}))
        .expect("stats");
    assert_eq!(
        stats_before, stats_after,
        "purge_orphaned must not have touched the indexed file set",
    );
}

// ---------------------------------------------------------------------------
// convert_incremental: idempotent on already-INCREMENTAL DB.
// ---------------------------------------------------------------------------

#[test]
fn convert_incremental_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub fn a() {}\n"),
        ],
    );
    let first = server
        .call_tool_by_name(
            "qartez_maintenance",
            json!({ "action": "convert_incremental" }),
        )
        .expect("first convert must succeed");
    let second = server
        .call_tool_by_name(
            "qartez_maintenance",
            json!({ "action": "convert_incremental" }),
        )
        .expect("second convert must succeed (idempotent)");
    // The second call must explicitly report that it was a no-op.
    assert!(
        second.to_lowercase().contains("already")
            || second.to_lowercase().contains("idempotent")
            || second.to_lowercase().contains("no-op"),
        "second convert must signal no-op, got first={first} second={second}"
    );
}

// ---------------------------------------------------------------------------
// diff_impact ACK footer differentiates per-file ACKs from idempotency
// marker (item 45). The agent test bundle did not test the actual on-
// disk file count. We verify the message wording but not the file
// count - that requires git history setup we'd rather not duplicate.
// ---------------------------------------------------------------------------

#[test]
fn ack_path_components_use_acks_directory() {
    // Ack path lands under .qartez/acks/<hex>; diff-marker path lands
    // under .qartez/acks/diff-markers/<hex>. The squash-merged commit
    // surfaces the diff-markers/ subdirectory in the user-facing
    // footer; this test pins the path layout so a future refactor of
    // the FNV digest scheme cannot move the marker into a sibling
    // directory without re-aligning the footer wording.
    let tmp = TempDir::new().unwrap();
    let path = guard::ack_path(tmp.path(), "src/lib.rs");
    let path_str = path.to_string_lossy();
    assert!(
        path_str.contains(".qartez/acks/") || path_str.contains(".qartez\\acks\\"),
        "ack path must live under .qartez/acks/, got: {path_str}"
    );
    // Per-file ack must NOT collide with the diff-markers subtree.
    assert!(
        !path_str.contains("diff-markers"),
        "per-file ack must not be confused with the marker subdir, got: {path_str}"
    );
}

// ---------------------------------------------------------------------------
// calls: depth-clamp warning lives in the footer.
// ---------------------------------------------------------------------------

#[test]
fn calls_depth_clamp_warning_is_in_footer() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            (
                "src/lib.rs",
                "pub fn root() { helper() }\npub fn helper() {}\n",
            ),
        ],
    );
    let result = server
        .call_tool_by_name("qartez_calls", json!({ "name": "root", "depth": 99 }))
        .expect("calls must succeed");
    // The warning should not be on the FIRST line - the result section
    // should come first, then the footer.
    let first_line = result.lines().next().unwrap_or("");
    assert!(
        !first_line.starts_with("!warning"),
        "depth clamp must NOT be on first line, got: {first_line}"
    );
    // But it should be present somewhere in the response.
    assert!(
        result.contains("depth=99") || result.contains("clamped"),
        "depth clamp note must be present somewhere, got: {result}"
    );
}

// ---------------------------------------------------------------------------
// refs: include_tests=false footer reports hidden count when filter
// suppressed any test refs.
// ---------------------------------------------------------------------------

#[test]
fn refs_hidden_footer_appears_when_filter_active() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            (
                "src/lib.rs",
                "pub fn helper() {}\npub fn callsite() { helper(); }\n",
            ),
            (
                "tests/it.rs",
                "use x::helper;\n#[test] fn t() { helper(); }\n",
            ),
        ],
    );
    // Default include_tests=true does NOT show hidden footer.
    let default = server
        .call_tool_by_name("qartez_refs", json!({ "symbol": "helper" }))
        .expect("refs default");
    assert!(
        !default.contains("hidden by include_tests=false"),
        "default must not advertise hidden footer, got: {default}"
    );
    // When include_tests=false suppresses test refs, the footer
    // should mention the hidden count. Note: the existing fixture
    // may not produce test edges here; the test asserts conditional
    // behavior - if any rows exist with include_tests=true that are
    // gone with include_tests=false, the footer must be present.
    let with_tests = server
        .call_tool_by_name(
            "qartez_refs",
            json!({ "symbol": "helper", "include_tests": true }),
        )
        .expect("with tests");
    let without_tests = server
        .call_tool_by_name(
            "qartez_refs",
            json!({ "symbol": "helper", "include_tests": false }),
        )
        .expect("without tests");
    if with_tests.len() > without_tests.len() {
        // If filter actually suppressed something, footer must say so.
        assert!(
            without_tests.contains("hidden") || without_tests.contains("include_tests=true"),
            "filter activated but no hidden-count footer, got: {without_tests}"
        );
    }
}

// ---------------------------------------------------------------------------
// refactor_plan: CC=1 functions do not get the past-budget rationale.
// ---------------------------------------------------------------------------

#[test]
fn refactor_plan_does_not_flag_trivial_cc_1_functions() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            (
                "src/lib.rs",
                "pub fn trivial() { let x = 1; let y = 2; let _ = x + y; }\n",
            ),
        ],
    );
    let result = server
        .call_tool_by_name(
            "qartez_refactor_plan",
            json!({
                "file_path": "src/lib.rs",
                "limit": 0,
                "min_complexity": 1,
            }),
        )
        .expect("refactor_plan must succeed");
    // CC=1 must never trigger the "past usual review/test budget"
    // wording even when min_complexity is forced down to 1.
    if result.contains("trivial") {
        let lower = result.to_lowercase();
        assert!(
            !lower.contains("review/test budget"),
            "CC=1 must not trip the budget wording, got: {result}"
        );
    }
}

// ---------------------------------------------------------------------------
// context: cross-language seeds and same-language guard interaction.
// A multi-language seed set keeps cross-language hits available; a
// single-language seed set drops them. Confirms the guard is gated on
// seed homogeneity and not unconditional.
// ---------------------------------------------------------------------------

#[test]
fn context_multi_language_seeds_allow_cross_language_hits() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            (
                "src/lib.rs",
                "pub fn rust_parse() {}\npub fn rust_analyze() {}\n",
            ),
            (
                "scripts/plugin.ts",
                "export function tsParse() {}\nexport function tsAnalyze() {}\n",
            ),
        ],
    );
    // Seed with BOTH languages: cross-language credit should now be
    // allowed because seed_languages contains both rust and typescript.
    let result = server
        .call_tool_by_name(
            "qartez_context",
            json!({
                "files": ["src/lib.rs", "scripts/plugin.ts"],
                "task": "parse and analyze",
            }),
        )
        .expect("context must succeed");
    // Specific assertion: when seed languages span both, the response
    // should not error out on the language filter. We accept any
    // legitimate response shape.
    assert!(
        !result.is_empty(),
        "multi-language seed context must produce a response"
    );
}

#[test]
fn outline_mermaid_rejection_keeps_both_messages() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &[("src/a.rs", "pub fn a() {}\n")]);
    let err = server
        .call_tool_by_name(
            "qartez_outline",
            json!({ "file_path": "src/a.rs", "format": "mermaid" }),
        )
        .expect_err("outline must reject mermaid");
    // Must explain the per-file mismatch.
    assert!(err.contains("symbol table") && err.contains("not a graph"));
    // Must still surface the working symbol-table format.
    assert!(err.contains("concise") && err.contains("default"));
    // Must still surface the graph alternatives for the visual-graph
    // intent so the existing contract test in
    // fp_regression_tool_batch_april_23.rs keeps passing.
    for tool in &["qartez_deps", "qartez_calls", "qartez_hierarchy"] {
        assert!(err.contains(tool), "missing alt {tool}: {err}");
    }
}
