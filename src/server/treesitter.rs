// Rust guideline compliant 2026-04-15

//! Tree-sitter AST walking helpers and file-path utilities used by the
//! rename, move, calls, and outline tool handlers.

use std::collections::HashMap;

/// Per-file identifier map keyed by identifier text. Each occurrence is
/// `(row, start_byte, end_byte)`.
pub(super) type IdentMap = HashMap<String, Vec<(usize, usize, usize)>>;

pub(super) const IDENTIFIER_NODE_KINDS: &[&str] = &[
    "identifier",
    "type_identifier",
    "field_identifier",
    "property_identifier",
    "simple_identifier",
    "shorthand_property_identifier_pattern",
    "shorthand_property_identifier",
];

/// Walk the tree once and group every identifier occurrence by its source
/// text. Used to populate the cross-invocation identifier cache so later
/// `qartez_rename` calls turn into O(1) HashMap lookups.
///
/// Uses iterative depth-first traversal with an explicit stack to avoid
/// stack overflow on deeply nested ASTs.
pub(super) fn collect_identifiers_grouped(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    results: &mut IdentMap,
) {
    let mut reached_root = false;
    while !reached_root {
        let node = cursor.node();
        if IDENTIFIER_NODE_KINDS.contains(&node.kind())
            && let Ok(text) = node.utf8_text(source)
        {
            let line = node.start_position().row + 1;
            results.entry(text.to_string()).or_default().push((
                line,
                node.start_byte(),
                node.end_byte(),
            ));
        }

        if cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                reached_root = true;
                break;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

pub(super) const CALL_NODE_KINDS: &[&str] = &[
    "call_expression",
    "method_invocation",
    "function_call",
    "member_expression",
];

pub(super) const CALLEE_FIELD_NAMES: &[&str] = &["function", "name", "method"];

/// Uses iterative depth-first traversal with an explicit stack to avoid
/// stack overflow on deeply nested ASTs.
pub(super) fn collect_call_names(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    results: &mut Vec<(String, usize)>,
) {
    let mut reached_root = false;
    while !reached_root {
        let node = cursor.node();
        if CALL_NODE_KINDS.contains(&node.kind()) {
            for field in CALLEE_FIELD_NAMES {
                if let Some(callee) = node.child_by_field_name(field) {
                    let name = extract_callee_name(callee, source);
                    if !name.is_empty() {
                        let line = node.start_position().row + 1;
                        results.push((name, line));
                    }
                    break;
                }
            }
            if results
                .last()
                .map(|(_, l)| *l != node.start_position().row + 1)
                .unwrap_or(true)
                && let Some(first_child) = node.child(0)
            {
                let name = extract_callee_name(first_child, source);
                if !name.is_empty() {
                    let line = node.start_position().row + 1;
                    results.push((name, line));
                }
            }
        }

        if cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                reached_root = true;
                break;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

pub(super) fn extract_callee_name(node: tree_sitter::Node, source: &[u8]) -> String {
    match node.kind() {
        "identifier" | "simple_identifier" | "property_identifier" => {
            node.utf8_text(source).unwrap_or("").to_string()
        }
        "field_expression" | "member_expression" | "scoped_identifier" | "attribute" => {
            if let Some(field) = node
                .child_by_field_name("field")
                .or_else(|| node.child_by_field_name("property"))
                .or_else(|| node.child_by_field_name("name"))
            {
                field.utf8_text(source).unwrap_or("").to_string()
            } else {
                let count = node.child_count();
                if count > 0 {
                    if let Some(last) = node.child(count - 1) {
                        last.utf8_text(source).unwrap_or("").to_string()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            }
        }
        _ => node.utf8_text(source).unwrap_or("").to_string(),
    }
}

pub(super) fn capitalize_kind(kind: &str) -> String {
    let mut chars = kind.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => {
            let upper: String = c.to_uppercase().collect();
            let rest: String = chars.collect();
            let singular = format!("{upper}{rest}");
            if singular.ends_with('s') || singular.ends_with("sh") || singular.ends_with("ch") {
                format!("{singular}es")
            } else {
                format!("{singular}s")
            }
        }
    }
}

pub(super) fn path_to_import_stem(file_path: &str) -> String {
    let without_ext = file_path
        .rsplit_once('.')
        .map(|(base, _)| base)
        .unwrap_or(file_path);
    without_ext.replace('/', "::")
}

pub(super) fn relative_import_stem(file_path: &str) -> String {
    let without_ext = file_path
        .rsplit_once('.')
        .map(|(base, _)| base)
        .unwrap_or(file_path);
    let stem = without_ext
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(without_ext);
    stem.to_string()
}

/// Build rename pairs that cover both the full index-style stem
/// (`src::foo::bar`) and the prefix-stripped stem (`foo::bar`). The second
/// form is what shows up in `use crate::...` and `use super::...` imports,
/// so without it rename_file/move silently leave those importers pointing
/// at the old path. Pairs are returned longest first so the caller applies
/// them in order without the short form clobbering a partial match of the
/// long form.
///
/// Returns an empty vector when old and new stems are identical.
pub(super) fn rename_stem_pairs(old_full: &str, new_full: &str) -> Vec<(String, String)> {
    if old_full == new_full {
        return Vec::new();
    }
    let mut pairs = vec![(old_full.to_string(), new_full.to_string())];

    let old_segs: Vec<&str> = old_full.split("::").collect();
    let new_segs: Vec<&str> = new_full.split("::").collect();
    let prefix_len = old_segs
        .iter()
        .zip(new_segs.iter())
        .take_while(|(a, b)| a == b)
        .count();

    // Emit the divergent-suffix pair only when stripping the common prefix
    // leaves at least one `::` on both sides. Single-segment suffixes
    // (`bar` → `qux`) are rejected on purpose - they would word-boundary
    // match unrelated identifiers that happen to share the stem.
    let mut suffix_emitted = false;
    if prefix_len > 0 && prefix_len < old_segs.len() && prefix_len < new_segs.len() {
        let old_suffix = old_segs[prefix_len..].join("::");
        let new_suffix = new_segs[prefix_len..].join("::");
        if old_suffix.contains("::") && new_suffix.contains("::") && old_suffix != new_suffix {
            pairs.push((old_suffix, new_suffix));
            suffix_emitted = true;
        }
    }

    // When the divergent suffix is a bare single segment (e.g. renaming
    // `src/foo.rs` → `src/baz.rs`, yielding suffix `foo` → `baz`), the
    // suffix pair is skipped above because `\bfoo\b` would rewrite any
    // local variable or parameter sharing the name. Fall back to a
    // `crate::`-prefixed pair: Rust imports reach the module through
    // `use crate::foo::...`, and the `crate::` prefix disambiguates the
    // match so unrelated identifiers are safe. Without this, root-level
    // Rust file renames left every `crate::`-relative importer dangling.
    if !suffix_emitted && old_segs.len() > 1 && new_segs.len() > 1 && old_segs[0] == new_segs[0] {
        let old_crate = format!("crate::{}", old_segs[1..].join("::"));
        let new_crate = format!("crate::{}", new_segs[1..].join("::"));
        if old_crate != new_crate {
            pairs.push((old_crate, new_crate));
        }
    }

    pairs
}

/// Apply the rename pairs produced by [`rename_stem_pairs`] to `content`,
/// longest pair first so the full-path match does not leak into the
/// shorter-path replacement.
pub(super) fn apply_rename_pairs(
    content: &str,
    pairs: &[(String, String)],
) -> std::result::Result<String, String> {
    let mut out = content.to_string();
    for (old, new) in pairs {
        let re = regex::Regex::new(&format!(r"\b{}\b", regex::escape(old)))
            .map_err(|e| format!("regex error: {e}"))?;
        out = re.replace_all(&out, new.as_str()).to_string();
    }
    Ok(out)
}

/// Resolve the parent module file that declares `mod <name>;` for a given
/// Rust source file. Covers both the `foo/mod.rs` and flat `foo.rs` module
/// layouts, falling back to the crate root (`lib.rs` / `main.rs`) when the
/// file lives directly under a crate source directory.
///
/// Returns `None` when the file is not a Rust source file or no parent
/// declaration file can be located.
pub(super) fn find_parent_mod_file(
    project_root: &std::path::Path,
    rel_path: &str,
) -> Option<std::path::PathBuf> {
    if !rel_path.ends_with(".rs") {
        return None;
    }
    let path = std::path::Path::new(rel_path);
    let parent = path.parent()?;
    let file_name = path.file_name()?.to_str()?;

    // Crate entry points have no parent `mod` declaration - Cargo.toml
    // registers them directly. Without this early return a rename of
    // `src/lib.rs` would pick up a sibling `src/mod.rs` or `src.rs` as a
    // "parent" and the caller would rewrite an unrelated file's
    // `mod lib;` / `mod main;` lines.
    if file_name == "lib.rs" || file_name == "main.rs" {
        return None;
    }

    let effective_parent: std::path::PathBuf = if file_name == "mod.rs" {
        parent.parent()?.to_path_buf()
    } else {
        parent.to_path_buf()
    };

    let candidates: Vec<std::path::PathBuf> = if effective_parent.as_os_str().is_empty() {
        vec![
            std::path::PathBuf::from("lib.rs"),
            std::path::PathBuf::from("main.rs"),
        ]
    } else {
        let mut v = vec![effective_parent.join("mod.rs")];
        if let Some(parent_of_parent) = effective_parent.parent()
            && let Some(dir_name) = effective_parent.file_name()
        {
            let mut flat = parent_of_parent.to_path_buf();
            flat.push(format!("{}.rs", dir_name.to_string_lossy()));
            v.push(flat);
        }
        v.push(effective_parent.join("lib.rs"));
        v.push(effective_parent.join("main.rs"));
        v
    };

    for cand in candidates {
        let abs = project_root.join(&cand);
        if abs.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Rewrite `mod <old>;` / `pub mod <old>;` declarations in `content` to use
/// `<new>`. Preserves visibility, attributes, and whitespace. Inline modules
/// (`mod foo { ... }`) are left alone because they are not backed by a file
/// and renaming the file has no effect on them.
pub(super) fn rewrite_mod_decl(content: &str, old: &str, new: &str) -> String {
    let pattern = format!(
        r"(?m)^(?P<prefix>\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+){}(?P<suffix>\s*;)",
        regex::escape(old),
    );
    match regex::Regex::new(&pattern) {
        Ok(re) => re
            .replace_all(content, format!("${{prefix}}{new}${{suffix}}"))
            .to_string(),
        Err(_) => content.to_string(),
    }
}

#[cfg(test)]
mod rename_pairs_tests {
    use super::*;

    #[test]
    fn generates_full_and_divergent_pairs() {
        let pairs = rename_stem_pairs("src::foo::bar", "src::baz::qux");
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("src::foo::bar".into(), "src::baz::qux".into()));
        assert_eq!(pairs[1], ("foo::bar".into(), "baz::qux".into()));
    }

    #[test]
    fn emits_crate_prefixed_pair_for_bare_single_segment_suffix() {
        // The bare suffix pair (`bar` → `qux`) is still dropped - it would
        // over-match local variables and parameters - but a `crate::`-
        // prefixed pair is emitted so `use crate::foo::bar::X;` importers
        // still get rewritten. The `crate::` prefix keeps the match
        // unambiguous even for a single-segment suffix.
        let pairs = rename_stem_pairs("src::foo::bar", "src::foo::qux");
        assert_eq!(
            pairs,
            vec![
                ("src::foo::bar".into(), "src::foo::qux".into()),
                ("crate::foo::bar".into(), "crate::foo::qux".into()),
            ]
        );
    }

    #[test]
    fn root_level_rust_file_rename_emits_crate_pair() {
        // Regression guard: renaming `src/foo.rs` → `src/baz.rs` yields
        // stems `src::foo` → `src::baz`. The divergent suffix is the bare
        // `foo` → `baz` so the suffix pair is dropped, but the crate pair
        // `(crate::foo, crate::baz)` must be emitted so `use crate::foo`
        // importers get rewritten. Without it the crate fails to build.
        let pairs = rename_stem_pairs("src::foo", "src::baz");
        assert_eq!(
            pairs,
            vec![
                ("src::foo".into(), "src::baz".into()),
                ("crate::foo".into(), "crate::baz".into()),
            ]
        );
    }

    #[test]
    fn root_level_rust_file_rename_rewrites_crate_imports() {
        let pairs = rename_stem_pairs("src::foo", "src::baz");
        let input = "use crate::foo::Bar;\nuse crate::foo;\nlet x = crate::foo::call();";
        let out = apply_rename_pairs(input, &pairs).unwrap();
        assert_eq!(
            out,
            "use crate::baz::Bar;\nuse crate::baz;\nlet x = crate::baz::call();"
        );
    }

    #[test]
    fn crate_pair_does_not_rewrite_unrelated_identifiers() {
        // The whole point of refusing the bare suffix is to protect local
        // identifiers named like the stem. Verify that the `crate::`-
        // prefixed pair keeps that invariant.
        let pairs = rename_stem_pairs("src::foo", "src::baz");
        let input = "let foo = 1;\nfn foo_ext() {}\nstruct S { foo: u32 }\nlet x = obj.foo();";
        let out = apply_rename_pairs(input, &pairs).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn empty_when_stems_equal() {
        assert!(rename_stem_pairs("src::foo", "src::foo").is_empty());
    }

    #[test]
    fn no_common_prefix_keeps_full_only() {
        let pairs = rename_stem_pairs("src::foo::bar", "other::qux");
        assert_eq!(pairs, vec![("src::foo::bar".into(), "other::qux".into())]);
    }

    #[test]
    fn applies_longest_pair_first() {
        let pairs = rename_stem_pairs("src::foo::bar", "src::baz::qux");
        // Input where the long pair would leak if ordered short-first.
        let input = "use src::foo::bar; use crate::foo::bar;";
        let out = apply_rename_pairs(input, &pairs).unwrap();
        assert_eq!(out, "use src::baz::qux; use crate::baz::qux;");
    }

    #[test]
    fn does_not_touch_unrelated_identifier_named_like_file_stem() {
        let pairs = rename_stem_pairs("src::foo::bar", "src::baz::qux");
        // Bare `bar` variables must remain untouched - this was the
        // regression caused by the previous rel_stem regex.
        let input = "let bar = 1;\nfn bar_ext() {}\nuse crate::foo::bar;";
        let out = apply_rename_pairs(input, &pairs).unwrap();
        assert!(out.contains("let bar = 1;"), "local var kept: {out}");
        assert!(out.contains("fn bar_ext()"), "unrelated name kept: {out}");
        assert!(
            out.contains("use crate::baz::qux;"),
            "import rewritten: {out}"
        );
    }

    #[test]
    fn preserves_word_boundary() {
        let pairs = rename_stem_pairs("foo::bar", "baz::qux");
        // `foo::barrier` must not match `foo::bar`.
        let input = "use foo::bar; use foo::barrier;";
        let out = apply_rename_pairs(input, &pairs).unwrap();
        assert!(out.contains("use baz::qux;"));
        assert!(out.contains("use foo::barrier;"));
    }
}
