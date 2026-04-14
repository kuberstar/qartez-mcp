use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct TypeScriptSupport;

impl LanguageSupport for TypeScriptSupport {
    fn extensions(&self) -> &[&str] {
        &["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"]
    }

    fn language_name(&self) -> &str {
        "typescript"
    }

    fn tree_sitter_language(&self, ext: &str) -> Language {
        match ext {
            "tsx" | "jsx" => Language::new(tree_sitter_typescript::LANGUAGE_TSX),
            "js" | "mjs" | "cjs" => Language::new(tree_sitter_javascript::LANGUAGE),
            _ => Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
        }
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
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    let kind = node.kind();
    let mut new_enclosing = enclosing;
    match kind {
        "function_declaration" | "generator_function_declaration" => {
            if let Some(sym) = extract_function_decl(node, source) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
        }
        "class_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Class) {
                let idx = symbols.len();
                symbols.push(sym);
                extract_class_methods(node, source, symbols, imports, references);
                new_enclosing = Some(idx);
            }
        }
        "interface_declaration" => {
            if let Some(sym) =
                extract_named_decl_by_field(node, source, "name", SymbolKind::Interface)
            {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
        }
        "type_alias_declaration" => {
            if let Some(sym) = extract_named_decl_by_field(node, source, "name", SymbolKind::Type) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
        }
        "enum_declaration" => {
            if let Some(sym) = extract_named_decl_by_field(node, source, "name", SymbolKind::Enum) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            let before = symbols.len();
            extract_variable_decl(node, source, symbols);
            if symbols.len() == before + 1 && matches!(symbols[before].kind, SymbolKind::Function) {
                new_enclosing = Some(before);
            }
        }
        "export_statement" => {
            extract_export_statement(node, source, symbols, imports, references);
            return;
        }
        "import_statement" => {
            if let Some(imp) = extract_import(node, source) {
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

/// Emit a reference for the node shapes we care about. Same shape as the
/// Rust extractor — kept separate so that TypeScript-specific node kinds
/// (e.g. `new_expression`, `member_expression`) do not leak into the Rust
/// extractor's match arms.
fn record_reference(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    let line = node.start_position().row as u32 + 1;
    match node.kind() {
        "call_expression" => {
            if let Some(func) = node.child_by_field_name("function") {
                let name = extract_callee_name(func, source);
                if !name.is_empty() {
                    references.push(ExtractedReference {
                        name,
                        line,
                        from_symbol_idx: enclosing,
                        kind: ReferenceKind::Call,
                        receiver_type_hint: None,
                    });
                }
            }
        }
        "new_expression" => {
            if let Some(cons) = node.child_by_field_name("constructor") {
                let name = extract_callee_name(cons, source);
                if !name.is_empty() {
                    references.push(ExtractedReference {
                        name,
                        line,
                        from_symbol_idx: enclosing,
                        kind: ReferenceKind::Call,
                        receiver_type_hint: None,
                    });
                }
            }
        }
        "type_identifier" => {
            let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
            if matches!(
                parent_kind,
                "class_declaration"
                    | "interface_declaration"
                    | "type_alias_declaration"
                    | "enum_declaration"
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
                    receiver_type_hint: None,
                });
            }
        }
        _ => {}
    }
}

/// Return the name from the `function` field of a `call_expression` or the
/// `constructor` of a `new_expression`. Handles bare identifiers, member
/// access, and parenthesised wrappers.
fn extract_callee_name(func: Node, source: &[u8]) -> String {
    match func.kind() {
        "identifier" | "property_identifier" | "type_identifier" => node_text(func, source),
        "member_expression" => func
            .child_by_field_name("property")
            .map(|n| node_text(n, source))
            .unwrap_or_default(),
        "parenthesized_expression" => children(func)
            .find(|c| !matches!(c.kind(), "(" | ")"))
            .map(|inner| extract_callee_name(inner, source))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// TypeScript built-in types. Filtering here keeps the symbol_refs table
/// small and the PageRank weights meaningful. Missing a rare built-in is
/// harmless — the resolver will just fail to find a matching symbol and
/// drop the reference.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "string"
            | "number"
            | "boolean"
            | "any"
            | "unknown"
            | "never"
            | "void"
            | "null"
            | "undefined"
            | "object"
            | "symbol"
            | "bigint"
            | "Array"
            | "Promise"
            | "Map"
            | "Set"
            | "WeakMap"
            | "WeakSet"
            | "Record"
            | "Partial"
            | "Readonly"
            | "Pick"
            | "Omit"
            | "Required"
            | "ReturnType"
            | "Parameters"
            | "Date"
            | "RegExp"
            | "Error"
            | "JSX"
    )
}

fn extract_export_statement(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    let mut has_source = false;
    let mut source_str = String::new();

    for child in children(node) {
        if child.kind() == "string" {
            source_str = unquote(node_text(child, source));
            has_source = true;
        }
    }

    if has_source {
        let mut specifiers = Vec::new();
        for child in children(node) {
            if child.kind() == "export_clause" {
                collect_export_specifiers(child, source, &mut specifiers);
            }
        }
        imports.push(ExtractedImport {
            source: source_str,
            specifiers,
            is_reexport: true,
        });
        return;
    }

    for child in children(node) {
        match child.kind() {
            "function_declaration" | "generator_function_declaration" => {
                if let Some(mut sym) = extract_function_decl(child, source) {
                    sym.is_exported = true;
                    let idx = symbols.len();
                    symbols.push(sym);
                    // Walk the function body with the newly-created symbol as
                    // enclosing so calls inside it attribute correctly.
                    if let Some(body) = child.child_by_field_name("body") {
                        for grand in children(body) {
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
            "class_declaration" => {
                if let Some(mut sym) = extract_named_decl(child, source, SymbolKind::Class) {
                    sym.is_exported = true;
                    symbols.push(sym);
                }
                extract_class_methods(child, source, symbols, imports, references);
            }
            "interface_declaration" => {
                if let Some(mut sym) =
                    extract_named_decl_by_field(child, source, "name", SymbolKind::Interface)
                {
                    sym.is_exported = true;
                    symbols.push(sym);
                }
            }
            "type_alias_declaration" => {
                if let Some(mut sym) =
                    extract_named_decl_by_field(child, source, "name", SymbolKind::Type)
                {
                    sym.is_exported = true;
                    symbols.push(sym);
                }
            }
            "enum_declaration" => {
                if let Some(mut sym) =
                    extract_named_decl_by_field(child, source, "name", SymbolKind::Enum)
                {
                    sym.is_exported = true;
                    symbols.push(sym);
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                let before_len = symbols.len();
                extract_variable_decl(child, source, symbols);
                for sym in symbols[before_len..].iter_mut() {
                    sym.is_exported = true;
                }
                // Walk arrow/function-expression bodies so references inside
                // exported const handlers are attributed to the symbol.
                if symbols.len() == before_len + 1
                    && matches!(symbols[before_len].kind, SymbolKind::Function)
                {
                    let encl = Some(before_len);
                    for grand in children(child) {
                        extract_from_node(grand, source, encl, symbols, imports, references);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Counts branching nodes inside a function body for cyclomatic complexity.
/// Recursively walks all children but stops at nested function boundaries
/// (arrow functions, function expressions) to avoid counting their branches.
fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut count = 0;
    match node.kind() {
        "arrow_function"
        | "function_expression"
        | "function_declaration"
        | "generator_function_declaration"
        | "function" => {
            return 0;
        }
        "if_statement" => count += 1,
        "switch_case" => count += 1,
        "for_statement" | "for_in_statement" | "while_statement" | "do_statement" => count += 1,
        "ternary_expression" => count += 1,
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

fn extract_function_decl(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let complexity = node
        .child_by_field_name("body")
        .map(|body| 1 + count_complexity(body, source));
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: false,
        parent_idx: None,
        unused_excluded: false,
        complexity,
    })
}

fn extract_named_decl(node: Node, source: &[u8], kind: SymbolKind) -> Option<ExtractedSymbol> {
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
        is_exported: false,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    })
}

fn extract_named_decl_by_field(
    node: Node,
    source: &[u8],
    field: &str,
    kind: SymbolKind,
) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name(field)?;
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
        is_exported: false,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    })
}

fn extract_class_methods(
    class_node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    let body = match class_node.child_by_field_name("body") {
        Some(b) => b,
        None => return,
    };
    for child in children(body) {
        if (child.kind() == "method_definition"
            || child.kind() == "abstract_method_signature"
            || child.kind() == "public_field_definition")
            && let Some(name_node) = child.child_by_field_name("name")
        {
            let name = node_text(name_node, source);
            if !name.is_empty() {
                let idx = symbols.len();
                let complexity = child
                    .child_by_field_name("body")
                    .map(|body| 1 + count_complexity(body, source));
                symbols.push(ExtractedSymbol {
                    name,
                    kind: SymbolKind::Method,
                    line_start: child.start_position().row as u32 + 1,
                    line_end: child.end_position().row as u32 + 1,
                    signature: extract_signature(child, source),
                    is_exported: false,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity,
                });
                if let Some(method_body) = child.child_by_field_name("body") {
                    for grand in children(method_body) {
                        extract_from_node(grand, source, Some(idx), symbols, imports, references);
                    }
                }
            }
        }
    }
}

fn extract_variable_decl(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let is_const = node.kind() == "lexical_declaration" && {
        children(node).any(|child| node_text(child, source) == "const")
    };

    for child in children(node) {
        if child.kind() != "variable_declarator" {
            continue;
        }
        let name_node = match child.child_by_field_name("name") {
            Some(n) => n,
            None => continue,
        };
        if matches!(name_node.kind(), "object_pattern" | "array_pattern") {
            continue;
        }
        let name = node_text(name_node, source);
        if name.is_empty() {
            continue;
        }

        let value = child.child_by_field_name("value");
        let is_func = value.is_some_and(|v| {
            matches!(
                v.kind(),
                "arrow_function" | "function_expression" | "function"
            )
        });

        let kind = if is_func {
            SymbolKind::Function
        } else if is_const {
            SymbolKind::Const
        } else {
            SymbolKind::Variable
        };

        let complexity = if is_func {
            value
                .and_then(|v| v.child_by_field_name("body"))
                .map(|body| 1 + count_complexity(body, source))
        } else {
            None
        };

        symbols.push(ExtractedSymbol {
            name,
            kind,
            line_start: node.start_position().row as u32 + 1,
            line_end: node.end_position().row as u32 + 1,
            signature: extract_signature(node, source),
            is_exported: false,
            parent_idx: None,
            unused_excluded: false,
            complexity,
        });
    }
}

fn extract_import(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let source_node = node.child_by_field_name("source")?;
    let raw = node_text(source_node, source);
    let specifier = unquote(raw);
    if specifier.is_empty() {
        return None;
    }

    let mut specifiers = Vec::new();
    for child in children(node) {
        if child.kind() == "import_clause" {
            collect_import_specifiers(child, source, &mut specifiers);
        }
    }

    Some(ExtractedImport {
        source: specifier,
        specifiers,
        is_reexport: false,
    })
}

fn collect_import_specifiers(node: Node, source: &[u8], specifiers: &mut Vec<String>) {
    for child in children(node) {
        match child.kind() {
            "identifier" => {
                let name = node_text(child, source);
                if !name.is_empty() {
                    specifiers.push(name);
                }
            }
            "named_imports" => {
                for spec in children(child) {
                    if spec.kind() == "import_specifier"
                        && let Some(name_node) = spec.child_by_field_name("name")
                    {
                        let name = node_text(name_node, source);
                        if !name.is_empty() {
                            specifiers.push(name);
                        }
                    }
                }
            }
            "namespace_import" => {
                for id in children(child) {
                    if id.kind() == "identifier" {
                        specifiers.push(node_text(id, source));
                    }
                }
            }
            _ => {
                collect_import_specifiers(child, source, specifiers);
            }
        }
    }
}

fn collect_export_specifiers(node: Node, source: &[u8], specifiers: &mut Vec<String>) {
    for child in children(node) {
        if child.kind() == "export_specifier"
            && let Some(name_node) = child.child_by_field_name("name")
        {
            let name = node_text(name_node, source);
            if !name.is_empty() {
                specifiers.push(name);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_ts(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = TypeScriptSupport;
        support.extract(source.as_bytes(), &tree)
    }

    fn parse_tsx(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_typescript::LANGUAGE_TSX);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = TypeScriptSupport;
        support.extract(source.as_bytes(), &tree)
    }

    fn parse_js(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_javascript::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = TypeScriptSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_function_declaration() {
        let result = parse_ts("function greet(name: string): string { return name; }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(!result.symbols[0].is_exported);
        assert!(result.symbols[0].signature.is_some());
    }

    #[test]
    fn test_exported_function() {
        let result = parse_ts("export function hello() { return 1; }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "hello");
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_class_declaration() {
        let result = parse_ts(
            "export class MyService {
                constructor() {}
                getData() { return []; }
            }",
        );
        let class_syms: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(class_syms.len(), 1);
        assert_eq!(class_syms[0].name, "MyService");
        assert!(class_syms[0].is_exported);

        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 2);
    }

    #[test]
    fn test_interface_declaration() {
        let result = parse_ts(
            "export interface UserConfig {
                name: string;
                age: number;
            }",
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "UserConfig");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Interface));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_type_alias() {
        let result = parse_ts("export type ID = string | number;");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "ID");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Type));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_enum_declaration() {
        let result = parse_ts("export enum Status { Active, Inactive }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Status");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Enum));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_arrow_function_const() {
        let result = parse_ts("export const add = (a: number, b: number) => a + b;");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "add");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_const_variable() {
        let result = parse_ts("const MAX_SIZE = 100;");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MAX_SIZE");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Const));
    }

    #[test]
    fn test_let_variable() {
        let result = parse_ts("let count = 0;");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "count");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
    }

    #[test]
    fn test_imports() {
        let result = parse_ts(
            r#"
import { useState, useEffect } from 'react';
import lodash from 'lodash';
import * as path from 'path';
import './side-effect';
"#,
        );
        assert_eq!(result.imports.len(), 4);
        assert_eq!(result.imports[0].source, "react");
        assert_eq!(result.imports[0].specifiers, vec!["useState", "useEffect"]);
        assert_eq!(result.imports[1].source, "lodash");
        assert_eq!(result.imports[1].specifiers, vec!["lodash"]);
        assert_eq!(result.imports[2].source, "path");
        assert_eq!(result.imports[2].specifiers, vec!["path"]);
        assert_eq!(result.imports[3].source, "./side-effect");
        assert!(result.imports[3].specifiers.is_empty());
    }

    #[test]
    fn test_reexport() {
        let result = parse_ts("export { foo, bar } from './utils';");
        assert_eq!(result.imports.len(), 1);
        assert!(result.imports[0].is_reexport);
        assert_eq!(result.imports[0].source, "./utils");
        assert_eq!(result.imports[0].specifiers, vec!["foo", "bar"]);
    }

    #[test]
    fn test_tsx_component() {
        let result = parse_tsx(
            r#"
export function App(): JSX.Element {
    return <div>Hello</div>;
}
"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "App");
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_js_function() {
        let result = parse_js("function helper(x) { return x * 2; }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "helper");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_line_numbers() {
        let result = parse_ts("function a() { }\n\nfunction b() {\n  return 1;\n}\n");
        assert_eq!(result.symbols.len(), 2);
        assert_eq!(result.symbols[0].line_start, 1);
        assert_eq!(result.symbols[0].line_end, 1);
        assert_eq!(result.symbols[1].line_start, 3);
        assert_eq!(result.symbols[1].line_end, 5);
    }

    #[test]
    fn test_mixed_declarations() {
        let result = parse_ts(
            r#"
import { Config } from './config';

export interface AppConfig extends Config {
    debug: boolean;
}

export const DEFAULT_CONFIG: AppConfig = { debug: false };

export function createApp(config: AppConfig) {
    return { config };
}

class InternalHelper {
    process() {}
}
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AppConfig"));
        assert!(names.contains(&"DEFAULT_CONFIG"));
        assert!(names.contains(&"createApp"));
        assert!(names.contains(&"InternalHelper"));

        let exported_count = result.symbols.iter().filter(|s| s.is_exported).count();
        assert_eq!(exported_count, 3);

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "./config");
    }

    // -- Reference extraction tests --

    #[test]
    fn test_refs_call_attributed_to_enclosing() {
        let result = parse_ts(
            r#"
function helper(): number { return 42; }
function caller(): number { return helper() + 1; }
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
    fn test_refs_member_expression_call() {
        let result = parse_ts(
            r#"
function run(svc: Service): void { svc.execute(); }
"#,
        );
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "execute" && matches!(r.kind, ReferenceKind::Call)),
            "svc.execute() should emit Call reference to `execute`"
        );
    }

    #[test]
    fn test_refs_new_expression_is_call() {
        let result = parse_ts(
            r#"
function make(): Foo { return new Foo(); }
"#,
        );
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "Foo" && matches!(r.kind, ReferenceKind::Call)),
            "new Foo() should emit Call reference to `Foo`"
        );
    }

    #[test]
    fn test_refs_type_identifier_in_param() {
        let result = parse_ts(
            r#"
function run(cfg: AppConfig): void {}
"#,
        );
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "AppConfig" && matches!(r.kind, ReferenceKind::TypeRef)),
            "AppConfig in param position should emit TypeRef"
        );
    }

    #[test]
    fn test_refs_primitive_types_filtered() {
        let result = parse_ts(
            r#"
function f(x: number): string { return "" + x; }
"#,
        );
        assert!(
            !result
                .references
                .iter()
                .any(|r| r.name == "number" || r.name == "string"),
            "built-in types must not be recorded as references"
        );
    }

    #[test]
    fn test_refs_class_method_body() {
        let result = parse_ts(
            r#"
function helper(): void {}
class Svc {
    run(): void { helper(); }
}
"#,
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
            "helper() inside run should be attributed to run method"
        );
    }

    #[test]
    fn test_refs_class_definition_name_not_self_ref() {
        let result = parse_ts("class Widget {}");
        let widget_refs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "Widget")
            .collect();
        assert!(
            widget_refs.is_empty(),
            "class Widget should not self-reference"
        );
    }

    #[test]
    fn test_refs_arrow_function_body_attributed() {
        let result = parse_ts(
            r#"
function helper(): number { return 1; }
const handler = () => helper();
"#,
        );
        let handler_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "handler")
            .expect("handler symbol");
        assert!(
            result.references.iter().any(|r| r.name == "helper"
                && matches!(r.kind, ReferenceKind::Call)
                && r.from_symbol_idx == Some(handler_idx)),
            "helper() inside arrow fn should be attributed to handler, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_exported_arrow_function_body() {
        let result = parse_ts(
            r#"
function helper(): number { return 1; }
export const handler = () => helper();
"#,
        );
        let handler_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "handler")
            .expect("handler symbol");
        assert!(
            result.references.iter().any(|r| r.name == "helper"
                && matches!(r.kind, ReferenceKind::Call)
                && r.from_symbol_idx == Some(handler_idx)),
            "helper() inside exported arrow fn should be attributed to handler, got {:?}",
            result.references
        );
    }
}
