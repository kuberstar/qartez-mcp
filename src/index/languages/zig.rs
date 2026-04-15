use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{ExtractedImport, ExtractedSymbol, ParseResult, SymbolKind};

pub struct ZigSupport;

impl LanguageSupport for ZigSupport {
    fn extensions(&self) -> &[&str] {
        &["zig"]
    }

    fn language_name(&self) -> &str {
        "zig"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_zig::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let root = tree.root_node();
        extract_from_node(root, source, &mut symbols, &mut imports);
        ParseResult {
            symbols,
            imports,
            references: Vec::new(),
            ..Default::default()
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn has_pub_keyword(node: Node) -> bool {
    children(node).any(|child| child.kind() == "pub")
}

fn has_const_keyword(node: Node) -> bool {
    children(node).any(|child| child.kind() == "const")
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    match node.kind() {
        "function_declaration" => {
            if let Some(sym) = extract_function(node, source) {
                symbols.push(sym);
            }
            return;
        }
        "variable_declaration" => {
            extract_variable_declaration(node, source, symbols, imports);
            return;
        }
        "test_declaration" => {
            if let Some(sym) = extract_test(node, source) {
                symbols.push(sym);
            }
            return;
        }
        _ => {}
    }

    for child in children(node) {
        extract_from_node(child, source, symbols, imports);
    }
}

fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if_expression" => 1,
        "switch_case" => 1,
        "for_expression" => 1,
        "while_expression" => 1,
        "binary_expression" => {
            let op = node
                .child_by_field_name("operator")
                .map(|n| node_text(n, source));
            match op.as_deref() {
                Some("and") | Some("or") => 1,
                _ => 0,
            }
        }
        "catch" | "orelse" => 1,
        _ => 0,
    };
    for child in children(node) {
        total += count_complexity(child, source);
    }
    total
}

fn extract_function(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let body_cc = node
        .child_by_field_name("body")
        .map(|body| count_complexity(body, source))
        .unwrap_or(0);
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: has_pub_keyword(node),
        parent_idx: None,
        unused_excluded: false,
        complexity: Some(1 + body_cc),
        owner_type: None,
    })
}

/// Handles all `variable_declaration` nodes in Zig. The right-hand side
/// determines the symbol kind: `struct_declaration`, `enum_declaration`,
/// `union_declaration`, and `error_set_declaration` produce typed symbols,
/// while a `builtin_function` whose identifier is `@import` produces an
/// import entry. Plain values fall back to `Const` or `Variable` depending
/// on whether the declaration uses `const` or `var`.
fn extract_variable_declaration(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    let name = find_identifier_child(node, source);
    if name.is_empty() {
        return;
    }

    let is_pub = has_pub_keyword(node);

    let rhs = find_rhs_node(node);

    match rhs.map(|n| n.kind()) {
        Some("struct_declaration") => {
            symbols.push(make_symbol(name, SymbolKind::Struct, node, source, is_pub));
        }
        Some("enum_declaration") => {
            symbols.push(make_symbol(name, SymbolKind::Enum, node, source, is_pub));
        }
        Some("union_declaration") => {
            symbols.push(make_symbol(name, SymbolKind::Struct, node, source, is_pub));
        }
        Some("error_set_declaration") => {
            symbols.push(make_symbol(name, SymbolKind::Enum, node, source, is_pub));
        }
        Some("builtin_function") => {
            let rhs_node = rhs.unwrap();
            if is_import_call(rhs_node, source) {
                if let Some(imp) = extract_import(rhs_node, source) {
                    imports.push(imp);
                }
            } else {
                let kind = if has_const_keyword(node) {
                    SymbolKind::Const
                } else {
                    SymbolKind::Variable
                };
                symbols.push(make_symbol(name, kind, node, source, is_pub));
            }
        }
        Some("field_expression") => {
            let rhs_node = rhs.unwrap();
            if let Some(inner) = find_builtin_function_in_field_expr(rhs_node)
                && is_import_call(inner, source)
            {
                if let Some(imp) = extract_import(inner, source) {
                    imports.push(imp);
                }
                return;
            }
            let kind = if has_const_keyword(node) {
                SymbolKind::Const
            } else {
                SymbolKind::Variable
            };
            symbols.push(make_symbol(name, kind, node, source, is_pub));
        }
        _ => {
            let kind = if has_const_keyword(node) {
                SymbolKind::Const
            } else {
                SymbolKind::Variable
            };
            symbols.push(make_symbol(name, kind, node, source, is_pub));
        }
    }
}

/// Finds the first named `identifier` child of a node (skipping unnamed
/// keyword tokens like `pub`, `const`, `var`).
fn find_identifier_child(node: Node, source: &[u8]) -> String {
    for child in children(node) {
        if child.is_named() && child.kind() == "identifier" {
            return node_text(child, source);
        }
    }
    String::new()
}

/// Finds the right-hand side value node in a `variable_declaration`. This is
/// the first named child after the `=` token, skipping the identifier, type
/// annotation, and keywords.
fn find_rhs_node(node: Node) -> Option<Node> {
    let mut past_eq = false;
    for child in children(node) {
        if !child.is_named() && child.kind() == "=" {
            past_eq = true;
            continue;
        }
        if past_eq && child.is_named() && child.kind() != ";" {
            return Some(child);
        }
    }
    None
}

/// Checks whether a `builtin_function` node represents an `@import(...)` call.
fn is_import_call(node: Node, source: &[u8]) -> bool {
    for child in children(node) {
        if child.kind() == "builtin_identifier" {
            return node_text(child, source) == "@import";
        }
    }
    false
}

/// Locates a `builtin_function` child inside a `field_expression` (handles
/// `@import("std").mem` patterns).
fn find_builtin_function_in_field_expr(node: Node) -> Option<Node> {
    children(node).find(|&child| child.kind() == "builtin_function")
}

/// Extracts an import path from a `builtin_function` node that is an
/// `@import(...)` call. Pulls the string content from the first argument.
fn extract_import(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    for child in children(node) {
        if child.kind() == "arguments" {
            for arg in children(child) {
                if arg.kind() == "string" {
                    let path = extract_string_content(arg, source);
                    if !path.is_empty() {
                        return Some(ExtractedImport {
                            source: path,
                            specifiers: vec![],
                            is_reexport: false,
                        });
                    }
                }
            }
        }
    }
    None
}

/// Extracts the inner text of a `string` node by finding its `string_content`
/// child.
fn extract_string_content(string_node: Node, source: &[u8]) -> String {
    for child in children(string_node) {
        if child.kind() == "string_content" {
            return node_text(child, source);
        }
    }
    String::new()
}

fn extract_test(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name = find_test_name(node, source);
    if name.is_empty() {
        return None;
    }
    let body_cc = node
        .child_by_field_name("body")
        .map(|body| count_complexity(body, source))
        .unwrap_or(0);
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: false,
        parent_idx: None,
        unused_excluded: false,
        complexity: Some(1 + body_cc),
        owner_type: None,
    })
}

/// Finds the test name from a `test_declaration` node. The name is stored as
/// a `string` child (e.g., `test "basic addition" { ... }`).
fn find_test_name(node: Node, source: &[u8]) -> String {
    for child in children(node) {
        if child.kind() == "string" {
            let content = extract_string_content(child, source);
            if !content.is_empty() {
                return content;
            }
        }
    }
    String::new()
}

fn make_symbol(
    name: String,
    kind: SymbolKind,
    node: Node,
    source: &[u8],
    is_exported: bool,
) -> ExtractedSymbol {
    ExtractedSymbol {
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    }
}

fn extract_signature(node: Node, source: &[u8]) -> Option<String> {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    let text = std::str::from_utf8(&source[start..end]).ok()?;

    let sig = if let Some(brace_pos) = text.find('{') {
        text[..brace_pos].trim()
    } else {
        text.lines().next().unwrap_or(text).trim()
    };

    if sig.is_empty() {
        return None;
    }

    let truncated = if sig.len() > 200 {
        &sig[..sig.floor_char_boundary(200)]
    } else {
        sig
    };
    Some(truncated.to_string())
}

fn node_text(node: Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    std::str::from_utf8(&source[start..end])
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_zig(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_zig::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = ZigSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_pub_function() {
        let result = parse_zig("pub fn add(a: u32, b: u32) u32 {\n    return a + b;\n}\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "add");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
        assert!(result.symbols[0].signature.is_some());
    }

    #[test]
    fn test_private_function() {
        let result = parse_zig("fn helper() void {}\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "helper");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_struct_definition() {
        let result = parse_zig("pub const Point = struct {\n    x: f64,\n    y: f64,\n};\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Point");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Struct));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_private_struct() {
        let result = parse_zig("const InternalState = struct {\n    value: i32,\n};\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "InternalState");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Struct));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_enum_definition() {
        let result = parse_zig("pub const Color = enum {\n    red,\n    green,\n    blue,\n};\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Color");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Enum));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_const_declaration() {
        let result = parse_zig("pub const MAX_SIZE: usize = 1024;\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MAX_SIZE");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Const));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_private_const() {
        let result = parse_zig("const internal_val: u32 = 42;\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "internal_val");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Const));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_var_declaration() {
        let result = parse_zig("pub var global_state: u32 = 0;\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "global_state");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_private_var() {
        let result = parse_zig("var local_state: u32 = 0;\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "local_state");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_import() {
        let result = parse_zig("const std = @import(\"std\");\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "std");
        assert!(result.symbols.is_empty());
    }

    #[test]
    fn test_import_field_access() {
        let result = parse_zig("const mem = @import(\"std\").mem;\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "std");
        assert!(result.symbols.is_empty());
    }

    #[test]
    fn test_test_declaration() {
        let result = parse_zig("test \"basic addition\" {\n    _ = 1 + 2;\n}\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "basic addition");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_union_definition() {
        let result =
            parse_zig("pub const Token = union(enum) {\n    int: i32,\n    float: f64,\n};\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Token");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Struct));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_error_set() {
        let result = parse_zig("pub const MyError = error{OutOfMemory, InvalidInput};\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MyError");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Enum));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_empty_file() {
        let result = parse_zig("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_mixed_zig() {
        let result = parse_zig(
            r#"const std = @import("std");
const mem = @import("std").mem;

pub fn add(a: u32, b: u32) u32 {
    return a + b;
}

fn helper() void {}

pub const Point = struct {
    x: f64,
    y: f64,
};

pub const Color = enum {
    red,
    green,
    blue,
};

pub const MAX_SIZE: usize = 1024;
const internal_val: u32 = 42;

pub var global_state: u32 = 0;
var local_state: u32 = 0;

test "basic addition" {
    const result = add(2, 3);
    _ = result;
}
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"add"));
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"Point"));
        assert!(names.contains(&"Color"));
        assert!(names.contains(&"MAX_SIZE"));
        assert!(names.contains(&"internal_val"));
        assert!(names.contains(&"global_state"));
        assert!(names.contains(&"local_state"));
        assert!(names.contains(&"basic addition"));

        let exported_count = result.symbols.iter().filter(|s| s.is_exported).count();
        assert_eq!(exported_count, 5);

        assert_eq!(result.imports.len(), 2);
        assert_eq!(result.imports[0].source, "std");
        assert_eq!(result.imports[1].source, "std");
    }
}
