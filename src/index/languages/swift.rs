use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct SwiftSupport;

impl LanguageSupport for SwiftSupport {
    fn extensions(&self) -> &[&str] {
        &["swift"]
    }

    fn language_name(&self) -> &str {
        "swift"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_swift::LANGUAGE)
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

fn get_visibility(node: Node, source: &[u8]) -> Option<String> {
    for child in children(node) {
        if child.kind() == "modifiers" {
            for m in children(child) {
                if m.kind() == "visibility_modifier" {
                    return Some(node_text(m, source));
                }
            }
        }
    }
    None
}

fn resolve_class_kind(node: Node, source: &[u8]) -> SymbolKind {
    if let Some(dk) = node.child_by_field_name("declaration_kind") {
        let text = node_text(dk, source);
        match text.as_str() {
            "struct" => return SymbolKind::Struct,
            "enum" => return SymbolKind::Enum,
            _ => {}
        }
    }
    SymbolKind::Class
}

fn is_exported(node: Node, source: &[u8]) -> bool {
    matches!(
        get_visibility(node, source).as_deref(),
        Some("public") | Some("open")
    )
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    match node.kind() {
        "class_declaration" => {
            let kind = resolve_class_kind(node, source);
            if let Some(sym) = extract_named_decl(node, source, kind) {
                symbols.push(sym);
            }
            extract_type_body(node, source, symbols, imports, references);
            return;
        }
        "protocol_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Interface) {
                symbols.push(sym);
            }
            extract_type_body(node, source, symbols, imports, references);
            return;
        }
        "function_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Function) {
                let idx = symbols.len();
                symbols.push(sym);
                if let Some(body) = node.child_by_field_name("body") {
                    for child in children(body) {
                        extract_from_node(child, source, Some(idx), symbols, imports, references);
                    }
                }
                return;
            }
        }
        "property_declaration" => {
            if let Some(sym) = extract_property(node, source) {
                symbols.push(sym);
            }
            return;
        }
        "import_declaration" => {
            if let Some(imp) = extract_import(node, source) {
                imports.push(imp);
            }
            return;
        }
        _ => {}
    }

    record_reference(node, source, enclosing, references);

    for child in children(node) {
        extract_from_node(child, source, enclosing, symbols, imports, references);
    }
}

fn record_reference(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    match node.kind() {
        "call_expression" => {
            let name = node
                .child(0)
                .map(|f| extract_callee_name(f, source))
                .unwrap_or_default();
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
        "type_identifier" | "user_type" => {
            let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
            if matches!(
                parent_kind,
                "class_declaration" | "protocol_declaration" | "type_alias_declaration"
            ) {
                return;
            }
            let name = node_text(node, source);
            if !name.is_empty() && !is_builtin_type(&name) {
                references.push(ExtractedReference {
                    name,
                    line: node.start_position().row as u32 + 1,
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

fn extract_callee_name(func: Node, source: &[u8]) -> String {
    match func.kind() {
        "simple_identifier" => node_text(func, source),
        "navigation_expression" => {
            let suffix = func.child_by_field_name("suffix");
            suffix.map(|s| node_text(s, source)).unwrap_or_default()
        }
        _ => {
            let text = node_text(func, source);
            text.rsplit('.').next().unwrap_or(&text).to_string()
        }
    }
}

fn is_builtin_callable(name: &str) -> bool {
    matches!(
        name,
        "print"
            | "debugPrint"
            | "fatalError"
            | "precondition"
            | "preconditionFailure"
            | "assert"
            | "assertionFailure"
            | "min"
            | "max"
            | "abs"
            | "zip"
            | "stride"
            | "type"
    )
}

fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "Int"
            | "Int8"
            | "Int16"
            | "Int32"
            | "Int64"
            | "UInt"
            | "UInt8"
            | "UInt16"
            | "UInt32"
            | "UInt64"
            | "Float"
            | "Double"
            | "Bool"
            | "String"
            | "Character"
            | "Void"
            | "Any"
            | "AnyObject"
            | "Optional"
            | "Array"
            | "Dictionary"
            | "Set"
            | "Never"
            | "Error"
            | "Self"
    )
}

fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if_statement" => 1,
        "switch_entry" => 1,
        "for_statement" => 1,
        "while_statement" => 1,
        "repeat_while_statement" => 1,
        "guard_statement" => 1,
        "catch_clause" => 1,
        "binary_expression" => {
            let op_text = node
                .child_by_field_name("operator")
                .or_else(|| node.child(1))
                .map(|n| node_text(n, source));
            match op_text.as_deref() {
                Some("&&") | Some("||") => 1,
                _ => 0,
            }
        }
        "lambda_literal" => return 0,
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
        is_exported: is_exported(node, source),
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

fn extract_type_body(
    type_node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    let body = match type_node.child_by_field_name("body") {
        Some(b) => b,
        None => return,
    };
    for child in children(body) {
        match child.kind() {
            "function_declaration" => {
                if let Some(mut sym) = extract_named_decl(child, source, SymbolKind::Method) {
                    sym.kind = SymbolKind::Method;
                    let idx = symbols.len();
                    symbols.push(sym);
                    if let Some(fn_body) = child.child_by_field_name("body") {
                        for grand in children(fn_body) {
                            extract_from_node(
                                grand,
                                source,
                                Some(idx),
                                symbols,
                                imports,
                                references,
                            );
                        }
                    }
                }
            }
            "property_declaration" => {
                if let Some(sym) = extract_property(child, source) {
                    symbols.push(sym);
                }
            }
            "init_declaration" => {
                let idx = symbols.len();
                let init_cc = child
                    .child_by_field_name("body")
                    .map(|body| count_complexity(body, source))
                    .unwrap_or(0);
                symbols.push(ExtractedSymbol {
                    is_exported: is_exported(child, source),
                    name: "init".to_string(),
                    kind: SymbolKind::Method,
                    line_start: child.start_position().row as u32 + 1,
                    line_end: child.end_position().row as u32 + 1,
                    signature: extract_signature(child, source),
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: Some(1 + init_cc),
                    owner_type: None,
                });
                if let Some(fn_body) = child.child_by_field_name("body") {
                    for grand in children(fn_body) {
                        extract_from_node(grand, source, Some(idx), symbols, imports, references);
                    }
                }
            }
            "class_declaration" => {
                let kind = resolve_class_kind(child, source);
                if let Some(sym) = extract_named_decl(child, source, kind) {
                    symbols.push(sym);
                }
                extract_type_body(child, source, symbols, imports, references);
            }
            "protocol_declaration" => {
                if let Some(sym) = extract_named_decl(child, source, SymbolKind::Interface) {
                    symbols.push(sym);
                }
            }
            _ => {}
        }
    }
}

fn extract_property(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }

    Some(ExtractedSymbol {
        is_exported: is_exported(node, source),
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

fn extract_import(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let full = node_text(node, source);
    let trimmed = full.trim();
    let path = trimmed.strip_prefix("import")?.trim();

    if path.is_empty() {
        return None;
    }

    Some(ExtractedImport {
        source: path.to_string(),
        specifiers: vec![],
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

    fn parse_swift(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_swift::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = SwiftSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_public_class() {
        let result = parse_swift("public class MyService { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MyService");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_internal_class() {
        let result = parse_swift("class Helper { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Helper");
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_struct() {
        let result = parse_swift("public struct Config { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Config");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Struct));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_enum() {
        let result = parse_swift("public enum Status { case active; case inactive }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Status");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Enum));
    }

    #[test]
    fn test_protocol() {
        let result = parse_swift("public protocol Repository { func save() }");
        let protos: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Interface))
            .collect();
        assert_eq!(protos.len(), 1);
        assert_eq!(protos[0].name, "Repository");
        assert!(protos[0].is_exported);
    }

    #[test]
    fn test_function() {
        let result = parse_swift("public func greet(name: String) -> String { return name }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_private_function() {
        let result = parse_swift("private func helper() -> Int { return 0 }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "helper");
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_import() {
        let result = parse_swift("import Foundation");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "Foundation");
    }

    #[test]
    fn test_empty_file() {
        let result = parse_swift("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_method_in_class() {
        let result = parse_swift(
            "public class Foo {\n    public func run() { }\n    private func count() -> Int { return 0 }\n}",
        );
        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].name, "run");
        assert!(methods[0].is_exported);
        assert_eq!(methods[1].name, "count");
        assert!(!methods[1].is_exported);
    }

    #[test]
    fn test_mixed_declarations() {
        let result = parse_swift(
            r#"import Foundation
import UIKit

public class AppService {
    public func getData() -> [String] { return [] }
    private func helper() { }
}

public struct Config {
    public var name: String
}
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AppService"));
        assert!(names.contains(&"getData"));
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"Config"));

        assert_eq!(result.imports.len(), 2);
    }

    #[test]
    fn test_refs_call_attributed_to_function() {
        let result = parse_swift(
            r#"func helper() -> Int { return 1 }
func caller() -> Int { return helper() }
"#,
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
    fn test_refs_method_in_class() {
        let result = parse_swift(
            r#"class Svc {
    func run() { process() }
    func process() {}
}
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
            "process() inside run should be attributed to run, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_builtin_filtered() {
        let result = parse_swift(
            r#"func f() {
    print("hello")
    fatalError("boom")
}
"#,
        );
        assert!(
            !result
                .references
                .iter()
                .any(|r| r.name == "print" || r.name == "fatalError"),
            "built-in calls must not be recorded as references"
        );
    }

    #[test]
    fn test_refs_builtin_types_filtered() {
        let result = parse_swift("func f(x: Int) -> String { return \"\" }");
        assert!(
            !result
                .references
                .iter()
                .any(|r| r.name == "Int" || r.name == "String"),
            "built-in types must not be recorded as references"
        );
    }
}
