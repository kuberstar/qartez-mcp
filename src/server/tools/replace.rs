// Rust guideline compliant 2026-04-21

#![allow(unused_imports)]

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};

use super::super::QartezServer;
use super::super::params::*;
use super::refactor_common::{
    join_lines_with_trailing, resolve_unique_symbol, validate_range, write_atomic,
};

#[tool_router(router = qartez_replace_symbol_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_replace_symbol",
        description = "Replace a symbol's whole source range (lines L[line_start..line_end]) with `new_code`. Caller supplies the new definition including its signature - this is a precise line-range rewrite, not a body-only splice. The tool refuses to run when `new_code` does not start with a recognised definition introducer (`fn`, `pub fn`, `struct`, `class`, `def`, etc.). Preview by default; set apply=true to execute. Use `kind` / `file_path` to disambiguate when the name is shared.",
        annotations(
            title = "Replace Symbol",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_replace_symbol(
        &self,
        Parameters(params): Parameters<SoulReplaceSymbolParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let (sym, source_file) = resolve_unique_symbol(
            &conn,
            &params.symbol,
            params.kind.as_deref(),
            params.file_path.as_deref(),
        )?;
        drop(conn);

        // Refuse kinds that are not standalone definitions. Fields,
        // enum variants, parameters and local variables live inside a
        // larger declaration. Rewriting their whole-line range with
        // `new_code` will corrupt the surrounding container's
        // indentation, trailing comma, or brace layout - exactly the
        // scenario the docstring promises the tool prevents. Callers
        // who want to edit a field should rewrite the parent struct.
        const NON_DEFINITION_KINDS: &[&str] = &[
            "field",
            "variant",
            "enum_variant",
            "parameter",
            "argument",
            "variable",
            "property",
        ];
        if NON_DEFINITION_KINDS
            .iter()
            .any(|k| sym.kind.eq_ignore_ascii_case(k))
        {
            return Err(format!(
                "Refusing to replace '{}' ({}): `{}` is not a standalone definition. `qartez_replace_symbol` rewrites whole-symbol line ranges; editing a {} would corrupt its parent container. Rewrite the enclosing struct/enum/class instead.",
                sym.name, sym.kind, sym.kind, sym.kind,
            ));
        }

        // Strip a leading UTF-8 BOM (U+FEFF) before any validation. A
        // BOM smuggled in by a Windows editor would otherwise trip the
        // introducer check with a confusing "does not start with fn"
        // message even when the rest of `new_code` is a perfectly
        // valid definition. The sanitized value is reused for every
        // downstream check AND the final splice, so the BOM never
        // survives into the rewritten file.
        let new_code_sanitized = strip_leading_bom(&params.new_code).to_string();

        // Refuse empty `new_code` so a stray empty string doesn't turn into
        // "replace the symbol with one blank line" via the `"".split('\n')`
        // -> `[""]` quirk. Callers wanting to remove a symbol should use
        // `qartez_safe_delete`, which also runs the importer check.
        if new_code_sanitized.trim_end_matches('\n').is_empty() {
            return Err(format!(
                "Empty `new_code` for qartez_replace_symbol. Pass the full replacement (including the signature), or use qartez_safe_delete to remove '{}'.",
                params.symbol,
            ));
        }

        let abs_path = self.safe_resolve(&source_file.path)?;
        let content = std::fs::read_to_string(&abs_path)
            .map_err(|e| format!("Cannot read {}: {e}", abs_path.display()))?;
        let (lines, start_idx, end_idx) = validate_range(&content, &sym, &source_file.path)?;

        // Signature sanity check. `qartez_replace_symbol` is a whole-symbol
        // rewrite (line range -> new_code); passing body-only code silently
        // corrupts the file by dropping the `fn name(...) {` line. Compare
        // the original symbol's first introducer token with the supplied
        // new_code; refuse when the new_code is clearly a body splice.
        if let Some(err) =
            check_signature_shape(&sym, &lines[start_idx..end_idx], &new_code_sanitized)
        {
            return Err(err);
        }

        // Defence-in-depth name check. The introducer-shape check above
        // only inspects the keyword (`fn`, `struct`, ...); it happily
        // accepts `fn DIFFERENT_NAME` for a target named `original`,
        // which is effectively a hidden rename that leaves every
        // importer dangling. Pull the defined identifier out of the
        // new_code and require it to match the resolved symbol name.
        if let Some(err) = check_identifier_match(&sym, &new_code_sanitized) {
            return Err(err);
        }

        // Trailing-content check. `new_code` is contracted to carry a
        // single top-level item; the previous implementation accepted
        // `fn foo() { 0 } garbage;\nstray();` because only the first
        // non-empty line's introducer was validated. Ensure no
        // non-whitespace, non-comment content survives past the
        // definition's closing brace so an apply cannot silently paste
        // junk into the file.
        if let Some(err) = check_trailing_content(&new_code_sanitized) {
            return Err(err);
        }

        // Structural-change analysis. `qartez_replace_symbol` is contracted
        // as a body-or-signature rewrite of ONE symbol; a caller who changes
        // the signature, visibility, or kind is effectively redeclaring the
        // item in place, which silently breaks every caller that relied on
        // the old shape. Previously kind changes were a soft warning; we
        // now REFUSE by default on any of the three structural axes, emit
        // the same warning list in preview, and point callers at
        // `qartez_rename` / `qartez_safe_delete + add` for the intentional
        // redeclaration path. No opt-in param is wired through the schema,
        // so the refusal is absolute on apply - callers MUST split the
        // change into smaller edits.
        let kind_change_note = detect_kind_change(&lines[start_idx..end_idx], &new_code_sanitized);
        let signature_change_note =
            detect_signature_change(&lines[start_idx..end_idx], &new_code_sanitized);
        let visibility_change_note =
            detect_visibility_change(&lines[start_idx..end_idx], &new_code_sanitized);

        let apply = params.apply.unwrap_or(false);
        let replaced_lines = end_idx - start_idx;
        let new_lines_count = new_code_sanitized.lines().count();

        if !apply {
            let mut out = format!(
                "Preview: replace '{}' ({}) in {} L{}-L{} ({} → {} lines)\n\n",
                sym.name,
                sym.kind,
                source_file.path,
                sym.line_start,
                sym.line_end,
                replaced_lines,
                new_lines_count,
            );
            if let Some(note) = &kind_change_note {
                out.push_str(&format!("WARNING: {note}\n"));
            }
            if let Some(note) = &signature_change_note {
                out.push_str(&format!("WARNING: {note}\n"));
            }
            if let Some(note) = &visibility_change_note {
                out.push_str(&format!("WARNING: {note}\n"));
            }
            if kind_change_note.is_some()
                || signature_change_note.is_some()
                || visibility_change_note.is_some()
            {
                // Structural changes are a common source of silent
                // downstream breakage; surface the apply-time refusal
                // in preview so the caller learns the escape hatch
                // before they waste a second round-trip.
                out.push_str(
                    "NOTE: apply=true will REFUSE while any structural change is present. Use qartez_rename for renames, qartez_safe_delete + add for re-declarations, or split the change into smaller edits.\n\n",
                );
            }
            out.push_str("Old:\n```\n");
            out.push_str(&lines[start_idx..end_idx].join("\n"));
            out.push_str("\n```\n\nNew:\n```\n");
            out.push_str(&new_code_sanitized);
            if !new_code_sanitized.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n");
            return Ok(out);
        }

        if kind_change_note.is_some()
            || signature_change_note.is_some()
            || visibility_change_note.is_some()
        {
            let mut reasons: Vec<String> = Vec::new();
            if let Some(note) = &kind_change_note {
                reasons.push(note.clone());
            }
            if let Some(note) = &signature_change_note {
                reasons.push(note.clone());
            }
            if let Some(note) = &visibility_change_note {
                reasons.push(note.clone());
            }
            return Err(format!(
                "Refusing to apply: signature/visibility/kind change detected. Run with apply=false to see the diff. This tool does not auto-approve structural changes; use qartez_rename for renames, qartez_safe_delete + add for re-declarations, or split the change into smaller edits.\nDetected: {}",
                reasons.join("; "),
            ));
        }

        // Build the rewritten file content. Strip the trailing newline of
        // `new_code` if present so we don't introduce a phantom blank line
        // at the seam; the global trailing-newline convention is restored
        // below via `join_lines_with_trailing`.
        let new_code = new_code_sanitized.trim_end_matches('\n');
        let preserve_trailing_newline = content.ends_with('\n');

        let mut rewritten: Vec<&str> = Vec::with_capacity(lines.len());
        rewritten.extend_from_slice(&lines[..start_idx]);
        for line in new_code.split('\n') {
            rewritten.push(line);
        }
        rewritten.extend_from_slice(&lines[end_idx..]);
        let new_content = join_lines_with_trailing(&rewritten, preserve_trailing_newline);

        if new_content == content {
            return Ok(format!(
                "No changes: new code matches existing definition of '{}' in {} L{}-L{}.",
                sym.name, source_file.path, sym.line_start, sym.line_end,
            ));
        }

        write_atomic(&abs_path, &new_content)?;
        let ok = format!(
            "Replaced '{}' ({}) in {} L{}-L{} ({} → {} lines).\n",
            sym.name,
            sym.kind,
            source_file.path,
            sym.line_start,
            sym.line_end,
            replaced_lines,
            new_lines_count,
        );
        Ok(ok)
    }
}

/// Verify the supplied `new_code` still looks like a full definition of the
/// symbol it is about to replace. Compares the introducer token of the
/// first non-blank line in the original range against the first non-blank
/// line of `new_code` and returns an explanatory error when the new_code
/// is clearly a body-only splice.
///
/// Language-aware for Rust (`fn`, `struct`, `trait`, `impl`, `enum`,
/// `async fn`, `const`, `static`, `type`, `mod`, `pub` prefixes, macros),
/// TypeScript / JavaScript (`function`, `class`, `interface`, `type`,
/// `enum`, `export`, `async`, `public/private/protected/static` for
/// methods), and Python (`def`, `async def`, `class`). When the kind is
/// unknown or the original range is blank the check is a no-op.
fn check_signature_shape(
    sym: &crate::storage::models::SymbolRow,
    old_lines: &[&str],
    new_code: &str,
) -> Option<String> {
    // Pre-processing: skip past attribute (`#[...]`) and doc-comment
    // preludes. A definition in Rust often starts with several `#[derive]`
    // / `#[serde(...)]` / `///` lines before the actual `pub fn` / `struct`.
    // `use`, `///`, `//!`, `#!`, and lone `*` / `//` / `#` are explicitly
    // NOT allowed as the introducer itself - they never start a new symbol
    // definition - but they are treated as valid prelude lines so the
    // scanner looks past them to find the real introducer.
    let old_first = first_real_introducer_line(old_lines.iter().copied())?;
    let new_first = first_real_introducer_line(new_code.lines())?;

    fn is_introducer(line: &str, introducers: &[&str]) -> bool {
        for intro in introducers {
            if line == *intro {
                return true;
            }
            if let Some(after) = line.strip_prefix(intro) {
                if let Some(ch) = after.chars().next()
                    && !ch.is_alphanumeric()
                    && ch != '_'
                {
                    return true;
                }
                if after.is_empty() {
                    return true;
                }
            }
        }
        false
    }

    // Core definition introducers. `use`, `///`, `//!`, `#[`, `#!`, `@`,
    // `/**`, `*`, `//`, `#` are handled as PRELUDE lines by
    // `first_real_introducer_line` above; they are never valid on their
    // own as the new-symbol introducer.
    let rust_introducers: &[&str] = &[
        "pub",
        "fn",
        "async",
        "struct",
        "trait",
        "impl",
        "enum",
        "const",
        "static",
        "type",
        "mod",
        "unsafe",
        "extern",
        "macro_rules!",
    ];
    let ts_introducers: &[&str] = &[
        "export",
        "function",
        "class",
        "interface",
        "type",
        "enum",
        "async",
        "public",
        "private",
        "protected",
        "static",
        "readonly",
        "abstract",
        "declare",
        "const",
        "let",
        "var",
        "get",
        "set",
    ];
    let python_introducers: &[&str] = &["def", "async", "class"];

    // Unknown-kind fallback sniffs every language. Merge once so `class`,
    // `def`, etc. are accepted when the backing language is TS/Python/Ruby
    // but the indexer reported a kind name the match arms below do not
    // enumerate (e.g. `function` on a Python `def`).
    let all_introducers: Vec<&str> = rust_introducers
        .iter()
        .chain(ts_introducers.iter())
        .chain(python_introducers.iter())
        .copied()
        .collect();

    let introducers: &[&str] = match sym.kind.as_str() {
        "function" | "method" | "struct" | "trait" | "impl" | "enum" | "const" | "static"
        | "type_alias" | "module" | "macro" => rust_introducers,
        "class" | "interface" | "type" => ts_introducers,
        _ => {
            // Unknown kind: sniff by the first old line itself across
            // every known language. Any definition introducer is
            // accepted on the new code as long as the old code also
            // started with one - otherwise we cannot distinguish
            // "signature rewrite" from "body splice" and fall through
            // to a no-op check.
            if is_introducer(old_first, &all_introducers) {
                return if is_introducer(new_first, &all_introducers) {
                    None
                } else {
                    Some(format!(
                        "Refusing to replace '{}' ({}): `new_code` does not start with a definition introducer. The first non-blank, non-attribute, non-doc-comment line is:\n  {}\nExpected a line beginning with something like `fn`, `pub fn`, `struct`, `class`, `def`, `interface`, etc. `qartez_replace_symbol` is a whole-symbol rewrite - include the full signature, not just the body.",
                        sym.name, sym.kind, new_first,
                    ))
                };
            }
            return None;
        }
    };

    if !is_introducer(old_first, introducers) {
        return None;
    }
    if is_introducer(new_first, introducers) {
        return None;
    }
    Some(format!(
        "Refusing to replace '{}' ({}): `new_code` does not start with a definition introducer. The first non-blank, non-attribute, non-doc-comment line is:\n  {}\nExpected a line beginning with one of: {}. `qartez_replace_symbol` is a whole-symbol rewrite - include the full signature, not just the body.",
        sym.name,
        sym.kind,
        new_first,
        introducers.join(", "),
    ))
}

/// Walk an iterator of source lines, skipping blank lines plus prelude-only
/// lines that precede a definition but never introduce one on their own:
/// Rust outer attributes (`#[derive(...)]`), inner attributes (`#!`),
/// doc comments (`///`, `//!`, `/**`, `*` continuation, `//`), Python /
/// TS decorators (`@derive`), and `#` comments. Returns the first line
/// that looks like an actual introducer token (`pub fn ...`, `struct ...`,
/// etc.) or `None` if the whole slice is prelude / blank.
fn first_real_introducer_line<'a, I: IntoIterator<Item = &'a str>>(lines: I) -> Option<&'a str> {
    for raw in lines {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_prelude_line(trimmed) {
            continue;
        }
        return Some(trimmed);
    }
    None
}

/// Return true for lines that typically sit above a definition (attributes,
/// doc comments, decorators) but are never introducers themselves.
fn is_prelude_line(trimmed: &str) -> bool {
    if let Some(rest) = trimmed.strip_prefix("#!")
        && rest.starts_with('[')
    {
        return true;
    }
    trimmed.starts_with("#[")
        || trimmed.starts_with("///")
        || trimmed.starts_with("//!")
        || trimmed.starts_with("/**")
        || trimmed.starts_with("/*")
        || trimmed.starts_with("//")
        || trimmed.starts_with("* ")
        || trimmed == "*"
        || trimmed.starts_with("*/")
        || trimmed.starts_with('@')
        || trimmed.starts_with('#')
}

/// Compare the introducer keyword on the first real line of `old_lines`
/// to the one on the first real line of `new_code`. When they differ,
/// return a human-readable note like "kind change: struct → fn" so the
/// caller can show it as a preview warning. Returns `None` when either
/// side has no detectable introducer or the kinds match.
fn detect_kind_change(old_lines: &[&str], new_code: &str) -> Option<String> {
    let old_first = first_real_introducer_line(old_lines.iter().copied())?;
    let new_first = first_real_introducer_line(new_code.lines())?;
    let old_kind = extract_introducer_token(old_first)?;
    let new_kind = extract_introducer_token(new_first)?;
    if old_kind == new_kind {
        return None;
    }
    Some(format!("kind change: {old_kind} → {new_kind}"))
}

/// Extract the first recognised introducer token from a trimmed line. Skips
/// leading visibility modifiers (`pub`, `pub(crate)`, `pub(super)`, `async`,
/// `unsafe`, `extern`, `export`, `declare`, `static`, `public`, `private`,
/// `protected`, `readonly`, `abstract`) so that `struct` shines through in
/// `pub struct Foo` and `fn` shines through in `pub(crate) async fn bar`.
fn extract_introducer_token(line: &str) -> Option<&'static str> {
    const STRIP: &[&str] = &[
        "pub(crate)",
        "pub(super)",
        "pub(self)",
        "pub",
        "async",
        "unsafe",
        "extern",
        "export",
        "default",
        "declare",
        "static",
        "public",
        "private",
        "protected",
        "readonly",
        "abstract",
        "const",
    ];
    const TOKENS: &[&str] = &[
        "fn",
        "struct",
        "trait",
        "impl",
        "enum",
        "const",
        "static",
        "type",
        "mod",
        "macro_rules!",
        "function",
        "class",
        "interface",
        "def",
    ];
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start();
        let mut advanced = false;
        for s in STRIP {
            if let Some(after) = trimmed.strip_prefix(s)
                && matches!(after.chars().next(), Some(c) if c.is_whitespace() || c == '(' || c == '!')
            {
                rest = after;
                advanced = true;
                break;
            }
        }
        if !advanced {
            break;
        }
    }
    let trimmed = rest.trim_start();
    for t in TOKENS {
        if let Some(after) = trimmed.strip_prefix(t)
            && matches!(
                after.chars().next(),
                None | Some(' ') | Some('\t') | Some('<') | Some('(') | Some('{')
            )
        {
            return Some(t);
        }
    }
    None
}

/// Strip a single UTF-8 BOM (U+FEFF) from the leading edge of `raw`.
/// Windows editors and clipboard paths occasionally smuggle the BOM in
/// front of pasted source; without this strip, downstream introducer
/// checks fail with a confusing "does not start with fn" even when the
/// rest of the new_code is a perfectly valid definition.
fn strip_leading_bom(raw: &str) -> &str {
    raw.strip_prefix('\u{FEFF}').unwrap_or(raw)
}

/// Structural summary extracted from a definition's leading lines. `params`
/// lists the parameter TYPES in declaration order (names are normalised
/// away so `fn foo(x: u32)` compares equal to `fn foo(y: u32)`), and
/// `return_type` captures the arrow-clause verbatim. `visibility` stores
/// the empty string for private items; otherwise the canonical `pub`,
/// `pub(crate)`, `pub(super)`, `pub(self)`, or `pub(in path)` modifier.
#[derive(Debug, PartialEq, Eq, Clone)]
struct SymbolSignature {
    params: Vec<String>,
    return_type: Option<String>,
    visibility: String,
}

/// Extract a lightweight signature summary from the first real introducer
/// line of a function-like definition. The scan is intentionally regex-free
/// and permissive: when the shape is not recognised (macros, consts,
/// structs, anything without a `(` on the introducer line) we return
/// `None` so the caller falls back to skipping the signature-change
/// guard instead of crashing.
fn extract_signature<'a, I: IntoIterator<Item = &'a str>>(lines: I) -> Option<SymbolSignature> {
    let joined = collect_definition_head(lines)?;
    let intro = first_real_introducer_line(joined.lines())?;

    let visibility = extract_visibility(intro);

    // Locate the opening paren of the parameter list. No paren means
    // this is a struct / enum / const / type alias - return None to
    // signal "no signature check here".
    let paren_open = joined.find('(')?;
    let paren_close = find_matching_paren(&joined, paren_open)?;
    let raw_params = &joined[paren_open + 1..paren_close];
    let params = split_params(raw_params);

    // Return-type clause: the `-> T` that follows the parameter list,
    // stopping at the next top-level `{` or `where` token. TS / Python
    // use `:` and `->` respectively; the same shape covers both.
    let after_paren = &joined[paren_close + 1..];
    let return_type = extract_return_type(after_paren);

    Some(SymbolSignature {
        params,
        return_type,
        visibility,
    })
}

/// Concatenate source lines into a single head string, stopping after the
/// first `{` that opens the body or the first `;` that terminates the
/// signature. This is enough to capture the whole parameter list even
/// when the signature is wrapped across several lines - which Rust
/// fmt commonly does for `fn` items with many args.
fn collect_definition_head<'a, I: IntoIterator<Item = &'a str>>(lines: I) -> Option<String> {
    let mut started = false;
    let mut depth: i32 = 0;
    let mut out = String::new();
    for raw in lines {
        let trimmed = raw.trim();
        if !started {
            if trimmed.is_empty() || is_prelude_line(trimmed) {
                continue;
            }
            started = true;
        }
        // Walk character by character so the depth tracker survives
        // tuple-struct `{` inside type params (`Fn() -> Foo`). Strings
        // and comments inside a head line are a theoretical edge case
        // we do not bother with: a leading signature line almost never
        // contains either.
        for ch in raw.chars() {
            out.push(ch);
            match ch {
                '(' | '<' | '[' => depth += 1,
                ')' | '>' | ']' => depth -= 1,
                '{' if depth == 0 => return Some(out),
                ';' if depth == 0 => return Some(out),
                _ => {}
            }
        }
        out.push('\n');
    }
    if started { Some(out) } else { None }
}

/// Find the matching close paren for the `(` at `open`. Ignores nested
/// parens so `Fn(Foo) -> Bar` inside a parameter type does not terminate
/// the scan early. Returns `None` when the string is unbalanced.
fn find_matching_paren(s: &str, open: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 1;
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Split a raw parameter list into type strings, one per parameter.
/// Names are stripped (everything before the first `:` at depth 0 on the
/// parameter) so the comparison is type-shape only: a rename of `x` to
/// `y` must not trip the signature-change guard, but flipping `u32` to
/// `u64` must.
fn split_params(raw: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut depth: i32 = 0;
    let mut current = String::new();
    for ch in raw.chars() {
        match ch {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(current.trim().to_string());
                current = String::new();
                continue;
            }
            _ => {}
        }
        current.push(ch);
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .map(normalise_param)
        .collect()
}

/// Normalise a single parameter slice to its type shape. Drops the name
/// portion (`foo: T` -> `T`, `mut foo: T` -> `T`), preserves `self` /
/// `&self` / `&mut self` receivers verbatim since they ARE the shape, and
/// collapses internal whitespace so formatting-only differences compare
/// equal.
fn normalise_param(raw: String) -> String {
    let trimmed = raw.trim();
    if trimmed == "self" || trimmed == "&self" || trimmed == "&mut self" {
        return trimmed.to_string();
    }
    // Name is everything before the first top-level `:`. If there is no
    // `:` the whole slice IS the type (TS tuple-style, or a macro param).
    if let Some(colon_idx) = find_top_level_colon(trimmed) {
        let ty = trimmed[colon_idx + 1..].trim();
        return collapse_ws(ty);
    }
    collapse_ws(trimmed)
}

/// Return the byte index of the first `:` that is not inside nested
/// generics / parens. Type annotations (`Result<T, E>`, `Map<K, V>`) hide
/// `:` inside trait-bound clauses that the shallow split must not pick up.
fn find_top_level_colon(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'<' | b'[' => depth += 1,
            b')' | b'>' | b']' => depth -= 1,
            b':' if depth == 0 => {
                // Skip `::` path separators; they are not a name/type
                // split even though the first byte matches.
                if i + 1 < bytes.len() && bytes[i + 1] == b':' {
                    i += 2;
                    continue;
                }
                return Some(i);
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_ws && !out.is_empty() {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    out.trim_end().to_string()
}

/// Extract the return-type clause from the bytes that follow the closing
/// paren of the parameter list. Looks for `->` (Rust / TS arrow) or a
/// top-level `:` (TS method return), stops at the next `{` / `where` /
/// `;`. Returns `None` when the function has no explicit return type.
fn extract_return_type(after: &str) -> Option<String> {
    let s = after.trim_start();
    let body = if let Some(rest) = s.strip_prefix("->") {
        rest
    } else if let Some(rest) = s.strip_prefix(':') {
        rest
    } else {
        return None;
    };
    // Walk depth so `Result<T, E>` does not get clipped by its inner
    // `,` or `>`. Stop at a top-level `{`, `where`, or `;`.
    let bytes = body.as_bytes();
    let mut depth: i32 = 0;
    let mut end = bytes.len();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'<' | b'[' => depth += 1,
            b')' | b'>' | b']' => depth -= 1,
            b'{' | b';' if depth == 0 => {
                end = i;
                break;
            }
            _ => {}
        }
        // Match the literal `where` keyword at a word boundary at depth 0.
        if depth == 0
            && bytes[i..].starts_with(b"where")
            && (i == 0 || !is_word_byte(bytes[i - 1]))
            && bytes.get(i + 5).is_none_or(|b| !is_word_byte(*b))
        {
            end = i;
            break;
        }
        i += 1;
    }
    let out = collapse_ws(body[..end].trim());
    if out.is_empty() { None } else { Some(out) }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Extract the canonical visibility modifier from a trimmed introducer
/// line. Handles `pub`, `pub(crate)`, `pub(super)`, `pub(self)`, and
/// `pub(in path::...)`. Returns an empty string for private items. The
/// check is anchored at the start of the line so `fn pub_thing()` does
/// not confuse it.
fn extract_visibility(line: &str) -> String {
    let s = line.trim_start();
    if !s.starts_with("pub") {
        return String::new();
    }
    let after = &s[3..];
    let first = after.chars().next();
    match first {
        None => "pub".to_string(),
        Some(c) if c.is_whitespace() => "pub".to_string(),
        Some('(') => {
            // Walk to the matching `)`. The content between decides the
            // variant (`crate`, `super`, `self`, `in path::...`).
            let close = after.find(')').unwrap_or(after.len());
            let inside = after[1..close].trim();
            if inside == "crate"
                || inside == "super"
                || inside == "self"
                || inside.starts_with("in ")
            {
                format!("pub({inside})")
            } else {
                // Unknown `pub(...)` content - fall back to a canonical
                // string so mismatched shapes still compare unequal.
                format!("pub({inside})")
            }
        }
        _ => String::new(),
    }
}

/// Compare signatures extracted from `old_lines` and `new_code`. Returns
/// `Some(note)` when either side parses AND the parameter types or return
/// type differ. When either side fails to parse we return `None` so the
/// guard degrades to a no-op rather than crashing on macros or exotic
/// syntax.
fn detect_signature_change(old_lines: &[&str], new_code: &str) -> Option<String> {
    let old_sig = extract_signature(old_lines.iter().copied())?;
    let new_sig = extract_signature(new_code.lines())?;
    if old_sig.params != new_sig.params {
        return Some(format!(
            "signature change: params {:?} -> {:?}",
            old_sig.params, new_sig.params,
        ));
    }
    if old_sig.return_type != new_sig.return_type {
        return Some(format!(
            "signature change: return type {:?} -> {:?}",
            old_sig.return_type, new_sig.return_type,
        ));
    }
    None
}

/// Compare the visibility modifier extracted from the first real
/// introducer line of each side. A change from `pub` to private (or any
/// variant flip) silently alters downstream compilation across every
/// importer; surface it as a refusal-worthy structural change.
fn detect_visibility_change(old_lines: &[&str], new_code: &str) -> Option<String> {
    let old_line = first_real_introducer_line(old_lines.iter().copied())?;
    let new_line = first_real_introducer_line(new_code.lines())?;
    let old_vis = extract_visibility(old_line);
    let new_vis = extract_visibility(new_line);
    if old_vis == new_vis {
        return None;
    }
    let fmt = |v: &str| {
        if v.is_empty() {
            "<private>".to_string()
        } else {
            v.to_string()
        }
    };
    Some(format!(
        "visibility change: {} -> {}",
        fmt(&old_vis),
        fmt(&new_vis),
    ))
}

/// Extract the defined identifier from the first real introducer line of
/// `new_code` and compare it to the resolved `sym.name`. Returns a
/// caller-facing error when the names disagree so a hidden rename
/// disguised as a replace is surfaced instead of silently leaving
/// every importer dangling.
fn check_identifier_match(
    sym: &crate::storage::models::SymbolRow,
    new_code: &str,
) -> Option<String> {
    let line = first_real_introducer_line(new_code.lines())?;
    let defined = extract_defined_identifier(line)?;
    if defined == sym.name {
        return None;
    }
    Some(format!(
        "Refusing to replace '{}' ({}): `new_code` defines '{}' but the target symbol is '{}'. Use qartez_rename for renames, or change `new_code` to match '{}'.",
        sym.name, sym.kind, defined, sym.name, sym.name,
    ))
}

/// Return the identifier declared on `line`. Handles the common Rust
/// shapes (`fn name(`, `pub fn name(`, `struct Name<`, `struct Name {`,
/// `struct Name;`, `enum Name`, `trait Name`, `type Name =`, `const
/// Name:`, `static Name:`, `mod name`, `macro_rules! name`), the
/// Python / TS shapes (`def name(`, `class Name(`, `interface Name {`,
/// `function name(`), and falls back to "None" when the first
/// non-modifier token is not one the table knows. `None` is treated
/// as "no signature check possible"; the introducer shape check
/// already handled the body-splice case.
fn extract_defined_identifier(line: &str) -> Option<String> {
    const STRIP: &[&str] = &[
        "pub(crate)",
        "pub(super)",
        "pub(self)",
        "pub",
        "async",
        "unsafe",
        "extern",
        "export",
        "default",
        "declare",
        "public",
        "private",
        "protected",
        "readonly",
        "abstract",
    ];
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start();
        let mut advanced = false;
        for s in STRIP {
            if let Some(after) = trimmed.strip_prefix(s)
                && matches!(
                    after.chars().next(),
                    Some(c) if c.is_whitespace() || c == '(' || c == '!'
                )
            {
                rest = after;
                advanced = true;
                break;
            }
        }
        if !advanced {
            break;
        }
    }
    let trimmed = rest.trim_start();
    // `macro_rules! name` lives outside the table-driven introducers.
    if let Some(after) = trimmed.strip_prefix("macro_rules!") {
        return Some(
            after
                .trim()
                .trim_end_matches([' ', '{', '('])
                .trim()
                .to_string(),
        )
        .filter(|s| !s.is_empty());
    }
    const KEYWORDS: &[&str] = &[
        "fn",
        "struct",
        "trait",
        "impl",
        "enum",
        "const",
        "static",
        "type",
        "mod",
        "def",
        "class",
        "interface",
        "function",
    ];
    for kw in KEYWORDS {
        if let Some(after) = trimmed.strip_prefix(kw)
            && matches!(after.chars().next(), Some(c) if c.is_whitespace())
        {
            let name_part = after.trim_start();
            // Stop at the first delimiter that ends the identifier.
            let end = name_part
                .find(|c: char| {
                    !(c.is_alphanumeric() || c == '_' || c == ':' || c == '\'' || c == '.')
                })
                .unwrap_or(name_part.len());
            let raw = &name_part[..end];
            // `impl` blocks have no single defined identifier (they
            // impl a type or a trait for a type). Skip the name check
            // for `impl` to avoid false positives; the introducer
            // shape check is sufficient there.
            if *kw == "impl" {
                return None;
            }
            let cleaned = raw.trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
            if cleaned.is_empty() {
                return None;
            }
            return Some(cleaned.to_string());
        }
    }
    None
}

/// Ensure `new_code` carries at most a single top-level item: the
/// definition. When the first real line opens a brace-delimited block,
/// walk the string tracking brace depth (skipping characters inside
/// `"..."`, `'...'`, `//` line comments, and `/* ... */` block
/// comments) and refuse when non-whitespace, non-comment content
/// survives after the matching closing brace. For `;`-terminated
/// items (`struct Foo;`, `const X: u32 = 0;`, `type T = U;`) the
/// check instead scans for trailing non-comment tokens after the
/// first top-level `;`.
fn check_trailing_content(new_code: &str) -> Option<String> {
    let trimmed_end = new_code.trim_end_matches('\n');
    let bytes = trimmed_end.as_bytes();
    // Find the first non-whitespace, non-comment character and inspect
    // the rest relative to the definition shape. Walking the whole
    // string with a state machine handles braces-in-strings and
    // comments-in-strings correctly.
    let mut i = 0;
    let n = bytes.len();
    let mut depth: i32 = 0;
    let mut saw_open_brace = false;
    let mut closed_at: Option<usize> = None;
    let mut semicolon_at: Option<usize> = None;
    while i < n {
        let b = bytes[i];
        // Skip line comments.
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Skip block comments.
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < n {
                i += 2;
            } else {
                i = n;
            }
            continue;
        }
        // Skip string literals.
        if b == b'"' {
            i += 1;
            while i < n {
                if bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // Skip char literals (Rust).
        if b == b'\'' {
            i += 1;
            while i < n {
                if bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'\'' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if b == b'{' {
            saw_open_brace = true;
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 && saw_open_brace && closed_at.is_none() {
                closed_at = Some(i + 1);
            }
        } else if b == b';' && depth == 0 && !saw_open_brace && semicolon_at.is_none() {
            semicolon_at = Some(i + 1);
        }
        i += 1;
    }

    // Pick the index past which nothing substantive may appear. When both
    // a closing brace and a semicolon are seen, use whichever came first -
    // the first-item boundary. Otherwise `struct Foo;\npub fn bar() {}`
    // would walk past the struct's semicolon to the fn's closing brace
    // and let the trailing fn through silently.
    let end = match (closed_at, semicolon_at) {
        (Some(a), Some(b)) => a.min(b),
        (Some(e), None) => e,
        (None, Some(e)) => e,
        (None, None) => return None,
    };

    // Inspect what follows `end`; allow whitespace and comments only.
    let mut j = end;
    while j < n {
        let b = bytes[j];
        if b.is_ascii_whitespace() {
            j += 1;
            continue;
        }
        if b == b'/' && j + 1 < n && bytes[j + 1] == b'/' {
            while j < n && bytes[j] != b'\n' {
                j += 1;
            }
            continue;
        }
        if b == b'/' && j + 1 < n && bytes[j + 1] == b'*' {
            j += 2;
            while j + 1 < n && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                j += 1;
            }
            if j + 1 < n {
                j += 2;
            } else {
                j = n;
            }
            continue;
        }
        // Anything else is trailing content and must be rejected.
        let tail = trimmed_end[end..].trim();
        let preview: String = tail.chars().take(80).collect();
        return Some(format!(
            "Refusing to replace: `new_code` has trailing content after the definition's closing brace/semicolon. `qartez_replace_symbol` expects exactly one top-level item. Offending suffix: '{preview}'",
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        check_trailing_content, detect_kind_change, detect_signature_change,
        detect_visibility_change, extract_defined_identifier, extract_introducer_token,
        extract_signature, extract_visibility, first_real_introducer_line, is_prelude_line,
        strip_leading_bom,
    };

    #[test]
    fn attribute_prelude_is_skipped() {
        let old: &[&str] = &["#[derive(Debug)]", "pub struct Foo;"];
        let new = "#[derive(Clone)]\npub struct Foo;\n";
        // No kind change even with different attributes.
        assert!(detect_kind_change(old, new).is_none());
    }

    #[test]
    fn doc_and_attribute_prelude_both_skipped() {
        let old: &[&str] = &["/// Doc.", "#[inline]", "pub fn foo() {}"];
        assert_eq!(
            first_real_introducer_line(old.iter().copied()),
            Some("pub fn foo() {}"),
        );
    }

    #[test]
    fn struct_to_fn_change_detected() {
        let old: &[&str] = &["pub struct Foo;"];
        let new = "pub fn foo() -> u32 { 0 }\n";
        assert_eq!(
            detect_kind_change(old, new).as_deref(),
            Some("kind change: struct → fn"),
        );
    }

    #[test]
    fn prelude_line_matches_attributes_and_comments() {
        assert!(is_prelude_line("#[derive(Debug)]"));
        assert!(is_prelude_line("#![allow(unused)]"));
        assert!(is_prelude_line("/// doc"));
        assert!(is_prelude_line("//! module doc"));
        assert!(is_prelude_line("// ordinary comment"));
        assert!(is_prelude_line("@decorator"));
        assert!(!is_prelude_line("pub fn foo() {}"));
        assert!(!is_prelude_line("use std::io;"));
    }

    #[test]
    fn introducer_token_extracted_past_visibility_modifiers() {
        assert_eq!(extract_introducer_token("pub fn foo() {}"), Some("fn"));
        assert_eq!(
            extract_introducer_token("pub(crate) async fn bar() {}"),
            Some("fn"),
        );
        assert_eq!(extract_introducer_token("pub struct Foo;"), Some("struct"));
        assert_eq!(extract_introducer_token("enum E { A }"), Some("enum"));
        assert_eq!(extract_introducer_token("use std::io;"), None);
    }

    #[test]
    fn strip_leading_bom_removes_single_bom() {
        assert_eq!(strip_leading_bom("\u{FEFF}fn foo() {}"), "fn foo() {}");
        assert_eq!(strip_leading_bom("fn foo() {}"), "fn foo() {}");
    }

    #[test]
    fn extract_defined_identifier_finds_name_past_modifiers() {
        assert_eq!(
            extract_defined_identifier("pub fn foo() {}").as_deref(),
            Some("foo"),
        );
        assert_eq!(
            extract_defined_identifier("pub(crate) async fn bar(x: u32) -> u32 { x }").as_deref(),
            Some("bar"),
        );
        assert_eq!(
            extract_defined_identifier("pub struct Foo { a: u32 }").as_deref(),
            Some("Foo"),
        );
        assert_eq!(
            extract_defined_identifier("struct Tuple(u32);").as_deref(),
            Some("Tuple"),
        );
        assert_eq!(
            extract_defined_identifier("enum E { A }").as_deref(),
            Some("E"),
        );
        assert_eq!(
            extract_defined_identifier("def foo(self):").as_deref(),
            Some("foo"),
        );
        assert_eq!(
            extract_defined_identifier("class Bar(Base):").as_deref(),
            Some("Bar"),
        );
        // `impl` blocks are skipped intentionally - they have no
        // single defined identifier.
        assert_eq!(extract_defined_identifier("impl Foo { }"), None);
    }

    #[test]
    fn trailing_content_accepts_single_item() {
        assert!(check_trailing_content("fn foo() -> u32 { 0 }").is_none());
        assert!(check_trailing_content("fn foo() -> u32 { 0 }\n").is_none());
        assert!(check_trailing_content("pub struct Foo;").is_none());
        assert!(
            check_trailing_content("fn foo() {\n    let s = \"}\";\n    println!(\"{}\", s);\n}")
                .is_none(),
        );
        // Trailing comment only is fine.
        assert!(check_trailing_content("fn foo() {}\n// trailing explanation only").is_none(),);
    }

    #[test]
    fn trailing_content_rejects_extra_items() {
        assert!(check_trailing_content("fn foo() { 0 }\nstuff();\ngarbage;").is_some());
        assert!(check_trailing_content("fn foo() -> u32 { 0 } garbage").is_some());
        assert!(check_trailing_content("struct Foo;\npub fn bar() {}").is_some());
    }

    #[test]
    fn visibility_extraction_covers_canonical_forms() {
        assert_eq!(extract_visibility("pub fn foo() {}"), "pub");
        assert_eq!(extract_visibility("pub(crate) fn foo() {}"), "pub(crate)");
        assert_eq!(extract_visibility("pub(super) fn foo() {}"), "pub(super)");
        assert_eq!(extract_visibility("pub(self) fn foo() {}"), "pub(self)");
        assert_eq!(
            extract_visibility("pub(in crate::server) fn foo() {}"),
            "pub(in crate::server)",
        );
        assert_eq!(extract_visibility("fn foo() {}"), "");
    }

    #[test]
    fn visibility_change_detection() {
        let old: &[&str] = &["pub fn foo() {}"];
        assert!(detect_visibility_change(old, "fn foo() {}\n").is_some());
        assert!(detect_visibility_change(old, "pub fn foo() { 1 }\n").is_none());
        let old_priv: &[&str] = &["fn foo() {}"];
        assert!(detect_visibility_change(old_priv, "pub(crate) fn foo() {}\n").is_some());
    }

    #[test]
    fn signature_change_rename_is_not_a_change() {
        let old: &[&str] = &["fn foo(x: u32, y: u32) -> u32 { x + y }"];
        // Rename parameter: still same param types, no signature change.
        let new = "fn foo(a: u32, b: u32) -> u32 { a + b }\n";
        assert!(detect_signature_change(old, new).is_none());
    }

    #[test]
    fn signature_change_type_flip_is_detected() {
        let old: &[&str] = &["fn foo(x: u32) -> u32 { x }"];
        let new = "fn foo(x: u64) -> u32 { x }\n";
        assert!(detect_signature_change(old, new).is_some());
    }

    #[test]
    fn signature_change_return_type_flip_is_detected() {
        let old: &[&str] = &["fn foo(x: u32) -> u32 { x }"];
        let new = "fn foo(x: u32) -> u64 { x as u64 }\n";
        assert!(detect_signature_change(old, new).is_some());
    }

    #[test]
    fn signature_extraction_handles_generics_and_self() {
        let sig = extract_signature(
            ["fn bar<T: Clone>(&self, a: Result<T, E>) -> Option<T> {"]
                .iter()
                .copied(),
        )
        .expect("fn with generics must parse");
        assert_eq!(
            sig.params,
            vec!["&self".to_string(), "Result<T, E>".to_string()]
        );
        assert_eq!(sig.return_type.as_deref(), Some("Option<T>"));
        assert_eq!(sig.visibility, "");
    }
}
