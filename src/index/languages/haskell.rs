use tree_sitter::{Language, Node};

use super::LanguageSupport;
use super::common::{self, children, node_text};
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct HaskellSupport;

impl LanguageSupport for HaskellSupport {
    fn extensions(&self) -> &[&str] {
        &["hs", "lhs"]
    }

    fn language_name(&self) -> &str {
        "haskell"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_haskell::LANGUAGE)
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
    match node.kind() {
        "header" => {
            if let Some(sym) = extract_module_header(node, source) {
                symbols.push(sym);
            }
            return;
        }
        "import" => {
            if let Some(imp) = extract_import(node, source) {
                imports.push(imp);
            }
            return;
        }
        "function" => {
            extract_function(node, source, enclosing, symbols, references);
            return;
        }
        "bind" => {
            extract_bind(node, source, enclosing, symbols, references);
            return;
        }
        "data_type" | "newtype" => {
            if let Some(sym) = extract_type_head(node, source, SymbolKind::Type) {
                symbols.push(sym);
            }
            return;
        }
        "type_synomym" => {
            if let Some(sym) = extract_type_head(node, source, SymbolKind::Type) {
                symbols.push(sym);
            }
            return;
        }
        "class" => {
            if let Some(sym) = extract_type_head(node, source, SymbolKind::Trait) {
                let idx = symbols.len();
                symbols.push(sym);
                walk_class_body(node, source, idx, symbols, references);
            }
            return;
        }
        "instance" => {
            walk_instance_body(node, source, symbols, references);
            return;
        }
        _ => {}
    }

    for child in children(node) {
        walk(child, source, enclosing, symbols, imports, references);
    }
}

fn extract_module_header(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let module_node = node.child_by_field_name("module")?;
    let name = node_text(module_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Module,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: None,
        unused_excluded: true,
        complexity: None,
        owner_type: None,
    })
}

fn extract_import(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let module_node = node.child_by_field_name("module")?;
    let source_path = node_text(module_node, source);
    if source_path.is_empty() {
        return None;
    }
    let mut specifiers = Vec::new();
    if let Some(names) = node.child_by_field_name("names") {
        for child in children(names) {
            if child.kind() == "import_name" {
                let text = node_text(child, source);
                if !text.is_empty() {
                    specifiers.push(text);
                }
            }
        }
    }
    Some(ExtractedImport {
        source: source_path,
        specifiers,
        is_reexport: false,
    })
}

fn extract_function(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    let name_node = node.child_by_field_name("name");
    let name = match name_node {
        Some(n) => node_text(n, source),
        None => return,
    };
    if name.is_empty() {
        return;
    }
    let body_cc = count_complexity(node, source);
    let idx = symbols.len();
    symbols.push(ExtractedSymbol {
        name,
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: enclosing,
        unused_excluded: false,
        complexity: Some(1 + body_cc),
        owner_type: None,
    });
    for child in children(node) {
        walk_references(child, source, Some(idx), references);
    }
}

fn extract_bind(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    let name = if let Some(name_node) = node.child_by_field_name("name") {
        node_text(name_node, source)
    } else if let Some(pat) = node.child_by_field_name("pattern") {
        match first_variable(pat, source) {
            Some(n) => n,
            None => return,
        }
    } else {
        return;
    };
    if name.is_empty() {
        return;
    }
    let idx = symbols.len();
    symbols.push(ExtractedSymbol {
        name,
        kind: SymbolKind::Variable,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: enclosing,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    });
    for child in children(node) {
        walk_references(child, source, Some(idx), references);
    }
}

fn first_variable(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() == "variable" {
        return Some(node_text(node, source));
    }
    for child in children(node) {
        if let Some(n) = first_variable(child, source) {
            return Some(n);
        }
    }
    None
}

fn extract_type_head(node: Node, source: &[u8], kind: SymbolKind) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind,
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

fn walk_class_body(
    node: Node,
    source: &[u8],
    parent_idx: usize,
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    let declarations = match node.child_by_field_name("declarations") {
        Some(d) => d,
        None => return,
    };
    for decl in children(declarations) {
        match decl.kind() {
            "signature" => {
                if let Some(sym) = extract_signature_decl(decl, source, Some(parent_idx)) {
                    symbols.push(sym);
                }
            }
            "function" => {
                extract_function_as_method(decl, source, parent_idx, symbols, references);
            }
            _ => {}
        }
    }
}

fn walk_instance_body(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    let declarations = match node.child_by_field_name("declarations") {
        Some(d) => d,
        None => return,
    };
    for decl in children(declarations) {
        if decl.kind() == "function" {
            let name_node = decl.child_by_field_name("name");
            let name = match name_node {
                Some(n) => node_text(n, source),
                None => continue,
            };
            if name.is_empty() {
                continue;
            }
            let body_cc = count_complexity(decl, source);
            let idx = symbols.len();
            symbols.push(ExtractedSymbol {
                name,
                kind: SymbolKind::Method,
                line_start: decl.start_position().row as u32 + 1,
                line_end: decl.end_position().row as u32 + 1,
                signature: extract_signature(decl, source),
                is_exported: true,
                parent_idx: None,
                unused_excluded: true,
                complexity: Some(1 + body_cc),
                owner_type: None,
            });
            for child in children(decl) {
                walk_references(child, source, Some(idx), references);
            }
        }
    }
}

fn extract_signature_decl(
    node: Node,
    source: &[u8],
    parent: Option<usize>,
) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Method,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: parent,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn extract_function_as_method(
    node: Node,
    source: &[u8],
    parent_idx: usize,
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    let name_node = node.child_by_field_name("name");
    let name = match name_node {
        Some(n) => node_text(n, source),
        None => return,
    };
    if name.is_empty() {
        return;
    }
    let body_cc = count_complexity(node, source);
    let idx = symbols.len();
    symbols.push(ExtractedSymbol {
        name,
        kind: SymbolKind::Method,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: Some(parent_idx),
        unused_excluded: false,
        complexity: Some(1 + body_cc),
        owner_type: None,
    });
    for child in children(node) {
        walk_references(child, source, Some(idx), references);
    }
}

fn record_reference(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    let line = node.start_position().row as u32 + 1;
    match node.kind() {
        "apply" => {
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
        "variable" => {
            if should_record_variable(node) {
                let name = node_text(node, source);
                if !name.is_empty() && !is_builtin(&name) {
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
        _ => {}
    }
}

fn should_record_variable(node: Node) -> bool {
    let parent = match node.parent() {
        Some(p) => p,
        None => return false,
    };
    match parent.kind() {
        "function" | "bind" | "signature" | "apply" | "patterns" => false,
        _ => {
            if let Some(grand) = parent.parent()
                && grand.kind() == "apply"
            {
                return false;
            }
            true
        }
    }
}

fn callee_name(node: Node, source: &[u8]) -> (String, Option<String>) {
    match node.kind() {
        "variable" => (node_text(node, source), None),
        "qualified" => {
            let full = node_text(node, source);
            if let Some(idx) = full.rfind('.') {
                let (q, rest) = full.split_at(idx);
                (rest[1..].to_string(), Some(q.to_string()))
            } else {
                (full, None)
            }
        }
        _ => (String::new(), None),
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
        "conditional" => 1,
        "case" | "alternative" => 1,
        "guards" => 0,
        "guard" => 1,
        "infix" => {
            let op = node
                .child_by_field_name("operator")
                .map(|n| node_text(n, source))
                .unwrap_or_default();
            if op == "&&" || op == "||" { 1 } else { 0 }
        }
        "lambda" => return 0,
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
        "print"
            | "putStrLn"
            | "putStr"
            | "getLine"
            | "getContents"
            | "readFile"
            | "writeFile"
            | "return"
            | "pure"
            | "show"
            | "read"
            | "id"
            | "const"
            | "fst"
            | "snd"
            | "not"
            | "map"
            | "filter"
            | "foldr"
            | "foldl"
            | "length"
            | "head"
            | "tail"
            | "init"
            | "last"
            | "null"
            | "reverse"
            | "otherwise"
            | "error"
            | "undefined"
            | "Just"
            | "Nothing"
            | "Left"
            | "Right"
            | "True"
            | "False"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_haskell(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_haskell::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = HaskellSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_empty_file() {
        let result = parse_haskell("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_module_header() {
        let result = parse_haskell("module Foo where\nx = 1\n");
        let modules: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Module))
            .collect();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name, "Foo");
    }

    #[test]
    fn test_simple_function() {
        let result = parse_haskell("module M where\ngreet name = \"hello \" ++ name\n");
        let fns: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function))
            .collect();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].name, "greet");
        assert!(fns[0].is_exported);
    }

    #[test]
    fn test_variable_binding() {
        let result = parse_haskell("module M where\nmaxRetries = 3 :: Int\n");
        let bindings: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.name == "maxRetries")
            .collect();
        assert_eq!(bindings.len(), 1);
    }

    #[test]
    fn test_data_type() {
        let result = parse_haskell("module M where\ndata Color = Red | Green | Blue\n");
        let types: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Type))
            .collect();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "Color");
    }

    #[test]
    fn test_newtype() {
        let result = parse_haskell("module M where\nnewtype Age = Age Int\n");
        let types: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Type))
            .collect();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "Age");
    }

    #[test]
    fn test_type_alias() {
        let result = parse_haskell("module M where\ntype Name = String\n");
        let types: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Type))
            .collect();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "Name");
    }

    #[test]
    fn test_typeclass() {
        let result =
            parse_haskell("module M where\nclass Greeter a where\n  greet :: a -> String\n");
        let traits: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Trait))
            .collect();
        assert_eq!(traits.len(), 1);
        assert_eq!(traits[0].name, "Greeter");
    }

    #[test]
    fn test_import() {
        let result =
            parse_haskell("module M where\nimport Data.List\nimport qualified Data.Map as Map\n");
        assert!(result.imports.len() >= 2);
        let sources: Vec<&str> = result.imports.iter().map(|i| i.source.as_str()).collect();
        assert!(sources.iter().any(|s| s.contains("Data.List")));
        assert!(sources.iter().any(|s| s.contains("Data.Map")));
    }

    #[test]
    fn test_mixed_haskell_file() {
        let result = parse_haskell(
            r#"module MyApp.Service where

import Data.List
import qualified Data.Map as Map

data Status = Active | Inactive

type UserId = Int

class Greeter a where
  greet :: a -> String

greetUser :: UserId -> String
greetUser userId =
  case userId of
    0 -> "admin"
    _ -> "user"

processItems :: [Int] -> Int
processItems xs
  | null xs = 0
  | otherwise = sum xs
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"MyApp.Service"));
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"UserId"));
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"greetUser"));
        assert!(names.contains(&"processItems"));

        assert!(result.imports.len() >= 2);

        let greet_user = result
            .symbols
            .iter()
            .find(|s| s.name == "greetUser")
            .unwrap();
        assert!(matches!(greet_user.kind, SymbolKind::Function));
        assert!(greet_user.complexity.is_some());
    }
}
