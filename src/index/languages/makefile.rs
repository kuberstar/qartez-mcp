use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{ExtractedImport, ExtractedSymbol, ParseResult, SymbolKind};

pub struct MakefileSupport;

impl LanguageSupport for MakefileSupport {
    fn extensions(&self) -> &[&str] {
        &["mk"]
    }

    fn filenames(&self) -> &[&str] {
        &["Makefile", "GNUmakefile", "makefile"]
    }

    fn language_name(&self) -> &str {
        "makefile"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_make::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let root = tree.root_node();
        let mut phony_targets: Vec<String> = Vec::new();

        // First pass: collect .PHONY targets
        for child in children(root) {
            if child.kind() == "rule" {
                collect_phony(child, source, &mut phony_targets);
            }
        }

        // Second pass: extract all symbols
        for child in children(root) {
            match child.kind() {
                "rule" => {
                    extract_rule(child, source, &mut symbols, &phony_targets);
                }
                "variable_assignment" => {
                    extract_variable(child, source, &mut symbols);
                }
                "include_directive" => {
                    extract_include(child, source, &mut imports);
                }
                _ => {}
            }
        }

        ParseResult {
            symbols,
            imports,
            references: Vec::new(),
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn collect_phony(node: Node, source: &[u8], phony_targets: &mut Vec<String>) {
    for child in children(node) {
        if child.kind() == "targets" {
            let text = node_text(child, source);
            if text.trim() == ".PHONY" {
                // Prerequisites of .PHONY are the phony target names
                for sibling in children(node) {
                    if sibling.kind() == "prerequisites" {
                        let prereqs = node_text(sibling, source);
                        for name in prereqs.split_whitespace() {
                            phony_targets.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
}

fn extract_rule(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    phony_targets: &[String],
) {
    for child in children(node) {
        if child.kind() == "targets" {
            let text = node_text(child, source);
            for target_name in text.split_whitespace() {
                // Skip special targets starting with dot (except .PHONY itself,
                // which we already handled)
                if target_name.starts_with('.') {
                    continue;
                }
                let is_phony = phony_targets.contains(&target_name.to_string());
                symbols.push(ExtractedSymbol {
                    name: target_name.to_string(),
                    kind: SymbolKind::Target,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    signature: extract_signature(node, source),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: is_phony,
                    complexity: None,
                });
            }
        }
    }
}

fn extract_variable(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let text = node_text(node, source);
    // Variable assignments look like: NAME = value, NAME := value, NAME ?= value
    let name = text
        .split(['=', ':', '?', '+'])
        .next()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
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
        });
    }
}

fn extract_include(node: Node, source: &[u8], imports: &mut Vec<ExtractedImport>) {
    let text = node_text(node, source);
    let path = text
        .strip_prefix("include")
        .or_else(|| text.strip_prefix("-include"))
        .or_else(|| text.strip_prefix("sinclude"))
        .unwrap_or("")
        .trim();
    if !path.is_empty() {
        imports.push(ExtractedImport {
            source: path.to_string(),
            specifiers: vec![],
            is_reexport: false,
        });
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

    fn parse_makefile(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_make::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = MakefileSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_simple_target() {
        let result = parse_makefile("build:\n\tcargo build\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "build");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Target));
    }

    #[test]
    fn test_variable() {
        let result = parse_makefile("CC = gcc\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "CC");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
    }

    #[test]
    fn test_phony() {
        let result = parse_makefile(".PHONY: build test\n\nbuild:\n\tcargo build\n\ntest:\n\tcargo test\n");
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"build"));
        assert!(names.contains(&"test"));
    }

    #[test]
    fn test_include() {
        let result = parse_makefile("include common.mk\n\nbuild:\n\techo done\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "common.mk");
    }

    #[test]
    fn test_mixed_makefile() {
        let result = parse_makefile(
            r#"CC = gcc
CFLAGS = -Wall

.PHONY: all clean

all: main.o
	$(CC) $(CFLAGS) -o app main.o

clean:
	rm -f *.o app
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"CC"));
        assert!(names.contains(&"CFLAGS"));
        assert!(names.contains(&"all"));
        assert!(names.contains(&"clean"));
    }
}
