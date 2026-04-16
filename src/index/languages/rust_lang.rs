use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedRelation, ExtractedSymbol, ParseResult,
    ReferenceKind, RelationKind, SymbolKind,
};
use crate::str_utils::floor_char_boundary;

pub struct RustSupport;

impl LanguageSupport for RustSupport {
    fn extensions(&self) -> &[&str] {
        &["rs"]
    }

    fn language_name(&self) -> &str {
        "rust"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_rust::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let mut references = Vec::new();
        let mut type_relations = Vec::new();
        let root = tree.root_node();
        extract_from_node(
            root,
            source,
            None,
            &mut symbols,
            &mut imports,
            &mut references,
            &mut type_relations,
        );
        ParseResult {
            symbols,
            imports,
            references,
            type_relations,
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn has_pub_visibility(node: Node) -> bool {
    children(node).any(|child| child.kind() == "visibility_modifier")
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
    type_relations: &mut Vec<ExtractedRelation>,
) {
    let kind = node.kind();
    let mut new_enclosing = enclosing;
    match kind {
        "function_item" => {
            if let Some(sym) = extract_named_item(node, source, SymbolKind::Function) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
        }
        "struct_item" => {
            if let Some(sym) = extract_named_item(node, source, SymbolKind::Struct) {
                let parent_idx = symbols.len();
                symbols.push(sym);
                extract_struct_fields(node, source, symbols, parent_idx);
                new_enclosing = Some(parent_idx);
            }
        }
        "enum_item" => {
            if let Some(sym) = extract_named_item(node, source, SymbolKind::Enum) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
        }
        "trait_item" => {
            if let Some(sym) = extract_named_item(node, source, SymbolKind::Trait) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
            }
        }
        "impl_item" => {
            extract_impl_methods(node, source, symbols, imports, references, type_relations);
            return;
        }
        "type_item" => {
            if let Some(sym) = extract_named_item(node, source, SymbolKind::Type) {
                symbols.push(sym);
            }
        }
        "const_item" => {
            if let Some(sym) = extract_named_item(node, source, SymbolKind::Const) {
                symbols.push(sym);
            }
        }
        "static_item" => {
            if let Some(sym) = extract_named_item(node, source, SymbolKind::Variable) {
                symbols.push(sym);
            }
        }
        "macro_definition" => {
            if let Some(sym) = extract_macro(node, source) {
                symbols.push(sym);
            }
        }
        "use_declaration" => {
            extract_use_declaration(node, source, &mut Vec::new(), imports);
            return;
        }
        _ => {}
    }

    // Reference harvesting runs with the INPUT enclosing - a call_expression
    // lives inside the symbol that contains it, not the symbol this very node
    // might be defining (definitions are harvested above via `new_enclosing`).
    record_reference(node, source, enclosing, references);

    for child in children(node) {
        extract_from_node(
            child,
            source,
            new_enclosing,
            symbols,
            imports,
            references,
            type_relations,
        );
    }
}

/// Called for every node visited during the symbol walk. Emits a reference
/// entry when the node matches one of the shapes we care about; otherwise
/// is a cheap no-op (single match on `node.kind()`). Filtering of noise
/// identifiers (primitive types, `Self`) happens here so the resolution
/// pass later does not have to re-apply the rules.
fn record_reference(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    let line = node.start_position().row as u32 + 1;
    match node.kind() {
        "call_expression" => {
            if let Some(func) = node.child_by_field_name("function") {
                let (name, qualifier) = extract_callee_info(func, source);
                if !name.is_empty() {
                    references.push(ExtractedReference {
                        name,
                        line,
                        from_symbol_idx: enclosing,
                        kind: ReferenceKind::Call,
                        qualifier,
                        receiver_type_hint: None,
                    });
                }
            }
        }
        "macro_invocation" => {
            // `foo!(...)` - the macro name behaves like a callee.
            if let Some(mac) = node.child_by_field_name("macro") {
                let name = match mac.kind() {
                    "identifier" => node_text(mac, source),
                    "scoped_identifier" => mac
                        .child_by_field_name("name")
                        .map(|n| node_text(n, source))
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                if !name.is_empty() && !is_builtin_macro(&name) {
                    references.push(ExtractedReference {
                        name,
                        line,
                        from_symbol_idx: enclosing,
                        kind: ReferenceKind::Call,
                        qualifier: None,
                        receiver_type_hint: None,
                    });
                }
            }
        }
        "type_identifier" => {
            // Skip type_identifiers that are the name of a definition - those
            // are not references to external symbols but the definitions
            // themselves. tree-sitter-rust attaches them as a direct child of
            // the defining node.
            let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
            if matches!(
                parent_kind,
                "struct_item" | "enum_item" | "trait_item" | "type_item" | "impl_item"
            ) {
                return;
            }
            let name = node_text(node, source);
            if !name.is_empty() && !is_builtin_type(&name) {
                references.push(ExtractedReference {
                    name,
                    line,
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

/// Given the `function` field of a `call_expression`, return the callee
/// name and an optional type qualifier. The qualifier lets the resolver
/// prefer `impl Foo { fn new() }` over other `new` symbols when the call
/// site is `Foo::new()`.
fn extract_callee_info(func: Node, source: &[u8]) -> (String, Option<String>) {
    match func.kind() {
        "identifier" => (node_text(func, source), None),
        "scoped_identifier" => {
            let name = func
                .child_by_field_name("name")
                .map(|n| node_text(n, source))
                .unwrap_or_default();
            // Extract qualifier: for `Foo::new()`, the path is `Foo`.
            // For `module::Foo::new()`, we want the type, which is
            // the segment right before the name (the path child).
            let qualifier = func
                .child_by_field_name("path")
                .map(|p| {
                    // If the path is itself a scoped_identifier (e.g. module::Foo),
                    // take only the last segment (Foo).
                    if p.kind() == "scoped_identifier" {
                        p.child_by_field_name("name")
                            .map(|n| node_text(n, source))
                            .unwrap_or_default()
                    } else {
                        node_text(p, source)
                    }
                })
                .filter(|q| !q.is_empty() && q.starts_with(|c: char| c.is_uppercase()));
            (name, qualifier)
        }
        "field_expression" => {
            let name = func
                .child_by_field_name("field")
                .map(|n| node_text(n, source))
                .unwrap_or_default();
            // For `self.foo()` or `x.foo()`, the qualifier is unknown at the
            // syntactic level. Receiver type tracking populates it later.
            (name, None)
        }
        "generic_function" => func
            .child_by_field_name("function")
            .map(|n| extract_callee_info(n, source))
            .unwrap_or_default(),
        _ => (String::new(), None),
    }
}

/// Primitive types and `Self` - these are either handled directly by the
/// compiler or self-references and should never reach the symbol graph.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "char"
            | "str"
            | "String"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            | "Self"
    )
}

/// Common macros from `std`/`core` that add noise without adding real
/// dependency information. Dropped at extraction time so the resolver and
/// PageRank stages don't have to re-filter them.
fn is_builtin_macro(name: &str) -> bool {
    matches!(
        name,
        "println"
            | "eprintln"
            | "print"
            | "eprint"
            | "format"
            | "write"
            | "writeln"
            | "vec"
            | "assert"
            | "assert_eq"
            | "assert_ne"
            | "debug_assert"
            | "debug_assert_eq"
            | "debug_assert_ne"
            | "panic"
            | "todo"
            | "unimplemented"
            | "unreachable"
            | "dbg"
            | "include_str"
            | "env"
            | "concat"
            | "stringify"
            | "matches"
    )
}

/// Recursively count cyclomatic complexity branching points in the AST subtree
/// rooted at `node`. Stops at nested function and closure boundaries so their
/// internal branching is not attributed to the enclosing function.
fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if_expression" => 1,
        "match_arm" => 1,
        "for_expression" => 1,
        "while_expression" => 1,
        "loop_expression" => 1,
        "try_expression" => 1,
        "binary_expression" => {
            let op = node
                .child_by_field_name("operator")
                .map(|n| node_text(n, source));
            match op.as_deref() {
                Some("&&") | Some("||") => 1,
                _ => 0,
            }
        }
        // Nested functions and closures are separate scopes; do not count
        // their branching as part of the enclosing function.
        "closure_expression" | "function_item" => return 0,
        _ => 0,
    };
    for child in children(node) {
        total += count_complexity(child, source);
    }
    total
}

fn extract_named_item(node: Node, source: &[u8], kind: SymbolKind) -> Option<ExtractedSymbol> {
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
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: has_pub_visibility(node),
        parent_idx: None,
        unused_excluded: false,
        complexity,
        owner_type: None,
    })
}

fn extract_macro(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: has_pub_visibility(node),
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

/// Walk a `struct_item` body and emit one `SymbolKind::Field` per member.
/// Handles both named structs (`field_declaration_list`) and tuple structs
/// (`ordered_field_declaration_list`). Each emitted field points back at the
/// struct's `symbols` index via `parent_idx`, which later becomes the real
/// parent row id at insert time.
fn extract_struct_fields(
    struct_node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    parent_idx: usize,
) {
    let Some(body) = struct_node.child_by_field_name("body") else {
        return;
    };
    match body.kind() {
        "field_declaration_list" => {
            extract_named_struct_fields(body, source, symbols, parent_idx);
        }
        "ordered_field_declaration_list" => {
            extract_tuple_struct_fields(body, source, symbols, parent_idx);
        }
        _ => {}
    }
}

/// Named struct fields: `struct Foo { pub x: f64, y: String }`.
fn extract_named_struct_fields(
    body: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    parent_idx: usize,
) {
    for child in children(body) {
        if child.kind() != "field_declaration" {
            continue;
        }
        let name_node = match child.child_by_field_name("name") {
            Some(n) => n,
            None => continue,
        };
        let name = node_text(name_node, source);
        if name.is_empty() {
            continue;
        }
        let type_text = child
            .child_by_field_name("type")
            .map(|n| node_text(n, source))
            .filter(|s| !s.is_empty());
        let signature = type_text.map(|t| format!("{}: {}", name, t));
        // A field is "exported" when the struct_item marks it `pub`.
        // tree-sitter-rust stores the visibility as a `visibility_modifier`
        // child; absence means pub(crate)/private - close enough for the
        // outline filter since `is_exported` is already a fuzzy flag.
        let is_pub = children(child).any(|c| c.kind() == "visibility_modifier");
        symbols.push(ExtractedSymbol {
            name,
            kind: SymbolKind::Field,
            line_start: child.start_position().row as u32 + 1,
            line_end: child.end_position().row as u32 + 1,
            signature,
            is_exported: is_pub,
            parent_idx: Some(parent_idx),
            // Fields of a struct never count as "unused exports" on their
            // own - the struct's own export status drives the check.
            unused_excluded: true,
            complexity: None,
            owner_type: None,
        });
    }
}

/// Tuple struct fields: `struct Foo(pub u32, String)`.
/// Positional members are emitted as fields named `0`, `1`, … with the type
/// in the signature. tree-sitter represents the body as an
/// `ordered_field_declaration_list` whose named children are
/// `visibility_modifier`, `attribute_item`, or type nodes.
fn extract_tuple_struct_fields(
    body: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    parent_idx: usize,
) {
    let mut field_idx: u32 = 0;
    let mut pending_vis = false;
    for child in children(body) {
        if child.kind() == "visibility_modifier" {
            pending_vis = true;
            continue;
        }
        if !child.is_named() || child.kind() == "attribute_item" {
            continue;
        }
        // Remaining named children are type nodes (the positional fields).
        let type_text = node_text(child, source);
        if type_text.is_empty() {
            continue;
        }
        let name = field_idx.to_string();
        let signature = Some(format!("{}: {}", name, type_text));
        symbols.push(ExtractedSymbol {
            name,
            kind: SymbolKind::Field,
            line_start: child.start_position().row as u32 + 1,
            line_end: child.end_position().row as u32 + 1,
            signature,
            is_exported: pending_vis,
            parent_idx: Some(parent_idx),
            unused_excluded: true,
            complexity: None,
            owner_type: None,
        });
        field_idx += 1;
        pending_vis = false;
    }
}

/// Extract the trait name from a trait node in `impl Trait for Type`.
/// Handles `type_identifier`, `generic_type` (e.g. `Iterator<Item = T>`),
/// and `scoped_type_identifier` (e.g. `std::fmt::Display`).
fn extract_trait_name(trait_node: Node, source: &[u8]) -> String {
    match trait_node.kind() {
        "type_identifier" => node_text(trait_node, source),
        "generic_type" => trait_node
            .child_by_field_name("type")
            .map(|n| node_text(n, source))
            .unwrap_or_default(),
        "scoped_type_identifier" => trait_node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Extract the implementing type name from an `impl` node.
/// For `impl Foo { ... }` returns `Some("Foo")`.
/// For `impl<T> Foo<T> { ... }` returns `Some("Foo")`.
/// For `impl Trait for Foo { ... }` returns `Some("Foo")` (the concrete type).
fn extract_impl_type_name(impl_node: Node, source: &[u8]) -> Option<String> {
    // For `impl Trait for Type`, tree-sitter-rust puts the concrete type in
    // the "type" field. For `impl Type`, same field holds the type directly.
    let type_node = impl_node.child_by_field_name("type")?;
    match type_node.kind() {
        "type_identifier" => Some(node_text(type_node, source)),
        // `impl Foo<Bar>` - generic type, take the type name part.
        "generic_type" => type_node
            .child_by_field_name("type")
            .map(|n| node_text(n, source)),
        // `impl module::Foo` - scoped type, take the last segment.
        "scoped_type_identifier" => type_node
            .child_by_field_name("name")
            .map(|n| node_text(n, source)),
        _ => None,
    }
}

fn extract_impl_methods(
    impl_node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
    type_relations: &mut Vec<ExtractedRelation>,
) {
    let body = match impl_node.child_by_field_name("body") {
        Some(b) => b,
        None => return,
    };
    // Trait impl methods are called via dynamic dispatch, so the static
    // "no file imports the defining file" check can't tell whether they
    // are actually unused. Flag them here so `qartez_unused` skips them at
    // query time - much cheaper than re-walking the AST for every call.
    let trait_node = impl_node.child_by_field_name("trait");
    let in_trait_impl = trait_node.is_some();
    let owner = extract_impl_type_name(impl_node, source);

    // Record type hierarchy: `impl Trait for Type` produces (Type, Trait, implements).
    if let (Some(trait_n), Some(owner_name)) = (trait_node, owner.as_deref()) {
        let trait_name = extract_trait_name(trait_n, source);
        if !trait_name.is_empty() {
            type_relations.push(ExtractedRelation {
                sub_name: owner_name.to_string(),
                super_name: trait_name,
                kind: RelationKind::Implements,
                line: impl_node.start_position().row as u32 + 1,
            });
        }
    }

    for child in children(body) {
        if child.kind() == "function_item"
            && let Some(name_node) = child.child_by_field_name("name")
        {
            let name = node_text(name_node, source);
            if !name.is_empty() {
                let method_cc = child
                    .child_by_field_name("body")
                    .map(|b| count_complexity(b, source))
                    .unwrap_or(0);
                let idx = symbols.len();
                symbols.push(ExtractedSymbol {
                    name,
                    kind: SymbolKind::Method,
                    line_start: child.start_position().row as u32 + 1,
                    line_end: child.end_position().row as u32 + 1,
                    signature: extract_signature(child, source),
                    is_exported: has_pub_visibility(child),
                    parent_idx: None,
                    unused_excluded: in_trait_impl,
                    complexity: Some(1 + method_cc),
                    owner_type: owner.clone(),
                });
                // Walk the method body with the method as enclosing so every
                // call/type reference inside is attributed to it.
                if let Some(method_body) = child.child_by_field_name("body") {
                    for grand in children(method_body) {
                        extract_from_node(
                            grand,
                            source,
                            Some(idx),
                            symbols,
                            imports,
                            references,
                            type_relations,
                        );
                    }
                }
            }
        }
    }
}

fn extract_use_declaration(
    node: Node,
    source: &[u8],
    path_parts: &mut Vec<String>,
    imports: &mut Vec<ExtractedImport>,
) {
    for child in children(node) {
        match child.kind() {
            "use_as_clause" | "scoped_use_list" | "use_wildcard" | "scoped_identifier"
            | "identifier" | "use_list" => {
                extract_use_tree(child, source, path_parts, imports);
            }
            _ => {}
        }
    }
}

fn extract_use_tree(
    node: Node,
    source: &[u8],
    path_parts: &mut Vec<String>,
    imports: &mut Vec<ExtractedImport>,
) {
    match node.kind() {
        "scoped_use_list" => {
            let mut prefix_parts = path_parts.clone();
            if let Some(path_node) = node.child_by_field_name("path") {
                collect_path_segments(path_node, source, &mut prefix_parts);
            }

            if let Some(list_node) = node.child_by_field_name("list") {
                extract_use_list(list_node, source, &prefix_parts, imports);
            }
        }
        "use_wildcard" => {
            let mut prefix_parts = path_parts.clone();
            if let Some(path_child) = children(node).find(|c| {
                c.kind() == "scoped_identifier"
                    || c.kind() == "identifier"
                    || c.kind() == "crate"
                    || c.kind() == "self"
                    || c.kind() == "super"
            }) {
                collect_path_segments(path_child, source, &mut prefix_parts);
            }
            let full_path = prefix_parts.join("::");
            if is_internal_path(&full_path) {
                imports.push(ExtractedImport {
                    source: full_path,
                    specifiers: vec!["*".to_string()],
                    is_reexport: false,
                });
            }
        }
        "scoped_identifier" => {
            let mut parts = path_parts.clone();
            collect_path_segments(node, source, &mut parts);
            let full_path = parts.join("::");
            if is_internal_path(&full_path) {
                let specifier = parts.last().cloned().unwrap_or_default();
                let source_path = if parts.len() > 1 {
                    parts[..parts.len() - 1].join("::")
                } else {
                    full_path.clone()
                };
                imports.push(ExtractedImport {
                    source: source_path,
                    specifiers: vec![specifier],
                    is_reexport: false,
                });
            }
        }
        "identifier" => {
            let name = node_text(node, source);
            if name == "crate" || name == "super" || name == "self" {
                let mut parts = path_parts.clone();
                parts.push(name);
                let full_path = parts.join("::");
                imports.push(ExtractedImport {
                    source: full_path,
                    specifiers: vec![],
                    is_reexport: false,
                });
            }
        }
        "use_as_clause" => {
            for child in children(node) {
                if child.kind() != "identifier" && child.kind() != "as" {
                    extract_use_tree(child, source, path_parts, imports);
                }
            }
        }
        "use_list" => {
            extract_use_list(node, source, path_parts, imports);
        }
        _ => {}
    }
}

fn extract_use_list(
    list_node: Node,
    source: &[u8],
    prefix_parts: &[String],
    imports: &mut Vec<ExtractedImport>,
) {
    let prefix = prefix_parts.join("::");
    if !is_internal_path(&prefix) {
        return;
    }
    let mut specifiers = Vec::new();
    for child in children(list_node) {
        match child.kind() {
            "identifier" | "type_identifier" => {
                let name = node_text(child, source);
                if !name.is_empty() {
                    specifiers.push(name);
                }
            }
            "self" => {
                specifiers.push("self".to_string());
            }
            "scoped_use_list" => {
                extract_use_tree(child, source, &mut prefix_parts.to_vec(), imports);
            }
            "use_as_clause" => {
                if let Some(first_id) = children(child).find(|c| {
                    c.kind() == "identifier"
                        || c.kind() == "type_identifier"
                        || c.kind() == "scoped_identifier"
                }) {
                    if first_id.kind() == "scoped_identifier" {
                        extract_use_tree(first_id, source, &mut prefix_parts.to_vec(), imports);
                    } else {
                        let name = node_text(first_id, source);
                        if !name.is_empty() {
                            specifiers.push(name);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if !specifiers.is_empty() {
        imports.push(ExtractedImport {
            source: prefix,
            specifiers,
            is_reexport: false,
        });
    }
}

fn collect_path_segments(node: Node, source: &[u8], parts: &mut Vec<String>) {
    match node.kind() {
        "scoped_identifier" => {
            if let Some(path) = node.child_by_field_name("path") {
                collect_path_segments(path, source, parts);
            }
            if let Some(name) = node.child_by_field_name("name") {
                let text = node_text(name, source);
                if !text.is_empty() {
                    parts.push(text);
                }
            }
        }
        "identifier" | "type_identifier" | "crate" | "self" | "super" | "metavariable" => {
            let text = node_text(node, source);
            if !text.is_empty() {
                parts.push(text);
            }
        }
        _ => {}
    }
}

fn is_internal_path(path: &str) -> bool {
    for prefix in &["crate", "super", "self"] {
        if let Some(rest) = path.strip_prefix(prefix)
            && (rest.is_empty() || rest.starts_with("::"))
        {
            return true;
        }
    }
    false
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

    let truncated = if sig.len() > 200 {
        &sig[..floor_char_boundary(sig, 200)]
    } else {
        sig
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

    fn parse_rust(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_rust::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = RustSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_function_item() {
        let result = parse_rust("fn helper(x: i32) -> i32 { x * 2 }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "helper");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(!result.symbols[0].is_exported);
        assert!(result.symbols[0].signature.is_some());
    }

    #[test]
    fn test_pub_function() {
        let result = parse_rust("pub fn greet(name: &str) -> String { name.to_string() }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_pub_crate_function() {
        let result = parse_rust("pub(crate) fn internal_helper() -> bool { true }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "internal_helper");
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_struct_item() {
        let result = parse_rust(
            "pub struct Config {
    name: String,
    debug: bool,
}",
        );
        // Struct itself + two fields (name, debug). Fields are emitted as
        // `SymbolKind::Field` pointing back at the struct via `parent_idx`,
        // which is what `qartez_outline` needs to group them visually.
        assert_eq!(result.symbols.len(), 3);
        assert_eq!(result.symbols[0].name, "Config");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Struct));
        assert!(result.symbols[0].is_exported);
        let fields: Vec<&ExtractedSymbol> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Field))
            .collect();
        assert_eq!(fields.len(), 2);
        assert!(fields.iter().all(|f| f.parent_idx == Some(0)));
        let field_names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert!(field_names.contains(&"name"));
        assert!(field_names.contains(&"debug"));
    }

    #[test]
    fn test_enum_item() {
        let result = parse_rust("pub enum Status { Active, Inactive, Pending }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Status");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Enum));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_trait_item() {
        let result = parse_rust(
            "pub trait Serializable {
    fn serialize(&self) -> Vec<u8>;
}",
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Serializable");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Trait));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_impl_methods() {
        let result = parse_rust(
            "struct Foo;
impl Foo {
    pub fn new() -> Self { Foo }
    fn private_method(&self) -> i32 { 42 }
}",
        );
        let struct_syms: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Struct))
            .collect();
        assert_eq!(struct_syms.len(), 1);
        assert_eq!(struct_syms[0].name, "Foo");

        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].name, "new");
        assert!(methods[0].is_exported);
        assert_eq!(methods[1].name, "private_method");
        assert!(!methods[1].is_exported);
    }

    #[test]
    fn test_type_alias() {
        let result = parse_rust("pub type Result<T> = std::result::Result<T, MyError>;");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Result");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Type));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_const_item() {
        let result = parse_rust("pub const MAX_SIZE: usize = 1024;");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MAX_SIZE");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Const));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_static_item() {
        let result = parse_rust("static COUNTER: AtomicUsize = AtomicUsize::new(0);");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "COUNTER");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_macro_definition() {
        let result = parse_rust(
            "macro_rules! my_macro {
    ($x:expr) => { println!(\"{}\", $x) };
}",
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "my_macro");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_use_crate_simple() {
        let result = parse_rust("use crate::module::Item;");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "crate::module");
        assert_eq!(result.imports[0].specifiers, vec!["Item"]);
    }

    #[test]
    fn test_use_crate_list() {
        let result = parse_rust("use crate::module::{Foo, Bar};");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "crate::module");
        assert_eq!(result.imports[0].specifiers, vec!["Foo", "Bar"]);
    }

    #[test]
    fn test_use_super() {
        let result = parse_rust("use super::something;");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "super");
        assert_eq!(result.imports[0].specifiers, vec!["something"]);
    }

    #[test]
    fn test_use_wildcard() {
        let result = parse_rust("use crate::prelude::*;");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "crate::prelude");
        assert_eq!(result.imports[0].specifiers, vec!["*"]);
    }

    #[test]
    fn test_external_crate_skipped() {
        let result = parse_rust("use serde::Serialize;");
        assert_eq!(result.imports.len(), 0);
    }

    #[test]
    fn test_line_numbers() {
        let result = parse_rust("fn a() { }\n\nfn b() {\n    return;\n}\n");
        assert_eq!(result.symbols.len(), 2);
        assert_eq!(result.symbols[0].line_start, 1);
        assert_eq!(result.symbols[0].line_end, 1);
        assert_eq!(result.symbols[1].line_start, 3);
        assert_eq!(result.symbols[1].line_end, 5);
    }

    #[test]
    fn test_mixed_declarations() {
        let result = parse_rust(
            r#"
use crate::config::Config;

pub struct AppConfig {
    debug: bool,
}

pub const DEFAULT_VALUE: i32 = 42;

pub fn create_app(config: AppConfig) -> App {
    App { config }
}

impl AppConfig {
    pub fn new() -> Self {
        Self { debug: false }
    }
}

enum InternalState {
    Ready,
    Busy,
}
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AppConfig"));
        assert!(names.contains(&"DEFAULT_VALUE"));
        assert!(names.contains(&"create_app"));
        assert!(names.contains(&"new"));
        assert!(names.contains(&"InternalState"));

        let exported_count = result.symbols.iter().filter(|s| s.is_exported).count();
        assert_eq!(exported_count, 4);

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "crate::config");
    }

    // -- Reference extraction tests --

    #[test]
    fn test_refs_call_expression_attributed_to_enclosing() {
        let result = parse_rust(
            r#"
fn helper() -> i32 { 42 }
fn caller() -> i32 { helper() + 1 }
"#,
        );
        let caller_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "caller")
            .expect("caller symbol");
        let call_refs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| matches!(r.kind, ReferenceKind::Call))
            .collect();
        assert!(
            call_refs
                .iter()
                .any(|r| r.name == "helper" && r.from_symbol_idx == Some(caller_idx)),
            "helper() call inside caller should be attributed to caller, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_field_expression_call() {
        let result = parse_rust(
            r#"
struct Foo;
impl Foo {
    fn bar(&self) {}
}
fn wrapper(f: &Foo) { f.bar(); }
"#,
        );
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "bar" && matches!(r.kind, ReferenceKind::Call)),
            "f.bar() should produce a Call reference to `bar`, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_scoped_call_uses_last_segment() {
        let result = parse_rust(
            r#"
fn outer() { some_mod::inner(); }
"#,
        );
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "inner" && matches!(r.kind, ReferenceKind::Call)),
            "some_mod::inner() should produce a Call reference to `inner`"
        );
    }

    #[test]
    fn test_refs_struct_field_type_is_typeref() {
        let result = parse_rust(
            r#"
pub struct Wrapper {
    inner: Config,
}
"#,
        );
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "Config" && matches!(r.kind, ReferenceKind::TypeRef)),
            "field type Config should produce a TypeRef reference"
        );
    }

    #[test]
    fn test_refs_primitive_types_are_filtered() {
        let result = parse_rust(
            r#"
fn f(x: i32) -> u64 { x as u64 }
"#,
        );
        let prim_refs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "i32" || r.name == "u64")
            .collect();
        assert!(
            prim_refs.is_empty(),
            "primitive types should not be recorded as references"
        );
    }

    #[test]
    fn test_refs_definition_name_not_self_ref() {
        let result = parse_rust(
            r#"
pub struct Foo {}
"#,
        );
        // The struct's own name must not appear as a reference - it is a
        // definition, not a use.
        let foo_refs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "Foo")
            .collect();
        assert!(
            foo_refs.is_empty(),
            "struct Foo should not reference itself, got {:?}",
            foo_refs
        );
    }

    #[test]
    fn test_refs_method_body_attributed_to_method() {
        let result = parse_rust(
            r#"
fn helper() {}
struct Foo;
impl Foo {
    fn run(&self) { helper(); }
}
"#,
        );
        let method_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "run" && matches!(s.kind, SymbolKind::Method))
            .expect("run method");
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "helper" && r.from_symbol_idx == Some(method_idx)),
            "helper() inside run should be attributed to run method"
        );
    }

    #[test]
    fn test_refs_builtin_macro_filtered() {
        let result = parse_rust(
            r#"
fn main() {
    println!("hi");
    some_user_macro!();
}
"#,
        );
        assert!(
            !result.references.iter().any(|r| r.name == "println"),
            "println! is noise and should be filtered"
        );
        assert!(
            result
                .references
                .iter()
                .any(|r| r.name == "some_user_macro"),
            "user-defined macro call should be retained"
        );
    }

    #[test]
    fn test_impl_methods_have_owner_type() {
        let result = parse_rust(
            r#"
struct Foo;
impl Foo {
    pub fn new() -> Self { Foo }
    fn helper(&self) {}
}
"#,
        );
        let new_method = result
            .symbols
            .iter()
            .find(|s| s.name == "new" && matches!(s.kind, SymbolKind::Method))
            .expect("new method should be extracted");
        assert_eq!(
            new_method.owner_type.as_deref(),
            Some("Foo"),
            "impl Foo {{ fn new() }} should have owner_type = Foo"
        );
        let helper = result
            .symbols
            .iter()
            .find(|s| s.name == "helper")
            .expect("helper method");
        assert_eq!(helper.owner_type.as_deref(), Some("Foo"));
    }

    #[test]
    fn test_trait_impl_methods_have_owner_type() {
        let result = parse_rust(
            r#"
struct Bar;
trait Greet { fn greet(&self); }
impl Greet for Bar {
    fn greet(&self) {}
}
"#,
        );
        let greet = result
            .symbols
            .iter()
            .find(|s| s.name == "greet" && matches!(s.kind, SymbolKind::Method))
            .expect("greet method from trait impl");
        assert_eq!(
            greet.owner_type.as_deref(),
            Some("Bar"),
            "impl Greet for Bar {{ fn greet() }} should have owner_type = Bar"
        );
    }

    #[test]
    fn test_generic_impl_owner_type() {
        let result = parse_rust(
            r#"
struct Wrapper<T>(T);
impl<T> Wrapper<T> {
    fn inner(&self) -> &T { &self.0 }
}
"#,
        );
        let inner = result
            .symbols
            .iter()
            .find(|s| s.name == "inner")
            .expect("inner method");
        assert_eq!(
            inner.owner_type.as_deref(),
            Some("Wrapper"),
            "generic impl should extract base type name without params"
        );
    }

    #[test]
    fn test_free_function_no_owner_type() {
        let result = parse_rust(
            r#"
pub fn standalone() {}
"#,
        );
        let f = result
            .symbols
            .iter()
            .find(|s| s.name == "standalone")
            .expect("standalone function");
        assert!(
            f.owner_type.is_none(),
            "free function should have no owner_type"
        );
    }

    #[test]
    fn test_scoped_call_has_qualifier() {
        let result = parse_rust(
            r#"
struct Foo;
impl Foo { pub fn new() -> Self { Foo } }
fn main() {
    let _x = Foo::new();
}
"#,
        );
        let new_ref = result
            .references
            .iter()
            .find(|r| r.name == "new" && matches!(r.kind, ReferenceKind::Call))
            .expect("Foo::new() call reference");
        assert_eq!(
            new_ref.qualifier.as_deref(),
            Some("Foo"),
            "Foo::new() should have qualifier = Foo"
        );
    }

    #[test]
    fn test_plain_call_no_qualifier() {
        let result = parse_rust(
            r#"
fn helper() {}
fn main() { helper(); }
"#,
        );
        let r = result
            .references
            .iter()
            .find(|r| r.name == "helper")
            .expect("helper() call");
        assert!(r.qualifier.is_none(), "plain call should have no qualifier");
    }

    #[test]
    fn test_module_scoped_call_qualifier() {
        let result = parse_rust(
            r#"
fn main() {
    let _x = std::collections::HashMap::new();
}
"#,
        );
        let new_ref = result
            .references
            .iter()
            .find(|r| r.name == "new" && matches!(r.kind, ReferenceKind::Call))
            .expect("HashMap::new() call reference");
        assert_eq!(
            new_ref.qualifier.as_deref(),
            Some("HashMap"),
            "std::collections::HashMap::new() should have qualifier = HashMap"
        );
    }
}
