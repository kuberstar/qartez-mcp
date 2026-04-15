use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct PhpSupport;

impl LanguageSupport for PhpSupport {
    fn extensions(&self) -> &[&str] {
        &["php"]
    }

    fn language_name(&self) -> &str {
        "php"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_php::LANGUAGE_PHP)
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
            ..Default::default()
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn has_visibility(node: Node, source: &[u8], modifier: &str) -> bool {
    children(node)
        .any(|child| child.kind() == "visibility_modifier" && node_text(child, source) == modifier)
}

fn is_public_member(node: Node, source: &[u8]) -> bool {
    has_visibility(node, source, "public")
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
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Class, true) {
                symbols.push(sym);
            }
            extract_class_body(node, source, enclosing, symbols, imports, references);
            return;
        }
        "interface_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Interface, true) {
                symbols.push(sym);
            }
            extract_class_body(node, source, enclosing, symbols, imports, references);
            return;
        }
        "trait_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Trait, true) {
                symbols.push(sym);
            }
            extract_class_body(node, source, enclosing, symbols, imports, references);
            return;
        }
        "enum_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Enum, true) {
                symbols.push(sym);
            }
            extract_class_body(node, source, enclosing, symbols, imports, references);
            return;
        }
        "function_definition" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Function, true) {
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
        "const_declaration" => {
            extract_const(node, source, symbols);
            return;
        }
        "namespace_use_declaration" => {
            extract_use(node, source, imports);
            return;
        }
        "expression_statement" => {
            if let Some(imp) = extract_require_include(node, source) {
                imports.push(imp);
                return;
            }
        }
        "namespace_definition" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Module, true) {
                symbols.push(sym);
            }
            if let Some(body) = node.child_by_field_name("body") {
                for child in children(body) {
                    extract_from_node(child, source, enclosing, symbols, imports, references);
                }
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
        "function_call_expression" => {
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
                    qualifier: None,
                    receiver_type_hint: None,
                });
            }
        }
        "member_call_expression" => {
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
                    qualifier: None,
                    receiver_type_hint: None,
                });
            }
        }
        "object_creation_expression" => {
            let name = node
                .child(1)
                .map(|n| extract_callee_name(n, source))
                .unwrap_or_default();
            if !name.is_empty() && !is_builtin_type(&name) {
                references.push(ExtractedReference {
                    name,
                    line: node.start_position().row as u32 + 1,
                    from_symbol_idx: enclosing,
                    kind: ReferenceKind::Call,
                    qualifier: None,
                    receiver_type_hint: None,
                });
            }
        }
        "named_type" => {
            let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
            if matches!(
                parent_kind,
                "class_declaration"
                    | "interface_declaration"
                    | "trait_declaration"
                    | "enum_declaration"
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
                    qualifier: None,
                    receiver_type_hint: None,
                });
            }
        }
        _ => {}
    }
}

fn extract_callee_name(func: Node, source: &[u8]) -> String {
    match func.kind() {
        "name" | "qualified_name" => {
            let text = node_text(func, source);
            text.rsplit('\\').next().unwrap_or(&text).to_string()
        }
        _ => node_text(func, source),
    }
}

fn is_builtin_callable(name: &str) -> bool {
    matches!(
        name,
        "echo"
            | "print"
            | "var_dump"
            | "print_r"
            | "isset"
            | "unset"
            | "empty"
            | "die"
            | "exit"
            | "array"
            | "count"
            | "strlen"
            | "substr"
            | "implode"
            | "explode"
            | "array_push"
            | "array_pop"
            | "array_map"
            | "array_filter"
            | "json_encode"
            | "json_decode"
            | "is_null"
            | "is_array"
            | "is_string"
    )
}

fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "string"
            | "float"
            | "bool"
            | "array"
            | "object"
            | "void"
            | "null"
            | "mixed"
            | "callable"
            | "iterable"
            | "self"
            | "static"
            | "parent"
    )
}

fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if_statement" => 1,
        "case_statement" => 1,
        "for_statement" | "foreach_statement" | "while_statement" | "do_statement" => 1,
        "catch_clause" => 1,
        "binary_expression" => {
            let op = node
                .child_by_field_name("operator")
                .map(|n| node_text(n, source));
            match op.as_deref() {
                Some("&&") | Some("||") | Some("and") | Some("or") => 1,
                _ => 0,
            }
        }
        "arrow_function" => return 0,
        _ => 0,
    };
    for child in children(node) {
        total += count_complexity(child, source);
    }
    total
}

fn extract_named_decl(
    node: Node,
    source: &[u8],
    kind: SymbolKind,
    top_level_exported: bool,
) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let complexity = match kind {
        SymbolKind::Function => {
            let body_cc = node
                .child_by_field_name("body")
                .map(|body| count_complexity(body, source))
                .unwrap_or(0);
            Some(1 + body_cc)
        }
        _ => None,
    };
    Some(ExtractedSymbol {
        is_exported: top_level_exported,
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity,
        owner_type: None,
    })
}

fn extract_class_body(
    class_node: Node,
    source: &[u8],
    _class_enclosing: Option<usize>,
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
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = node_text(name_node, source);
                    if !name.is_empty() {
                        let idx = symbols.len();
                        let method_cc = child
                            .child_by_field_name("body")
                            .map(|body| count_complexity(body, source))
                            .unwrap_or(0);
                        symbols.push(ExtractedSymbol {
                            is_exported: is_public_member(child, source),
                            name,
                            kind: SymbolKind::Method,
                            line_start: child.start_position().row as u32 + 1,
                            line_end: child.end_position().row as u32 + 1,
                            signature: extract_signature(child, source),
                            parent_idx: None,
                            unused_excluded: false,
                            complexity: Some(1 + method_cc),
                            owner_type: None,
                        });
                        if let Some(body) = child.child_by_field_name("body") {
                            for grand in children(body) {
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
                }
            }
            "property_declaration" => {
                extract_property(child, source, symbols);
            }
            "const_declaration" => {
                extract_const(child, source, symbols);
            }
            _ => {}
        }
    }
}

fn extract_property(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(node) {
        if child.kind() == "property_element"
            && let Some(var_node) = child.child_by_field_name("name")
        {
            let name = node_text(var_node, source);
            let name = name.trim_start_matches('$');
            if !name.is_empty() {
                symbols.push(ExtractedSymbol {
                    is_exported: is_public_member(node, source),
                    name: name.to_string(),
                    kind: SymbolKind::Variable,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    signature: extract_signature(node, source),
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
                return;
            }
        }
        if child.kind() == "variable_name" {
            let name = node_text(child, source);
            let name = name.trim_start_matches('$');
            if !name.is_empty() {
                symbols.push(ExtractedSymbol {
                    is_exported: is_public_member(node, source),
                    name: name.to_string(),
                    kind: SymbolKind::Variable,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    signature: extract_signature(node, source),
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
                return;
            }
        }
    }
}

fn extract_const(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(node) {
        if child.kind() == "const_element"
            && let Some(name_node) = child.child_by_field_name("name")
        {
            let name = node_text(name_node, source);
            if !name.is_empty() {
                symbols.push(ExtractedSymbol {
                    is_exported: true,
                    name,
                    kind: SymbolKind::Const,
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
    }
}

fn extract_use(node: Node, source: &[u8], imports: &mut Vec<ExtractedImport>) {
    for child in children(node) {
        if child.kind() == "namespace_use_clause" {
            let full = node_text(child, source);
            let path = full.trim().trim_start_matches('\\');
            if !path.is_empty() {
                let parts: Vec<&str> = path.rsplitn(2, '\\').collect();
                let (specifier, source_path) = if parts.len() == 2 {
                    (parts[0].to_string(), parts[1].to_string())
                } else {
                    (String::new(), path.to_string())
                };

                imports.push(ExtractedImport {
                    source: source_path,
                    specifiers: if specifier.is_empty() {
                        vec![]
                    } else {
                        vec![specifier]
                    },
                    is_reexport: false,
                });
            }
        }
        if child.kind() == "namespace_name" {
            let path = node_text(child, source);
            let path = path.trim().trim_start_matches('\\');
            if !path.is_empty() {
                let parts: Vec<&str> = path.rsplitn(2, '\\').collect();
                let (specifier, source_path) = if parts.len() == 2 {
                    (parts[0].to_string(), parts[1].to_string())
                } else {
                    (String::new(), path.to_string())
                };

                imports.push(ExtractedImport {
                    source: source_path,
                    specifiers: if specifier.is_empty() {
                        vec![]
                    } else {
                        vec![specifier]
                    },
                    is_reexport: false,
                });
            }
        }
    }
}

fn extract_require_include(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let text = node_text(node, source);
    let trimmed = text.trim().trim_end_matches(';').trim();

    let path = if let Some(rest) = trimmed.strip_prefix("require_once") {
        rest.trim()
    } else if let Some(rest) = trimmed.strip_prefix("require") {
        rest.trim()
    } else if let Some(rest) = trimmed.strip_prefix("include_once") {
        rest.trim()
    } else if let Some(rest) = trimmed.strip_prefix("include") {
        rest.trim()
    } else {
        return None;
    };

    let path = unquote(path);
    if path.is_empty() {
        return None;
    }

    Some(ExtractedImport {
        source: path,
        specifiers: vec![],
        is_reexport: false,
    })
}

fn unquote(s: &str) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
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

    fn parse_php(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_php::LANGUAGE_PHP);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = PhpSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_class() {
        let result = parse_php("<?php class MyService { }");
        let classes: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "MyService");
        assert!(classes[0].is_exported);
    }

    #[test]
    fn test_interface() {
        let result = parse_php("<?php interface Repository { public function save(); }");
        let ifaces: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Interface))
            .collect();
        assert_eq!(ifaces.len(), 1);
        assert_eq!(ifaces[0].name, "Repository");
    }

    #[test]
    fn test_trait() {
        let result = parse_php("<?php trait Loggable { public function log() { } }");
        let traits: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Trait))
            .collect();
        assert_eq!(traits.len(), 1);
        assert_eq!(traits[0].name, "Loggable");
    }

    #[test]
    fn test_function() {
        let result = parse_php("<?php function greet($name) { return \"Hello $name\"; }");
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
    fn test_method_visibility() {
        let result = parse_php(
            "<?php class Foo {\n    public function run() { }\n    private function count() { return 0; }\n}",
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
    fn test_use_statement() {
        let result = parse_php("<?php use App\\Models\\User;");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "App\\Models");
        assert_eq!(result.imports[0].specifiers, vec!["User"]);
    }

    #[test]
    fn test_empty_file() {
        let result = parse_php("<?php");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_enum() {
        let result = parse_php("<?php enum Status { case Active; case Inactive; }");
        let enums: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Enum))
            .collect();
        assert_eq!(enums.len(), 1);
        assert_eq!(enums[0].name, "Status");
    }

    #[test]
    fn test_mixed_declarations() {
        let result = parse_php(
            r#"<?php
use App\Models\User;
use App\Services\AuthService;

class UserController {
    public function index() { }
    private function helper() { }
}

function standalone() { }
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserController"));
        assert!(names.contains(&"index"));
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"standalone"));

        assert_eq!(result.imports.len(), 2);
    }

    #[test]
    fn test_refs_call_attributed_to_method() {
        let result = parse_php(
            r#"<?php
class Svc {
    public function run() { $this->process(); }
    public function process() {}
}
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

    #[test]
    fn test_refs_function_call() {
        let result = parse_php(
            r#"<?php
function helper() { return 1; }
function caller() { return helper(); }
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
    fn test_refs_builtin_filtered() {
        let result = parse_php(
            r#"<?php
function f() {
    $x = count([1,2,3]);
    var_dump($x);
}
"#,
        );
        assert!(
            !result
                .references
                .iter()
                .any(|r| r.name == "count" || r.name == "var_dump"),
            "built-in calls must not be recorded as references"
        );
    }
}
