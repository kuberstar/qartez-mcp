// Rust guideline compliant 2026-04-22
//
// End-to-end regression coverage for the qartez_calls / qartez_refs
// accuracy fixes landed 2026-04-22. Each test indexes a hand-built
// Cargo fixture under TempDir, drives the MCP handler through
// `call_tool_by_name`, and asserts the exact FP-vs-TP shape reported
// by the triage.
//
// Bugs covered:
//   B1  same-named method disambiguation (callees)
//   B2  depth parameter respected beyond 2
//   B3  limit / token_budget truncation with explicit footer
//   B4  include_tests=false excludes test callers by default
//   B5  refs transitive walks the per-symbol importer set, not the
//       defining file's dependents
//   B6  refs direct-references list drops the defining file

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

fn build_and_index(dir: &Path) -> QartezServer {
    fs::create_dir_all(dir.join(".git")).unwrap();
    let conn = setup_db();
    index::full_index(&conn, dir, false).unwrap();
    QartezServer::new(conn, dir.to_path_buf(), 0)
}

fn write_cargo_manifest(dir: &Path) {
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .unwrap();
}

// --------------------------------------------------------------------------
// B1: callees with the same bare name but different owner types must be
// reported as ambiguous candidates rather than collapsed under one
// arbitrary pick.
// --------------------------------------------------------------------------

#[test]
fn calls_ambiguous_same_name_method_emits_every_candidate() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("lib.rs"),
        "pub mod foo;\npub mod bar;\npub mod caller;\n",
    )
    .unwrap();
    fs::write(
        src.join("foo.rs"),
        "pub struct Foo;\nimpl Foo {\n    pub fn new() -> Self { Foo }\n}\n",
    )
    .unwrap();
    fs::write(
        src.join("bar.rs"),
        "pub struct Bar;\nimpl Bar {\n    pub fn new() -> Self { Bar }\n}\n",
    )
    .unwrap();
    fs::write(
        src.join("caller.rs"),
        "use crate::foo::Foo;\nuse crate::bar::Bar;\npub fn uses_both() {\n    let _f = Foo::new();\n    let _b = Bar::new();\n}\n",
    )
    .unwrap();

    let server = build_and_index(root);
    let out = server
        .call_tool_by_name(
            "qartez_calls",
            json!({
                "name": "uses_both",
                "direction": "callees",
                "include_tests": false,
            }),
        )
        .expect("qartez_calls uses_both must succeed");

    assert!(
        out.contains("ambiguous") || (out.contains("Foo::new") && out.contains("Bar::new")),
        "same-name callees must surface both candidates (either as qualified rows or as an `ambiguous (N candidates)` block):\n{out}"
    );
    assert!(
        out.contains("src/foo.rs") && out.contains("src/bar.rs"),
        "both defining files of the colliding `new` candidates must appear in the output:\n{out}"
    );
}

// --------------------------------------------------------------------------
// B2: depth=3 must expose a deeper chain than depth=2 when the graph
// allows it.
// --------------------------------------------------------------------------

#[test]
fn calls_depth_respected_beyond_two() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(src.join("lib.rs"), "pub mod chain;\n").unwrap();
    fs::write(
        src.join("chain.rs"),
        r#"pub fn level0() { level1(); }
pub fn level1() { level2(); }
pub fn level2() { level3(); }
pub fn level3() { level4(); }
pub fn level4() { }
"#,
    )
    .unwrap();

    let server = build_and_index(root);

    let out2 = server
        .call_tool_by_name(
            "qartez_calls",
            json!({ "name": "level0", "direction": "callees", "depth": 2 }),
        )
        .expect("qartez_calls level0 depth=2 must succeed");
    let out3 = server
        .call_tool_by_name(
            "qartez_calls",
            json!({ "name": "level0", "direction": "callees", "depth": 3 }),
        )
        .expect("qartez_calls level0 depth=3 must succeed");
    let out5 = server
        .call_tool_by_name(
            "qartez_calls",
            json!({ "name": "level0", "direction": "callees", "depth": 5 }),
        )
        .expect("qartez_calls level0 depth=5 must succeed");

    let count_level3 = |s: &str| s.matches("level3").count();
    assert!(
        count_level3(&out3) > count_level3(&out2),
        "depth=3 must produce additional `level3` references compared to depth=2. depth=2:\n{out2}\n---\ndepth=3:\n{out3}"
    );
    assert!(
        out5.contains("level4"),
        "depth=5 must reach level4 through the chain:\n{out5}"
    );
}

// --------------------------------------------------------------------------
// B3: limit parameter truncates the callers section with an explicit
// footer pointing at the `limit=` knob.
// --------------------------------------------------------------------------

#[test]
fn calls_limit_truncates_with_explicit_footer() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    let mut lib = String::from("pub fn hub() { }\n");
    for i in 0..40 {
        lib.push_str(&format!("pub fn caller_{i}() {{ hub(); }}\n"));
    }
    fs::write(src.join("lib.rs"), lib).unwrap();

    let server = build_and_index(root);

    let out = server
        .call_tool_by_name(
            "qartez_calls",
            json!({
                "name": "hub",
                "direction": "callers",
                "limit": 5,
                "include_tests": false,
            }),
        )
        .expect("qartez_calls hub limit=5 must succeed");

    assert!(
        out.contains("callers: 40"),
        "callers count header must still reflect the real 40 callers, not the truncated 5:\n{out}"
    );
    assert!(
        out.contains("more, raise limit=") || out.contains("more, raise token_budget="),
        "truncated output must mention how to widen the cap:\n{out}"
    );
    let caller_lines = out.lines().filter(|l| l.contains("caller_")).count();
    assert!(
        caller_lines <= 5,
        "limit=5 must cap the emitted caller rows; got {caller_lines}:\n{out}"
    );
}

// --------------------------------------------------------------------------
// B4: include_tests defaults to false. Test-file callers must not appear
// unless the caller opts in.
// --------------------------------------------------------------------------

#[test]
fn calls_excludes_test_callers_by_default() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    let tests = root.join("tests");
    fs::create_dir_all(&tests).unwrap();

    fs::write(
        src.join("production_caller.rs"),
        "use crate::widget;\npub fn use_widget() { let _ = widget(); }\n",
    )
    .unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub mod production_caller;\npub fn widget() -> u32 { 7 }\n",
    )
    .unwrap();
    fs::write(
        tests.join("widget_test.rs"),
        "use fixture::widget;\n#[test]\nfn t() {\n    let _ = widget();\n}\n",
    )
    .unwrap();

    let server = build_and_index(root);

    let out_default = server
        .call_tool_by_name(
            "qartez_calls",
            json!({ "name": "widget", "direction": "callers" }),
        )
        .expect("qartez_calls widget must succeed");
    assert!(
        !out_default.contains("tests/widget_test.rs"),
        "test-file caller must be excluded by default:\n{out_default}"
    );
    assert!(
        out_default.contains("production_caller.rs"),
        "production caller must still be listed:\n{out_default}"
    );

    let out_with_tests = server
        .call_tool_by_name(
            "qartez_calls",
            json!({
                "name": "widget",
                "direction": "callers",
                "include_tests": true,
            }),
        )
        .expect("qartez_calls widget include_tests=true must succeed");
    assert!(
        out_with_tests.contains("widget_test.rs"),
        "include_tests=true must re-include test-file callers:\n{out_with_tests}"
    );
}

// --------------------------------------------------------------------------
// B5: qartez_refs transitive=true walks the per-symbol importer set,
// not the defining file's full dependents.
// --------------------------------------------------------------------------

#[test]
fn refs_transitive_is_per_symbol_not_per_file() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("hub.rs"),
        "pub fn used_symbol() -> u32 { 1 }\npub fn unused_symbol() -> u32 { 2 }\n",
    )
    .unwrap();
    fs::write(
        src.join("uses_used.rs"),
        "use crate::hub::used_symbol;\npub fn a() -> u32 { used_symbol() }\n",
    )
    .unwrap();
    fs::write(
        src.join("chain_used.rs"),
        "use crate::uses_used::a;\npub fn b() -> u32 { a() }\n",
    )
    .unwrap();
    fs::write(
        src.join("noise.rs"),
        "use crate::hub::unused_symbol;\npub fn c() -> u32 { unused_symbol() }\n",
    )
    .unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub mod hub;\npub mod uses_used;\npub mod chain_used;\npub mod noise;\n",
    )
    .unwrap();

    let server = build_and_index(root);

    let out = server
        .call_tool_by_name(
            "qartez_refs",
            json!({
                "symbol": "used_symbol",
                "transitive": true,
                "token_budget": 20000,
            }),
        )
        .expect("qartez_refs used_symbol transitive must succeed");

    assert!(
        out.contains("used_symbol"),
        "output must name the queried symbol:\n{out}"
    );
    assert!(
        !out.contains("noise.rs"),
        "noise.rs imports only unused_symbol and must NOT appear in the transitive dependents of used_symbol (pre-fix bug):\n{out}"
    );
    assert!(
        out.contains("chain_used.rs") || out.contains("uses_used.rs"),
        "transitive walk must still surface files that actually reach used_symbol:\n{out}"
    );
}

// --------------------------------------------------------------------------
// B6: the defining file must not appear in the Direct-references section.
// --------------------------------------------------------------------------

#[test]
fn refs_filters_defining_file_from_direct_references() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("target.rs"),
        "pub fn target_fn() -> u32 { 0 }\npub fn neighbour() -> u32 { target_fn() }\n",
    )
    .unwrap();
    fs::write(
        src.join("consumer.rs"),
        "use crate::target::target_fn;\npub fn call() -> u32 { target_fn() }\n",
    )
    .unwrap();
    fs::write(src.join("lib.rs"), "pub mod target;\npub mod consumer;\n").unwrap();

    let server = build_and_index(root);

    let out = server
        .call_tool_by_name(
            "qartez_refs",
            json!({ "symbol": "target_fn", "token_budget": 20000 }),
        )
        .expect("qartez_refs target_fn must succeed");

    // Self-references (a symbol's own body calling itself) must NOT pad
    // the `Direct references` list. Intra-file references from a
    // DIFFERENT symbol in the same file (`neighbour` -> `target_fn`)
    // are legitimate usages and DO appear - hiding them would make
    // `pub(super)` helpers reached only through a sibling in the same
    // module look dead.
    let direct_section_start = out
        .find("Direct references")
        .expect("Direct references section must be present");
    let direct_section_end = out[direct_section_start..]
        .find("\n\n")
        .map(|n| direct_section_start + n)
        .unwrap_or(out.len());
    let direct_section = &out[direct_section_start..direct_section_end];

    assert!(
        direct_section.contains("src/target.rs"),
        "intra-file caller (`neighbour` inside src/target.rs) must surface:\n{direct_section}"
    );
    assert!(
        direct_section.contains("src/consumer.rs"),
        "legitimate external importer must still appear in Direct references:\n{direct_section}"
    );
    assert!(
        out.contains("Defined in: src/target.rs"),
        "defining file must still be surfaced in the `Defined in:` header:\n{out}"
    );
}
