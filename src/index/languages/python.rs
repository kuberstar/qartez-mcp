use tree_sitter::{Language, Node};

use super::LanguageSupport;
use super::common::{self, children, node_text};
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedRelation, ExtractedSymbol, ParseResult,
    ReferenceKind, RelationKind, SymbolKind,
};

pub struct PythonSupport;

impl LanguageSupport for PythonSupport {
    fn extensions(&self) -> &[&str] {
        &["py", "pyi"]
    }

    fn language_name(&self) -> &str {
        "python"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_python::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let mut references = Vec::new();
        let root = tree.root_node();
        extract_from_node(
            root,
            source,
            false,
            None,
            &mut symbols,
            &mut imports,
            &mut references,
        );
        let type_relations = extract_type_relations(root, source);
        ParseResult {
            symbols,
            imports,
            references,
            type_relations,
        }
    }
}

fn extract_type_relations(node: Node, source: &[u8]) -> Vec<ExtractedRelation> {
    let mut relations = Vec::new();
    collect_type_relations(node, source, &mut relations);
    relations
}

fn collect_type_relations(node: Node, source: &[u8], out: &mut Vec<ExtractedRelation>) {
    if node.kind() == "class_definition" {
        let class_name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))
            .unwrap_or_default();
        if !class_name.is_empty()
            && let Some(bases) = node.child_by_field_name("superclasses")
        {
            let line = node.start_position().row as u32 + 1;
            for child in children(bases) {
                let name = match child.kind() {
                    "identifier" => node_text(child, source),
                    "attribute" => child
                        .child_by_field_name("attribute")
                        .map(|n| node_text(n, source))
                        .unwrap_or_default(),
                    "call" => child
                        .child_by_field_name("function")
                        .map(|n| node_text(n, source))
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                if !name.is_empty() && name != "object" {
                    out.push(ExtractedRelation {
                        sub_name: class_name.clone(),
                        super_name: name,
                        kind: RelationKind::Extends,
                        line,
                    });
                }
            }
        }
    }
    for child in children(node) {
        collect_type_relations(child, source, out);
    }
}

fn is_exported(name: &str) -> bool {
    if name.starts_with("__") && name.ends_with("__") {
        return true;
    }
    !name.starts_with('_')
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    inside_class: bool,
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    let mut new_enclosing = enclosing;
    match node.kind() {
        "function_definition" => {
            if let Some(sym) = extract_function(node, source, inside_class) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
        }
        "class_definition" => {
            if let Some(sym) = extract_class(node, source) {
                let idx = symbols.len();
                let class_name = sym.name.clone();
                symbols.push(sym);
                extract_class_methods(node, source, &class_name, symbols, imports, references);
                new_enclosing = Some(idx);
                // Still walk the class body for references at class scope
                // (base class name, decorators). Methods were already walked
                // with their own enclosing inside extract_class_methods.
                if let Some(bases) = node.child_by_field_name("superclasses") {
                    record_reference(bases, source, new_enclosing, references);
                    for grand in children(bases) {
                        extract_from_node(
                            grand,
                            source,
                            false,
                            new_enclosing,
                            symbols,
                            imports,
                            references,
                        );
                    }
                }
                return;
            }
        }
        "decorated_definition" => {
            extract_decorated(node, source, inside_class, symbols, imports, references);
            return;
        }
        "import_statement" => {
            extract_import_statement(node, source, imports);
            return;
        }
        "import_from_statement" => {
            extract_import_from_statement(node, source, imports);
            return;
        }
        _ => {}
    }

    record_reference(node, source, enclosing, references);

    for child in children(node) {
        extract_from_node(
            child,
            source,
            inside_class,
            new_enclosing,
            symbols,
            imports,
            references,
        );
    }
}

/// Emit a reference for the node shapes Python cares about. The Python
/// grammar merges "function call" and "constructor call" into a single
/// `call` node - there is no dedicated `new_expression` - so a single
/// match arm covers both.
fn record_reference(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    if node.kind() == "call"
        && let Some(func) = node.child_by_field_name("function")
    {
        let name = extract_callee_name(func, source);
        if !name.is_empty() && !is_builtin_callable(&name) {
            references.push(ExtractedReference {
                name,
                line: node.start_position().row as u32 + 1,
                from_symbol_idx: enclosing,
                kind: ReferenceKind::Call,
                qualifier: None,
                receiver_type_hint: None,
            });
        }
    }
}

/// Pull the callee name out of a `call`'s `function` field. The two common
/// shapes are bare `identifier` (`foo()`) and `attribute` (`obj.foo()`).
fn extract_callee_name(func: Node, source: &[u8]) -> String {
    match func.kind() {
        "identifier" => node_text(func, source),
        "attribute" => func
            .child_by_field_name("attribute")
            .map(|n| node_text(n, source))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Python built-ins / noise that a user's symbol graph would be better
/// off ignoring. Printing and common conversions dominate call counts
/// otherwise and drown out the interesting call edges.
fn is_builtin_callable(name: &str) -> bool {
    matches!(
        name,
        "print"
            | "len"
            | "range"
            | "str"
            | "int"
            | "float"
            | "bool"
            | "list"
            | "dict"
            | "set"
            | "tuple"
            | "type"
            | "repr"
            | "hash"
            | "id"
            | "iter"
            | "next"
            | "enumerate"
            | "zip"
            | "map"
            | "filter"
            | "sorted"
            | "reversed"
            | "min"
            | "max"
            | "sum"
            | "any"
            | "all"
            | "isinstance"
            | "issubclass"
            | "getattr"
            | "setattr"
            | "hasattr"
            | "super"
    )
}

/// Recursively count cyclomatic complexity branching points in the AST subtree
/// rooted at `node`. Stops at nested lambda boundaries so their internal
/// branching is not attributed to the enclosing function.
fn count_complexity(node: Node) -> u32 {
    let mut total = match node.kind() {
        "if_statement" => 1,
        "elif_clause" => 1,
        "for_statement" => 1,
        "while_statement" => 1,
        "except_clause" => 1,
        "boolean_operator" => 1,
        "conditional_expression" => 1,
        "list_comprehension"
        | "set_comprehension"
        | "dictionary_comprehension"
        | "generator_expression" => 1,
        // Nested lambdas are separate scopes; do not count their branching
        // as part of the enclosing function.
        "lambda" => return 0,
        _ => 0,
    };
    for child in children(node) {
        total += count_complexity(child);
    }
    total
}

fn extract_function(node: Node, source: &[u8], inside_class: bool) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let kind = if inside_class {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };
    let body_cc = node
        .child_by_field_name("body")
        .map(|body| count_complexity(body))
        .unwrap_or(0);
    Some(ExtractedSymbol {
        is_exported: is_exported(&name),
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: Some(1 + body_cc),
        owner_type: None,
    })
}

fn extract_class(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        is_exported: is_exported(&name),
        name,
        kind: SymbolKind::Class,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn extract_class_methods(
    class_node: Node,
    source: &[u8],
    class_name: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    let body = match class_node.child_by_field_name("body") {
        Some(b) => b,
        None => return,
    };
    let before = symbols.len();
    for child in children(body) {
        extract_from_node(child, source, true, None, symbols, imports, references);
    }
    let owner = Some(class_name.to_string());
    for sym in &mut symbols[before..] {
        if matches!(sym.kind, SymbolKind::Method) {
            sym.owner_type = owner.clone();
        }
    }
}

fn extract_decorated(
    node: Node,
    source: &[u8],
    inside_class: bool,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    for child in children(node) {
        match child.kind() {
            "function_definition" => {
                if let Some(mut sym) = extract_function(child, source, inside_class) {
                    sym.line_start = node.start_position().row as u32 + 1;
                    let idx = symbols.len();
                    symbols.push(sym);
                    if let Some(body) = child.child_by_field_name("body") {
                        for grand in children(body) {
                            extract_from_node(
                                grand,
                                source,
                                inside_class,
                                Some(idx),
                                symbols,
                                imports,
                                references,
                            );
                        }
                    }
                }
            }
            "class_definition" => {
                let class_name = if let Some(mut sym) = extract_class(child, source) {
                    sym.line_start = node.start_position().row as u32 + 1;
                    let name = sym.name.clone();
                    symbols.push(sym);
                    name
                } else {
                    String::new()
                };
                extract_class_methods(child, source, &class_name, symbols, imports, references);
            }
            "decorated_definition" => {
                extract_decorated(child, source, inside_class, symbols, imports, references);
            }
            _ => {}
        }
    }
}

fn extract_import_statement(node: Node, source: &[u8], imports: &mut Vec<ExtractedImport>) {
    for child in children(node) {
        if child.kind() == "dotted_name" {
            let module = node_text(child, source);
            if !module.is_empty() {
                imports.push(ExtractedImport {
                    source: module,
                    specifiers: vec![],
                    is_reexport: false,
                });
            }
        } else if child.kind() == "aliased_import"
            && let Some(name_node) = child.child_by_field_name("name")
        {
            let module = node_text(name_node, source);
            if !module.is_empty() {
                imports.push(ExtractedImport {
                    source: module,
                    specifiers: vec![],
                    is_reexport: false,
                });
            }
        }
    }
}

fn extract_import_from_statement(node: Node, source: &[u8], imports: &mut Vec<ExtractedImport>) {
    let module_node = match node.child_by_field_name("module_name") {
        Some(n) => n,
        None => return,
    };

    let module_name = match module_node.kind() {
        "relative_import" => {
            let mut name = String::new();
            for child in children(module_node) {
                match child.kind() {
                    "import_prefix" => name.push_str(&node_text(child, source)),
                    "dotted_name" => name.push_str(&node_text(child, source)),
                    _ => {}
                }
            }
            name
        }
        _ => node_text(module_node, source),
    };

    if module_name.is_empty() {
        return;
    }

    let mut specifiers = Vec::new();
    let mut cursor = node.walk();
    for child in node.children_by_field_name("name", &mut cursor) {
        match child.kind() {
            "dotted_name" => {
                let text = node_text(child, source);
                if !text.is_empty() {
                    specifiers.push(text);
                }
            }
            "aliased_import" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = node_text(name_node, source);
                    if !name.is_empty() {
                        specifiers.push(name);
                    }
                }
            }
            _ => {}
        }
    }

    for child in children(node) {
        if child.kind() == "wildcard_import" {
            specifiers.push("*".to_string());
        }
    }

    imports.push(ExtractedImport {
        source: module_name,
        specifiers,
        is_reexport: false,
    });
}

fn extract_signature(node: Node, source: &[u8]) -> Option<String> {
    common::first_line_signature(node, source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_python(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_python::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = PythonSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_function_definition() {
        let result = parse_python("def greet(name: str) -> str:\n    return name\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
        assert!(result.symbols[0].signature.is_some());
    }

    #[test]
    fn test_private_function() {
        let result = parse_python("def _internal_helper():\n    pass\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "_internal_helper");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_class_definition() {
        let result = parse_python(
            "class MyService:\n    def __init__(self):\n        self.data = []\n    def get_data(self):\n        return self.data\n",
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
        assert_eq!(methods[0].name, "__init__");
        assert!(methods[0].is_exported); // dunder = exported
        assert_eq!(methods[0].owner_type.as_deref(), Some("MyService"));
        assert_eq!(methods[1].name, "get_data");
        assert!(methods[1].is_exported);
        assert_eq!(methods[1].owner_type.as_deref(), Some("MyService"));
    }

    #[test]
    fn test_private_method() {
        let result = parse_python(
            "class Foo:\n    def _private(self):\n        pass\n    def public(self):\n        pass\n",
        );
        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].name, "_private");
        assert!(!methods[0].is_exported);
        assert_eq!(methods[1].name, "public");
        assert!(methods[1].is_exported);
    }

    #[test]
    fn test_private_class() {
        let result = parse_python("class _InternalHelper:\n    pass\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "_InternalHelper");
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_decorated_function() {
        let result = parse_python("@staticmethod\ndef create():\n    pass\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "create");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert_eq!(result.symbols[0].line_start, 1);
    }

    #[test]
    fn test_decorated_class() {
        let result = parse_python("@dataclass\nclass Config:\n    debug: bool = False\n");
        let classes: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Config");
        assert_eq!(classes[0].line_start, 1);
    }

    #[test]
    fn test_import_statement() {
        let result = parse_python("import os\nimport sys\n");
        assert_eq!(result.imports.len(), 2);
        assert_eq!(result.imports[0].source, "os");
        assert!(result.imports[0].specifiers.is_empty());
        assert_eq!(result.imports[1].source, "sys");
    }

    #[test]
    fn test_import_dotted() {
        let result = parse_python("import os.path\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "os.path");
    }

    #[test]
    fn test_import_aliased() {
        let result = parse_python("import numpy as np\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "numpy");
    }

    #[test]
    fn test_from_import() {
        let result = parse_python("from utils import foo, bar\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "utils");
        assert!(result.imports[0].specifiers.contains(&"foo".to_string()));
        assert!(result.imports[0].specifiers.contains(&"bar".to_string()));
    }

    #[test]
    fn test_from_import_relative() {
        let result = parse_python("from .utils import foo\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, ".utils");
        assert_eq!(result.imports[0].specifiers, vec!["foo"]);
    }

    #[test]
    fn test_from_import_parent_relative() {
        let result = parse_python("from ..models import User\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "..models");
        assert_eq!(result.imports[0].specifiers, vec!["User"]);
    }

    #[test]
    fn test_from_import_wildcard() {
        let result = parse_python("from package.module import *\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "package.module");
        assert_eq!(result.imports[0].specifiers, vec!["*"]);
    }

    #[test]
    fn test_from_import_aliased() {
        let result = parse_python("from collections import OrderedDict as OD\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "collections");
        assert_eq!(result.imports[0].specifiers, vec!["OrderedDict"]);
    }

    #[test]
    fn test_line_numbers() {
        let result = parse_python("def a():\n    pass\n\ndef b():\n    return 1\n");
        assert_eq!(result.symbols.len(), 2);
        assert_eq!(result.symbols[0].line_start, 1);
        assert_eq!(result.symbols[0].line_end, 2);
        assert_eq!(result.symbols[1].line_start, 4);
        assert_eq!(result.symbols[1].line_end, 5);
    }

    #[test]
    fn test_mixed_declarations() {
        let result = parse_python(
            r#"
from .config import Config

class AppConfig(Config):
    debug: bool = False

    def __init__(self):
        super().__init__()

def create_app(config: AppConfig) -> "App":
    return App(config)

class _InternalHelper:
    pass
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AppConfig"));
        assert!(names.contains(&"__init__"));
        assert!(names.contains(&"create_app"));
        assert!(names.contains(&"_InternalHelper"));

        let exported_count = result.symbols.iter().filter(|s| s.is_exported).count();
        assert_eq!(exported_count, 3); // AppConfig, __init__, create_app

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, ".config");
    }

    #[test]
    fn test_dunder_methods_exported() {
        let result = parse_python(
            "class Foo:\n    def __repr__(self):\n        return 'Foo'\n    def __str__(self):\n        return 'Foo'\n",
        );
        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 2);
        assert!(methods.iter().all(|m| m.is_exported));
    }

    #[test]
    fn test_decorated_method_in_class() {
        let result = parse_python(
            "class Foo:\n    @property\n    def name(self):\n        return self._name\n",
        );
        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "name");
    }

    // -- Reference extraction tests --

    #[test]
    fn test_refs_call_attributed_to_enclosing() {
        let result = parse_python(
            "def helper():\n    return 42\n\ndef caller():\n    return helper() + 1\n",
        );
        let caller_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "caller")
            .expect("caller symbol");
        assert!(
            result.references.iter().any(|r| r.name == "helper"
                && matches!(r.kind, ReferenceKind::Call)
                && r.from_symbol_idx == Some(caller_idx)),
            "helper() inside caller should be attributed to caller, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_attribute_call() {
        let result = parse_python("def run(svc):\n    svc.execute()\n");
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "execute" && matches!(r.kind, ReferenceKind::Call)),
            "svc.execute() should emit Call reference to `execute`"
        );
    }

    #[test]
    fn test_refs_method_body() {
        let result = parse_python(
            "def helper():\n    pass\n\nclass Svc:\n    def run(self):\n        helper()\n",
        );
        let run_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "run" && matches!(s.kind, SymbolKind::Method))
            .expect("run method");
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "helper" && r.from_symbol_idx == Some(run_idx)),
            "helper() inside run should be attributed to run method, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_builtins_filtered() {
        let result = parse_python("def f(xs):\n    return len(xs) + sum(xs)\n");
        assert!(
            !result
                .references
                .iter()
                .any(|r| r.name == "len" || r.name == "sum"),
            "built-in callables must not be recorded as references"
        );
    }

    #[test]
    fn test_refs_decorated_function_body() {
        let result =
            parse_python("def helper():\n    pass\n\n@decorator\ndef wrapped():\n    helper()\n");
        let wrapped_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "wrapped")
            .expect("wrapped symbol");
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "helper" && r.from_symbol_idx == Some(wrapped_idx)),
            "helper() inside @-decorated wrapped should attribute to wrapped"
        );
    }

    #[test]
    fn test_refs_super_not_emitted() {
        let result = parse_python(
            "class Child(Parent):\n    def __init__(self):\n        super().__init__()\n",
        );
        assert!(
            !result.references.iter().any(|r| r.name == "super"),
            "super() must be filtered as noise"
        );
    }
}
