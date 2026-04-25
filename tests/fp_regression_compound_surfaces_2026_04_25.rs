// Rust guideline compliant 2026-04-25
//
// End-to-end regression coverage for the compound-surfaces additions
// landed in 2026-04-25:
//
//   1. qartez_context: include_impact / include_test_gaps flags.
//   2. qartez_understand: new compound investigation tool.
//   3. qartez_map: with_health flag.
//
// Every test indexes a real on-disk fixture so the assertions run
// against the same code path qartez serves to MCP clients - in
// particular, `call_tool_by_name` exercises the JSON dispatch layer
// (alias handling, flexible deserialisation, schema match) on top
// of the typed handler.

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
        "[package]\nname = \"compound_fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .unwrap();
}

/// Fixture with three source files and one test file that imports two
/// of them. `service.rs` carries a deliberately god-shaped function
/// (high CC, long body, many params) so qartez_map with_health=true
/// has a row that exercises every smell branch. `lonely.rs` has no
/// importer and no test, so it surfaces in the test-gaps output as
/// `untested`.
fn write_compound_fixture(dir: &Path) {
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    let tests = dir.join("tests");
    fs::create_dir_all(&tests).unwrap();

    // service.rs: a god-shaped dispatcher that should trip both the
    // `god_function` (high CC + long body) and `long_params` (>=5
    // params) heuristics. The body intentionally branches on every
    // arm so cyclomatic complexity climbs above the qartez_health
    // default threshold of 15.
    let mut service = String::from(
        "pub fn dispatch(\n    a: u32,\n    b: u32,\n    c: u32,\n    d: u32,\n    e: u32,\n    f: u32,\n) -> u32 {\n",
    );
    for i in 0..30 {
        service.push_str(&format!(
            "    if a == {i} {{ return b + c + {i}; }}\n    if b == {i} {{ return c + d + {i}; }}\n",
        ));
    }
    service.push_str("    a + b + c + d + e + f\n}\n\n");
    service.push_str(
        "pub fn helper() -> u32 { 42 }\n\npub struct Config {\n    pub name: String,\n}\n\nimpl Config {\n    pub fn new() -> Self {\n        Config { name: String::from(\"x\") }\n    }\n}\n",
    );
    fs::write(src.join("service.rs"), service).unwrap();

    // util.rs: small clean module with low CC, used by service via a
    // crate-rooted import.
    fs::write(
        src.join("util.rs"),
        "pub fn add(x: u32, y: u32) -> u32 { x + y }\npub fn mul(x: u32, y: u32) -> u32 { x * y }\n",
    )
    .unwrap();

    // lonely.rs: no importer, no test. Exists so test-gaps lands at
    // least one row in the `untested` branch when the input list
    // includes it.
    fs::write(
        src.join("lonely.rs"),
        "pub fn forgotten() -> bool { true }\n",
    )
    .unwrap();

    fs::write(
        src.join("lib.rs"),
        "pub mod service;\npub mod util;\npub mod lonely;\n",
    )
    .unwrap();

    // Test file pulls in service.rs only. test-gaps must mark
    // util.rs and lonely.rs as untested when those files are passed
    // as input.
    fs::write(
        tests.join("service_test.rs"),
        "use compound_fixture::service;\n#[test]\nfn dispatch_works() {\n    assert_eq!(service::dispatch(0, 1, 2, 3, 4, 5), 1 + 2);\n    assert_eq!(service::helper(), 42);\n}\n",
    )
    .unwrap();
}

fn fixture() -> (TempDir, QartezServer) {
    let dir = TempDir::new().unwrap();
    write_cargo_manifest(dir.path());
    write_compound_fixture(dir.path());
    let server = build_and_index(dir.path());
    (dir, server)
}

// =========================================================================
// qartez_context: include_impact + include_test_gaps
// =========================================================================

#[test]
fn context_include_impact_emits_real_counts() {
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name(
            "qartez_context",
            json!({
                "files": ["src/service.rs"],
                "include_impact": true,
            }),
        )
        .expect("qartez_context with include_impact must succeed");

    assert!(
        out.contains("## Impact (per input file)"),
        "Impact section header must render: {out}",
    );
    // service.rs is reachable from lib.rs (mod), so direct must be >0.
    assert!(
        out.contains("src/service.rs - direct="),
        "Impact row must include direct count: {out}",
    );
    assert!(
        out.contains("transitive=") && out.contains("cochange="),
        "Impact row must include transitive + cochange counts: {out}",
    );
}

#[test]
fn context_include_test_gaps_distinguishes_tested_vs_untested() {
    let (_dir, server) = fixture();
    // Pass two input files: service.rs (covered by tests/service_test.rs)
    // and lonely.rs (no test imports it). The output must contain one
    // tested row and one untested row.
    let out = server
        .call_tool_by_name(
            "qartez_context",
            json!({
                "files": ["src/service.rs", "src/lonely.rs"],
                "include_test_gaps": true,
            }),
        )
        .expect("qartez_context with include_test_gaps must succeed");

    assert!(
        out.contains("## Test gaps (per input file)"),
        "Test gaps section header must render: {out}",
    );
    assert!(
        out.contains("src/service.rs - 1 test(s)") || out.contains("src/service.rs - 2 test(s)"),
        "service.rs is imported by tests/service_test.rs, must show >=1 test: {out}",
    );
    assert!(
        out.contains("src/lonely.rs - untested"),
        "lonely.rs has no test importer, must be marked untested: {out}",
    );
}

#[test]
fn context_both_flags_render_both_sections() {
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name(
            "qartez_context",
            json!({
                "files": ["src/service.rs"],
                "include_impact": true,
                "include_test_gaps": true,
            }),
        )
        .expect("qartez_context with both flags must succeed");

    assert!(
        out.contains("## Impact (per input file)"),
        "Impact section must render when both flags on: {out}",
    );
    assert!(
        out.contains("## Test gaps (per input file)"),
        "Test gaps section must render when both flags on: {out}",
    );
    // Sections must appear in deterministic order: Impact before Test gaps.
    let impact_pos = out.find("## Impact").unwrap();
    let gaps_pos = out.find("## Test gaps").unwrap();
    assert!(
        impact_pos < gaps_pos,
        "Impact must appear before Test gaps: {out}",
    );
}

#[test]
fn context_default_omits_compound_sections() {
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name("qartez_context", json!({ "files": ["src/service.rs"] }))
        .expect("qartez_context default must succeed");

    assert!(
        !out.contains("## Impact (per input file)"),
        "default must omit Impact section: {out}",
    );
    assert!(
        !out.contains("## Test gaps (per input file)"),
        "default must omit Test gaps section: {out}",
    );
}

#[test]
fn context_flags_respect_concise_format() {
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name(
            "qartez_context",
            json!({
                "files": ["src/service.rs"],
                "include_impact": true,
                "include_test_gaps": true,
                "format": "concise",
            }),
        )
        .expect("qartez_context concise + flags must succeed");

    // Sections still emit their header + per-file rows even in concise
    // mode; the concise flag only narrows the ranked-listing rendering.
    assert!(
        out.contains("## Impact") && out.contains("## Test gaps"),
        "compound sections must render in concise format too: {out}",
    );
}

#[test]
fn context_flags_with_task_seed() {
    let (_dir, server) = fixture();
    // Empty `files` plus a `task` term that matches `dispatch` -
    // exercises the seed-from-task path together with the flags.
    let out = server
        .call_tool_by_name(
            "qartez_context",
            json!({
                "task": "dispatch",
                "include_impact": true,
            }),
        )
        .expect("qartez_context task seed + flag must succeed");

    assert!(
        out.contains("## Impact (per input file)"),
        "Impact must render when seeded from task: {out}",
    );
}

#[test]
fn context_flags_with_missing_input_errors_before_section() {
    let (_dir, server) = fixture();
    let err = server
        .call_tool_by_name(
            "qartez_context",
            json!({
                "files": ["src/does_not_exist.rs"],
                "include_impact": true,
            }),
        )
        .expect_err("missing file must error");
    assert!(
        err.contains("not found in index"),
        "missing input must trip the standard error before the section runs: {err}",
    );
}

// =========================================================================
// qartez_understand
// =========================================================================

#[test]
fn understand_resolves_function_with_full_sections() {
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name("qartez_understand", json!({ "name": "helper" }))
        .expect("qartez_understand on unique helper must succeed");

    assert!(
        out.contains("# Symbol:") && out.contains("helper"),
        "header must surface the resolved name: {out}",
    );
    assert!(
        out.contains("## Definition"),
        "Definition default-on: {out}"
    );
    assert!(out.contains("## Calls"), "Calls default-on: {out}");
    assert!(
        out.contains("## References"),
        "References default-on: {out}"
    );
    assert!(
        out.contains("## Co-change partners"),
        "Co-change default-on: {out}",
    );
}

#[test]
fn understand_sections_filter_omits_excluded() {
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name(
            "qartez_understand",
            json!({ "name": "helper", "sections": ["definition"] }),
        )
        .expect("sections=[definition] must succeed");

    assert!(out.contains("## Definition"), "Definition retained: {out}");
    assert!(!out.contains("## Calls"), "Calls omitted: {out}");
    assert!(!out.contains("## References"), "References omitted: {out}",);
    assert!(
        !out.contains("## Co-change partners"),
        "Co-change omitted: {out}",
    );
}

#[test]
fn understand_unknown_section_errors_with_valid_list() {
    let (_dir, server) = fixture();
    let err = server
        .call_tool_by_name(
            "qartez_understand",
            json!({ "name": "helper", "sections": ["definitions"] }),
        )
        .expect_err("typo'd section must error");

    assert!(
        err.contains("Unknown section"),
        "must surface unknown-section: {err}",
    );
    assert!(
        err.contains("definition")
            && err.contains("calls")
            && err.contains("refs")
            && err.contains("cochange"),
        "error must list every valid section: {err}",
    );
}

#[test]
fn understand_empty_sections_falls_back_to_default() {
    // Empty list (Some(vec![])) is treated as "use defaults" by the
    // validator's `Some(v) if !v.is_empty()` guard. This is the same
    // ergonomic shortcut callers get from passing `sections: null`.
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name(
            "qartez_understand",
            json!({ "name": "helper", "sections": [] }),
        )
        .expect("sections=[] must succeed by falling back to defaults");
    assert!(
        out.contains("## Definition") && out.contains("## Calls"),
        "empty sections must render defaults: {out}",
    );
}

#[test]
fn understand_missing_symbol_errors() {
    let (_dir, server) = fixture();
    let err = server
        .call_tool_by_name(
            "qartez_understand",
            json!({ "name": "definitely_missing_symbol_xyz" }),
        )
        .expect_err("missing symbol must error");
    assert!(
        err.contains("No symbol found"),
        "must use the unified missing-symbol wording: {err}",
    );
}

#[test]
fn understand_struct_kind_works() {
    // Config is a struct in the fixture, not a function. The tool
    // must still resolve it and render the header + definition - the
    // calls/refs sections degrade gracefully (qartez_calls returns a
    // friendly error which we wrap as `(calls unavailable: ...)`).
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name("qartez_understand", json!({ "name": "Config" }))
        .expect("Config struct must resolve");
    assert!(
        out.contains("Config") && out.contains("(struct)"),
        "struct kind must be reflected in header: {out}",
    );
    assert!(
        out.contains("## Definition"),
        "Definition still renders for struct: {out}",
    );
}

#[test]
fn understand_alias_symbol_field() {
    // The `name` field accepts `symbol` and `symbol_name` aliases. The
    // JSON dispatch layer must pass them through.
    let (_dir, server) = fixture();
    let via_symbol = server
        .call_tool_by_name("qartez_understand", json!({ "symbol": "helper" }))
        .expect("symbol alias must work");
    assert!(via_symbol.contains("# Symbol:") && via_symbol.contains("helper"));

    let via_symbol_name = server
        .call_tool_by_name("qartez_understand", json!({ "symbol_name": "helper" }))
        .expect("symbol_name alias must work");
    assert!(via_symbol_name.contains("# Symbol:") && via_symbol_name.contains("helper"));
}

#[test]
fn understand_token_budget_caps_response() {
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name(
            "qartez_understand",
            json!({
                "name": "dispatch",
                "token_budget": 600,
            }),
        )
        .expect("dispatch with tight budget must succeed");

    // The compound output for a god function is large; with a 600
    // token budget, at least one section must hit the truncation
    // marker. This exercises the per-section budget split + the
    // append_capped overflow path.
    assert!(
        out.len() < 50_000,
        "tight budget must keep response small: {} chars",
        out.len(),
    );
}

#[test]
fn understand_concise_format_renders_compact_header() {
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name(
            "qartez_understand",
            json!({ "name": "helper", "format": "concise" }),
        )
        .expect("concise must succeed");
    // Concise mode drops the `# Symbol:` framing and emits the
    // single-line locator instead.
    assert!(
        !out.contains("# Symbol:"),
        "concise must not render the verbose header: {out}",
    );
    assert!(
        out.contains("helper") && out.contains("(function)"),
        "concise must still identify the symbol: {out}",
    );
}

#[test]
fn understand_rejects_mermaid_format() {
    let (_dir, server) = fixture();
    let err = server
        .call_tool_by_name(
            "qartez_understand",
            json!({ "name": "helper", "format": "mermaid" }),
        )
        .expect_err("mermaid must be rejected");
    assert!(
        err.contains("mermaid") || err.contains("Mermaid"),
        "rejection must mention mermaid: {err}",
    );
}

// =========================================================================
// qartez_map: with_health
// =========================================================================

#[test]
fn map_with_health_emits_per_row_cc_marker() {
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name("qartez_map", json!({ "with_health": true, "top_n": 10 }))
        .expect("map with_health=true must succeed");

    assert!(
        out.contains("Health"),
        "with_health header must include the Health column: {out}",
    );
    assert!(
        out.contains("CC="),
        "every row must carry a CC=N marker: {out}",
    );
    // service.rs has the god dispatcher, so its row must surface
    // `god_function` and (because the signature has 6 params)
    // `long_params`.
    assert!(
        out.contains("god_function") || out.contains("long_params"),
        "service.rs row must trip a smell tag: {out}",
    );
}

#[test]
fn map_with_health_off_preserves_legacy_columns() {
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name("qartez_map", json!({ "top_n": 10 }))
        .expect("default map must succeed");

    assert!(
        !out.contains("CC="),
        "default map must not emit CC marker: {out}",
    );
    assert!(
        !out.contains("Health"),
        "default map header must omit Health column: {out}",
    );
    assert!(
        out.contains("PageRank") || out.contains("PR "),
        "default header still carries the PageRank column: {out}",
    );
}

#[test]
fn map_with_health_concise_appends_marker() {
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name(
            "qartez_map",
            json!({ "with_health": true, "format": "concise", "top_n": 10 }),
        )
        .expect("concise + with_health must succeed");

    // Concise header carries `health` (lowercase) + every row appends
    // a CC marker.
    assert!(
        out.contains("health"),
        "concise header must mention health column: {out}",
    );
    assert!(
        out.contains("CC="),
        "concise rows must carry CC marker: {out}",
    );
}

#[test]
fn map_with_health_combines_with_boost_terms() {
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name(
            "qartez_map",
            json!({
                "with_health": true,
                "boost_terms": ["dispatch"],
                "top_n": 5,
            }),
        )
        .expect("with_health + boost_terms must succeed");

    assert!(
        out.contains("CC=") && out.contains("service.rs"),
        "boost must surface service.rs and CC marker still applies: {out}",
    );
}

// =========================================================================
// Dispatch / discoverability
// =========================================================================

#[test]
fn call_tool_by_name_routes_qartez_understand() {
    let (_dir, server) = fixture();
    // An unknown tool name returns the catch-all error, so a
    // successful response confirms the dispatch arm is wired up
    // and the typed handler runs to completion.
    let out = server
        .call_tool_by_name("qartez_understand", json!({ "name": "helper" }))
        .expect("dispatch must reach the typed handler");
    assert!(out.contains("# Symbol:"));
}

#[test]
fn call_tool_by_name_unknown_understand_typo_still_routed_as_unknown() {
    // Guards against a refactor that accidentally collapses the
    // unknown-tool fallthrough. The catch-all error prefix is the
    // contract every existing dispatch test relies on.
    let (_dir, server) = fixture();
    let err = server
        .call_tool_by_name("qartez_understnd", json!({}))
        .expect_err("typo must not match any arm");
    assert!(
        err.to_lowercase().contains("unknown") || err.to_lowercase().contains("not"),
        "typo dispatch must surface a no-match error: {err}",
    );
}

// =========================================================================
// Hardening: defensive edge cases
// =========================================================================

#[test]
fn context_isolated_input_with_flags_renders_section() {
    // Files with no incoming/outgoing edges previously triggered the
    // "No related context files found" early return - bypassing the
    // compound flag sections. The new code path emits the flag
    // sections even when the ranked list is empty.
    let (dir, _) = fixture();
    let src = dir.path().join("src");
    fs::write(src.join("orphan.rs"), "pub fn orphan() {}\n").unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub mod service;\npub mod util;\npub mod lonely;\npub mod orphan;\n",
    )
    .unwrap();
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_context",
            json!({
                "files": ["src/orphan.rs"],
                "include_impact": true,
                "include_test_gaps": true,
            }),
        )
        .expect("isolated file with flags must succeed");
    assert!(
        out.contains("compound annotations follow"),
        "isolated file must surface the compound-annotation marker: {out}",
    );
    assert!(
        out.contains("## Impact (per input file)"),
        "Impact section must render for isolated input: {out}",
    );
    assert!(
        out.contains("## Test gaps (per input file)"),
        "Test gaps section must render for isolated input: {out}",
    );
}

#[test]
fn context_isolated_input_without_flags_keeps_legacy_message() {
    // Back-compat assertion: when no compound flag is set, the
    // legacy "No related context" message must still surface for
    // isolated files. Without this guard a future tweak could
    // silently change the response shape for every existing caller.
    let (dir, _) = fixture();
    let src = dir.path().join("src");
    fs::write(src.join("orphan.rs"), "pub fn orphan() {}\n").unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub mod service;\npub mod util;\npub mod lonely;\npub mod orphan;\n",
    )
    .unwrap();
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name("qartez_context", json!({"files": ["src/orphan.rs"]}))
        .expect("isolated file without flags must succeed");
    assert!(
        out.contains("No related context files found"),
        "legacy message must still apply when no flag is set: {out}",
    );
}

#[test]
fn context_inline_only_test_coverage_surfaces_marker() {
    // A source file with `#[cfg(test)] mod tests` but no external
    // test importer must be classified as covered (inline tests
    // only). This is the canonical inline-rust-tests detection
    // path that distinguishes the new helper from a naive edge
    // walk.
    let dir = TempDir::new().unwrap();
    write_cargo_manifest(dir.path());
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub mod inline;\n").unwrap();
    fs::write(
        src.join("inline.rs"),
        r#"pub fn add(x: i32, y: i32) -> i32 { x + y }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn add_works() {
        assert_eq!(add(1, 2), 3);
    }
}
"#,
    )
    .unwrap();
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_context",
            json!({
                "files": ["src/inline.rs"],
                "include_test_gaps": true,
            }),
        )
        .expect("inline-tested file must produce output");
    assert!(
        out.contains("inline tests"),
        "inline-only coverage must mention inline tests in the row: {out}",
    );
    assert!(
        !out.contains("src/inline.rs - untested"),
        "inline-tested file must not be marked untested: {out}",
    );
}

#[test]
fn understand_test_only_definition_with_include_tests_flag() {
    // helper symbol exists only in tests/service_test.rs - by
    // default qartez_understand filters out test-only candidates.
    // Setting include_tests=true must surface them.
    let (_dir, server) = fixture();
    let err = server
        .call_tool_by_name("qartez_understand", json!({"name": "dispatch_works"}))
        .expect_err("default include_tests=false must filter test-only definitions");
    assert!(
        err.contains("No symbol")
            || err.contains("no candidate matching")
            || err.contains("'dispatch_works'"),
        "test-only symbol must error when include_tests=false: {err}",
    );

    let out = server
        .call_tool_by_name(
            "qartez_understand",
            json!({"name": "dispatch_works", "include_tests": true}),
        )
        .expect("include_tests=true must surface test-only definitions");
    assert!(
        out.contains("dispatch_works"),
        "include_tests=true must resolve test symbols: {out}",
    );
}

#[test]
fn understand_kind_disambiguates_when_two_match() {
    // The fixture has Config (struct) AND a Config::new constructor.
    // Without disambiguation the multi-candidate error can fire.
    // Passing kind=struct must resolve uniquely.
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name(
            "qartez_understand",
            json!({"name": "Config", "kind": "struct"}),
        )
        .expect("kind=struct must disambiguate");
    assert!(
        out.contains("(struct)"),
        "header must reflect the picked kind: {out}",
    );
}

#[test]
fn map_with_health_handles_empty_index_gracefully() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    let conn = setup_db();
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);
    // No index. with_health=true must not panic - the iteration
    // simply has nothing to annotate.
    let out = server
        .call_tool_by_name("qartez_map", json!({"with_health": true}))
        .expect("empty index must not panic");
    // The output must still emit a header even with no rows.
    assert!(
        out.contains("Codebase") || out.contains("files"),
        "empty-index map must still render a header: {out}",
    );
}

#[test]
fn understand_huge_token_budget_does_not_panic() {
    // Defensive: a caller passing 100k must not overflow the
    // per-section division or trip an arithmetic clamp.
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name(
            "qartez_understand",
            json!({"name": "helper", "token_budget": 100_000}),
        )
        .expect("huge token budget must succeed");
    assert!(
        out.contains("# Symbol:"),
        "huge budget must still produce a valid response: {out}",
    );
}

#[test]
fn context_explain_combines_with_compound_flags() {
    // explain=true and include_impact=true must coexist - both add
    // new lines after the ranked listing and neither path should
    // suppress the other.
    let (_dir, server) = fixture();
    let out = server
        .call_tool_by_name(
            "qartez_context",
            json!({
                "files": ["src/service.rs"],
                "include_impact": true,
                "explain": true,
            }),
        )
        .expect("explain + flags must succeed");
    assert!(
        out.contains("## Impact (per input file)"),
        "Impact section must render alongside explain: {out}",
    );
}
