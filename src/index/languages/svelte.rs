//! Svelte single-file components (`.svelte`).
//!
//! A `.svelte` file is three languages at once: zero or more `<script>` blocks
//! (JavaScript or TypeScript), zero or more `<style>` blocks (CSS), and Svelte
//! markup in between. tree-sitter binds one grammar per parse, so the ordinary
//! `tree_sitter_language` + `extract` path cannot describe such a file. This
//! module therefore owns its whole parse through
//! [`LanguageSupport::parse_and_extract`]:
//!
//! 1. locate the `<script>` / `<style>` blocks with a byte scan;
//! 2. build a *masked* copy of the source for each embedded language - every
//!    byte outside the blocks of interest becomes a space, newlines are kept.
//!    The copy has the same length and the same line breaks as the real file,
//!    so every byte offset, line, and column tree-sitter reports is already
//!    correct for the original file: no offset arithmetic, no line shifting;
//! 3. parse the script mask with the TypeScript grammar (a syntactic superset
//!    of JavaScript, so a plain `<script>` block parses under it too) and the
//!    style mask with the CSS grammar, then reuse the existing TypeScript and
//!    CSS extractors verbatim;
//! 4. emit the component itself as a symbol named after the file stem, and
//!    scan the markup for capitalized element names (`<Button />`) so a
//!    component used only in markup still produces a reference edge.

use std::path::Path;

use tree_sitter::{Language, Parser, Tree};

use super::LanguageSupport;
use super::css::CssSupport;
use super::typescript::TypeScriptSupport;
use crate::index::symbols::{
    ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct SvelteSupport;

impl LanguageSupport for SvelteSupport {
    fn extensions(&self) -> &[&str] {
        &["svelte"]
    }

    fn language_name(&self) -> &str {
        "svelte"
    }

    /// Never used for parsing - [`Self::parse_and_extract`] owns the parse and
    /// the registry only calls this on the single-grammar path. Returning the
    /// TypeScript grammar keeps the trait total without inventing a language.
    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT)
    }

    /// Unreachable for `.svelte` files: the pipeline takes the
    /// [`Self::parse_and_extract`] branch. Kept as an empty result rather than
    /// a panic so a future caller cannot crash the indexer.
    fn extract(&self, _source: &[u8], _tree: &Tree) -> ParseResult {
        ParseResult::default()
    }

    fn parse_and_extract(&self, path: &Path, source: &[u8]) -> Option<ParseResult> {
        Some(extract_component(path, source))
    }
}

/// Embedded-language block of a Svelte file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Script,
    Style,
}

/// Byte range of a block's *content* (between `>` and `</tag`), never the tags.
#[derive(Debug, Clone, Copy)]
struct Block {
    kind: BlockKind,
    start: usize,
    end: usize,
}

fn extract_component(path: &Path, source: &[u8]) -> ParseResult {
    let blocks = find_blocks(source);
    let mut result = ParseResult::default();

    // The component itself, so `qartez_find Button` lands on Button.svelte.
    let component_idx = component_name(path).map(|name| {
        let idx = result.symbols.len();
        result.symbols.push(ExtractedSymbol {
            name,
            kind: SymbolKind::Component,
            line_start: 1,
            line_end: line_count(source),
            signature: None,
            is_exported: true,
            unused_excluded: false,
            parent_idx: None,
            complexity: None,
            owner_type: None,
        });
        idx
    });

    if blocks.iter().any(|b| b.kind == BlockKind::Script) {
        let masked = mask_outside(source, &blocks, BlockKind::Script);
        let lang = Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT);
        if let Some(tree) = parse_with(&lang, &masked) {
            append_shifted(&mut result, TypeScriptSupport.extract(&masked, &tree));
        }
    }

    if blocks.iter().any(|b| b.kind == BlockKind::Style) {
        let masked = mask_outside(source, &blocks, BlockKind::Style);
        let lang = Language::new(tree_sitter_css::LANGUAGE);
        if let Some(tree) = parse_with(&lang, &masked) {
            let mut css = CssSupport.extract(&masked, &tree);
            // Svelte styles are component-scoped: a selector is "used" by the
            // markup, not exported to other files. Leaving them in scope would
            // make `qartez_unused` report every rule of every component.
            for sym in &mut css.symbols {
                sym.unused_excluded = true;
            }
            append_shifted(&mut result, css);
        }
    }

    result
        .references
        .extend(markup_references(source, &blocks, component_idx));

    result
}

/// Append `src` to `dst`, shifting every index that points into the symbol
/// vector (`parent_idx`, `from_symbol_idx`) by the number of symbols already
/// collected. Extractors emit those indices relative to their own output, so
/// merging two parses without the shift would silently mis-parent symbols.
fn append_shifted(dst: &mut ParseResult, src: ParseResult) {
    let offset = dst.symbols.len();
    for mut sym in src.symbols {
        sym.parent_idx = sym.parent_idx.map(|idx| idx + offset);
        dst.symbols.push(sym);
    }
    for mut reference in src.references {
        reference.from_symbol_idx = reference.from_symbol_idx.map(|idx| idx + offset);
        dst.references.push(reference);
    }
    dst.imports.extend(src.imports);
    dst.type_relations.extend(src.type_relations);
}

fn parse_with(lang: &Language, source: &[u8]) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(lang).ok()?;
    parser.parse(source, None)
}

/// The component's symbol name: the file stem (`Button.svelte` -> `Button`,
/// `+page.svelte` -> `+page`). `None` for a path without a usable stem.
fn component_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    (!stem.is_empty()).then(|| stem.to_string())
}

/// Copy of `source` where every byte outside the blocks of `kind` is replaced
/// by a space, with `\n` and `\r` preserved. Same byte length and same line
/// breaks as the original, so tree-sitter positions need no translation.
fn mask_outside(source: &[u8], blocks: &[Block], kind: BlockKind) -> Vec<u8> {
    let mut masked: Vec<u8> = source
        .iter()
        .map(|&b| if b == b'\n' || b == b'\r' { b } else { b' ' })
        .collect();
    for block in blocks.iter().filter(|b| b.kind == kind) {
        masked[block.start..block.end].copy_from_slice(&source[block.start..block.end]);
    }
    masked
}

/// Locate every `<script>` and `<style>` block, returning the byte range of
/// each block's content. Self-closing tags (`<script src="..." />`) carry no
/// content and are skipped; an unterminated block runs to the end of the file,
/// so a truncated or half-written component still yields its symbols.
fn find_blocks(source: &[u8]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut i = 0usize;

    while i < source.len() {
        if source[i] != b'<' {
            i += 1;
            continue;
        }
        if let Some(after) = skip_comment(source, i) {
            i = after;
            continue;
        }
        let Some((kind, tag)) = open_tag_at(source, i) else {
            i += 1;
            continue;
        };
        let Some(open_end) = find_tag_end(source, i) else {
            break;
        };
        // `<script ... />` - no content to parse.
        if source[open_end - 1] == b'/' {
            i = open_end + 1;
            continue;
        }
        let content_start = open_end + 1;
        let close = format!("</{tag}");
        let close_at =
            find_ascii_ci(source, content_start, close.as_bytes()).unwrap_or(source.len());
        blocks.push(Block {
            kind,
            start: content_start,
            end: close_at,
        });
        i = close_at + close.len();
    }

    blocks
}

/// Offset of the `>` that closes the tag opening at `at`, skipping any `>`
/// inside a quoted attribute value. Svelte 5's typed components put one there
/// routinely - `<script lang="ts" generics="T extends Record<string, unknown>">`
/// - and taking the first `>` would cut the script block off mid-attribute.
fn find_tag_end(source: &[u8], at: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (offset, &byte) in source.iter().enumerate().skip(at) {
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'>' => return Some(offset),
                _ => {}
            },
        }
    }
    None
}

/// If an HTML comment opens at `at`, the offset just past its `-->` (or the end
/// of the file when unterminated). `None` when `at` does not open a comment.
/// Commented-out markup must not be read as live tags.
fn skip_comment(source: &[u8], at: usize) -> Option<usize> {
    if !source[at..].starts_with(b"<!--") {
        return None;
    }
    let body = (at + 4).min(source.len());
    Some(find_ascii_ci(source, body, b"-->").map_or(source.len(), |end| end + 3))
}

/// Whether `source[at..]` opens a `<script>` or `<style>` tag. The byte after
/// the tag name must not be a name character, so `<styled-thing>` and
/// `<scripts>` do not match.
fn open_tag_at(source: &[u8], at: usize) -> Option<(BlockKind, &'static str)> {
    for (tag, kind) in [("script", BlockKind::Script), ("style", BlockKind::Style)] {
        let end = at + 1 + tag.len();
        if end <= source.len()
            && source[at + 1..end].eq_ignore_ascii_case(tag.as_bytes())
            && source
                .get(end)
                .is_some_and(|b| matches!(b, b'>' | b'/' | b' ' | b'\t' | b'\n' | b'\r'))
        {
            return Some((kind, tag));
        }
    }
    None
}

fn find_ascii_ci(source: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    source[from..]
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
        .map(|p| from + p)
}

/// Component usages in the markup: `<Button ...>`, `<Modal/>`, `<Foo.Bar>`.
///
/// Svelte distinguishes a component from an HTML element by capitalization, so
/// a leading uppercase letter is the whole test. Without this, a component
/// imported in `<script>` and used only in markup would look unreferenced.
fn markup_references(
    source: &[u8],
    blocks: &[Block],
    from_symbol_idx: Option<usize>,
) -> Vec<ExtractedReference> {
    let mut refs = Vec::new();
    let mut i = 0usize;

    while i < source.len() {
        if source[i] != b'<' {
            i += 1;
            continue;
        }
        // Skip embedded blocks wholesale - their identifiers belong to the
        // TypeScript / CSS extractors, and `a < b` in a script is not a tag.
        if let Some(block) = blocks.iter().find(|b| i >= b.start && i < b.end) {
            i = block.end;
            continue;
        }
        // Commented-out markup is not a usage.
        if let Some(after) = skip_comment(source, i) {
            i = after;
            continue;
        }
        let name_start = i + 1;
        if !source.get(name_start).is_some_and(u8::is_ascii_uppercase) {
            i += 1;
            continue;
        }
        let mut end = name_start;
        while end < source.len()
            && (source[end].is_ascii_alphanumeric()
                || source[end] == b'_'
                || source[end] == b'$'
                || source[end] == b'.')
        {
            end += 1;
        }
        // `<Foo.Bar>` refers to the imported `Foo`.
        let name = String::from_utf8_lossy(&source[name_start..end]);
        let name = name.split('.').next().unwrap_or_default().to_string();
        if !name.is_empty() {
            refs.push(ExtractedReference {
                name,
                line: line_at(source, name_start),
                from_symbol_idx,
                kind: ReferenceKind::Use,
                ..Default::default()
            });
        }
        i = end;
    }

    refs
}

/// 1-based line number of byte `offset`.
fn line_at(source: &[u8], offset: usize) -> u32 {
    let newlines = source[..offset.min(source.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();
    u32::try_from(newlines + 1).unwrap_or(u32::MAX)
}

/// Total number of lines, so the component symbol spans the whole file.
fn line_count(source: &[u8]) -> u32 {
    let newlines = source.iter().filter(|&&b| b == b'\n').count();
    let trailing = usize::from(!source.ends_with(b"\n") && !source.is_empty());
    u32::try_from(newlines + trailing)
        .unwrap_or(u32::MAX)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPONENT: &str = r#"<script lang="ts">
  import Button from './Button.svelte';
  import { formatDate } from '$lib/date';

  export let title: string;

  interface Props {
    id: number;
  }

  function greet(name: string): string {
    return `hello ${name}`;
  }
</script>

<div class="card">
  <h1>{title}</h1>
  <Button on:click={() => greet(title)} />
</div>

<style>
  .card {
    color: red;
  }
</style>
"#;

    fn parse(name: &str, src: &str) -> ParseResult {
        SvelteSupport
            .parse_and_extract(Path::new(name), src.as_bytes())
            .expect("svelte support always returns a result")
    }

    fn names(result: &ParseResult) -> Vec<&str> {
        result.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn extracts_script_style_and_component_symbols() {
        let result = parse("src/lib/Card.svelte", COMPONENT);
        let names = names(&result);

        assert!(
            names.contains(&"Card"),
            "component symbol missing: {names:?}"
        );
        assert!(
            names.contains(&"greet"),
            "script function missing: {names:?}"
        );
        assert!(
            names.contains(&"Props"),
            "script interface missing: {names:?}"
        );
        assert!(names.contains(&".card"), "style rule missing: {names:?}");
    }

    #[test]
    fn component_symbol_is_a_component_kind_spanning_the_file() {
        let result = parse("src/lib/Card.svelte", COMPONENT);
        let component = result
            .symbols
            .iter()
            .find(|s| s.name == "Card")
            .expect("component symbol");

        assert_eq!(component.kind.as_str(), "component");
        assert_eq!(component.line_start, 1);
        assert_eq!(component.line_end, 25, "component must span the whole file");
        assert!(component.is_exported);
    }

    #[test]
    fn script_symbols_keep_their_real_line_numbers() {
        let result = parse("src/lib/Card.svelte", COMPONENT);
        let greet = result
            .symbols
            .iter()
            .find(|s| s.name == "greet")
            .expect("greet");

        // `function greet` is on line 11 of the file, not line 11 of the script.
        assert_eq!(greet.line_start, 11);
    }

    #[test]
    fn extracts_imports_from_the_script_block() {
        let result = parse("src/lib/Card.svelte", COMPONENT);
        let sources: Vec<&str> = result.imports.iter().map(|i| i.source.as_str()).collect();

        assert!(sources.contains(&"./Button.svelte"), "got {sources:?}");
        assert!(sources.contains(&"$lib/date"), "got {sources:?}");
    }

    #[test]
    fn markup_component_usage_becomes_a_reference() {
        let result = parse("src/lib/Card.svelte", COMPONENT);
        let button = result
            .references
            .iter()
            .find(|r| r.name == "Button")
            .expect("markup usage of <Button /> must produce a reference");

        assert_eq!(button.line, 18);
    }

    #[test]
    fn html_elements_are_not_component_references() {
        let result = parse("src/lib/Card.svelte", COMPONENT);
        for lowercase in ["div", "h1", "style", "script"] {
            assert!(
                !result.references.iter().any(|r| r.name == lowercase),
                "`<{lowercase}>` must not be treated as a component"
            );
        }
    }

    #[test]
    fn style_symbols_are_excluded_from_dead_code() {
        let result = parse("src/lib/Card.svelte", COMPONENT);
        let rule = result
            .symbols
            .iter()
            .find(|s| s.name == ".card")
            .expect(".card rule");

        assert!(rule.unused_excluded);
        // The style block sits below the markup: its line number only survives
        // because the mask keeps every newline of the skipped region.
        assert_eq!(rule.line_start, 22);
    }

    #[test]
    fn handles_module_and_instance_scripts() {
        let src = "<script context=\"module\">\n  export const prerender = true;\n</script>\n\n<script>\n  let count = 0;\n</script>\n\n<p>{count}</p>\n";
        let result = parse("+page.svelte", src);
        let names = names(&result);

        assert!(names.contains(&"prerender"), "got {names:?}");
        assert!(names.contains(&"count"), "got {names:?}");
        assert!(names.contains(&"+page"), "got {names:?}");
    }

    #[test]
    fn markup_only_component_still_indexes() {
        let result = parse("Empty.svelte", "<p>nothing here</p>\n");
        assert_eq!(names(&result), vec!["Empty"]);
    }

    #[test]
    fn self_closing_script_tag_is_skipped() {
        let result = parse("Ext.svelte", "<script src=\"./x.js\" />\n<p>hi</p>\n");
        assert_eq!(names(&result), vec!["Ext"]);
    }

    #[test]
    fn script_references_point_at_their_enclosing_symbol() {
        // The TypeScript extractor numbers its references against its own
        // symbol vector; merging it after the component symbol has to shift
        // those indices or every script reference lands on the wrong symbol.
        let src = "<script lang=\"ts\">\n  function helper(): number {\n    return 1;\n  }\n  function caller(): number {\n    return helper();\n  }\n</script>\n<p>{caller()}</p>\n";
        let result = parse("Calls.svelte", src);
        let call = result
            .references
            .iter()
            .find(|r| r.name == "helper" && r.kind == ReferenceKind::Call)
            .expect("call to helper()");
        let idx = call.from_symbol_idx.expect("enclosing symbol");

        assert_eq!(
            result.symbols[idx].name, "caller",
            "reference indices must be shifted past the component symbol"
        );
    }

    #[test]
    fn generics_attribute_with_an_angle_bracket_does_not_truncate_the_script() {
        // Svelte 5 typed components: the `>` closing `Record<...>` sits inside
        // a quoted attribute and must not be read as the end of the open tag.
        let src = "<script lang=\"ts\" generics=\"T extends Record<string, unknown>\">\n  function count(rows: T[]): number {\n    return rows.length;\n  }\n</script>\n<p>{count([])}</p>\n";
        let result = parse("Table.svelte", src);
        let count = result
            .symbols
            .iter()
            .find(|s| s.name == "count")
            .expect("script symbol must survive a `>` inside an attribute value");

        assert_eq!(count.line_start, 2);
    }

    #[test]
    fn commented_out_markup_is_not_a_component_usage() {
        let src = "<script>\n  import Ghost from './Ghost.svelte';\n</script>\n<!-- <Ghost /> was removed -->\n<p>none</p>\n";
        let result = parse("Commented.svelte", src);

        assert!(
            !result.references.iter().any(|r| r.name == "Ghost"),
            "a component inside an HTML comment is not a usage: {:?}",
            result.references
        );
    }

    #[test]
    fn unterminated_script_still_yields_its_symbols() {
        let result = parse(
            "Half.svelte",
            "<script lang=\"ts\">\n  export const HALF = 1;\n",
        );
        let names = names(&result);

        assert!(names.contains(&"HALF"), "got {names:?}");
    }

    #[test]
    fn crlf_source_keeps_symbols_and_line_numbers() {
        let src = "<script lang=\"ts\">\r\n  export const CRLF = 1;\r\n</script>\r\n<style>\r\n  .a {\r\n    color: red;\r\n  }\r\n</style>\r\n";
        let result = parse("Crlf.svelte", src);
        let konst = result
            .symbols
            .iter()
            .find(|s| s.name == "CRLF")
            .expect("const from a CRLF file");
        let rule = result.symbols.iter().find(|s| s.name == ".a").expect(".a");

        assert_eq!(konst.line_start, 2);
        assert_eq!(rule.line_start, 5);
    }

    #[test]
    fn empty_file_does_not_panic() {
        let result = parse("Empty.svelte", "");
        assert_eq!(names(&result), vec!["Empty"]);
    }

    #[test]
    fn non_ascii_markup_does_not_break_offsets() {
        let src = "<script lang=\"ts\">\n  const label = 'Заголовок';\n</script>\n<p>Привет - Мир</p>\n<Button />\n";
        let result = parse("Cyrillic.svelte", src);
        let button = result
            .references
            .iter()
            .find(|r| r.name == "Button")
            .expect("component after multi-byte text");

        assert_eq!(button.line, 5);
    }

    #[test]
    fn less_than_in_script_is_not_a_markup_tag() {
        let src = "<script>\n  const ok = A < B;\n</script>\n<p>x</p>\n";
        let result = parse("Cmp.svelte", src);

        assert!(
            !result.references.iter().any(|r| r.name == "B"),
            "`A < B` in a script must not be read as a `<B>` markup tag"
        );
    }
}
