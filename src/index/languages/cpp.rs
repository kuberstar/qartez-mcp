use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct CppSupport;

impl LanguageSupport for CppSupport {
    fn extensions(&self) -> &[&str] {
        &["cpp", "cc", "cxx", "hpp", "hh", "hxx"]
    }

    fn language_name(&self) -> &str {
        "cpp"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_cpp::LANGUAGE)
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
    in_anonymous_ns: bool,
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    match node.kind() {
        "function_definition" => {
            if let Some(sym) = extract_function(node, source, in_anonymous_ns) {
                let idx = symbols.len();
                symbols.push(sym);
                if let Some(body) = node.child_by_field_name("body") {
                    for child in children(body) {
                        extract_from_node(
                            child,
                            source,
                            in_anonymous_ns,
                            Some(idx),
                            symbols,
                            imports,
                            references,
                        );
                    }
                }
                return;
            }
        }
        "class_specifier" => {
            if let Some(sym) = extract_tagged_type(node, source, SymbolKind::Class, in_anonymous_ns)
            {
                symbols.push(sym);
            }
            extract_class_methods(
                node,
                source,
                in_anonymous_ns,
                symbols,
                imports,
                references,
            );
            return;
        }
        "struct_specifier" => {
            if let Some(sym) =
                extract_tagged_type(node, source, SymbolKind::Struct, in_anonymous_ns)
            {
                symbols.push(sym);
            }
        }
        "enum_specifier" => {
            if let Some(sym) = extract_tagged_type(node, source, SymbolKind::Enum, in_anonymous_ns)
            {
                symbols.push(sym);
            }
        }
        "namespace_definition" => {
            let name_node = node.child_by_field_name("name");
            let is_anonymous = name_node.is_none();
            if !is_anonymous && let Some(n) = name_node {
                let name = node_text(n, source);
                if !name.is_empty() {
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Module,
                        line_start: node.start_position().row as u32 + 1,
                        line_end: node.end_position().row as u32 + 1,
                        signature: extract_signature(node, source),
                        is_exported: !in_anonymous_ns,
                        parent_idx: None,
                        unused_excluded: false,
                        complexity: None,
                    });
                }
            }
            if let Some(body) = node.child_by_field_name("body") {
                for child in children(body) {
                    extract_from_node(
                        child,
                        source,
                        in_anonymous_ns || is_anonymous,
                        enclosing,
                        symbols,
                        imports,
                        references,
                    );
                }
            }
            return;
        }
        "template_declaration" => {
            for child in children(node) {
                match child.kind() {
                    "function_definition" => {
                        if let Some(sym) = extract_function(child, source, in_anonymous_ns) {
                            let idx = symbols.len();
                            symbols.push(sym);
                            if let Some(body) = child.child_by_field_name("body") {
                                for grand in children(body) {
                                    extract_from_node(
                                        grand,
                                        source,
                                        in_anonymous_ns,
                                        Some(idx),
                                        symbols,
                                        imports,
                                        references,
                                    );
                                }
                            }
                        }
                    }
                    "class_specifier" => {
                        if let Some(sym) =
                            extract_tagged_type(child, source, SymbolKind::Class, in_anonymous_ns)
                        {
                            symbols.push(sym);
                        }
                        extract_class_methods(
                            child,
                            source,
                            in_anonymous_ns,
                            symbols,
                            imports,
                            references,
                        );
                    }
                    "struct_specifier" => {
                        if let Some(sym) =
                            extract_tagged_type(child, source, SymbolKind::Struct, in_anonymous_ns)
                        {
                            symbols.push(sym);
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
        "preproc_include" => {
            if let Some(imp) = extract_include(node, source) {
                imports.push(imp);
            }
            return;
        }
        "using_declaration" => {
            if let Some(imp) = extract_using(node, source) {
                imports.push(imp);
            }
            return;
        }
        _ => {}
    }

    record_reference(node, source, enclosing, references);

    for child in children(node) {
        extract_from_node(
            child,
            source,
            in_anonymous_ns,
            enclosing,
            symbols,
            imports,
            references,
        );
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
        "new_expression" => {
            let name = node
                .child_by_field_name("type")
                .map(|t| extract_callee_name(t, source))
                .unwrap_or_default();
            if !name.is_empty() && !is_builtin_type(&name) {
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
                "class_specifier"
                    | "struct_specifier"
                    | "enum_specifier"
                    | "type_definition"
                    | "base_class_clause"
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
        "identifier" | "type_identifier" => node_text(func, source),
        "qualified_identifier" | "template_function" => {
            let full = node_text(func, source);
            full.rsplit("::").next().unwrap_or(&full).to_string()
        }
        "field_expression" => func
            .child_by_field_name("field")
            .map(|n| node_text(n, source))
            .unwrap_or_default(),
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
            | "cout"
            | "cin"
            | "endl"
            | "cerr"
            | "clog"
            | "make_shared"
            | "make_unique"
            | "static_cast"
            | "dynamic_cast"
            | "const_cast"
            | "reinterpret_cast"
            | "move"
            | "forward"
            | "swap"
            | "begin"
            | "end"
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
            | "string"
            | "vector"
            | "map"
            | "set"
            | "unique_ptr"
            | "shared_ptr"
            | "optional"
            | "pair"
            | "tuple"
            | "array"
            | "nullptr_t"
            | "auto"
    )
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
        "case_statement" => count += 1,
        "for_statement" | "for_range_loop" | "while_statement" | "do_statement" => count += 1,
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

fn extract_function(node: Node, source: &[u8], in_anonymous_ns: bool) -> Option<ExtractedSymbol> {
    let declarator = node.child_by_field_name("declarator")?;
    let name = find_declarator_name(declarator, source)?;
    if name.is_empty() {
        return None;
    }
    let complexity = node
        .child_by_field_name("body")
        .map(|body| 1 + count_complexity(body, source));
    Some(ExtractedSymbol {
        is_exported: !in_anonymous_ns && !has_static_storage(node, source),
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

fn extract_tagged_type(
    node: Node,
    source: &[u8],
    kind: SymbolKind,
    in_anonymous_ns: bool,
) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    node.child_by_field_name("body")?;
    Some(ExtractedSymbol {
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: !in_anonymous_ns,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    })
}

fn extract_class_methods(
    class_node: Node,
    source: &[u8],
    in_anonymous_ns: bool,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    let body = match class_node.child_by_field_name("body") {
        Some(b) => b,
        None => return,
    };
    for child in children(body) {
        match child.kind() {
            "function_definition" => {
                if let Some(declarator) = child.child_by_field_name("declarator")
                    && let Some(name) = find_declarator_name(declarator, source)
                    && !name.is_empty()
                {
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
                        is_exported: has_public_access(child, source),
                        parent_idx: None,
                        unused_excluded: false,
                        complexity,
                    });
                    if let Some(body) = child.child_by_field_name("body") {
                        for grand in children(body) {
                            extract_from_node(
                                grand,
                                source,
                                in_anonymous_ns,
                                Some(idx),
                                symbols,
                                imports,
                                references,
                            );
                        }
                    }
                }
            }
            "declaration" => {
                for decl_child in children(child) {
                    if decl_child.kind() == "function_declarator"
                        && let Some(name) = find_declarator_name(decl_child, source)
                        && !name.is_empty()
                    {
                        symbols.push(ExtractedSymbol {
                            name,
                            kind: SymbolKind::Method,
                            line_start: child.start_position().row as u32 + 1,
                            line_end: child.end_position().row as u32 + 1,
                            signature: extract_signature(child, source),
                            is_exported: has_public_access(child, source),
                            parent_idx: None,
                            unused_excluded: false,
                            complexity: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

fn has_public_access(node: Node, source: &[u8]) -> bool {
    children(node).any(|child| {
        child.kind() == "access_specifier" && node_text(child, source).contains("public")
    })
}

fn extract_include(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let path_node = node.child_by_field_name("path")?;
    let raw = node_text(path_node, source);
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

fn extract_using(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let full = node_text(node, source);
    let trimmed = full.trim().trim_end_matches(';').trim();
    let path = trimmed.strip_prefix("using")?;
    let path = path.trim().strip_prefix("namespace").unwrap_or(path).trim();
    if path.is_empty() {
        return None;
    }
    let parts: Vec<&str> = path.split("::").collect();
    let specifier = parts.last().unwrap_or(&"").to_string();
    let source_path = if parts.len() > 1 {
        parts[..parts.len() - 1].join("::")
    } else {
        path.to_string()
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

fn find_declarator_name(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "type_identifier" | "field_identifier" | "destructor_name" => {
            let name = node_text(node, source);
            if name.is_empty() { None } else { Some(name) }
        }
        "qualified_identifier" | "template_function" => {
            let full = node_text(node, source);
            let name = full.rsplit("::").next().unwrap_or(&full).to_string();
            if name.is_empty() { None } else { Some(name) }
        }
        "function_declarator"
        | "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "parenthesized_declarator"
        | "init_declarator"
        | "structured_binding_declarator" => {
            let declarator = node.child_by_field_name("declarator")?;
            find_declarator_name(declarator, source)
        }
        "operator_name" => {
            let name = node_text(node, source);
            if name.is_empty() { None } else { Some(name) }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_cpp(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_cpp::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = CppSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_function_definition() {
        let result = parse_cpp("int add(int a, int b) { return a + b; }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "add");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_static_function() {
        let result = parse_cpp("static int helper(int x) { return x * 2; }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "helper");
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_class() {
        let result = parse_cpp("class MyClass { public: void run(); };");
        let classes: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "MyClass");
        assert!(classes[0].is_exported);
    }

    #[test]
    fn test_struct() {
        let result = parse_cpp("struct Point { int x; int y; };");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Point");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Struct));
    }

    #[test]
    fn test_enum() {
        let result = parse_cpp("enum Color { RED, GREEN, BLUE };");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Color");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Enum));
    }

    #[test]
    fn test_namespace() {
        let result = parse_cpp("namespace utils {\n    int helper() { return 42; }\n}");
        let ns: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Module))
            .collect();
        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].name, "utils");
    }

    #[test]
    fn test_anonymous_namespace_not_exported() {
        let result = parse_cpp("namespace {\n    int secret() { return 0; }\n}");
        let funcs: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function))
            .collect();
        assert_eq!(funcs.len(), 1);
        assert!(!funcs[0].is_exported);
    }

    #[test]
    fn test_local_include() {
        let result = parse_cpp("#include \"myheader.hpp\"");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "myheader.hpp");
    }

    #[test]
    fn test_system_include_skipped() {
        let result = parse_cpp("#include <iostream>");
        assert_eq!(result.imports.len(), 0);
    }

    #[test]
    fn test_using_declaration() {
        let result = parse_cpp("using std::cout;");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "std");
        assert_eq!(result.imports[0].specifiers, vec!["cout"]);
    }

    #[test]
    fn test_empty_file() {
        let result = parse_cpp("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_template_class() {
        let result = parse_cpp("template<typename T>\nclass Container { T value; };");
        let classes: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Container");
    }

    #[test]
    fn test_refs_call_attributed_to_function() {
        let result = parse_cpp(
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
        let result = parse_cpp(
            r#"struct Config {};
Config create() { Config c; return c; }
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
        let result = parse_cpp(
            r#"void f() {
    auto p = make_shared<int>(42);
}
"#,
        );
        assert!(
            !result
                .references
                .iter()
                .any(|r| r.name == "make_shared"),
            "built-in calls must not be recorded as references"
        );
    }

    #[test]
    fn test_refs_method_in_class() {
        let result = parse_cpp(
            r#"class Svc {
    void run() { process(); }
    void process() {}
};
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
}
