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

        // Gather every file that imports the defining file. This is the
        // same signal `qartez_move` uses; it covers glob-imports and
        // parent-module re-exports that a naive specifier filter drops.
        let edges =
            read::get_edges_to(&conn, source_file.id).map_err(|e| format!("DB error: {e}"))?;
        let mut importer_paths: Vec<String> = Vec::new();
        for edge in &edges {
            if let Ok(Some(f)) = read::get_file_by_id(&conn, edge.from_file)
                && !importer_paths.contains(&f.path)
                && f.path != source_file.path
            {
                importer_paths.push(f.path);
            }
        }
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
                out.push_str("No files import this file. Safe to delete.\n");
            } else {
                out.push_str(&format!(
                    "WARNING: {} file(s) import '{}' and may break after delete:\n",
                    importer_paths.len(),
                    source_file.path,
                ));
                for p in &importer_paths {
                    out.push_str(&format!("  {p}\n"));
                }
                out.push_str(
                    "\nPass `force=true` with `apply=true` to delete anyway. The caller must then fix the dangling imports.\n",
                );
            }
            return Ok(out);
        }

        if !importer_paths.is_empty() && !force {
            let mut out = format!(
                "Refusing to delete '{}' ({}): {} importer(s) still reference {}:\n",
                sym.name,
                sym.kind,
                importer_paths.len(),
                source_file.path,
            );
            for p in &importer_paths {
                out.push_str(&format!("  {p}\n"));
            }
            out.push_str("Pass `force=true` to proceed anyway.\n");
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
                "\nWARNING: {} file(s) still reference {} - dangling imports:\n",
                importer_paths.len(),
                source_file.path,
            ));
            for p in &importer_paths {
                out.push_str(&format!("  {p}\n"));
            }
        }
        Ok(out)
    }
}
