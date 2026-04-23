use tree_sitter::{Language, Node};

use super::LanguageSupport;
use super::common::{self, children, node_text};
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct ScalaSupport;

impl LanguageSupport for ScalaSupport {
    fn extensions(&self) -> &[&str] {
        &["scala", "sc"]
    }

    fn language_name(&self) -> &str {
        "scala"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_scala::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let mut references = Vec::new();
        let root = tree.root_node();
        extract_from_node(
            root,
            source,
            None,
            &mut symbols,
            &mut imports,
            &mut references,
        );
        ParseResult {
            symbols,
            imports,
            references,
            ..Default::default()
        }
    }
}

fn has_modifier(node: Node, source: &[u8], modifier: &str) -> bool {
    children(node).any(|child| {
        child.kind() == "modifiers"
            && children(child).any(|m| {
                let text = node_text(m, source);
                text == modifier
            })
    })
}

fn is_private_or_protected(node: Node, source: &[u8]) -> bool {
    has_modifier(node, source, "private") || has_modifier(node, source, "protected")
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    let mut new_enclosing = enclosing;
    match node.kind() {
        "class_definition" => {
            let kind = SymbolKind::Class;
            if let Some(sym) = extract_named_decl(node, source, kind) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
            extract_class_body(node, source, new_enclosing, symbols, imports, references);
            return;
        }
        "object_definition" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Module) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
            extract_class_body(node, source, new_enclosing, symbols, imports, references);
            return;
        }
        "trait_definition" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Trait) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
            extract_class_body(node, source, new_enclosing, symbols, imports, references);
            return;
        }
        "function_definition" => {
            let kind = if enclosing.is_some() {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            };
            if let Some(sym) = extract_named_decl(node, source, kind) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
        }
        "val_definition" | "var_definition" => {
            if let Some(sym) = extract_val_var(node, source) {
                symbols.push(sym);
            }
        }
        "type_definition" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Type) {
                symbols.push(sym);
            }
        }
        "import_declaration" => {
            if let Some(imp) = extract_import(node, source) {
                imports.push(imp);
            }
            return;
        }
        "package_clause" => {
            if let Some(sym) = extract_package(node, source) {
                symbols.push(sym);
            }
            return;
        }
        _ => {}
    }

    record_reference(node, source, enclosing, references);

    for child in children(node) {
        extract_from_node(child, source, new_enclosing, symbols, imports, references);
    }
}

fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if_expression" => 1,
        "case_clause" => 1,
        "for_expression" => 1,
        "while_expression" => 1,
        "catch_clause" => 1,
        "infix_expression" => {
            let op = node
                .child_by_field_name("operator")
                .map(|n| node_text(n, source));
            match op.as_deref() {
                Some("&&") | Some("||") => 1,
                _ => 0,
            }
        }
        "lambda_expression" => return 0,
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
    let complexity = match kind {
        SymbolKind::Function | SymbolKind::Method => {
            let body_cc = node
                .child_by_field_name("body")
                .map(|body| count_complexity(body, source))
                .unwrap_or(0);
            Some(1 + body_cc)
        }
        _ => None,
    };
    Some(ExtractedSymbol {
        is_exported: !is_private_or_protected(node, source),
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity,
        owner_type: None,
    })
}

#[expect(
    clippy::only_used_in_recursion,
    reason = "imports/refs collected in nested classes"
)]
fn extract_class_body(
    class_node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    let body = children(class_node)
        .find(|c| c.kind() == "template_body" || c.kind() == "class_body" || c.kind() == "block");
    let body = match body {
        Some(b) => b,
        None => return,
    };
    for child in children(body) {
        match child.kind() {
            "function_definition" => {
                if let Some(mut sym) = extract_named_decl(child, source, SymbolKind::Method) {
                    sym.kind = SymbolKind::Method;
                    let idx = symbols.len();
                    symbols.push(sym);
                    walk_body_references(child, source, Some(idx), references);
                }
            }
            "val_definition" | "var_definition" => {
                if let Some(sym) = extract_val_var(child, source) {
                    symbols.push(sym);
                }
            }
            "type_definition" => {
                if let Some(sym) = extract_named_decl(child, source, SymbolKind::Type) {
                    symbols.push(sym);
                }
            }
            "class_definition" => {
                if let Some(sym) = extract_named_decl(child, source, SymbolKind::Class) {
                    let idx = symbols.len();
                    symbols.push(sym);
                    extract_class_body(child, source, Some(idx), symbols, imports, references);
                } else {
                    extract_class_body(child, source, enclosing, symbols, imports, references);
                }
            }
            "object_definition" => {
                if let Some(sym) = extract_named_decl(child, source, SymbolKind::Module) {
                    let idx = symbols.len();
                    symbols.push(sym);
                    extract_class_body(child, source, Some(idx), symbols, imports, references);
                } else {
                    extract_class_body(child, source, enclosing, symbols, imports, references);
                }
            }
            "trait_definition" => {
                if let Some(sym) = extract_named_decl(child, source, SymbolKind::Trait) {
                    let idx = symbols.len();
                    symbols.push(sym);
                    extract_class_body(child, source, Some(idx), symbols, imports, references);
                } else {
                    extract_class_body(child, source, enclosing, symbols, imports, references);
                }
            }
            _ => {}
        }
    }
}

fn extract_val_var(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("pattern").or_else(|| {
        children(node).find(|c| c.kind() == "identifier" || c.kind() == "simple_identifier")
    })?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        is_exported: !is_private_or_protected(node, source),
        name,
        kind: SymbolKind::Variable,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn extract_package(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let full = node_text(node, source);
    let trimmed = full.trim();
    let name = trimmed.strip_prefix("package")?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        is_exported: true,
        name,
        kind: SymbolKind::Module,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: Some(trimmed.to_string()),
        parent_idx: None,
        unused_excluded: true,
        complexity: None,
        owner_type: None,
    })
}

fn extract_import(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let full = node_text(node, source);
    let trimmed = full.trim();
    let path = trimmed.strip_prefix("import")?.trim();

    if path.is_empty() {
        return None;
    }

    let parts: Vec<&str> = path.rsplitn(2, '.').collect();
    let (specifier, source_path) = if parts.len() == 2 {
        (parts[0].to_string(), parts[1].to_string())
    } else {
        (String::new(), path.to_string())
    };

    Some(ExtractedImport {
        source: source_path,
        specifiers: if specifier.is_empty() {
            vec![]
        } else {
            vec![specifier]
        },
        is_reexport: false,
    })
}

fn record_reference(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    let line = node.start_position().row as u32 + 1;
    match node.kind() {
        "call_expression" => {
            if let Some(callee) = node
                .child_by_field_name("function")
                .or_else(|| node.child(0))
            {
                let name = extract_callee_name(callee, source);
                if !name.is_empty() && !is_builtin_callable(&name) {
                    references.push(ExtractedReference {
                        name,
                        line,
                        from_symbol_idx: enclosing,
                        kind: ReferenceKind::Call,
                        qualifier: None,
                        receiver_type_hint: None,
                        via_method_syntax: false,
                    });
                }
            }
        }
        "field_expression" => {
            let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
            if parent_kind != "call_expression"
                && let Some(field) = node.child_by_field_name("field").or_else(|| {
                    node.child(node.child_count().saturating_sub(1) as u32)
                        .filter(|n| matches!(n.kind(), "identifier" | "simple_identifier"))
                })
            {
                let name = node_text(field, source);
                if !name.is_empty() && !is_builtin_callable(&name) {
                    references.push(ExtractedReference {
                        name,
                        line,
                        from_symbol_idx: enclosing,
                        kind: ReferenceKind::Use,
                        qualifier: None,
                        receiver_type_hint: None,
                        via_method_syntax: false,
                    });
                }
            }
        }
        "type_identifier" => {
            let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
            if matches!(
                parent_kind,
                "class_definition" | "object_definition" | "trait_definition"
            ) {
                return;
            }
            let name = node_text(node, source);
            if !name.is_empty() && !is_builtin_type(&name) {
                references.push(ExtractedReference {
                    name,
                    line,
                    from_symbol_idx: enclosing,
                    kind: ReferenceKind::TypeRef,
                    qualifier: None,
                    receiver_type_hint: None,
                    via_method_syntax: false,
                });
            }
        }
        _ => {}
    }
}

fn extract_callee_name(node: Node, source: &[u8]) -> String {
    match node.kind() {
        "identifier" | "simple_identifier" => node_text(node, source),
        "field_expression" => node
            .child(node.child_count().saturating_sub(1) as u32)
            .filter(|n| matches!(n.kind(), "identifier" | "simple_identifier"))
            .map(|n| node_text(n, source))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn walk_body_references(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    record_reference(node, source, enclosing, references);
    for child in children(node) {
        walk_body_references(child, source, enclosing, references);
    }
}

fn is_builtin_callable(name: &str) -> bool {
    matches!(
        name,
        "println"
            | "print"
            | "printf"
            | "require"
            | "assert"
            | "assume"
            | "sys"
            | "List"
            | "Map"
            | "Set"
            | "Seq"
            | "Vector"
            | "Array"
            | "Some"
            | "None"
            | "Left"
            | "Right"
            | "Success"
            | "Failure"
            | "Future"
            | "Try"
    )
}

fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "Int"
            | "Long"
            | "Short"
            | "Byte"
            | "Float"
            | "Double"
            | "Boolean"
            | "Char"
            | "String"
            | "Unit"
            | "Nothing"
            | "Any"
            | "AnyRef"
            | "AnyVal"
            | "Null"
            | "Array"
            | "List"
            | "Map"
            | "Set"
            | "Seq"
            | "Vector"
            | "Option"
            | "Either"
            | "Future"
            | "Try"
            | "Tuple2"
            | "Tuple3"
            | "Iterable"
            | "Iterator"
    )
}

fn extract_signature(node: Node, source: &[u8]) -> Option<String> {
    common::brace_or_first_line_signature(node, source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_scala(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_scala::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = ScalaSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_class_definition() {
        let result = parse_scala("class MyService { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MyService");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_case_class() {
        let result = parse_scala("case class User(name: String, age: Int)");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "User");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
        let sig = result.symbols[0].signature.as_deref().unwrap_or("");
        assert!(
            sig.contains("case class"),
            "signature should contain 'case class', got: {sig}"
        );
    }

    #[test]
    fn test_object_definition() {
        let result = parse_scala("object Singleton { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Singleton");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Module));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_trait_definition() {
        let result = parse_scala("trait Serializable { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Serializable");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Trait));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_function_definition() {
        let result = parse_scala("def greet(name: String): String = s\"Hello $name\"");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_val_definition() {
        let result = parse_scala("val version: String = \"1.0\"");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "version");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_import_declaration() {
        let result = parse_scala("import scala.collection.mutable.ListBuffer");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "scala.collection.mutable");
        assert_eq!(result.imports[0].specifiers, vec!["ListBuffer"]);
    }

    #[test]
    fn test_empty_file() {
        let result = parse_scala("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_mixed_scala() {
        let result = parse_scala(
            r#"package com.example

import scala.collection.mutable.ListBuffer
import akka.actor.ActorSystem

sealed trait Command
case class Start(name: String) extends Command
case class Stop(reason: String) extends Command

object Application {
  val version: String = "1.0"

  def main(args: Array[String]): Unit = {
    println("Starting")
  }

  private def helper(): Int = 42
}

class Service(name: String) {
  def run(): Unit = { }
  private def cleanup(): Unit = { }
}
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"com.example"), "should contain package");
        assert!(names.contains(&"Command"), "should contain sealed trait");
        assert!(names.contains(&"Start"), "should contain case class Start");
        assert!(names.contains(&"Stop"), "should contain case class Stop");
        assert!(names.contains(&"Application"), "should contain object");
        assert!(names.contains(&"Service"), "should contain class");

        assert_eq!(result.imports.len(), 2);
        assert_eq!(result.imports[0].source, "scala.collection.mutable");
        assert_eq!(result.imports[1].source, "akka.actor");
    }

    #[test]
    fn test_private_method() {
        let result = parse_scala(
            r#"class Processor {
  def run(): Unit = { }
  private def internal(): Int = 0
  protected def hook(): Unit = { }
}"#,
        );
        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 3);

        let run = methods
            .iter()
            .find(|m| m.name == "run")
            .expect("run method");
        assert!(run.is_exported);

        let internal = methods
            .iter()
            .find(|m| m.name == "internal")
            .expect("internal method");
        assert!(!internal.is_exported);

        let hook = methods
            .iter()
            .find(|m| m.name == "hook")
            .expect("hook method");
        assert!(!hook.is_exported);
    }
}
