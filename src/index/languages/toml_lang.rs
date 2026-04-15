use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{ExtractedSymbol, ParseResult, SymbolKind};

pub struct TomlSupport;

impl LanguageSupport for TomlSupport {
    fn extensions(&self) -> &[&str] {
        &["toml"]
    }

    fn language_name(&self) -> &str {
        "toml"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_toml_ng::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_toml_nodes(root, source, &mut symbols);
        ParseResult {
            symbols,
            imports: Vec::new(),
            references: Vec::new(),
            ..Default::default()
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn extract_toml_nodes(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(node) {
        match child.kind() {
            "table" => {
                extract_table(child, source, symbols);
            }
            "table_array_element" => {
                extract_table_array(child, source, symbols);
            }
            "pair" => {
                extract_top_level_pair(child, source, symbols);
            }
            _ => {}
        }
    }
}

fn extract_table(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(node) {
        if child.kind().contains("key") || child.kind() == "dotted_key" {
            let name = node_text(child, source)
                .trim_matches(|c| c == '[' || c == ']')
                .trim()
                .to_string();
            if !name.is_empty() {
                symbols.push(ExtractedSymbol {
                    name,
                    kind: SymbolKind::Class,
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
            return;
        }
    }
}

fn extract_table_array(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(node) {
        if child.kind().contains("key") || child.kind() == "dotted_key" {
            let name = node_text(child, source)
                .trim_matches(|c| c == '[' || c == ']')
                .trim()
                .to_string();
            if !name.is_empty() {
                symbols.push(ExtractedSymbol {
                    name,
                    kind: SymbolKind::Class,
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
            return;
        }
    }
}

fn extract_top_level_pair(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    // Only extract pairs whose parent is the document root
    if let Some(parent) = node.parent()
        && parent.kind() != "document"
    {
        return;
    }
    for child in children(node) {
        if child.kind().contains("key") || child.kind() == "dotted_key" {
            let name = node_text(child, source).trim().to_string();
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
            return;
        }
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

    fn parse_toml(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_toml_ng::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = TomlSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_top_level_key() {
        let result = parse_toml("name = \"myproject\"\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "name");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
    }

    #[test]
    fn test_table() {
        let result = parse_toml("[package]\nname = \"foo\"\nversion = \"1.0\"\n");
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"package"));
    }

    #[test]
    fn test_table_array() {
        let result = parse_toml("[[bin]]\nname = \"mybin\"\npath = \"src/main.rs\"\n");
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"bin"));
    }

    #[test]
    fn test_cargo_toml() {
        let result = parse_toml(
            r#"[package]
name = "qartez-mcp"
version = "0.1.0"
edition = "2024"

[dependencies]
anyhow = "1.0"
serde = { version = "1.0", features = ["derive"] }

[[bin]]
name = "qartez-mcp"
path = "src/main.rs"
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"package"));
        assert!(names.contains(&"dependencies"));
        assert!(names.contains(&"bin"));
    }

    #[test]
    fn test_nested_table() {
        let result = parse_toml("[tool.pytest.ini_options]\nminversion = \"6.0\"\n");
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(!names.is_empty());
    }
}
