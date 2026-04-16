// Rust guideline compliant 2026-04-15

//! Free-standing helper functions shared across tool handlers.

use std::collections::HashMap;

use crate::str_utils::floor_char_boundary;

pub(super) fn elide_file_source(
    project_root: &std::path::Path,
    file_path: &str,
    symbols: &[crate::storage::models::SymbolRow],
    token_budget_remaining: usize,
) -> Option<String> {
    let abs_path = project_root.join(file_path);
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
    let mut result = String::with_capacity(text.len());
    let mut start = 0;
    while let Some(pos) = text[start..].find(old) {
        let abs_pos = start + pos;
        let before_ok = if abs_pos == 0 {
            true
        } else {
            let ch = text[..abs_pos].chars().next_back().unwrap();
            !ch.is_alphanumeric() && ch != '_'
        };
        let after_pos = abs_pos + old.len();
        let after_ok = if after_pos >= text.len() {
            true
        } else {
            let ch = text[after_pos..].chars().next().unwrap();
            !ch.is_alphanumeric() && ch != '_'
        };

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
        let mut files: Vec<String> = diff
            .deltas()
            .filter_map(|d| {
                d.new_file()
                    .path()
                    .and_then(|p| p.to_str())
                    .map(|s| s.to_string())
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
    if path.starts_with("tests/")
        || path.starts_with("test/")
        || path.starts_with("benches/")
        || path.starts_with("__tests__/")
        || path.starts_with("spec/")
    {
        return true;
    }
    if path.contains("/tests/")
        || path.contains("/test/")
        || path.contains("/benches/")
        || path.contains("/__tests__/")
        || path.contains("/spec/")
    {
        return true;
    }
    if let Some(name) = path.rsplit('/').next() {
        if matches!(name, "test.rs" | "tests.rs") {
            return true;
        }
        if name.ends_with("_test.rs") || name.ends_with("_tests.rs") {
            return true;
        }
        if name.ends_with("_test.go") {
            return true;
        }
        if name.ends_with("_test.dart") {
            return true;
        }
        if name.ends_with(".test.ts")
            || name.ends_with(".spec.ts")
            || name.ends_with(".test.tsx")
            || name.ends_with(".spec.tsx")
            || name.ends_with(".test.js")
            || name.ends_with(".spec.js")
            || name.ends_with(".test.jsx")
            || name.ends_with(".spec.jsx")
        {
            return true;
        }
        if (name.starts_with("test_") && name.ends_with(".py")) || name.ends_with("_test.py") {
            return true;
        }
        if name.ends_with("Test.java")
            || name.ends_with("Tests.java")
            || name.ends_with("Test.kt")
            || name.ends_with("Tests.kt")
        {
            return true;
        }
        if name.ends_with("_spec.rb") {
            return true;
        }
        if name.ends_with("Test.cs") || name.ends_with("Tests.cs") {
            return true;
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
