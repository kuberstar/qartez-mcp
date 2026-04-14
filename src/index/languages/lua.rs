// Rust guideline compliant 2026-04-13

use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{ExtractedImport, ExtractedSymbol, ParseResult, SymbolKind};

pub struct LuaSupport;

impl LanguageSupport for LuaSupport {
    fn extensions(&self) -> &[&str] {
        &["lua"]
    }

    fn language_name(&self) -> &str {
        "lua"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_lua::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let root = tree.root_node();
        extract_from_node(root, source, &mut symbols, &mut imports);
        ParseResult {
            symbols,
            imports,
            references: Vec::new(),
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    let mut skip_children = false;

    match node.kind() {
        "function_declaration" => {
            if let Some(sym) = extract_function_declaration(node, source) {
                symbols.push(sym);
            }
        }
        "variable_declaration" => {
            extract_variable_declaration(node, source, symbols, imports);
            skip_children = true;
        }
        "assignment_statement" => {
            if node.parent().is_some_and(|p| p.kind() == "chunk") {
                extract_top_level_assignment(node, source, symbols, imports);
                skip_children = true;
            }
        }
        "function_call" => {
            if let Some(imp) = extract_require_call(node, source) {
                imports.push(imp);
            }
        }
        _ => {}
    }

    if !skip_children {
        for child in children(node) {
            extract_from_node(child, source, symbols, imports);
        }
    }
}

fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if_statement" => 1,
        "elseif_clause" => 1,
        "for_statement" => 1,
        "while_statement" => 1,
        "repeat_statement" => 1,
        "binary_expression" => {
            let op = node
                .child_by_field_name("operator")
                .or_else(|| node.child(1))
                .map(|n| node_text(n, source));
            match op.as_deref() {
                Some("and") | Some("or") => 1,
                _ => 0,
            }
        }
        _ => 0,
    };
    for child in children(node) {
        total += count_complexity(child, source);
    }
    total
}

fn extract_function_declaration(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;

    // `local function foo()` is parsed as a `function_declaration` whose
    // first child token is the `local` keyword.
    let is_local = node
        .child(0)
        .is_some_and(|c| c.kind() == "local");

    let (name, kind, is_exported) = match name_node.kind() {
        "identifier" => {
            let name = node_text(name_node, source);
            (name, SymbolKind::Function, !is_local)
        }
        "dot_index_expression" => {
            let full = node_text(name_node, source);
            (full, SymbolKind::Method, true)
        }
        "method_index_expression" => {
            let full = node_text(name_node, source);
            (full, SymbolKind::Method, true)
        }
        _ => return None,
    };
    if name.is_empty() {
        return None;
    }
    let body_cc = node
        .child_by_field_name("body")
        .map(|body| count_complexity(body, source))
        .unwrap_or(0);
    Some(ExtractedSymbol {
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported,
        parent_idx: None,
        unused_excluded: false,
        complexity: Some(1 + body_cc),
    })
}

fn extract_variable_declaration(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    // `variable_declaration` wraps either `assignment_statement` or `variable_list`.
    // For `local x = require("mod")` the child is an `assignment_statement`.
    for child in children(node) {
        if child.kind() == "assignment_statement" {
            extract_local_assignment(child, node, source, symbols, imports);
        } else if child.kind() == "variable_list" {
            // `local x` with no assignment
            for var_child in children(child) {
                if var_child.kind() == "variable" || var_child.kind() == "identifier" {
                    let name = node_text(var_child, source);
                    if !name.is_empty() {
                        symbols.push(ExtractedSymbol {
                            name,
                            kind: SymbolKind::Variable,
                            line_start: node.start_position().row as u32 + 1,
                            line_end: node.end_position().row as u32 + 1,
                            signature: extract_signature(node, source),
                            is_exported: false,
                            parent_idx: None,
                            unused_excluded: false,
                            complexity: None,
                        });
                    }
                }
            }
        }
    }
}

fn extract_local_assignment(
    assign: Node,
    decl: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    let mut var_list = None;
    let mut expr_list = None;
    for child in children(assign) {
        match child.kind() {
            "variable_list" => var_list = Some(child),
            "expression_list" => expr_list = Some(child),
            _ => {}
        }
    }

    let var_list = match var_list {
        Some(v) => v,
        None => return,
    };

    // Check for `local function` by detecting an anonymous function_definition
    // in the RHS. Also check for `require(...)` calls.
    let is_local_func = expr_list.is_some_and(|el| {
        children(el).any(|c| c.kind() == "function_definition")
    });

    if let Some(el) = expr_list {
        for expr_child in children(el) {
            if expr_child.kind() == "function_call"
                && let Some(imp) = extract_require_call(expr_child, source) {
                    imports.push(imp);
                }
        }
    }

    for var_child in children(var_list) {
        if var_child.kind() != "variable" && var_child.kind() != "identifier" {
            continue;
        }
        let name = node_text(var_child, source);
        if name.is_empty() {
            continue;
        }
        let kind = if is_local_func {
            SymbolKind::Function
        } else {
            SymbolKind::Variable
        };
        symbols.push(ExtractedSymbol {
            name,
            kind,
            line_start: decl.start_position().row as u32 + 1,
            line_end: decl.end_position().row as u32 + 1,
            signature: extract_signature(decl, source),
            is_exported: false,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
        });
    }
}

fn extract_top_level_assignment(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    let mut var_list = None;
    let mut expr_list = None;
    for child in children(node) {
        match child.kind() {
            "variable_list" => var_list = Some(child),
            "expression_list" => expr_list = Some(child),
            _ => {}
        }
    }

    let var_list = match var_list {
        Some(v) => v,
        None => return,
    };

    // Check for require calls in the RHS
    if let Some(el) = expr_list {
        for expr_child in children(el) {
            if expr_child.kind() == "function_call"
                && let Some(imp) = extract_require_call(expr_child, source) {
                    imports.push(imp);
                }
        }
    }

    for var_child in children(var_list) {
        if var_child.kind() != "variable" && var_child.kind() != "identifier" {
            continue;
        }
        let name = node_text(var_child, source);
        if name.is_empty() {
            continue;
        }
        symbols.push(ExtractedSymbol {
            name,
            kind: SymbolKind::Variable,
            line_start: node.start_position().row as u32 + 1,
            line_end: node.end_position().row as u32 + 1,
            signature: extract_signature(node, source),
            is_exported: true,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
        });
    }
}

fn extract_require_call(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let name_node = node.child_by_field_name("name")?;
    let func_name = node_text(name_node, source);
    if func_name != "require" {
        return None;
    }
    let args_node = node.child_by_field_name("arguments")?;
    // The arguments node wraps the actual argument expressions.
    // For `require("module")`, we want the string content.
    let arg = children(args_node).find(|c| c.kind() == "string")?;
    let raw = node_text(arg, source);
    let module = unquote_lua(&raw);
    if module.is_empty() {
        return None;
    }
    Some(ExtractedImport {
        source: module,
        specifiers: vec![],
        is_reexport: false,
    })
}

fn unquote_lua(s: &str) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
        trimmed[2..trimmed.len() - 2].to_string()
    } else {
        trimmed.to_string()
    }
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
        &first_line[..200]
    } else {
        first_line
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

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_lua(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_lua::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = LuaSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_global_function() {
        let result = parse_lua("function greet(name)\n  print(name)\nend\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_local_function() {
        let result = parse_lua("local function helper(x)\n  return x + 1\nend\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "helper");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_method_dot_syntax() {
        let result = parse_lua("function Module.init(self)\n  self.x = 0\nend\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Module.init");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Method));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_method_colon_syntax() {
        let result = parse_lua("function Module:update(dt)\n  self.t = self.t + dt\nend\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Module:update");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Method));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_local_variable() {
        let result = parse_lua("local count = 0\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "count");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_require_import() {
        let result = parse_lua("local json = require(\"cjson\")\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "cjson");
    }

    #[test]
    fn test_empty_file() {
        let result = parse_lua("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_mixed_lua() {
        let result = parse_lua(
            r#"local json = require("cjson")
local utils = require("utils")

local VERSION = "1.0"

local M = {}

function M.new(opts)
  return setmetatable(opts, { __index = M })
end

function M:process(data)
  return json.decode(data)
end

local function validate(input)
  return input ~= nil
end

function global_setup()
  print("setup")
end

return M
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"VERSION"));
        assert!(names.contains(&"M"));
        assert!(names.contains(&"M.new"));
        assert!(names.contains(&"M:process"));
        assert!(names.contains(&"validate"));
        assert!(names.contains(&"global_setup"));

        assert_eq!(result.imports.len(), 2);
        assert_eq!(result.imports[0].source, "cjson");
        assert_eq!(result.imports[1].source, "utils");

        let validate_sym = result.symbols.iter().find(|s| s.name == "validate").unwrap();
        assert!(!validate_sym.is_exported);
        assert!(matches!(validate_sym.kind, SymbolKind::Function));

        let global_sym = result.symbols.iter().find(|s| s.name == "global_setup").unwrap();
        assert!(global_sym.is_exported);
        assert!(matches!(global_sym.kind, SymbolKind::Function));

        let method_sym = result.symbols.iter().find(|s| s.name == "M:process").unwrap();
        assert!(matches!(method_sym.kind, SymbolKind::Method));
        assert!(method_sym.is_exported);
    }
}
