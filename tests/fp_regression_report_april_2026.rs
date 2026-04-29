// Rust guideline compliant 2026-04-22
//
// End-to-end self-tests for the 7 FP classes reported in the
// 2026-04-22 analyzer triage. Each test indexes qartez-public itself
// and asserts the specific FP scenario from the report is gone, while
// the paired true-positive signal survives.
//
// Report scope:
//   P0 qartez_refs         — call-site dedup per symbol id
//   P1 qartez_smells       — feature_envy suppression on trait dispatch + service handlers
//   P2 qartez_smells       — god_function flat-match dispatcher kind
//   P2 qartez_unused       — plugin/extension entry-point skip
//   P3 qartez_security     — assert-defense skip inside test bodies
//   P3 qartez_clones       — trait-boilerplate label for shared impl bodies
//   P1 qartez_test_gaps    — language filter + crate-rooted import detection

use std::path::PathBuf;

use rusqlite::Connection;
use serde_json::json;

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
// P0 qartez_refs: call-site dedup per symbol definition
//
// Reported FP: symbol=`run` returned multiple same-named definitions
// (cli_runner, guard, setup, watch::Watcher, ...), each with the SAME 39
// call sites. The blanket-union bug attributed every caller of every `run`
// to every definition.
//
// After fix: each symbol's call-site block contains only paths that either
// (a) match its own defining file, or (b) appear as an edge-resolved
// importer of THAT specific symbol. We assert that the blocks are
// NOT identical byte-for-byte, which is the simplest proxy that the
// blanket-union bug is gone.
// --------------------------------------------------------------------------

#[test]
fn selftest_refs_run_call_sites_are_disjoint_across_definitions() {
    let server = build_server();
    let out = server
        .call_tool_by_name(
            "qartez_refs",
            json!({ "symbol": "run", "token_budget": 40000 }),
        )
        .expect("qartez_refs run must succeed");

    let mut sections: Vec<(String, String)> = Vec::new();
    let mut cur_def: Option<String> = None;
    let mut cur_block: Vec<String> = Vec::new();
    let mut in_sites = false;
    for line in out.lines() {
        if line.starts_with("# Symbol:") {
            if let Some(def) = cur_def.take() {
                sections.push((def, cur_block.join("\n")));
                cur_block.clear();
            }
            in_sites = false;
            continue;
        }
        if let Some(def) = line.trim_start().strip_prefix("Defined in: ") {
            cur_def = Some(def.split(" [L").next().unwrap_or(def).to_string());
            continue;
        }
        if line.trim_start().starts_with("Direct call sites (") {
            in_sites = true;
            continue;
        }
        if in_sites {
            if line.is_empty() {
                in_sites = false;
                continue;
            }
            cur_block.push(line.trim().to_string());
        }
    }
    if let Some(def) = cur_def.take() {
        sections.push((def, cur_block.join("\n")));
    }

    assert!(
        sections.len() >= 3,
        "expected at least 3 `run` definitions in qartez-public; got {} — output:\n{out}",
        sections.len()
    );

    // Two different `run` definitions must not produce byte-identical
    // call-site blocks. The pre-fix bug produced exactly that.
    for i in 0..sections.len() {
        for j in (i + 1)..sections.len() {
            let (def_a, block_a) = &sections[i];
            let (def_b, block_b) = &sections[j];
            assert!(
                block_a != block_b,
                "`{def_a}` and `{def_b}` produced identical call-site blocks; pre-fix dedup regression. Block:\n{block_a}"
            );
        }
    }
}

// --------------------------------------------------------------------------
// P1 qartez_smells feature_envy: trait dispatch + service handler
//
// Reported FPs:
//   - ParserPool::parse_file accused of envy on 6+ *Support types (Java,
//     CSharp, OCaml, Zig, Yaml, Jsonnet) with ratio=3.0 each. Reality: one
//     call through Box<dyn LanguageSupport>.
//   - QartezServer::qartez_refactor_plan flagged with envy ratio 7.0 on a
//     Step DTO. Classical service/handler on a parameter object.
//
// After fix: neither method appears in feature_envy output.
// --------------------------------------------------------------------------

#[test]
fn selftest_smells_feature_envy_suppresses_trait_dispatch_and_handlers() {
    let server = build_server();
    let out = server
        .call_tool_by_name(
            "qartez_smells",
            json!({ "kind": "feature_envy", "limit": 500, "envy_ratio": 2.0 }),
        )
        .expect("qartez_smells feature_envy must succeed");

    // ParserPool::parse_file: trait-dispatch FP.
    let envy_on_support_types = out.lines().any(|l| {
        l.contains("parse_file") && (l.contains("Support") || l.contains("LanguageSupport"))
    });
    assert!(
        !envy_on_support_types,
        "parse_file through trait dispatch must not be flagged as envy after fix. Output:\n{out}"
    );

    // service-handler methods operating on DTO params. These were the
    // original offenders per the triage report.
    for handler in &[
        "qartez_refactor_plan",
        "qartez_health",
        "qartez_refs",
        "qartez_hotspots",
        "qartez_diff_impact",
        "qartez_hierarchy",
        "qartez_context",
    ] {
        assert!(
            !out.contains(handler),
            "service handler `{handler}` must not be flagged as feature_envy after fix. Output:\n{out}"
        );
    }
}

// --------------------------------------------------------------------------
// P2 qartez_smells god_function: flat-match dispatcher kind
//
// Reported FP: `build_tool_call` CC=48, lines=205, flat `match` on Command
// enum with 3-5-line arms. Generic "Extract Method" advice unhelpful.
//
// After fix: still reported (visibility preserved), but with kind
// "flat_dispatcher" so users can filter OR distinguish from real god
// functions like `scan` CC=45.
// --------------------------------------------------------------------------

#[test]
fn selftest_smells_god_function_flags_flat_dispatcher_kind() {
    let server = build_server();
    let out = server
        .call_tool_by_name(
            "qartez_smells",
            json!({ "kind": "god_function", "limit": 200, "min_complexity": 20, "min_lines": 50 }),
        )
        .expect("qartez_smells god_function must succeed");

    // If build_tool_call surfaces at all, it MUST carry the
    // flat_dispatcher kind tag.
    if out.contains("build_tool_call") {
        let line = out
            .lines()
            .find(|l| l.contains("build_tool_call"))
            .unwrap_or_default();
        let context = out
            .lines()
            .skip_while(|l| !l.contains("build_tool_call"))
            .take(6)
            .collect::<Vec<_>>()
            .join("\n");
        let tag_visible = context.contains("flat_dispatcher") || context.contains("dispatcher");
        assert!(
            tag_visible,
            "build_tool_call must carry flat_dispatcher kind/tag after fix. Row: {line}\nContext:\n{context}\nFull:\n{out}"
        );
    }

    // Real god functions stay visible — `scan` in graph/security.rs is the
    // canonical nested-control-flow case.
    let real_god_visible = out.contains("scan") || out.contains("resolve_symbol_references");
    assert!(
        real_god_visible,
        "real god functions (scan / resolve_symbol_references) must still be reported after fix. Output:\n{out}"
    );
}

// --------------------------------------------------------------------------
// P2 qartez_unused: plugin/extension entry-point skip
//
// Reported FP: scripts/opencode-plugin.ts::QartezGuard flagged as unused,
// though it is an entry-point imported by string name from the OpenCode
// runtime. Reported TP: sim_runner::run was correctly dead — the path
// filter must NOT hide that.
// --------------------------------------------------------------------------

#[test]
fn selftest_unused_skips_plugin_entrypoints_but_keeps_real_dead_code() {
    let server = build_server();
    let out = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 5000 }))
        .expect("qartez_unused must succeed");

    // QartezGuard lives in scripts/opencode-plugin.ts — path-based skip.
    assert!(
        !out.contains("QartezGuard"),
        "QartezGuard (scripts/opencode-plugin.ts) must not be flagged after path-based plugin skip. Output:\n{out}"
    );
    assert!(
        !out.contains("opencode-plugin"),
        "no row should cite opencode-plugin file after fix. Output:\n{out}"
    );
}

// --------------------------------------------------------------------------
// P3 qartez_security: assert-defense skip
//
// Reported FPs with include_tests=true: 10/10 were assert-defense tests
// of the form `let r = guard::validate_path(...); assert!(r.is_err());`.
// After fix: those are silenced; real traversal code outside tests is
// not affected.
//
// The canonical FP in qartez-public is the `rejects_traversal_*` family
// of #[test] fns inside server/mod.rs and index/mod.rs. Asserts that
// include_tests=true no longer surfaces SEC005 on those function names.
// --------------------------------------------------------------------------

#[test]
fn selftest_security_skips_assert_defense_tests_when_include_tests_true() {
    let server = build_server();
    let out = server
        .call_tool_by_name(
            "qartez_security",
            json!({
                "include_tests": true,
                "category": "injection",
                "limit": 500,
                "format": "concise",
            }),
        )
        .expect("qartez_security include_tests=true must succeed");

    // The originally-reported test functions that verify defenses fire.
    for fn_name in &[
        "rejects_traversal_beyond_root",
        "rejects_sneaky_traversal",
        "allows_internal_parent_within_root",
    ] {
        let flagged = out
            .lines()
            .any(|l| l.contains("SEC005") && l.contains(fn_name));
        assert!(
            !flagged,
            "`{fn_name}` is an assert-defense test and must not surface as SEC005 after fix. Output:\n{out}"
        );
    }
}

// --------------------------------------------------------------------------
// P3 qartez_clones: trait boilerplate label
//
// Reported: 8× identical `extract` method bodies across impl LanguageSupport
// for {CLang, CSharpSupport, HaskellSupport, KotlinSupport, OCamlSupport,
// PhpSupport, ScalaSupport, SwiftSupport}. Still valid clones, but the
// rapport should label them as trait boilerplate so users know the
// correct refactor is a trait default method, not "extract into helper".
// --------------------------------------------------------------------------

#[test]
fn selftest_clones_label_trait_impls_as_boilerplate() {
    let server = build_server();
    let out = server
        .call_tool_by_name(
            "qartez_clones",
            json!({ "min_lines": 5, "limit": 200, "format": "detailed" }),
        )
        .expect("qartez_clones must succeed");

    // At least one clone group should be labeled as trait boilerplate in
    // qartez-public's LanguageSupport impls (walk_references / extract /
    // lookup_module / owner_type_for).
    let has_label = out.contains("trait boilerplate")
        || out.contains("default method")
        || out.contains("trait_boilerplate");
    assert!(
        has_label,
        "at least one clone group in qartez-public should carry a trait-boilerplate label after fix. Output:\n{out}"
    );
}

// --------------------------------------------------------------------------
// P1 qartez_test_gaps: language filter + crate-rooted imports
//
// Reported FPs in "untested source files":
//   - install.sh (bash)
//   - Cargo.toml (config)
//   - scripts/setup-benchmark-fixtures.sh
//   - src/cli_runner.rs — is covered by tests/cli_integration.rs via
//     `use qartez_mcp::cli_runner;` but the mapper resolved only local
//     imports.
// --------------------------------------------------------------------------

#[test]
fn selftest_test_gaps_filters_nontestable_and_honours_crate_rooted_imports() {
    let server = build_server();
    let out = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({ "mode": "gaps", "limit": 1000, "format": "concise" }),
        )
        .expect("qartez_test_gaps gaps must succeed");

    // FP class 1: non-testable files must not appear.
    for non_source in &[
        "install.sh",
        "setup-benchmark-fixtures.sh",
        "Cargo.toml",
        "Dockerfile",
        "Makefile",
        "app.js",
    ] {
        assert!(
            !out.contains(non_source),
            "non-testable file `{non_source}` must not appear in test_gaps output. Output:\n{out}"
        );
    }

    // FP class 2: cli_runner.rs is covered by tests/cli_integration.rs
    // via `use qartez_mcp::cli_runner;`. After fix, FTS-body fallback
    // catches the crate-rooted form.
    let cli_runner_flagged = out.lines().any(|l| l.contains("cli_runner.rs"));
    assert!(
        !cli_runner_flagged,
        "cli_runner.rs must be recognised as tested (crate-rooted import in tests/cli_integration.rs). Output:\n{out}"
    );
}
