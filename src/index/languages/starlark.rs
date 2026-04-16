use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{ExtractedImport, ExtractedSymbol, ParseResult, SymbolKind};
use crate::str_utils::floor_char_boundary;

/// Starlark/BUILD file parser for Bazel and similar build systems.
///
/// Starlark is a Python subset used by Bazel, Buck, and other build tools.
/// This parser reuses tree-sitter-python since Starlark shares Python syntax.
pub struct StarlarkSupport;

impl LanguageSupport for StarlarkSupport {
    fn extensions(&self) -> &[&str] {
        &["bzl", "star", "bazel"]
    }

    fn filenames(&self) -> &[&str] {
        &["BUILD", "BUILD.bazel", "WORKSPACE", "WORKSPACE.bazel"]
    }

    fn language_name(&self) -> &str {
        "starlark"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_python::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let root = tree.root_node();

        for child in children(root) {
            match child.kind() {
                "function_definition" => {
                    extract_function(child, source, &mut symbols);
                }
                "expression_statement" => {
                    extract_expression_statement(child, source, &mut symbols, &mut imports);
                }
                _ => {}
            }
        }

        ParseResult {
            symbols,
            imports,
            references: Vec::new(),
            ..Default::default()
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn node_text(node: Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    std::str::from_utf8(&source[start..end])
        .unwrap_or("")
        .to_string()
}

fn extract_signature(node: Node, source: &[u8]) -> Option<String> {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    let text = std::str::from_utf8(&source[start..end]).ok()?;
    let first_line = text.lines().next().unwrap_or(text).trim();
    if first_line.is_empty() {
        return None;
    }
    let truncated = if first_line.len() > 200 {
        &first_line[..floor_char_boundary(first_line, 200)]
    } else {
        first_line
    };
    Some(truncated.to_string())
}

fn extract_function(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, source);
    if name.is_empty() {
        return;
    }
    symbols.push(ExtractedSymbol {
        is_exported: !name.starts_with('_'),
        name,
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    });
}

fn extract_expression_statement(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    for child in children(node) {
        match child.kind() {
            "assignment" => {
                extract_assignment(child, source, symbols);
            }
            "call" => {
                extract_call(child, source, symbols, imports);
            }
            _ => {}
        }
    }
}

fn extract_assignment(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let left = match node.child_by_field_name("left") {
        Some(n) => n,
        None => return,
    };
    if left.kind() != "identifier" {
        return;
    }
    let name = node_text(left, source);
    if name.is_empty() {
        return;
    }
    symbols.push(ExtractedSymbol {
        is_exported: true,
        name,
        kind: SymbolKind::Variable,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    });
}

fn extract_call(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    let func = match node.child_by_field_name("function") {
        Some(n) => n,
        None => return,
    };
    let func_name = node_text(func, source);

    if func_name == "load" {
        extract_load(node, source, imports);
        return;
    }

    // Any call with a `name = "..."` keyword argument is a build rule target
    if let Some(target_name) = extract_name_kwarg(node, source) {
        symbols.push(ExtractedSymbol {
            is_exported: true,
            name: target_name,
            kind: SymbolKind::Target,
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

fn extract_load(node: Node, source: &[u8], imports: &mut Vec<ExtractedImport>) {
    let args = match node.child_by_field_name("arguments") {
        Some(n) => n,
        None => return,
    };

    let mut source_path = String::new();
    let mut specifiers = Vec::new();
    let mut first = true;

    for child in children(args) {
        if child.kind() == "string" {
            let text = strip_quotes(&node_text(child, source));
            if first {
                source_path = text;
                first = false;
            } else {
                specifiers.push(text);
            }
        }
    }

    if !source_path.is_empty() {
        imports.push(ExtractedImport {
            source: source_path,
            specifiers,
            is_reexport: false,
        });
    }
}

fn extract_name_kwarg(call_node: Node, source: &[u8]) -> Option<String> {
    let args = call_node.child_by_field_name("arguments")?;
    for child in children(args) {
        if child.kind() == "keyword_argument" {
            let key = child.child_by_field_name("name")?;
            if node_text(key, source) == "name" {
                let value = child.child_by_field_name("value")?;
                if value.kind() == "string" {
                    return Some(strip_quotes(&node_text(value, source)));
                }
            }
        }
    }
    None
}

fn strip_quotes(s: &str) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

// Rust guideline compliant 2026-04-13

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_starlark(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_python::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = StarlarkSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_function_definition() {
        let result = parse_starlark("def my_rule(ctx):\n    return ctx.attr.name\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "my_rule");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
        assert!(result.symbols[0].signature.is_some());
    }

    #[test]
    fn test_private_function() {
        let result = parse_starlark("def _impl(ctx):\n    pass\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "_impl");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_build_rule() {
        let result = parse_starlark(
            r#"cc_library(
    name = "mylib",
    srcs = ["mylib.cc"],
    hdrs = ["mylib.h"],
)
"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "mylib");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Target));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_load_statement() {
        let result = parse_starlark(r#"load("@rules_cc//cc:defs.bzl", "cc_library", "cc_binary")"#);
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "@rules_cc//cc:defs.bzl");
        assert_eq!(result.imports[0].specifiers.len(), 2);
        assert!(
            result.imports[0]
                .specifiers
                .contains(&"cc_library".to_string())
        );
        assert!(
            result.imports[0]
                .specifiers
                .contains(&"cc_binary".to_string())
        );
    }

    #[test]
    fn test_variable_assignment() {
        let result = parse_starlark("COPTS = [\"-Wall\", \"-Werror\"]\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "COPTS");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_empty_file() {
        let result = parse_starlark("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
        assert!(result.references.is_empty());
    }

    #[test]
    fn test_mixed_build_file() {
        let result = parse_starlark(
            r#"load("@rules_cc//cc:defs.bzl", "cc_library", "cc_binary")

COPTS = ["-Wall"]

def _check_deps(deps):
    return len(deps) > 0

cc_library(
    name = "mylib",
    srcs = ["mylib.cc"],
    hdrs = ["mylib.h"],
    copts = COPTS,
)

cc_binary(
    name = "myapp",
    srcs = ["main.cc"],
    deps = [":mylib"],
)
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"COPTS"));
        assert!(names.contains(&"_check_deps"));
        assert!(names.contains(&"mylib"));
        assert!(names.contains(&"myapp"));

        let private_fn = result
            .symbols
            .iter()
            .find(|s| s.name == "_check_deps")
            .unwrap();
        assert!(!private_fn.is_exported);

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "@rules_cc//cc:defs.bzl");
    }

    #[test]
    fn test_multiple_rules() {
        let result = parse_starlark(
            r#"java_library(
    name = "core",
    srcs = glob(["src/**/*.java"]),
)

java_test(
    name = "core_test",
    srcs = ["CoreTest.java"],
    deps = [":core"],
)

py_library(
    name = "utils",
    srcs = ["utils.py"],
)
"#,
        );
        assert_eq!(result.symbols.len(), 3);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"core"));
        assert!(names.contains(&"core_test"));
        assert!(names.contains(&"utils"));
        assert!(
            result
                .symbols
                .iter()
                .all(|s| matches!(s.kind, SymbolKind::Target))
        );
    }
}
