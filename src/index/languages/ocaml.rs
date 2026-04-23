use tree_sitter::{Language, Node};

use super::LanguageSupport;
use super::common::{self, children, node_text};
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct OCamlSupport;

impl LanguageSupport for OCamlSupport {
    fn extensions(&self) -> &[&str] {
        &["ml", "mli"]
    }

    fn language_name(&self) -> &str {
        "ocaml"
    }

    fn tree_sitter_language(&self, ext: &str) -> Language {
        if ext == "mli" {
            Language::new(tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE)
        } else {
            Language::new(tree_sitter_ocaml::LANGUAGE_OCAML)
        }
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let mut references = Vec::new();
        let root = tree.root_node();
        walk(
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

fn extract_signature(node: Node, source: &[u8]) -> Option<String> {
    common::first_line_signature(node, source)
}

fn walk(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    let mut new_enclosing = enclosing;
    match node.kind() {
        "value_definition" => {
            extract_value_definition(node, source, enclosing, symbols, references);
            return;
        }
        "value_specification" => {
            if let Some(sym) = extract_value_specification(node, source) {
                symbols.push(sym);
            }
            return;
        }
        "external" => {
            if let Some(sym) = extract_external(node, source) {
                symbols.push(sym);
            }
            return;
        }
        "type_definition" => {
            extract_type_definition(node, source, symbols);
            return;
        }
        "module_definition" => {
            extract_module_definition(node, source, enclosing, symbols, imports, references);
            return;
        }
        "module_type_definition" => {
            if let Some(sym) = extract_module_type_definition(node, source) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
        }
        "exception_definition" => {
            if let Some(sym) = extract_exception_definition(node, source) {
                symbols.push(sym);
            }
            return;
        }
        "class_definition" => {
            extract_class_definition(node, source, symbols, references);
            return;
        }
        "class_type_definition" => {
            extract_class_type_definition(node, source, symbols);
            return;
        }
        "open_module" => {
            if let Some(imp) = extract_open_or_include(node, source) {
                imports.push(imp);
            }
            return;
        }
        "include_module" | "include_module_type" => {
            if let Some(imp) = extract_open_or_include(node, source) {
                imports.push(imp);
            }
            return;
        }
        _ => {}
    }

    record_reference(node, source, enclosing, references);

    for child in children(node) {
        walk(child, source, new_enclosing, symbols, imports, references);
    }
}

fn extract_value_definition(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    for child in children(node) {
        if child.kind() == "let_binding" {
            extract_let_binding(child, source, enclosing, symbols, references);
        }
    }
}

fn extract_let_binding(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    let pattern = match node.child_by_field_name("pattern") {
        Some(p) => p,
        None => return,
    };
    let name = match binding_name(pattern, source) {
        Some(n) => n,
        None => return,
    };

    let has_parameters = children(node).any(|c| c.kind() == "parameter");
    let kind = if has_parameters {
        SymbolKind::Function
    } else {
        SymbolKind::Variable
    };

    let is_exported = !name.starts_with('_');

    let complexity = if matches!(kind, SymbolKind::Function) {
        let body_cc = node
            .child_by_field_name("body")
            .map(|b| count_complexity(b, source))
            .unwrap_or(0);
        Some(1 + body_cc)
    } else {
        None
    };

    let idx = symbols.len();
    symbols.push(ExtractedSymbol {
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported,
        parent_idx: enclosing,
        unused_excluded: false,
        complexity,
        owner_type: None,
    });

    if let Some(body) = node.child_by_field_name("body") {
        walk_references(body, source, Some(idx), references);
    }
}

fn binding_name(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "value_name" => Some(node_text(node, source)),
        "value_pattern" => Some(node_text(node, source)),
        "parenthesized_pattern" | "typed_pattern" => {
            for child in children(node) {
                if let Some(n) = binding_name(child, source) {
                    return Some(n);
                }
            }
            None
        }
        _ => {
            for child in children(node) {
                if let Some(n) = binding_name(child, source) {
                    return Some(n);
                }
            }
            None
        }
    }
}

fn extract_value_specification(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = children(node).find(|c| c.kind() == "value_name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
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

fn extract_external(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = children(node).find(|c| c.kind() == "value_name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
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

fn extract_type_definition(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(node) {
        if child.kind() == "type_binding" {
            let name_node = child.child_by_field_name("name");
            let name = match name_node {
                Some(n) => node_text(n, source),
                None => continue,
            };
            if name.is_empty() {
                continue;
            }
            symbols.push(ExtractedSymbol {
                name,
                kind: SymbolKind::Type,
                line_start: child.start_position().row as u32 + 1,
                line_end: child.end_position().row as u32 + 1,
                signature: extract_signature(child, source),
                is_exported: true,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            });
        }
    }
}

fn extract_module_definition(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    for child in children(node) {
        if child.kind() == "module_binding" {
            let name_node = children(child).find(|c| c.kind() == "module_name");
            let name = match name_node {
                Some(n) => node_text(n, source),
                None => continue,
            };
            if name.is_empty() {
                continue;
            }
            let idx = symbols.len();
            symbols.push(ExtractedSymbol {
                name,
                kind: SymbolKind::Module,
                line_start: child.start_position().row as u32 + 1,
                line_end: child.end_position().row as u32 + 1,
                signature: extract_signature(child, source),
                is_exported: true,
                parent_idx: enclosing,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            });
            for grand in children(child) {
                walk(grand, source, Some(idx), symbols, imports, references);
            }
        }
    }
}

fn extract_module_type_definition(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = children(node).find(|c| c.kind() == "module_type_name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Interface,
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

fn extract_exception_definition(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    for child in children(node) {
        if child.kind() == "constructor_declaration" {
            let name_node = children(child).find(|c| c.kind() == "constructor_name");
            if let Some(n) = name_node {
                let name = node_text(n, source);
                if name.is_empty() {
                    return None;
                }
                return Some(ExtractedSymbol {
                    name,
                    kind: SymbolKind::Type,
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
    None
}

fn extract_class_definition(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    for child in children(node) {
        if child.kind() == "class_binding" {
            let name_node = children(child).find(|c| c.kind() == "class_name");
            let name = match name_node {
                Some(n) => node_text(n, source),
                None => continue,
            };
            if name.is_empty() {
                continue;
            }
            let idx = symbols.len();
            let owner = name.clone();
            symbols.push(ExtractedSymbol {
                name,
                kind: SymbolKind::Class,
                line_start: child.start_position().row as u32 + 1,
                line_end: child.end_position().row as u32 + 1,
                signature: extract_signature(child, source),
                is_exported: true,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            });
            extract_class_body(child, source, idx, &owner, symbols, references);
        }
    }
}

fn extract_class_type_definition(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(node) {
        if child.kind() == "class_type_binding" {
            let name_node = children(child).find(|c| c.kind() == "class_type_name");
            let name = match name_node {
                Some(n) => node_text(n, source),
                None => continue,
            };
            if name.is_empty() {
                continue;
            }
            symbols.push(ExtractedSymbol {
                name,
                kind: SymbolKind::Interface,
                line_start: child.start_position().row as u32 + 1,
                line_end: child.end_position().row as u32 + 1,
                signature: extract_signature(child, source),
                is_exported: true,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            });
        }
    }
}

fn extract_class_body(
    node: Node,
    source: &[u8],
    parent_idx: usize,
    owner: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    for child in children(node) {
        if child.kind() == "method_definition" {
            let name_node = children(child).find(|c| c.kind() == "method_name");
            let name = match name_node {
                Some(n) => node_text(n, source),
                None => continue,
            };
            if name.is_empty() {
                continue;
            }
            let body_cc = child
                .child_by_field_name("body")
                .map(|b| count_complexity(b, source))
                .unwrap_or(0);
            let idx = symbols.len();
            symbols.push(ExtractedSymbol {
                name,
                kind: SymbolKind::Method,
                line_start: child.start_position().row as u32 + 1,
                line_end: child.end_position().row as u32 + 1,
                signature: extract_signature(child, source),
                is_exported: true,
                parent_idx: Some(parent_idx),
                unused_excluded: false,
                complexity: Some(1 + body_cc),
                owner_type: Some(owner.to_string()),
            });
            if let Some(body) = child.child_by_field_name("body") {
                walk_references(body, source, Some(idx), references);
            }
        } else {
            extract_class_body(child, source, parent_idx, owner, symbols, references);
        }
    }
}

fn extract_open_or_include(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let module_node = node.child_by_field_name("module")?;
    let text = node_text(module_node, source);
    let path = text.trim().trim_end_matches(';').to_string();
    if path.is_empty() {
        return None;
    }
    Some(ExtractedImport {
        source: path,
        specifiers: vec![],
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
        "application_expression" => {
            if let Some(func) = node.child_by_field_name("function") {
                let (name, qualifier) = callee_name(func, source);
                if !name.is_empty() && !is_builtin(&name) {
                    references.push(ExtractedReference {
                        name,
                        line,
                        from_symbol_idx: enclosing,
                        kind: ReferenceKind::Call,
                        qualifier,
                        receiver_type_hint: None,
                        via_method_syntax: false,
                    });
                }
            }
        }
        "value_path" => {
            if let Some(parent) = node.parent()
                && parent.kind() == "application_expression"
            {
                return;
            }
            let (name, qualifier) = path_parts(node, source);
            if !name.is_empty() && !is_builtin(&name) {
                references.push(ExtractedReference {
                    name,
                    line,
                    from_symbol_idx: enclosing,
                    kind: ReferenceKind::Use,
                    qualifier,
                    receiver_type_hint: None,
                    via_method_syntax: false,
                });
            }
        }
        _ => {}
    }
}

fn callee_name(node: Node, source: &[u8]) -> (String, Option<String>) {
    match node.kind() {
        "value_path" => path_parts(node, source),
        _ => (String::new(), None),
    }
}

fn path_parts(node: Node, source: &[u8]) -> (String, Option<String>) {
    let full = node_text(node, source);
    if let Some(idx) = full.rfind('.') {
        let (q, rest) = full.split_at(idx);
        (rest[1..].to_string(), Some(q.to_string()))
    } else {
        (full, None)
    }
}

fn walk_references(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    record_reference(node, source, enclosing, references);
    for child in children(node) {
        walk_references(child, source, enclosing, references);
    }
}

fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if_expression" => 1,
        "match_case" => 1,
        "while_expression" | "for_expression" => 1,
        "try_expression" => 1,
        "infix_expression" => {
            let op = node
                .child_by_field_name("operator")
                .map(|n| node_text(n, source))
                .unwrap_or_default();
            if op == "&&" || op == "||" { 1 } else { 0 }
        }
        "fun_expression" | "function_expression" => return 0,
        _ => 0,
    };
    for child in children(node) {
        total += count_complexity(child, source);
    }
    total
}

fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "print_endline"
            | "print_string"
            | "print_int"
            | "print_float"
            | "prerr_endline"
            | "prerr_string"
            | "ignore"
            | "fst"
            | "snd"
            | "raise"
            | "failwith"
            | "invalid_arg"
            | "ref"
            | "incr"
            | "decr"
            | "not"
            | "min"
            | "max"
            | "compare"
            | "succ"
            | "pred"
            | "abs"
            | "string_of_int"
            | "int_of_string"
            | "string_of_float"
            | "float_of_string"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_ml(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_ocaml::LANGUAGE_OCAML);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = OCamlSupport;
        support.extract(source.as_bytes(), &tree)
    }

    fn parse_mli(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = OCamlSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_empty_file() {
        let result = parse_ml("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_let_variable() {
        let result = parse_ml("let x = 42\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "x");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_let_function() {
        let result = parse_ml("let greet name = \"hello \" ^ name\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_underscore_private() {
        let result = parse_ml("let _internal x = x + 1\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "_internal");
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_type_definition() {
        let result = parse_ml("type color = Red | Green | Blue\n");
        let types: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Type))
            .collect();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "color");
    }

    #[test]
    fn test_module_definition() {
        let result = parse_ml("module M = struct\n  let x = 1\nend\n");
        let modules: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Module))
            .collect();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name, "M");

        let vars: Vec<_> = result.symbols.iter().filter(|s| s.name == "x").collect();
        assert_eq!(vars.len(), 1);
        assert!(vars[0].parent_idx.is_some());
    }

    #[test]
    fn test_open_module() {
        let result = parse_ml("open List\nlet x = length [1; 2; 3]\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "List");
    }

    #[test]
    fn test_exception_definition() {
        let result = parse_ml("exception My_error of string\n");
        let excs: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Type))
            .collect();
        assert_eq!(excs.len(), 1);
        assert_eq!(excs[0].name, "My_error");
    }

    #[test]
    fn test_class_definition() {
        let result = parse_ml(
            "class counter = object\n  val mutable count = 0\n  method get = count\nend\n",
        );
        let classes: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "counter");

        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "get");
        assert_eq!(methods[0].owner_type.as_deref(), Some("counter"));
    }

    #[test]
    fn test_mli_value_specification() {
        let result = parse_mli("val greet : string -> string\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_mli_type_specification() {
        let result = parse_mli("type t\n");
        let types: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Type))
            .collect();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "t");
    }

    #[test]
    fn test_mixed_ocaml_file() {
        let result = parse_ml(
            r#"open Printf
module Util = struct
  let greet name = sprintf "hello %s" name
  let _private x = x + 1
end

type shape = Circle of float | Square of float

exception Invalid_shape

let area s =
  match s with
  | Circle r -> 3.14 *. r *. r
  | Square side -> side *. side
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Util"));
        assert!(names.contains(&"greet"));
        assert!(names.contains(&"_private"));
        assert!(names.contains(&"shape"));
        assert!(names.contains(&"Invalid_shape"));
        assert!(names.contains(&"area"));

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "Printf");

        let area = result.symbols.iter().find(|s| s.name == "area").unwrap();
        assert!(matches!(area.kind, SymbolKind::Function));
        assert!(area.complexity.is_some());
        assert!(area.complexity.unwrap() >= 2);

        let private_fn = result
            .symbols
            .iter()
            .find(|s| s.name == "_private")
            .unwrap();
        assert!(!private_fn.is_exported);
    }
}
