// Rust guideline compliant 2026-04-15

//! Free-standing helper functions shared across tool handlers.

use std::collections::HashMap;

use crate::str_utils::floor_char_boundary;

/// Resolve a prefixed multi-root path (e.g. `"MyAlias/src/lib.rs"`) to an
/// absolute path by matching its first component against known roots and
/// aliases. Returns `None` when there is only one root or no prefix matches.
pub(super) fn resolve_prefixed_path(
    path: &std::path::Path,
    roots: &[std::path::PathBuf],
    aliases: &HashMap<std::path::PathBuf, String>,
) -> Option<std::path::PathBuf> {
    if roots.len() <= 1 {
        return None;
    }
    let first = match path.components().next() {
        Some(std::path::Component::Normal(n)) => n,
        _ => return None,
    };
    let first_str = first.to_string_lossy();
    for root in roots {
        let alias = aliases.get(root).map(|s| s.as_str());
        let matches = if let Some(a) = alias {
            first_str == a
        } else {
            root.file_name()
                .map(|n| n.to_string_lossy() == first_str)
                .unwrap_or(false)
        };
        if matches {
            let remainder: std::path::PathBuf = path.components().skip(1).collect();
            return Some(root.join(remainder));
        }
    }
    None
}

pub(super) const DEFAULT_TOKEN_BUDGET: usize = 4000;

pub(super) fn elide_file_source(
    project_root: &std::path::Path,
    project_roots: &[std::path::PathBuf],
    root_aliases: &HashMap<std::path::PathBuf, String>,
    file_path: &str,
    symbols: &[crate::storage::models::SymbolRow],
    token_budget_remaining: usize,
) -> Option<String> {
    let path = std::path::Path::new(file_path);
    let abs_path = resolve_prefixed_path(path, project_roots, root_aliases)
        .unwrap_or_else(|| project_root.join(file_path));

    let source = std::fs::read_to_string(&abs_path).ok()?;
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return None;
    }

    let mut sorted: Vec<&crate::storage::models::SymbolRow> =
        symbols.iter().filter(|s| s.is_exported).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by_key(|s| s.line_start);

    let mut out = String::new();
    let mut last_shown_line: usize = 0;

    for sym in &sorted {
        let start = (sym.line_start as usize).saturating_sub(1);
        let end = (sym.line_end as usize).min(lines.len());
        if start >= lines.len() || start >= end {
            continue;
        }

        if start > last_shown_line + 1 {
            out.push_str("⋯\n");
        }

        let body_kinds = ["function", "method", "constructor"];
        if body_kinds.contains(&sym.kind.as_str()) {
            let sym_text: String = lines[start..end].join("\n");
            if let Some(brace_pos) = sym_text.find('{') {
                let before = sym_text[..brace_pos].trim_end();
                out.push_str(before);
                out.push_str(" {⋯}\n");
            } else {
                out.push_str(lines[start]);
                out.push_str(" {⋯}\n");
            }
        } else {
            let span = end - start;
            if span <= 5 {
                for line in &lines[start..end] {
                    out.push_str(line);
                    out.push('\n');
                }
            } else {
                for line in &lines[start..(start + 2).min(end)] {
                    out.push_str(line);
                    out.push('\n');
                }
                out.push_str("    ⋯\n");
                if end > 0 {
                    out.push_str(lines[end - 1]);
                    out.push('\n');
                }
            }
        }

        last_shown_line = end;

        if estimate_tokens(&out) > token_budget_remaining {
            out.push_str("⋯\n");
            break;
        }
    }

    if !out.is_empty() { Some(out) } else { None }
}

pub(super) fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        path.to_string()
    } else if max_len <= 3 {
        path[..floor_char_boundary(path, max_len)].to_string()
    } else {
        let start = floor_char_boundary(path, path.len() - (max_len - 3));
        format!("...{}", &path[start..])
    }
}

// Approximate Claude token count: ~3 characters per token for code.
// Uses char count (not byte length) so multibyte Unicode does not inflate
// the estimate. This is a soft budget hint, not a hard limit.
pub(super) fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / 3
}

/// Render priority-sorted items under a token budget.
///
/// Items are emitted highest-priority-first until the budget is exhausted.
/// At least one item is always emitted so the caller never gets empty output.
pub(super) fn budget_render(items: &[(f64, String)], budget_tokens: usize) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut sorted: Vec<&(f64, String)> = items.iter().collect();
    sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut out = String::new();
    for (emitted, (_, line)) in sorted.iter().enumerate() {
        if emitted > 0 && estimate_tokens(&out) + estimate_tokens(line) > budget_tokens {
            let remaining = sorted.len() - emitted;
            if remaining > 0 {
                out.push_str(&format!("[truncated: {} more items]\n", remaining));
            }
            return out;
        }
        out.push_str(line);
    }
    out
}

pub(super) fn human_bytes(bytes: i64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let n = bytes as f64;
    if n >= GB {
        format!("{:.1}G", n / GB)
    } else if n >= MB {
        format!("{:.1}M", n / MB)
    } else if n >= KB {
        format!("{:.1}K", n / KB)
    } else {
        format!("{bytes}B")
    }
}

/// Per-signal contribution to a `qartez_context` candidate score. Kept as a
/// separate struct (rather than a flat f64) so `explain=true` can print the
/// breakdown and callers can reason about why a given file ranked where it did.
#[derive(Debug, Default, Clone)]
pub(super) struct ScoreBreakdown {
    pub imports: f64,
    pub importer: f64,
    pub cochange: f64,
    pub transitive: f64,
    pub task_match: f64,
}

impl ScoreBreakdown {
    pub(super) fn total(&self) -> f64 {
        self.imports + self.importer + self.cochange + self.transitive + self.task_match
    }

    pub(super) fn reasons(&self) -> Vec<&'static str> {
        let mut r = Vec::new();
        if self.imports > 0.0 {
            r.push("imports");
        }
        if self.importer > 0.0 {
            r.push("importer");
        }
        if self.cochange > 0.0 {
            r.push("cochange");
        }
        if self.transitive > 0.0 {
            r.push("transitive");
        }
        if self.task_match > 0.0 {
            r.push("task-match");
        }
        r
    }

    pub(super) fn explain(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.imports > 0.0 {
            parts.push(format!("imports={:.1}", self.imports));
        }
        if self.importer > 0.0 {
            parts.push(format!("importer={:.1}", self.importer));
        }
        if self.cochange > 0.0 {
            parts.push(format!("cochange={:.1}", self.cochange));
        }
        if self.transitive > 0.0 {
            parts.push(format!("transitive={:.1}", self.transitive));
        }
        if self.task_match > 0.0 {
            parts.push(format!("task-match={:.1}", self.task_match));
        }
        parts.join(" + ")
    }
}

pub(super) fn replace_whole_word(text: &str, old: &str, new: &str) -> String {
    if old.is_empty() {
        return text.to_string();
    }
    let mut result = String::with_capacity(text.len());
    let mut start = 0;
    while let Some(pos) = text[start..].find(old) {
        let abs_pos = start + pos;
        let before_ok = text[..abs_pos]
            .chars()
            .next_back()
            .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_');
        let after_pos = abs_pos + old.len();
        let after_ok = text[after_pos..]
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_');

        if before_ok && after_ok {
            result.push_str(&text[start..abs_pos]);
            result.push_str(new);
        } else {
            result.push_str(&text[start..abs_pos + old.len()]);
        }
        start = after_pos;
    }
    result.push_str(&text[start..]);
    result
}

/// Walk up to `commit_limit` recent commits from HEAD, count co-change pairs
/// involving `target_path`, and return the top `limit` partners descending.
///
/// Commits touching more than `max_commit_size` files are skipped - they are
/// typically format passes, bulk renames, or lockfile bumps whose pair counts
/// drown the signal from real feature work.
///
/// Returns `None` only when git is unavailable in `project_root`.
pub(super) fn compute_cochange_pairs(
    project_root: &std::path::Path,
    target_path: &str,
    max_commit_size: usize,
    commit_limit: usize,
    limit: usize,
) -> Option<Vec<(String, u32)>> {
    let repo = git2::Repository::discover(project_root).ok()?;
    let head_oid = repo.head().ok()?.target()?;
    let mut revwalk = repo.revwalk().ok()?;
    revwalk.set_sorting(git2::Sort::TIME).ok()?;
    revwalk.push(head_oid).ok()?;

    let mut counts: HashMap<String, u32> = HashMap::new();
    for oid_result in revwalk.take(commit_limit) {
        let Ok(oid) = oid_result else { continue };
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        let Ok(tree) = commit.tree() else { continue };
        let parent_tree = if commit.parent_count() > 0 {
            commit.parent(0).ok().and_then(|p| p.tree().ok())
        } else {
            None
        };
        let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) else {
            continue;
        };
        // Collect both the pre- and post-commit paths from each delta so
        // deletions (empty `new_file`) and renames (diverging old vs. new
        // paths) still contribute to the co-change tally. Relying solely
        // on `new_file` silently dropped these cases, under-counting real
        // signal for files that were moved or removed in a commit.
        let mut files: Vec<String> = diff
            .deltas()
            .flat_map(|d| {
                let old = d
                    .old_file()
                    .path()
                    .and_then(|p| p.to_str())
                    .map(|s| s.to_string());
                let new = d
                    .new_file()
                    .path()
                    .and_then(|p| p.to_str())
                    .map(|s| s.to_string());
                [old, new].into_iter().flatten()
            })
            .collect();
        files.sort();
        files.dedup();
        if files.len() < 2 || files.len() > max_commit_size {
            continue;
        }
        if !files.iter().any(|f| f == target_path) {
            continue;
        }
        for f in &files {
            if f != target_path {
                *counts.entry(f.clone()).or_insert(0) += 1;
            }
        }
    }

    let mut pairs: Vec<(String, u32)> = counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs.truncate(limit);
    Some(pairs)
}

/// Heuristic: return true for paths that look like test files (so they can be
/// excluded from blast radius and aggregate counts by default). Covers common
/// conventions across Rust, Go, TypeScript/JavaScript, Python, Java, and Ruby.
pub(super) fn is_test_path(path: &str) -> bool {
    const TEST_DIR_PREFIXES: &[&str] = &["tests/", "test/", "benches/", "__tests__/", "spec/"];
    const TEST_DIR_SUBSTRINGS: &[&str] =
        &["/tests/", "/test/", "/benches/", "/__tests__/", "/spec/"];
    const TEST_FILE_EXACT: &[&str] = &["test.rs", "tests.rs"];
    const TEST_FILE_SUFFIXES: &[&str] = &[
        "_test.rs",
        "_tests.rs",
        "_test.go",
        "_test.dart",
        ".test.ts",
        ".spec.ts",
        ".test.tsx",
        ".spec.tsx",
        ".test.js",
        ".spec.js",
        ".test.jsx",
        ".spec.jsx",
        "_test.py",
        "Test.java",
        "Tests.java",
        "Test.kt",
        "Tests.kt",
        "_spec.rb",
        "Test.cs",
        "Tests.cs",
    ];

    if TEST_DIR_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return true;
    }
    if TEST_DIR_SUBSTRINGS.iter().any(|p| path.contains(p)) {
        return true;
    }
    let Some(name) = path.rsplit('/').next() else {
        return false;
    };
    if TEST_FILE_EXACT.contains(&name) {
        return true;
    }
    if TEST_FILE_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        return true;
    }
    name.starts_with("test_") && name.ends_with(".py")
}

/// Returns true if the Rust source at `path` contains inline test markers
/// (`#[test]`, `#[cfg(test)]`, or custom test attributes like `#[tokio::test]`).
///
/// Used by `test_gaps` to avoid flagging idiomatic Rust files with inline
/// `mod tests` blocks as "untested" just because no external test file imports
/// them. Non-Rust paths and unreadable files return `false`.
pub(super) fn has_inline_rust_tests(project_root: &std::path::Path, path: &str) -> bool {
    if !path.ends_with(".rs") {
        return false;
    }
    let abs_path = project_root.join(path);
    let Ok(source) = std::fs::read_to_string(&abs_path) else {
        return false;
    };
    if source.contains("#[cfg(test)]") || source.contains("#[test]") {
        return true;
    }
    // Accept path-qualified test attributes like `#[tokio::test]` but not
    // incidental `::test]` occurrences inside macros or array indexing
    // (e.g. `vec![mod::test]`). The attribute must open with `#[` and the
    // preceding text between `#[` and `::test]` must be a bare path without
    // intervening whitespace or brackets.
    for (idx, _) in source.match_indices("::test]") {
        let before = &source[..idx];
        if let Some(attr_start) = before.rfind("#[") {
            let between = &before[attr_start + 2..];
            if !between.is_empty()
                && between
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b':' || b == b'_')
            {
                return true;
            }
        }
    }
    false
}

/// Convert a file path or symbol name into a valid Mermaid node ID.
///
/// Mermaid node IDs must be alphanumeric (plus underscores). Characters like
/// `/`, `.`, `-`, and `::` are replaced with `_`. Leading digits get a prefix.
pub(super) fn mermaid_node_id(name: &str) -> String {
    let mut id = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            id.push(ch);
        } else {
            id.push('_');
        }
    }
    if id.starts_with(|c: char| c.is_ascii_digit()) {
        id.insert(0, 'n');
    }
    if id.is_empty() {
        id.push_str("node");
    }
    id
}

/// Escape a label for Mermaid quoted node labels (`["..."]`).
///
/// Mermaid interprets `"` and `]` inside bracket labels, so they must be
/// replaced with safe alternatives.
pub(super) fn mermaid_label(label: &str) -> String {
    label.replace('"', "'").replace(']', ")")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_empty_text_returns_empty() {
        assert_eq!(replace_whole_word("", "foo", "bar"), "");
    }

    #[test]
    fn replace_no_match_returns_input_unchanged() {
        assert_eq!(replace_whole_word("abc xyz", "foo", "bar"), "abc xyz");
    }

    #[test]
    fn replace_exact_match_returns_replacement() {
        assert_eq!(replace_whole_word("foo", "foo", "bar"), "bar");
    }

    #[test]
    fn replace_at_start() {
        assert_eq!(replace_whole_word("foo bar", "foo", "X"), "X bar");
    }

    #[test]
    fn replace_at_end() {
        assert_eq!(replace_whole_word("bar foo", "foo", "X"), "bar X");
    }

    #[test]
    fn replace_multiple_occurrences() {
        assert_eq!(replace_whole_word("foo foo foo", "foo", "X"), "X X X");
    }

    #[test]
    fn replace_respects_alphanumeric_prefix() {
        assert_eq!(replace_whole_word("xfoo", "foo", "X"), "xfoo");
    }

    #[test]
    fn replace_respects_alphanumeric_suffix() {
        assert_eq!(replace_whole_word("foox", "foo", "X"), "foox");
    }

    #[test]
    fn replace_respects_digit_prefix() {
        assert_eq!(replace_whole_word("1foo", "foo", "X"), "1foo");
    }

    #[test]
    fn replace_respects_digit_suffix() {
        assert_eq!(replace_whole_word("foo1", "foo", "X"), "foo1");
    }

    #[test]
    fn replace_respects_underscore_prefix() {
        assert_eq!(replace_whole_word("_foo", "foo", "X"), "_foo");
    }

    #[test]
    fn replace_respects_underscore_suffix() {
        assert_eq!(replace_whole_word("foo_", "foo", "X"), "foo_");
    }

    #[test]
    fn replace_after_punctuation() {
        assert_eq!(replace_whole_word(".foo", "foo", "X"), ".X");
        assert_eq!(replace_whole_word("(foo)", "foo", "X"), "(X)");
    }

    #[test]
    fn replace_before_punctuation() {
        assert_eq!(replace_whole_word("foo.", "foo", "X"), "X.");
        assert_eq!(replace_whole_word("foo;bar", "foo", "X"), "X;bar");
    }

    #[test]
    fn replace_surrounded_by_whitespace() {
        assert_eq!(replace_whole_word(" foo ", "foo", "X"), " X ");
        assert_eq!(replace_whole_word("\tfoo\n", "foo", "X"), "\tX\n");
    }

    #[test]
    fn replace_blocked_by_non_ascii_alphanumeric_prefix() {
        // Greek Omega is a Letter in Unicode, counts as alphanumeric.
        assert_eq!(replace_whole_word("\u{03A9}foo", "foo", "X"), "\u{03A9}foo");
    }

    #[test]
    fn replace_blocked_by_cjk_prefix() {
        // CJK ideograph is a Letter in Unicode.
        assert_eq!(replace_whole_word("\u{6587}foo", "foo", "X"), "\u{6587}foo");
    }

    #[test]
    fn replace_allowed_after_non_alphanumeric_unicode_punctuation() {
        // « is punctuation, not alphanumeric.
        assert_eq!(
            replace_whole_word("\u{00AB}foo\u{00BB}", "foo", "X"),
            "\u{00AB}X\u{00BB}"
        );
    }

    #[test]
    fn replace_does_not_match_substring_of_longer_word() {
        // `foo` inside `foobar` must not be replaced.
        assert_eq!(replace_whole_word("foobar", "foo", "X"), "foobar");
        assert_eq!(replace_whole_word("barfoo", "foo", "X"), "barfoo");
    }

    #[test]
    fn replace_handles_mixed_whole_and_partial_matches() {
        // First `foo` is standalone, second is part of `foobar`.
        assert_eq!(
            replace_whole_word("foo foobar foo", "foo", "X"),
            "X foobar X"
        );
    }

    #[test]
    fn replace_with_longer_replacement() {
        assert_eq!(replace_whole_word("foo bar", "foo", "LONGER"), "LONGER bar");
    }

    #[test]
    fn replace_with_shorter_replacement() {
        assert_eq!(replace_whole_word("LONGER bar", "LONGER", "X"), "X bar");
    }

    #[test]
    fn replace_preserves_trailing_content_after_non_matching_occurrence() {
        // `foox` is blocked, `foo` at end is allowed, verify trailing text.
        assert_eq!(
            replace_whole_word("foox and foo tail", "foo", "X"),
            "foox and X tail"
        );
    }

    #[test]
    fn replace_identical_old_and_new_is_idempotent_for_valid_matches() {
        assert_eq!(replace_whole_word("foo bar", "foo", "foo"), "foo bar");
    }

    #[test]
    fn replace_non_match_position_advances_past_old() {
        // After a blocked match at `xfoo`, the scan must resume at position
        // just past the `old` we tried, not loop forever.
        assert_eq!(replace_whole_word("xfoo foo", "foo", "X"), "xfoo X");
    }

    #[test]
    fn replace_empty_old_returns_text_unchanged() {
        assert_eq!(replace_whole_word("foo bar", "", "X"), "foo bar");
        assert_eq!(replace_whole_word("", "", "X"), "");
    }

    #[test]
    fn replace_empty_old_with_non_empty_new_returns_text_unchanged() {
        assert_eq!(
            replace_whole_word("alpha beta", "", "INSERTED"),
            "alpha beta"
        );
    }

    #[test]
    fn replace_empty_old_with_unicode_text_returns_text_unchanged() {
        assert_eq!(replace_whole_word("α β γ", "", "X"), "α β γ");
    }

    #[test]
    fn replace_non_empty_old_empty_new_removes_old_as_whole_word() {
        assert_eq!(replace_whole_word("foo bar foo", "foo", ""), " bar ");
    }

    #[test]
    fn replace_whole_word_is_idempotent_on_repeated_empty_old() {
        let input = "untouched text";
        let once = replace_whole_word(input, "", "X");
        let twice = replace_whole_word(&once, "", "X");
        let thrice = replace_whole_word(&twice, "", "X");
        assert_eq!(once, input);
        assert_eq!(twice, input);
        assert_eq!(thrice, input);
    }

    /// Reference implementation mirroring the inline word-boundary scan in
    /// `qartez_rename`'s non-tree-sitter fallback. This exists to assert that
    /// the per-line rename logic agrees with `replace_whole_word` on every
    /// input, so both code paths share a single tested semantic.
    fn per_line_rename(content: &str, old: &str, new: &str) -> String {
        let mut out = String::new();
        for (idx, line) in content.lines().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            let mut new_line = String::new();
            let mut start = 0;
            while let Some(pos) = line[start..].find(old) {
                let abs_pos = start + pos;
                let before_ok = line[..abs_pos]
                    .chars()
                    .next_back()
                    .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_');
                let after_pos = abs_pos + old.len();
                let after_ok = line[after_pos..]
                    .chars()
                    .next()
                    .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_');
                if before_ok && after_ok {
                    new_line.push_str(&line[start..abs_pos]);
                    new_line.push_str(new);
                } else {
                    new_line.push_str(&line[start..abs_pos + old.len()]);
                }
                start = abs_pos + old.len();
            }
            new_line.push_str(&line[start..]);
            out.push_str(&new_line);
        }
        out
    }

    #[test]
    fn per_line_rename_matches_replace_whole_word_on_single_line_inputs() {
        let cases = [
            ("foo", "foo", "X"),
            ("foo bar", "foo", "X"),
            ("xfoo", "foo", "X"),
            ("foox", "foo", "X"),
            ("_foo", "foo", "X"),
            ("foo_", "foo", "X"),
            (".foo.", "foo", "X"),
            ("foo foo foo", "foo", "X"),
            ("foo foobar foo", "foo", "X"),
            ("\u{03A9}foo", "foo", "X"),
            ("\u{00AB}foo\u{00BB}", "foo", "X"),
            ("", "foo", "X"),
            ("abc", "foo", "X"),
        ];
        for (text, old, new) in cases {
            let whole = replace_whole_word(text, old, new);
            let lined = per_line_rename(text, old, new);
            assert_eq!(
                whole, lined,
                "divergence on input ({text:?}, {old:?}, {new:?})"
            );
        }
    }

    #[test]
    fn per_line_rename_handles_multi_line_independently() {
        // Each line is scanned separately - a word on line 1 does not affect
        // matching on line 2. Verifies the fallback's per-line iteration
        // does not carry state between lines.
        let input = "foo\nfoox\n_foo\nfoo bar";
        let expected = "X\nfoox\n_foo\nX bar";
        assert_eq!(per_line_rename(input, "foo", "X"), expected);
    }

    #[test]
    fn budget_render_all_fit() {
        let items = vec![
            (1.0, "line one\n".to_string()),
            (2.0, "line two\n".to_string()),
        ];
        let out = budget_render(&items, 4000);
        assert!(out.contains("line two"));
        assert!(out.contains("line one"));
        assert!(!out.contains("[truncated"));
    }

    #[test]
    fn budget_render_truncates() {
        let items: Vec<(f64, String)> = (0..100)
            .map(|i| {
                (
                    i as f64,
                    format!("item number {i} with some padding text here\n"),
                )
            })
            .collect();
        let out = budget_render(&items, 100);
        assert!(out.contains("[truncated:"));
        assert!(out.contains("more items]"));
        assert!(out.contains("item number 99"));
    }

    #[test]
    fn budget_render_priority_ordering() {
        let items = vec![
            (1.0, "low priority\n".to_string()),
            (3.0, "high priority\n".to_string()),
            (2.0, "mid priority\n".to_string()),
        ];
        let out = budget_render(&items, 4000);
        let high_pos = out.find("high priority").unwrap();
        let mid_pos = out.find("mid priority").unwrap();
        let low_pos = out.find("low priority").unwrap();
        assert!(high_pos < mid_pos);
        assert!(mid_pos < low_pos);
    }

    #[test]
    fn budget_render_empty() {
        let out = budget_render(&[], 4000);
        assert!(out.is_empty());
    }
}

#[cfg(test)]
mod has_inline_rust_tests_tests {
    use super::has_inline_rust_tests;
    use tempfile::TempDir;

    fn make_rust_file(contents: &str) -> (TempDir, &'static str) {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("file.rs"), contents).unwrap();
        (tmp, "file.rs")
    }

    #[test]
    fn detects_plain_test_attr() {
        let (tmp, rel) = make_rust_file("#[test]\nfn t() {}\n");
        assert!(has_inline_rust_tests(tmp.path(), rel));
    }

    #[test]
    fn detects_cfg_test() {
        let (tmp, rel) = make_rust_file("#[cfg(test)]\nmod tests {}\n");
        assert!(has_inline_rust_tests(tmp.path(), rel));
    }

    #[test]
    fn detects_tokio_test() {
        let (tmp, rel) = make_rust_file("#[tokio::test]\nasync fn t() {}\n");
        assert!(
            has_inline_rust_tests(tmp.path(), rel),
            "path-qualified test attrs must still count"
        );
    }

    #[test]
    fn detects_async_std_test() {
        let (tmp, rel) = make_rust_file("#[async_std::test]\nasync fn t() {}\n");
        assert!(has_inline_rust_tests(tmp.path(), rel));
    }

    #[test]
    fn rejects_macro_indexing_false_positive() {
        // The previous `contains("::test]")` heuristic flagged this as
        // having inline tests - it is the regression being fixed.
        let (tmp, rel) = make_rust_file("let _ = vec![mod::test];\n");
        assert!(
            !has_inline_rust_tests(tmp.path(), rel),
            "vec![mod::test] must not count as an inline test"
        );
    }

    #[test]
    fn rejects_partial_macro_false_positive() {
        let (tmp, rel) = make_rust_file("macro_call!(Foo::test];\n");
        assert!(!has_inline_rust_tests(tmp.path(), rel));
    }

    #[test]
    fn rejects_file_without_any_tests() {
        let (tmp, rel) = make_rust_file("pub fn regular() {}\n");
        assert!(!has_inline_rust_tests(tmp.path(), rel));
    }

    #[test]
    fn rejects_non_rust_extension() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("file.py"), "#[test]").unwrap();
        assert!(!has_inline_rust_tests(tmp.path(), "file.py"));
    }
}
