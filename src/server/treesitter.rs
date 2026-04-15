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
pub(super) fn collect_identifiers_grouped(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    results: &mut IdentMap,
) {
    loop {
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
            collect_identifiers_grouped(cursor, source, results);
            cursor.goto_parent();
        }

        if !cursor.goto_next_sibling() {
            break;
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

pub(super) fn collect_call_names(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    results: &mut Vec<(String, usize)>,
) {
    loop {
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
            collect_call_names(cursor, source, results);
            cursor.goto_parent();
        }

        if !cursor.goto_next_sibling() {
            break;
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
                    if let Some(last) = node.child((count - 1) as u32) {
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
            let singular = format!("{}{}", upper, rest);
            if singular.ends_with('s') || singular.ends_with("sh") || singular.ends_with("ch") {
                format!("{}es", singular)
            } else {
                format!("{}s", singular)
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
