use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{ExtractedImport, ExtractedSymbol, ParseResult, SymbolKind};

/// Tree-sitter parser for Elixir source files (.ex, .exs).
pub struct ElixirSupport;

impl LanguageSupport for ElixirSupport {
    fn extensions(&self) -> &[&str] {
        &["ex", "exs"]
    }

    fn language_name(&self) -> &str {
        "elixir"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_elixir::LANGUAGE)
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
            ..Default::default()
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
    match node.kind() {
        "call" => {
            let callee = call_target_name(node, source);
            match callee.as_str() {
                "defmodule" => {
                    if let Some(sym) = extract_defmodule(node, source) {
                        symbols.push(sym);
                    }
                }
                "def" | "defmacro" => {
                    if let Some(sym) = extract_def(node, source, true) {
                        symbols.push(sym);
                    }
                }
                "defp" | "defmacrop" => {
                    if let Some(sym) = extract_def(node, source, false) {
                        symbols.push(sym);
                    }
                }
                "defstruct" => {
                    if let Some(sym) = extract_defstruct(node, source) {
                        symbols.push(sym);
                    }
                }
                "alias" | "import" | "use" | "require" => {
                    if let Some(imp) = extract_import(node, source) {
                        imports.push(imp);
                    }
                }
                _ => {}
            }
        }
        "unary_operator" => {
            if let Some(sym) = extract_module_attribute(node, source) {
                symbols.push(sym);
            }
        }
        _ => {}
    }

    for child in children(node) {
        extract_from_node(child, source, symbols, imports);
    }
}

fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "call" => {
            let callee = node
                .child_by_field_name("target")
                .map(|t| node_text(t, source))
                .unwrap_or_default();
            match callee.as_str() {
                "if" | "cond" | "with" => 1,
                "case" => 1,
                _ => 0,
            }
        }
        "binary_operator" => {
            let op = node
                .child_by_field_name("operator")
                .map(|n| node_text(n, source));
            match op.as_deref() {
                Some("and") | Some("or") | Some("&&") | Some("||") => 1,
                _ => 0,
            }
        }
        "anonymous_function" => return 0,
        "stab_clause" => 1,
        _ => 0,
    };
    for child in children(node) {
        total += count_complexity(child, source);
    }
    total
}

/// Returns the callee name for a `call` node by reading its `target` field.
fn call_target_name(node: Node, source: &[u8]) -> String {
    node.child_by_field_name("target")
        .map(|t| node_text(t, source))
        .unwrap_or_default()
}

/// Extracts a `defmodule` declaration. The module name is the first
/// argument, typically an `alias` node (e.g. `MyApp.MyModule`).
fn extract_defmodule(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name = first_argument_text(node, source)?;
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Module,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

/// Extracts `def`, `defp`, `defmacro`, or `defmacrop`. The function name
/// is the target of the nested `call` node inside `arguments`, or a plain
/// `identifier` for zero-arity functions written without parentheses.
fn extract_def(node: Node, source: &[u8], is_exported: bool) -> Option<ExtractedSymbol> {
    let args_node = children(node).find(|c| c.kind() == "arguments")?;
    let first_arg = children(args_node).next()?;

    let name = match first_arg.kind() {
        "call" => {
            // `def greet(name)` -- the function head is itself a call node
            call_target_name(first_arg, source)
        }
        "identifier" => {
            // `def run do ... end` -- zero-arity without parens
            node_text(first_arg, source)
        }
        "binary_operator" => {
            // `def greet(name) when is_binary(name)` -- guard clause wraps
            // the call in a `when` binary operator
            let left = first_arg.child_by_field_name("left")?;
            match left.kind() {
                "call" => call_target_name(left, source),
                "identifier" => node_text(left, source),
                _ => return None,
            }
        }
        _ => return None,
    };

    if name.is_empty() {
        return None;
    }

    let body_cc = count_complexity(node, source);
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported,
        parent_idx: None,
        unused_excluded: false,
        complexity: Some(1 + body_cc),
        owner_type: None,
    })
}

/// Extracts a `defstruct` declaration. The symbol name is set to
/// `__struct__` since the struct itself is anonymous and derives its
/// name from the enclosing module.
fn extract_defstruct(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    Some(ExtractedSymbol {
        name: "__struct__".to_string(),
        kind: SymbolKind::Struct,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

/// Extracts `alias`, `import`, `use`, or `require` as an import. The
/// module path is the first argument (an `alias` node or dotted path).
fn extract_import(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let path = first_argument_text(node, source)?;
    if path.is_empty() {
        return None;
    }
    Some(ExtractedImport {
        source: path,
        specifiers: vec![],
        is_reexport: false,
    })
}

/// Extracts a module attribute (`@attr value`). In tree-sitter-elixir
/// these are `unary_operator` nodes with `@` as the operator. Skips
/// documentation and typespec attributes.
fn extract_module_attribute(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let op_node = node.child_by_field_name("operator")?;
    if node_text(op_node, source) != "@" {
        return None;
    }

    let operand = node.child_by_field_name("operand")?;
    let name = match operand.kind() {
        "call" => call_target_name(operand, source),
        "identifier" => node_text(operand, source),
        _ => return None,
    };

    if name.is_empty() || is_skipped_attribute(&name) {
        return None;
    }

    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Variable,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

/// Returns true for attribute names that should not be indexed as
/// symbols (documentation, typespecs, callbacks, etc.).
fn is_skipped_attribute(name: &str) -> bool {
    matches!(
        name,
        "moduledoc"
            | "doc"
            | "typedoc"
            | "type"
            | "typep"
            | "opaque"
            | "spec"
            | "callback"
            | "macrocallback"
            | "impl"
            | "derive"
            | "enforce_keys"
            | "behaviour"
            | "behavior"
            | "before_compile"
            | "after_compile"
            | "after_verify"
            | "on_load"
            | "on_definition"
            | "compile"
            | "deprecated"
            | "dialyzer"
            | "external_resource"
            | "file"
            | "vsn"
            | "optional_callbacks"
    )
}

/// Returns the text of the first child inside the `arguments` node of
/// a `call`.
fn first_argument_text(node: Node, source: &[u8]) -> Option<String> {
    let args = children(node).find(|c| c.kind() == "arguments")?;
    let first = children(args).next()?;
    let text = node_text(first, source);
    if text.is_empty() { None } else { Some(text) }
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

    fn parse_elixir(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_elixir::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = ElixirSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_defmodule() {
        let result = parse_elixir("defmodule MyApp.MyModule do\nend\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MyApp.MyModule");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Module));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_public_function() {
        let result = parse_elixir("defmodule M do\n  def greet(name) do\n    name\n  end\nend\n");
        let fns: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function))
            .collect();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].name, "greet");
        assert!(fns[0].is_exported);
    }

    #[test]
    fn test_private_function() {
        let result = parse_elixir("defmodule M do\n  defp internal do\n    :ok\n  end\nend\n");
        let fns: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function))
            .collect();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].name, "internal");
        assert!(!fns[0].is_exported);
    }

    #[test]
    fn test_defstruct() {
        let result = parse_elixir("defmodule User do\n  defstruct [:name, :age]\nend\n");
        let structs: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Struct))
            .collect();
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].name, "__struct__");
        assert!(structs[0].is_exported);
    }

    #[test]
    fn test_defmacro() {
        let result = parse_elixir(
            "defmodule M do\n  defmacro my_macro(arg) do\n    quote do: unquote(arg)\n  end\nend\n",
        );
        let macros: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.name == "my_macro")
            .collect();
        assert_eq!(macros.len(), 1);
        assert!(matches!(macros[0].kind, SymbolKind::Function));
        assert!(macros[0].is_exported);
    }

    #[test]
    fn test_alias_import() {
        let result = parse_elixir("defmodule M do\n  alias MyApp.Utils\n  import Enum\nend\n");
        assert_eq!(result.imports.len(), 2);
        assert_eq!(result.imports[0].source, "MyApp.Utils");
        assert_eq!(result.imports[1].source, "Enum");
    }

    #[test]
    fn test_use_import() {
        let result = parse_elixir("defmodule M do\n  use GenServer\nend\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "GenServer");
    }

    #[test]
    fn test_module_attribute() {
        let result = parse_elixir("defmodule M do\n  @version \"1.0\"\nend\n");
        let attrs: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Variable))
            .collect();
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].name, "version");
        assert!(attrs[0].is_exported);
    }

    #[test]
    fn test_empty_file() {
        let result = parse_elixir("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_mixed_elixir() {
        let result = parse_elixir(
            r#"defmodule MyApp.UserService do
  @moduledoc "Manages user operations"
  @max_retries 3

  alias MyApp.Repo
  import Ecto.Query
  use GenServer

  defstruct [:name, :email]

  def start_link(opts) do
    GenServer.start_link(__MODULE__, opts)
  end

  defp validate(user) do
    :ok
  end

  defmacro log_call(func) do
    quote do: Logger.info(unquote(func))
  end

  defmacrop internal_log(msg) do
    quote do: Logger.debug(unquote(msg))
  end
end
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"MyApp.UserService"));
        assert!(names.contains(&"max_retries"));
        assert!(names.contains(&"__struct__"));
        assert!(names.contains(&"start_link"));
        assert!(names.contains(&"validate"));
        assert!(names.contains(&"log_call"));
        assert!(names.contains(&"internal_log"));

        // @moduledoc must be skipped
        assert!(!names.contains(&"moduledoc"));

        let start_link = result
            .symbols
            .iter()
            .find(|s| s.name == "start_link")
            .unwrap();
        assert!(start_link.is_exported);

        let validate = result
            .symbols
            .iter()
            .find(|s| s.name == "validate")
            .unwrap();
        assert!(!validate.is_exported);

        let internal_log = result
            .symbols
            .iter()
            .find(|s| s.name == "internal_log")
            .unwrap();
        assert!(!internal_log.is_exported);

        assert_eq!(result.imports.len(), 3);
        let import_sources: Vec<&str> = result.imports.iter().map(|i| i.source.as_str()).collect();
        assert!(import_sources.contains(&"MyApp.Repo"));
        assert!(import_sources.contains(&"Ecto.Query"));
        assert!(import_sources.contains(&"GenServer"));
    }
}
