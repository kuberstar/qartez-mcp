use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{ExtractedImport, ExtractedSymbol, ParseResult, SymbolKind};

pub struct BashSupport;

impl LanguageSupport for BashSupport {
    fn extensions(&self) -> &[&str] {
        &["sh", "bash"]
    }

    fn language_name(&self) -> &str {
        "bash"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_bash::LANGUAGE)
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

fn extract_from_node(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    match node.kind() {
        "function_definition" => {
            if let Some(sym) = extract_function(node, source) {
                symbols.push(sym);
            }
        }
        "variable_assignment" => {
            if let Some(sym) = extract_variable(node, source) {
                symbols.push(sym);
            }
        }
        "command" => {
            if let Some(imp) = extract_source_command(node, source) {
                imports.push(imp);
            } else {
                extract_export_variable(node, source, symbols);
            }
        }
        "declaration_command" => {
            extract_export_declaration(node, source, symbols);
        }
        _ => {}
    }

    for child in children(node) {
        extract_from_node(child, source, symbols, imports);
    }
}

fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if_statement" => 1,
        "elif_clause" => 1,
        "case_item" => 1,
        "for_statement" | "while_statement" => 1,
        "binary_expression" => {
            let op = node
                .child_by_field_name("operator")
                .or_else(|| node.child(1))
                .map(|n| node_text(n, source));
            match op.as_deref() {
                Some("&&") | Some("||") => 1,
                _ => 0,
            }
        }
        "pipeline" => {
            // +1 per pipe segment beyond the first
            children(node).filter(|c| c.kind() == "|").count() as u32
        }
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
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: Some(1 + body_cc),
        owner_type: None,
    })
}

fn extract_variable(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    // Only extract top-level variable assignments
    let parent = node.parent()?;
    if parent.kind() != "program" {
        return None;
    }
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Variable,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: false,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn extract_export_variable(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let mut child_iter = children(node);
    let cmd_name = match child_iter.next() {
        Some(n) if n.kind() == "command_name" => node_text(n, source),
        _ => return,
    };
    if cmd_name != "export" {
        return;
    }
    for arg in child_iter {
        let text = node_text(arg, source);
        let name = if let Some(eq_pos) = text.find('=') {
            text[..eq_pos].to_string()
        } else {
            text
        };
        if !name.is_empty() {
            symbols.push(ExtractedSymbol {
                name,
                kind: SymbolKind::Variable,
                line_start: node.start_position().row as u32 + 1,
                line_end: node.end_position().row as u32 + 1,
                signature: extract_signature(node, source),
                is_exported: true,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            });
        }
    }
}

fn extract_export_declaration(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let full = node_text(node, source);
    if !full.starts_with("export") {
        return;
    }
    for child in children(node) {
        if child.kind() == "variable_assignment"
            && let Some(name_node) = child.child_by_field_name("name")
        {
            let name = node_text(name_node, source);
            if !name.is_empty() {
                symbols.push(ExtractedSymbol {
                    name,
                    kind: SymbolKind::Variable,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    signature: extract_signature(node, source),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            }
        }
    }
}

fn extract_source_command(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let mut child_iter = children(node);
    let cmd_name_node = child_iter.next()?;
    if cmd_name_node.kind() != "command_name" {
        return None;
    }
    let cmd_name = node_text(cmd_name_node, source);
    if cmd_name != "source" && cmd_name != "." {
        return None;
    }
    let arg = child_iter.next()?;
    let path = unquote_bash(node_text(arg, source));
    if path.is_empty() {
        return None;
    }
    Some(ExtractedImport {
        source: path,
        specifiers: vec![],
        is_reexport: false,
    })
}

fn unquote_bash(s: String) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn extract_signature(node: Node, source: &[u8]) -> Option<String> {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    let text = std::str::from_utf8(&source[start..end]).ok()?;
    let first_line = text.lines().next().unwrap_or(text).trim();

    if first_line.is_empty() {
        return None;
    }

    let truncated = if first_line.len() > 200 {
        &first_line[..200]
    } else {
        first_line
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

    fn parse_bash(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_bash::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = BashSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_function_definition() {
        let result = parse_bash("greet() {\n  echo \"hello\"\n}\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_function_keyword_syntax() {
        let result = parse_bash("function setup {\n  echo \"setup\"\n}\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "setup");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_top_level_variable() {
        let result = parse_bash("MY_VAR=\"hello\"\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MY_VAR");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_source_command() {
        let result = parse_bash("source ./utils.sh\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "./utils.sh");
    }

    #[test]
    fn test_dot_source_command() {
        let result = parse_bash(". /etc/profile\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "/etc/profile");
    }

    #[test]
    fn test_empty_file() {
        let result = parse_bash("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_mixed_declarations() {
        let result = parse_bash(
            r#"#!/bin/bash
source ./config.sh

VERSION="1.0"

setup() {
  echo "setting up"
}

cleanup() {
  echo "cleaning up"
}
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"VERSION"));
        assert!(names.contains(&"setup"));
        assert!(names.contains(&"cleanup"));

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "./config.sh");
    }
}
