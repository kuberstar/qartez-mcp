use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct JavaSupport;

impl LanguageSupport for JavaSupport {
    fn extensions(&self) -> &[&str] {
        &["java"]
    }

    fn language_name(&self) -> &str {
        "java"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_java::LANGUAGE)
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

fn has_modifier(node: Node, source: &[u8], modifier: &str) -> bool {
    children(node).any(|child| {
        child.kind() == "modifiers" && children(child).any(|m| node_text(m, source) == modifier)
    })
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
    match node.kind() {
        "class_declaration" => {
            let idx = if let Some(sym) = extract_named_decl(node, source, SymbolKind::Class) {
                let i = symbols.len();
                symbols.push(sym);
                Some(i)
            } else {
                enclosing
            };
            extract_class_body(node, source, idx, symbols, imports, references);
            return;
        }
        "interface_declaration" => {
            let idx = if let Some(sym) = extract_named_decl(node, source, SymbolKind::Interface) {
                let i = symbols.len();
                symbols.push(sym);
                Some(i)
            } else {
                enclosing
            };
            extract_class_body(node, source, idx, symbols, imports, references);
            return;
        }
        "enum_declaration" => {
            let idx = if let Some(sym) = extract_named_decl(node, source, SymbolKind::Enum) {
                let i = symbols.len();
                symbols.push(sym);
                Some(i)
            } else {
                enclosing
            };
            extract_class_body(node, source, idx, symbols, imports, references);
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
        "method_invocation" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source))
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
        "object_creation_expression" => {
            let name = node
                .child_by_field_name("type")
                .and_then(|t| {
                    if t.kind() == "type_identifier" {
                        Some(node_text(t, source))
                    } else {
                        t.child_by_field_name("name").map(|n| node_text(n, source))
                    }
                })
                .unwrap_or_default();
            if !name.is_empty() && !is_builtin_type(&name) {
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
                "class_declaration"
                    | "interface_declaration"
                    | "enum_declaration"
                    | "constructor_declaration"
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

fn is_builtin_callable(name: &str) -> bool {
    matches!(
        name,
        "toString"
            | "equals"
            | "hashCode"
            | "getClass"
            | "notify"
            | "notifyAll"
            | "wait"
            | "clone"
            | "finalize"
            | "valueOf"
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
            | "boolean"
            | "char"
            | "void"
            | "String"
            | "Object"
            | "Integer"
            | "Long"
            | "Short"
            | "Byte"
            | "Float"
            | "Double"
            | "Boolean"
            | "Character"
            | "Void"
            | "Class"
            | "System"
            | "Throwable"
            | "Exception"
            | "RuntimeException"
            | "Error"
            | "Override"
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
        "switch_block_statement_group" => count += 1,
        "for_statement" | "enhanced_for_statement" | "while_statement" | "do_statement" => {
            count += 1;
        }
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
    })
}

fn extract_class_body(
    class_node: Node,
    source: &[u8],
    class_enclosing: Option<usize>,
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
            "method_declaration" => {
                if let Some(sym) = extract_method(child, source) {
                    let idx = symbols.len();
                    symbols.push(sym);
                    for grand in children(child) {
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
            "constructor_declaration" => {
                if let Some(sym) = extract_constructor(child, source) {
                    let idx = symbols.len();
                    symbols.push(sym);
                    for grand in children(child) {
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
            "field_declaration" => {
                extract_field(child, source, symbols);
            }
            "class_declaration" => {
                let idx =
                    if let Some(sym) = extract_named_decl(child, source, SymbolKind::Class) {
                        let i = symbols.len();
                        symbols.push(sym);
                        Some(i)
                    } else {
                        class_enclosing
                    };
                extract_class_body(child, source, idx, symbols, imports, references);
            }
            "interface_declaration" => {
                let idx =
                    if let Some(sym) = extract_named_decl(child, source, SymbolKind::Interface) {
                        let i = symbols.len();
                        symbols.push(sym);
                        Some(i)
                    } else {
                        class_enclosing
                    };
                extract_class_body(child, source, idx, symbols, imports, references);
            }
            "enum_declaration" => {
                let idx =
                    if let Some(sym) = extract_named_decl(child, source, SymbolKind::Enum) {
                        let i = symbols.len();
                        symbols.push(sym);
                        Some(i)
                    } else {
                        class_enclosing
                    };
                extract_class_body(child, source, idx, symbols, imports, references);
            }
            _ => {}
        }
    }
}

fn extract_method(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let complexity = node
        .child_by_field_name("body")
        .map(|body| 1 + count_complexity(body, source));
    Some(ExtractedSymbol {
        is_exported: has_public(node, source),
        name,
        kind: SymbolKind::Method,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity,
    })
}

fn extract_constructor(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let complexity = node
        .child_by_field_name("body")
        .map(|body| 1 + count_complexity(body, source));
    Some(ExtractedSymbol {
        is_exported: has_public(node, source),
        name,
        kind: SymbolKind::Method,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity,
    })
}

fn extract_field(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let is_static = has_modifier(node, source, "static");
    let is_final = has_modifier(node, source, "final");
    // Only extract static final fields (constants)
    if !(is_static && is_final) {
        return;
    }
    for child in children(node) {
        if child.kind() == "variable_declarator"
            && let Some(name_node) = child.child_by_field_name("name")
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
                });
            }
        }
    }
}

fn extract_import(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let full = node_text(node, source);
    let trimmed = full.trim().trim_end_matches(';').trim();
    let path = trimmed
        .strip_prefix("import")?
        .trim()
        .strip_prefix("static")
        .map_or_else(
            || trimmed.strip_prefix("import").unwrap().trim(),
            |s| s.trim(),
        );

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

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_java(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_java::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = JavaSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_public_class() {
        let result = parse_java("public class MyService { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MyService");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_package_private_class() {
        let result = parse_java("class Helper { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Helper");
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_interface() {
        let result = parse_java("public interface Repository { void save(); }");
        let ifaces: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Interface))
            .collect();
        assert_eq!(ifaces.len(), 1);
        assert_eq!(ifaces[0].name, "Repository");
        assert!(ifaces[0].is_exported);
    }

    #[test]
    fn test_enum() {
        let result = parse_java("public enum Status { ACTIVE, INACTIVE }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Status");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Enum));
    }

    #[test]
    fn test_method() {
        let result = parse_java(
            "public class Foo {\n    public void run() { }\n    private int count() { return 0; }\n}",
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
    fn test_constructor() {
        let result = parse_java("public class Foo {\n    public Foo(int x) { }\n}");
        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "Foo");
    }

    #[test]
    fn test_static_final_field() {
        let result = parse_java("public class Config {\n    public static final int MAX = 100;\n}");
        let consts: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Const))
            .collect();
        assert_eq!(consts.len(), 1);
        assert_eq!(consts[0].name, "MAX");
        assert!(consts[0].is_exported);
    }

    #[test]
    fn test_non_constant_field_skipped() {
        let result = parse_java("public class Foo {\n    private int value;\n}");
        let consts: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Const))
            .collect();
        assert_eq!(consts.len(), 0);
    }

    #[test]
    fn test_import() {
        let result = parse_java("import java.util.List;");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "java.util");
        assert_eq!(result.imports[0].specifiers, vec!["List"]);
    }

    #[test]
    fn test_wildcard_import() {
        let result = parse_java("import java.util.*;");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "java.util");
        assert_eq!(result.imports[0].specifiers, vec!["*"]);
    }

    #[test]
    fn test_static_import() {
        let result = parse_java("import static org.junit.Assert.assertEquals;");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "org.junit.Assert");
        assert_eq!(result.imports[0].specifiers, vec!["assertEquals"]);
    }

    #[test]
    fn test_empty_file() {
        let result = parse_java("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_mixed_declarations() {
        let result = parse_java(
            r#"import java.util.List;
import java.io.IOException;

public class AppService {
    public static final String VERSION = "1.0";

    public AppService() { }

    public List<String> getData() throws IOException { return null; }

    private void helper() { }
}
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AppService"));
        assert!(names.contains(&"VERSION"));
        assert!(names.contains(&"getData"));
        assert!(names.contains(&"helper"));

        let exported_count = result.symbols.iter().filter(|s| s.is_exported).count();
        assert_eq!(exported_count, 4); // class, const, constructor, getData

        assert_eq!(result.imports.len(), 2);
    }

    #[test]
    fn test_refs_method_call_attributed() {
        let result = parse_java(
            r#"public class Svc {
    public void run() { helper(); }
    private void helper() { }
}"#,
        );
        let run_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "run")
            .expect("run method");
        assert!(
            result.references.iter().any(|r| r.name == "helper"
                && matches!(r.kind, ReferenceKind::Call)
                && r.from_symbol_idx == Some(run_idx)),
            "helper() inside run should be attributed to run, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_constructor_call() {
        let result = parse_java(
            r#"public class App {
    public void start() { Config c = new Config(); }
}
class Config { }"#,
        );
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "Config" && matches!(r.kind, ReferenceKind::Call)),
            "new Config() should emit a Call reference, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_type_in_param() {
        let result = parse_java(
            r#"public class Svc {
    public void process(Config cfg) { }
}
class Config { }"#,
        );
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "Config" && matches!(r.kind, ReferenceKind::TypeRef)),
            "Config in param type should emit TypeRef, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_builtin_types_filtered() {
        let result = parse_java(
            r#"public class Svc {
    public String process(int x) { return ""; }
}"#,
        );
        assert!(
            !result
                .references
                .iter()
                .any(|r| r.name == "int" || r.name == "String"),
            "built-in types must not be recorded as references"
        );
    }
}
