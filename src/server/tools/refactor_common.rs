// Rust guideline compliant 2026-04-21

//! Shared helpers for `qartez_replace_symbol`, `qartez_insert_before_symbol`,
//! `qartez_insert_after_symbol`, and `qartez_safe_delete`.
//!
//! Each of those tools resolves a symbol by name (with optional `kind` and
//! `file_path` disambiguation), then rewrites a line range in the defining
//! file. The disambiguation logic is identical across all four, so it lives
//! here instead of being duplicated per tool. Byte-level rewriting is
//! different per tool and stays in the tool's own module.

use crate::storage::models::{FileRow, SymbolRow};
use crate::storage::read;

/// Resolve `name` (with optional `kind` and `file_path` filters) to a single
/// symbol + defining file. Mirrors the disambiguation flow in `qartez_move`
/// so callers get consistent error messages when a name is ambiguous.
///
/// Returns `Err(message)` when:
/// - no symbol matches the name,
/// - the `kind` filter excludes every match,
/// - the `file_path` filter excludes every match,
/// - multiple definitions remain after filtering (the caller must pick one).
pub(super) fn resolve_unique_symbol(
    conn: &rusqlite::Connection,
    name: &str,
    kind_hint: Option<&str>,
    file_path_hint: Option<&str>,
) -> Result<(SymbolRow, FileRow), String> {
    let mut results =
        read::find_symbol_by_name(conn, name).map_err(|e| format!("DB error: {e}"))?;

    if results.is_empty() {
        return Err(format!("No symbol found with name '{name}'"));
    }

    if let Some(k) = kind_hint.filter(|s| !s.is_empty()) {
        let available: Vec<String> = results
            .iter()
            .map(|(s, _)| s.kind.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        results.retain(|(s, _)| s.kind.eq_ignore_ascii_case(k));
        if results.is_empty() {
            return Err(format!(
                "No symbol '{name}' with kind '{k}'. Available kinds: {}",
                available.join(", "),
            ));
        }
    }

    if let Some(fp) = file_path_hint.filter(|s| !s.is_empty()) {
        let available: Vec<String> = results
            .iter()
            .map(|(_, f)| f.path.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        results.retain(|(_, f)| f.path == fp);
        if results.is_empty() {
            return Err(format!(
                "No symbol '{name}' in file '{fp}'. Available files: {}",
                available.join(", "),
            ));
        }
    }

    if results.len() > 1 {
        let locations: Vec<String> = results
            .iter()
            .map(|(s, f)| {
                format!(
                    "  {} ({}) in {} [L{}-L{}]",
                    s.name, s.kind, f.path, s.line_start, s.line_end
                )
            })
            .collect();
        // Canonical phrasing shared with every refactor/query tool that
        // routes through this helper. The three historical variants
        // ("Pass `kind` or `file_path`", "Pass `kind` and/or `file_path`",
        // and "Refusing to rename '<name>': multiple definitions found")
        // drifted apart enough that regression tests had to OR-check
        // for any of them. The wording below keeps the action-first
        // framing because either hint, or both, can disambiguate.
        return Err(format!(
            "Multiple definitions of '{name}' found. Pass `kind` and/or `file_path` to disambiguate:\n{}",
            locations.join("\n"),
        ));
    }

    Ok(results.remove(0))
}

/// Split `content` into lines and validate that the `[line_start..line_end]`
/// range recorded in the index is still in bounds. Returns the lines as a
/// `Vec<&str>` so callers can splice without re-splitting.
pub(super) fn validate_range<'a>(
    content: &'a str,
    sym: &SymbolRow,
    source_path: &str,
) -> Result<(Vec<&'a str>, usize, usize), String> {
    let lines: Vec<&'a str> = content.lines().collect();
    let start_idx = (sym.line_start as usize).saturating_sub(1);
    let end_idx = (sym.line_end as usize).min(lines.len());
    if start_idx >= lines.len() {
        return Err(format!(
            "Symbol line range L{}-L{} out of bounds for {} ({} lines). The index may be stale; re-run indexing.",
            sym.line_start,
            sym.line_end,
            source_path,
            lines.len(),
        ));
    }
    if start_idx >= end_idx {
        return Err(format!(
            "Invalid line range L{}-L{} for symbol '{}' in {}",
            sym.line_start, sym.line_end, sym.name, source_path,
        ));
    }
    Ok((lines, start_idx, end_idx))
}

/// Write `new_content` atomically to `abs_path` via a sibling tmp file +
/// rename. Preserves the source's trailing newline convention (POSIX) so
/// git diffs stay clean.
///
/// The tmp path carries a per-call nonce (pid + thread id + monotonic
/// counter) so two concurrent tool invocations racing on the same file
/// never land on the same tmp path. A shared tmp name would let one
/// call's `rename()` consume the other call's bytes before its own
/// `rename()` ran, yielding `ENOENT` on the second call.
///
/// When `abs_path` already exists, its permissions are copied onto the tmp
/// file before the rename. The rename replaces the inode, so without this
/// step a file `chmod 600` would silently downgrade to the umask default
/// (typically `0644`) after a refactor write, and an executable script
/// (`0755`) would lose its `+x` bit. The copy is best-effort: a permission
/// read or apply failure is logged but does not abort the write, since
/// proceeding with default permissions is still better than losing the
/// edit. New files (no prior `abs_path`) take whatever permissions the
/// process umask grants the tmp file, matching pre-fix behaviour.
pub(super) fn write_atomic(abs_path: &std::path::Path, new_content: &str) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let tid = format!("{:?}", std::thread::current().id());
    let tid_clean: String = tid.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let ext = abs_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("tmp");
    let tmp_name = format!("{ext}.qartez_edit_{pid}_{tid_clean}_{nonce}.tmp");
    let tmp_path = abs_path.with_extension(tmp_name);

    let original_perms = std::fs::metadata(abs_path).map(|m| m.permissions()).ok();

    std::fs::write(&tmp_path, new_content)
        .map_err(|e| format!("Cannot write {}: {e}", tmp_path.display()))?;

    if let Some(perms) = original_perms
        && let Err(e) = std::fs::set_permissions(&tmp_path, perms)
    {
        tracing::warn!(
            path = %tmp_path.display(),
            error = %e,
            "could not copy original file permissions to temp file; proceeding with umask defaults",
        );
    }

    std::fs::rename(&tmp_path, abs_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("Cannot rename temp file to {}: {e}", abs_path.display())
    })?;
    Ok(())
}

/// Join `lines` with `\n` and append a trailing `\n` when `preserve_trailing`
/// is true. `str::lines` strips the last `\n`, so a naive `join` corrupts
/// POSIX files - every caller here must go through this helper.
pub(super) fn join_lines_with_trailing(lines: &[&str], preserve_trailing: bool) -> String {
    let mut out = lines.join("\n");
    if preserve_trailing && !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}
