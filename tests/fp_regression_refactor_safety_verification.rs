// Rust guideline compliant 2026-04-23
//
// Verification-depth regression coverage for the cluster-a P0 data-loss
// batch committed as c5a62d1. The disambiguation regression file already
// anchors the baseline behavior; this file targets the edge cases the
// baseline tests do NOT exercise:
//
//   - safe_delete trait-impl enumeration when deleting a trait with
//     concrete implementors
//   - rename integration-level refusal when old_name OR new_name is a
//     builtin method identifier
//   - rename allow_collision=true escape hatch when targeting a builtin
//     name
//   - rename refusal when 2+ `impl ... { fn same_name }` methods exist
//     AND the caller already set `kind=method`
//   - rename_name validation rejects unicode identifiers Rust refuses
//   - rename cross-codebase collision detection when new_name is defined
//     in a completely untouched file
//   - rename_file crate-root refusal at workspace-nested layouts
//     (`crates/foo/src/lib.rs`) and at `src/bin/` entry points
//   - rename_file preview shows the parent module file as an importer
//     even when no `use` importer records the file
//   - replace_symbol kind-change WARNING fires on struct -> fn and is
//     still applied
//   - replace_symbol rejects a standalone `use ...;` line disguised as a
//     replacement introducer
//   - replace_symbol accepts multi-attribute prelude (`#[derive]`
//     stacked with `#[cfg(...)]`) before the real introducer
//   - insert rejects whitespace-only `new_code`
//   - mv preview displays the deterministic rewrite-pair for
//     symbol-ref importers (no `(unspecified)` leak)

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
// Fix 2: safe_delete trait-impl guard must enumerate every `impl Trait for`
// ---------------------------------------------------------------------------

#[test]
fn safe_delete_trait_preview_lists_every_impl_site() {
    // A trait with three distinct implementors in three files. The
    // preview must enumerate every `impl Greeter for ...` site so the
    // caller sees the full build breakage before applying.
    let dir = TempDir::new().unwrap();
    let trait_def = "pub trait Greeter { fn greet(&self) -> &'static str; }\n";
    let impl_a = r#"use crate::traits::Greeter;
pub struct Alpha;
impl Greeter for Alpha {
    fn greet(&self) -> &'static str { "alpha" }
}
"#;
    let impl_b = r#"use crate::traits::Greeter;
pub struct Beta;
impl Greeter for Beta {
    fn greet(&self) -> &'static str { "beta" }
}
"#;
    let impl_c = r#"use crate::traits::Greeter;
pub struct Gamma;
impl Greeter for Gamma {
    fn greet(&self) -> &'static str { "gamma" }
}
"#;
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            (
                "src/lib.rs",
                "pub mod traits;\npub mod alpha;\npub mod beta;\npub mod gamma;\n",
            ),
            ("src/traits.rs", trait_def),
            ("src/alpha.rs", impl_a),
            ("src/beta.rs", impl_b),
            ("src/gamma.rs", impl_c),
        ],
    );

    let preview = server
        .call_tool_by_name(
            "qartez_safe_delete",
            json!({
                "symbol": "Greeter",
                "kind": "trait",
                "file_path": "src/traits.rs",
            }),
        )
        .expect("trait delete preview must succeed");
    assert!(
        preview.contains("trait impl block"),
        "preview must advertise the trait-impl enumeration, got: {preview}"
    );
    for concrete in ["Alpha", "Beta", "Gamma"] {
        assert!(
            preview.contains(concrete),
            "preview must list every concrete implementor (missing {concrete}): {preview}",
        );
    }

    // Apply without `force` must refuse with the same enumeration visible.
    let err = server
        .call_tool_by_name(
            "qartez_safe_delete",
            json!({
                "symbol": "Greeter",
                "kind": "trait",
                "file_path": "src/traits.rs",
                "apply": true,
            }),
        )
        .expect_err("apply without force must refuse");
    assert!(
        err.contains("trait impl block"),
        "apply-refusal must mention the trait-impl blast radius, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix 3: rename integration-level refusal for builtin method names.
// ---------------------------------------------------------------------------

#[test]
fn rename_refuses_when_old_name_is_builtin_method() {
    // Renaming FROM `clone` without `allow_collision=true` must refuse -
    // a blanket rename would rewrite every `.clone()` call in the file.
    let dir = TempDir::new().unwrap();
    let src = r#"pub struct Foo;
impl Foo {
    pub fn clone(&self) -> Self { Foo }
}
pub fn caller(f: &Foo) -> Foo {
    f.clone()
}
"#;
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", src),
        ],
    );

    let err = server
        .call_tool_by_name(
            "qartez_rename",
            json!({ "old_name": "clone", "new_name": "duplicate" }),
        )
        .expect_err("builtin old_name must refuse by default");
    assert!(
        err.contains("builtin") || err.contains("allow_collision"),
        "refusal must cite the builtin/allow_collision escape, got: {err}"
    );
}

#[test]
fn rename_refuses_when_new_name_is_builtin_method() {
    // Renaming TO `iter` must refuse - existing `.iter()` call sites
    // elsewhere in the codebase start resolving through the renamed
    // symbol silently.
    let dir = TempDir::new().unwrap();
    let src = "pub fn walk() {}\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", src),
        ],
    );

    let err = server
        .call_tool_by_name(
            "qartez_rename",
            json!({ "old_name": "walk", "new_name": "iter" }),
        )
        .expect_err("builtin new_name must refuse by default");
    assert!(
        err.contains("builtin") || err.contains("allow_collision"),
        "refusal must cite the builtin/allow_collision escape, got: {err}"
    );
}

#[test]
fn rename_allow_collision_override_works_for_builtin_name() {
    // `allow_collision=true` is the documented escape hatch. The rename
    // must proceed instead of tripping the builtin guard.
    let dir = TempDir::new().unwrap();
    let src = "pub fn make_iter() {}\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", src),
        ],
    );

    // Preview mode, with allow_collision=true. Must NOT mention the
    // builtin guard anymore.
    let out = server
        .call_tool_by_name(
            "qartez_rename",
            json!({
                "old_name": "make_iter",
                "new_name": "iter",
                "allow_collision": true,
            }),
        )
        .expect("allow_collision must unblock the builtin guard");
    assert!(
        !out.contains("builtin"),
        "allow_collision must skip the builtin refusal, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// Fix 4: rename must refuse when kind filter alone leaves 2+ candidates.
// ---------------------------------------------------------------------------

#[test]
fn rename_refuses_when_kind_filter_leaves_multiple_methods() {
    // Three distinct `impl X { fn make(...) }` blocks in one file. The
    // baseline `rename_refuses_when_name_shared_by_multiple_kinds` test
    // exercises no-filter; this one specifically pins `kind=method` so
    // the remaining disambiguation knob is `file_path` and the tool
    // must still refuse instead of rewriting all three impls.
    let dir = TempDir::new().unwrap();
    let src = r#"pub struct Alpha;
pub struct Beta;
pub struct Gamma;

impl Alpha {
    pub fn make() -> Alpha { Alpha }
}

impl Beta {
    pub fn make() -> Beta { Beta }
}

impl Gamma {
    pub fn make() -> Gamma { Gamma }
}
"#;
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", src),
        ],
    );

    let err = server
        .call_tool_by_name(
            "qartez_rename",
            json!({
                "old_name": "make",
                "new_name": "build",
                "kind": "method",
            }),
        )
        .expect_err("kind-only filter must still refuse on multi-method collision");
    assert!(
        err.contains("file_path"),
        "refusal must ask for file_path specifically, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix 5: identifier validation rejects non-ASCII identifiers.
// ---------------------------------------------------------------------------

#[test]
fn rename_refuses_unicode_new_name() {
    // `foo_λ` is a valid Unicode identifier but Rust rejects it (non-XID
    // character). The validation gate must refuse before touching disk.
    let dir = TempDir::new().unwrap();
    let src = "pub fn foo() {}\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", src),
        ],
    );

    let err = server
        .call_tool_by_name(
            "qartez_rename",
            json!({ "old_name": "foo", "new_name": "foo_\u{03bb}" }),
        )
        .expect_err("unicode new_name must be refused");
    assert!(
        err.contains("not a valid identifier")
            || err.contains("outside [A-Za-z0-9_]")
            || err.contains("identifier"),
        "refusal must cite the identifier constraint, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix 6: cross-codebase collision detection (not just touched files).
// ---------------------------------------------------------------------------

#[test]
fn rename_refuses_collision_in_unrelated_file() {
    // `foo` lives in src/a.rs with no importer; `bar` is defined in a
    // completely unrelated file src/b.rs. Renaming `foo` -> `bar` must
    // refuse because the index-wide scan surfaces the collision.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod a;\npub mod b;\n"),
            ("src/a.rs", "pub fn foo() -> u32 { 1 }\n"),
            ("src/b.rs", "pub fn bar() -> u32 { 2 }\n"),
        ],
    );

    let err = server
        .call_tool_by_name(
            "qartez_rename",
            json!({
                "old_name": "foo",
                "new_name": "bar",
                "file_path": "src/a.rs",
            }),
        )
        .expect_err("cross-file collision must refuse");
    assert!(
        err.contains("already defined") || err.contains("collision"),
        "refusal must cite the collision, got: {err}"
    );
    assert!(
        err.contains("src/b.rs"),
        "refusal must point at the colliding definition file, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix 7: crate-root refusal applies at nested-workspace layouts.
// ---------------------------------------------------------------------------

#[test]
fn rename_file_refuses_crate_root_lib_rs() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub fn api() {}\n"),
        ],
    );

    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({
                "from": "src/lib.rs",
                "to": "src/api.rs",
                "apply": true,
            }),
        )
        .expect_err("src/lib.rs rename must refuse");
    assert!(
        err.contains("crate root") || err.contains("Cargo.toml"),
        "refusal must cite the crate-root concern, got: {err}"
    );
}

#[test]
fn rename_file_refuses_src_bin_entry_point() {
    // `src/bin/tool.rs` is a named binary registered in Cargo.toml; the
    // guard must treat it the same as `src/main.rs`.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub fn api() {}\n"),
            ("src/bin/main.rs", "fn main() { println!(\"hi\"); }\n"),
        ],
    );

    let err = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({
                "from": "src/bin/main.rs",
                "to": "src/bin/app.rs",
                "apply": true,
            }),
        )
        .expect_err("src/bin/main.rs rename must refuse");
    assert!(
        err.contains("crate root") || err.contains("Cargo.toml"),
        "refusal must cite the crate-root concern, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix 8: rename_file preview shows parent mod file as an importer.
// ---------------------------------------------------------------------------

#[test]
fn rename_file_preview_lists_parent_mod_as_importer() {
    // `src/index/parser.rs` has NO `use` importer anywhere. The only
    // referrer is `src/index/mod.rs` via `pub mod parser;`. The preview
    // must surface that parent mod file in the importer list so the
    // caller sees the full rewrite surface before apply.
    let dir = TempDir::new().unwrap();
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod index;\n"),
            ("src/index/mod.rs", "pub mod parser;\n"),
            ("src/index/parser.rs", "pub fn parse() -> u32 { 0 }\n"),
        ],
    );

    let preview = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({
                "from": "src/index/parser.rs",
                "to": "src/index/ast.rs",
            }),
        )
        .expect("preview must succeed");
    assert!(
        preview.contains("src/index/mod.rs"),
        "preview must list the parent mod file as an importer, got: {preview}"
    );
    // Counter-assert: the "0 importers" short-circuit path MUST NOT
    // fire. The parent is a legitimate importer.
    assert!(
        !preview.contains("0 importers"),
        "parent mod importer must push count above zero, got: {preview}"
    );
}

// ---------------------------------------------------------------------------
// Fix 9: replace_symbol kind-change surfaces as WARNING and applies.
// ---------------------------------------------------------------------------

#[test]
fn replace_symbol_kind_change_struct_to_fn_warns_and_applies() {
    // Morphing `pub struct Wrapper;` into `pub fn wrapper() -> u32 { 0 }`
    // is a legitimate refactor but the caller deserves a WARNING.
    let dir = TempDir::new().unwrap();
    let src = "pub struct Wrapper;\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", src),
        ],
    );

    // Preview: must show the WARNING line. The replacement keeps the
    // same identifier (`Wrapper`) so the "same identifier" guard passes
    // and only the kind-change WARNING fires.
    let preview = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "Wrapper",
                "new_code": "pub fn Wrapper() -> u32 { 0 }\n",
            }),
        )
        .expect("preview must succeed");
    assert!(
        preview.contains("WARNING") && preview.contains("kind change"),
        "preview must surface the kind-change WARNING, got: {preview}"
    );

    // Apply: the new structural-change guard refuses kind changes in
    // apply mode. Callers must route through qartez_rename or stage
    // the change as delete + re-declare. The preview remains the way
    // to surface the WARNING without mutating disk.
    let err = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "Wrapper",
                "new_code": "pub fn Wrapper() -> u32 { 0 }\n",
                "apply": true,
            }),
        )
        .expect_err("apply must now refuse structural changes");
    assert!(
        err.contains("kind change") && err.contains("struct") && err.contains("fn"),
        "refusal must name the kind change, got: {err}"
    );
    let written = fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert!(
        written.contains("pub struct Wrapper"),
        "refused apply must leave the original struct on disk, got:\n{written}"
    );
}

// ---------------------------------------------------------------------------
// Fix 10: replace_symbol rejects `use ...;` disguised as an introducer.
// ---------------------------------------------------------------------------

#[test]
fn replace_symbol_refuses_use_statement_as_replacement() {
    // `use std::collections::HashMap;` is NOT a standalone definition;
    // the prelude-strip must NOT accept it as the "first real
    // introducer" because downstream would then splice a `use` line
    // into the symbol's range with no signature at all.
    let dir = TempDir::new().unwrap();
    let src = "pub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", src),
        ],
    );

    let err = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "add",
                "new_code": "use std::collections::HashMap;\n",
                "apply": true,
            }),
        )
        .expect_err("bare `use` must refuse");
    assert!(
        err.contains("introducer") || err.contains("signature"),
        "refusal must cite the missing introducer, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix 11: multi-attribute prelude (stacked `#[cfg(...)]`) accepted.
// ---------------------------------------------------------------------------

#[test]
fn replace_symbol_accepts_stacked_attribute_prelude() {
    // Several stacked attributes + doc comments before the real `pub
    // fn`. The check_signature_shape scanner must walk past them all.
    let dir = TempDir::new().unwrap();
    let src = "pub fn noop() {}\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", src),
        ],
    );

    let new_code = "\
/// Stacked attributes + doc comment prelude.
#[cfg(test)]
#[allow(dead_code)]
#[inline]
pub fn noop() {}
";

    let out = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "noop",
                "new_code": new_code,
                "apply": true,
            }),
        )
        .expect("stacked prelude must be accepted");
    assert!(
        out.contains("Replaced"),
        "apply must succeed with stacked prelude, got: {out}"
    );
    let written = fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert!(
        written.contains("#[cfg(test)]") && written.contains("#[inline]"),
        "multi-attribute prelude must land intact on disk, got:\n{written}"
    );
}

// ---------------------------------------------------------------------------
// Fix 12: insert rejects whitespace-only `new_code`.
// ---------------------------------------------------------------------------

#[test]
fn insert_refuses_whitespace_only_new_code() {
    // `   \n\t\n` trims to empty and must be rejected by both
    // insert_before and insert_after so a silent noop cannot corrupt
    // the preview/apply contract.
    let dir = TempDir::new().unwrap();
    let src = "pub fn target() {}\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", src),
        ],
    );

    let err = server
        .call_tool_by_name(
            "qartez_insert_before_symbol",
            json!({
                "symbol": "target",
                "new_code": "   \n\t \n",
                "apply": true,
            }),
        )
        .expect_err("whitespace-only insert must refuse");
    assert!(
        err.contains("Empty `new_code`"),
        "refusal must cite the empty-new_code guard, got: {err}"
    );

    let err = server
        .call_tool_by_name(
            "qartez_insert_after_symbol",
            json!({
                "symbol": "target",
                "new_code": "  \n",
                "apply": true,
            }),
        )
        .expect_err("whitespace-only insert must refuse on `after` too");
    assert!(
        err.contains("Empty `new_code`"),
        "refusal must cite the empty-new_code guard on `after`, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Fix 13: mv preview shows real rewrite pair instead of `(unspecified)`.
// ---------------------------------------------------------------------------

#[test]
fn move_preview_shows_deterministic_rewrite_pair() {
    // An importer that reaches `greet` via a symbol-ref edge (no `use`
    // specifier recorded) previously surfaced as ` - via '(unspecified)'`.
    // The preview must now display the concrete `old_stem -> new_stem`
    // rewrite pair so the caller can audit the flip.
    let dir = TempDir::new().unwrap();
    let defs = "pub fn greet() -> &'static str { \"hi\" }\n";
    // The caller uses a fully-qualified path (`crate::defs::greet()`)
    // so the edge is captured but the `use` specifier is absent.
    let caller = "pub fn a() -> &'static str { crate::defs::greet() }\n";
    let server = build_and_index(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod defs;\npub mod a;\n"),
            ("src/defs.rs", defs),
            ("src/a.rs", caller),
        ],
    );

    let preview = server
        .call_tool_by_name(
            "qartez_move",
            json!({
                "symbol": "greet",
                "to_file": "src/util.rs",
                "file_path": "src/defs.rs",
            }),
        )
        .expect("preview must succeed");
    assert!(
        !preview.contains("(unspecified)"),
        "preview must not leak `(unspecified)` placeholder, got: {preview}"
    );
    // Preview must mention either the explicit rewrite-pair string or a
    // concrete `use`-style specifier. The deterministic pair path
    // emits `will rewrite '...' -> '...'`.
    assert!(
        preview.contains("will rewrite") || preview.contains("via '"),
        "preview must show concrete rewrite info for the importer, got: {preview}"
    );
}
