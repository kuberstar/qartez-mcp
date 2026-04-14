use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct CSupport;

impl LanguageSupport for CSupport {
    fn extensions(&self) -> &[&str] {
        &["c", "h"]
    }

    fn language_name(&self) -> &str {
        "c"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_c::LANGUAGE)
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

fn has_static_storage(node: Node, source: &[u8]) -> bool {
    children(node).any(|child| {
        child.kind() == "storage_class_specifier" && node_text(child, source) == "static"
    })
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
        "function_definition" => {
            if let Some(sym) = extract_function(node, source) {
                let idx = symbols.len();
                symbols.push(sym);
                for child in children(node) {
                    extract_from_node(child, source, Some(idx), symbols, imports, references);
                }
                return;
            }
        }
        "struct_specifier" => {
            if let Some(sym) = extract_tagged_type(node, source, SymbolKind::Struct) {
                symbols.push(sym);
            }
        }
        "enum_specifier" => {
            if let Some(sym) = extract_tagged_type(node, source, SymbolKind::Enum) {
                symbols.push(sym);
            }
        }
        "type_definition" => {
            if let Some(sym) = extract_typedef(node, source) {
                symbols.push(sym);
            }
        }
        "declaration" => {
            if node.parent().is_none_or(|p| p.kind() == "translation_unit") {
                extract_global_declaration(node, source, symbols);
            }
        }
        "preproc_def" => {
            if let Some(sym) = extract_macro(node, source) {
                symbols.push(sym);
            }
        }
        "preproc_include" => {
            if let Some(imp) = extract_include(node, source) {
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

/// Counts branching nodes inside a function body for cyclomatic complexity.
/// Recursively walks all children. C has no nested function boundaries to stop at.
fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut count = 0;
    match node.kind() {
        "if_statement" => count += 1,
        "case_statement" => count += 1,
        "for_statement" | "while_statement" | "do_statement" => count += 1,
        "conditional_expression" => count += 1,
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

fn extract_function(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let declarator = node.child_by_field_name("declarator")?;
    let name = find_declarator_name(declarator, source)?;
    if name.is_empty() {
        return None;
    }
    let complexity = node
        .child_by_field_name("body")
        .map(|body| 1 + count_complexity(body, source));
    Some(ExtractedSymbol {
        is_exported: !has_static_storage(node, source),
        name,
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity,
    })
}

fn extract_tagged_type(node: Node, source: &[u8], kind: SymbolKind) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    // Only extract if it has a body (definition, not just forward declaration)
    node.child_by_field_name("body")?;
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
    })
}

fn extract_typedef(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let declarator = node.child_by_field_name("declarator")?;
    let name = find_declarator_name(declarator, source)?;
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Type,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    })
}

fn extract_global_declaration(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let is_static = has_static_storage(node, source);
    for child in children(node) {
        if child.kind() == "init_declarator" || child.kind() == "identifier" {
            let name = if child.kind() == "identifier" {
                node_text(child, source)
            } else {
                match find_declarator_name(child, source) {
                    Some(n) => n,
                    None => continue,
                }
            };
            if name.is_empty() {
                continue;
            }
            // Skip function declarations (prototypes)
            let has_function_declarator =
                children(child).any(|c| c.kind() == "function_declarator");
            if has_function_declarator {
                continue;
            }
            symbols.push(ExtractedSymbol {
                name,
                kind: SymbolKind::Variable,
                line_start: node.start_position().row as u32 + 1,
                line_end: node.end_position().row as u32 + 1,
                signature: extract_signature(node, source),
                is_exported: !is_static,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
            });
        }
    }
}

fn extract_macro(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Const,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    })
}

fn extract_include(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let path_node = node.child_by_field_name("path")?;
    let raw = node_text(path_node, source);
    // Only extract quoted includes (local headers), not angle-bracket system headers
    if raw.starts_with('"') && raw.ends_with('"') {
        let path = raw[1..raw.len() - 1].to_string();
        if !path.is_empty() {
            return Some(ExtractedImport {
                source: path,
                specifiers: vec![],
                is_reexport: false,
            });
        }
    }
    None
}

fn find_declarator_name(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "type_identifier" | "field_identifier" | "primitive_type" => {
            let name = node_text(node, source);
            if name.is_empty() { None } else { Some(name) }
        }
        "function_declarator"
        | "pointer_declarator"
        | "array_declarator"
        | "parenthesized_declarator"
        | "init_declarator" => {
            let declarator = node.child_by_field_name("declarator")?;
            find_declarator_name(declarator, source)
        }
        _ => {
            for child in children(node) {
                if let Some(name) = find_declarator_name(child, source) {
                    return Some(name);
                }
            }
            None
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

fn record_reference(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    match node.kind() {
        "call_expression" => {
            let name = node
                .child_by_field_name("function")
                .map(|f| extract_callee_name(f, source))
                .unwrap_or_default();
            if !name.is_empty() && !is_builtin_callable(&name) {
                references.push(ExtractedReference {
                    name,
                    line: node.start_position().row as u32 + 1,
                    from_symbol_idx: enclosing,
                    kind: ReferenceKind::Call,
                    receiver_type_hint: None,
                });
            }
        }
        "type_identifier" => {
            let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
            if matches!(
                parent_kind,
                "struct_specifier" | "enum_specifier" | "type_definition"
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
                    receiver_type_hint: None,
                });
            }
        }
        _ => {}
    }
}

fn extract_callee_name(func: Node, source: &[u8]) -> String {
    match func.kind() {
        "identifier" => node_text(func, source),
        _ => String::new(),
    }
}

fn is_builtin_callable(name: &str) -> bool {
    matches!(
        name,
        "printf"
            | "scanf"
            | "malloc"
            | "free"
            | "calloc"
            | "realloc"
            | "memcpy"
            | "memset"
            | "strlen"
            | "strcmp"
            | "strcpy"
            | "sizeof"
            | "assert"
            | "exit"
            | "abort"
            | "fprintf"
            | "sprintf"
            | "snprintf"
    )
}

fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "long"
            | "short"
            | "char"
            | "float"
            | "double"
            | "void"
            | "unsigned"
            | "signed"
            | "size_t"
            | "FILE"
            | "bool"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_c(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_c::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = CSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_function_definition() {
        let result = parse_c("int add(int a, int b) { return a + b; }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "add");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
        assert!(result.symbols[0].signature.is_some());
    }

    #[test]
    fn test_static_function() {
        let result = parse_c("static int helper(int x) { return x * 2; }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "helper");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_struct() {
        let result = parse_c("struct Point { int x; int y; };");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Point");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Struct));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_enum() {
        let result = parse_c("enum Color { RED, GREEN, BLUE };");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Color");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Enum));
    }

    #[test]
    fn test_typedef() {
        let result = parse_c("typedef unsigned long size_t;");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "size_t");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Type));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_macro() {
        let result = parse_c("#define MAX_SIZE 1024");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MAX_SIZE");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Const));
    }

    #[test]
    fn test_local_include() {
        let result = parse_c("#include \"myheader.h\"");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "myheader.h");
    }

    #[test]
    fn test_system_include_skipped() {
        let result = parse_c("#include <stdio.h>");
        assert_eq!(result.imports.len(), 0);
    }

    #[test]
    fn test_empty_file() {
        let result = parse_c("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_mixed_declarations() {
        let result = parse_c(
            r#"#include "utils.h"
#define BUF_SIZE 256

struct Buffer { char data[256]; int len; };

typedef struct Buffer Buffer;

static int internal_count = 0;

int process(Buffer *buf) { return buf->len; }
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"BUF_SIZE"));
        assert!(names.contains(&"Buffer"));
        assert!(names.contains(&"process"));

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "utils.h");
    }

    #[test]
    fn test_refs_call_attributed_to_function() {
        let result = parse_c(
            r#"int helper() { return 1; }
int caller() { return helper(); }
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
    fn test_refs_type_identifier() {
        let result = parse_c(
            r#"typedef struct Config Config;
Config* create_config() { return 0; }
"#,
        );
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "Config" && matches!(r.kind, ReferenceKind::TypeRef)),
            "Config in return type should emit TypeRef, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_builtin_filtered() {
        let result = parse_c(
            r#"void f() {
    int* p = malloc(10);
    free(p);
    printf("hello");
}
"#,
        );
        assert!(
            !result
                .references
                .iter()
                .any(|r| r.name == "malloc" || r.name == "free" || r.name == "printf"),
            "built-in calls must not be recorded as references"
        );
    }

    #[test]
    fn test_refs_builtin_types_filtered() {
        let result = parse_c("int process(char* buf) { return 0; }");
        assert!(
            !result
                .references
                .iter()
                .any(|r| r.name == "int" || r.name == "char"),
            "built-in types must not be recorded as references"
        );
    }
}
