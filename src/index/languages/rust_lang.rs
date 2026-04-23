use tree_sitter::{Language, Node};

use super::LanguageSupport;
use super::common::{self, children, node_text};
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedRelation, ExtractedSymbol, ParseResult,
    ReferenceKind, RelationKind, SymbolKind,
};

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

fn has_pub_visibility(node: Node) -> bool {
    children(node).any(|child| child.kind() == "visibility_modifier")
}

/// True when the preceding attribute chain contains `#[name(...)]` or
/// `#[name]`. Attributes sit as `prev_sibling` nodes of the item they
/// annotate in tree-sitter-rust; `preceding_cfg_test_attr_row` in the
/// security scanner walks the same chain. Line comments between an
/// attribute and its item are skipped so they don't break the match.
fn has_preceding_attribute_named(node: Node, source: &[u8], attr_name: &str) -> bool {
    let mut sib = node.prev_sibling();
    while let Some(s) = sib {
        match s.kind() {
            "attribute_item" | "inner_attribute_item" => {
                if attribute_path_head(s, source).as_deref() == Some(attr_name) {
                    return true;
                }
                sib = s.prev_sibling();
            }
            "line_comment" | "block_comment" => {
                sib = s.prev_sibling();
            }
            _ => break,
        }
    }
    false
}

/// Return the first identifier of an attribute, e.g. `"tool"` for
/// `#[tool(name = "foo")]`, `"tool_router"` for `#[tool_router(...)]`.
/// None if the attribute does not contain a recognizable path.
fn attribute_path_head(attr_item: Node, source: &[u8]) -> Option<String> {
    let attribute = children(attr_item).find(|c| c.kind() == "attribute")?;
    let head = children(attribute).find(|c| {
        matches!(
            c.kind(),
            "identifier" | "scoped_identifier" | "scoped_type_identifier"
        )
    })?;
    let name = match head.kind() {
        "scoped_identifier" | "scoped_type_identifier" => head
            .child_by_field_name("name")
            .map(|n| node_text(n, source))
            .unwrap_or_default(),
        _ => node_text(head, source),
    };
    if name.is_empty() { None } else { Some(name) }
}

/// rmcp's proc macros generate a parallel dispatch surface
/// (`<name>_router()`) that the static reference graph cannot see. Any
/// item bearing `#[tool(...)]` or living inside an `#[tool_router]` impl
/// block is reached only via that generated router, so `qartez_unused`
/// must treat them as excluded from the "no importers" dead-code check.
const MCP_TOOL_EXCLUSION_ATTRS: &[&str] = &["tool", "tool_router"];

fn is_mcp_tool_item(node: Node, source: &[u8]) -> bool {
    MCP_TOOL_EXCLUSION_ATTRS
        .iter()
        .any(|n| has_preceding_attribute_named(node, source, n))
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
                let idx = symbols.len();
                symbols.push(sym);
                // References inside the initializer (e.g. `const REG: [&dyn T;
                // N] = [&bash::BashSupport, &rust::RustSupport, ...];`) belong
                // to the const itself. Without setting `new_enclosing` the
                // initializer's refs would be attributed to the outer module,
                // which has no symbol id, and the resolver would drop them as
                // `no-enclosing`. That is why `qartez_refs` reported zero
                // references for every `LanguageSupport` struct in the 37-way
                // dispatch table.
                new_enclosing = Some(idx);
            }
        }
        "static_item" => {
            if let Some(sym) = extract_named_item(node, source, SymbolKind::Variable) {
                let idx = symbols.len();
                symbols.push(sym);
                new_enclosing = Some(idx);
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
                let (name, qualifier, module_qual) = extract_callee_info(func, source);
                // Method-call syntax (`receiver.method(...)`) has a
                // `field_expression` as the callee. The extractor cannot
                // infer the receiver's type statically, so the resolver
                // must treat these refs more carefully - otherwise common
                // iterator / Option / Result method names (`filter`,
                // `map`, `collect`) get fanned out to every same-named
                // field and free function in the index. `via_method_syntax`
                // is the signal the resolver uses to drop cross-file
                // ambiguity for these refs.
                let via_method_syntax = func.kind() == "field_expression";
                if !name.is_empty() {
                    references.push(ExtractedReference {
                        name,
                        line,
                        from_symbol_idx: enclosing,
                        kind: ReferenceKind::Call,
                        qualifier: qualifier.clone(),
                        receiver_type_hint: None,
                        via_method_syntax,
                    });
                    // Attribute a TypeRef to the immediate qualifier when
                    // it's a type (`QartezError` in `QartezError::Io(...)`,
                    // `Cli` in `cli::Cli::parse()`, `Foo` in `Foo::new()`).
                    // Without this, enums/structs reached only through
                    // scoped-path calls show zero refs even when used in
                    // dozens of places. The module segment (`cli` in
                    // `cli::Cli::parse`) rides along as the TypeRef's
                    // qualifier so the resolver can pick the struct in the
                    // file whose stem matches - essential when several bins
                    // define a struct named `Cli`.
                    //
                    // Lowercase qualifiers (module names like `typescript`
                    // in `typescript::maybe_profile()`) are NOT emitted as
                    // TypeRefs because modules aren't symbols; they're used
                    // solely for file-stem matching on the primary Call
                    // reference.
                    if let Some(q) = qualifier
                        && q.starts_with(|c: char| c.is_uppercase())
                        && !is_builtin_type(&q)
                    {
                        references.push(ExtractedReference {
                            name: q,
                            line,
                            from_symbol_idx: enclosing,
                            kind: ReferenceKind::TypeRef,
                            qualifier: module_qual,
                            receiver_type_hint: None,
                            via_method_syntax: false,
                        });
                    }
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
                if name.is_empty() {
                    return;
                }
                let is_builtin = is_builtin_macro(&name);
                if !is_builtin {
                    references.push(ExtractedReference {
                        name,
                        line,
                        from_symbol_idx: enclosing,
                        kind: ReferenceKind::Call,
                        qualifier: None,
                        receiver_type_hint: None,
                        via_method_syntax: false,
                    });
                }
                // Macro bodies are opaque token trees - tree-sitter cannot
                // resolve identifier meaning without macro expansion. We
                // still walk the tokens to recover:
                //  * uppercase-leading identifiers / scoped paths (types,
                //    consts) via low-confidence Use refs - useful for
                //    proc-macro DSLs like `tool_router! { ... }`.
                //  * `ident(...)` / `Type::method(...)` shapes as Call refs
                //    - needed because format-family macros routinely wrap
                //      method chains like `f.severity.label()`, and without
                //      this the callee looks dead.
                // For builtin macros (format!, println!, etc.) we emit ONLY
                // the call pattern - the standalone Use arm would turn
                // `format!("{}", SomeType::NAME)` into type-churn noise.
                let emit_mode = if is_builtin {
                    MacroEmitMode::CallsOnly
                } else {
                    MacroEmitMode::Full
                };
                if let Some(tt) = node.child_by_field_name("token_tree") {
                    emit_macro_body_refs(tt, source, enclosing, references, emit_mode);
                } else {
                    // Some macro shapes attach the body as an unnamed
                    // `token_tree` child rather than via the named field.
                    for child in children(node) {
                        if child.kind() == "token_tree" {
                            emit_macro_body_refs(child, source, enclosing, references, emit_mode);
                        }
                    }
                }
            }
        }
        "attribute_item" => {
            // Serde's `deserialize_with = "path::to::fn"` hides a function
            // reference inside a string literal. Without special-casing,
            // `qartez_refs` shows zero usages for every helper in
            // `flexible::*` even though they're wired into dozens of
            // structs. Restricted to attributes whose path starts with
            // `serde` so non-serde DSLs that reuse `with = "..."` keep
            // their semantics unchanged.
            extract_serde_path_refs(node, source, enclosing, references);
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
                    via_method_syntax: false,
                });
            }
        }
        "identifier" => {
            // UPPER_SNAKE_CASE identifiers in expression position are const
            // or static references by convention (`MISC_CLUSTER_ID`,
            // `EMBEDDING_DIM`, `DEFAULT_PAGERANK_MIN`). Without this arm
            // a bare const use emits no reference and the defining const
            // shows 0 refs in `qartez_refs` / gets flagged by
            // `qartez_unused` despite heavy use.
            //
            // Lowercase identifiers are emitted ONLY when passed as a
            // callback / function pointer to another call
            // (`.map(expand_kind_alias)`, `foo(helper)`). Without this,
            // intra-file `pub(super)` helpers that are only referenced
            // via callback syntax look dead because the argument-
            // position identifier produces no reference at all. Free-
            // standing lowercase reads (`let x = foo; foo`) stay
            // unemitted so the signal-to-noise stays usable; the
            // resolver drops the rare false positive where the argument
            // is actually a local binding rather than a symbol name
            // (no candidate is indexed, so the edge never materialises).
            // CamelCase is reached via `type_identifier` and doesn't
            // need a fallback here.
            //
            // The parent-kind filter skips identifiers that are
            // self-definitions or binding names (so `const FOO:` /
            // `let FOO = ...` patterns / function parameters don't
            // self-reference).
            let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
            if matches!(
                parent_kind,
                "scoped_identifier"
                    | "scoped_type_identifier"
                    | "scoped_use_list"
                    | "use_declaration"
                    | "use_as_clause"
                    | "use_list"
                    | "let_declaration"
                    | "parameter"
                    | "mut_pattern"
                    | "reference_pattern"
                    | "tuple_pattern"
                    | "tuple_struct_pattern"
                    | "captured_pattern"
                    | "for_expression"
                    | "closure_parameters"
                    | "macro_invocation"
                    | "meta_item"
                    | "field_declaration"
                    | "enum_variant"
                    | "function_item"
                    | "const_item"
                    | "static_item"
                    | "mod_item"
            ) {
                return;
            }
            let name = node_text(node, source);
            if name.is_empty() {
                return;
            }
            let is_const_shape = name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && name.chars().any(|c| c.is_ascii_uppercase());
            // Tree-sitter-rust wraps call arguments in an `arguments` node,
            // so a bare `identifier` inside `arguments` is either a local
            // read or a function pointer passed as a callback. Emitting a
            // Use ref here lets the resolver match it against an indexed
            // symbol when one exists; locals get dropped as `no candidate`.
            let in_call_arguments = parent_kind == "arguments";
            if !is_const_shape && !in_call_arguments {
                return;
            }
            references.push(ExtractedReference {
                name,
                line,
                from_symbol_idx: enclosing,
                kind: ReferenceKind::Use,
                qualifier: None,
                receiver_type_hint: None,
                via_method_syntax: false,
            });
        }
        "scoped_identifier" => {
            // `scoped_identifier` in VALUE position: `&bash::BashSupport`,
            // `module::CONST`, `module::function_alias`. Type-position paths
            // (`-> bar::Baz`, `let x: bar::Baz`) parse as `scoped_type_identifier`
            // and surface through the `type_identifier` arm above, so we need
            // this arm only for expression-context paths.
            //
            // Skip when this node is the `function` child of a `call_expression`
            // (handled by the `call_expression` arm above, which attaches a
            // `qualifier`) or a path *segment* inside a larger `scoped_identifier`
            // / `scoped_type_identifier`, or the qualifier of a struct literal
            // name that `type_identifier` already covered via its last segment.
            let parent = node.parent();
            let parent_kind = parent.map(|p| p.kind()).unwrap_or("");
            if matches!(
                parent_kind,
                "scoped_identifier"
                    | "scoped_type_identifier"
                    | "scoped_use_list"
                    | "use_declaration"
                    | "use_as_clause"
                    | "use_list"
                    | "generic_function"
            ) {
                return;
            }
            if parent_kind == "call_expression" {
                // A `scoped_identifier` as the function of a call_expression
                // is the callee - the `call_expression` arm above records it
                // with a `Call` kind and a qualifier. Skipping here avoids
                // emitting a duplicate `Use` edge for the same position.
                if let Some(p) = parent
                    && p.child_by_field_name("function").map(|f| f.id()) == Some(node.id())
                {
                    return;
                }
            }
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source))
                .unwrap_or_default();
            if name.is_empty() || is_builtin_type(&name) {
                return;
            }
            // Value-context qualifier: keep the full last-segment text even
            // when it starts with lowercase (module names, crate names). In
            // call context the qualifier must be a type for
            // `Foo::method`-style dispatch; here the qualifier is just a
            // locator for the defining module, which is typically lowercase.
            let (qualifier, module_qual) = extract_scoped_path_parts(node, source);
            references.push(ExtractedReference {
                name,
                line,
                from_symbol_idx: enclosing,
                kind: ReferenceKind::Use,
                qualifier: qualifier.clone(),
                receiver_type_hint: None,
                via_method_syntax: false,
            });
            // If the immediate qualifier is uppercase, it names an enum or
            // struct being referenced via variant/associated-item syntax
            // (`QartezError::Io`, `Foo::CONST`). Emit a TypeRef to it so
            // the defining type gets credit instead of only the trailing
            // variant/const name (which is unresolvable when the variant
            // isn't indexed as its own symbol).
            if let Some(q) = qualifier
                && q.starts_with(|c: char| c.is_uppercase())
                && !is_builtin_type(&q)
            {
                references.push(ExtractedReference {
                    name: q,
                    line,
                    from_symbol_idx: enclosing,
                    kind: ReferenceKind::TypeRef,
                    qualifier: module_qual,
                    receiver_type_hint: None,
                    via_method_syntax: false,
                });
            }
        }
        _ => {}
    }
}

/// Walk a macro invocation's `token_tree`. Two emission modes are
/// supported because tree-sitter-rust does not recurse into macro
/// bodies - every identifier is a flat token, and we have to pattern-
/// match call shapes ourselves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MacroEmitMode {
    /// User-written macro: emit both standalone Use refs for uppercase
    /// tokens (types / consts referenced inside proc-macro DSLs) and
    /// Call refs for identifier-followed-by-paren patterns.
    Full,
    /// Builtin formatting macro (`format!`, `println!`, ...): emit ONLY
    /// Call refs. Uppercase standalone Use refs here would flood the
    /// graph with noise - format args routinely name types without
    /// referencing them in a way that should count for PageRank.
    CallsOnly,
}

fn emit_macro_body_refs(
    token_tree: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
    mode: MacroEmitMode,
) {
    let mut stack = vec![token_tree];
    while let Some(node) = stack.pop() {
        // Materialize children so we can peek the next sibling. Macro bodies
        // are flat token streams after tree-sitter-rust refuses to recurse
        // into them, so pattern detection has to happen here rather than
        // through the regular AST walker. We look for:
        //   ident (                     -> free-function or method-chain call
        //   scoped_ident (               -> scoped call, last segment is name
        // The preceding-dot check tells us the call is the tail of a method
        // chain like `f.severity.label()` - important because before this
        // fix every such callee was invisible and the target method looked
        // dead despite being invoked dozens of times inside `format!(...)`.
        let kids: Vec<Node> = children(node).collect();
        for (i, child) in kids.iter().enumerate() {
            match child.kind() {
                // Nested delimiter groups - recurse to scan their tokens too.
                "token_tree" => stack.push(*child),
                "identifier" | "type_identifier" => {
                    let followed_by_call = kids.get(i + 1).is_some_and(|n| is_paren_token_tree(*n));
                    let name = node_text(*child, source);
                    if name.is_empty() {
                        continue;
                    }
                    if followed_by_call {
                        // `ident(...)` inside a macro body - a function or
                        // method call. Emit a Call ref regardless of case
                        // because lowercase callables (method names, free
                        // functions) are exactly the class that would
                        // otherwise be missed.
                        let line = child.start_position().row as u32 + 1;
                        references.push(ExtractedReference {
                            name,
                            line,
                            from_symbol_idx: enclosing,
                            kind: ReferenceKind::Call,
                            qualifier: None,
                            receiver_type_hint: None,
                            via_method_syntax: false,
                        });
                    } else if mode == MacroEmitMode::Full
                        && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                        && !is_builtin_type(&name)
                    {
                        let line = child.start_position().row as u32 + 1;
                        references.push(ExtractedReference {
                            name,
                            line,
                            from_symbol_idx: enclosing,
                            kind: ReferenceKind::Use,
                            qualifier: None,
                            receiver_type_hint: None,
                            via_method_syntax: false,
                        });
                    }
                }
                "scoped_identifier" | "scoped_type_identifier" => {
                    let Some(n) = child.child_by_field_name("name") else {
                        continue;
                    };
                    let name = node_text(n, source);
                    if name.is_empty() {
                        continue;
                    }
                    let followed_by_call = kids.get(i + 1).is_some_and(|n| is_paren_token_tree(*n));
                    if followed_by_call {
                        // `Foo::new(...)` inside a macro body. Emit a Call
                        // on the last segment so the callee gets credit,
                        // and also a TypeRef for the uppercase qualifier so
                        // the owning type is not flagged dead.
                        let line = child.start_position().row as u32 + 1;
                        let (qualifier, _) = extract_scoped_path_parts(*child, source);
                        references.push(ExtractedReference {
                            name: name.clone(),
                            line,
                            from_symbol_idx: enclosing,
                            kind: ReferenceKind::Call,
                            qualifier: qualifier.clone(),
                            receiver_type_hint: None,
                            via_method_syntax: false,
                        });
                        if let Some(q) = qualifier
                            && q.starts_with(|c: char| c.is_uppercase())
                            && !is_builtin_type(&q)
                        {
                            references.push(ExtractedReference {
                                name: q,
                                line,
                                from_symbol_idx: enclosing,
                                kind: ReferenceKind::TypeRef,
                                qualifier: None,
                                receiver_type_hint: None,
                                via_method_syntax: false,
                            });
                        }
                    } else if mode == MacroEmitMode::Full
                        && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                        && !is_builtin_type(&name)
                    {
                        let line = child.start_position().row as u32 + 1;
                        references.push(ExtractedReference {
                            name,
                            line,
                            from_symbol_idx: enclosing,
                            kind: ReferenceKind::Use,
                            qualifier: None,
                            receiver_type_hint: None,
                            via_method_syntax: false,
                        });
                    }
                }
                _ => {}
            }
        }
    }
}

fn is_paren_token_tree(node: Node) -> bool {
    // Token trees come in three delimiter shapes: `(...)`, `[...]`, `{...}`.
    // Only `(...)` signals a call; `vec![a, b]` and struct-update `{ .. }`
    // must not be misread as calls.
    if node.kind() != "token_tree" {
        return false;
    }
    node.child(0).is_some_and(|c| c.kind() == "(")
}

/// Extract serde `deserialize_with` / `serialize_with` / `with` string
/// paths from an `attribute_item` node and emit Use refs for the
/// function they point to. Only fires when the attribute's path starts
/// with `serde` - other DSLs that happen to use `with = "..."` are left
/// alone.
///
/// Tree-sitter-rust parses `#[serde(deserialize_with = "path::fn")]`
/// as `attribute_item` -> `attribute` -> `token_tree` -> chain of
/// `token_tree` / `identifier` / `=` / `string_literal`. We recognise
/// the relevant triple (`ident` `=` `string`) inside the token tree and
/// split the string on `::` to recover the function name.
fn extract_serde_path_refs(
    attr: Node,
    source: &[u8],
    enclosing: Option<usize>,
    references: &mut Vec<ExtractedReference>,
) {
    // Attribute shape: `attribute_item` -> `attribute` whose first
    // non-delimiter child is the path identifier and whose second is a
    // `token_tree` holding `(key = value, ...)` tokens. tree-sitter-rust
    // does not expose a named `path` field on `attribute`, so we match
    // positionally.
    let Some(attribute) = children(attr).find(|c| c.kind() == "attribute") else {
        return;
    };
    let path_text = children(attribute)
        .find(|c| matches!(c.kind(), "identifier" | "scoped_identifier"))
        .map(|n| match n.kind() {
            "scoped_identifier" => n
                .child_by_field_name("name")
                .map(|x| node_text(x, source))
                .unwrap_or_default(),
            _ => node_text(n, source),
        })
        .unwrap_or_default();
    if path_text != "serde" {
        return;
    }
    // Walk every descendant `token_tree` looking for the triple
    // `ident` `=` `string_literal` where ident is one of the serde
    // path-bearing keys.
    let mut stack: Vec<Node> = children(attribute)
        .filter(|c| c.kind() == "token_tree")
        .collect();
    while let Some(tt) = stack.pop() {
        let kids: Vec<Node> = children(tt).collect();
        for window in kids.windows(3) {
            let key = window[0];
            let eq = window[1];
            let value = window[2];
            if key.kind() != "identifier" || eq.kind() != "=" {
                continue;
            }
            if value.kind() != "string_literal" {
                continue;
            }
            let key_name = node_text(key, source);
            if !matches!(
                key_name.as_str(),
                "deserialize_with" | "serialize_with" | "with"
            ) {
                continue;
            }
            // Strip surrounding quotes; handle raw strings conservatively
            // by taking whatever is inside the outermost pair.
            let raw = node_text(value, source);
            let inner = raw.trim_matches('"');
            if inner.is_empty() {
                continue;
            }
            let tail = inner.rsplit("::").next().unwrap_or(inner);
            if tail.is_empty() {
                continue;
            }
            // Immediate module qualifier, if the path had segments.
            let qualifier = if inner.contains("::") {
                inner
                    .rsplit("::")
                    .nth(1)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            } else {
                None
            };
            let line = value.start_position().row as u32 + 1;
            references.push(ExtractedReference {
                name: tail.to_string(),
                line,
                from_symbol_idx: enclosing,
                kind: ReferenceKind::Use,
                qualifier,
                receiver_type_hint: None,
                via_method_syntax: false,
            });
        }
        // Recurse into nested token_trees.
        for child in kids {
            if child.kind() == "token_tree" {
                stack.push(child);
            }
        }
    }
}

/// Extract `(immediate_qualifier, module_qualifier)` from a
/// `scoped_identifier` node's `path` field. Used by both the
/// `scoped_identifier` arm of `record_reference` and `extract_callee_info`
/// so the two code paths interpret `cli::Cli::parse` the same way.
///
/// * `Foo::bar`              -> (Some("Foo"),          None)
/// * `QartezError::Io`       -> (Some("QartezError"),  None)
/// * `cli::Cli::parse`       -> (Some("Cli"),          Some("cli"))
/// * `crate::foo::Bar::new`  -> (Some("Bar"),          Some("foo"))
fn extract_scoped_path_parts(node: Node, source: &[u8]) -> (Option<String>, Option<String>) {
    let path = match node.child_by_field_name("path") {
        Some(p) => p,
        None => return (None, None),
    };
    if path.kind() == "scoped_identifier" {
        let immediate = path
            .child_by_field_name("name")
            .map(|n| node_text(n, source))
            .filter(|s| !s.is_empty());
        let module = path
            .child_by_field_name("path")
            .map(|pp| {
                if pp.kind() == "scoped_identifier" {
                    pp.child_by_field_name("name")
                        .map(|n| node_text(n, source))
                        .unwrap_or_default()
                } else {
                    node_text(pp, source)
                }
            })
            .filter(|s| !s.is_empty());
        (immediate, module)
    } else {
        let text = node_text(path, source);
        if text.is_empty() {
            (None, None)
        } else {
            (Some(text), None)
        }
    }
}

/// Given the `function` field of a `call_expression`, return the callee
/// name, an optional type qualifier (for `impl Foo { fn new() }`-style
/// dispatch), and an optional module qualifier (the segment before the
/// type, used to disambiguate same-named types across files whose stem
/// matches the module segment).
fn extract_callee_info(func: Node, source: &[u8]) -> (String, Option<String>, Option<String>) {
    match func.kind() {
        "identifier" => (node_text(func, source), None, None),
        "scoped_identifier" => {
            let name = func
                .child_by_field_name("name")
                .map(|n| node_text(n, source))
                .unwrap_or_default();
            // For `Foo::new()`, the path is `Foo` -> immediate = "Foo"
            //   (feeds owner_type match: impl Foo { fn new })
            // For `cli::Cli::parse()`, the path is `cli::Cli` ->
            //   immediate = "Cli", module = "cli"
            //   (immediate feeds owner_type match, module feeds file-stem
            //    match for picking the right Cli among bin crates).
            // For `typescript::maybe_profile()`, path = "typescript" ->
            //   immediate = "typescript", module = None
            //   (file-stem match picks typescript.rs's maybe_profile).
            //
            // The immediate qualifier is returned as-is: the resolver's
            // qualifier heuristic tries BOTH owner_type and file_stem
            // matching, so lowercase module names still resolve through
            // the file-stem path. Filtering out lowercase here would
            // discard the very signal needed to pick one of several
            // same-named functions defined in sibling module files.
            let (immediate, module_qual) = extract_scoped_path_parts(func, source);
            (name, immediate, module_qual)
        }
        "field_expression" => {
            let name = func
                .child_by_field_name("field")
                .map(|n| node_text(n, source))
                .unwrap_or_default();
            // For `self.foo()` or `x.foo()`, the qualifier is unknown at the
            // syntactic level. Receiver type tracking populates it later.
            (name, None, None)
        }
        "generic_function" => func
            .child_by_field_name("function")
            .map(|n| extract_callee_info(n, source))
            .unwrap_or_default(),
        _ => (String::new(), None, None),
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
        // `?` is technically an early-return branch under strict McCabe, but
        // counting it inflates CC on flat dispatchers where every helper
        // call ends in `?`. Those then look as bad as deeply-nested control
        // flow. clippy::cognitive_complexity excludes `?` for the same
        // reason. Real branching (`if`, `match`, loops, `&&`, `||`) is
        // still counted.
        "try_expression" => 0,
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
    // Top-level functions can also be wired to rmcp's macro-generated
    // router via `#[tool(...)]`. Apply the same exclusion rule as method
    // items inside a `#[tool_router]` impl block.
    let unused_excluded = matches!(kind, SymbolKind::Function) && is_mcp_tool_item(node, source);
    Some(ExtractedSymbol {
        name,
        kind,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: has_pub_visibility(node),
        parent_idx: None,
        unused_excluded,
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
        let signature = type_text.map(|t| format!("{name}: {t}"));
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
        let signature = Some(format!("{name}: {type_text}"));
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
    // `#[tool_router]` on the impl block generates a parallel dispatch
    // surface that the reference graph cannot see. Every method inside
    // such a block is reached only via that generated router, so flag
    // them all as `unused_excluded` up front - same rationale as trait
    // impl methods, different invisibility mechanism.
    let in_tool_router_impl = has_preceding_attribute_named(impl_node, source, "tool_router");

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
                // Methods gain `unused_excluded` from three independent
                // signals: dynamic dispatch through a trait impl, parallel
                // dispatch via `#[tool_router]` on the enclosing impl, or
                // `#[tool(...)]` on the method itself (rare but legal).
                let unused_excluded =
                    in_trait_impl || in_tool_router_impl || is_mcp_tool_item(child, source);
                symbols.push(ExtractedSymbol {
                    name,
                    kind: SymbolKind::Method,
                    line_start: child.start_position().row as u32 + 1,
                    line_end: child.end_position().row as u32 + 1,
                    signature: extract_signature(child, source),
                    is_exported: has_pub_visibility(child),
                    parent_idx: None,
                    unused_excluded,
                    complexity: Some(1 + method_cc),
                    owner_type: owner.clone(),
                });
                // Walk every child of the method - parameters, return_type,
                // where_clause, body - with the method as enclosing so every
                // call/type reference inside is attributed to it. Walking only
                // the body missed signature type references: e.g.
                // `Parameters<ToolsParams>` in the method's parameter list
                // would not emit a TypeRef to `ToolsParams` and the resolver
                // would drop it as module-scope, flagging `ToolsParams` as
                // dead despite the method using it.
                for grand in children(child) {
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
    common::brace_or_first_line_signature(node, source)
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
    fn test_refs_nested_field_expression_call() {
        // `f.severity.label()` - the method name lives two field hops away
        // from the receiver binding. Before this test, the parser emitted a
        // Call ref for the outer field_expression as expected, but we want
        // to pin the behavior against regressions in the callee walker.
        let result = parse_rust(
            r#"
pub enum Severity { Low }
impl Severity {
    pub fn label(&self) -> &'static str { "Low" }
}
pub struct Finding { pub severity: Severity }
fn emit(f: &Finding) { let _ = f.severity.label(); }
"#,
        );
        let call_refs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "label" && matches!(r.kind, ReferenceKind::Call))
            .collect();
        assert!(
            !call_refs.is_empty(),
            "f.severity.label() should produce a Call reference to `label`, got {:?}",
            result.references,
        );
    }

    #[test]
    fn test_mcp_tool_router_impl_methods_marked_unused_excluded() {
        // rmcp's `#[tool_router]` proc macro on an `impl` block and
        // `#[tool(...)]` on its methods wire those methods into a
        // generated dispatch surface that the static import graph cannot
        // see. The parser must stamp `unused_excluded=true` on every
        // method so `qartez_unused` does not flood with 37 fake dead
        // tool methods on a self-scan.
        let result = parse_rust(
            r#"
struct QartezServer;

#[tool_router(router = qartez_security_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_security",
        description = "scan"
    )]
    pub fn qartez_security(&self) -> String { String::new() }

    pub fn not_a_tool(&self) -> i32 { 0 }
}

#[tool_router(router = qartez_map_router)]
impl QartezServer {
    #[tool(name = "qartez_map")]
    pub fn qartez_map(&self) -> String { String::new() }
}
"#,
        );
        for name in ["qartez_security", "not_a_tool", "qartez_map"] {
            let sym = result
                .symbols
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("symbol {name} missing"));
            assert!(
                sym.unused_excluded,
                "method `{name}` must be excluded from unused (tool_router impl), got {sym:?}"
            );
        }
    }

    #[test]
    fn test_bare_tool_attribute_on_free_function_marks_unused_excluded() {
        // Not the common shape - rmcp wires most tools inside an impl
        // block - but `#[tool(...)]` can also decorate a free function
        // (e.g. when a crate re-exports a single-tool router). Both paths
        // go through the same generated dispatch.
        let result = parse_rust(
            r#"
#[tool(name = "standalone")]
pub fn standalone_tool() -> String { String::new() }
"#,
        );
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "standalone_tool")
            .expect("standalone_tool symbol");
        assert!(
            sym.unused_excluded,
            "free function under `#[tool(...)]` must be excluded from unused, got {sym:?}"
        );
    }

    #[test]
    fn test_tool_router_impl_excludes_methods_without_tool_attr() {
        // `#[tool_router]` on the impl block inherits exclusion to every
        // method inside, even the ones not individually annotated. rmcp
        // typically annotates every routable method, but a helper method
        // next to a tool method is still reached only through the
        // generated dispatcher and must not read as dead.
        let result = parse_rust(
            r#"
struct S;
#[tool_router(router = r)]
impl S {
    #[tool(name = "a")]
    pub fn a(&self) {}
    pub fn helper_for_a(&self) {}
}
"#,
        );
        for n in ["a", "helper_for_a"] {
            let sym = result
                .symbols
                .iter()
                .find(|s| s.name == n)
                .unwrap_or_else(|| panic!("symbol {n} missing"));
            assert!(sym.unused_excluded, "{n} must be excluded, got {sym:?}");
        }
    }

    #[test]
    fn test_plain_impl_methods_not_excluded() {
        // Inverse guard: an impl block WITHOUT `#[tool_router]` must
        // leave methods alone. Regressing this would mark every inherent
        // method in the project as dead-code-safe and hide real unused
        // candidates.
        let result = parse_rust(
            r#"
struct S;
impl S { pub fn a(&self) {} }
"#,
        );
        let sym = result.symbols.iter().find(|s| s.name == "a").unwrap();
        assert!(!sym.unused_excluded, "inherent `a` must NOT be excluded");
    }

    #[test]
    fn test_trait_impl_and_tool_router_stack() {
        // Trait impl methods are already excluded (dynamic dispatch).
        // Adding `#[tool_router]` on top must not regress the pre-existing
        // trait-impl exclusion.
        let result = parse_rust(
            r#"
trait T { fn run(&self); }
struct S;
#[tool_router(router = r)]
impl T for S {
    fn run(&self) {}
}
"#,
        );
        let sym = result.symbols.iter().find(|s| s.name == "run").unwrap();
        assert!(sym.unused_excluded, "trait impl method must be excluded");
    }

    #[test]
    fn test_attribute_with_comments_between() {
        // Walker must skip line_comment / block_comment siblings between
        // an attribute and its item - a docstring between `#[tool]` and
        // the method is idiomatic.
        let result = parse_rust(
            r#"
struct S;
impl S {
    #[tool(name = "a")]
    // doc comment
    /* block comment */
    pub fn a(&self) {}
}
"#,
        );
        let sym = result.symbols.iter().find(|s| s.name == "a").unwrap();
        assert!(
            sym.unused_excluded,
            "#[tool] must survive intervening comments"
        );
    }

    #[test]
    fn test_attribute_chain_cfg_then_tool() {
        // Multiple attributes stack. Walker must continue through
        // attribute_items that aren't `tool` until it sees one that is,
        // stopping only at the first non-attribute / non-comment sibling.
        let result = parse_rust(
            r#"
struct S;
impl S {
    #[cfg(feature = "x")]
    #[allow(dead_code)]
    #[tool(name = "a")]
    pub fn a(&self) {}
}
"#,
        );
        let sym = result.symbols.iter().find(|s| s.name == "a").unwrap();
        assert!(
            sym.unused_excluded,
            "#[tool] deep in attribute chain must be detected"
        );
    }

    #[test]
    fn test_typo_toolx_does_not_exclude() {
        // `#[toolx(...)]` or `#[mytool(...)]` must NOT trigger exclusion.
        // We check the exact attribute head identifier, not a prefix.
        let result = parse_rust(
            r#"
struct S;
impl S {
    #[toolx(name = "not_a_tool")]
    pub fn a(&self) {}
    #[mytool(name = "also_not")]
    pub fn b(&self) {}
}
"#,
        );
        for n in ["a", "b"] {
            let sym = result.symbols.iter().find(|s| s.name == n).unwrap();
            assert!(
                !sym.unused_excluded,
                "{n} must NOT be excluded by typo attr"
            );
        }
    }

    #[test]
    fn test_attribute_on_struct_does_not_leak_to_next_fn() {
        // The walker stops at the first non-attribute / non-comment
        // sibling. An attribute on an earlier unrelated item must not
        // bleed into functions that follow it.
        let result = parse_rust(
            r#"
#[tool(name = "on_struct")]
pub struct Unrelated;

pub fn later_fn() {}
"#,
        );
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "later_fn")
            .unwrap();
        assert!(
            !sym.unused_excluded,
            "tool attribute on struct must not leak to later_fn"
        );
    }

    #[test]
    fn test_scoped_tool_attribute_head_extracted() {
        // `#[rmcp::tool(...)]` is a legit way to name the attribute -
        // the head path's LAST segment is what we match ("tool").
        let result = parse_rust(
            r#"
struct S;
impl S {
    #[rmcp::tool(name = "a")]
    pub fn a(&self) {}
}
"#,
        );
        let sym = result.symbols.iter().find(|s| s.name == "a").unwrap();
        assert!(
            sym.unused_excluded,
            "#[rmcp::tool(...)] scoped path must be detected as `tool`"
        );
    }

    #[test]
    fn test_tool_router_on_trait_not_confused_with_builtin() {
        // Guard against overreach: `#[tool]` in isolation on an unrelated
        // non-function item must not accidentally bleed `unused_excluded`
        // into siblings. The check walks prev_siblings and stops at the
        // first non-attribute, so an attribute on an unrelated earlier
        // item cannot contaminate later ones.
        let result = parse_rust(
            r#"
#[tool(name = "A")]
pub fn tool_a() -> String { String::new() }

pub fn plain_fn() -> i32 { 0 }
"#,
        );
        let plain = result
            .symbols
            .iter()
            .find(|s| s.name == "plain_fn")
            .expect("plain_fn symbol");
        assert!(
            !plain.unused_excluded,
            "plain_fn must not inherit tool_a's exclusion, got {plain:?}"
        );
    }

    #[test]
    fn test_vec_bracket_macro_does_not_emit_spurious_calls() {
        // `vec![a, b, c]` uses a bracket token_tree, not a paren one.
        // is_paren_token_tree must return false and no Call refs should
        // be emitted for the elements.
        let result = parse_rust(
            r#"
fn build() -> Vec<i32> { vec![some_a, some_b, some_c] }
"#,
        );
        let bogus: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| {
                matches!(r.kind, ReferenceKind::Call)
                    && matches!(r.name.as_str(), "some_a" | "some_b" | "some_c")
            })
            .collect();
        assert!(
            bogus.is_empty(),
            "vec![] elements must NOT be reported as Calls, got {bogus:?}"
        );
    }

    #[test]
    fn test_assert_eq_args_are_not_calls() {
        // `assert_eq!(a, b)` inside a function body - `a` and `b` are
        // value expressions, not calls. Only actual `foo()` shapes
        // should produce Call refs.
        let result = parse_rust(
            r#"
fn t() {
    let x = 1;
    let y = 2;
    assert_eq!(x, y);
}
"#,
        );
        let bogus: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| {
                matches!(r.kind, ReferenceKind::Call) && matches!(r.name.as_str(), "x" | "y")
            })
            .collect();
        assert!(
            bogus.is_empty(),
            "assert_eq! args must NOT read as Calls, got {bogus:?}"
        );
    }

    #[test]
    fn test_free_function_call_inside_format_macro() {
        // `format!("{}", some_helper(arg))` - `some_helper` is followed
        // by a paren token_tree. Call ref emitted, `arg` is not followed
        // by parens and stays silent.
        let result = parse_rust(
            r#"
fn some_helper(_x: i32) -> i32 { 0 }
fn emit(arg: i32) -> String { format!("{}", some_helper(arg)) }
"#,
        );
        let call = result
            .references
            .iter()
            .find(|r| r.name == "some_helper" && matches!(r.kind, ReferenceKind::Call));
        assert!(
            call.is_some(),
            "some_helper() inside format!() must produce a Call, got {:?}",
            result.references
        );
        let bogus: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| matches!(r.kind, ReferenceKind::Call) && r.name == "arg")
            .collect();
        assert!(bogus.is_empty(), "arg must NOT be a Call, got {bogus:?}");
    }

    #[test]
    fn test_scoped_call_inside_format_macro() {
        // `format!("{}", Mod::build())` - tree-sitter-rust tokenizes the
        // scoped path inside a macro body as flat tokens (identifier,
        // ::, identifier, token_tree), NOT as a scoped_identifier node.
        // Under CallsOnly mode we still capture the callee `build`
        // (lowercase ident followed by parens) but the qualifier `Mod`
        // stays silent - we deliberately drop uppercase-standalone refs
        // in CallsOnly to avoid flooding the graph with fake type edges
        // on every `{:?}` debug dump. The Call alone is enough to keep
        // the target method out of the `unused` list, which is the bug
        // that motivated the change.
        let result = parse_rust(
            r#"
fn emit() -> String { format!("{}", Mod::build()) }
"#,
        );
        let has_call = result
            .references
            .iter()
            .any(|r| r.name == "build" && matches!(r.kind, ReferenceKind::Call));
        assert!(
            has_call,
            "build() must be a Call, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_scoped_call_in_non_builtin_macro_emits_type_use() {
        // In Full mode (non-builtin macro), the uppercase-standalone
        // branch still fires - `Mod` emits a Use ref so the type is not
        // flagged dead even when it only shows up inside a DSL block.
        // This is the existing proc-macro-DSL behavior that protects
        // structs like `ToolsParams` against false unused reports.
        let result = parse_rust(
            r#"
fn emit() { my_dsl!(Mod::build()) }
"#,
        );
        let has_use = result
            .references
            .iter()
            .any(|r| r.name == "Mod" && matches!(r.kind, ReferenceKind::Use));
        let has_call = result
            .references
            .iter()
            .any(|r| r.name == "build" && matches!(r.kind, ReferenceKind::Call));
        assert!(
            has_use,
            "Mod must emit Use in non-builtin DSL body, got {:?}",
            result.references
        );
        assert!(
            has_call,
            "build() must emit Call in non-builtin DSL body, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_callsonly_mode_suppresses_uppercase_use_in_format() {
        // Under the CallsOnly mode we drop the "uppercase standalone -> Use"
        // branch. `format!("{:?}", SomeType)` must NOT emit a Use ref
        // for SomeType (would flood the graph with fake type edges for
        // every println! debug-dump).
        let result = parse_rust(
            r#"
fn emit() -> String { format!("{:?}", SomeType) }
"#,
        );
        let spurious_use = result
            .references
            .iter()
            .filter(|r| r.name == "SomeType" && matches!(r.kind, ReferenceKind::Use))
            .count();
        assert_eq!(
            spurious_use, 0,
            "builtin macro body must not emit standalone Use for uppercase, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_non_builtin_macro_still_emits_uppercase_use() {
        // `my_dsl!(SomeType)` - non-builtin macro bodies remain in Full
        // mode. The uppercase standalone -> Use branch is the only way
        // proc-macro DSL parameter structs like `ToolsParams` get refs.
        let result = parse_rust(
            r#"
fn emit() { my_dsl!(SomeType) }
"#,
        );
        let has_use = result
            .references
            .iter()
            .any(|r| r.name == "SomeType" && matches!(r.kind, ReferenceKind::Use));
        assert!(
            has_use,
            "non-builtin macro body must emit Use for uppercase, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_deep_method_chain_in_format() {
        // `format!("{}", a.b.c.d.e())` - final `.e()` is the method call,
        // earlier dots are field access tokens. Only `e` should emit a
        // Call ref.
        let result = parse_rust(
            r#"
fn emit(x: X) -> String { format!("{}", x.b.c.d.e()) }
"#,
        );
        let has_e = result
            .references
            .iter()
            .any(|r| r.name == "e" && matches!(r.kind, ReferenceKind::Call));
        assert!(
            has_e,
            "deep chain .e() must emit Call, got {:?}",
            result.references
        );
        let intermediate_calls: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| {
                matches!(r.kind, ReferenceKind::Call) && matches!(r.name.as_str(), "b" | "c" | "d")
            })
            .collect();
        assert!(
            intermediate_calls.is_empty(),
            "intermediate field hops must NOT be Calls, got {intermediate_calls:?}"
        );
    }

    #[test]
    fn test_nested_format_macro() {
        // `format!("{}", format!("{}", x.inner()))` - the outer format's
        // body contains a macro_invocation which has its own token_tree.
        // Recursion via the stack must see the inner `x.inner()` call.
        let result = parse_rust(
            r#"
fn emit(x: X) -> String { format!("{}", format!("{}", x.inner())) }
"#,
        );
        let has_inner = result
            .references
            .iter()
            .any(|r| r.name == "inner" && matches!(r.kind, ReferenceKind::Call));
        assert!(
            has_inner,
            "nested format!() inner call must be captured, got {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_field_method_inside_format_macro() {
        // `format!("{}", f.severity.label())` - the method call lives inside
        // a macro invocation's token tree. tree-sitter-rust does not recurse
        // into `format!(...)` as a structured call_expression, so without
        // explicit macro-arg walking the `label` Call ref is never emitted.
        // This is the bug that makes `Severity::label()` appear unused.
        let result = parse_rust(
            r#"
pub enum Severity { Low }
impl Severity {
    pub fn label(&self) -> &'static str { "Low" }
}
pub struct Finding { pub severity: Severity }
fn emit(f: &Finding) -> String { format!("{}", f.severity.label()) }
"#,
        );
        let call_refs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "label" && matches!(r.kind, ReferenceKind::Call))
            .collect();
        assert!(
            !call_refs.is_empty(),
            "f.severity.label() inside format!() should produce a Call reference to `label`, got {:?}",
            result.references,
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
            "struct Foo should not reference itself, got {foo_refs:?}"
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

    #[test]
    fn test_refs_scoped_path_expression_in_const_array() {
        // Regression for the ALL_LANGUAGES-style registry pattern that used
        // to produce zero references: `qartez_refs BashSupport` returned
        // "No direct references found" even though `index/languages/mod.rs`
        // mentioned every dispatch struct. Two bugs combined: (a) `const_item`
        // did not set `new_enclosing`, so references inside the initializer
        // had `from_symbol_idx = None` and the resolver dropped them as
        // `no-enclosing`; (b) `record_reference` only handled scoped paths
        // in CALL position, so `&bash::BashSupport` never emitted an edge.
        let result = parse_rust(
            r#"
const ALL_LANGUAGES: [&dyn LanguageSupport; 2] = [
    &bash::BashSupport,
    &rust_lang::RustSupport,
];
"#,
        );
        let const_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "ALL_LANGUAGES")
            .expect("ALL_LANGUAGES const symbol");
        let bash_ref = result
            .references
            .iter()
            .find(|r| r.name == "BashSupport")
            .expect("BashSupport path reference");
        assert!(
            matches!(bash_ref.kind, ReferenceKind::Use),
            "&bash::BashSupport is a value use, not a call/type-ref"
        );
        assert_eq!(
            bash_ref.qualifier.as_deref(),
            Some("bash"),
            "scoped path should carry the module segment as qualifier"
        );
        assert_eq!(
            bash_ref.from_symbol_idx,
            Some(const_idx),
            "initializer references should be attributed to the const"
        );
        assert!(
            result.references.iter().any(|r| r.name == "RustSupport"),
            "RustSupport path reference should also be recorded"
        );
    }

    #[test]
    fn test_refs_scoped_path_no_duplicate_on_call() {
        // A `scoped_identifier` that is the function of a `call_expression`
        // already emits a Call edge via the `call_expression` arm. The new
        // `scoped_identifier` arm must NOT emit a second `Use` edge for the
        // same call, or every `module::func()` site would record two refs.
        let result = parse_rust(
            r#"
fn outer() {
    some_mod::inner();
}
"#,
        );
        let inner_refs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "inner")
            .collect();
        assert_eq!(
            inner_refs.len(),
            1,
            "exactly one reference should be recorded per callsite"
        );
        assert!(matches!(inner_refs[0].kind, ReferenceKind::Call));
    }

    // =========================================================================
    // Edge cases added during post-fix verification.
    // =========================================================================

    #[test]
    fn test_refs_scoped_path_in_static_initializer() {
        // Same fix as for `const_item`, but checking `static_item`. A static
        // that references a value-position scoped path must attribute the
        // reference to itself, not drop it as no-enclosing.
        let result = parse_rust(
            r#"
static DISPATCH: &[&dyn Handler] = &[&foo::FooHandler, &bar::BarHandler];
"#,
        );
        let static_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "DISPATCH")
            .expect("DISPATCH static symbol");
        let foo_ref = result
            .references
            .iter()
            .find(|r| r.name == "FooHandler")
            .expect("FooHandler path reference");
        assert_eq!(foo_ref.from_symbol_idx, Some(static_idx));
        assert!(matches!(foo_ref.kind, ReferenceKind::Use));
    }

    #[test]
    fn test_refs_scoped_path_does_not_record_use_inside_use_declaration() {
        // `use foo::Bar;` creates an import edge via extract_use_declaration.
        // The scoped_identifier arm must not ALSO emit a `Use` reference for
        // `Bar` - it is a name binding, not a value use.
        let result = parse_rust(
            r#"
use foo::Bar;

fn main() { let _ = Bar; }
"#,
        );
        let bar_refs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "Bar")
            .collect();
        assert!(
            bar_refs.len() <= 1,
            "use-declaration name must not be double-recorded, got {bar_refs:?}"
        );
    }

    #[test]
    fn test_refs_scoped_path_variant_matching() {
        // `match x { foo::Bar::Variant => ... }` - the scoped path to a
        // variant is a pattern, not an expression value use, but it still
        // should produce a reference to `Variant` (the last segment) so the
        // dispatch graph knows `foo::Bar` is used.
        let result = parse_rust(
            r#"
fn handle(x: foo::Bar) {
    match x {
        foo::Bar::One => {}
        foo::Bar::Two => {}
    }
}
"#,
        );
        // The important invariant is that parsing doesn't panic and that
        // reference extraction still works - the `Bar` TypeRef in the param
        // list must be present.
        assert!(
            result.references.iter().any(|r| r.name == "Bar"),
            "scoped type in param list should produce a Bar reference"
        );
    }

    #[test]
    fn test_refs_const_initializer_does_not_swallow_post_const_refs() {
        // Setting `new_enclosing = Some(idx)` for const_item must NOT cause
        // references AFTER the const (on a sibling item) to be attributed
        // to the const. The recursion consumes children of the const node,
        // and the caller picks up siblings with the original enclosing.
        let result = parse_rust(
            r#"
const A: [&dyn T; 1] = [&foo::Foo];

fn later() {
    bar::bar_fn();
}
"#,
        );
        let later_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "later")
            .expect("later fn symbol");
        let bar_ref = result
            .references
            .iter()
            .find(|r| r.name == "bar_fn")
            .expect("bar_fn call");
        assert_eq!(
            bar_ref.from_symbol_idx,
            Some(later_idx),
            "call inside `fn later` must be attributed to `later`, not the earlier const"
        );
    }

    #[test]
    fn test_refs_chained_method_on_scoped_path() {
        // `foo::Bar::method()` - the callee is `method`, qualifier `Bar`.
        // The outer scoped_identifier arm must not record an extra
        // `Use`-edge for `Bar`, because the call_expression arm already
        // handles the whole path.
        let result = parse_rust(
            r#"
fn main() { foo::Bar::method(); }
"#,
        );
        let method_refs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "method")
            .collect();
        assert_eq!(method_refs.len(), 1);
        assert!(matches!(method_refs[0].kind, ReferenceKind::Call));
        // `Bar` can appear as a qualifier on the method reference; that is
        // the call_expression arm's job. No separate `Use` edge for Bar.
        let bar_use_edges: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "Bar" && matches!(r.kind, ReferenceKind::Use))
            .collect();
        assert!(
            bar_use_edges.is_empty(),
            "Bar in foo::Bar::method() is a callee qualifier, not a standalone Use: {bar_use_edges:?}"
        );
    }

    #[test]
    fn test_refs_enum_variant_emits_typeref_to_enum() {
        // `QartezError::Io(e)` - the primary reference is the Call to
        // `Io` (variant constructor), but we also need a TypeRef to
        // `QartezError` so the defining enum gets credit. Without this
        // extra TypeRef, enums reached only through variant construction
        // show 0 refs and get flagged by qartez_unused.
        let result = parse_rust(
            r#"
fn wrap(e: Err) -> QartezError {
    QartezError::Io(e)
}
"#,
        );
        let err_typerefs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "QartezError" && matches!(r.kind, ReferenceKind::TypeRef))
            .collect();
        assert!(
            !err_typerefs.is_empty(),
            "QartezError::Io(...) must emit a TypeRef to QartezError: {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_sibling_module_function_call_carries_module_qualifier() {
        // `typescript::maybe_profile()` - the qualifier `typescript` is
        // lowercase (a module name), but it MUST still ride along on the
        // reference so the resolver's file-stem heuristic can pick
        // `maybe_profile` defined in `typescript.rs` out of same-named
        // functions in sibling module files.
        let result = parse_rust(
            r#"
fn dispatch() -> Option<T> {
    typescript::maybe_profile()
}
"#,
        );
        let call_refs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "maybe_profile")
            .collect();
        assert_eq!(call_refs.len(), 1);
        assert!(matches!(call_refs[0].kind, ReferenceKind::Call));
        assert_eq!(
            call_refs[0].qualifier.as_deref(),
            Some("typescript"),
            "lowercase module qualifier must survive for file-stem matching"
        );
    }

    #[test]
    fn test_refs_double_scoped_call_attributes_type_and_module() {
        // `cli::Cli::parse()` - the Call references `parse` with qualifier
        // `Cli`, AND a TypeRef to `Cli` with qualifier `cli` (module
        // segment) so the resolver's file-stem heuristic can disambiguate
        // same-named types across files.
        let result = parse_rust(
            r#"
fn main() {
    let cli = cli::Cli::parse();
}
"#,
        );
        let parse_refs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "parse" && matches!(r.kind, ReferenceKind::Call))
            .collect();
        assert_eq!(parse_refs.len(), 1);
        assert_eq!(parse_refs[0].qualifier.as_deref(), Some("Cli"));

        let cli_typeref: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "Cli" && matches!(r.kind, ReferenceKind::TypeRef))
            .collect();
        assert_eq!(cli_typeref.len(), 1);
        assert_eq!(
            cli_typeref[0].qualifier.as_deref(),
            Some("cli"),
            "Cli TypeRef must carry the `cli` module segment for file-stem resolution"
        );
    }

    #[test]
    fn test_refs_upper_snake_identifier_emits_use() {
        // Bare UPPER_SNAKE_CASE identifiers in expression position are
        // const / static references. Without this emission the defining
        // const gets 0 refs and is flagged as unused.
        let result = parse_rust(
            r#"
fn pick() -> i64 {
    MISC_CLUSTER_ID
}
"#,
        );
        let const_refs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "MISC_CLUSTER_ID")
            .collect();
        assert_eq!(const_refs.len(), 1);
        assert!(matches!(const_refs[0].kind, ReferenceKind::Use));
    }

    #[test]
    fn test_refs_lowercase_identifier_does_not_emit_use() {
        // Lowercase local identifiers outside of call-argument position
        // are NOT emitted as references. Only UPPER_SNAKE_CASE bare reads
        // pass the const-shape filter; lowercase identifiers show up as
        // Use refs only when passed as callback / function pointer
        // arguments (see `test_refs_lowercase_fn_as_callback_arg`).
        let result = parse_rust(
            r#"
fn pick() -> i64 {
    let x = 5;
    x
}
"#,
        );
        let x_refs: Vec<&ExtractedReference> =
            result.references.iter().filter(|r| r.name == "x").collect();
        assert!(
            x_refs.is_empty(),
            "lowercase locals must not emit Use refs: {x_refs:?}"
        );
    }

    #[test]
    fn test_refs_lowercase_fn_as_callback_arg() {
        // Regression: intra-file `pub(super)` helpers passed to
        // `.map(expand_kind_alias)` were invisible to `qartez_refs`
        // because the identifier in argument position emitted no ref at
        // all. tree-sitter-rust wraps call arguments in an `arguments`
        // node; the identifier arm of `record_reference` now emits a
        // Use ref when it sees that parent, letting the resolver match
        // against an indexed helper.
        let result = parse_rust(
            r#"
fn helper(x: i32) -> i32 { x * 2 }
fn caller(list: Vec<i32>) -> Vec<i32> {
    list.into_iter().map(helper).collect()
}
"#,
        );
        let helper_use_refs: Vec<&ExtractedReference> = result
            .references
            .iter()
            .filter(|r| r.name == "helper" && matches!(r.kind, ReferenceKind::Use))
            .collect();
        assert!(
            !helper_use_refs.is_empty(),
            "function-pointer argument `helper` must emit a Use ref; refs: {:?}",
            result.references
        );
    }

    #[test]
    fn test_refs_method_syntax_call_is_flagged() {
        // `.filter(...)` parses as a `call_expression` whose `function`
        // child is a `field_expression`. The emitted Call ref must carry
        // `via_method_syntax=true` so the resolver can drop cross-file
        // ambiguity against fields / functions sharing the method name.
        let result = parse_rust(
            r#"
fn caller(list: Vec<i32>) -> Vec<i32> {
    list.into_iter().filter(|_| true).collect()
}
"#,
        );
        let filter_ref = result
            .references
            .iter()
            .find(|r| r.name == "filter" && matches!(r.kind, ReferenceKind::Call))
            .expect("filter call ref");
        assert!(
            filter_ref.via_method_syntax,
            "method-syntax call must flip via_method_syntax: {filter_ref:?}"
        );
    }

    #[test]
    fn test_refs_scoped_call_is_not_method_syntax() {
        // Positive counter-test: `Foo::new()` is a scoped call, not a
        // method-syntax call. It must NOT flip `via_method_syntax` -
        // otherwise the resolver would drop legitimate cross-file
        // associated-function resolution.
        let result = parse_rust(
            r#"
fn caller() { Foo::new(); }
"#,
        );
        let new_ref = result
            .references
            .iter()
            .find(|r| r.name == "new" && matches!(r.kind, ReferenceKind::Call))
            .expect("new call ref");
        assert!(
            !new_ref.via_method_syntax,
            "scoped call must not flip via_method_syntax: {new_ref:?}"
        );
    }

    #[test]
    fn test_refs_generic_type_argument_emits_typeref() {
        // `Parameters<ToolsParams>` in type position: the inner
        // `type_identifier` ToolsParams sits inside a `type_arguments`
        // node, which is NOT in the skip list of the `type_identifier`
        // arm, so the walker should reach it and emit a TypeRef.
        let result = parse_rust(
            r#"
fn handler(Parameters(params): Parameters<ToolsParams>) -> Result<(), String> {
    Ok(())
}
"#,
        );
        let names: Vec<&str> = result.references.iter().map(|r| r.name.as_str()).collect();
        assert!(
            names.contains(&"ToolsParams"),
            "generic type argument ToolsParams must be recorded; got: {names:?}"
        );
    }

    #[test]
    fn test_refs_serde_deserialize_with_string_path() {
        // `#[serde(deserialize_with = "flexible::u32_opt")]`: the function
        // name `u32_opt` lives inside a string literal. Extract it as a
        // Use reference so `qartez_refs u32_opt` finds the call site and
        // `qartez_unused` does not flag the function as dead.
        let result = parse_rust(
            r#"
#[derive(Deserialize)]
struct S {
    #[serde(deserialize_with = "flexible::u32_opt")]
    pub limit: Option<u32>,
}
"#,
        );
        let names: Vec<&str> = result.references.iter().map(|r| r.name.as_str()).collect();
        assert!(
            names.contains(&"u32_opt"),
            "serde deserialize_with path tail must be recorded; got: {names:?}"
        );
    }

    #[test]
    fn test_refs_serde_with_only_fires_inside_serde() {
        // Non-serde attributes with a `with = "path"` entry must NOT emit
        // string-path references - this is specific to serde's DSL.
        let result = parse_rust(
            r#"
#[custom_attr(with = "some::other_func")]
struct S;
"#,
        );
        let names: Vec<&str> = result.references.iter().map(|r| r.name.as_str()).collect();
        assert!(
            !names.contains(&"other_func"),
            "non-serde attributes must not emit string-path refs; got: {names:?}"
        );
    }

    #[test]
    fn test_refs_macro_body_captures_uppercase_identifiers() {
        // Proc-macro / macro_rules! DSL bodies: no macro expansion is
        // done, but we heuristically walk the token_tree and emit Use
        // refs for identifiers that look like types or consts.
        let result = parse_rust(
            r#"
macro_rules! router {
    ($($x:tt)*) => {};
}
router! {
    SoulWorkspaceParams => workspace,
    ToolsParams => tools,
}
"#,
        );
        let names: Vec<&str> = result.references.iter().map(|r| r.name.as_str()).collect();
        assert!(
            names.contains(&"SoulWorkspaceParams"),
            "macro body must capture uppercase identifiers; got: {names:?}"
        );
        assert!(
            names.contains(&"ToolsParams"),
            "macro body must capture uppercase identifiers; got: {names:?}"
        );
    }

    #[test]
    fn test_refs_macro_body_skips_lowercase_and_builtins() {
        // The macro-body heuristic restricts to uppercase-leading names
        // to avoid flooding refs with keywords-as-identifiers (`fn`,
        // `let`, `self`) and built-in types (`String`, `u32`, `Self`).
        let result = parse_rust(
            r#"
macro_rules! router {
    ($($x:tt)*) => {};
}
router! {
    foo => bar,
    String => value,
    Self => current,
}
"#,
        );
        let names: Vec<&str> = result.references.iter().map(|r| r.name.as_str()).collect();
        assert!(
            !names.contains(&"foo"),
            "lowercase tokens must not leak from macro bodies; got: {names:?}"
        );
        assert!(
            !names.contains(&"String"),
            "built-in types must not leak from macro bodies; got: {names:?}"
        );
        assert!(
            !names.contains(&"Self"),
            "Self must not leak from macro bodies; got: {names:?}"
        );
    }

    #[test]
    fn test_refs_builtin_macro_body_not_walked() {
        // `println!`, `vec!`, etc. are noise - do not walk their bodies
        // either. The macro itself is already filtered by
        // `is_builtin_macro`; bodies stay untouched by the heuristic.
        let result = parse_rust(
            r#"
fn f() {
    println!("hello {}", SomeType);
}
"#,
        );
        let names: Vec<&str> = result.references.iter().map(|r| r.name.as_str()).collect();
        let count = names.iter().filter(|n| **n == "SomeType").count();
        assert_eq!(
            count, 0,
            "builtin macro bodies must not emit heuristic refs; got: {names:?}"
        );
    }
}
