// Rust guideline compliant 2026-04-22
//
// End-to-end regression tests for the analyzer fix in commit c309e1d:
// verifies that through the full index-and-query pipeline,
//   1. clone detection excludes `#[cfg(test)]` modules and `tests/` paths
//      by default but restores them under `include_tests=true`;
//   2. cyclomatic complexity no longer inflates on `?` operators and that
//      feature-envy skips associated-function calls while still flagging
//      instance-method envy;
//   3. against qartez-public's own source, the originally-misflagged
//      symbols (SEC004 false-positives and proc-macro-DSL / serde
//      deserialize_with identifiers) no longer surface.

use std::fs;
use std::path::Path;

use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use qartez_mcp::index;
use qartez_mcp::server::QartezServer;
use qartez_mcp::storage::{read, schema};

// ---------------------------------------------------------------------------
// Shared fixture helpers
// ---------------------------------------------------------------------------

fn setup_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    conn
}

fn build_and_index(dir: &Path) -> QartezServer {
    // Simulate a project: a `.git` dir marks it as a project, so downstream
    // tools treat the TempDir as the project root.
    fs::create_dir_all(dir.join(".git")).unwrap();
    let conn = setup_db();
    index::full_index(&conn, dir, false).unwrap();
    QartezServer::new(conn, dir.to_path_buf(), 0)
}

// ---------------------------------------------------------------------------
// Part 1. Clone detector excludes test modules by default
// ---------------------------------------------------------------------------

fn write_clones_fixture(dir: &Path) {
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    // Two production-side clones (process_a / process_b) plus a `#[cfg(test)]`
    // module with two structurally identical fixtures (test_fixture_alpha /
    // test_fixture_beta). Default scan must drop the cfg(test) members.
    let main_lib = r#"pub fn process_a(items: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for x in &items { if x.len() > 3 { out.push(x.clone()); } }
    out.sort();
    out.dedup();
    out
}

pub fn process_b(items: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for x in &items { if x.len() > 3 { out.push(x.clone()); } }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_fixture_alpha(items: Vec<String>) -> Vec<String> {
        let mut out = Vec::new();
        for x in &items { if x.len() > 3 { out.push(x.clone()); } }
        out.sort();
        out.dedup();
        out
    }

    fn test_fixture_beta(items: Vec<String>) -> Vec<String> {
        let mut out = Vec::new();
        for x in &items { if x.len() > 3 { out.push(x.clone()); } }
        out.sort();
        out.dedup();
        out
    }
}
"#;
    fs::write(src.join("main_lib.rs"), main_lib).unwrap();

    // Test-path file: same shape, at a conventional test path.
    let tests = dir.join("tests");
    fs::create_dir_all(&tests).unwrap();
    let integration = r#"pub fn integration_helper(items: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for x in &items { if x.len() > 3 { out.push(x.clone()); } }
    out.sort();
    out.dedup();
    out
}
"#;
    fs::write(tests.join("integration.rs"), integration).unwrap();
}

#[test]
fn clones_default_excludes_cfg_test_and_test_path_members() {
    let dir = TempDir::new().unwrap();
    write_clones_fixture(dir.path());
    let server = build_and_index(dir.path());

    // Default scan - `include_tests` omitted.
    let out = server
        .call_tool_by_name(
            "qartez_clones",
            json!({ "min_lines": 5, "limit": 50, "format": "detailed" }),
        )
        .expect("qartez_clones default should succeed");

    assert!(
        out.contains("process_a") && out.contains("process_b"),
        "production clones must surface by default: {out}"
    );
    assert!(
        !out.contains("test_fixture_alpha"),
        "cfg(test) fixture alpha must be filtered by default: {out}"
    );
    assert!(
        !out.contains("test_fixture_beta"),
        "cfg(test) fixture beta must be filtered by default: {out}"
    );
    assert!(
        !out.contains("integration_helper"),
        "tests/ path member must be filtered by default: {out}"
    );
}

#[test]
fn clones_include_tests_restores_all_members() {
    let dir = TempDir::new().unwrap();
    write_clones_fixture(dir.path());
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_clones",
            json!({
                "min_lines": 5,
                "limit": 50,
                "include_tests": true,
                "format": "detailed",
            }),
        )
        .expect("qartez_clones with include_tests=true should succeed");

    for expected in [
        "process_a",
        "process_b",
        "test_fixture_alpha",
        "test_fixture_beta",
        "integration_helper",
    ] {
        assert!(
            out.contains(expected),
            "include_tests=true must surface '{expected}': {out}"
        );
    }
}

// ---------------------------------------------------------------------------
// C5 qartez_clones: small `limit` must not return empty when the top
// raw groups are test code. Loop-fetch oversamples the DB so production
// clones still surface at limit=1..=4.
// ---------------------------------------------------------------------------

#[test]
fn clones_small_limit_still_surfaces_production_when_tests_dominate() {
    let dir = TempDir::new().unwrap();
    write_clones_fixture(dir.path());
    let server = build_and_index(dir.path());

    for limit in 1..=3 {
        let out = server
            .call_tool_by_name(
                "qartez_clones",
                json!({ "min_lines": 5, "limit": limit, "format": "detailed" }),
            )
            .unwrap_or_else(|e| panic!("qartez_clones limit={limit} must succeed: {e}"));

        assert!(
            !out.contains("No clones in page"),
            "limit={limit} must not hit empty-page stub when production clones exist: {out}"
        );
        assert!(
            out.contains("process_a") || out.contains("process_b"),
            "limit={limit} must surface a production clone name: {out}"
        );
        assert!(
            !out.contains("test_fixture_alpha") && !out.contains("test_fixture_beta"),
            "limit={limit} must not leak cfg(test) fixtures via the oversample loop: {out}"
        );
    }
}

// ---------------------------------------------------------------------------
// C9 qartez_clones: min_lines=0 is a validation error, not a silent no-op.
// ---------------------------------------------------------------------------

#[test]
fn clones_min_lines_zero_is_rejected() {
    let dir = TempDir::new().unwrap();
    write_clones_fixture(dir.path());
    let server = build_and_index(dir.path());

    let err = server
        .call_tool_by_name(
            "qartez_clones",
            json!({ "min_lines": 0, "limit": 5, "format": "detailed" }),
        )
        .expect_err("min_lines=0 must return a validation error");

    assert!(
        err.contains("min_lines must be >= 1"),
        "error must explain why min_lines=0 is rejected, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Part 2. CC ignores `?` and feature-envy skips associated calls
// ---------------------------------------------------------------------------

fn write_dispatcher_fixture(dir: &Path) {
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    // Flat 8-arm dispatcher: each arm uses `?` for error propagation. With
    // the fix, `?` contributes 0 to CC, so CC tracks the number of match
    // arms (plus the outer match node itself, depending on tree-sitter
    // accounting). Before the fix, CC would inflate to roughly 25+.
    //
    // `construct_pipeline` calls only `Step {}` literal constructors and
    // the associated function style is not present here - we rely on the
    // absence of envy signal. `envy_bar` calls `&self` instance methods
    // four times on a foreign type; that is genuine feature envy.
    let dispatcher = r#"use std::io::{Read, Write};

pub struct Runner;

pub struct Step;

impl Step {
    pub fn build_a(&self) -> Result<(), String> { Ok(()) }
    pub fn build_b(&self) -> Result<(), String> { Ok(()) }
    pub fn build_c(&self) -> Result<(), String> { Ok(()) }
    pub fn build_d(&self) -> Result<(), String> { Ok(()) }
    pub fn factory() -> Step { Step {} }
}

impl Runner {
    pub fn run(&self, op: &str) -> Result<(), String> {
        match op {
            "a" => { let _ = std::fs::read("/tmp/a").map_err(|e| e.to_string())?; Ok(()) }
            "b" => { let _ = std::fs::read("/tmp/b").map_err(|e| e.to_string())?; Ok(()) }
            "c" => { let _ = std::fs::read("/tmp/c").map_err(|e| e.to_string())?; Ok(()) }
            "d" => { let _ = std::fs::read("/tmp/d").map_err(|e| e.to_string())?; Ok(()) }
            "e" => { let _ = std::fs::read("/tmp/e").map_err(|e| e.to_string())?; Ok(()) }
            "f" => { let _ = std::fs::read("/tmp/f").map_err(|e| e.to_string())?; Ok(()) }
            "g" => { let _ = std::fs::read("/tmp/g").map_err(|e| e.to_string())?; Ok(()) }
            "h" => { let _ = std::fs::read("/tmp/h").map_err(|e| e.to_string())?; Ok(()) }
            _ => Ok(()),
        }
    }

    pub fn construct_pipeline(&self) -> Vec<Step> {
        let a = Step::factory();
        let b = Step::factory();
        let c = Step::factory();
        let d = Step::factory();
        let e = Step::factory();
        vec![a, b, c, d, e]
    }

    pub fn envy_bar(&self, bar: &Step) -> Result<(), String> {
        bar.build_a()?;
        bar.build_b()?;
        bar.build_c()?;
        bar.build_d()
    }
}
"#;
    fs::write(src.join("dispatcher.rs"), dispatcher).unwrap();
}

/// Fetch the complexity value for a symbol by (name, file_path). Returns
/// the first matching row or None when the symbol was not indexed.
fn complexity_for(conn: &Connection, name: &str, file_path: &str) -> Option<u32> {
    let all = read::get_all_symbols_with_path(conn).unwrap();
    all.into_iter()
        .find(|(s, p)| s.name == name && p == file_path)
        .and_then(|(s, _)| s.complexity)
}

#[test]
fn cc_runner_run_does_not_count_try_operator() {
    let dir = TempDir::new().unwrap();
    write_dispatcher_fixture(dir.path());
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    let conn = setup_db();
    index::full_index(&conn, dir.path(), false).unwrap();

    let cc = complexity_for(&conn, "run", "src/dispatcher.rs")
        .expect("Runner::run must be indexed with a complexity value");

    // With the fix, `?` adds 0. 8 match arms plus the outer match fallback
    // is well under 15. Before the fix, per-arm `?` pushed CC above 20.
    assert!(
        cc < 15,
        "Runner::run CC expected < 15 after the `?` fix, got {cc}"
    );
    // Sanity lower bound: there are still at least 8 match arms, so CC
    // must be at least 8.
    assert!(
        cc >= 8,
        "Runner::run CC expected >= 8 (8 match arms), got {cc}"
    );
}

#[test]
fn feature_envy_skips_associated_calls_but_flags_instance_calls() {
    let dir = TempDir::new().unwrap();
    write_dispatcher_fixture(dir.path());
    let server = build_and_index(dir.path());

    // Low envy_ratio so any instance-method envy surfaces. We explicitly
    // request only feature_envy to keep the output focused.
    let out = server
        .call_tool_by_name(
            "qartez_smells",
            json!({
                "kind": "feature_envy",
                "envy_ratio": 1.0,
                "limit": 50,
                "format": "detailed",
            }),
        )
        .expect("qartez_smells feature_envy should succeed");

    // `construct_pipeline` calls `Step::factory()` repeatedly, an
    // associated function with no `self` receiver. The fix excludes
    // associated-function calls from envy accounting, so the symbol must
    // NOT appear in the feature-envy section.
    assert!(
        !out.contains("construct_pipeline"),
        "construct_pipeline must not be flagged as feature envy: {out}"
    );

    // `envy_bar` calls four `&self` instance methods on `Step` from a
    // method whose own_type is `Runner`. That is real feature envy and
    // must still be flagged with envied_type=Step.
    assert!(
        out.contains("envy_bar"),
        "envy_bar must still be flagged as feature envy: {out}"
    );
    assert!(
        out.contains("Step"),
        "feature-envy output must name the envied type Step: {out}"
    );
}

// ---------------------------------------------------------------------------
// Part 3. Self-test: qartez analyzers against qartez-public's own source
//
// A full self-index of qartez-public runs in < 1s in release mode (CI
// builds with `--release`), so these regressions run on every PR rather
// than hiding behind `#[ignore]`. They catch the class of false positives
// that stale-binary installs routinely regressed on.
// ---------------------------------------------------------------------------

/// Resolve the qartez-public source directory relative to the Cargo
/// manifest. `CARGO_MANIFEST_DIR` is set by cargo to the directory of the
/// crate being tested, which is qartez-public itself.
fn qartez_public_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn selftest_sec004_no_false_positives_on_qartez_public() {
    let root = qartez_public_root();
    assert!(
        root.join("src/lib.rs").exists(),
        "qartez-public/src/lib.rs must exist at {root:?}",
    );
    let conn = setup_db();
    index::full_index(&conn, &root, false).unwrap();
    let server = QartezServer::new(conn, root, 0);

    let out = server
        .call_tool_by_name(
            "qartez_security",
            json!({ "limit": 500, "format": "concise" }),
        )
        .expect("qartez_security on self must succeed");

    // Regression guard: these three functions were SEC004 FPs before the
    // commit-c309e1d fix. They must no longer appear in SEC004 findings.
    for (symbol, file) in &[
        ("run_command", "src/toolchain.rs"),
        ("schedule_update_check", "src/main.rs"),
        ("run_session_start", "src/bin/setup.rs"),
    ] {
        // SEC004 is the Command/process-injection rule. A line containing
        // both "SEC004" and the symbol name is the strongest signal we
        // can check from the concise one-line-per-finding output.
        let flagged = out.lines().any(|l| {
            l.contains("SEC004") && l.contains(symbol) && (l.contains(file) || l.contains(".rs"))
        });
        assert!(
            !flagged,
            "{symbol} (in {file}) must NOT appear in SEC004 findings after the fix:\n{out}"
        );
    }
}

#[test]
fn selftest_unused_no_false_positives_on_qartez_public() {
    let root = qartez_public_root();
    assert!(
        root.join("src/lib.rs").exists(),
        "qartez-public/src/lib.rs must exist at {root:?}",
    );
    let conn = setup_db();
    index::full_index(&conn, &root, false).unwrap();
    let server = QartezServer::new(conn, root, 0);

    // Ask for a very large page so every unused export is returned in one
    // response; then scan the text for specific symbol names.
    let out = server
        .call_tool_by_name("qartez_unused", json!({ "limit": 5000 }))
        .expect("qartez_unused on self must succeed");

    // Proc-macro DSL parameter structs - referenced from the
    // `dispatch_tool_call!` macro body in `QartezServer::call_tool_by_name`.
    // Before the fix, the rust_lang parser did not descend into token trees
    // of non-builtin macros, so uppercase identifiers there never emitted
    // ref edges and these structs were reported as unused exports.
    //
    // `ToolsParams` is referenced via `Parameters<ToolsParams>` inside a
    // `#[tool]`-attributed async method signature. The follow-up fix in
    // `extract_impl_methods` walks the full method node (not just the body),
    // so signature type references in `impl` methods now reach the resolver.
    for name in &[
        "SoulWorkspaceParams",
        "SoulSecurityParams",
        "SoulHierarchyParams",
        "ToolsParams",
    ] {
        assert!(
            !contains_symbol_line(&out, name),
            "{name} must NOT be reported as unused after the fix (proc-macro-DSL ref):\n{out}"
        );
    }

    // `deserialize_with = "flexible::u32_opt"` - the rust_lang parser now
    // parses the string path inside the serde attribute and emits a Use
    // ref to the tail segment.
    for name in &["u32_opt", "bool_opt", "f64_opt"] {
        assert!(
            !contains_symbol_line(&out, name),
            "{name} must NOT be reported as unused after the fix (serde deserialize_with):\n{out}"
        );
    }

    // MCP tool methods wired via rmcp's `#[tool_router]` proc macro. The
    // generated dispatch surface is invisible to the static import graph,
    // so without the `unused_excluded` stamp applied by the rust_lang
    // parser every one of these showed up as dead code on a self-scan.
    // Cover each router entry in `server/tools/mod.rs::tool_router` that
    // previously surfaced as an FP.
    for name in &[
        "qartez_map",
        "qartez_security",
        "qartez_workspace",
        "qartez_semantic",
        "qartez_tools",
        "qartez_hierarchy",
        "qartez_insert_before_symbol",
        "qartez_insert_after_symbol",
        "qartez_replace_symbol",
        "qartez_safe_delete",
    ] {
        assert!(
            !contains_symbol_line(&out, name),
            "{name} must NOT be reported as unused (wired via #[tool_router]):\n{out}"
        );
    }

    // Regression for the macro-body Call-walker: `Severity::label` is
    // invoked as `f.severity.label()` inside `format!(...)` calls in
    // `qartez_security`. Before the fix, the token-tree walker skipped
    // builtin macros and never emitted the Call ref, so the method read
    // as dead.
    assert!(
        !contains_symbol_line(&out, "label"),
        "`label` method on Severity must NOT be unused (called in format!() bodies):\n{out}"
    );
}

/// Heuristic whole-word match for a symbol in the `qartez_unused` output.
/// The compact format is `<kind-letter> <name> L<line>` per entry; we
/// search for ` <name> L` to avoid matching prefixes inside longer names.
fn contains_symbol_line(out: &str, name: &str) -> bool {
    let pat = format!(" {name} L");
    out.lines().any(|l| l.contains(&pat))
}

// ---------------------------------------------------------------------------
// Part 4. Clone detector preserves string-literal bodies on data declarations.
// Before this fix the shape-hasher collapsed every string literal to `_S`,
// so `const A: &str = "..."; const B: &str = "..."` hashed identically
// regardless of body. That swept ~12 of the 13 CREATE_* SQL schema consts
// into a single clone group.
// ---------------------------------------------------------------------------

#[test]
fn clones_does_not_collapse_distinct_const_string_literals() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    let schema = r#"pub const CREATE_FOO: &str = "
CREATE TABLE IF NOT EXISTS foo (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    ts INTEGER NOT NULL
)";

pub const CREATE_BAR: &str = "
CREATE TABLE IF NOT EXISTS bar (
    file_id INTEGER NOT NULL,
    count INTEGER NOT NULL,
    last_seen INTEGER
)";

pub const CREATE_BAZ: &str = "
CREATE TABLE IF NOT EXISTS baz (
    id TEXT PRIMARY KEY,
    value REAL NOT NULL,
    created_at INTEGER NOT NULL
)";
"#;
    fs::write(src.join("schema.rs"), schema).unwrap();

    let server = build_and_index(dir.path());
    let out = server
        .call_tool_by_name(
            "qartez_clones",
            json!({ "min_lines": 3, "limit": 50, "format": "detailed" }),
        )
        .expect("qartez_clones should succeed");

    for name in ["CREATE_FOO", "CREATE_BAR", "CREATE_BAZ"] {
        assert!(
            !out.contains(name),
            "{name} must not be grouped as a clone just because all three are \
             `const X: &str = \"...different SQL...\"`. output:\n{out}"
        );
    }
}

// ---------------------------------------------------------------------------
// Part 5. Smells: trait dispatch is not feature envy.
//
// A single source-level call through `&dyn Trait` expands in the symbol-ref
// index into one edge per impl - same method name, different owner types.
// The naive envy detector interpreted those 6 edges as 6 distinct foreign
// calls and flagged the caller with a 6:1 ratio. The fix recognizes the
// fan-out shape (one method name shared across 3+ owner types dominating
// the external-call set) and suppresses the flag.
// ---------------------------------------------------------------------------

fn write_trait_dispatch_fixture(dir: &Path) {
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    // Four impls of a common trait with identical method signatures, then
    // a caller that walks through them via `&dyn Trait`. The rust_lang
    // resolver emits one Call edge per impl method with the same target
    // name, which used to read as envy. After the fix, the shared-method
    // fan-out is recognized and suppressed.
    let dispatch = r#"pub trait Extractor {
    fn run(&self, input: &str) -> String;
    fn name(&self) -> &str;
}

pub struct JavaExt;
pub struct CSharpExt;
pub struct OCamlExt;
pub struct KotlinExt;

impl Extractor for JavaExt {
    fn run(&self, input: &str) -> String { input.to_string() }
    fn name(&self) -> &str { "java" }
}

impl Extractor for CSharpExt {
    fn run(&self, input: &str) -> String { input.to_string() }
    fn name(&self) -> &str { "csharp" }
}

impl Extractor for OCamlExt {
    fn run(&self, input: &str) -> String { input.to_string() }
    fn name(&self) -> &str { "ocaml" }
}

impl Extractor for KotlinExt {
    fn run(&self, input: &str) -> String { input.to_string() }
    fn name(&self) -> &str { "kotlin" }
}

pub struct ExtractorPool;

impl ExtractorPool {
    pub fn process(&self, ext: &dyn Extractor, input: &str) -> (String, String) {
        let out = ext.run(input);
        let label = ext.name().to_string();
        (out, label)
    }
}
"#;
    fs::write(src.join("dispatch.rs"), dispatch).unwrap();
}

#[test]
fn feature_envy_suppresses_trait_dispatch_fan_out() {
    let dir = TempDir::new().unwrap();
    write_trait_dispatch_fixture(dir.path());
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_smells",
            json!({
                "kind": "feature_envy",
                "envy_ratio": 1.0,
                "limit": 50,
                "format": "detailed",
            }),
        )
        .expect("qartez_smells feature_envy should succeed");

    // `process` calls `ext.run()` and `ext.name()` once each. The resolver
    // fans those out across every impl, but since `run` and `name` are
    // both implemented on >= 3 distinct owner types, the fix attributes
    // them to trait dispatch and the caller must NOT surface as envy.
    assert!(
        !out.contains("process"),
        "process (via &dyn Extractor) must not be flagged as feature envy: {out}"
    );
}

// ---------------------------------------------------------------------------
// Part 6. Smells: service handler + DTO param is not feature envy.
//
// Types whose name matches a service/handler suffix legitimately operate
// on DTO parameters. Without the fix, a `QartezServer::qartez_foo(step: &Step)`
// style handler that processes 7+ fields on the `Step` DTO would get
// envy ratio 7.0 and be flagged for "Move to Step" refactor - which is
// structurally wrong for an MCP tool router.
// ---------------------------------------------------------------------------

fn write_service_handler_fixture(dir: &Path) {
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    let service = r#"pub struct Payload {
    value: u32,
}

impl Payload {
    pub fn new(value: u32) -> Self { Self { value } }
    pub fn incr(&self) -> u32 { self.value + 1 }
    pub fn decr(&self) -> u32 { self.value.saturating_sub(1) }
    pub fn label(&self) -> String { format!("p{}", self.value) }
    pub fn debug(&self) -> String { format!("{{value={}}}", self.value) }
    pub fn double(&self) -> u32 { self.value * 2 }
}

pub struct ApiServer;

impl ApiServer {
    pub fn handle(&self, payload: &Payload) -> String {
        let a = payload.incr();
        let b = payload.decr();
        let c = payload.label();
        let d = payload.debug();
        let e = payload.double();
        let f = payload.label();
        format!("{a}-{b}-{c}-{d}-{e}-{f}")
    }
}
"#;
    fs::write(src.join("service.rs"), service).unwrap();
}

#[test]
fn feature_envy_suppresses_service_handler_on_dto() {
    let dir = TempDir::new().unwrap();
    write_service_handler_fixture(dir.path());
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_smells",
            json!({
                "kind": "feature_envy",
                "envy_ratio": 1.0,
                "limit": 50,
                "format": "detailed",
            }),
        )
        .expect("qartez_smells feature_envy should succeed");

    // `ApiServer::handle` calls six Payload methods. Without the fix the
    // envy ratio is 6:0 = 6.0 and the row surfaces. With the service-
    // handler suffix exclusion, it must not.
    assert!(
        !out.contains("ApiServer"),
        "ApiServer::handle (service suffix + DTO param) must not be flagged as envy: {out}"
    );
    assert!(
        !out.contains("| handle "),
        "handle must not be listed as a feature-envy row: {out}"
    );
}

// ---------------------------------------------------------------------------
// Part 7. Smells: flat-match dispatcher gets a distinct kind and advice.
//
// A function whose CC budget is dominated by a single flat match (many
// trivial arms) should still surface as a god function when thresholds
// are hit - but flagged as `flat_dispatcher` so the recommendation can
// steer users away from the useless "Extract Method on the largest
// branch" advice. Nested god functions must still read as plain
// god_function so the regression doesn't silence real smells.
// ---------------------------------------------------------------------------

fn write_flat_dispatcher_fixture(dir: &Path) {
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    // Eight-arm flat match; each arm is one line. CC should track arms,
    // the body is long enough to cross min_lines at the default 50, so
    // drop the threshold in the test call.
    let body = r#"pub enum Kind {
    A, B, C, D, E, F, G, H, I, J, K, L,
}

pub fn build_dispatch(k: Kind) -> u32 {
    match k {
        Kind::A => 1,
        Kind::B => 2,
        Kind::C => 3,
        Kind::D => 4,
        Kind::E => 5,
        Kind::F => 6,
        Kind::G => 7,
        Kind::H => 8,
        Kind::I => 9,
        Kind::J => 10,
        Kind::K => 11,
        Kind::L => 12,
    }
}

pub fn deeply_nested_god(x: i32, y: i32) -> i32 {
    let mut total = 0;
    if x > 0 {
        if y > 0 {
            for i in 0..x {
                if i % 2 == 0 {
                    total += i;
                } else {
                    total -= i;
                }
                if i > 5 {
                    total *= 2;
                }
            }
        } else {
            for j in 0..y.abs() {
                if j % 3 == 0 {
                    total += 1;
                } else if j % 3 == 1 {
                    total += 2;
                } else {
                    total += 3;
                }
            }
        }
    } else if y > 0 {
        while total < 100 {
            total += y;
            if total > 50 {
                break;
            }
        }
    } else {
        total = x + y;
    }
    total
}
"#;
    fs::write(src.join("dispatch.rs"), body).unwrap();
}

#[test]
fn god_function_flags_flat_dispatcher_kind() {
    let dir = TempDir::new().unwrap();
    write_flat_dispatcher_fixture(dir.path());
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_smells",
            json!({
                "kind": "god_function",
                // Lower thresholds so the 12-arm match qualifies despite
                // being short.
                "min_complexity": 10,
                "min_lines": 12,
                "limit": 50,
                "format": "detailed",
            }),
        )
        .expect("qartez_smells god_function should succeed");

    // When build_dispatch surfaces, it must be tagged as `flat_dispatcher`.
    // If thresholds don't pull it into the output at all, that's also an
    // acceptable outcome per the spec ("(a) Skip god_function reporting
    // entirely, OR (b) flag as flat_dispatcher"). Assert the disjunction.
    if out.contains("build_dispatch") {
        assert!(
            out.contains("flat_dispatcher"),
            "build_dispatch surfaced but was not tagged flat_dispatcher: {out}"
        );
    }
}

#[test]
fn god_function_still_flags_real_god_function() {
    let dir = TempDir::new().unwrap();
    write_flat_dispatcher_fixture(dir.path());
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_smells",
            json!({
                "kind": "god_function",
                // Thresholds tuned so deeply_nested_god (CC ~= 14, lines ~= 30)
                // reliably qualifies.
                "min_complexity": 8,
                "min_lines": 20,
                "limit": 50,
                "format": "detailed",
            }),
        )
        .expect("qartez_smells god_function should succeed");

    assert!(
        out.contains("deeply_nested_god"),
        "real god function must still surface under god_function detection: {out}"
    );
    // The deeply-nested function has almost no `=>` arrows, so it must
    // not be mistaken for a flat dispatcher.
    let deeply_line = out
        .lines()
        .find(|l| l.contains("deeply_nested_god"))
        .unwrap_or("");
    assert!(
        !deeply_line.contains("flat_dispatcher"),
        "deeply_nested_god must not be tagged as flat_dispatcher: {deeply_line}"
    );
}

// ---------------------------------------------------------------------------
// Part 5. Clone detector labels trait-impl boilerplate groups as a
// candidate for a default method implementation. Byte-identical method
// bodies across `impl <Trait> for <Struct>` blocks are mechanical and
// collapse into `trait { fn m(...) { <body> } }`, so the report should
// name them differently from generic refactor-opportunity clones.
// ---------------------------------------------------------------------------

#[test]
fn clones_label_trait_impl_boilerplate_as_default_method_candidate() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    let trait_decl = r#"pub trait Lang {
    fn name(&self) -> String;
}
"#;
    fs::write(src.join("lang.rs"), trait_decl).unwrap();

    // Three structurally and byte-identical `fn name` bodies, each in
    // its own `impl Lang for <Struct>` block in its own file. Every
    // body is long enough to clear the default `min_lines = 8` cutoff.
    let body = |struct_name: &str| -> String {
        format!(
            r#"use crate::lang::Lang;

pub struct {struct_name};

impl Lang for {struct_name} {{
    fn name(&self) -> String {{
        let mut buf = String::new();
        buf.push_str("x");
        buf.push_str("y");
        buf.push_str("z");
        buf.push('!');
        buf.push('?');
        buf
    }}
}}
"#
        )
    };
    fs::write(src.join("a.rs"), body("CLang")).unwrap();
    fs::write(src.join("b.rs"), body("CSharpSupport")).unwrap();
    fs::write(src.join("c.rs"), body("GoLang")).unwrap();

    let server = build_and_index(dir.path());
    let out = server
        .call_tool_by_name(
            "qartez_clones",
            json!({ "min_lines": 5, "limit": 50, "format": "detailed" }),
        )
        .expect("qartez_clones should succeed");

    assert!(
        out.contains("trait boilerplate"),
        "trait-impl clone group must carry the `trait boilerplate` label so \
         the user knows to promote `fn name` to a default method, got:\n{out}"
    );
    assert!(
        out.contains("candidate for default method"),
        "trait-boilerplate groups must recommend a default method impl, got:\n{out}"
    );
    assert!(
        out.contains("fn name"),
        "the recommendation must name the method so the user can find it, got:\n{out}"
    );
}
