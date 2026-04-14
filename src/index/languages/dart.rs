use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{ExtractedImport, ExtractedSymbol, ParseResult, SymbolKind};

pub struct DartSupport;

impl LanguageSupport for DartSupport {
    fn extensions(&self) -> &[&str] {
        &["dart"]
    }

    fn language_name(&self) -> &str {
        "dart"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_dart::LANGUAGE)
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
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn is_dart_exported(name: &str) -> bool {
    !name.starts_with('_')
}

fn has_abstract_child(node: Node) -> bool {
    children(node).any(|c| c.kind() == "abstract")
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    match node.kind() {
        "class_declaration" => {
            if let Some(sym) = extract_class(node, source) {
                let idx = symbols.len();
                symbols.push(sym);
                extract_class_body(node, source, idx, symbols);
            }
            return;
        }
        "mixin_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Trait) {
                let idx = symbols.len();
                symbols.push(sym);
                extract_class_body(node, source, idx, symbols);
            }
            return;
        }
        "enum_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Enum) {
                symbols.push(sym);
            }
            return;
        }
        "extension_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Class) {
                let idx = symbols.len();
                symbols.push(sym);
                extract_class_body(node, source, idx, symbols);
            }
            return;
        }
        "type_alias" => {
            if let Some(sym) = extract_type_alias(node, source) {
                symbols.push(sym);
            }
            return;
        }
        "function_signature" => {
            if is_top_level(node)
                && let Some(sym) = extract_function(node, source) {
                    symbols.push(sym);
                }
            return;
        }
        "import_or_export" => {
            if let Some(imp) = extract_import(node, source) {
                imports.push(imp);
            }
            return;
        }
        "part_directive" => {
            if let Some(imp) = extract_part(node, source) {
                imports.push(imp);
            }
            return;
        }
        "static_final_declaration_list" => {
            if is_top_level(node) {
                let kind = resolve_variable_kind(node);
                extract_variable_list(node, source, kind, symbols);
            }
            return;
        }
        "initialized_identifier_list" => {
            if is_top_level(node) {
                extract_initialized_list(node, source, symbols);
            }
            return;
        }
        _ => {}
    }

    for child in children(node) {
        extract_from_node(child, source, symbols, imports);
    }
}

fn is_top_level(node: Node) -> bool {
    node.parent()
        .map(|p| p.kind() == "source_file")
        .unwrap_or(false)
}

fn resolve_variable_kind(node: Node) -> SymbolKind {
    if let Some(prev) = node.prev_sibling()
        && prev.kind() == "const" {
            return SymbolKind::Const;
        }
    SymbolKind::Variable
}

fn extract_class(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let sig = if has_abstract_child(node) {
        let base = extract_signature(node, source);
        base.or_else(|| Some(format!("abstract class {name}")))
    } else {
        extract_signature(node, source)
    };
    Some(ExtractedSymbol {
        is_exported: is_dart_exported(&name),
        name,
        kind: SymbolKind::Class,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: sig,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    })
}

fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if_statement" => 1,
        "switch_statement_case" => 1,
        "for_statement" | "while_statement" | "do_statement" => 1,
        "catch_clause" => 1,
        "conditional_expression" => 1,
        "binary_expression" => {
            let op = node
                .child_by_field_name("operator")
                .map(|n| node_text(n, source));
            match op.as_deref() {
                Some("&&") | Some("||") => 1,
                _ => 0,
            }
        }
        _ => 0,
    };
    for child in children(node) {
        total += count_complexity(child, source);
    }
    total
}

fn extract_named_decl(node: Node, source: &[u8], kind: SymbolKind) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        is_exported: is_dart_exported(&name),
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

fn extract_type_alias(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = children(node).find(|c| c.kind() == "type_identifier")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        is_exported: is_dart_exported(&name),
        name,
        kind: SymbolKind::Type,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    })
}

fn extract_function(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let body_node = node.next_sibling().filter(|s| s.kind() == "function_body");
    let line_end = body_node
        .map(|b| b.end_position().row as u32 + 1)
        .unwrap_or(node.end_position().row as u32 + 1);
    let body_cc = body_node
        .map(|body| count_complexity(body, source))
        .unwrap_or(0);
    Some(ExtractedSymbol {
        is_exported: is_dart_exported(&name),
        name,
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: Some(1 + body_cc),
    })
}

fn extract_class_body(
    class_node: Node,
    source: &[u8],
    class_idx: usize,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let body = children(class_node)
        .find(|c| c.kind() == "class_body" || c.kind() == "extension_body");
    let body = match body {
        Some(b) => b,
        None => return,
    };
    for member in children(body) {
        if member.kind() != "class_member" {
            continue;
        }
        if let Some(sym) = extract_member(member, source, class_idx) {
            symbols.push(sym);
        }
    }
}

fn extract_member(
    member: Node,
    source: &[u8],
    parent_idx: usize,
) -> Option<ExtractedSymbol> {
    for child in children(member) {
        match child.kind() {
            "method_signature" => {
                return extract_method_from_signature(child, source, parent_idx, member);
            }
            "declaration" => {
                for inner in children(child) {
                    match inner.kind() {
                        "function_signature" | "getter_signature" | "setter_signature"
                        | "constructor_signature" => {
                            let name_node = inner.child_by_field_name("name")?;
                            let name = node_text(name_node, source);
                            if name.is_empty() {
                                return None;
                            }
                            let member_cc = member
                                .child_by_field_name("body")
                                .or_else(|| children(member).find(|c| c.kind() == "function_body"))
                                .map(|body| count_complexity(body, source))
                                .unwrap_or(0);
                            return Some(ExtractedSymbol {
                                is_exported: is_dart_exported(&name),
                                name,
                                kind: SymbolKind::Method,
                                line_start: member.start_position().row as u32 + 1,
                                line_end: member.end_position().row as u32 + 1,
                                signature: extract_signature(inner, source),
                                parent_idx: Some(parent_idx),
                                unused_excluded: false,
                                complexity: Some(1 + member_cc),
                            });
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_method_from_signature(
    sig_node: Node,
    source: &[u8],
    parent_idx: usize,
    member: Node,
) -> Option<ExtractedSymbol> {
    for child in children(sig_node) {
        match child.kind() {
            "function_signature" | "getter_signature" | "setter_signature"
            | "constructor_signature" | "factory_constructor_signature" => {
                let name_node = child.child_by_field_name("name")?;
                let name = node_text(name_node, source);
                if name.is_empty() {
                    return None;
                }
                let sig_cc = member
                    .child_by_field_name("body")
                    .or_else(|| children(member).find(|c| c.kind() == "function_body"))
                    .map(|body| count_complexity(body, source))
                    .unwrap_or(0);
                return Some(ExtractedSymbol {
                    is_exported: is_dart_exported(&name),
                    name,
                    kind: SymbolKind::Method,
                    line_start: member.start_position().row as u32 + 1,
                    line_end: member.end_position().row as u32 + 1,
                    signature: extract_signature(child, source),
                    parent_idx: Some(parent_idx),
                    unused_excluded: false,
                    complexity: Some(1 + sig_cc),
                });
            }
            _ => {}
        }
    }
    None
}

fn extract_import(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let uri = find_configurable_uri(node, source)?;
    if uri.is_empty() {
        return None;
    }
    Some(ExtractedImport {
        source: uri,
        specifiers: vec![],
        is_reexport: false,
    })
}

fn find_configurable_uri(node: Node, source: &[u8]) -> Option<String> {
    for child in children(node) {
        if child.kind() == "library_import" || child.kind() == "import_specification" {
            return find_configurable_uri(child, source);
        }
        if child.kind() == "configurable_uri" {
            let raw = node_text(child, source);
            return Some(unquote_dart(raw));
        }
    }
    None
}

fn extract_part(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let uri_node = children(node).find(|c| c.kind() == "uri")?;
    let raw = node_text(uri_node, source);
    let path = unquote_dart(raw);
    if path.is_empty() {
        return None;
    }
    Some(ExtractedImport {
        source: path,
        specifiers: vec![],
        is_reexport: true,
    })
}

fn extract_variable_list(
    node: Node,
    source: &[u8],
    kind: SymbolKind,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    for child in children(node) {
        if child.kind() == "static_final_declaration" {
            let name_node = children(child).find(|c| c.kind() == "identifier");
            if let Some(name_node) = name_node {
                let name = node_text(name_node, source);
                if !name.is_empty() {
                    symbols.push(ExtractedSymbol {
                        is_exported: is_dart_exported(&name),
                        name,
                        kind: kind.clone(),
                        line_start: node.start_position().row as u32 + 1,
                        line_end: node.end_position().row as u32 + 1,
                        signature: extract_signature(node, source),
                        parent_idx: None,
                        unused_excluded: false,
                        complexity: None,
                    });
                }
            }
        }
    }
}

fn extract_initialized_list(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
) {
    for child in children(node) {
        if child.kind() == "initialized_identifier" {
            let name_node = children(child).find(|c| c.kind() == "identifier");
            if let Some(name_node) = name_node {
                let name = node_text(name_node, source);
                if !name.is_empty() {
                    symbols.push(ExtractedSymbol {
                        is_exported: is_dart_exported(&name),
                        name,
                        kind: SymbolKind::Variable,
                        line_start: node.start_position().row as u32 + 1,
                        line_end: node.end_position().row as u32 + 1,
                        signature: extract_signature(node, source),
                        parent_idx: None,
                        unused_excluded: false,
                        complexity: None,
                    });
                }
            }
        }
    }
}

fn unquote_dart(s: String) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('\'') && trimmed.ends_with('\''))
        || (trimmed.starts_with('"') && trimmed.ends_with('"'))
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

    fn parse_dart(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_dart::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = DartSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_class_definition() {
        let result = parse_dart("class MyService { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MyService");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_abstract_class() {
        let result = parse_dart("abstract class Animal {\n  void speak();\n}");
        let classes: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Animal");
        assert!(classes[0].is_exported);
    }

    #[test]
    fn test_enum_definition() {
        let result = parse_dart("enum Color { red, green, blue }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Color");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Enum));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_top_level_function() {
        let result = parse_dart("void greet(String name) {\n  print(name);\n}");
        let funcs: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function))
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "greet");
        assert!(funcs[0].is_exported);
    }

    #[test]
    fn test_private_function() {
        let result = parse_dart("String _privateHelper() {\n  return 'secret';\n}");
        let funcs: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function))
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "_privateHelper");
        assert!(!funcs[0].is_exported);
    }

    #[test]
    fn test_import_statement() {
        let result = parse_dart("import 'package:flutter/material.dart';");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "package:flutter/material.dart");
        assert!(!result.imports[0].is_reexport);
    }

    #[test]
    fn test_mixin_definition() {
        let result = parse_dart("mixin Swimming {\n  void swim() {}\n}");
        let mixins: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Trait))
            .collect();
        assert_eq!(mixins.len(), 1);
        assert_eq!(mixins[0].name, "Swimming");
        assert!(mixins[0].is_exported);
    }

    #[test]
    fn test_empty_file() {
        let result = parse_dart("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_mixed_dart() {
        let result = parse_dart(
            r#"import 'package:flutter/material.dart';
import 'dart:async';
part 'model.dart';

abstract class Animal {
  void speak();
}

class Dog extends Animal {
  final String name;
  Dog(this.name);
  void speak() {}
  void _helper() {}
}

mixin Swimming {
  void swim() {}
}

enum Color { red, green, blue }

extension StringExt on String {
  bool get isBlank => trim().isEmpty;
}

typedef IntList = List<int>;

void greet(String name) {
  print(name);
}

String _privateHelper() {
  return 'secret';
}

const maxRetries = 3;
final appName = 'MyApp';
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Animal"), "missing Animal, got {names:?}");
        assert!(names.contains(&"Dog"), "missing Dog, got {names:?}");
        assert!(names.contains(&"Swimming"), "missing Swimming, got {names:?}");
        assert!(names.contains(&"Color"), "missing Color, got {names:?}");
        assert!(names.contains(&"StringExt"), "missing StringExt, got {names:?}");
        assert!(names.contains(&"IntList"), "missing IntList, got {names:?}");
        assert!(names.contains(&"greet"), "missing greet, got {names:?}");
        assert!(names.contains(&"_privateHelper"), "missing _privateHelper, got {names:?}");
        assert!(names.contains(&"maxRetries"), "missing maxRetries, got {names:?}");
        assert!(names.contains(&"appName"), "missing appName, got {names:?}");

        let private_fn = result
            .symbols
            .iter()
            .find(|s| s.name == "_privateHelper")
            .unwrap();
        assert!(!private_fn.is_exported);

        let swimming = result
            .symbols
            .iter()
            .find(|s| s.name == "Swimming")
            .unwrap();
        assert!(matches!(swimming.kind, SymbolKind::Trait));

        let max_retries = result
            .symbols
            .iter()
            .find(|s| s.name == "maxRetries")
            .unwrap();
        assert!(matches!(max_retries.kind, SymbolKind::Const));

        let app_name = result
            .symbols
            .iter()
            .find(|s| s.name == "appName")
            .unwrap();
        assert!(matches!(app_name.kind, SymbolKind::Variable));

        let int_list = result
            .symbols
            .iter()
            .find(|s| s.name == "IntList")
            .unwrap();
        assert!(matches!(int_list.kind, SymbolKind::Type));

        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method) && s.parent_idx.is_some())
            .collect();
        assert!(
            methods.iter().any(|m| m.name == "speak"),
            "missing method speak, got {methods:?}"
        );

        assert_eq!(result.imports.len(), 3);
        let part = result
            .imports
            .iter()
            .find(|i| i.source == "model.dart")
            .unwrap();
        assert!(part.is_reexport);
    }
}
