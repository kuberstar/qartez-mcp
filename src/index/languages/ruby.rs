use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct RubySupport;

impl LanguageSupport for RubySupport {
    fn extensions(&self) -> &[&str] {
        &["rb"]
    }

    fn language_name(&self) -> &str {
        "ruby"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_ruby::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let mut references = Vec::new();
        let root = tree.root_node();
        extract_from_node(root, source, 0, None, &mut symbols, &mut imports, &mut references);
        ParseResult {
            symbols,
            imports,
            references,
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    depth: usize,
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    match node.kind() {
        "method" => {
            if let Some(sym) = extract_method(node, source, depth) {
                let idx = symbols.len();
                symbols.push(sym);
                for child in children(node) {
                    extract_from_node(
                        child,
                        source,
                        depth,
                        Some(idx),
                        symbols,
                        imports,
                        references,
                    );
                }
                return;
            }
        }
        "singleton_method" => {
            if let Some(sym) = extract_singleton_method(node, source, depth) {
                let idx = symbols.len();
                symbols.push(sym);
                for child in children(node) {
                    extract_from_node(
                        child,
                        source,
                        depth,
                        Some(idx),
                        symbols,
                        imports,
                        references,
                    );
                }
                return;
            }
        }
        "class" => {
            if let Some(sym) = extract_class_or_module(node, source, SymbolKind::Class, depth) {
                symbols.push(sym);
            }
            for child in children(node) {
                extract_from_node(
                    child,
                    source,
                    depth + 1,
                    enclosing,
                    symbols,
                    imports,
                    references,
                );
            }
            return;
        }
        "module" => {
            if let Some(sym) = extract_class_or_module(node, source, SymbolKind::Module, depth) {
                symbols.push(sym);
            }
            for child in children(node) {
                extract_from_node(
                    child,
                    source,
                    depth + 1,
                    enclosing,
                    symbols,
                    imports,
                    references,
                );
            }
            return;
        }
        "assignment" => {
            if let Some(sym) = extract_constant_assignment(node, source, depth) {
                symbols.push(sym);
            }
        }
        "call" => {
            if let Some(imp) = extract_require(node, source) {
                imports.push(imp);
            }
        }
        _ => {}
    }

    record_reference(node, source, enclosing, references);

    for child in children(node) {
        extract_from_node(child, source, depth, enclosing, symbols, imports, references);
    }
}

fn record_reference(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    if node.kind() != "call" {
        return;
    }
    let name = node
        .child_by_field_name("method")
        .map(|m| node_text(m, source))
        .unwrap_or_default();
    if !name.is_empty() && !is_builtin_callable(&name) {
        references.push(ExtractedReference {
            name,
            line: node.start_position().row as u32 + 1,
            from_symbol_idx: enclosing,
            kind: ReferenceKind::Call,
        });
    }
}

fn is_builtin_callable(name: &str) -> bool {
    matches!(
        name,
        "puts"
            | "print"
            | "p"
            | "require"
            | "require_relative"
            | "include"
            | "extend"
            | "attr_reader"
            | "attr_writer"
            | "attr_accessor"
            | "raise"
            | "yield"
            | "super"
            | "lambda"
            | "proc"
            | "loop"
            | "sleep"
    )
}

fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if" | "elsif" | "unless" => 1,
        "when" => 1,
        "for" => 1,
        "while" | "until" => 1,
        "rescue" => 1,
        "binary" => {
            let op = node
                .child_by_field_name("operator")
                .map(|n| node_text(n, source));
            match op.as_deref() {
                Some("&&") | Some("||") | Some("and") | Some("or") => 1,
                _ => 0,
            }
        }
        "block" | "lambda" => return 0,
        _ => 0,
    };
    for child in children(node) {
        total += count_complexity(child, source);
    }
    total
}

fn extract_method(node: Node, source: &[u8], depth: usize) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let kind = if depth > 0 {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };
    let body_cc = node
        .child_by_field_name("body")
        .map(|body| count_complexity(body, source))
        .unwrap_or(0);
    Some(ExtractedSymbol {
        is_exported: true,
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: Some(1 + body_cc),
    })
}

fn extract_singleton_method(node: Node, source: &[u8], depth: usize) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let kind = if depth > 0 {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };
    let body_cc = node
        .child_by_field_name("body")
        .map(|body| count_complexity(body, source))
        .unwrap_or(0);
    Some(ExtractedSymbol {
        is_exported: true,
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: Some(1 + body_cc),
    })
}

fn extract_class_or_module(
    node: Node,
    source: &[u8],
    kind: SymbolKind,
    depth: usize,
) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        is_exported: depth == 0,
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    })
}

fn extract_constant_assignment(node: Node, source: &[u8], depth: usize) -> Option<ExtractedSymbol> {
    let left = node.child_by_field_name("left")?;
    if left.kind() != "constant" {
        return None;
    }
    let name = node_text(left, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        is_exported: depth == 0,
        name,
        kind: SymbolKind::Const,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    })
}

fn extract_require(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let method_node = node.child_by_field_name("method")?;
    let method_name = node_text(method_node, source);

    match method_name.as_str() {
        "require" | "require_relative" => {}
        _ => return None,
    }

    let args = node.child_by_field_name("arguments")?;
    for child in children(args) {
        if child.kind() == "argument_list" {
            for arg in children(child) {
                if arg.kind() == "string" {
                    let path = unquote_ruby_string(arg, source);
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
        if child.kind() == "string" {
            let path = unquote_ruby_string(child, source);
            if !path.is_empty() {
                return Some(ExtractedImport {
                    source: path,
                    specifiers: vec![],
                    is_reexport: false,
                });
            }
        }
    }
    None
}

fn unquote_ruby_string(node: Node, source: &[u8]) -> String {
    for child in children(node) {
        if child.kind() == "string_content" {
            return node_text(child, source);
        }
    }
    let raw = node_text(node, source);
    let trimmed = raw.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        raw
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

    fn parse_ruby(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_ruby::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = RubySupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_method_definition() {
        let result = parse_ruby("def greet(name)\n  puts name\nend\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_class() {
        let result = parse_ruby(
            "class MyService\n  def initialize(name)\n    @name = name\n  end\n\n  def run\n    puts @name\n  end\nend\n",
        );
        let classes: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "MyService");
        assert!(classes[0].is_exported);

        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 2);
    }

    #[test]
    fn test_module() {
        let result = parse_ruby("module Utils\n  def self.helper\n    42\n  end\nend\n");
        let mods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Module))
            .collect();
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].name, "Utils");
    }

    #[test]
    fn test_constant() {
        let result = parse_ruby("MAX_SIZE = 1024\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MAX_SIZE");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Const));
    }

    #[test]
    fn test_require() {
        let result = parse_ruby("require 'json'\nrequire_relative './helper'\n");
        assert_eq!(result.imports.len(), 2);
        assert_eq!(result.imports[0].source, "json");
        assert_eq!(result.imports[1].source, "./helper");
    }

    #[test]
    fn test_empty_file() {
        let result = parse_ruby("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_singleton_method() {
        let result = parse_ruby("class Foo\n  def self.create\n    new\n  end\nend\n");
        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "create");
    }

    #[test]
    fn test_mixed_declarations() {
        let result = parse_ruby(
            r#"require 'net/http'
require_relative './config'

VERSION = "1.0"

class AppService
  def initialize
    @running = false
  end

  def start
    @running = true
  end

  def self.create
    new
  end
end

def top_level_helper
  42
end
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"VERSION"));
        assert!(names.contains(&"AppService"));
        assert!(names.contains(&"initialize"));
        assert!(names.contains(&"start"));
        assert!(names.contains(&"create"));
        assert!(names.contains(&"top_level_helper"));

        assert_eq!(result.imports.len(), 2);
    }

    #[test]
    fn test_refs_call_attributed_to_method() {
        let result = parse_ruby(
            r#"class Svc
  def run
    self.process
  end

  def process
    42
  end
end
"#,
        );
        let run_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "run")
            .expect("run method");
        assert!(
            result.references.iter().any(|r| r.name == "process"
                && matches!(r.kind, ReferenceKind::Call)
                && r.from_symbol_idx == Some(run_idx)),
            "self.process inside run should be attributed to run, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_method_call_with_receiver() {
        let result = parse_ruby(
            r#"class Svc
  def run
    client.fetch
  end
end
"#,
        );
        let run_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "run")
            .expect("run method");
        assert!(
            result.references.iter().any(|r| r.name == "fetch"
                && matches!(r.kind, ReferenceKind::Call)
                && r.from_symbol_idx == Some(run_idx)),
            "client.fetch inside run should record fetch, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_builtin_filtered() {
        let result = parse_ruby(
            r#"def f
  puts "hello"
  print "world"
  raise "error"
end
"#,
        );
        assert!(
            !result
                .references
                .iter()
                .any(|r| r.name == "puts" || r.name == "print" || r.name == "raise"),
            "built-in calls must not be recorded as references"
        );
    }
}
