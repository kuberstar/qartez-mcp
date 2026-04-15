use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct CSharpSupport;

impl LanguageSupport for CSharpSupport {
    fn extensions(&self) -> &[&str] {
        &["cs"]
    }

    fn language_name(&self) -> &str {
        "csharp"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_c_sharp::LANGUAGE)
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

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn has_modifier(node: Node, source: &[u8], modifier: &str) -> bool {
    children(node).any(|child| child.kind() == "modifier" && node_text(child, source) == modifier)
}

fn has_public(node: Node, source: &[u8]) -> bool {
    has_modifier(node, source, "public")
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
        "class_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Class) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
            extract_type_body(node, source, new_enclosing, symbols, references);
            return;
        }
        "interface_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Interface) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
            extract_type_body(node, source, new_enclosing, symbols, references);
            return;
        }
        "struct_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Struct) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
            extract_type_body(node, source, new_enclosing, symbols, references);
            return;
        }
        "enum_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Enum) {
                symbols.push(sym);
            }
            return;
        }
        "record_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Class) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
            extract_type_body(node, source, new_enclosing, symbols, references);
            return;
        }
        "namespace_declaration" | "file_scoped_namespace_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Module) {
                symbols.push(sym);
            }
            extract_type_body(node, source, new_enclosing, symbols, references);
            return;
        }
        "method_declaration" => {
            if let Some(mut sym) = extract_named_decl(node, source, SymbolKind::Function) {
                sym.complexity = node
                    .child_by_field_name("body")
                    .map(|body| 1 + count_complexity(body, source));
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
        }
        "using_directive" => {
            if let Some(imp) = extract_using(node, source) {
                imports.push(imp);
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

/// Counts branching nodes inside a function body for cyclomatic complexity.
/// Recursively walks all children but stops at nested lambda boundaries.
fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut count = 0;
    match node.kind() {
        "lambda_expression" => {
            return 0;
        }
        "if_statement" => count += 1,
        "switch_expression_arm" => count += 1,
        "for_statement" | "for_each_statement" | "while_statement" | "do_statement" => {
            count += 1;
        }
        "conditional_expression" => count += 1,
        "catch_clause" => count += 1,
        "binary_expression" => {
            for child in children(node) {
                let text = node_text(child, source);
                if text == "&&" || text == "||" {
                    count += 1;
                }
            }
        }
        _ => {}
    }
    for child in children(node) {
        count += count_complexity(child, source);
    }
    count
}

fn extract_named_decl(node: Node, source: &[u8], kind: SymbolKind) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        is_exported: has_public(node, source),
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

#[expect(
    clippy::only_used_in_recursion,
    reason = "enclosing is the fallback for unnamed nested declarations"
)]
fn extract_type_body(
    type_node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    let body = match type_node.child_by_field_name("body") {
        Some(b) => b,
        None => return,
    };
    for child in children(body) {
        match child.kind() {
            "method_declaration" => {
                if let Some(mut sym) = extract_named_decl(child, source, SymbolKind::Method) {
                    sym.complexity = child
                        .child_by_field_name("body")
                        .map(|body| 1 + count_complexity(body, source));
                    let idx = symbols.len();
                    symbols.push(sym);
                    record_return_type(child, source, Some(idx), references);
                    walk_body_references(child, source, Some(idx), references);
                }
            }
            "constructor_declaration" => {
                if let Some(mut sym) = extract_named_decl(child, source, SymbolKind::Method) {
                    sym.complexity = child
                        .child_by_field_name("body")
                        .map(|body| 1 + count_complexity(body, source));
                    let idx = symbols.len();
                    symbols.push(sym);
                    walk_body_references(child, source, Some(idx), references);
                }
            }
            "property_declaration" => {
                if let Some(sym) = extract_named_decl(child, source, SymbolKind::Variable) {
                    symbols.push(sym);
                }
            }
            "field_declaration" => {
                extract_field(child, source, symbols);
            }
            "class_declaration" => {
                if let Some(sym) = extract_named_decl(child, source, SymbolKind::Class) {
                    let idx = symbols.len();
                    symbols.push(sym);
                    extract_type_body(child, source, Some(idx), symbols, references);
                } else {
                    extract_type_body(child, source, enclosing, symbols, references);
                }
            }
            "interface_declaration" => {
                if let Some(sym) = extract_named_decl(child, source, SymbolKind::Interface) {
                    let idx = symbols.len();
                    symbols.push(sym);
                    extract_type_body(child, source, Some(idx), symbols, references);
                } else {
                    extract_type_body(child, source, enclosing, symbols, references);
                }
            }
            "struct_declaration" => {
                if let Some(sym) = extract_named_decl(child, source, SymbolKind::Struct) {
                    let idx = symbols.len();
                    symbols.push(sym);
                    extract_type_body(child, source, Some(idx), symbols, references);
                } else {
                    extract_type_body(child, source, enclosing, symbols, references);
                }
            }
            "enum_declaration" => {
                if let Some(sym) = extract_named_decl(child, source, SymbolKind::Enum) {
                    symbols.push(sym);
                }
            }
            _ => {}
        }
    }
}

fn extract_field(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let is_const = has_modifier(node, source, "const");
    let is_static = has_modifier(node, source, "static");
    let is_readonly = has_modifier(node, source, "readonly");

    if !(is_const || (is_static && is_readonly)) {
        return;
    }

    for child in children(node) {
        if child.kind() == "variable_declaration" {
            for decl in children(child) {
                if decl.kind() == "variable_declarator"
                    && let Some(name_node) = decl.child_by_field_name("name")
                {
                    let name = node_text(name_node, source);
                    if !name.is_empty() {
                        symbols.push(ExtractedSymbol {
                            is_exported: has_public(node, source),
                            name,
                            kind: SymbolKind::Const,
                            line_start: node.start_position().row as u32 + 1,
                            line_end: node.end_position().row as u32 + 1,
                            signature: extract_signature(node, source),
                            parent_idx: None,
                            unused_excluded: false,
                            complexity: None,
                            owner_type: None,
                        });
                    }
                }
            }
        }
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
        "invocation_expression" => {
            // `Foo.Bar(x)` — first child is the callee expression.
            if let Some(func) = node
                .child_by_field_name("function")
                .or_else(|| node.child(0))
            {
                let name = extract_callee_name(func, source);
                if !name.is_empty() && !is_builtin_callable(&name) {
                    references.push(ExtractedReference {
                        name,
                        line,
                        from_symbol_idx: enclosing,
                        kind: ReferenceKind::Call,
                        qualifier: None,
                        receiver_type_hint: None,
                    });
                }
            }
        }
        "object_creation_expression" => {
            // `new Foo()` — the type child holds the constructor name.
            if let Some(type_node) = node.child_by_field_name("type").or_else(|| {
                children(node).find(|c| {
                    matches!(
                        c.kind(),
                        "identifier_name" | "generic_name" | "identifier" | "qualified_name"
                    )
                })
            }) {
                let name = extract_type_name(type_node, source);
                if !name.is_empty() && !is_builtin_type(&name) {
                    references.push(ExtractedReference {
                        name,
                        line,
                        from_symbol_idx: enclosing,
                        kind: ReferenceKind::Call,
                        qualifier: None,
                        receiver_type_hint: None,
                    });
                }
            }
        }
        "type_identifier" | "identifier_name" | "generic_name" => {
            let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
            if matches!(
                parent_kind,
                "class_declaration"
                    | "interface_declaration"
                    | "struct_declaration"
                    | "enum_declaration"
                    | "record_declaration"
                    | "namespace_declaration"
                    | "object_creation_expression"
            ) {
                return;
            }
            let name = extract_type_name(node, source);
            if !name.is_empty() && !is_builtin_type(&name) {
                references.push(ExtractedReference {
                    name,
                    line,
                    from_symbol_idx: enclosing,
                    kind: ReferenceKind::TypeRef,
                    qualifier: None,
                    receiver_type_hint: None,
                });
            }
        }
        _ => {}
    }
}

fn extract_callee_name(node: Node, source: &[u8]) -> String {
    match node.kind() {
        "identifier_name" | "identifier" => node_text(node, source),
        "member_access_expression" => {
            // `obj.Method` — try "name" field, fall back to last identifier child.
            node.child_by_field_name("name")
                .or_else(|| {
                    node.child(node.child_count().saturating_sub(1) as u32)
                        .filter(|n| {
                            matches!(n.kind(), "identifier" | "identifier_name" | "generic_name")
                        })
                })
                .map(|n| node_text(n, source))
                .unwrap_or_default()
        }
        "generic_name" => {
            // `Foo<T>` — the identifier child is the base name.
            node.child_by_field_name("name")
                .or_else(|| node.child(0))
                .map(|n| node_text(n, source))
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn extract_type_name(node: Node, source: &[u8]) -> String {
    match node.kind() {
        "identifier_name" | "identifier" | "type_identifier" => node_text(node, source),
        "generic_name" => node
            .child_by_field_name("name")
            .or_else(|| node.child(0))
            .map(|n| node_text(n, source))
            .unwrap_or_default(),
        "qualified_name" => {
            // Take the rightmost simple name.
            node.child_by_field_name("right")
                .or_else(|| node.child(node.child_count().saturating_sub(1) as u32))
                .map(|n| node_text(n, source))
                .unwrap_or_default()
        }
        _ => node_text(node, source),
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

fn record_return_type(
    method_node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    if let Some(ret) = method_node.child_by_field_name("returns") {
        let name = extract_type_name(ret, source);
        if !name.is_empty() && !is_builtin_type(&name) {
            references.push(ExtractedReference {
                name,
                line: ret.start_position().row as u32 + 1,
                from_symbol_idx: enclosing,
                kind: ReferenceKind::TypeRef,
                qualifier: None,
                receiver_type_hint: None,
            });
        }
    }
}

fn is_builtin_callable(name: &str) -> bool {
    matches!(
        name,
        "ToString" | "Equals" | "GetHashCode" | "GetType" | "ReferenceEquals"
    )
}

fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "long"
            | "short"
            | "byte"
            | "float"
            | "double"
            | "decimal"
            | "bool"
            | "char"
            | "string"
            | "object"
            | "void"
            | "dynamic"
            | "var"
            | "nint"
            | "nuint"
            | "sbyte"
            | "ushort"
            | "uint"
            | "ulong"
            | "Task"
            | "String"
            | "Object"
            | "Exception"
            | "Console"
            | "Math"
            | "Convert"
    )
}

fn extract_using(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let full = node_text(node, source);
    let trimmed = full.trim().trim_end_matches(';').trim();
    let path = trimmed.strip_prefix("using")?.trim();
    let path = path.strip_prefix("static").map_or(path, |s| s.trim());

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

    fn parse_csharp(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_c_sharp::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = CSharpSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_public_class() {
        let result = parse_csharp("public class MyService { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MyService");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_internal_class() {
        let result = parse_csharp("internal class Helper { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Helper");
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_interface() {
        let result = parse_csharp("public interface IRepository { void Save(); }");
        let ifaces: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Interface))
            .collect();
        assert_eq!(ifaces.len(), 1);
        assert_eq!(ifaces[0].name, "IRepository");
        assert!(ifaces[0].is_exported);
    }

    #[test]
    fn test_struct() {
        let result = parse_csharp("public struct Point { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Point");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Struct));
    }

    #[test]
    fn test_enum() {
        let result = parse_csharp("public enum Status { Active, Inactive }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Status");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Enum));
    }

    #[test]
    fn test_method() {
        let result = parse_csharp(
            "public class Foo {\n    public void Run() { }\n    private int Count() { return 0; }\n}",
        );
        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].name, "Run");
        assert!(methods[0].is_exported);
        assert_eq!(methods[1].name, "Count");
        assert!(!methods[1].is_exported);
    }

    #[test]
    fn test_const_field() {
        let result = parse_csharp("public class Config {\n    public const int MaxSize = 100;\n}");
        let consts: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Const))
            .collect();
        assert_eq!(consts.len(), 1);
        assert_eq!(consts[0].name, "MaxSize");
        assert!(consts[0].is_exported);
    }

    #[test]
    fn test_using() {
        let result = parse_csharp("using System.Collections.Generic;");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "System.Collections");
        assert_eq!(result.imports[0].specifiers, vec!["Generic"]);
    }

    #[test]
    fn test_namespace() {
        let result = parse_csharp("namespace MyApp.Models { public class User { } }");
        let namespaces: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Module))
            .collect();
        assert_eq!(namespaces.len(), 1);
        assert_eq!(namespaces[0].name, "MyApp.Models");
    }

    #[test]
    fn test_empty_file() {
        let result = parse_csharp("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_mixed_declarations() {
        let result = parse_csharp(
            r#"using System;
using System.Collections.Generic;

namespace MyApp {
    public class AppService {
        public const string Version = "1.0";
        public List<string> GetData() { return null; }
        private void Helper() { }
    }

    public interface IService {
        void Execute();
    }

    public enum Status { Active, Inactive }
}
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"MyApp"));
        assert!(names.contains(&"AppService"));
        assert!(names.contains(&"IService"));
        assert!(names.contains(&"Status"));

        assert_eq!(result.imports.len(), 2);
    }

    #[test]
    fn test_ref_call_attributed_to_method() {
        let result = parse_csharp(
            r#"
public class Service {
    public void Process() {
        var repo = Repository.Open();
        repo.Save();
    }
}
"#,
        );
        let calls: Vec<_> = result
            .references
            .iter()
            .filter(|r| matches!(r.kind, ReferenceKind::Call))
            .collect();
        assert!(
            calls.iter().any(|r| r.name == "Open"),
            "expected call ref to 'Open', got: {calls:?}"
        );
        let process_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "Process")
            .expect("Process symbol");
        for call in &calls {
            assert_eq!(
                call.from_symbol_idx,
                Some(process_idx),
                "call '{}' should be attributed to Process",
                call.name
            );
        }
    }

    #[test]
    fn test_ref_object_creation() {
        let result = parse_csharp(
            r#"
public class Factory {
    public void Build() {
        var w = new Widget();
    }
}
"#,
        );
        let calls: Vec<_> = result
            .references
            .iter()
            .filter(|r| matches!(r.kind, ReferenceKind::Call))
            .collect();
        assert!(
            calls.iter().any(|r| r.name == "Widget"),
            "expected call ref to 'Widget' from new expression, got: {calls:?}"
        );
    }

    #[test]
    fn test_ref_builtin_filtered() {
        let result = parse_csharp(
            r#"
public class Demo {
    public void Run() {
        var s = ToString();
        int x = 42;
        string name = "hello";
    }
}
"#,
        );
        let ref_names: Vec<&str> = result.references.iter().map(|r| r.name.as_str()).collect();
        assert!(
            !ref_names.contains(&"ToString"),
            "ToString should be filtered"
        );
        assert!(!ref_names.contains(&"int"), "int should be filtered");
        assert!(!ref_names.contains(&"string"), "string should be filtered");
    }

    #[test]
    fn test_ref_type_ref_in_signature() {
        let result = parse_csharp(
            r#"
public class Handler {
    public AppConfig Load() { return null; }
}
"#,
        );
        let type_refs: Vec<_> = result
            .references
            .iter()
            .filter(|r| matches!(r.kind, ReferenceKind::TypeRef))
            .collect();
        let names: Vec<&str> = type_refs.iter().map(|r| r.name.as_str()).collect();
        assert!(
            names.contains(&"AppConfig"),
            "expected type ref to AppConfig, got: {names:?}"
        );
    }
}
