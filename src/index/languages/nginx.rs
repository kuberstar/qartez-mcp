use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{ExtractedSymbol, ParseResult, SymbolKind};

pub struct NginxSupport;

impl LanguageSupport for NginxSupport {
    fn extensions(&self) -> &[&str] {
        &["nginx", "conf"]
    }

    fn language_name(&self) -> &str {
        "nginx"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_nginx::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_nodes(root, source, &mut symbols);
        ParseResult {
            symbols,
            imports: Vec::new(),
            references: Vec::new(),
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

/// Walk the AST produced by tree-sitter-nginx v1.x.
/// Top-level constructs are `attribute` nodes (with `keyword` + optional
/// `block` children). `location` blocks get their own dedicated node kind.
fn extract_nodes(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(node) {
        match child.kind() {
            "attribute" => {
                extract_attribute(child, source, symbols);
            }
            "location" => {
                extract_location(child, source, symbols);
            }
            "block" => {
                // Recurse into block bodies (e.g. http { ... })
                extract_nodes(child, source, symbols);
            }
            _ => {
                extract_nodes(child, source, symbols);
            }
        }
    }
}

/// An `attribute` in tree-sitter-nginx is any keyword-based construct.
/// `server { }` is an attribute with keyword="server" and a block child.
/// `upstream name { }` is an attribute with keyword="upstream", value="name".
/// Simple directives like `server_name x;` are attributes too.
fn extract_attribute(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let keyword = children(node)
        .find(|n| n.kind() == "keyword")
        .map(|n| node_text(n, source));
    let keyword = match keyword {
        Some(k) => k,
        None => return,
    };

    let has_block = children(node).any(|n| n.kind() == "block");

    match keyword.as_str() {
        "server" if has_block => {
            // Find server_name from inner attributes
            let server_name = find_inner_value(node, "server_name", source)
                .unwrap_or_else(|| "unnamed".to_string());
            symbols.push(ExtractedSymbol {
                name: format!("server:{server_name}"),
                kind: SymbolKind::Class,
                line_start: node.start_position().row as u32 + 1,
                line_end: node.end_position().row as u32 + 1,
                signature: Some(format!("server {{ # {server_name} }}")),
                is_exported: true,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
            });
            // Recurse into the block for location blocks etc.
            extract_nodes(node, source, symbols);
        }
        "upstream" => {
            // The upstream name is the first `value` child
            let name = children(node)
                .find(|n| n.kind() == "value")
                .map(|n| node_text(n, source).trim().to_string())
                .unwrap_or_default();
            if !name.is_empty() {
                symbols.push(ExtractedSymbol {
                    name: format!("upstream:{name}"),
                    kind: SymbolKind::Class,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    signature: Some(format!("upstream {name}")),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                });
            }
        }
        // Context blocks that just need recursion
        "http" | "events" | "stream" | "mail" if has_block => {
            extract_nodes(node, source, symbols);
        }
        _ => {}
    }
}

/// `location` is a dedicated node kind in tree-sitter-nginx with a
/// `location_route` child for the path.
fn extract_location(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let route = children(node)
        .find(|n| n.kind() == "location_route")
        .map(|n| node_text(n, source).trim().to_string())
        .unwrap_or_else(|| "/".to_string());

    symbols.push(ExtractedSymbol {
        name: format!("location {route}"),
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: Some(format!("location {route}")),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    });

    // Recurse for nested locations
    extract_nodes(node, source, symbols);
}

/// Find the value of an inner attribute by keyword name.
/// Scans `attribute` children looking for a matching `keyword` node and
/// returns the first `value` sibling's text.
fn find_inner_value(node: Node, keyword: &str, source: &[u8]) -> Option<String> {
    for child in children(node) {
        if child.kind() == "attribute" {
            let kw = children(child)
                .find(|n| n.kind() == "keyword")
                .map(|n| node_text(n, source));
            if kw.as_deref() == Some(keyword) {
                return children(child)
                    .find(|n| n.kind() == "value" || n.kind() == "numeric_literal")
                    .map(|n| node_text(n, source).trim().to_string());
            }
        }
        if child.kind() == "block"
            && let Some(found) = find_inner_value(child, keyword, source) {
                return Some(found);
            }
    }
    None
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

    fn parse_nginx(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_nginx::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = NginxSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_server_block() {
        let result = parse_nginx(
            r#"server {
    server_name example.com;
    listen 80;

    location / {
        proxy_pass http://backend;
    }
}
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"server:example.com"));
        assert!(names.iter().any(|n| n.starts_with("location")));
    }

    #[test]
    fn test_upstream_block() {
        let result = parse_nginx(
            r#"upstream backend {
    server 127.0.0.1:8080;
    server 127.0.0.1:8081;
}
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"upstream:backend"));
    }

    #[test]
    fn test_location_blocks() {
        let result = parse_nginx(
            r#"server {
    server_name api.example.com;

    location /api {
        proxy_pass http://api_backend;
    }

    location /static {
        root /var/www/static;
    }
}
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"server:api.example.com"));
        assert!(names.iter().any(|n| n.contains("/api")));
        assert!(names.iter().any(|n| n.contains("/static")));
    }
}
