use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{ExtractedImport, ExtractedSymbol, ParseResult, SymbolKind};

pub struct CssSupport;

impl LanguageSupport for CssSupport {
    fn extensions(&self) -> &[&str] {
        &["css", "scss"]
    }

    fn language_name(&self) -> &str {
        "css"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_css::LANGUAGE)
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
        "rule_set" => {
            extract_rule_set(node, source, symbols);
        }
        "keyframes_statement" => {
            if let Some(sym) = extract_keyframes(node, source) {
                symbols.push(sym);
            }
        }
        "import_statement" => {
            if let Some(imp) = extract_import(node, source) {
                imports.push(imp);
            }
            return;
        }
        "media_statement" => {
            if let Some(sym) = extract_media(node, source) {
                symbols.push(sym);
            }
            // Recurse into media block to find nested rules
            for child in children(node) {
                if child.kind() == "block" {
                    for block_child in children(child) {
                        extract_from_node(block_child, source, symbols, imports);
                    }
                }
            }
            return;
        }
        "declaration" => {
            if let Some(sym) = extract_custom_property(node, source) {
                symbols.push(sym);
            }
        }
        _ => {}
    }

    for child in children(node) {
        extract_from_node(child, source, symbols, imports);
    }
}

fn extract_rule_set(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(node) {
        if child.kind() == "selectors" {
            for selector in children(child) {
                extract_selectors(selector, node, source, symbols);
            }
            return;
        }
    }
}

fn extract_selectors(
    selector_node: Node,
    rule_node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
) {
    match selector_node.kind() {
        "class_selector" => {
            let name = extract_selector_name(selector_node, source, ".");
            if !name.is_empty() {
                symbols.push(ExtractedSymbol {
                    name,
                    kind: SymbolKind::Class,
                    line_start: rule_node.start_position().row as u32 + 1,
                    line_end: rule_node.end_position().row as u32 + 1,
                    signature: extract_signature(rule_node, source),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            }
        }
        "id_selector" => {
            let name = extract_selector_name(selector_node, source, "#");
            if !name.is_empty() {
                symbols.push(ExtractedSymbol {
                    name,
                    kind: SymbolKind::Variable,
                    line_start: rule_node.start_position().row as u32 + 1,
                    line_end: rule_node.end_position().row as u32 + 1,
                    signature: extract_signature(rule_node, source),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            }
        }
        _ => {
            // Recurse into compound selectors, descendant selectors, etc.
            for child in children(selector_node) {
                extract_selectors(child, rule_node, source, symbols);
            }
        }
    }
}

fn extract_selector_name(node: Node, source: &[u8], prefix: &str) -> String {
    for child in children(node) {
        if child.kind() == "class_name" || child.kind() == "id_name" {
            return format!("{}{}", prefix, node_text(child, source));
        }
    }
    // Fallback: parse from full text
    let full = node_text(node, source);
    if full.starts_with(prefix) {
        return full;
    }
    String::new()
}

fn extract_keyframes(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name = children(node)
        .find(|c| c.kind() == "keyframes_name")
        .map(|n| node_text(n, source))?;
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name: format!("@keyframes {}", name),
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn extract_media(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let full = node_text(node, source);
    let first_line = full.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        return None;
    }
    let sig = if let Some(brace_pos) = first_line.find('{') {
        first_line[..brace_pos].trim()
    } else {
        first_line
    };
    if sig.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name: sig.to_string(),
        kind: SymbolKind::Module,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: Some(sig.to_string()),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn extract_custom_property(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let prop_node = children(node).find(|c| c.kind() == "property_name")?;
    let name = node_text(prop_node, source);
    if !name.starts_with("--") {
        return None;
    }
    Some(ExtractedSymbol {
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
    })
}

fn extract_import(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    for child in children(node) {
        match child.kind() {
            "string_value" => {
                let path = node_text(child, source);
                let unquoted = unquote(path);
                if !unquoted.is_empty() {
                    return Some(ExtractedImport {
                        source: unquoted,
                        specifiers: vec![],
                        is_reexport: false,
                    });
                }
            }
            "call_expression" => {
                // url("path")
                for arg in children(child) {
                    if arg.kind() == "arguments" {
                        for val in children(arg) {
                            if val.kind() == "string_value" {
                                let path = node_text(val, source);
                                let unquoted = unquote(path);
                                if !unquoted.is_empty() {
                                    return Some(ExtractedImport {
                                        source: unquoted,
                                        specifiers: vec![],
                                        is_reexport: false,
                                    });
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn unquote(s: String) -> String {
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

    let sig = if let Some(brace_pos) = text.find('{') {
        text[..brace_pos].trim()
    } else {
        text.lines().next().unwrap_or(text).trim()
    };

    if sig.is_empty() {
        return None;
    }

    let truncated = if sig.len() > 200 { &sig[..200] } else { sig };
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

    fn parse_css(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_css::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = CssSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_class_selector() {
        let result = parse_css(".container { display: flex; }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, ".container");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_id_selector() {
        let result = parse_css("#header { height: 60px; }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "#header");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
    }

    #[test]
    fn test_keyframes() {
        let result = parse_css("@keyframes fadeIn { from { opacity: 0; } to { opacity: 1; } }");
        let kf: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function))
            .collect();
        assert_eq!(kf.len(), 1);
        assert_eq!(kf[0].name, "@keyframes fadeIn");
    }

    #[test]
    fn test_custom_property() {
        let result = parse_css(":root { --primary-color: #333; }");
        let vars: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.name.starts_with("--"))
            .collect();
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "--primary-color");
    }

    #[test]
    fn test_import() {
        let result = parse_css("@import \"reset.css\";");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "reset.css");
    }

    #[test]
    fn test_import_url() {
        let result = parse_css("@import url(\"fonts.css\");");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "fonts.css");
    }

    #[test]
    fn test_empty_file() {
        let result = parse_css("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_media_query() {
        let result = parse_css("@media (max-width: 768px) {\n  .mobile { display: block; }\n}");
        let media: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Module))
            .collect();
        assert_eq!(media.len(), 1);

        // Should also find nested class selector
        let classes: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, ".mobile");
    }

    #[test]
    fn test_mixed_declarations() {
        let result = parse_css(
            r#"@import "base.css";

:root {
  --main-bg: #fff;
  --text-color: #333;
}

.header { background: var(--main-bg); }

#app { margin: 0 auto; }

@keyframes slideIn {
  from { transform: translateX(-100%); }
  to { transform: translateX(0); }
}
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"--main-bg"));
        assert!(names.contains(&"--text-color"));
        assert!(names.contains(&".header"));
        assert!(names.contains(&"#app"));
        assert!(names.contains(&"@keyframes slideIn"));

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "base.css");
    }
}
