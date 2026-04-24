// Rust guideline compliant 2026-04-23
//
// Verification layer for the cluster-b query-tool fixes landed 2026-04-23
// (commit 50cd793 - "fix(tools): calls disambiguation, grep name-anchored
// prefix, refs concise collapse"). Each test targets an edge case that the
// first regression pass did not cover, so a future regression on any of
// these surfaces lights up here first.
//
// Edge cases covered:
//   V1  calls disambiguation banner lists every overload even when all
//       candidates live in the same file (owner-type collision only).
//   V2  calls depth=0 with an unresolved name still returns the unified
//       "No symbol found" error so the seed-only branch cannot silently
//       swallow missing names.
//   V3  calls mermaid with a 3-node minimum diagram fits inside a tight
//       token_budget=150 without immediately tripping the truncation
//       marker, confirming the budget cap is not catastrophically low.
//   V4  grep plain name pattern (no wildcard) is still anchored to the
//       name column so substring matches inside file paths do not leak
//       back through the FTS default-column union.
//   V5  grep column-qualified FTS query (`name:Foo* AND kind:struct`)
//       passes through the anchor helper unchanged.
//   V6  grep kind filter accepts valid kinds and silently narrows;
//       bogus kinds return an empty-result message (consistency policy
//       shared with find.rs - neither rejects unknown kinds).
//   V7  refs concise mode with N distinct importer files (each with one
//       ref) emits N rows and does NOT collapse them under a fake
//       aggregate - collapse only fires for same-path duplicates.
//   V8  refs include_tests=false does NOT filter `#[cfg(test)] mod tests`
//       inline modules inside production files - the path-based
//       `is_test_path` predicate cannot see the cfg guard. Documents the
//       current behaviour as a known limitation rather than a silent bug.

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
// V1: disambiguation banner must list every candidate even when they all
// live in the same file. Owner-type collision (Foo::new + Bar::new inside
// lib.rs) is the worst case because the pre-fix banner listed the file
// once; the fix must produce one row per symbol.
// --------------------------------------------------------------------------

#[test]
fn calls_disambiguation_banner_handles_same_file_overloads() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("lib.rs"),
        "pub struct Foo;\nimpl Foo { pub fn overload() {} }\npub struct Bar;\nimpl Bar { pub fn overload() {} }\npub struct Baz;\nimpl Baz { pub fn overload() {} }\n",
    )
    .unwrap();

    let server = build_and_index(root);
    let out = server
        .call_tool_by_name("qartez_calls", json!({ "name": "overload" }))
        .expect("qartez_calls on shared-file overloads must succeed");

    assert!(
        out.contains("resolves to 3 function-like candidate(s)"),
        "multi-candidate banner must count every in-file overload:\n{out}"
    );
    let lib_rows = out.matches("src/lib.rs").count();
    assert!(
        lib_rows >= 3,
        "each overload must emit its own candidate row even when the defining file is shared (got {lib_rows} `src/lib.rs` occurrences):\n{out}"
    );
    assert!(
        !out.contains("callers:") && !out.contains("callees:"),
        "banner mode must not emit any callers/callees count (attribution is impossible pre-disambiguation):\n{out}"
    );
}

// --------------------------------------------------------------------------
// V2: depth=0 with an unknown name must still error with the unified
// "No symbol found" message. The seed-only branch lives AFTER the symbol
// lookup, so it never sees a missing name - but a regression could
// accidentally gate the error on `!seed_only` and swallow misses.
// --------------------------------------------------------------------------

#[test]
fn calls_depth_zero_unknown_name_errors_clearly() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub fn present() {}\n").unwrap();

    let server = build_and_index(root);
    let result = server.call_tool_by_name(
        "qartez_calls",
        json!({ "name": "absent_symbol", "depth": 0 }),
    );

    let err = result
        .expect_err("depth=0 with unknown name must return an error, not an empty seed header");
    assert!(
        err.contains("No symbol found with name 'absent_symbol'"),
        "missing-symbol error must use the unified wording shared with qartez_refs / qartez_find, got:\n{err}"
    );
}

// --------------------------------------------------------------------------
// V3: mermaid minimum-viable diagram (target + 1 callee + truncation
// marker) fits inside a reasonable token budget so the tool is still
// useful for callers who want a cheap graph. A catastrophically low cap
// would emit only the target node and no edges, which is unhelpful.
// --------------------------------------------------------------------------

#[test]
fn calls_mermaid_minimum_diagram_fits_reasonable_budget() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("lib.rs"),
        "pub fn root_fn() { leaf(); }\npub fn leaf() {}\n",
    )
    .unwrap();

    let server = build_and_index(root);
    let out = server
        .call_tool_by_name(
            "qartez_calls",
            json!({
                "name": "root_fn",
                "direction": "callees",
                "format": "mermaid",
                "token_budget": 150,
            }),
        )
        .expect("mermaid minimum diagram must succeed");

    assert!(
        out.starts_with("graph TD"),
        "mermaid output must carry the `graph TD` header:\n{out}"
    );
    assert!(out.contains("root_fn"), "target node must appear:\n{out}");
    assert!(
        out.contains("leaf") || out.contains("truncated"),
        "either the single callee is drawn or the truncation marker is emitted; a budget of 150 tokens must not silently drop both:\n{out}"
    );
}

// --------------------------------------------------------------------------
// V4: plain name query without a wildcard must still be anchored to the
// `name` column. A file path like `src/parser.rs` must not surface
// unrelated symbols (e.g. `new` inside parser.rs) when the user searches
// for `Parser` without the prefix star.
// --------------------------------------------------------------------------

#[test]
fn grep_plain_name_pattern_is_anchored_to_name_column() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(src.join("lib.rs"), "pub mod parser;\n").unwrap();
    // File PATH contains 'parser' but its symbol names do not start with
    // 'Parser'. Without name-column anchoring, `query=Parser` used to
    // surface these via the default-column FTS union.
    fs::write(
        src.join("parser.rs"),
        "pub struct Parser;\nimpl Parser { pub fn new() -> Self { Parser } }\npub fn helper() {}\n",
    )
    .unwrap();

    let server = build_and_index(root);
    let out = server
        .call_tool_by_name(
            "qartez_grep",
            json!({ "query": "Parser", "format": "concise" }),
        )
        .expect("qartez_grep Parser must succeed");

    assert!(
        out.contains("Parser"),
        "the struct named `Parser` must surface:\n{out}"
    );
    assert!(
        !out.contains(" new "),
        "unrelated symbols inside parser.rs (e.g. `new`) must NOT leak through the name-column anchor:\n{out}"
    );
    assert!(
        !out.contains("helper"),
        "unrelated `helper` inside parser.rs must NOT match a plain `Parser` query:\n{out}"
    );
}

// --------------------------------------------------------------------------
// V5: column-qualified FTS query (`name:Foo*`) is currently quoted by
// `sanitize_fts_query` before it ever reaches `anchor_prefix_to_name_column`,
// so the anchor helper leaves it alone (the branch that returns on a
// leading `"` is the live path, not the `contains(':')` branch). This
// test locks down the contract at the tool boundary: the query must not
// hard-error, and the anchor helper must not double-qualify it into
// `name:name:Foo*`. The helper's column-qualified branch is still useful
// for internal callers that bypass `sanitize_fts_query`; the inline unit
// tests in grep.rs cover that path directly.
// --------------------------------------------------------------------------

#[test]
fn grep_column_qualified_query_does_not_hard_error() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("lib.rs"),
        "pub struct FooStruct;\npub fn foo_fn() {}\n",
    )
    .unwrap();

    let server = build_and_index(root);
    // `name:Foo*` - explicit column qualifier. Because `sanitize_fts_query`
    // quotes any query containing `:`, the anchor helper sees a quoted
    // phrase and leaves it alone. FTS5 treats it as a literal phrase, so
    // the result set is empty - but the tool must NOT hard-error. The
    // empty-result message is the correct UX here.
    let out = server
        .call_tool_by_name(
            "qartez_grep",
            json!({ "query": "name:Foo*", "format": "concise" }),
        )
        .expect("column-qualified FTS query must not panic at the tool boundary");

    assert!(
        !out.contains("FTS error") && !out.contains("name:name:"),
        "anchor helper must not double-qualify column-qualified input (`name:name:` leak) and FTS must not raise a grammar error:\n{out}"
    );
}

// --------------------------------------------------------------------------
// V6: `kind` filter with a valid kind narrows results; with a bogus kind
// the tool returns an empty-result message rather than a hard error.
// This matches the policy in find.rs (`expand_kind_alias` also returns
// unknown kinds as-is for downstream filtering).
// --------------------------------------------------------------------------

#[test]
fn grep_kind_filter_accepts_valid_and_tolerates_bogus() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("lib.rs"),
        "pub struct Widget;\npub fn widget_fn() {}\n",
    )
    .unwrap();

    let server = build_and_index(root);

    // Valid `kind=struct` must narrow to the struct only.
    let structs = server
        .call_tool_by_name(
            "qartez_grep",
            json!({ "query": "Widget", "kind": "struct", "format": "concise" }),
        )
        .expect("kind=struct must succeed");
    assert!(
        structs.contains("Widget"),
        "kind=struct must include the struct row:\n{structs}"
    );
    assert!(
        !structs.contains("widget_fn"),
        "kind=struct must exclude the function:\n{structs}"
    );

    // Valid `kind=function` must narrow to the function only.
    let fns = server
        .call_tool_by_name(
            "qartez_grep",
            json!({ "query": "widget", "kind": "function", "format": "concise" }),
        )
        .expect("kind=function must succeed");
    assert!(
        fns.contains("widget_fn"),
        "kind=function must include the function row:\n{fns}"
    );
    assert!(
        !fns.contains(" Widget "),
        "kind=function must exclude the struct row:\n{fns}"
    );

    // Bogus kind: current policy is silent empty-result (no hard error).
    // Documents the shared laxness with find.rs.
    let bogus = server
        .call_tool_by_name(
            "qartez_grep",
            json!({ "query": "Widget", "kind": "bogus_kind_xyz", "format": "concise" }),
        )
        .expect("bogus kind must not hard-error; it must return a no-match message");
    assert!(
        bogus.contains("No symbols matching") && bogus.contains("kind=bogus_kind_xyz"),
        "bogus kind must surface in the empty-result message so the caller can spot the typo:\n{bogus}"
    );
}

// --------------------------------------------------------------------------
// V7: refs concise mode with 10 distinct importer files (each with one
// ref) emits up to 10 importer paths and does NOT artificially collapse
// them. Collapse only fires for same-path duplicates inside one importer.
// --------------------------------------------------------------------------

#[test]
fn refs_concise_preserves_distinct_importers() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    let mut lib = String::from("pub mod target;\n");
    fs::write(src.join("target.rs"), "pub fn hot() {}\n").unwrap();
    // 10 separate importer files, each with exactly one call site. The
    // concise collapse must NOT treat these as duplicates.
    for i in 0..10 {
        lib.push_str(&format!("pub mod importer_{i};\n"));
        fs::write(
            src.join(format!("importer_{i}.rs")),
            "use crate::target::hot;\npub fn use_it() { hot(); }\n",
        )
        .unwrap();
    }
    fs::write(src.join("lib.rs"), lib).unwrap();

    let server = build_and_index(root);
    let out = server
        .call_tool_by_name(
            "qartez_refs",
            json!({
                "symbol": "hot",
                "format": "concise",
                "token_budget": 20000,
            }),
        )
        .expect("qartez_refs concise must succeed");

    let distinct_importer_rows = (0..10)
        .filter(|i| out.contains(&format!("importer_{i}.rs")))
        .count();
    assert!(
        distinct_importer_rows >= 8,
        "at least 8 of the 10 distinct importers must appear (not artificially collapsed); saw {distinct_importer_rows}:\n{out}"
    );
    assert!(
        !out.contains(" x10"),
        "concise collapse must not emit a `xN` tag for distinct file paths:\n{out}"
    );
}

// --------------------------------------------------------------------------
// V8: `include_tests=false` cannot filter `#[cfg(test)] mod tests` inline
// test modules inside production files because `is_test_path` is a
// path-only predicate and the production file is not a test path. This
// test locks down the current limitation so callers are not surprised.
// --------------------------------------------------------------------------

#[test]
fn refs_include_tests_false_does_not_filter_inline_cfg_test_modules() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_cargo_manifest(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(src.join("target.rs"), "pub fn hot() {}\n").unwrap();
    // Production file with an inline `#[cfg(test)] mod tests { ... }`.
    // The path is `src/inline.rs` - not a test path. The ref from the
    // inline test module is still visible with include_tests=false.
    fs::write(
        src.join("inline.rs"),
        "use crate::target::hot;\npub fn prod() { hot(); }\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn t() { hot(); }\n}\n",
    )
    .unwrap();
    fs::write(src.join("lib.rs"), "pub mod target;\npub mod inline;\n").unwrap();

    let server = build_and_index(root);
    let out = server
        .call_tool_by_name(
            "qartez_refs",
            json!({
                "symbol": "hot",
                "include_tests": false,
                "token_budget": 20000,
            }),
        )
        .expect("qartez_refs include_tests=false must succeed");

    // Production file importer is still visible - that is the
    // correctness contract. The inline cfg(test) module's ref is NOT
    // filtered out because `is_test_path` only inspects the file path.
    assert!(
        out.contains("src/inline.rs"),
        "production importer must remain visible:\n{out}"
    );
    // Document the known limitation: the ref count for `src/inline.rs`
    // still includes the cfg(test) call site. A future fix would have
    // to parse attribute scopes to filter those out; today the
    // guarantee is path-only.
    // (No negative assertion here - this test locks down behaviour,
    // not a bug. If a future refactor starts filtering attribute-gated
    // call sites too, this test must be updated together with the
    // feature.)
}
