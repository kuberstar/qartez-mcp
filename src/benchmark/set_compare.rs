// Rust guideline compliant 2026-04-13
//! Programmatic set comparison for list-returning MCP tools.
//!
//! For tools that return lists (qartez_find, qartez_grep, qartez_refs,
//! qartez_unused, qartez_deps, qartez_outline), parses both MCP and non-MCP
//! outputs into comparable item sets, then computes precision and recall.

use std::collections::BTreeSet;

/// Precision/recall scores from comparing MCP vs non-MCP item sets.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SetComparisonScores {
    pub mcp_items: usize,
    pub non_mcp_items: usize,
    pub intersection: usize,
    pub precision: f64,
    pub recall: f64,
    /// Items present in MCP output but not non-MCP (capped at 5).
    pub mcp_only: Vec<String>,
    /// Items present in non-MCP output but not MCP (capped at 5).
    pub non_mcp_only: Vec<String>,
}

/// Maximum number of diff items to report per side.
const MAX_DIFF_ITEMS: usize = 5;

/// Tools where set comparison does not apply - they return prose,
/// statistics, or structured data that cannot be meaningfully
/// decomposed into comparable item sets.
const EXCLUDED_TOOLS: &[&str] = &[
    "qartez_stats",
    "qartez_project",
    "qartez_context",
    "qartez_cochange",
    "qartez_calls",
    "qartez_rename",
    "qartez_move",
    "qartez_rename_file",
];

/// Compares MCP and non-MCP outputs for a given tool, returning
/// precision/recall scores. Returns `None` for tools where set
/// comparison is not applicable.
pub fn compare(tool: &str, mcp_output: &str, non_mcp_output: &str) -> Option<SetComparisonScores> {
    if EXCLUDED_TOOLS.contains(&tool) {
        return None;
    }

    let mcp_set = parse_items(tool, mcp_output);
    let non_mcp_set = parse_items(tool, non_mcp_output);

    let intersection: BTreeSet<_> = mcp_set.intersection(&non_mcp_set).cloned().collect();

    let mcp_only: Vec<String> = mcp_set
        .difference(&non_mcp_set)
        .take(MAX_DIFF_ITEMS)
        .cloned()
        .collect();
    let non_mcp_only: Vec<String> = non_mcp_set
        .difference(&mcp_set)
        .take(MAX_DIFF_ITEMS)
        .cloned()
        .collect();

    let precision = if mcp_set.is_empty() {
        1.0
    } else {
        intersection.len() as f64 / mcp_set.len() as f64
    };
    let recall = if non_mcp_set.is_empty() {
        1.0
    } else {
        intersection.len() as f64 / non_mcp_set.len() as f64
    };

    Some(SetComparisonScores {
        mcp_items: mcp_set.len(),
        non_mcp_items: non_mcp_set.len(),
        intersection: intersection.len(),
        precision,
        recall,
        mcp_only,
        non_mcp_only,
    })
}

/// Dispatches to tool-specific parsers to extract comparable items.
fn parse_items(tool: &str, output: &str) -> BTreeSet<String> {
    match tool {
        "qartez_find" => parse_find_output(output),
        "qartez_grep" => parse_grep_output(output),
        "qartez_refs" => parse_refs_output(output),
        "qartez_unused" => parse_unused_output(output),
        "qartez_deps" => parse_deps_output(output),
        "qartez_outline" => parse_outline_output(output),
        _ => parse_generic_identifiers(output),
    }
}

/// Parses qartez_find output. Matches lines like:
/// - `+ SymbolName  function  src/foo.rs  L10-L20  pub fn ...`
/// - `SymbolName  function  src/foo.rs`
/// Also handles non-MCP grep output with `fn symbol_name` patterns.
fn parse_find_output(output: &str) -> BTreeSet<String> {
    let mut items = BTreeSet::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("---") {
            continue;
        }
        // MCP format: `+ <name> <kind> <file> ...` or `<name> <kind> <file>`
        if let Some(name) = extract_leading_symbol(trimmed) {
            items.insert(name);
        }
        // Non-MCP grep format: `file:line: ... fn/struct/enum Name ...`
        else if let Some(name) = extract_definition_from_grep(trimmed) {
            items.insert(name);
        }
    }
    items
}

/// Parses qartez_grep output. Matches lines like:
/// - `+ <name> <kind> <file>` (MCP symbol result lines)
/// - `file:line:content` (grep content output)
/// Extracts identifiers from both formats.
fn parse_grep_output(output: &str) -> BTreeSet<String> {
    let mut items = BTreeSet::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("---") {
            continue;
        }
        if let Some(name) = extract_leading_symbol(trimmed) {
            items.insert(name);
        } else if let Some(ident) = extract_identifier_from_content_line(trimmed) {
            items.insert(ident);
        }
    }
    items
}

/// Parses qartez_refs output. Extracts `file:line` pairs from reference
/// listings. Handles both MCP (`src/foo.rs:42`) and non-MCP
/// (`src/foo.rs:42:  use crate::...`) formats.
fn parse_refs_output(output: &str) -> BTreeSet<String> {
    let mut items = BTreeSet::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("---")
            || trimmed.starts_with("References for")
        {
            continue;
        }
        if let Some(file_line) = extract_file_line_pair(trimmed) {
            items.insert(file_line);
        }
    }
    items
}

/// Parses qartez_unused output. Extracts symbol names from unused symbol
/// listings.
fn parse_unused_output(output: &str) -> BTreeSet<String> {
    let mut items = BTreeSet::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("---")
            || trimmed.starts_with("Unused")
        {
            continue;
        }
        // MCP: `+ <name> <kind> <file>` or `<name> (<kind>) in <file>`
        if let Some(name) = extract_leading_symbol(trimmed) {
            items.insert(name);
        }
        // Non-MCP grep: `file:line: pub fn/struct/enum Name`
        else if let Some(name) = extract_definition_from_grep(trimmed) {
            items.insert(name);
        }
    }
    items
}

/// Parses qartez_deps output. Extracts file paths from dependency edge
/// listings. Handles arrows (`→`, `->`) and plain path lines.
fn parse_deps_output(output: &str) -> BTreeSet<String> {
    let mut items = BTreeSet::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("---")
            || trimmed.starts_with("Dependencies")
        {
            continue;
        }
        // MCP edge format: `src/a.rs → src/b.rs` or `src/a.rs -> src/b.rs`
        for segment in trimmed.split('→') {
            let part = segment.trim();
            // Handle the ASCII `->` arrow within segments left after '→' split
            for sub in part.split("->") {
                let path = sub.trim();
                if looks_like_path(path) {
                    items.insert(normalize_path(path));
                }
            }
        }
        // Plain path line
        if looks_like_path(trimmed) {
            items.insert(normalize_path(trimmed));
        }
    }
    items
}

/// Parses qartez_outline output. Extracts symbol names from outline rows.
/// Handles both MCP outline format and non-MCP full file reads.
fn parse_outline_output(output: &str) -> BTreeSet<String> {
    let mut items = BTreeSet::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("---") {
            continue;
        }
        // MCP outline: `+ name [L10-L20] - signature` or `- name [L10-L20]`
        if let Some(name) = extract_outline_symbol(trimmed) {
            items.insert(name);
        }
        // Non-MCP: raw source - extract fn/struct/enum/trait definitions
        else if let Some(name) = extract_definition_from_source(trimmed) {
            items.insert(name);
        }
    }
    items
}

/// Fallback parser for unknown tools - extracts Rust-like identifiers.
fn parse_generic_identifiers(output: &str) -> BTreeSet<String> {
    let mut items = BTreeSet::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(name) = extract_leading_symbol(trimmed) {
            items.insert(name);
        } else if let Some(name) = extract_definition_from_grep(trimmed) {
            items.insert(name);
        }
    }
    items
}

// ---- Extraction helpers ---------------------------------------------------

/// Extracts a symbol name from MCP-style leading-symbol lines:
/// `+ <name> ...` or `- <name> ...` where name is an identifier.
fn extract_leading_symbol(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("+ ")
        .or_else(|| line.strip_prefix("- "))?;
    let name = rest.split_whitespace().next()?;
    if is_rust_identifier(name) {
        Some(name.to_string())
    } else {
        None
    }
}

/// Extracts a definition name from grep content output lines like:
/// `src/foo.rs:42:pub fn bar_baz(...)` or `src/foo.rs:42:  struct Foo {`
fn extract_definition_from_grep(line: &str) -> Option<String> {
    // Skip to content after `file:line:` prefix
    let content = skip_file_line_prefix(line);
    extract_definition_from_source(content)
}

/// Extracts a definition name from raw source code lines:
/// `pub fn foo(`, `struct Bar {`, `enum Baz`, etc.
fn extract_definition_from_source(line: &str) -> Option<String> {
    let trimmed = line.trim().trim_start_matches("pub ");
    let trimmed = trimmed.trim_start_matches("pub(crate) ");

    for keyword in &[
        "fn ", "struct ", "enum ", "trait ", "const ", "type ", "static ",
    ] {
        if let Some(rest) = trimmed.strip_prefix(keyword) {
            let name = rest
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()?;
            if !name.is_empty() && is_rust_identifier(name) {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Extracts a `file:line` pair from reference-style output.
/// Handles `src/foo.rs:42`, `src/foo.rs:42: content`, and `L42` markers.
fn extract_file_line_pair(line: &str) -> Option<String> {
    // Direct `file:line` format
    let parts: Vec<&str> = line.splitn(3, ':').collect();
    if parts.len() >= 2 && looks_like_path(parts[0]) {
        if let Ok(_line_num) = parts[1].trim().parse::<u32>() {
            return Some(format!("{}:{}", normalize_path(parts[0]), parts[1].trim()));
        }
    }
    // MCP `file L42` format
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.len() >= 2 && looks_like_path(words[0]) {
        if let Some(line_str) = words[1].strip_prefix('L') {
            if line_str.parse::<u32>().is_ok() {
                return Some(format!("{}:{}", normalize_path(words[0]), line_str));
            }
        }
    }
    None
}

/// Extracts a symbol name from MCP outline format:
/// `+ name [L10-L20] - signature` or `- name - type`
fn extract_outline_symbol(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("+ ")
        .or_else(|| line.strip_prefix("- "))
        .or_else(|| line.strip_prefix("  + "))
        .or_else(|| line.strip_prefix("  - "))?;
    let name = rest.split(|c: char| c.is_whitespace() || c == '[').next()?;
    if is_rust_identifier(name) {
        Some(name.to_string())
    } else {
        None
    }
}

/// Extracts an identifier from a grep content line by finding the most
/// prominent Rust identifier in the content portion after `file:line:`.
fn extract_identifier_from_content_line(line: &str) -> Option<String> {
    let content = skip_file_line_prefix(line);
    // Try definition extraction first
    if let Some(name) = extract_definition_from_source(content) {
        return Some(name);
    }
    // Fallback: grab the first significant identifier from content
    for word in content.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if word.len() >= 2 && is_rust_identifier(word) && !is_rust_keyword(word) {
            return Some(word.to_string());
        }
    }
    None
}

/// Skips the `file:line:` prefix common in grep content output, returning
/// the content portion.
fn skip_file_line_prefix(line: &str) -> &str {
    let parts: Vec<&str> = line.splitn(3, ':').collect();
    if parts.len() >= 3 && looks_like_path(parts[0]) && parts[1].trim().parse::<u32>().is_ok() {
        parts[2]
    } else {
        line
    }
}

/// Returns true if the string looks like a file path.
fn looks_like_path(s: &str) -> bool {
    let s = s.trim();
    (s.contains('/') || s.contains('.')) && !s.starts_with("//") && !s.contains(' ') && s.len() > 2
}

/// Normalizes a file path by trimming whitespace and leading `./`.
fn normalize_path(path: &str) -> String {
    path.trim().trim_start_matches("./").to_string()
}

/// Returns true if the string is a valid Rust identifier
/// (starts with letter or underscore, contains only alphanumerics and underscores).
fn is_rust_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// Common Rust keywords that should not be extracted as symbol identifiers.
fn is_rust_keyword(s: &str) -> bool {
    matches!(
        s,
        "fn" | "pub"
            | "struct"
            | "enum"
            | "trait"
            | "impl"
            | "use"
            | "mod"
            | "let"
            | "mut"
            | "const"
            | "static"
            | "if"
            | "else"
            | "for"
            | "while"
            | "loop"
            | "match"
            | "return"
            | "self"
            | "super"
            | "crate"
            | "type"
            | "where"
            | "as"
            | "in"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qartez_find_output_parsing() {
        let mcp = "# Matches for `QartezServer`\n\
                    + QartezServer  struct  src/server/mod.rs  L22-L35  pub struct QartezServer\n\
                    + QartezServer  impl  src/server/mod.rs  L40-L200";
        let items = parse_find_output(mcp);
        assert!(items.contains("QartezServer"));
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn qartez_find_nonmcp_grep_parsing() {
        let non_mcp = "src/server/mod.rs:22:pub struct QartezServer {\n\
                        src/server/mod.rs:40:impl QartezServer {";
        let items = parse_find_output(non_mcp);
        assert!(items.contains("QartezServer"));
    }

    #[test]
    fn qartez_grep_output_parsing() {
        let mcp = "# grep: handle_*\n\
                    + handle_request  function  src/server/mod.rs\n\
                    + handle_tool  function  src/server/mod.rs";
        let items = parse_grep_output(mcp);
        assert!(items.contains("handle_request"));
        assert!(items.contains("handle_tool"));
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn qartez_grep_nonmcp_content_parsing() {
        let non_mcp = "src/server/mod.rs:100:    pub fn handle_request(&self) {\n\
                        src/server/mod.rs:200:    pub fn handle_tool(&self) {";
        let items = parse_grep_output(non_mcp);
        assert!(items.contains("handle_request"));
        assert!(items.contains("handle_tool"));
    }

    #[test]
    fn qartez_refs_file_line_parsing() {
        let mcp = "References for `foo`:\n\
                    src/lib.rs:10\n\
                    src/server/mod.rs:42\n\
                    src/graph/mod.rs:7";
        let non_mcp = "src/lib.rs:10:  use crate::foo;\n\
                        src/server/mod.rs:42:  foo();\n\
                        src/graph/mod.rs:7:  let x = foo;";
        let mcp_items = parse_refs_output(mcp);
        let non_mcp_items = parse_refs_output(non_mcp);
        assert_eq!(mcp_items.len(), 3);
        assert_eq!(non_mcp_items.len(), 3);
        assert_eq!(mcp_items, non_mcp_items);
    }

    #[test]
    fn empty_outputs_produce_perfect_scores() {
        let result = compare("qartez_find", "", "").unwrap();
        assert_eq!(result.mcp_items, 0);
        assert_eq!(result.non_mcp_items, 0);
        assert_eq!(result.precision, 1.0);
        assert_eq!(result.recall, 1.0);
    }

    #[test]
    fn disjoint_sets_produce_zero_scores() {
        let mcp = "+ alpha  function  src/a.rs\n+ beta  function  src/b.rs";
        let non_mcp = "src/c.rs:1:pub fn gamma() {}\nsrc/d.rs:2:pub fn delta() {}";
        let result = compare("qartez_find", mcp, non_mcp).unwrap();
        assert_eq!(result.intersection, 0);
        assert_eq!(result.precision, 0.0);
        assert_eq!(result.recall, 0.0);
        assert!(!result.mcp_only.is_empty());
        assert!(!result.non_mcp_only.is_empty());
    }

    #[test]
    fn identical_sets_produce_perfect_scores() {
        let mcp = "+ foo  function  src/a.rs\n+ bar  struct  src/b.rs";
        let non_mcp = "src/a.rs:1:pub fn foo() {}\nsrc/b.rs:2:pub struct bar {}";
        let result = compare("qartez_find", mcp, non_mcp).unwrap();
        assert_eq!(result.intersection, 2);
        assert_eq!(result.precision, 1.0);
        assert_eq!(result.recall, 1.0);
        assert!(result.mcp_only.is_empty());
        assert!(result.non_mcp_only.is_empty());
    }

    #[test]
    fn excluded_tools_return_none() {
        assert!(compare("qartez_stats", "foo", "bar").is_none());
        assert!(compare("qartez_project", "foo", "bar").is_none());
        assert!(compare("qartez_cochange", "foo", "bar").is_none());
        assert!(compare("qartez_rename", "foo", "bar").is_none());
        assert!(compare("qartez_move", "foo", "bar").is_none());
        assert!(compare("qartez_rename_file", "foo", "bar").is_none());
    }

    #[test]
    fn outline_symbol_extraction() {
        let mcp = "# Outline: src/foo.rs\n\
                    + new [L10-L20] - pub fn new() -> Self\n\
                    + run [L25-L50] - pub fn run(&self)\n\
                    - helper [L55-L60] - fn helper()";
        let items = parse_outline_output(mcp);
        assert!(items.contains("new"));
        assert!(items.contains("run"));
        assert!(items.contains("helper"));
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn deps_path_extraction() {
        let mcp = "Dependencies for src/server/mod.rs:\n\
                    src/server/mod.rs → src/storage/mod.rs\n\
                    src/server/mod.rs → src/graph/mod.rs";
        let items = parse_deps_output(mcp);
        assert!(items.contains("src/server/mod.rs"));
        assert!(items.contains("src/storage/mod.rs"));
        assert!(items.contains("src/graph/mod.rs"));
    }

    #[test]
    fn diff_items_capped_at_max() {
        let mut mcp_lines = String::new();
        for i in 0..10 {
            mcp_lines.push_str(&format!("+ sym_{i}  function  src/a.rs\n"));
        }
        let result = compare("qartez_find", &mcp_lines, "").unwrap();
        assert!(result.mcp_only.len() <= MAX_DIFF_ITEMS);
    }
}
