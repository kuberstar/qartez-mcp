use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{ExtractedImport, ExtractedSymbol, ParseResult, SymbolKind};

pub struct NixSupport;

impl LanguageSupport for NixSupport {
    fn extensions(&self) -> &[&str] {
        &["nix"]
    }

    fn language_name(&self) -> &str {
        "nix"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_nix::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let root = tree.root_node();
        extract_from_node(root, source, &mut symbols, &mut imports, true);
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
        &first_line[..200]
    } else {
        first_line
    };
    Some(truncated.to_string())
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    top_level: bool,
) {
    match node.kind() {
        // NixOS module pattern: `{ config, pkgs, ... }: { ... }`
        "function_expression" if top_level => {
            extract_function_params(node, source, symbols);
        }
        // `let x = 42; in x` bindings via the binding_set child
        "let_expression" => {
            extract_binding_set(node, source, symbols, imports, false);
        }
        // `{ name = value; }` attribute set bindings via the binding_set child
        "attrset_expression" if top_level => {
            extract_binding_set(node, source, symbols, imports, true);
        }
        _ => {}
    }

    for child in children(node) {
        let child_top = top_level && matches!(node.kind(), "source_code" | "function_expression");
        extract_from_node(child, source, symbols, imports, child_top);
    }
}

/// Extract parameters from a top-level function expression.
///
/// Handles the NixOS module pattern `{ config, pkgs, lib, ... }: { ... }`
/// where the formals set defines the module's input parameters.
fn extract_function_params(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(node) {
        if child.kind() == "formals" {
            for formal in children(child) {
                if formal.kind() == "formal" {
                    let name_node = children(formal).find(|n| n.kind() == "identifier");
                    if let Some(name_node) = name_node {
                        let name = node_text(name_node, source);
                        if !name.is_empty() {
                            symbols.push(ExtractedSymbol {
                                name,
                                kind: SymbolKind::Variable,
                                line_start: formal.start_position().row as u32 + 1,
                                line_end: formal.end_position().row as u32 + 1,
                                signature: extract_signature(formal, source),
                                is_exported: false,
                                parent_idx: None,
                                unused_excluded: false,
                                complexity: None,
                                owner_type: None,
                            });
                        }
                    }
                }
            }
        }
    }
}

/// Find the `binding_set` child and extract each `binding` from it.
///
/// Both `let_expression` and `attrset_expression` use a `binding_set`
/// wrapper around their `binding` children in tree-sitter-nix.
fn extract_binding_set(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    is_exported: bool,
) {
    for child in children(node) {
        if child.kind() == "binding_set" {
            for binding in children(child) {
                if binding.kind() == "binding" {
                    extract_binding(binding, source, symbols, imports, is_exported);
                }
            }
        }
    }
}

/// Extract a single `name = value;` binding.
///
/// Determines whether the value is a function expression (producing a
/// `Function` symbol) or another value (producing a `Variable` symbol).
/// Also scans the value subtree for `import` expressions.
fn extract_binding(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    is_exported: bool,
) {
    let name = extract_binding_name(node, source);
    if name.is_empty() {
        return;
    }

    let value_node = find_binding_value(node);
    let is_function = value_node.map(|v| is_function_value(v)).unwrap_or(false);

    let kind = if is_function {
        SymbolKind::Function
    } else {
        SymbolKind::Variable
    };

    symbols.push(ExtractedSymbol {
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    });

    if let Some(value) = value_node {
        scan_for_imports(value, source, imports);
    }
}

/// Extract the name from a binding's `attrpath`.
///
/// For dotted paths like `services.nginx.enable`, returns the full
/// dot-separated path. For simple bindings like `name = value;`,
/// returns just the identifier.
fn extract_binding_name(node: Node, source: &[u8]) -> String {
    for child in children(node) {
        if child.kind() == "attrpath" {
            let parts: Vec<String> = children(child)
                .filter(|n| n.kind() == "identifier" || n.kind() == "string_expression")
                .map(|n| {
                    if n.kind() == "string_expression" {
                        extract_nix_string_content(n, source)
                    } else {
                        node_text(n, source)
                    }
                })
                .collect();
            return parts.join(".");
        }
    }
    String::new()
}

/// Extract the string content from a Nix string expression node,
/// stripping the surrounding quotes.
fn extract_nix_string_content(node: Node, source: &[u8]) -> String {
    for child in children(node) {
        if child.kind() == "string_fragment" {
            return node_text(child, source);
        }
    }
    let text = node_text(node, source);
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        text[1..text.len() - 1].to_string()
    } else {
        text
    }
}

/// Find the value expression in a binding (the part after `=`).
fn find_binding_value(node: Node) -> Option<Node> {
    let mut saw_eq = false;
    for child in children(node) {
        if child.kind() == "=" {
            saw_eq = true;
            continue;
        }
        if saw_eq && child.kind() != ";" {
            return Some(child);
        }
    }
    None
}

/// Check whether a value node represents a function definition.
///
/// A function in Nix is any expression that starts with `args:` -- either
/// a bare identifier (`x: body`) or a formals set (`{ a, b }: body`).
fn is_function_value(node: Node) -> bool {
    if node.kind() == "function_expression" {
        return true;
    }
    for child in children(node) {
        if child.kind() == "function_expression" {
            return true;
        }
    }
    false
}

/// Recursively scan a value expression for `import ./path.nix` calls.
fn scan_for_imports(node: Node, source: &[u8], imports: &mut Vec<ExtractedImport>) {
    if node.kind() == "apply_expression" {
        extract_import(node, source, imports);
    }
    for child in children(node) {
        scan_for_imports(child, source, imports);
    }
}

/// Extract an `import ./path.nix` expression as an `ExtractedImport`.
///
/// In tree-sitter-nix, `import` is parsed as an `apply_expression` whose
/// function child is a `variable_expression` containing an `identifier`
/// with text "import", and whose argument is a `path_expression`.
fn extract_import(node: Node, source: &[u8], imports: &mut Vec<ExtractedImport>) {
    let mut child_iter = children(node);
    let func = match child_iter.next() {
        Some(n) => n,
        None => return,
    };

    let is_import = if func.kind() == "variable_expression" {
        children(func).any(|n| n.kind() == "identifier" && node_text(n, source) == "import")
    } else {
        false
    };

    if is_import && let Some(arg) = child_iter.next() {
        let path = if arg.kind() == "path_expression" {
            children(arg)
                .find(|n| n.kind() == "path_fragment")
                .map(|n| node_text(n, source))
                .unwrap_or_default()
        } else {
            node_text(arg, source)
        };
        if !path.is_empty() && (path.starts_with("./") || path.starts_with("../")) {
            imports.push(ExtractedImport {
                source: path,
                specifiers: vec![],
                is_reexport: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_nix(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_nix::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = NixSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_attribute_binding() {
        let result = parse_nix("{ name = \"hello\"; version = \"1.0\"; }");
        assert_eq!(result.symbols.len(), 2);
        assert_eq!(result.symbols[0].name, "name");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(result.symbols[0].is_exported);
        assert_eq!(result.symbols[1].name, "version");
    }

    #[test]
    fn test_function_definition() {
        let result = parse_nix("{ greet = name: \"hello ${name}\"; }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_let_binding() {
        let result = parse_nix(
            r#"let
  x = 42;
  y = "hello";
in x"#,
        );
        assert_eq!(result.symbols.len(), 2);
        assert_eq!(result.symbols[0].name, "x");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(!result.symbols[0].is_exported);
        assert_eq!(result.symbols[1].name, "y");
    }

    #[test]
    fn test_import_path() {
        let result = parse_nix("{ utils = import ./utils.nix; }");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "./utils.nix");
    }

    #[test]
    fn test_nested_attrset() {
        let result = parse_nix(
            r#"{
  meta = {
    description = "A package";
    license = "MIT";
  };
}"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"meta"));
    }

    #[test]
    fn test_empty_file() {
        let result = parse_nix("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_mixed_nix() {
        let result = parse_nix(
            r#"{
  pname = "mypackage";
  version = "1.0.0";

  src = import ./src.nix;

  buildInputs = [ ];

  buildPhase = ''
    make build
  '';

  installPhase = ''
    make install PREFIX=$out
  '';

  meta = {
    description = "My package";
  };
}"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"pname"));
        assert!(names.contains(&"version"));
        assert!(names.contains(&"src"));
        assert!(names.contains(&"buildInputs"));
        assert!(names.contains(&"buildPhase"));
        assert!(names.contains(&"installPhase"));
        assert!(names.contains(&"meta"));

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "./src.nix");
    }

    #[test]
    fn test_nixos_module() {
        let result = parse_nix(
            r#"{ config, pkgs, lib, ... }:

{
  environment.systemPackages = [ pkgs.vim ];
  services.nginx.enable = true;
}"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"config"));
        assert!(names.contains(&"pkgs"));
        assert!(names.contains(&"lib"));

        let param_syms: Vec<&ExtractedSymbol> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Variable) && !s.is_exported)
            .collect();
        assert!(param_syms.len() >= 3);
    }
}
