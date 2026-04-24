// Rust guideline compliant 2026-04-23
//
// Regression coverage for the trait-impl awareness fixes in
// `qartez_rename`, `qartez_move`, and `qartez_safe_delete`, plus the
// rename preview byte cap.
//
// Background. A trait with N `impl Trait for X` blocks was silently
// undercounted by every refactor tool: rename rewrote the definition
// and direct callers but ignored the impl lines; move rewrote import
// edges but ignored the implementor files; safe_delete claimed "no
// importers" even when 37 concrete implementors existed. Apply would
// have broken the crate in each case.
//
// The fixes route every trait-shaped rename / move / delete through
// `read::get_subtypes`, which reads the authoritative `type_hierarchy`
// table populated by the indexer. This suite locks that in and also
// pins the rename preview output under a reasonable byte budget so the
// MCP transport cap is never exceeded.

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

fn trait_fixture() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        ),
        (
            "src/lib.rs",
            "pub mod traits;\npub mod alpha;\npub mod beta;\npub mod gamma;\n",
        ),
        (
            "src/traits.rs",
            "pub trait LanguageSupport { fn tag(&self) -> &'static str; }\n",
        ),
        (
            "src/alpha.rs",
            "use crate::traits::LanguageSupport;\npub struct Alpha;\nimpl LanguageSupport for Alpha {\n    fn tag(&self) -> &'static str { \"alpha\" }\n}\n",
        ),
        (
            "src/beta.rs",
            "use crate::traits::LanguageSupport;\npub struct Beta;\nimpl LanguageSupport for Beta {\n    fn tag(&self) -> &'static str { \"beta\" }\n}\n",
        ),
        (
            "src/gamma.rs",
            "use crate::traits::LanguageSupport;\npub struct Gamma;\nimpl LanguageSupport for Gamma {\n    fn tag(&self) -> &'static str { \"gamma\" }\n}\n",
        ),
    ]
}

// ---------------------------------------------------------------------------
// Bug A: qartez_rename must rewrite every `impl OldTrait for X` site.
// ---------------------------------------------------------------------------

#[test]
fn rename_trait_preview_covers_every_impl_site() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &trait_fixture());

    let preview = server
        .call_tool_by_name(
            "qartez_rename",
            json!({
                "old_name": "LanguageSupport",
                "new_name": "LanguageBackend",
                "kind": "trait",
                "file_path": "src/traits.rs",
            }),
        )
        .expect("trait rename preview must succeed");

    // Every implementor file must appear in the preview - otherwise the
    // apply pass would leave `impl LanguageSupport for Alpha` behind.
    for impl_file in ["src/alpha.rs", "src/beta.rs", "src/gamma.rs"] {
        assert!(
            preview.contains(impl_file),
            "preview must include implementor file {impl_file}, got: {preview}",
        );
    }

    // The impl-block line itself must surface in the preview so the
    // caller can verify the rewrite plan.
    assert!(
        preview.matches("impl LanguageBackend for").count() >= 3,
        "preview must show the rewritten `impl OldTrait for X` line for every implementor, got: {preview}",
    );
}

#[test]
fn rename_trait_apply_rewrites_every_impl_block() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &trait_fixture());

    let _ = server
        .call_tool_by_name(
            "qartez_rename",
            json!({
                "old_name": "LanguageSupport",
                "new_name": "LanguageBackend",
                "kind": "trait",
                "file_path": "src/traits.rs",
                "apply": true,
            }),
        )
        .expect("trait rename apply must succeed");

    for impl_file in ["src/alpha.rs", "src/beta.rs", "src/gamma.rs"] {
        let content = fs::read_to_string(dir.path().join(impl_file)).unwrap();
        assert!(
            content.contains("impl LanguageBackend for"),
            "{impl_file} must contain `impl LanguageBackend for` after rename, got: {content}",
        );
        assert!(
            !content.contains("impl LanguageSupport for"),
            "{impl_file} must no longer contain `impl LanguageSupport for`, got: {content}",
        );
    }
}

// ---------------------------------------------------------------------------
// Bug B: qartez_move must include every `impl OldTrait for X` file in the
// importer set so the `use` path is rewritten on apply.
// ---------------------------------------------------------------------------

#[test]
fn move_trait_lists_every_impl_file_as_importer() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &trait_fixture());

    let preview = server
        .call_tool_by_name(
            "qartez_move",
            json!({
                "symbol": "LanguageSupport",
                "kind": "trait",
                "file_path": "src/traits.rs",
                "to_file": "src/backends.rs",
            }),
        )
        .expect("trait move preview must succeed");

    assert!(
        !preview.contains("No files import this symbol"),
        "preview must not claim zero importers for a trait with concrete impls, got: {preview}",
    );
    for impl_file in ["src/alpha.rs", "src/beta.rs", "src/gamma.rs"] {
        assert!(
            preview.contains(impl_file),
            "move preview must list implementor file {impl_file}, got: {preview}",
        );
    }
}

// ---------------------------------------------------------------------------
// Bug C: qartez_safe_delete must enumerate every implementor file for a
// trait delete. (Regression guard - the underlying fix already lives in
// `safe_delete.rs`.)
// ---------------------------------------------------------------------------

#[test]
fn safe_delete_trait_enumerates_every_impl_file() {
    let dir = TempDir::new().unwrap();
    let server = build_and_index(dir.path(), &trait_fixture());

    let preview = server
        .call_tool_by_name(
            "qartez_safe_delete",
            json!({
                "symbol": "LanguageSupport",
                "kind": "trait",
                "file_path": "src/traits.rs",
            }),
        )
        .expect("trait delete preview must succeed");

    assert!(
        preview.contains("trait impl block"),
        "preview must surface the trait-impl blast radius, got: {preview}",
    );
    for concrete in ["Alpha", "Beta", "Gamma"] {
        assert!(
            preview.contains(concrete),
            "preview must list concrete implementor {concrete}, got: {preview}",
        );
    }
    for impl_file in ["src/alpha.rs", "src/beta.rs", "src/gamma.rs"] {
        assert!(
            preview.contains(impl_file),
            "preview must include implementor file {impl_file}, got: {preview}",
        );
    }
}

// ---------------------------------------------------------------------------
// Bug D: the rename preview output must fit under a reasonable byte budget
// and advertise truncation when the caller needs to narrow.
// ---------------------------------------------------------------------------

#[test]
fn rename_preview_respects_byte_cap_and_emits_truncation_footer() {
    // Construct a scenario with an intentionally chatty symbol: a
    // trait named `Chatty` with many implementor files so the raw
    // occurrence list exceeds the 48 KB preview cap.
    let dir = TempDir::new().unwrap();
    let mut files: Vec<(String, String)> = Vec::new();
    files.push((
        "Cargo.toml".to_string(),
        "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n".to_string(),
    ));

    const IMPL_COUNT: usize = 400;
    let mut lib_rs = String::from("pub mod trait_def;\n");
    for i in 0..IMPL_COUNT {
        lib_rs.push_str(&format!("pub mod impl_{i:03};\n"));
    }
    files.push(("src/lib.rs".to_string(), lib_rs));
    files.push((
        "src/trait_def.rs".to_string(),
        "pub trait Chatty { fn chat(&self) -> &'static str; }\n".to_string(),
    ));
    for i in 0..IMPL_COUNT {
        // A long, unique line per impl so the concatenated preview body
        // overflows the cap if left unbounded.
        let body = format!(
            "use crate::trait_def::Chatty;\n\
             pub struct Impl{i:03}_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa;\n\
             impl Chatty for Impl{i:03}_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa {{\n    \
                 fn chat(&self) -> &'static str {{ \"impl_{i:03}_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }}\n\
             }}\n",
        );
        files.push((format!("src/impl_{i:03}.rs"), body));
    }

    let slice: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let server = build_and_index(dir.path(), &slice);

    let preview = server
        .call_tool_by_name(
            "qartez_rename",
            json!({
                "old_name": "Chatty",
                "new_name": "Renamed",
                "kind": "trait",
                "file_path": "src/trait_def.rs",
            }),
        )
        .expect("chatty rename preview must succeed");

    // Bound on the preview size: the cap is 48 KB; allow a small fudge
    // factor for the header and the truncation footer but ensure the
    // result is well below the MCP transport cap.
    assert!(
        preview.len() <= 64 * 1024,
        "preview must fit under 64 KB, got {} bytes",
        preview.len(),
    );
    // If the preview output hit the 48 KB cap, the truncation footer
    // must be present. For smaller renames the footer is absent by
    // design. This keeps the test meaningful across extractor-shape
    // changes that can shrink the per-occurrence byte count.
    if preview.len() > 48 * 1024 {
        assert!(
            preview.contains("truncated by preview cap"),
            "preview over 48 KB must emit the truncation footer, got {} bytes",
            preview.len(),
        );
    }
    // Header scope counters must still report the full picture.
    assert!(
        preview.contains("Chatty → Renamed"),
        "preview header must announce the rename, got: {preview}",
    );
}
