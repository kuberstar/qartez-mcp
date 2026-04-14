use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct GoSupport;

impl LanguageSupport for GoSupport {
    fn extensions(&self) -> &[&str] {
        &["go"]
    }

    fn language_name(&self) -> &str {
        "go"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_go::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let mut references = Vec::new();
        let root = tree.root_node();
        extract_from_node(root, source, None, &mut symbols, &mut imports, &mut references);
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

fn is_exported(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase())
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    let new_enclosing = enclosing;
    match node.kind() {
        "function_declaration" => {
            if let Some(sym) = extract_function(node, source) {
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
        "method_declaration" => {
            if let Some(sym) = extract_method(node, source) {
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
        "type_declaration" => {
            extract_type_declaration(node, source, symbols);
            return;
        }
        "const_declaration" => {
            extract_const_or_var(node, source, symbols, SymbolKind::Const, "const_spec");
            return;
        }
        "var_declaration" => {
            extract_const_or_var(node, source, symbols, SymbolKind::Variable, "var_spec");
            return;
        }
        "import_declaration" => {
            extract_imports(node, source, imports);
            return;
        }
        _ => {}
    }

    record_reference(node, source, enclosing, references);

    for child in children(node) {
        extract_from_node(child, source, new_enclosing, symbols, imports, references);
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
                });
            }
        }
        "type_identifier" => {
            let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
            if matches!(
                parent_kind,
                "type_spec" | "type_declaration" | "method_declaration"
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
                });
            }
        }
        _ => {}
    }
}

fn extract_callee_name(func: Node, source: &[u8]) -> String {
    match func.kind() {
        "identifier" => node_text(func, source),
        "selector_expression" => func
            .child_by_field_name("field")
            .map(|n| node_text(n, source))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn is_builtin_callable(name: &str) -> bool {
    matches!(
        name,
        "make"
            | "len"
            | "cap"
            | "append"
            | "copy"
            | "delete"
            | "close"
            | "new"
            | "panic"
            | "recover"
            | "print"
            | "println"
            | "complex"
            | "real"
            | "imag"
            | "clear"
            | "min"
            | "max"
    )
}

fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "byte"
            | "complex64"
            | "complex128"
            | "error"
            | "float32"
            | "float64"
            | "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "rune"
            | "string"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "uintptr"
            | "any"
            | "comparable"
    )
}

/// Recursively count cyclomatic complexity branching points in the AST subtree
/// rooted at `node`. Stops at nested function literal boundaries so their
/// internal branching is not attributed to the enclosing function.
fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if_statement" => 1,
        "expression_case" => 1,
        "for_statement" => 1,
        "binary_expression" => {
            let op = node
                .child_by_field_name("operator")
                .map(|n| node_text(n, source));
            match op.as_deref() {
                Some("&&") | Some("||") => 1,
                _ => 0,
            }
        }
        // Nested function literals are separate scopes; do not count
        // their branching as part of the enclosing function.
        "func_literal" => return 0,
        _ => 0,
    };
    for child in children(node) {
        total += count_complexity(child, source);
    }
    total
}

fn extract_function(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let body_cc = node
        .child_by_field_name("body")
        .map(|body| count_complexity(body, source))
        .unwrap_or(0);
    Some(ExtractedSymbol {
        is_exported: is_exported(&name),
        name,
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: Some(1 + body_cc),
    })
}

fn extract_method(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let body_cc = node
        .child_by_field_name("body")
        .map(|body| count_complexity(body, source))
        .unwrap_or(0);
    Some(ExtractedSymbol {
        is_exported: is_exported(&name),
        name,
        kind: SymbolKind::Method,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: Some(1 + body_cc),
    })
}

fn extract_type_declaration(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(node) {
        if child.kind() == "type_spec"
            && let Some(sym) = extract_type_spec(child, node, source)
        {
            symbols.push(sym);
        }
    }
}

fn extract_type_spec(spec: Node, decl: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = spec.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }

    let type_node = spec.child_by_field_name("type");
    let kind = match type_node.map(|n| n.kind()) {
        Some("struct_type") => SymbolKind::Struct,
        Some("interface_type") => SymbolKind::Interface,
        _ => SymbolKind::Type,
    };

    Some(ExtractedSymbol {
        is_exported: is_exported(&name),
        name,
        kind,
        line_start: decl.start_position().row as u32 + 1,
        line_end: decl.end_position().row as u32 + 1,
        signature: extract_signature(decl, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    })
}

fn extract_const_or_var(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    kind: SymbolKind,
    spec_kind: &str,
) {
    for child in children(node) {
        if child.kind() == spec_kind {
            extract_single_spec(child, source, symbols, &kind);
        } else if child.kind().ends_with("_spec_list") {
            for grandchild in children(child) {
                if grandchild.kind() == spec_kind {
                    extract_single_spec(grandchild, source, symbols, &kind);
                }
            }
        }
    }
}

fn extract_single_spec(
    spec: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    kind: &SymbolKind,
) {
    if let Some(name_node) = spec.child_by_field_name("name") {
        let name = node_text(name_node, source);
        if !name.is_empty() {
            symbols.push(ExtractedSymbol {
                is_exported: is_exported(&name),
                name,
                kind: kind.clone(),
                line_start: spec.start_position().row as u32 + 1,
                line_end: spec.end_position().row as u32 + 1,
                signature: extract_signature(spec, source),
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
            });
        }
    }
}

fn extract_imports(node: Node, source: &[u8], imports: &mut Vec<ExtractedImport>) {
    for child in children(node) {
        match child.kind() {
            "import_spec" => {
                if let Some(imp) = extract_single_import(child, source) {
                    imports.push(imp);
                }
            }
            "import_spec_list" => {
                for spec in children(child) {
                    if spec.kind() == "import_spec"
                        && let Some(imp) = extract_single_import(spec, source)
                    {
                        imports.push(imp);
                    }
                }
            }
            _ => {}
        }
    }
}

fn extract_single_import(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let path_node = node.child_by_field_name("path")?;
    let raw = node_text(path_node, source);
    let path = unquote(raw);
    if path.is_empty() {
        return None;
    }

    let alias = node
        .child_by_field_name("name")
        .map(|n| node_text(n, source));

    let specifiers = match alias {
        Some(a) if !a.is_empty() => vec![a],
        _ => vec![],
    };

    Some(ExtractedImport {
        source: path,
        specifiers,
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

    let truncated = if sig.len() > 200 { &sig[..sig.floor_char_boundary(200)] } else { sig };
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
    if trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_go(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_go::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = GoSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_exported_function() {
        let result = parse_go("package main\n\nfunc Hello(name string) string { return name }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Hello");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
        assert!(result.symbols[0].signature.is_some());
    }

    #[test]
    fn test_unexported_function() {
        let result = parse_go("package main\n\nfunc helper(x int) int { return x * 2 }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "helper");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_struct() {
        let result =
            parse_go("package main\n\ntype Config struct {\n\tName string\n\tDebug bool\n}");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Config");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Struct));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_unexported_struct() {
        let result = parse_go("package main\n\ntype config struct {\n\tname string\n}");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "config");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Struct));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_interface() {
        let result = parse_go(
            "package main\n\ntype Reader interface {\n\tRead(p []byte) (n int, err error)\n}",
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Reader");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Interface));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_type_alias() {
        let result = parse_go("package main\n\ntype ID string");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "ID");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Type));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_method() {
        let result = parse_go("package main\n\nfunc (c *Config) Validate() error { return nil }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Validate");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Method));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_unexported_method() {
        let result = parse_go("package main\n\nfunc (c *Config) validate() error { return nil }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "validate");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Method));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_const_single() {
        let result = parse_go("package main\n\nconst MaxSize = 1024");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MaxSize");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Const));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_const_block() {
        let result = parse_go("package main\n\nconst (\n\tMaxSize = 1024\n\tminSize = 1\n)");
        assert_eq!(result.symbols.len(), 2);
        assert_eq!(result.symbols[0].name, "MaxSize");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Const));
        assert!(result.symbols[0].is_exported);
        assert_eq!(result.symbols[1].name, "minSize");
        assert!(matches!(result.symbols[1].kind, SymbolKind::Const));
        assert!(!result.symbols[1].is_exported);
    }

    #[test]
    fn test_var_single() {
        let result = parse_go("package main\n\nvar Counter int");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Counter");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_var_block() {
        let result = parse_go("package main\n\nvar (\n\tGlobal = 42\n\tlocal = 0\n)");
        assert_eq!(result.symbols.len(), 2);
        assert_eq!(result.symbols[0].name, "Global");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(result.symbols[0].is_exported);
        assert_eq!(result.symbols[1].name, "local");
        assert!(!result.symbols[1].is_exported);
    }

    #[test]
    fn test_single_import() {
        let result = parse_go("package main\n\nimport \"fmt\"");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "fmt");
        assert!(result.imports[0].specifiers.is_empty());
    }

    #[test]
    fn test_grouped_imports() {
        let result = parse_go("package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n\t\"net/http\"\n)");
        assert_eq!(result.imports.len(), 3);
        assert_eq!(result.imports[0].source, "fmt");
        assert_eq!(result.imports[1].source, "os");
        assert_eq!(result.imports[2].source, "net/http");
    }

    #[test]
    fn test_aliased_import() {
        let result = parse_go("package main\n\nimport myhttp \"net/http\"");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "net/http");
        assert_eq!(result.imports[0].specifiers, vec!["myhttp"]);
    }

    #[test]
    fn test_dot_import() {
        let result = parse_go("package main\n\nimport . \"fmt\"");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "fmt");
        assert_eq!(result.imports[0].specifiers, vec!["."]);
    }

    #[test]
    fn test_line_numbers() {
        let result = parse_go("package main\n\nfunc a() {}\n\nfunc b() {\n\treturn\n}\n");
        assert_eq!(result.symbols.len(), 2);
        assert_eq!(result.symbols[0].line_start, 3);
        assert_eq!(result.symbols[0].line_end, 3);
        assert_eq!(result.symbols[1].line_start, 5);
        assert_eq!(result.symbols[1].line_end, 7);
    }

    #[test]
    fn test_mixed_declarations() {
        let result = parse_go(
            r#"package main

import (
	"fmt"
	"os"
)

type AppConfig struct {
	Debug bool
}

const DefaultValue = 42

func CreateApp(config AppConfig) *App {
	return &App{config: config}
}

func (a *App) Run() error {
	return nil
}

type internalState int
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AppConfig"));
        assert!(names.contains(&"DefaultValue"));
        assert!(names.contains(&"CreateApp"));
        assert!(names.contains(&"Run"));
        assert!(names.contains(&"internalState"));

        let exported_count = result.symbols.iter().filter(|s| s.is_exported).count();
        assert_eq!(exported_count, 4);

        assert_eq!(result.imports.len(), 2);
        assert_eq!(result.imports[0].source, "fmt");
        assert_eq!(result.imports[1].source, "os");
    }

    #[test]
    fn test_refs_call_attributed_to_function() {
        let result = parse_go(
            r#"package main

func helper() int { return 1 }
func caller() int { return helper() }
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
    fn test_refs_method_call() {
        let result = parse_go(
            r#"package main

type Svc struct{}

func (s *Svc) Run() { s.Process() }
func (s *Svc) Process() {}
"#,
        );
        let run_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "Run")
            .expect("Run method");
        assert!(
            result.references.iter().any(|r| r.name == "Process"
                && matches!(r.kind, ReferenceKind::Call)
                && r.from_symbol_idx == Some(run_idx)),
            "Process() inside Run should be attributed to Run, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_type_identifier() {
        let result = parse_go(
            r#"package main

type Config struct{}
func New() Config { return Config{} }
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
        let result = parse_go(
            r#"package main

func f() {
    s := make([]int, 10)
    _ = len(s)
    _ = append(s, 1)
}
"#,
        );
        assert!(
            !result
                .references
                .iter()
                .any(|r| r.name == "make" || r.name == "len" || r.name == "append"),
            "built-in calls must not be recorded as references"
        );
    }

    #[test]
    fn test_refs_builtin_types_filtered() {
        let result = parse_go(
            r#"package main

func f(x int) string { return "" }
"#,
        );
        assert!(
            !result
                .references
                .iter()
                .any(|r| r.name == "int" || r.name == "string"),
            "built-in types must not be recorded as references"
        );
    }
}
