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
        description = "Replace a symbol's whole source range (lines L[line_start..line_end]) with `new_code`. Caller supplies the new definition including its signature - this is a precise line-range rewrite, not a body-only splice. Preview by default; set apply=true to execute. Use `kind` / `file_path` to disambiguate when the name is shared.",
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

        // Refuse empty `new_code` so a stray empty string doesn't turn into
        // "replace the symbol with one blank line" via the `"".split('\n')`
        // -> `[""]` quirk. Callers wanting to remove a symbol should use
        // `qartez_safe_delete`, which also runs the importer check.
        if params.new_code.trim_end_matches('\n').is_empty() {
            return Err(format!(
                "Empty `new_code` for qartez_replace_symbol. Pass the full replacement (including the signature), or use qartez_safe_delete to remove '{}'.",
                params.symbol,
            ));
        }

        let abs_path = self.safe_resolve(&source_file.path)?;
        let content = std::fs::read_to_string(&abs_path)
            .map_err(|e| format!("Cannot read {}: {e}", abs_path.display()))?;
        let (lines, start_idx, end_idx) = validate_range(&content, &sym, &source_file.path)?;

        let apply = params.apply.unwrap_or(false);
        let replaced_lines = end_idx - start_idx;
        let new_lines_count = params.new_code.lines().count();

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
            out.push_str("Old:\n```\n");
            out.push_str(&lines[start_idx..end_idx].join("\n"));
            out.push_str("\n```\n\nNew:\n```\n");
            out.push_str(&params.new_code);
            if !params.new_code.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n");
            return Ok(out);
        }

        // Build the rewritten file content. Strip the trailing newline of
        // `new_code` if present so we don't introduce a phantom blank line
        // at the seam; the global trailing-newline convention is restored
        // below via `join_lines_with_trailing`.
        let new_code = params.new_code.trim_end_matches('\n');
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
        Ok(format!(
            "Replaced '{}' ({}) in {} L{}-L{} ({} → {} lines).\n",
            sym.name,
            sym.kind,
            source_file.path,
            sym.line_start,
            sym.line_end,
            replaced_lines,
            new_lines_count,
        ))
    }
}
