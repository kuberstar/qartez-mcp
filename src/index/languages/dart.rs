use std::collections::HashMap;

use tree_sitter::{Language, Node};

use super::LanguageSupport;
use super::common::{self, children, node_text};
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

/// Identifier -> declared type name, populated from formal parameters, typed
/// locals, and (for methods) enclosing-class fields. Used by the resolver's
/// receiver-type heuristic: a call `receiver.method(...)` emits a hint when
/// `receiver` is a bare identifier found in this map.
type ScopeEnv = HashMap<String, String>;

pub struct DartSupport;

impl LanguageSupport for DartSupport {
    fn extensions(&self) -> &[&str] {
        &["dart"]
    }

    fn language_name(&self) -> &str {
        "dart"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_dart::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let mut references = Vec::new();
        let root = tree.root_node();
        extract_from_node(
            root,
            source,
            &mut symbols,
            &mut imports,
            &mut references,
            None,
        );
        ParseResult {
            symbols,
            imports,
            references,
            ..Default::default()
        }
    }
}

fn is_dart_exported(name: &str) -> bool {
    !name.starts_with('_')
}

fn has_abstract_child(node: Node) -> bool {
    children(node).any(|c| c.kind() == "abstract")
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
    enclosing: Option<usize>,
) {
    match node.kind() {
        "class_declaration" => {
            if let Some(sym) = extract_class(node, source) {
                let idx = symbols.len();
                symbols.push(sym);
                // Class-header references (superclass, `with`, `implements`)
                // attribute to the class. Skip the body - `extract_class_body`
                // walks each member and attributes per-method.
                collect_references_skip(node, source, Some(idx), references, &["class_body"]);
                extract_class_body(node, source, idx, symbols, references);
            }
            return;
        }
        "mixin_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Trait) {
                let idx = symbols.len();
                symbols.push(sym);
                collect_references_skip(node, source, Some(idx), references, &["class_body"]);
                extract_class_body(node, source, idx, symbols, references);
            }
            return;
        }
        "enum_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Enum) {
                let idx = symbols.len();
                symbols.push(sym);
                collect_references(node, source, Some(idx), references);
            }
            return;
        }
        "extension_declaration" => {
            if let Some(sym) = extract_named_decl(node, source, SymbolKind::Class) {
                let idx = symbols.len();
                symbols.push(sym);
                collect_references_skip(node, source, Some(idx), references, &["extension_body"]);
                extract_class_body(node, source, idx, symbols, references);
            }
            return;
        }
        "type_alias" => {
            if let Some(sym) = extract_type_alias(node, source) {
                let idx = symbols.len();
                symbols.push(sym);
                collect_references(node, source, Some(idx), references);
            }
            return;
        }
        "function_signature" => {
            if is_top_level(node)
                && let Some(sym) = extract_function(node, source)
            {
                let idx = symbols.len();
                symbols.push(sym);
                // Sweep the signature (for param/return type refs) and
                // the adjacent function_body sibling (for call and
                // type refs in the body). Using the shared source_file
                // parent here would bleed references from unrelated
                // top-level declarations into this function.
                let mut env = ScopeEnv::new();
                collect_param_types(node, source, &mut env);
                collect_references_scoped(node, source, Some(idx), references, &[], Some(&env));
                if let Some(body) = node.next_sibling()
                    && body.kind() == "function_body"
                {
                    collect_local_types(body, source, &mut env);
                    collect_references_scoped(body, source, Some(idx), references, &[], Some(&env));
                }
            }
            return;
        }
        "import_or_export" => {
            if let Some(imp) = extract_import(node, source) {
                imports.push(imp);
            }
            return;
        }
        "part_directive" => {
            if let Some(imp) = extract_part(node, source) {
                imports.push(imp);
            }
            return;
        }
        "static_final_declaration_list" => {
            if is_top_level(node) {
                let kind = resolve_variable_kind(node);
                extract_variable_list(node, source, kind, symbols);
                collect_references(node, source, enclosing, references);
            }
            return;
        }
        "initialized_identifier_list" => {
            if is_top_level(node) {
                extract_initialized_list(node, source, symbols);
                collect_references(node, source, enclosing, references);
            }
            return;
        }
        _ => {}
    }

    for child in children(node) {
        extract_from_node(child, source, symbols, imports, references, enclosing);
    }
}

fn is_top_level(node: Node) -> bool {
    node.parent()
        .map(|p| p.kind() == "source_file")
        .unwrap_or(false)
}

fn resolve_variable_kind(node: Node) -> SymbolKind {
    if let Some(prev) = node.prev_sibling()
        && prev.kind() == "const"
    {
        return SymbolKind::Const;
    }
    SymbolKind::Variable
}

fn extract_class(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let sig = if has_abstract_child(node) {
        let base = extract_signature(node, source);
        base.or_else(|| Some(format!("abstract class {name}")))
    } else {
        extract_signature(node, source)
    };
    Some(ExtractedSymbol {
        is_exported: is_dart_exported(&name),
        name,
        kind: SymbolKind::Class,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: sig,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn count_complexity(node: Node, source: &[u8]) -> u32 {
    let mut total = match node.kind() {
        "if_statement" => 1,
        "switch_statement_case" => 1,
        "for_statement" | "while_statement" | "do_statement" => 1,
        "catch_clause" => 1,
        "conditional_expression" => 1,
        "binary_expression" => {
            let op = node
                .child_by_field_name("operator")
                .map(|n| node_text(n, source));
            match op.as_deref() {
                Some("&&") | Some("||") => 1,
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

fn extract_named_decl(node: Node, source: &[u8], kind: SymbolKind) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        is_exported: is_dart_exported(&name),
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn extract_type_alias(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = children(node).find(|c| c.kind() == "type_identifier")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        is_exported: is_dart_exported(&name),
        name,
        kind: SymbolKind::Type,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn extract_function(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let body_node = node.next_sibling().filter(|s| s.kind() == "function_body");
    let line_end = body_node
        .map(|b| b.end_position().row as u32 + 1)
        .unwrap_or(node.end_position().row as u32 + 1);
    let body_cc = body_node
        .map(|body| count_complexity(body, source))
        .unwrap_or(0);
    Some(ExtractedSymbol {
        is_exported: is_dart_exported(&name),
        name,
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end,
        signature: extract_signature(node, source),
        parent_idx: None,
        unused_excluded: false,
        complexity: Some(1 + body_cc),
        owner_type: None,
    })
}

fn extract_class_body(
    class_node: Node,
    source: &[u8],
    class_idx: usize,
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    let body =
        children(class_node).find(|c| c.kind() == "class_body" || c.kind() == "extension_body");
    let body = match body {
        Some(b) => b,
        None => return,
    };
    // Build the class-level field type environment once; every method below
    // layers its params + locals on top of this base.
    let mut class_fields = ScopeEnv::new();
    collect_class_field_types(class_node, source, &mut class_fields);
    for member in children(body) {
        if member.kind() != "class_member" {
            continue;
        }
        if let Some(sym) = extract_member(member, source, class_idx) {
            let method_idx = symbols.len();
            symbols.push(sym);
            // Walk this method's body (and signature) for call + type
            // references attributed to the method itself, not the class.
            let method_body = member
                .child_by_field_name("body")
                .or_else(|| children(member).find(|c| c.kind() == "function_body"));
            let mut method_env = class_fields.clone();
            collect_param_types(member, source, &mut method_env);
            if let Some(b) = method_body {
                collect_local_types(b, source, &mut method_env);
                collect_references_scoped(
                    b,
                    source,
                    Some(method_idx),
                    references,
                    &[],
                    Some(&method_env),
                );
            }
            // Sweep the signature too so parameter and return types attribute
            // to the method.
            collect_references_scoped(
                member,
                source,
                Some(method_idx),
                references,
                &["function_body"],
                Some(&method_env),
            );
        } else {
            // Non-method member (e.g. field with initializer): attribute any
            // references to the enclosing class. Field initializers can see
            // sibling field types (useful for `this.x` patterns) so pass the
            // class env.
            collect_references_scoped(
                member,
                source,
                Some(class_idx),
                references,
                &[],
                Some(&class_fields),
            );
        }
    }
}

fn extract_member(member: Node, source: &[u8], parent_idx: usize) -> Option<ExtractedSymbol> {
    for child in children(member) {
        match child.kind() {
            "method_signature" => {
                return extract_method_from_signature(child, source, parent_idx, member);
            }
            "declaration" => {
                for inner in children(child) {
                    match inner.kind() {
                        "function_signature"
                        | "getter_signature"
                        | "setter_signature"
                        | "constructor_signature" => {
                            let name_node = inner.child_by_field_name("name")?;
                            let name = node_text(name_node, source);
                            if name.is_empty() {
                                return None;
                            }
                            let member_cc = member
                                .child_by_field_name("body")
                                .or_else(|| children(member).find(|c| c.kind() == "function_body"))
                                .map(|body| count_complexity(body, source))
                                .unwrap_or(0);
                            return Some(ExtractedSymbol {
                                is_exported: is_dart_exported(&name),
                                name,
                                kind: SymbolKind::Method,
                                line_start: member.start_position().row as u32 + 1,
                                line_end: member.end_position().row as u32 + 1,
                                signature: extract_signature(inner, source),
                                parent_idx: Some(parent_idx),
                                unused_excluded: false,
                                complexity: Some(1 + member_cc),
                                owner_type: None,
                            });
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_method_from_signature(
    sig_node: Node,
    source: &[u8],
    parent_idx: usize,
    member: Node,
) -> Option<ExtractedSymbol> {
    for child in children(sig_node) {
        match child.kind() {
            "function_signature"
            | "getter_signature"
            | "setter_signature"
            | "constructor_signature"
            | "factory_constructor_signature" => {
                let name_node = child.child_by_field_name("name")?;
                let name = node_text(name_node, source);
                if name.is_empty() {
                    return None;
                }
                let sig_cc = member
                    .child_by_field_name("body")
                    .or_else(|| children(member).find(|c| c.kind() == "function_body"))
                    .map(|body| count_complexity(body, source))
                    .unwrap_or(0);
                return Some(ExtractedSymbol {
                    is_exported: is_dart_exported(&name),
                    name,
                    kind: SymbolKind::Method,
                    line_start: member.start_position().row as u32 + 1,
                    line_end: member.end_position().row as u32 + 1,
                    signature: extract_signature(child, source),
                    parent_idx: Some(parent_idx),
                    unused_excluded: false,
                    complexity: Some(1 + sig_cc),
                    owner_type: None,
                });
            }
            _ => {}
        }
    }
    None
}

fn extract_import(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let uri = find_configurable_uri(node, source)?;
    if uri.is_empty() {
        return None;
    }
    // Dart's `import_or_export` covers both `import '…';` and `export '…';`.
    // A barrel library re-exports its internal files so every consumer of the
    // barrel transitively depends on them - we record that as an edge (with
    // is_reexport=true) so impact/blast analysis walks through the barrel.
    let is_reexport = is_export_directive(node);
    Some(ExtractedImport {
        source: uri,
        specifiers: vec![],
        is_reexport,
    })
}

fn is_export_directive(node: Node) -> bool {
    children(node).any(|c| matches!(c.kind(), "library_export" | "export_specification"))
}

fn find_configurable_uri(node: Node, source: &[u8]) -> Option<String> {
    for child in children(node) {
        if child.kind() == "library_import"
            || child.kind() == "import_specification"
            || child.kind() == "library_export"
            || child.kind() == "export_specification"
        {
            return find_configurable_uri(child, source);
        }
        if child.kind() == "configurable_uri" {
            let raw = node_text(child, source);
            return Some(common::unquote(&raw));
        }
    }
    None
}

fn extract_part(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let uri_node = children(node).find(|c| c.kind() == "uri")?;
    let raw = node_text(uri_node, source);
    let path = common::unquote(&raw);
    if path.is_empty() {
        return None;
    }
    Some(ExtractedImport {
        source: path,
        specifiers: vec![],
        is_reexport: true,
    })
}

fn extract_variable_list(
    node: Node,
    source: &[u8],
    kind: SymbolKind,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    for child in children(node) {
        if child.kind() == "static_final_declaration" {
            let name_node = children(child).find(|c| c.kind() == "identifier");
            if let Some(name_node) = name_node {
                let name = node_text(name_node, source);
                if !name.is_empty() {
                    symbols.push(ExtractedSymbol {
                        is_exported: is_dart_exported(&name),
                        name,
                        kind: kind.clone(),
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
}

fn extract_initialized_list(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(node) {
        if child.kind() == "initialized_identifier" {
            let name_node = children(child).find(|c| c.kind() == "identifier");
            if let Some(name_node) = name_node {
                let name = node_text(name_node, source);
                if !name.is_empty() {
                    symbols.push(ExtractedSymbol {
                        is_exported: is_dart_exported(&name),
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
            }
        }
    }
}

fn extract_signature(node: Node, source: &[u8]) -> Option<String> {
    common::brace_or_first_line_signature(node, source)
}

/// Walks `root` and records call-site and type-reference edges. Dart's
/// tree-sitter grammar exposes call sites under a handful of parent kinds
/// (`function_expression_invocation`, `method_invocation`,
/// `invocation_expression`, `selector`) where the callee lives in an
/// `identifier` child. We treat a `type_identifier` as a TypeRef unless its
/// parent is itself a declaration header (class/mixin/enum/extension/type
/// alias), to avoid recording a class as a reference to itself.
///
/// References inside method bodies are attributed per-method by
/// `extract_class_body`; this function is invoked separately on each
/// method body with `enclosing = method_idx`. Class-level call sites pass
/// `["class_body"]` (or `["extension_body"]`) to `collect_references_skip`
/// so the class header sweep stops at the body boundary.
fn collect_references(
    root: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    collect_references_scoped(root, source, enclosing, references, &[], None);
}

/// Like `collect_references` but does not descend into nodes whose kind is
/// listed in `skip_kinds`. Used so that a class-level sweep can record
/// references in the class header (superclass, `with`, `implements`) without
/// also bleeding every method-body call into the class symbol - those are
/// attributed per-method by `extract_class_body`.
fn collect_references_skip(
    root: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
    skip_kinds: &[&str],
) {
    collect_references_scoped(root, source, enclosing, references, skip_kinds, None);
}

fn collect_references_scoped(
    root: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
    skip_kinds: &[&str],
    env: Option<&ScopeEnv>,
) {
    let mut cursor = root.walk();
    let mut stack: Vec<Node> = children(root).collect();
    while let Some(node) = stack.pop() {
        record_reference(node, source, enclosing, references, env);
        if skip_kinds.contains(&node.kind()) {
            continue;
        }
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn record_reference(
    node: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
    env: Option<&ScopeEnv>,
) {
    let line = node.start_position().row as u32 + 1;
    match node.kind() {
        // Tree-sitter-dart emits calls as two sibling nodes under the
        // parent: an `identifier` for the callee and a `selector` that
        // contains an `argument_part`. Detect the shape by looking at the
        // selector: if it wraps an argument_part, the previous sibling's
        // identifier text is the callee.
        "selector" => {
            // Calls in tree-sitter-dart show up as a `selector` whose child
            // is an `argument_part`. The callee lives in the previous
            // sibling. Two shapes:
            //
            //   bare/constructor call    `Foo(args)`
            //     identifier "Foo"         <-- prev sibling
            //     selector "(args)"        <-- this node
            //
            //   method call              `obj.method(args)`
            //     identifier "obj"
            //     selector ".method"       <-- prev sibling (no argument_part)
            //       unconditional_assignable_selector
            //         identifier "method"  <-- the callee
            //     selector "(args)"        <-- this node
            //
            // For the bare shape we read the identifier directly; for the
            // method shape we descend into the prev selector to find the
            // member identifier.
            let has_args = children(node).any(|c| c.kind() == "argument_part");
            if !has_args {
                return;
            }
            let Some(prev) = node.prev_sibling() else {
                return;
            };
            let (callee_text, callee_line, receiver_type_hint) = match prev.kind() {
                "identifier" => (
                    node_text(prev, source),
                    prev.start_position().row as u32 + 1,
                    None,
                ),
                "selector" => {
                    let Some(member) = children(prev).find_map(|c| {
                        if c.kind() == "unconditional_assignable_selector"
                            || c.kind() == "conditional_assignable_selector"
                        {
                            children(c).find(|g| g.kind() == "identifier")
                        } else {
                            None
                        }
                    }) else {
                        return;
                    };
                    // Receiver hint: walk back one more sibling past the
                    // dot-selector. For `foo.method()` that sibling is the
                    // bare `identifier "foo"`. Skip cases where the receiver
                    // is anything else (a `this`, another selector chain,
                    // parenthesized expression).
                    let hint = prev.prev_sibling().and_then(|recv| {
                        if recv.kind() == "identifier" {
                            let name = node_text(recv, source);
                            env.and_then(|e| e.get(&name)).cloned()
                        } else {
                            None
                        }
                    });
                    (
                        node_text(member, source),
                        member.start_position().row as u32 + 1,
                        hint,
                    )
                }
                _ => return,
            };
            if callee_text.is_empty() {
                return;
            }
            references.push(ExtractedReference {
                name: callee_text,
                line: callee_line,
                from_symbol_idx: enclosing,
                kind: ReferenceKind::Call,
                qualifier: None,
                receiver_type_hint,
                via_method_syntax: false,
            });
        }
        // Type positions: parameter annotations, field/variable types,
        // return types. The grammar uses `type_identifier` only here;
        // declaration headers (`class Foo`) use plain `identifier`, so no
        // self-reference filtering is needed.
        "type_identifier" => {
            let name = node_text(node, source);
            if !name.is_empty() && !is_dart_builtin_type(&name) {
                references.push(ExtractedReference {
                    name,
                    line,
                    from_symbol_idx: enclosing,
                    kind: ReferenceKind::TypeRef,
                    qualifier: None,
                    receiver_type_hint: None,
                    via_method_syntax: false,
                });
            }
        }
        _ => {}
    }
}

/// Collect `field_name -> type_name` entries from a class/mixin/extension
/// body. Walks each `class_member`'s leading `declaration` subtree; a field
/// is a declaration whose first `type_identifier` child (non-builtin) is
/// followed by an `initialized_identifier_list`. Methods are skipped because
/// their `declaration` carries a `function_signature` / `method_signature`
/// instead of a typed identifier list.
fn collect_class_field_types(class_node: Node, source: &[u8], env: &mut ScopeEnv) {
    let Some(body) =
        children(class_node).find(|c| c.kind() == "class_body" || c.kind() == "extension_body")
    else {
        return;
    };
    for member in children(body) {
        if member.kind() != "class_member" {
            continue;
        }
        // A field member wraps a `declaration` whose sibling is `;`. Method
        // members wrap a `method_signature`. Skip anything without a bare
        // declaration.
        let Some(decl) = children(member).find(|c| c.kind() == "declaration") else {
            continue;
        };
        // Method declarations nest a `function_signature`/`getter_signature`/etc.
        // Field declarations do not.
        let has_sig = children(decl).any(|c| {
            matches!(
                c.kind(),
                "function_signature"
                    | "method_signature"
                    | "getter_signature"
                    | "setter_signature"
                    | "constructor_signature"
                    | "factory_constructor_signature"
            )
        });
        if has_sig {
            continue;
        }
        let type_name = children(decl)
            .find(|c| c.kind() == "type_identifier")
            .map(|c| node_text(c, source));
        let Some(type_name) = type_name else { continue };
        if type_name.is_empty() || is_dart_builtin_type(&type_name) {
            continue;
        }
        for list in children(decl) {
            let list_kind = list.kind();
            if list_kind != "initialized_identifier_list"
                && list_kind != "static_final_declaration_list"
            {
                continue;
            }
            for init in children(list) {
                let ident = match init.kind() {
                    "initialized_identifier" => children(init).find(|c| c.kind() == "identifier"),
                    "static_final_declaration" => children(init).find(|c| c.kind() == "identifier"),
                    _ => None,
                };
                if let Some(id) = ident {
                    let name = node_text(id, source);
                    if !name.is_empty() {
                        env.insert(name, type_name.clone());
                    }
                }
            }
        }
    }
}

/// Walk a `formal_parameter_list` (or any node containing one) and insert
/// `param_name -> type_name` entries. Dart's tree-sitter emits
/// `formal_parameter` with a direct `type_identifier` child (the annotation)
/// and an `identifier` child (the parameter name). Untyped parameters and
/// `this.x` constructor params are silently skipped.
fn collect_param_types(sig_node: Node, source: &[u8], env: &mut ScopeEnv) {
    let mut cursor = sig_node.walk();
    let mut stack: Vec<Node> = vec![sig_node];
    while let Some(n) = stack.pop() {
        if n.kind() == "formal_parameter" {
            let type_name = children(n)
                .find(|c| c.kind() == "type_identifier")
                .map(|c| node_text(c, source));
            let var_name = children(n)
                .find(|c| c.kind() == "identifier")
                .map(|c| node_text(c, source));
            if let (Some(t), Some(v)) = (type_name, var_name)
                && !t.is_empty()
                && !v.is_empty()
                && !is_dart_builtin_type(&t)
            {
                env.insert(v, t);
            }
            continue;
        }
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}

/// Walk a function body and insert `local_name -> type_name` for typed locals
/// declared via `final T x = ...;`, `T x = ...;`, or `const T x = ...;`. The
/// relevant AST shape is `initialized_variable_definition` with direct
/// `type_identifier` and `identifier` children.
fn collect_local_types(body: Node, source: &[u8], env: &mut ScopeEnv) {
    let mut cursor = body.walk();
    let mut stack: Vec<Node> = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "initialized_variable_definition" {
            let type_name = children(n)
                .find(|c| c.kind() == "type_identifier")
                .map(|c| node_text(c, source));
            let var_name = children(n)
                .find(|c| c.kind() == "identifier")
                .map(|c| node_text(c, source));
            if let (Some(t), Some(v)) = (type_name, var_name)
                && !t.is_empty()
                && !v.is_empty()
                && !is_dart_builtin_type(&t)
            {
                env.insert(v, t);
            }
            // Still descend: nested closures may declare further typed
            // locals we want to pick up.
        }
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}

fn is_dart_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "double"
            | "num"
            | "bool"
            | "String"
            | "void"
            | "dynamic"
            | "Object"
            | "Null"
            | "Never"
            | "List"
            | "Map"
            | "Set"
            | "Iterable"
            | "Future"
            | "Stream"
            | "Function"
            | "Symbol"
            | "Type"
            | "Record"
            | "Enum"
            | "Comparable"
            | "DateTime"
            | "Duration"
            | "RegExp"
            | "Uri"
            | "BigInt"
            | "StackTrace"
            | "Error"
            | "Exception"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_dart(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_dart::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = DartSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_class_definition() {
        let result = parse_dart("class MyService { }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MyService");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_abstract_class() {
        let result = parse_dart("abstract class Animal {\n  void speak();\n}");
        let classes: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Animal");
        assert!(classes[0].is_exported);
    }

    #[test]
    fn test_enum_definition() {
        let result = parse_dart("enum Color { red, green, blue }");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Color");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Enum));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_top_level_function() {
        let result = parse_dart("void greet(String name) {\n  print(name);\n}");
        let funcs: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function))
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "greet");
        assert!(funcs[0].is_exported);
    }

    #[test]
    fn test_private_function() {
        let result = parse_dart("String _privateHelper() {\n  return 'secret';\n}");
        let funcs: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function))
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "_privateHelper");
        assert!(!funcs[0].is_exported);
    }

    #[test]
    fn test_import_statement() {
        let result = parse_dart("import 'package:flutter/material.dart';");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "package:flutter/material.dart");
        assert!(!result.imports[0].is_reexport);
    }

    #[test]
    fn test_export_directive_is_tracked_as_reexport() {
        // Barrel libraries re-export internal files so downstream importers
        // of the barrel transitively reach them. The edge must be recorded,
        // and is_reexport must be true so consumers can tell it apart from a
        // real `import`.
        let result = parse_dart(
            r#"library arrow_swe;

export 'src/swe_facade.dart';
export 'src/eph_snapshot.dart';
"#,
        );
        assert_eq!(result.imports.len(), 2, "two export edges expected");
        assert!(
            result.imports.iter().all(|i| i.is_reexport),
            "export directives must set is_reexport=true, got {:?}",
            result.imports
        );
        let sources: Vec<&str> = result.imports.iter().map(|i| i.source.as_str()).collect();
        assert!(sources.contains(&"src/swe_facade.dart"));
        assert!(sources.contains(&"src/eph_snapshot.dart"));
    }

    #[test]
    fn test_mixin_definition() {
        let result = parse_dart("mixin Swimming {\n  void swim() {}\n}");
        let mixins: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Trait))
            .collect();
        assert_eq!(mixins.len(), 1);
        assert_eq!(mixins[0].name, "Swimming");
        assert!(mixins[0].is_exported);
    }

    #[test]
    fn test_empty_file() {
        let result = parse_dart("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_mixed_dart() {
        let result = parse_dart(
            r#"import 'package:flutter/material.dart';
import 'dart:async';
part 'model.dart';

abstract class Animal {
  void speak();
}

class Dog extends Animal {
  final String name;
  Dog(this.name);
  void speak() {}
  void _helper() {}
}

mixin Swimming {
  void swim() {}
}

enum Color { red, green, blue }

extension StringExt on String {
  bool get isBlank => trim().isEmpty;
}

typedef IntList = List<int>;

void greet(String name) {
  print(name);
}

String _privateHelper() {
  return 'secret';
}

const maxRetries = 3;
final appName = 'MyApp';
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Animal"), "missing Animal, got {names:?}");
        assert!(names.contains(&"Dog"), "missing Dog, got {names:?}");
        assert!(
            names.contains(&"Swimming"),
            "missing Swimming, got {names:?}"
        );
        assert!(names.contains(&"Color"), "missing Color, got {names:?}");
        assert!(
            names.contains(&"StringExt"),
            "missing StringExt, got {names:?}"
        );
        assert!(names.contains(&"IntList"), "missing IntList, got {names:?}");
        assert!(names.contains(&"greet"), "missing greet, got {names:?}");
        assert!(
            names.contains(&"_privateHelper"),
            "missing _privateHelper, got {names:?}"
        );
        assert!(
            names.contains(&"maxRetries"),
            "missing maxRetries, got {names:?}"
        );
        assert!(names.contains(&"appName"), "missing appName, got {names:?}");

        let private_fn = result
            .symbols
            .iter()
            .find(|s| s.name == "_privateHelper")
            .unwrap();
        assert!(!private_fn.is_exported);

        let swimming = result
            .symbols
            .iter()
            .find(|s| s.name == "Swimming")
            .unwrap();
        assert!(matches!(swimming.kind, SymbolKind::Trait));

        let max_retries = result
            .symbols
            .iter()
            .find(|s| s.name == "maxRetries")
            .unwrap();
        assert!(matches!(max_retries.kind, SymbolKind::Const));

        let app_name = result.symbols.iter().find(|s| s.name == "appName").unwrap();
        assert!(matches!(app_name.kind, SymbolKind::Variable));

        let int_list = result.symbols.iter().find(|s| s.name == "IntList").unwrap();
        assert!(matches!(int_list.kind, SymbolKind::Type));

        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method) && s.parent_idx.is_some())
            .collect();
        assert!(
            methods.iter().any(|m| m.name == "speak"),
            "missing method speak, got {methods:?}"
        );

        assert_eq!(result.imports.len(), 3);
        let part = result
            .imports
            .iter()
            .find(|i| i.source == "model.dart")
            .unwrap();
        assert!(part.is_reexport);
    }

    #[test]
    #[ignore = "debug aid - dumps AST"]
    fn _dump_ast_for_typed_scope() {
        let src = r#"
class Foo { void doit() {} }
class Bar {
  final Foo foo;
  Bar(this.foo);
  void run(Foo other) {
    foo.doit();
    other.doit();
    final Foo local = Foo();
    local.doit();
  }
}
"#;
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_dart::LANGUAGE))
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        fn w(n: Node, src: &[u8], d: usize) {
            let t: String = std::str::from_utf8(&src[n.start_byte()..n.end_byte().min(src.len())])
                .unwrap_or("")
                .chars()
                .take(60)
                .collect();
            eprintln!("{}{} {:?}", "  ".repeat(d), n.kind(), t);
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                w(ch, src, d + 1);
            }
        }
        w(tree.root_node(), src.as_bytes(), 0);
    }

    #[test]
    #[ignore = "debug aid - dumps AST"]
    fn _dump_ast_for_calls() {
        let src = r#"
void setUp() {
  facade = SweFacade(swe, ephePath: ephePath);
  obj.method(arg);
  Body.Sun;
}
"#;
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_dart::LANGUAGE))
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        fn walk(n: Node, src: &[u8], d: usize) {
            let t: String = std::str::from_utf8(&src[n.start_byte()..n.end_byte().min(src.len())])
                .unwrap_or("")
                .chars()
                .take(40)
                .collect();
            eprintln!("{}{} {:?}", "  ".repeat(d), n.kind(), t);
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                walk(ch, src, d + 1);
            }
        }
        walk(tree.root_node(), src.as_bytes(), 0);
    }

    #[test]
    fn extracts_call_references() {
        let source = r#"
void helper() {}

void main() {
  helper();
  print('hi');
  facade = SweFacade(swe);
  obj.method(arg);
}
"#;
        let result = parse_dart(source);
        let calls: Vec<&str> = result
            .references
            .iter()
            .filter(|r| matches!(r.kind, ReferenceKind::Call))
            .map(|r| r.name.as_str())
            .collect();
        assert!(
            calls.contains(&"helper"),
            "expected `helper` call ref, got {calls:?}"
        );
        assert!(
            calls.contains(&"print"),
            "expected `print` call ref, got {calls:?}"
        );
        // Constructor-style call inside an assignment must be picked up.
        assert!(
            calls.contains(&"SweFacade"),
            "expected `SweFacade` call ref (constructor in assignment), got {calls:?}"
        );
        // Method call `obj.method(arg)` - the method name (not the receiver)
        // is the call target.
        assert!(
            calls.contains(&"method"),
            "expected `method` call ref (method invocation), got {calls:?}"
        );
    }

    #[test]
    fn extracts_type_references() {
        let source = r#"
class Greeter {
  final Duration delay;
  Greeter(this.delay);
  DateTime now() => DateTime.now();
}
"#;
        let result = parse_dart(source);
        let type_refs: Vec<&str> = result
            .references
            .iter()
            .filter(|r| matches!(r.kind, ReferenceKind::TypeRef))
            .map(|r| r.name.as_str())
            .collect();
        // Duration and DateTime are Dart builtins - they must be filtered.
        assert!(
            !type_refs.contains(&"Duration"),
            "builtin Duration should not be a type ref, got {type_refs:?}"
        );
        assert!(
            !type_refs.contains(&"DateTime"),
            "builtin DateTime should not be a type ref, got {type_refs:?}"
        );
        // Greeter is the declared class itself - its own header must not
        // produce a self-reference.
        let self_refs: Vec<_> = result
            .references
            .iter()
            .filter(|r| r.name == "Greeter")
            .collect();
        assert!(
            self_refs.is_empty(),
            "declared class name must not appear as its own reference, got {self_refs:?}"
        );
    }

    #[test]
    fn extracts_user_type_references() {
        let source = r#"
class Animal {}

class Dog {
  final Animal parent;
  Dog(this.parent);
}

Dog adopt(Animal a) => Dog(a);
"#;
        let result = parse_dart(source);
        let type_refs: Vec<&str> = result
            .references
            .iter()
            .filter(|r| matches!(r.kind, ReferenceKind::TypeRef))
            .map(|r| r.name.as_str())
            .collect();
        assert!(
            type_refs.contains(&"Animal"),
            "expected `Animal` type ref, got {type_refs:?}"
        );
    }

    #[test]
    fn attributes_method_body_calls_to_method() {
        let source = r#"
void helper() {}

class Worker {
  void run() {
    helper();
  }
}
"#;
        let result = parse_dart(source);
        let run_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "run")
            .expect("run method exists");
        let helper_call = result
            .references
            .iter()
            .find(|r| r.name == "helper" && matches!(r.kind, ReferenceKind::Call))
            .expect("helper call ref exists");
        assert_eq!(
            helper_call.from_symbol_idx,
            Some(run_idx),
            "helper() inside Worker.run should attribute to run, not Worker"
        );
    }

    #[test]
    fn cross_class_method_call_attributes_to_caller_method() {
        let source = r#"
class A {
  void foo() {}
}

class B {
  void bar() {
    foo();
  }
}
"#;
        let result = parse_dart(source);
        let bar_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "bar")
            .expect("bar method exists");
        let b_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "B")
            .expect("B class exists");
        let foo_call = result
            .references
            .iter()
            .find(|r| r.name == "foo" && matches!(r.kind, ReferenceKind::Call))
            .expect("foo call ref exists");
        assert_eq!(
            foo_call.from_symbol_idx,
            Some(bar_idx),
            "foo() inside B.bar should attribute to bar (not B)"
        );
        assert_ne!(foo_call.from_symbol_idx, Some(b_idx));
    }

    #[test]
    fn class_header_type_refs_attribute_to_class() {
        let source = r#"
abstract class Animal {}

class Dog extends Animal {
  void bark() {}
}
"#;
        let result = parse_dart(source);
        let dog_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "Dog")
            .expect("Dog class exists");
        let animal_ref = result
            .references
            .iter()
            .find(|r| r.name == "Animal" && matches!(r.kind, ReferenceKind::TypeRef))
            .expect("Animal type ref exists");
        assert_eq!(
            animal_ref.from_symbol_idx,
            Some(dog_idx),
            "Animal in `extends Animal` should attribute to Dog (class header)"
        );
    }

    fn call_ref<'a>(result: &'a ParseResult, name: &str) -> &'a ExtractedReference {
        result
            .references
            .iter()
            .find(|r| r.name == name && matches!(r.kind, ReferenceKind::Call))
            .unwrap_or_else(|| panic!("expected call ref to `{name}`"))
    }

    #[test]
    fn typed_local_populates_receiver_hint() {
        let source = r#"
class Foo { void doit() {} }

void run() {
  final Foo local = Foo();
  local.doit();
}
"#;
        let result = parse_dart(source);
        let call = call_ref(&result, "doit");
        assert_eq!(
            call.receiver_type_hint.as_deref(),
            Some("Foo"),
            "local.doit() should carry Foo receiver hint"
        );
    }

    #[test]
    fn typed_parameter_populates_receiver_hint() {
        let source = r#"
class Foo { void doit() {} }

void run(Foo other) {
  other.doit();
}
"#;
        let result = parse_dart(source);
        let call = call_ref(&result, "doit");
        assert_eq!(
            call.receiver_type_hint.as_deref(),
            Some("Foo"),
            "parameter `Foo other` should carry Foo receiver hint at other.doit()"
        );
    }

    #[test]
    fn typed_field_populates_receiver_hint() {
        let source = r#"
class Foo { void doit() {} }

class Bar {
  final Foo foo;
  Bar(this.foo);
  void run() {
    foo.doit();
  }
}
"#;
        let result = parse_dart(source);
        let call = call_ref(&result, "doit");
        assert_eq!(
            call.receiver_type_hint.as_deref(),
            Some("Foo"),
            "class field `Foo foo` should carry Foo hint at foo.doit()"
        );
    }

    #[test]
    fn untyped_receiver_leaves_hint_none() {
        let source = r#"
class Foo { void doit() {} }

void run() {
  var x = Foo();
  x.doit();
}
"#;
        let result = parse_dart(source);
        let call = call_ref(&result, "doit");
        assert!(
            call.receiver_type_hint.is_none(),
            "untyped `var x` must not produce a receiver hint, got {:?}",
            call.receiver_type_hint
        );
    }

    #[test]
    fn bare_call_has_no_receiver_hint() {
        let source = r#"
void helper() {}

void run() {
  helper();
}
"#;
        let result = parse_dart(source);
        let call = call_ref(&result, "helper");
        assert!(
            call.receiver_type_hint.is_none(),
            "bare calls have no receiver; hint must stay None"
        );
    }

    #[test]
    fn builtin_typed_receiver_is_skipped() {
        // `String` is filtered by is_dart_builtin_type; even though we know
        // the type, emitting a hint for it is pointless: there is no
        // user-defined symbol named `String` in the project index.
        let source = r#"
void run(String s) {
  s.trim();
}
"#;
        let result = parse_dart(source);
        let call = call_ref(&result, "trim");
        assert!(
            call.receiver_type_hint.is_none(),
            "builtin types must not emit hints"
        );
    }
}
