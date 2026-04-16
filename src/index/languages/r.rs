use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};
use crate::str_utils::floor_char_boundary;

pub struct RSupport;

impl LanguageSupport for RSupport {
    fn extensions(&self) -> &[&str] {
        &["r", "R"]
    }

    fn language_name(&self) -> &str {
        "r"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_r::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let mut references = Vec::new();
        let root = tree.root_node();
        for child in children(root) {
            visit_top_level(child, source, &mut symbols, &mut imports, &mut references);
        }
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

fn visit_top_level(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    match node.kind() {
        "binary_operator" => {
            let op = operator_text(node, source);
            let right_assign = matches!(op.as_str(), "->" | "->>");
            let assign = matches!(op.as_str(), "<-" | "<<-" | "=" | "->" | "->>");
            if assign {
                let (target_idx, value_node) =
                    extract_assignment(node, source, symbols, right_assign);
                if let Some(value) = value_node {
                    walk_body_references(value, source, target_idx, references);
                }
                return;
            }
        }
        "call" => {
            if let Some(imp) = extract_library_call(node, source) {
                imports.push(imp);
            }
            if let Some(sym) = extract_s4_registration(node, source) {
                symbols.push(sym);
                return;
            }
        }
        _ => {}
    }

    walk_body_references(node, source, None, references);
}

fn operator_text(node: Node, source: &[u8]) -> String {
    node.child_by_field_name("operator")
        .map(|n| node_text(n, source))
        .unwrap_or_default()
}

fn extract_assignment<'a>(
    node: Node<'a>,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    right_assign: bool,
) -> (Option<usize>, Option<Node<'a>>) {
    let name_node = if right_assign {
        node.child_by_field_name("rhs")
    } else {
        node.child_by_field_name("lhs")
    };
    let value_node = if right_assign {
        node.child_by_field_name("lhs")
    } else {
        node.child_by_field_name("rhs")
    };
    let name_node = match name_node {
        Some(n) => n,
        None => return (None, value_node),
    };
    let value = match value_node {
        Some(v) => v,
        None => return (None, None),
    };

    if name_node.kind() != "identifier" {
        return (None, Some(value));
    }
    let name = node_text(name_node, source);
    if name.is_empty() {
        return (None, Some(value));
    }

    let is_function = value.kind() == "function_definition";
    let kind = if is_function {
        SymbolKind::Function
    } else if is_r6_class_call(value, source) {
        SymbolKind::Class
    } else {
        SymbolKind::Variable
    };

    let complexity = if is_function {
        let body_cc = value
            .child_by_field_name("body")
            .map(|b| count_complexity(b, source))
            .unwrap_or(0);
        Some(1 + body_cc)
    } else {
        None
    };

    let idx = symbols.len();
    symbols.push(ExtractedSymbol {
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity,
        owner_type: None,
    });
    (Some(idx), Some(value))
}

fn is_r6_class_call(node: Node, source: &[u8]) -> bool {
    if node.kind() != "call" {
        return false;
    }
    let func = match node.child_by_field_name("function") {
        Some(f) => f,
        None => return false,
    };
    let name = node_text(func, source);
    name == "R6Class" || name.ends_with("::R6Class")
}

fn extract_s4_registration(call: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let func = call.child_by_field_name("function")?;
    let func_name = node_text(func, source);
    let kind = match func_name.as_str() {
        "setClass" => SymbolKind::Class,
        "setMethod" | "setGeneric" => SymbolKind::Method,
        _ => return None,
    };
    let name = first_string_argument(call, source)?;
    Some(ExtractedSymbol {
        name,
        kind,
        line_start: call.start_position().row as u32 + 1,
        line_end: call.end_position().row as u32 + 1,
        signature: extract_signature(call, source),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn first_string_argument(call: Node, source: &[u8]) -> Option<String> {
    let args = call.child_by_field_name("arguments")?;
    for arg in children(args) {
        if arg.kind() != "argument" {
            continue;
        }
        let value = arg.child_by_field_name("value")?;
        if value.kind() == "string" {
            return Some(extract_string_content(value, source));
        }
        return None;
    }
    None
}

fn first_name_argument(call: Node, source: &[u8]) -> Option<String> {
    let args = call.child_by_field_name("arguments")?;
    for arg in children(args) {
        if arg.kind() != "argument" {
            continue;
        }
        let value = arg.child_by_field_name("value")?;
        match value.kind() {
            "identifier" => return Some(node_text(value, source)),
            "string" => return Some(extract_string_content(value, source)),
            _ => return None,
        }
    }
    None
}

fn extract_string_content(node: Node, source: &[u8]) -> String {
    for child in children(node) {
        if child.kind() == "string_content" {
            return node_text(child, source);
        }
    }
    let raw = node_text(node, source);
    if raw.len() >= 2
        && ((raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\'')))
    {
        raw[1..raw.len() - 1].to_string()
    } else {
        raw
    }
}

fn extract_library_call(call: Node, source: &[u8]) -> Option<ExtractedImport> {
    let func = call.child_by_field_name("function")?;
    let name = node_text(func, source);
    match name.as_str() {
        "library" | "require" | "requireNamespace" | "loadNamespace" => {
            let pkg = first_name_argument(call, source)?;
            if pkg.is_empty() {
                return None;
            }
            Some(ExtractedImport {
                source: pkg,
                specifiers: vec![],
                is_reexport: false,
            })
        }
        "source" | "sys.source" => {
            let path = first_string_argument(call, source)?;
            if path.is_empty() {
                return None;
            }
            Some(ExtractedImport {
                source: path,
                specifiers: vec![],
                is_reexport: false,
            })
        }
        _ => None,
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
        "call" => {
            let func = match node.child_by_field_name("function") {
                Some(f) => f,
                None => return,
            };
            let (name, qualifier) = match func.kind() {
                "identifier" => (node_text(func, source), None),
                "namespace_operator" => {
                    let rhs = func
                        .child_by_field_name("rhs")
                        .map(|n| node_text(n, source))
                        .unwrap_or_default();
                    let lhs = func
                        .child_by_field_name("lhs")
                        .map(|n| node_text(n, source));
                    (rhs, lhs)
                }
                _ => return,
            };
            if name.is_empty() || is_builtin(&name) {
                return;
            }
            references.push(ExtractedReference {
                name,
                line,
                from_symbol_idx: enclosing,
                kind: ReferenceKind::Call,
                qualifier,
                receiver_type_hint: None,
            });
        }
        "namespace_operator" => {
            if node.parent().map(|p| p.kind()) == Some("call") {
                return;
            }
            let rhs = match node.child_by_field_name("rhs") {
                Some(n) => n,
                None => return,
            };
            let name = node_text(rhs, source);
            if name.is_empty() {
                return;
            }
            let qualifier = node
                .child_by_field_name("lhs")
                .map(|n| node_text(n, source));
            references.push(ExtractedReference {
                name,
                line,
                from_symbol_idx: enclosing,
                kind: ReferenceKind::Use,
                qualifier,
                receiver_type_hint: None,
            });
        }
        _ => {}
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

fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if_statement" => 1,
        "for_statement" | "while_statement" | "repeat_statement" => 1,
        "binary_operator" => {
            let op = operator_text(node, source);
            match op.as_str() {
                "&&" | "||" | "&" | "|" => 1,
                _ => 0,
            }
        }
        "function_definition" => return 0,
        _ => 0,
    };
    for child in children(node) {
        total += count_complexity(child, source);
    }
    total
}

fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "c" | "list"
            | "vector"
            | "print"
            | "cat"
            | "paste"
            | "paste0"
            | "sprintf"
            | "format"
            | "length"
            | "nrow"
            | "ncol"
            | "dim"
            | "names"
            | "return"
            | "stop"
            | "warning"
            | "message"
            | "invisible"
            | "seq"
            | "seq_len"
            | "seq_along"
            | "rep"
            | "sum"
            | "mean"
            | "min"
            | "max"
            | "is.null"
            | "is.na"
            | "library"
            | "require"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_r(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_r::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = RSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_empty_file() {
        let result = parse_r("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_function_assignment() {
        let result = parse_r("greet <- function(name) {\n  paste(\"hello\", name)\n}\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_equals_function_assignment() {
        let result = parse_r("greet = function(name) name\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_right_assignment() {
        let result = parse_r("42 -> answer\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "answer");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
    }

    #[test]
    fn test_variable_assignment() {
        let result = parse_r("x <- 42\ny <- \"hello\"\n");
        assert_eq!(result.symbols.len(), 2);
        assert_eq!(result.symbols[0].name, "x");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(result.symbols[0].is_exported);
        assert_eq!(result.symbols[1].name, "y");
    }

    #[test]
    fn test_library_import() {
        let result = parse_r("library(dplyr)\nrequire(ggplot2)\n");
        assert_eq!(result.imports.len(), 2);
        assert_eq!(result.imports[0].source, "dplyr");
        assert_eq!(result.imports[1].source, "ggplot2");
    }

    #[test]
    fn test_source_import() {
        let result = parse_r("source(\"utils.R\")\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "utils.R");
    }

    #[test]
    fn test_s4_class() {
        let result = parse_r("setClass(\"Person\", representation(name=\"character\"))\n");
        let classes: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Person");
    }

    #[test]
    fn test_s4_method() {
        let result =
            parse_r("setMethod(\"show\", \"Person\", function(object) cat(object@name))\n");
        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "show");
    }

    #[test]
    fn test_r6_class() {
        let result = parse_r("Counter <- R6Class(\"Counter\", public = list())\n");
        let classes: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Counter");
    }

    #[test]
    fn test_qualified_call() {
        let result = parse_r("greet <- function() dplyr::mutate(x, y = 1)\n");
        let refs: Vec<_> = result
            .references
            .iter()
            .filter(|r| r.name == "mutate")
            .collect();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].qualifier.as_deref(), Some("dplyr"));
    }

    #[test]
    fn test_mixed_r_file() {
        let result = parse_r(
            r#"library(dplyr)
library(ggplot2)

MAX_RETRIES <- 3

calculate_mean <- function(x) {
  if (length(x) == 0) {
    return(NA)
  }
  sum(x) / length(x)
}

process_data <- function(df) {
  dplyr::filter(df, value > MAX_RETRIES)
}

setClass("Counter", representation(count = "numeric"))
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"MAX_RETRIES"));
        assert!(names.contains(&"calculate_mean"));
        assert!(names.contains(&"process_data"));
        assert!(names.contains(&"Counter"));

        assert_eq!(result.imports.len(), 2);
        let sources: Vec<&str> = result.imports.iter().map(|i| i.source.as_str()).collect();
        assert!(sources.contains(&"dplyr"));
        assert!(sources.contains(&"ggplot2"));

        let calculate_mean = result
            .symbols
            .iter()
            .find(|s| s.name == "calculate_mean")
            .unwrap();
        assert!(matches!(calculate_mean.kind, SymbolKind::Function));
        assert!(calculate_mean.complexity.is_some());
        assert!(calculate_mean.complexity.unwrap() >= 2);
    }
}
