use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct KotlinSupport;

impl LanguageSupport for KotlinSupport {
    fn extensions(&self) -> &[&str] {
        &["kt", "kts"]
    }

    fn language_name(&self) -> &str {
        "kotlin"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_kotlin_ng::LANGUAGE)
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
        child.kind() == "modifiers"
            && children(child).any(|m| {
                let text = node_text(m, source);
                text == modifier
            })
    })
}

fn is_private_or_internal(node: Node, source: &[u8]) -> bool {
    has_modifier(node, source, "private") || has_modifier(node, source, "internal")
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
            let is_enum = has_modifier(node, source, "enum");
            let kind = if is_enum {
                SymbolKind::Enum
            } else {
                SymbolKind::Class
            };
            if let Some(sym) = extract_named_decl(node, source, kind) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
            extract_class_body(node, source, new_enclosing, symbols, references);
            return;
        }
        "object_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Class) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
            extract_class_body(node, source, new_enclosing, symbols, references);
            return;
        }
        "function_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Function) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
        }
        "property_declaration" => {
            if let Some(sym) = extract_property(node, source) {
                symbols.push(sym);
            }
        }
        "import_header" | "import" => {
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

fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if_expression" => 1,
        "when_entry" => 1,
        "for_statement" => 1,
        "while_statement" => 1,
        "do_while_statement" => 1,
        "catch_block" => 1,
        "conjunction_expression" => 1,
        "disjunction_expression" => 1,
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
        is_exported: !is_private_or_internal(node, source),
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity,
    })
}

#[expect(clippy::only_used_in_recursion, reason = "enclosing is the fallback for unnamed nested declarations")]
fn extract_class_body(
    class_node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    let body =
        children(class_node).find(|c| c.kind() == "class_body" || c.kind() == "enum_class_body");
    let body = match body {
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
                    walk_body_references(child, source, Some(idx), references);
                }
            }
            "property_declaration" => {
                if let Some(sym) = extract_property(child, source) {
                    symbols.push(sym);
                }
            }
            "class_declaration" => {
                let is_enum = has_modifier(child, source, "enum");
                let kind = if is_enum {
                    SymbolKind::Enum
                } else {
                    SymbolKind::Class
                };
                if let Some(sym) = extract_named_decl(child, source, kind) {
                    let idx = symbols.len();
                    symbols.push(sym);
                    extract_class_body(child, source, Some(idx), symbols, references);
                } else {
                    extract_class_body(child, source, enclosing, symbols, references);
                }
            }
            "object_declaration" => {
                if let Some(sym) = extract_named_decl(child, source, SymbolKind::Class) {
                    let idx = symbols.len();
                    symbols.push(sym);
                    extract_class_body(child, source, Some(idx), symbols, references);
                } else {
                    extract_class_body(child, source, enclosing, symbols, references);
                }
            }
            "companion_object" => {
                extract_class_body(child, source, enclosing, symbols, references);
            }
            _ => {}
        }
    }
}

fn extract_property(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name = children(node)
        .find(|c| c.kind() == "variable_declaration")
        .and_then(|vd| {
            children(vd)
                .find(|c| c.kind() == "simple_identifier" || c.kind() == "identifier")
                .map(|n| node_text(n, source))
        })?;

    if name.is_empty() {
        return None;
    }

    let is_const = has_modifier(node, source, "const");
    let kind = if is_const {
        SymbolKind::Const
    } else {
        SymbolKind::Variable
    };

    Some(ExtractedSymbol {
        is_exported: !is_private_or_internal(node, source),
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

fn record_reference(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    let line = node.start_position().row as u32 + 1;
    match node.kind() {
        "call_expression" => {
            // `foo()` or `bar(x, y)` — the first child is the callee, the
            // second is `call_suffix` containing the argument list.
            if let Some(callee) = node.child(0) {
                let name = extract_callee_name(callee, source);
                if !name.is_empty() && !is_builtin_callable(&name) {
                    references.push(ExtractedReference {
                        name,
                        line,
                        from_symbol_idx: enclosing,
                        kind: ReferenceKind::Call,
                    });
                }
            }
        }
        "navigation_expression" => {
            // `obj.method` — only record when the parent is NOT a
            // `call_expression`; the call_expression arm already handles
            // `obj.method()`.
            let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
            if parent_kind != "call_expression"
                && let Some(prop) = node
                    .child_by_field_name("suffix")
                    .or_else(|| node.child(node.child_count().saturating_sub(1) as u32))
                && matches!(prop.kind(), "simple_identifier" | "identifier")
            {
                let name = node_text(prop, source);
                if !name.is_empty() && !is_builtin_callable(&name) {
                    references.push(ExtractedReference {
                        name,
                        line,
                        from_symbol_idx: enclosing,
                        kind: ReferenceKind::Use,
                    });
                }
            }
        }
        "user_type" | "type_identifier" => {
            let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
            if matches!(
                parent_kind,
                "class_declaration" | "object_declaration"
            ) {
                return;
            }
            // user_type wraps an identifier child in tree-sitter-kotlin-ng
            let name = if node.kind() == "user_type" {
                children(node)
                    .find(|c| {
                        matches!(
                            c.kind(),
                            "simple_identifier" | "type_identifier" | "identifier"
                        )
                    })
                    .map(|n| node_text(n, source))
                    .unwrap_or_default()
            } else {
                node_text(node, source)
            };
            if !name.is_empty() && !is_builtin_type(&name) {
                references.push(ExtractedReference {
                    name,
                    line,
                    from_symbol_idx: enclosing,
                    kind: ReferenceKind::TypeRef,
                });
            }
        }
        _ => {}
    }
}

fn extract_callee_name(node: Node, source: &[u8]) -> String {
    match node.kind() {
        "simple_identifier" | "identifier" => node_text(node, source),
        "navigation_expression" => {
            // `obj.method` — use the rightmost identifier as the callee name
            node.child(node.child_count().saturating_sub(1) as u32)
                .filter(|n| matches!(n.kind(), "simple_identifier" | "identifier"))
                .map(|n| node_text(n, source))
                .unwrap_or_default()
        }
        _ => String::new(),
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

fn is_builtin_callable(name: &str) -> bool {
    matches!(
        name,
        "println"
            | "print"
            | "listOf"
            | "mapOf"
            | "setOf"
            | "arrayOf"
            | "mutableListOf"
            | "mutableMapOf"
            | "mutableSetOf"
            | "require"
            | "check"
            | "error"
            | "TODO"
            | "lazy"
            | "run"
            | "let"
            | "also"
            | "apply"
            | "with"
            | "repeat"
            | "buildString"
    )
}

fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "Int"
            | "Long"
            | "Short"
            | "Byte"
            | "Float"
            | "Double"
            | "Boolean"
            | "Char"
            | "String"
            | "Unit"
            | "Nothing"
            | "Any"
            | "Array"
            | "List"
            | "Map"
            | "Set"
            | "MutableList"
            | "MutableMap"
            | "MutableSet"
            | "Pair"
            | "Triple"
            | "Sequence"
            | "Comparable"
            | "Iterable"
    )
}

fn extract_import(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let full = node_text(node, source);
    let trimmed = full.trim();
    let path = trimmed.strip_prefix("import")?.trim();

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

    fn parse_kotlin(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_kotlin_ng::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = KotlinSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_class() {
        let result = parse_kotlin("class MyService { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MyService");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_private_class() {
        let result = parse_kotlin("private class Helper { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Helper");
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_internal_class() {
        let result = parse_kotlin("internal class Helper { }");
        assert_eq!(result.symbols.len(), 1);
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_data_class() {
        let result = parse_kotlin("data class User(val name: String, val age: Int)");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "User");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_object() {
        let result = parse_kotlin("object Singleton { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Singleton");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_function() {
        let result = parse_kotlin("fun greet(name: String): String { return \"Hello $name\" }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_private_function() {
        let result = parse_kotlin("private fun helper(): Int { return 0 }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "helper");
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_method_in_class() {
        let result = parse_kotlin(
            "class Foo {\n    fun run() { }\n    private fun count(): Int { return 0 }\n}",
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
    fn test_import() {
        let result = parse_kotlin("import kotlin.collections.List");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "kotlin.collections");
        assert_eq!(result.imports[0].specifiers, vec!["List"]);
    }

    #[test]
    fn test_wildcard_import() {
        let result = parse_kotlin("import kotlin.collections.*");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "kotlin.collections");
        assert_eq!(result.imports[0].specifiers, vec!["*"]);
    }

    #[test]
    fn test_empty_file() {
        let result = parse_kotlin("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_enum_class() {
        let result = parse_kotlin("enum class Status { ACTIVE, INACTIVE }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Status");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Enum));
    }

    #[test]
    fn test_mixed_declarations() {
        let result = parse_kotlin(
            r#"import kotlin.collections.List

class AppService {
    fun getData(): List<String> { return emptyList() }
    private fun helper() { }
}

fun createService(): AppService { return AppService() }
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AppService"));
        assert!(names.contains(&"getData"));
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"createService"));

        assert_eq!(result.imports.len(), 1);
    }

    #[test]
    fn test_ref_call_attributed_to_function() {
        let result = parse_kotlin(
            r#"
fun process() {
    val svc = ServiceFactory.create()
    svc.execute()
}
"#,
        );
        let calls: Vec<_> = result
            .references
            .iter()
            .filter(|r| matches!(r.kind, ReferenceKind::Call))
            .collect();
        assert!(
            calls.iter().any(|r| r.name == "create"),
            "expected call ref to 'create', got: {calls:?}"
        );
        let process_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "process")
            .expect("process symbol");
        for call in &calls {
            assert_eq!(
                call.from_symbol_idx,
                Some(process_idx),
                "call '{}' should be attributed to process",
                call.name
            );
        }
    }

    #[test]
    fn test_ref_type_ref() {
        let result = parse_kotlin(
            r#"
fun build(): Config {
    val cfg: AppConfig = AppConfig()
    return cfg
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
            names.contains(&"Config") || names.contains(&"AppConfig"),
            "expected type ref to Config or AppConfig, got: {names:?}"
        );
    }

    #[test]
    fn test_ref_builtin_filtered() {
        let result = parse_kotlin(
            r#"
fun demo() {
    println("hello")
    val items = listOf(1, 2, 3)
    val map = mapOf("a" to 1)
    val x: Int = 42
    val s: String = "hi"
}
"#,
        );
        let ref_names: Vec<&str> = result.references.iter().map(|r| r.name.as_str()).collect();
        assert!(
            !ref_names.contains(&"println"),
            "println should be filtered"
        );
        assert!(
            !ref_names.contains(&"listOf"),
            "listOf should be filtered"
        );
        assert!(!ref_names.contains(&"Int"), "Int should be filtered");
        assert!(!ref_names.contains(&"String"), "String should be filtered");
    }

    #[test]
    fn test_ref_method_in_class_body() {
        let result = parse_kotlin(
            r#"
class Processor {
    fun run() {
        val db = Database.open()
        db.query("SELECT 1")
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
            calls.iter().any(|r| r.name == "open"),
            "expected call ref to 'open', got: {calls:?}"
        );
        let run_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "run")
            .expect("run symbol");
        for call in &calls {
            assert_eq!(
                call.from_symbol_idx,
                Some(run_idx),
                "call '{}' should be attributed to run",
                call.name
            );
        }
    }
}
