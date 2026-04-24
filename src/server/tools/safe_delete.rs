// Rust guideline compliant 2026-04-23

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

use crate::storage::read;

#[tool_router(router = qartez_safe_delete_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_safe_delete",
        description = "Delete a symbol after reporting every file that still references it. Preview by default, always listing importers that would break. Apply refuses to run when importers exist unless `force=true`; the caller is then responsible for fixing the dangling uses. Use `kind` / `file_path` to disambiguate when the name is shared.",
        annotations(
            title = "Safe Delete Symbol",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_safe_delete(
        &self,
        Parameters(params): Parameters<SoulSafeDeleteParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let (sym, source_file) = resolve_unique_symbol(
            &conn,
            &params.symbol,
            params.kind.as_deref(),
            params.file_path.as_deref(),
        )?;

        // Use the per-symbol reference table only. File-level `use`
        // edges (`get_edges_to`) count every module that imports
        // *anything* from the defining file, so a zero-caller private
        // helper living in `mod.rs` still got flagged "7 files
        // reference mod.rs" - the guard fired on a perfectly safe
        // delete. `get_symbol_references_filtered` is scoped to the
        // exact symbol being deleted and is the signal `qartez_refs`
        // already surfaces.
        let sym_refs = read::get_symbol_references_filtered(
            &conn,
            &sym.name,
            Some(&sym.kind),
            Some(&source_file.path),
        )
        .map_err(|e| format!("DB error: {e}"))?;
        // Split importers by whether they live in the source file
        // (same-file refs, i.e. sibling symbols in the same module)
        // or elsewhere. Before, both categories collapsed into one
        // flat path list and the refusal message could not distinguish
        // "callers will fail to compile" from "sibling symbol will be
        // orphaned inside the defining file", leaving the user to
        // read the paths and pick out the source file by eye. Pure
        // self-references (recursion) are dropped - deletion removes
        // both sides of that edge at once.
        let mut ref_importers: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        let mut external_importers: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        let mut same_file_from_ids: std::collections::BTreeSet<i64> =
            std::collections::BTreeSet::new();
        for (_, _, importers) in &sym_refs {
            for (_, importer_file, from_symbol_id) in importers {
                if *from_symbol_id == sym.id {
                    continue;
                }
                ref_importers.insert(importer_file.path.clone());
                if importer_file.path == source_file.path {
                    same_file_from_ids.insert(*from_symbol_id);
                } else {
                    external_importers.insert(importer_file.path.clone());
                }
            }
        }
        // Resolve same-file refs to line numbers by scanning the
        // source file's symbol table once. This is cheap - a single
        // indexed SELECT - and avoids a per-reference DB round trip.
        let mut same_file_lines: Vec<u32> = Vec::new();
        if !same_file_from_ids.is_empty()
            && let Ok(file_syms) = read::get_symbols_for_file(&conn, source_file.id)
        {
            for s in &file_syms {
                if same_file_from_ids.contains(&s.id) {
                    same_file_lines.push(s.line_start);
                }
            }
        }
        same_file_lines.sort_unstable();
        same_file_lines.dedup();

        // Belt-and-suspenders: augment the `symbol_refs` importer set with
        // a tree-sitter call-site scan, mirroring the hybrid resolver in
        // `qartez_calls`. The `symbol_refs` table relies on the indexer
        // binding each call target by name, which fails for method calls
        // through a typed receiver (e.g. `pool.parse_file(...)` where the
        // resolver cannot pick `ParserPool::parse_file` over any other
        // `parse_file` definition). Without this scan a private helper
        // with live callers would be reported as "safe to delete".
        if matches!(sym.kind.as_str(), "function" | "method" | "constructor") {
            let all_files = read::get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;
            for file in &all_files {
                if ref_importers.contains(&file.path) {
                    continue;
                }
                let Some(source) = self.cached_source(&file.path) else {
                    continue;
                };
                if !source.contains(sym.name.as_str()) {
                    continue;
                }
                let calls = self.cached_calls(&file.path);
                let has_call_here = calls.iter().any(|(n, line)| {
                    n == sym.name.as_str()
                        && !(file.path == source_file.path
                            && (*line as u32) >= sym.line_start
                            && (*line as u32) <= sym.line_end)
                });
                if has_call_here {
                    ref_importers.insert(file.path.clone());
                }
            }
        }

        // Trait-impl guard: when deleting a trait, enumerate every `impl
        // TraitName for ...` site via the `type_hierarchy` table so the
        // caller sees every concrete implementation that would stop
        // compiling. `symbol_refs` only records name-resolved usages and
        // does not capture trait-impl relationships.
        let mut trait_impl_sites: Vec<(String, String, u32)> = Vec::new();
        if sym.kind.eq_ignore_ascii_case("trait") || sym.kind.eq_ignore_ascii_case("interface") {
            let subtypes =
                read::get_subtypes(&conn, &sym.name).map_err(|e| format!("DB error: {e}"))?;
            for (rel, file) in subtypes {
                if file.path == source_file.path
                    && rel.line >= sym.line_start
                    && rel.line <= sym.line_end
                {
                    continue;
                }
                ref_importers.insert(file.path.clone());
                trait_impl_sites.push((rel.sub_name, file.path, rel.line));
            }
        }

        let importer_paths: Vec<String> = ref_importers.iter().cloned().collect();
        drop(conn);

        let apply = params.apply.unwrap_or(false);
        let force = params.force.unwrap_or(false);

        let abs_path = self.safe_resolve(&source_file.path)?;
        let content = std::fs::read_to_string(&abs_path)
            .map_err(|e| format!("Cannot read {}: {e}", abs_path.display()))?;
        let (lines, start_idx, end_idx) = validate_range(&content, &sym, &source_file.path)?;

        if !apply {
            let mut out = format!(
                "Preview: delete '{}' ({}) from {} L{}-L{} ({} lines).\n\n",
                sym.name,
                sym.kind,
                source_file.path,
                sym.line_start,
                sym.line_end,
                end_idx - start_idx,
            );
            if importer_paths.is_empty() {
                // `qartez_safe_delete` targets a symbol inside the
                // source file, not the file itself; wording the
                // success line with "this symbol" avoids misleading
                // callers into believing the file would be removed.
                out.push_str("No files import this symbol. Safe to delete.\n");
            } else {
                out.push_str(&format!(
                    "WARNING: {} file(s) reference symbol '{}' ({}) and may break after delete:\n",
                    importer_paths.len(),
                    sym.name,
                    sym.kind,
                ));
                for p in &importer_paths {
                    out.push_str(&format!("  {p}\n"));
                }
                if !trait_impl_sites.is_empty() {
                    out.push_str(&format!(
                        "\n{} trait impl block(s) target '{}' and would fail to compile:\n",
                        trait_impl_sites.len(),
                        sym.name,
                    ));
                    for (sub_name, path, line) in &trait_impl_sites {
                        out.push_str(&format!(
                            "  impl {} for {sub_name} @ {path}:L{line}\n",
                            sym.name
                        ));
                    }
                }
                // Context-aware trigger: when the caller already set
                // `force=true` the hint to "pass `force=true` with
                // `apply=true`" reads like the tool lost their flag,
                // so collapse the guidance to "call again with
                // `apply=true`" whenever `force` is already on. The
                // default preview path keeps the original wording
                // since both flags are still absent.
                if force {
                    out.push_str(&format!(
                        "\nTo delete a symbol with {} live reference(s), call again with `apply=true` (force=true already set). The caller must then fix the dangling imports.\n",
                        importer_paths.len(),
                    ));
                } else {
                    out.push_str(
                        "\nPass `force=true` with `apply=true` to delete anyway. The caller must then fix the dangling imports.\n",
                    );
                }
            }
            return Ok(out);
        }

        if !importer_paths.is_empty() && !force {
            // Refusal message separates external importers from
            // same-file sibling references. External importers would
            // stop compiling if the delete proceeded; same-file refs
            // are only orphaned inside the defining module. Splitting
            // the two lists lets the caller see exactly which use
            // sites they still need to fix before re-running with
            // `force=true`.
            let mut out = format!(
                "Refusing to delete '{}' ({}): {} importer(s) would break.\n",
                sym.name,
                sym.kind,
                importer_paths.len(),
            );
            if !external_importers.is_empty() {
                out.push_str(&format!(
                    "\nExternal importers ({}):\n",
                    external_importers.len(),
                ));
                for p in &external_importers {
                    out.push_str(&format!("  - {p}\n"));
                }
            }
            if !same_file_lines.is_empty() {
                let lines_str: Vec<String> =
                    same_file_lines.iter().map(|l| format!("L{l}")).collect();
                out.push_str(&format!(
                    "\nSame-file references ({}):\n  - {} (lines: {})\n",
                    same_file_lines.len(),
                    source_file.path,
                    lines_str.join(", "),
                ));
            }
            if !trait_impl_sites.is_empty() {
                out.push_str(&format!(
                    "\n{} trait impl block(s) target '{}':\n",
                    trait_impl_sites.len(),
                    sym.name,
                ));
                for (sub_name, path, line) in &trait_impl_sites {
                    out.push_str(&format!(
                        "  impl {} for {sub_name} @ {path}:L{line}\n",
                        sym.name
                    ));
                }
            }
            out.push_str(
                "\nPass force=true to delete anyway. External importers will be left with dangling references.\n",
            );
            return Err(out);
        }

        // Drop the symbol's lines plus the immediately following blank
        // line when one exists at the seam. A global `\n\n\n` sweep would
        // flatten intentional paragraph separators elsewhere in the file
        // - mirrors the extraction logic in `qartez_move::write_atomic`.
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

        let preserve_trailing_newline = content.ends_with('\n');
        let new_content = join_lines_with_trailing(&remaining_lines, preserve_trailing_newline);
        write_atomic(&abs_path, &new_content)?;

        let mut out = format!(
            "Deleted '{}' ({}) from {} L{}-L{} ({} lines).\n",
            sym.name,
            sym.kind,
            source_file.path,
            sym.line_start,
            sym.line_end,
            end_idx - start_idx,
        );
        if !importer_paths.is_empty() {
            out.push_str(&format!(
                "\nWARNING: {} file(s) still reference '{}' ({}) - dangling imports:\n",
                importer_paths.len(),
                sym.name,
                sym.kind,
            ));
            for p in &importer_paths {
                out.push_str(&format!("  {p}\n"));
            }
        }
        Ok(out)
    }
}
