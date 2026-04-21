// Rust guideline compliant 2026-04-21

#![allow(unused_imports)]

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};

use super::super::QartezServer;
use super::super::helpers::{self, *};
use super::super::params::*;
use super::super::tiers;
use super::super::treesitter::*;
use super::refactor_common;

use crate::graph::blast;
use crate::guard;
use crate::storage::read;
use crate::storage::read::sanitize_fts_query;
use crate::toolchain;

#[tool_router(router = qartez_move_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_move",
        description = "Move a symbol to another file and update all import paths automatically. Handles extraction, insertion, and importer rewrites in one step. Preview by default; set apply=true to execute.",
        annotations(
            title = "Move Symbol",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_move(
        &self,
        Parameters(params): Parameters<SoulMoveParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let (sym, source_file) = validate_source(
            &conn,
            &params.symbol,
            params.kind.as_deref(),
            &params.to_file,
        )?;

        let source_abs = self.safe_resolve(&source_file.path)?;
        let target_abs = self.safe_resolve(&params.to_file)?;

        let source_content = std::fs::read_to_string(&source_abs)
            .map_err(|e| format!("Cannot read {}: {e}", source_abs.display()))?;
        let (extracted_code, lines) = extract_lines(&source_content, &sym, &source_file.path)?;
        let start_idx = (sym.line_start as usize).saturating_sub(1);
        let end_idx = (sym.line_end as usize).min(lines.len());

        let importer_files = gather_importers(&conn, source_file.id)?;

        let apply = params.apply.unwrap_or(false);
        if !apply {
            return Ok(format_move_preview(
                &sym,
                &source_file.path,
                &params.to_file,
                &extracted_code,
                start_idx,
                end_idx,
                &importer_files,
            ));
        }

        apply_move_writes(
            &source_abs,
            &target_abs,
            &extracted_code,
            &lines,
            &source_content,
            start_idx,
            end_idx,
        )?;

        let (import_updates, failed_writes) =
            self.rewrite_importers(&source_file.path, &params.to_file, &importer_files)?;

        let status = if failed_writes.is_empty() {
            "All imports updated.".to_string()
        } else {
            format!(
                "WARNING: {} import(s) failed to write:\n  {}",
                failed_writes.len(),
                failed_writes.join("\n  "),
            )
        };
        let mut out = format!(
            "Moved '{}' ({}) from {} → {}. {status}\n\n",
            sym.name, sym.kind, source_file.path, params.to_file,
        );
        out.push_str(&format!(
            "{} lines extracted, {} importer(s) rewritten.\n",
            end_idx - start_idx,
            import_updates
        ));

        Ok(out)
    }

    /// Apply rename-pair rewrites to every file that imports the source.
    /// Returns `(succeeded_count, failed_writes)`. Per-file read/write
    /// failures are collected and reported at the end so the caller can
    /// surface them to the user without aborting other importers.
    fn rewrite_importers(
        &self,
        source_path: &str,
        target_path: &str,
        importer_files: &[(String, Option<String>)],
    ) -> Result<(u32, Vec<String>), String> {
        let mut import_updates: u32 = 0;
        let mut failed_writes: Vec<String> = Vec::new();
        let old_import_path = path_to_import_stem(source_path);
        let new_import_path = path_to_import_stem(target_path);
        // Match both the full index-style stem (`src::foo::bar`) and the
        // divergent suffix (`foo::bar`) so `use crate::foo::bar;` importers
        // get rewritten alongside `use src::foo::bar;`. Without pair-based
        // matching only the literal on-disk path would resolve, and the
        // common `crate::…` / `super::…` imports were silently left broken.
        let stem_pairs = rename_stem_pairs(&old_import_path, &new_import_path);
        if stem_pairs.is_empty() {
            return Ok((0, failed_writes));
        }
        for (importer_path, _) in importer_files {
            let importer_abs = match self.safe_resolve(importer_path) {
                Ok(p) => p,
                Err(e) => {
                    failed_writes.push(format!("{importer_path}: resolve/read failed: {e}"));
                    continue;
                }
            };
            let content = match std::fs::read_to_string(&importer_abs) {
                Ok(c) => c,
                Err(e) => {
                    failed_writes.push(format!("{importer_path}: resolve/read failed: {e}"));
                    continue;
                }
            };
            let updated = apply_rename_pairs(&content, &stem_pairs)?;
            if updated != content {
                if let Err(e) = refactor_common::write_atomic(&importer_abs, &updated) {
                    failed_writes.push(format!("{}: {e}", importer_abs.display()));
                } else {
                    import_updates += 1;
                }
            }
        }
        Ok((import_updates, failed_writes))
    }
}

/// Resolve `name` (with optional `kind` filter) to a single source symbol.
/// Errors when nothing matches, when the kind hint excludes everything, or
/// when multiple definitions remain ambiguous - the caller must disambiguate
/// before any destructive write happens. Also short-circuits if the
/// destination already has a same-name same-kind symbol so the move never
/// silently overwrites unrelated code.
fn validate_source(
    conn: &rusqlite::Connection,
    name: &str,
    kind_hint: Option<&str>,
    to_file: &str,
) -> Result<
    (
        crate::storage::models::SymbolRow,
        crate::storage::models::FileRow,
    ),
    String,
> {
    let mut results =
        read::find_symbol_by_name(conn, name).map_err(|e| format!("DB error: {e}"))?;

    if results.is_empty() {
        return Err(format!("No symbol found with name '{name}'"));
    }

    // Narrow by kind when the caller supplies one. The SQL layer only
    // matches on name, so free `fn foo()` and `impl Foo { fn foo() }`
    // arrive together — a `kind` hint lets the caller pick exactly one
    // without touching the DB query path.
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
        return Err(format!(
            "Multiple definitions of '{name}' found. Pass `kind` to disambiguate or specify a unique name:\n{}",
            locations.join("\n"),
        ));
    }

    let (sym, source_file) = results.remove(0);

    if source_file.path != to_file
        && let Ok(Some(target_file)) = read::get_file_by_path(conn, to_file)
        && let Ok(target_syms) = read::get_symbols_for_file(conn, target_file.id)
        && let Some(conflict) = target_syms
            .iter()
            .find(|s| s.name == sym.name && s.kind == sym.kind)
    {
        return Err(format!(
            "Cannot move '{}' ({}): destination '{}' already defines a {} '{}' at L{}-L{}. Refusing to overwrite.",
            sym.name,
            sym.kind,
            to_file,
            conflict.kind,
            conflict.name,
            conflict.line_start,
            conflict.line_end,
        ));
    }

    Ok((sym, source_file))
}

/// Slice the symbol's source-line range out of `source_content`. Returns
/// the joined extracted code along with all source lines (so the caller
/// can reuse them without re-splitting). Errors when the symbol's
/// recorded range is out of bounds, which usually means the index is
/// stale relative to the on-disk file.
fn extract_lines<'a>(
    source_content: &'a str,
    sym: &crate::storage::models::SymbolRow,
    source_path: &str,
) -> Result<(String, Vec<&'a str>), String> {
    let lines: Vec<&str> = source_content.lines().collect();
    let start_idx = (sym.line_start as usize).saturating_sub(1);
    let end_idx = (sym.line_end as usize).min(lines.len());

    if start_idx >= lines.len() {
        return Err(format!(
            "Symbol line range L{}-L{} out of bounds for {} ({} lines)",
            sym.line_start,
            sym.line_end,
            source_path,
            lines.len(),
        ));
    }

    let extracted_code: String = lines[start_idx..end_idx].join("\n");
    Ok((extracted_code, lines))
}

/// Resolve every `from_file → source_file` edge to its (path, specifier)
/// pair. Includes every edge-graph importer unconditionally - filtering
/// by specifier text used to silently miss glob imports (`use foo::*;`)
/// and parent-module imports (`use foo;` then `foo::sym(...)`). The
/// downstream regex rewrite is a no-op for unrelated importers, whereas
/// excluding them corrupts the build.
fn gather_importers(
    conn: &rusqlite::Connection,
    source_file_id: i64,
) -> Result<Vec<(String, Option<String>)>, String> {
    let importers =
        read::get_edges_to(conn, source_file_id).map_err(|e| format!("DB error: {e}"))?;
    let mut importer_files: Vec<(String, Option<String>)> = Vec::new();
    for edge in &importers {
        if let Ok(Some(f)) = read::get_file_by_id(conn, edge.from_file) {
            importer_files.push((f.path.clone(), edge.specifier.clone()));
        }
    }
    Ok(importer_files)
}

/// Format the dry-run report shown when `apply=false`. Includes a 500-byte
/// (char-boundary-safe) preview of the extracted code plus the list of
/// importers that the apply pass would rewrite.
fn format_move_preview(
    sym: &crate::storage::models::SymbolRow,
    source_path: &str,
    to_file: &str,
    extracted_code: &str,
    start_idx: usize,
    end_idx: usize,
    importer_files: &[(String, Option<String>)],
) -> String {
    let mut out = format!(
        "Preview: move '{}' ({}) from {} to {}\n\n",
        sym.name, sym.kind, source_path, to_file,
    );

    out.push_str(&format!(
        "Code to extract (L{}-L{}, {} lines):\n",
        sym.line_start,
        sym.line_end,
        end_idx - start_idx
    ));
    out.push_str("```\n");
    let preview = if extracted_code.len() > 500 {
        let end = crate::str_utils::floor_char_boundary(extracted_code, 500);
        format!("{}...\n(truncated)", &extracted_code[..end])
    } else {
        extracted_code.to_string()
    };
    out.push_str(&preview);
    out.push_str("\n```\n\n");

    if importer_files.is_empty() {
        out.push_str("No files import this symbol.\n");
    } else {
        out.push_str(&format!(
            "Files that import this symbol ({}):\n",
            importer_files.len()
        ));
        for (path, spec) in importer_files {
            let spec_str = spec.as_deref().unwrap_or("(unspecified)");
            out.push_str(&format!("  {path} — via '{spec_str}'\n"));
        }
        out.push_str(
            "\nImport paths in these files will be updated to point to the new location.\n",
        );
    }

    out
}

/// Target-first move that routes both writes through the shared
/// `refactor_common::write_atomic` helper (tmp file + rename). The target
/// is written first so a mid-operation failure leaves the source intact.
fn apply_move_writes(
    source_abs: &std::path::Path,
    target_abs: &std::path::Path,
    extracted_code: &str,
    lines: &[&str],
    source_content: &str,
    start_idx: usize,
    end_idx: usize,
) -> Result<(), String> {
    // Compute the post-removal source content, collapsing blank lines
    // only at the seam where the symbol used to live. A blanket
    // `while contains("\n\n\n")` sweep would flatten every three-line
    // gap in the file and quietly destroy intentional paragraph
    // separators far from the extraction site.
    let mut remaining_lines: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| *i < start_idx || *i >= end_idx)
        .map(|(_, l)| *l)
        .collect();
    if start_idx > 0
        && start_idx < remaining_lines.len()
        && remaining_lines[start_idx - 1].trim().is_empty()
        && remaining_lines[start_idx].trim().is_empty()
    {
        remaining_lines.remove(start_idx);
    }
    // `str::lines` strips the trailing newline, so `join("\n")` loses the
    // final `\n`. Preserve the POSIX trailing newline when the source had
    // one to avoid phantom diff lines in git.
    let preserve_trailing_newline = source_content.ends_with('\n');
    let mut new_source = remaining_lines.join("\n");
    if preserve_trailing_newline && !new_source.is_empty() && !new_source.ends_with('\n') {
        new_source.push('\n');
    }
    let source_should_be_blank = new_source.trim().is_empty() && remaining_lines.len() <= 1;

    // Write the target file FIRST. A failure mid-way (disk full, permission
    // denied, read-only filesystem) leaves the source file intact so the
    // caller can retry without losing the extracted symbol.
    let target_content = if target_abs.exists() {
        let existing = std::fs::read_to_string(target_abs)
            .map_err(|e| format!("Cannot read {}: {e}", target_abs.display()))?;
        if existing.ends_with('\n') {
            format!("{}\n{}\n", existing.trim_end(), extracted_code)
        } else {
            format!("{existing}\n\n{extracted_code}\n")
        }
    } else {
        if let Some(parent) = target_abs.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot create dirs for {}: {e}", target_abs.display()))?;
        }
        format!("{extracted_code}\n")
    };
    refactor_common::write_atomic(target_abs, &target_content)?;

    // With the target safely on disk, remove the symbol from the source.
    let source_final = if source_should_be_blank {
        String::new()
    } else {
        new_source
    };
    refactor_common::write_atomic(source_abs, &source_final)?;

    Ok(())
}
