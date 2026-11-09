//! Helpers shared by every per-language parser in `src/index/languages/*.rs`.
//!
//! Each language module used to carry its own copy of these tiny utilities
//! (child iteration, quote stripping, signature truncation). Centralising
//! them here keeps the extraction logic consistent across languages and is
//! the place to fix subtle bugs (UTF-8 truncation, for instance) once.

use tree_sitter::Node;

use crate::str_utils::floor_char_boundary;

pub(super) const MAX_SIGNATURE_LEN: usize = 200;

pub(super) fn children(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    (0..node.child_count()).filter_map(move |i| node.child(i))
}

pub(super) fn unquote(s: &str) -> String {
    let trimmed = s.trim();
    // `len() >= 2` rules out a lone quote character, which would otherwise
    // slice `trimmed[1..0]` and panic. Every pre-refactor copy except
    // protobuf's had this bug; centralising fixes it everywhere.
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

pub(super) fn first_line_signature(node: Node<'_>, source: &[u8]) -> Option<String> {
    first_line_signature_str(node_text_opt(node, source)?)
}

pub(super) fn brace_or_first_line_signature(node: Node<'_>, source: &[u8]) -> Option<String> {
    brace_or_first_line_signature_str(node_text_opt(node, source)?)
}

/// Lossy node text: returns the UTF-8 slice for a node, or an empty string if
/// the span is not valid UTF-8. Used across every tree-sitter language parser.
pub(super) fn node_text(node: Node<'_>, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    std::str::from_utf8(&source[start..end])
        .unwrap_or("")
        .to_string()
}

fn node_text_opt<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    std::str::from_utf8(&source[start..end]).ok()
}

fn first_line_signature_str(text: &str) -> Option<String> {
    let first_line = text.lines().next().unwrap_or(text).trim();
    if first_line.is_empty() {
        return None;
    }
    Some(truncate_at_char_boundary(first_line).to_string())
}

fn brace_or_first_line_signature_str(text: &str) -> Option<String> {
    let sig = if let Some(brace_pos) = text.find('{') {
        text[..brace_pos].trim()
    } else {
        text.lines().next().unwrap_or(text).trim()
    };

    if sig.is_empty() {
        return None;
    }

    Some(truncate_at_char_boundary(sig).to_string())
}

fn truncate_at_char_boundary(s: &str) -> &str {
    if s.len() > MAX_SIGNATURE_LEN {
        &s[..floor_char_boundary(s, MAX_SIGNATURE_LEN)]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod unquote {
        use super::*;

        #[test]
        fn plain_text_unchanged() {
            assert_eq!(unquote("hello"), "hello");
        }

        #[test]
        fn strips_double_quotes() {
            assert_eq!(unquote("\"hello\""), "hello");
        }

        #[test]
        fn strips_single_quotes() {
            assert_eq!(unquote("'hello'"), "hello");
        }

        #[test]
        fn trims_whitespace_before_stripping() {
            assert_eq!(unquote("  \"hello\"  "), "hello");
            assert_eq!(unquote("\n\t'hello'\t\n"), "hello");
        }

        #[test]
        fn leaves_mismatched_quotes_alone() {
            assert_eq!(unquote("\"hello"), "\"hello");
            assert_eq!(unquote("hello\""), "hello\"");
            assert_eq!(unquote("\"hello'"), "\"hello'");
            assert_eq!(unquote("'hello\""), "'hello\"");
        }

        #[test]
        fn empty_string_stays_empty() {
            assert_eq!(unquote(""), "");
            assert_eq!(unquote("   "), "");
        }

        #[test]
        fn single_quote_char_unchanged() {
            assert_eq!(unquote("\""), "\"");
            assert_eq!(unquote("'"), "'");
        }

        #[test]
        fn empty_quoted_string_becomes_empty() {
            assert_eq!(unquote("\"\""), "");
            assert_eq!(unquote("''"), "");
        }

        #[test]
        fn preserves_inner_whitespace() {
            assert_eq!(unquote("\"hello world\""), "hello world");
            assert_eq!(unquote("'  inner  '"), "  inner  ");
        }

        #[test]
        fn preserves_utf8_content() {
            assert_eq!(unquote("\"привет\""), "привет");
            assert_eq!(unquote("'你好'"), "你好");
            assert_eq!(unquote("\"🎉\""), "🎉");
        }

        #[test]
        fn does_not_strip_nested_mixed_quotes() {
            assert_eq!(unquote("\"'nested'\""), "'nested'");
            assert_eq!(unquote("'\"nested\"'"), "\"nested\"");
        }

        #[test]
        fn nested_same_quotes_preserved() {
            assert_eq!(
                unquote("\"outer \"inner\" outer\""),
                "outer \"inner\" outer"
            );
        }
    }

    mod first_line_signature_str {
        use super::*;

        #[test]
        fn empty_returns_none() {
            assert_eq!(first_line_signature_str(""), None);
            assert_eq!(first_line_signature_str("   "), None);
            assert_eq!(first_line_signature_str("\n\n\n"), None);
        }

        #[test]
        fn single_line_returned() {
            assert_eq!(
                first_line_signature_str("fn hello() {}"),
                Some("fn hello() {}".into())
            );
        }

        #[test]
        fn multi_line_returns_only_first() {
            assert_eq!(
                first_line_signature_str("fn hello() {\n    body();\n}"),
                Some("fn hello() {".into())
            );
        }

        #[test]
        fn leading_whitespace_trimmed() {
            assert_eq!(
                first_line_signature_str("    fn hello()"),
                Some("fn hello()".into())
            );
        }

        #[test]
        fn trailing_whitespace_trimmed() {
            assert_eq!(
                first_line_signature_str("fn hello()   \t"),
                Some("fn hello()".into())
            );
        }

        #[test]
        fn short_signature_passthrough() {
            let sig = "def f()";
            assert_eq!(first_line_signature_str(sig), Some(sig.into()));
        }

        #[test]
        fn exact_200_bytes_not_truncated() {
            let sig = "x".repeat(200);
            let result = first_line_signature_str(&sig).unwrap();
            assert_eq!(result.len(), 200);
            assert_eq!(result, sig);
        }

        #[test]
        fn ascii_over_200_truncated_to_200() {
            let sig = "x".repeat(300);
            let result = first_line_signature_str(&sig).unwrap();
            assert_eq!(result.len(), 200);
            assert_eq!(result, "x".repeat(200));
        }

        #[test]
        fn utf8_boundary_safe_over_200() {
            let sig = "а".repeat(101);
            assert_eq!(sig.len(), 202);
            let result = first_line_signature_str(&sig).unwrap();
            assert!(result.len() <= 200);
            assert!(result.chars().all(|c| c == 'а'));
        }

        #[test]
        fn utf8_boundary_rounds_down() {
            let mut sig = "x".repeat(199);
            sig.push('а');
            assert_eq!(sig.len(), 201);
            let result = first_line_signature_str(&sig).unwrap();
            assert_eq!(result.len(), 199);
            assert_eq!(result, "x".repeat(199));
        }

        #[test]
        fn emoji_at_boundary_safe() {
            let mut sig = "x".repeat(198);
            sig.push('\u{1F600}');
            sig.push_str("xx");
            assert!(sig.len() > 200);
            let result = first_line_signature_str(&sig).unwrap();
            assert_eq!(result.len(), 198);
        }

        #[test]
        fn preserves_internal_whitespace() {
            assert_eq!(
                first_line_signature_str("fn  foo  (a: i32)  -> i32"),
                Some("fn  foo  (a: i32)  -> i32".into())
            );
        }
    }

    mod brace_or_first_line_signature_str {
        use super::*;

        #[test]
        fn no_brace_falls_through_to_first_line() {
            assert_eq!(
                brace_or_first_line_signature_str("pub fn bar() -> i32"),
                Some("pub fn bar() -> i32".into())
            );
        }

        #[test]
        fn brace_on_first_line_cuts_at_brace() {
            assert_eq!(
                brace_or_first_line_signature_str("fn hello() { body }"),
                Some("fn hello()".into())
            );
        }

        #[test]
        fn brace_on_later_line_takes_multi_line_pre_brace() {
            assert_eq!(
                brace_or_first_line_signature_str(
                    "fn long_sig(\n    arg: i32,\n) -> i32 {\n    body\n}"
                ),
                Some("fn long_sig(\n    arg: i32,\n) -> i32".into())
            );
        }

        #[test]
        fn empty_returns_none() {
            assert_eq!(brace_or_first_line_signature_str(""), None);
            assert_eq!(brace_or_first_line_signature_str("   "), None);
            assert_eq!(brace_or_first_line_signature_str("{"), None);
            assert_eq!(brace_or_first_line_signature_str("  {  body }"), None);
        }

        #[test]
        fn brace_first_char_returns_none() {
            assert_eq!(brace_or_first_line_signature_str("{ body }"), None);
        }

        #[test]
        fn trailing_whitespace_before_brace_trimmed() {
            assert_eq!(
                brace_or_first_line_signature_str("fn foo()   {"),
                Some("fn foo()".into())
            );
        }

        #[test]
        fn multi_line_no_brace_returns_first_line_only() {
            assert_eq!(
                brace_or_first_line_signature_str("fn foo()\n    -> i32\n    where T: Copy"),
                Some("fn foo()".into())
            );
        }

        #[test]
        fn short_signature_passthrough() {
            assert_eq!(
                brace_or_first_line_signature_str("interface Foo"),
                Some("interface Foo".into())
            );
        }

        #[test]
        fn over_200_ascii_truncated() {
            let sig = "x".repeat(300);
            let result = brace_or_first_line_signature_str(&sig).unwrap();
            assert_eq!(result.len(), 200);
        }

        #[test]
        fn utf8_safe_over_200_no_brace() {
            let sig = "а".repeat(101);
            assert_eq!(sig.len(), 202);
            let result = brace_or_first_line_signature_str(&sig).unwrap();
            assert!(result.len() <= 200);
            assert!(result.chars().all(|c| c == 'а'));
        }

        #[test]
        fn utf8_safe_pre_brace_over_200() {
            let mut sig = "а".repeat(101);
            sig.push_str(" { body }");
            let result = brace_or_first_line_signature_str(&sig).unwrap();
            assert!(result.len() <= 200);
            assert!(result.chars().all(|c| c == 'а'));
        }
    }

    mod behavioural_equivalence_with_old_impls {
        use super::*;

        fn old_first_line_byte_slice(text: &str) -> Option<String> {
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

        fn old_first_line_floor(text: &str) -> Option<String> {
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

        fn old_brace_floor(text: &str) -> Option<String> {
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

        fn old_brace_byte_slice(text: &str) -> Option<String> {
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

        fn ascii_fixtures() -> Vec<String> {
            vec![
                String::new(),
                "   ".into(),
                "\n\n".into(),
                "fn hello()".into(),
                "fn hello() {}".into(),
                "fn hello() {\n    body\n}".into(),
                "def foo(x, y):\n    return x + y".into(),
                "interface Foo { bar(): void }".into(),
                "class X extends Y implements Z {".into(),
                "type Option<T> = ...".into(),
                "x".repeat(200),
                "x".repeat(201),
                "x".repeat(300),
                "   leading whitespace   \n  second line".into(),
                "func() { body_with_braces: { nested } }".into(),
                "   { brace_first }".into(),
                "fn sig(\n    a: i32,\n    b: i32,\n) -> i32 {".into(),
            ]
        }

        #[test]
        fn first_line_ascii_matches_old_byte_slice() {
            for input in ascii_fixtures() {
                assert_eq!(
                    first_line_signature_str(&input),
                    old_first_line_byte_slice(&input),
                    "first_line (byte-slice) divergence on input: {input:?}"
                );
            }
        }

        #[test]
        fn first_line_ascii_matches_old_floor() {
            for input in ascii_fixtures() {
                assert_eq!(
                    first_line_signature_str(&input),
                    old_first_line_floor(&input),
                    "first_line (floor) divergence on input: {input:?}"
                );
            }
        }

        #[test]
        fn brace_ascii_matches_old_floor() {
            for input in ascii_fixtures() {
                assert_eq!(
                    brace_or_first_line_signature_str(&input),
                    old_brace_floor(&input),
                    "brace (floor) divergence on input: {input:?}"
                );
            }
        }

        #[test]
        fn brace_ascii_matches_old_byte_slice() {
            for input in ascii_fixtures() {
                assert_eq!(
                    brace_or_first_line_signature_str(&input),
                    old_brace_byte_slice(&input),
                    "brace (byte-slice) divergence on input: {input:?}"
                );
            }
        }

        #[test]
        fn new_impl_never_panics_on_utf8_where_old_would() {
            let mut sig_a = "x".repeat(199);
            sig_a.push('а');
            sig_a.push_str(&"a".repeat(50));
            let panicked = std::panic::catch_unwind(|| old_first_line_byte_slice(&sig_a));
            assert!(
                panicked.is_err(),
                "old byte-slice implementation should panic on this input"
            );
            let new = first_line_signature_str(&sig_a).expect("new impl must not fail");
            assert!(new.len() <= 200);

            // 99 'а' = 198 bytes, +1 'x' at byte 198, +1 'а' spans bytes 199..201.
            // Byte 200 is mid-character -> old byte-slice path panics.
            let mut pre_brace = "а".repeat(99);
            pre_brace.push('x');
            pre_brace.push('а');
            pre_brace.push_str(&"y".repeat(30));
            let sig_b = format!("{pre_brace} {{ body }}");
            let panicked = std::panic::catch_unwind(|| old_brace_byte_slice(&sig_b));
            assert!(
                panicked.is_err(),
                "old brace byte-slice implementation should panic on this input"
            );
            let new = brace_or_first_line_signature_str(&sig_b).expect("new impl must not fail");
            assert!(new.len() <= 200);
        }
    }

    mod children_iter {
        use super::*;
        use tree_sitter::Parser;

        fn parse_rust(src: &str) -> tree_sitter::Tree {
            let mut parser = Parser::new();
            parser
                .set_language(&tree_sitter::Language::new(tree_sitter_rust::LANGUAGE))
                .unwrap();
            parser.parse(src, None).unwrap()
        }

        #[test]
        fn iterates_over_all_direct_children() {
            let tree = parse_rust("fn a() {} fn b() {}");
            let root = tree.root_node();
            assert_eq!(children(root).count(), 2);
        }

        #[test]
        fn empty_for_leaf_node() {
            let tree = parse_rust("fn a() {}");
            let root = tree.root_node();
            fn find_leaf(node: Node<'_>) -> Node<'_> {
                if node.child_count() == 0 {
                    return node;
                }
                find_leaf(node.child(0).unwrap())
            }
            let leaf = find_leaf(root);
            assert_eq!(children(leaf).count(), 0);
        }
    }

    mod signature_via_real_parse {
        use super::*;
        use tree_sitter::Parser;

        fn parse_rust(src: &str) -> (tree_sitter::Tree, Vec<u8>) {
            let mut parser = Parser::new();
            parser
                .set_language(&tree_sitter::Language::new(tree_sitter_rust::LANGUAGE))
                .unwrap();
            let tree = parser.parse(src, None).unwrap();
            (tree, src.as_bytes().to_vec())
        }

        #[test]
        fn first_line_signature_matches_string_helper() {
            let src = "fn hello() {\n    let x = 1;\n}";
            let (tree, source) = parse_rust(src);
            let fn_node = tree.root_node().child(0).unwrap();
            let from_node = first_line_signature(fn_node, &source);
            let from_str = first_line_signature_str(&src[fn_node.start_byte()..fn_node.end_byte()]);
            assert_eq!(from_node, from_str);
            assert_eq!(from_node, Some("fn hello() {".into()));
        }

        #[test]
        fn brace_signature_cuts_at_brace() {
            let src = "fn sig(a: i32, b: i32) -> i32 {\n    a + b\n}";
            let (tree, source) = parse_rust(src);
            let fn_node = tree.root_node().child(0).unwrap();
            let from_node = brace_or_first_line_signature(fn_node, &source);
            assert_eq!(from_node, Some("fn sig(a: i32, b: i32) -> i32".into()));
        }

        #[test]
        fn both_return_none_for_empty_node() {
            let (tree, source) = parse_rust("");
            let root = tree.root_node();
            assert_eq!(first_line_signature(root, &source), None);
            assert_eq!(brace_or_first_line_signature(root, &source), None);
        }

        #[test]
        fn invalid_utf8_returns_none() {
            let src = "fn a() {}";
            let (tree, _) = parse_rust(src);
            let node = tree.root_node().child(0).unwrap();
            let mut bad_source = vec![0xFF, 0xFE, 0xFD];
            while bad_source.len() < node.end_byte() {
                bad_source.push(0xFF);
            }
            assert_eq!(first_line_signature(node, &bad_source), None);
            assert_eq!(brace_or_first_line_signature(node, &bad_source), None);
        }
    }

    /// Behavioural-equivalence check: the new `pub(super) node_text` in
    /// `common.rs` must return *exactly* the same `String` that every
    /// per-language `fn node_text(node, source) -> String` did before the
    /// migration. This module pins that contract so any future tweak to
    /// `node_text` cannot silently regress the 31 callers that used to own
    /// their own copy.
    mod node_text_equivalence {
        use super::*;
        use tree_sitter::Parser;

        /// Verbatim copy of the old per-language implementation that lived in
        /// every `qartez-public/src/index/languages/<lang>.rs` before the
        /// migration. Used as the oracle in the equivalence tests below.
        fn old_per_language_node_text(node: tree_sitter::Node<'_>, source: &[u8]) -> String {
            let start = node.start_byte();
            let end = node.end_byte().min(source.len());
            std::str::from_utf8(&source[start..end])
                .unwrap_or("")
                .to_string()
        }

        fn parse_rust(src: &str) -> tree_sitter::Tree {
            let mut parser = Parser::new();
            parser
                .set_language(&tree_sitter::Language::new(tree_sitter_rust::LANGUAGE))
                .unwrap();
            parser.parse(src, None).unwrap()
        }

        fn walk_all<'tree>(
            node: tree_sitter::Node<'tree>,
            out: &mut Vec<tree_sitter::Node<'tree>>,
        ) {
            out.push(node);
            for child in children(node) {
                walk_all(child, out);
            }
        }

        #[test]
        fn matches_old_impl_for_every_node_in_a_real_parse() {
            let src = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\nstruct Point { x: i32, y: i32 }\nconst K: u32 = 7;\n";
            let tree = parse_rust(src);
            let mut nodes = Vec::new();
            walk_all(tree.root_node(), &mut nodes);
            assert!(
                nodes.len() > 20,
                "fixture should produce a non-trivial tree, got {}",
                nodes.len()
            );
            for node in nodes {
                assert_eq!(
                    node_text(node, src.as_bytes()),
                    old_per_language_node_text(node, src.as_bytes()),
                    "divergence at byte range {}..{} kind={}",
                    node.start_byte(),
                    node.end_byte(),
                    node.kind(),
                );
            }
        }

        #[test]
        fn returns_empty_string_for_invalid_utf8() {
            let src = "fn a() {}";
            let tree = parse_rust(src);
            let node = tree.root_node().child(0).unwrap();
            let mut bad_source = vec![0xFF, 0xFE, 0xFD];
            while bad_source.len() < node.end_byte() {
                bad_source.push(0xFF);
            }
            // Old behaviour: `unwrap_or("").to_string()` - empty string, NOT None.
            // This is the contract the 31 callers relied on (many used
            // `.find()`, `.starts_with()`, `.is_empty()` on the result).
            assert_eq!(node_text(node, &bad_source), "");
            assert_eq!(
                node_text(node, &bad_source),
                old_per_language_node_text(node, &bad_source)
            );
        }

        #[test]
        fn clamps_end_byte_to_source_len() {
            // Old impl had `let end = node.end_byte().min(source.len());`.
            // Verify the new helper preserves that guard - feed a node whose
            // end_byte is past the source length and confirm no panic + same
            // result as the oracle.
            let src = "fn a() {}";
            let tree = parse_rust(src);
            let node = tree.root_node().child(0).unwrap();
            let truncated_source = &src.as_bytes()[..3];
            let new = node_text(node, truncated_source);
            let old = old_per_language_node_text(node, truncated_source);
            assert_eq!(new, old, "truncated-source clamp must match old impl");
            // Either both sides return "" (truncation falls inside a multi-byte
            // boundary or a non-UTF8 cut point) or both return a prefix.
            assert!(new == "fn " || new.is_empty());
        }

        #[test]
        fn empty_source_returns_empty() {
            let tree = parse_rust("");
            let root = tree.root_node();
            assert_eq!(node_text(root, b""), "");
        }

        #[test]
        fn single_char_node_round_trips() {
            let src = "fn a() {}";
            let tree = parse_rust(src);
            let mut nodes = Vec::new();
            walk_all(tree.root_node(), &mut nodes);
            // Every leaf token under 4 bytes (identifiers, parens, braces) must
            // round-trip identically.
            for node in nodes
                .iter()
                .filter(|n| n.child_count() == 0 && (n.end_byte() - n.start_byte()) <= 4)
            {
                let new = node_text(*node, src.as_bytes());
                let old = old_per_language_node_text(*node, src.as_bytes());
                assert_eq!(new, old);
            }
        }

        #[test]
        fn utf8_content_preserved() {
            // Cyrillic identifier, mixed with ASCII syntax, plus emoji in a
            // string literal. tree-sitter-rust accepts non-ASCII identifiers,
            // so `привет` parses cleanly.
            let src = "fn привет() -> &'static str { \"hello 🎉\" }";
            let tree = parse_rust(src);
            let mut nodes = Vec::new();
            walk_all(tree.root_node(), &mut nodes);
            for node in nodes {
                assert_eq!(
                    node_text(node, src.as_bytes()),
                    old_per_language_node_text(node, src.as_bytes()),
                    "UTF-8 divergence at byte range {}..{} kind={}",
                    node.start_byte(),
                    node.end_byte(),
                    node.kind(),
                );
            }
        }

        #[test]
        fn whole_program_round_trip_matches_source_slice() {
            // The root node always spans the entire source. node_text on it
            // should reproduce the full source byte-for-byte (provided UTF-8).
            let src = "fn main() {\n    let x = 42;\n    println!(\"{}\", x);\n}";
            let tree = parse_rust(src);
            let root = tree.root_node();
            assert_eq!(node_text(root, src.as_bytes()), src);
        }
    }
}
