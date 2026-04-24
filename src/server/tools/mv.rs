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
        description = "Move a symbol to another file and update all import paths automatically. Handles extraction, insertion, and importer rewrites in one step. Importer count is sourced from the full symbol reference graph (same data as `qartez_refs`), not just the file-level `use` edges. Use `kind` / `file_path` to disambiguate when the name is shared. Preview by default; set apply=true to execute.",
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
        // Module-root and crate-root basenames get the same refusal
        // that `qartez_rename_file` applies. Moving a symbol into
        // `mod.rs` / `lib.rs` / `main.rs` silently mutates the
        // module/crate entry point in ways the caller almost never
        // intends; surface the explicit error before any DB work.
        validate_move_target_basename(&params.to_file)?;

        // Builtin-method names (new, default, from, clone, ...) resolve
        // by name across every type - extracting a `new` into a different
        // file breaks every unrelated `Type::new()` call site because
        // method dispatch is name-based, not type-based. Unlike the
        // rename guard there is no `allow_collision` escape here: moving
        // such a symbol is categorically unsafe. Callers must rename it
        // first or delete it via `qartez_safe_delete`.
        if let Some(name) = is_builtin_method_name(&params.symbol) {
            return Err(format!(
                "Refusing to move '{name}' ({kind}): '{name}' is a builtin-method name. Moving it risks breaking unrelated callers that reference a same-named method on a different type. Pass a new name via qartez_rename first, or use qartez_safe_delete if you want to remove it.",
                kind = params.kind.as_deref().unwrap_or("symbol"),
            ));
        }

        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let (sym, source_file) = validate_source(
            &conn,
            &params.symbol,
            params.kind.as_deref(),
            params.file_path.as_deref(),
            &params.to_file,
        )?;

        // Cross-language moves are never correct: a Rust symbol pasted
        // into a `.ts` file would compile-fail on both sides of the
        // move. Reject when the target extension differs from the
        // source, and reject unknown extensions outright so the caller
        // cannot silently retarget assets, docs, or config files.
        validate_move_target_extension(&source_file.path, &params.to_file)?;

        let normalized_to = crate::index::to_forward_slash(params.to_file.clone());
        if normalized_to == source_file.path {
            return Err(format!(
                "Refusing to move '{}' ({}): `to_file` equals the source file '{}'. A self-move would double-insert or lose the symbol body on apply.",
                sym.name, sym.kind, source_file.path,
            ));
        }

        let source_abs = self.safe_resolve(&source_file.path)?;
        let target_abs = self.safe_resolve(&params.to_file)?;

        if !target_abs.exists()
            && let Some(parent) = target_abs.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            return Err(format!(
                "Refusing to move '{}' ({}): parent directory for `to_file` '{}' does not exist. Create it first or pick an existing directory.",
                sym.name, sym.kind, params.to_file,
            ));
        }

        let source_content = std::fs::read_to_string(&source_abs)
            .map_err(|e| format!("Cannot read {}: {e}", source_abs.display()))?;
        let (extracted_code, lines) = extract_lines(&source_content, &sym, &source_file.path)?;
        let start_idx = (sym.line_start as usize).saturating_sub(1);
        let end_idx = (sym.line_end as usize).min(lines.len());

        // Count identifier occurrences of `sym.name` that live in the
        // source file but OUTSIDE the extract range. A non-zero count
        // means the move would leave same-file callers (commonly
        // `#[cfg(test)] mod tests { ... }`) pointing at a symbol that
        // no longer lives in this file. We surface this both in the
        // preview (as a WARNING) and as a hard refusal on apply,
        // because no `force` param is wired through the tool schema.
        let dangling_self_refs =
            count_same_file_refs_outside_range(&lines, start_idx, end_idx, &sym.name);

        let importer_files = gather_importers(&conn, source_file.id, &sym)?;

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
                dangling_self_refs,
            ));
        }

        if dangling_self_refs > 0 {
            return Err(format!(
                "Refusing to move '{}' ({}): {} same-file reference(s) to '{}' outside the extract range will be left dangling after move. Use qartez_rename or qartez_safe_delete first, or re-point them before moving. Re-run with apply=false to inspect the preview.",
                sym.name, sym.kind, dangling_self_refs, sym.name,
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
    file_path_hint: Option<&str>,
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
    // arrive together - a `kind` hint lets the caller pick exactly one
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

    if let Some(fp) = file_path_hint.filter(|s| !s.is_empty()) {
        let fp_norm = crate::index::to_forward_slash(fp.to_string());
        let available: Vec<String> = results
            .iter()
            .map(|(_, f)| f.path.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        results.retain(|(_, f)| f.path == fp_norm);
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
        return Err(format!(
            "Multiple definitions of '{name}' found. Pass `kind` and/or `file_path` to disambiguate:\n{}",
            locations.join("\n"),
        ));
    }

    let (sym, source_file) = results.remove(0);

    // Destination-collision guard: when the target file already defines
    // anything (any kind) with the same name, refuse. Same-name items
    // in one file are the usual Rust name-resolution ambiguity (shadowed
    // impl methods, conflicting `fn` vs. `const`, etc.) that the move
    // would silently create. Caller must rename one side first.
    if source_file.path != to_file
        && let Ok(Some(target_file)) = read::get_file_by_path(conn, to_file)
        && let Ok(target_syms) = read::get_symbols_for_file(conn, target_file.id)
        && target_syms.iter().any(|s| s.name == sym.name)
    {
        return Err(format!(
            "Refusing to move '{}' ({}): target file '{}' already defines a symbol with that name. Rename either side first.",
            sym.name, sym.kind, to_file,
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

/// Resolve every file that references this symbol. Unions two signals:
/// file-level `edges` (which cover `use` imports resolved by the index
/// resolver) and symbol-level `symbol_refs` rows (which cover cross-file
/// usages that never showed up as an import edge, e.g. glob imports,
/// re-exports, or parent-module qualified calls). Using `get_edges_to`
/// alone under-counted importers - `qartez_refs` for the same symbol
/// returned 8 hits while `qartez_move` reported 1. Deduplicated by path
/// because an importer that both `use`s the source file AND references
/// the symbol directly must only be rewritten once.
fn gather_importers(
    conn: &rusqlite::Connection,
    source_file_id: i64,
    sym: &crate::storage::models::SymbolRow,
) -> Result<Vec<(String, Option<String>)>, String> {
    let mut seen: std::collections::BTreeMap<String, Option<String>> =
        std::collections::BTreeMap::new();

    let edge_importers =
        read::get_edges_to(conn, source_file_id).map_err(|e| format!("DB error: {e}"))?;
    for edge in &edge_importers {
        if let Ok(Some(f)) = read::get_file_by_id(conn, edge.from_file) {
            seen.entry(f.path.clone()).or_insert(edge.specifier.clone());
        }
    }

    // Hydrate symbol-level references. `get_symbol_references_filtered`
    // restricts the lookup to the exact symbol we are moving (same
    // kind + defining-file) so cross-file homonyms do not pollute the
    // count.
    if let Ok(Some(def_file)) = read::get_file_by_id(conn, source_file_id) {
        let sym_refs = read::get_symbol_references_filtered(
            conn,
            &sym.name,
            Some(&sym.kind),
            Some(&def_file.path),
        )
        .map_err(|e| format!("DB error: {e}"))?;
        for (_, _, importers) in sym_refs {
            for (_, importer_file, _from_symbol_id) in importers {
                if importer_file.id == source_file_id {
                    continue;
                }
                seen.entry(importer_file.path.clone()).or_insert(None);
            }
        }
    }

    // Trait-impl awareness: moving a trait relocates the canonical
    // `use` path every `impl OldTrait for X` file depends on.
    // `symbol_refs` records name-resolved usages only and misses
    // implementor files whose `use` line comes from a prelude re-export
    // or a sibling-module shortcut. `type_hierarchy` is the
    // authoritative map of those sites; pull every subtype's file into
    // the importer set so `rewrite_importers` can rewrite the `use`
    // path along with the ordinary name-import sites.
    if sym.kind.eq_ignore_ascii_case("trait") || sym.kind.eq_ignore_ascii_case("interface") {
        let subtypes = read::get_subtypes(conn, &sym.name).map_err(|e| format!("DB error: {e}"))?;
        for (_rel, file) in subtypes {
            if file.id == source_file_id {
                continue;
            }
            seen.entry(file.path).or_insert(None);
        }
    }

    Ok(seen.into_iter().collect())
}

/// Format the dry-run report shown when `apply=false`. Includes a 500-byte
/// (char-boundary-safe) preview of the extracted code plus the list of
/// importers that the apply pass would rewrite.
#[allow(clippy::too_many_arguments)]
fn format_move_preview(
    sym: &crate::storage::models::SymbolRow,
    source_path: &str,
    to_file: &str,
    extracted_code: &str,
    start_idx: usize,
    end_idx: usize,
    importer_files: &[(String, Option<String>)],
    dangling_self_refs: usize,
) -> String {
    let mut out = format!(
        "Preview: move '{}' ({}) from {} to {}\n\n",
        sym.name, sym.kind, source_path, to_file,
    );

    if dangling_self_refs > 0 {
        out.push_str(&format!(
            "WARNING: {dangling} same-file reference(s) to '{name}' outside the extract range will be left dangling after move. Use qartez_rename or qartez_safe_delete first, or re-point them before moving.\n\n",
            dangling = dangling_self_refs,
            name = sym.name,
        ));
    }

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
        // Compute the future import stem pairs once. The apply path uses
        // the exact same `rename_stem_pairs` output to rewrite importers,
        // so the preview now shows what the caller will actually see
        // post-apply instead of "(unspecified)" for every symbol-refs
        // importer (which never has a `use` specifier to display).
        let old_stem = path_to_import_stem(source_path);
        let new_stem = path_to_import_stem(to_file);
        let stem_pairs = rename_stem_pairs(&old_stem, &new_stem);
        let primary_pair = stem_pairs
            .first()
            .cloned()
            .unwrap_or_else(|| (old_stem.clone(), new_stem.clone()));

        out.push_str(&format!(
            "Files that import this symbol ({}):\n",
            importer_files.len()
        ));
        for (path, spec) in importer_files {
            match spec.as_deref() {
                Some(s) if !s.is_empty() => {
                    out.push_str(&format!("  {path} - via '{s}'\n"));
                }
                _ => {
                    // No `use` specifier in the index (the importer
                    // reached the symbol through a non-import edge such
                    // as a re-export chain or a qualified parent-module
                    // call). Show the deterministic rewrite pair so the
                    // caller knows which `::` path will flip on apply.
                    out.push_str(&format!(
                        "  {path} - symbol-ref (will rewrite '{}' -> '{}')\n",
                        primary_pair.0, primary_pair.1,
                    ));
                }
            }
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

/// Reject move destinations whose basename is a Rust module root
/// (`mod.rs`) or crate entry (`lib.rs` / `main.rs`). These names are
/// load-bearing for the build system; silently dropping a symbol into
/// one of them mutates module resolution or Cargo bin/lib registration.
/// `qartez_rename_file` applies the symmetric guard on its `to` arg.
fn validate_move_target_basename(to_file: &str) -> Result<(), String> {
    let norm = crate::index::to_forward_slash(to_file.to_string());
    let basename = std::path::Path::new(&norm)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    match basename {
        "mod.rs" => Err(format!(
            "Refusing to move into '{to_file}': 'mod.rs' is a Rust module root. Pick a sibling file or create a new one instead.",
        )),
        "lib.rs" | "main.rs" => Err(format!(
            "Refusing to move into '{to_file}': '{basename}' is a Rust crate entry point registered in Cargo.toml. Pick a module file instead.",
        )),
        _ => Ok(()),
    }
}

/// Reject cross-language moves and unknown-extension targets. A move
/// mixes source and target file contents, so the extensions must agree
/// on the language being written. The recognised set mirrors the
/// languages the indexer parses (`rs`, `ts`, `tsx`, `js`, `jsx`, `py`,
/// `go`, `java`, `rb`). Anything else is either a binary/asset or a
/// config file that should never receive a symbol body.
fn validate_move_target_extension(source_path: &str, to_file: &str) -> Result<(), String> {
    const KNOWN: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "pyi", "go", "java", "kt", "kts", "rb",
        "swift", "scala",
    ];
    let to_norm = crate::index::to_forward_slash(to_file.to_string());
    let to_ext = std::path::Path::new(&to_norm)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    let src_ext = std::path::Path::new(source_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    let Some(to_ext) = to_ext else {
        return Err(format!(
            "Refusing to move into '{to_file}': target has no file extension. Supply a source file path (e.g. `.rs`, `.ts`, `.py`).",
        ));
    };
    if !KNOWN.contains(&to_ext.as_str()) {
        return Err(format!(
            "Refusing to move into '{to_file}': unsupported target extension '.{to_ext}'. Known extensions: {}.",
            KNOWN.join(", "),
        ));
    }
    if let Some(src) = src_ext
        && src != to_ext
    {
        return Err(format!(
            "Refusing to move into '{to_file}': source extension '.{src}' does not match target extension '.{to_ext}'. Cross-language moves are not supported.",
        ));
    }
    Ok(())
}

/// Return `Some(name)` when `candidate` matches a Rust builtin trait /
/// inherent method name. Moving such a symbol by name is unsafe because
/// method dispatch does not carry receiver-type information into the
/// index: every `.new()` / `.clone()` / `.len()` call site resolves by
/// name, so relocating the definition silently breaks unrelated callers.
/// This mirrors `rename.rs::is_builtin_method_name` but is duplicated
/// here so both guards can evolve independently.
fn is_builtin_method_name(candidate: &str) -> Option<&'static str> {
    const BUILTIN_METHOD_NAMES: &[&str] = &[
        "new",
        "default",
        "from",
        "into",
        "as_ref",
        "as_mut",
        "clone",
        "drop",
        "deref",
        "fmt",
        "len",
        "is_empty",
        "iter",
        "next",
        "hash",
        "eq",
        "cmp",
        "partial_cmp",
        "push",
        "pop",
        "insert",
        "remove",
        "get",
        "contains",
    ];
    BUILTIN_METHOD_NAMES
        .iter()
        .find(|n| **n == candidate)
        .copied()
}

/// Count identifier occurrences of `needle` in `lines` that fall OUTSIDE
/// the line range `[start_idx..end_idx]`. Uses a word-boundary character
/// classifier so a reference to `foo_bar` is not counted as a hit for
/// `foo`. The scan is intentionally permissive: it does not try to
/// distinguish comments from code, because a stale doc link to a moved
/// symbol is still a real caller-visible regression.
fn count_same_file_refs_outside_range(
    lines: &[&str],
    start_idx: usize,
    end_idx: usize,
    needle: &str,
) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if i >= start_idx && i < end_idx {
            continue;
        }
        count += count_identifier_hits(line, needle);
    }
    count
}

/// Count whole-identifier matches of `needle` in `haystack`. A match
/// requires the surrounding chars (or the string ends) to be non-word
/// so `foo` does not match inside `foo_bar` or `barfoo`.
fn count_identifier_hits(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    let bytes = haystack.as_bytes();
    let n = needle.as_bytes();
    let mut i = 0usize;
    let mut hits = 0usize;
    while i + n.len() <= bytes.len() {
        if &bytes[i..i + n.len()] == n {
            let before_ok = i == 0 || !is_word_byte(bytes[i - 1]);
            let after_ok = i + n.len() == bytes.len() || !is_word_byte(bytes[i + n.len()]);
            if before_ok && after_ok {
                hits += 1;
                i += n.len();
                continue;
            }
        }
        i += 1;
    }
    hits
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod move_validation_tests {
    use super::{
        count_same_file_refs_outside_range, is_builtin_method_name, validate_move_target_basename,
        validate_move_target_extension,
    };

    #[test]
    fn rejects_mod_and_crate_roots() {
        assert!(validate_move_target_basename("src/mod.rs").is_err());
        assert!(validate_move_target_basename("src/lib.rs").is_err());
        assert!(validate_move_target_basename("src/main.rs").is_err());
        assert!(validate_move_target_basename("src/a.rs").is_ok());
    }

    #[test]
    fn rejects_cross_language_and_unknown_extensions() {
        assert!(validate_move_target_extension("src/a.rs", "src/b.ts").is_err());
        assert!(validate_move_target_extension("src/a.rs", "src/b.md").is_err());
        assert!(validate_move_target_extension("src/a.rs", "src/b").is_err());
        assert!(validate_move_target_extension("src/a.rs", "src/b.rs").is_ok());
        assert!(validate_move_target_extension("src/a.ts", "src/b.ts").is_ok());
    }

    #[test]
    fn builtin_method_names_detected() {
        assert_eq!(is_builtin_method_name("new"), Some("new"));
        assert_eq!(is_builtin_method_name("clone"), Some("clone"));
        assert_eq!(is_builtin_method_name("len"), Some("len"));
        assert_eq!(is_builtin_method_name("my_fn"), None);
    }

    #[test]
    fn same_file_refs_count_only_outside_range() {
        let lines: Vec<&str> = vec![
            "fn foo() {}",        // 0: definition
            "fn bar() { foo() }", // 1: outside caller
            "// foo mentioned",   // 2: comment mention (still a hit)
            "fn foo_bar() {}",    // 3: different identifier, no hit
        ];
        // Range = [0..1) means only line 0 is the extract; lines 1-3
        // are outside and we expect 2 real hits on "foo".
        assert_eq!(count_same_file_refs_outside_range(&lines, 0, 1, "foo"), 2);
        // When the whole file is the range, there should be zero hits
        // outside.
        assert_eq!(count_same_file_refs_outside_range(&lines, 0, 4, "foo"), 0);
    }
}
