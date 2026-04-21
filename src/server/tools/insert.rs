// Rust guideline compliant 2026-04-21

#![allow(unused_imports)]

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};

use super::super::QartezServer;
use super::super::params::*;
use super::refactor_common::{join_lines_with_trailing, resolve_unique_symbol, write_atomic};

/// Direction of the insert relative to the anchor symbol.
enum InsertPos {
    Before,
    After,
}

#[tool_router(router = qartez_insert_before_symbol_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_insert_before_symbol",
        description = "Insert `new_code` on the line immediately before the anchor symbol's first line. Use to add helpers, tests, or new items next to related code without needing the exact surrounding context. Preview by default; set apply=true to execute. Use `kind` / `file_path` to disambiguate when the name is shared.",
        annotations(
            title = "Insert Before Symbol",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_insert_before_symbol(
        &self,
        Parameters(params): Parameters<SoulInsertSymbolParams>,
    ) -> Result<String, String> {
        self.do_insert(params, InsertPos::Before)
    }
}

#[tool_router(router = qartez_insert_after_symbol_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_insert_after_symbol",
        description = "Insert `new_code` on the line immediately after the anchor symbol's last line. Use to add helpers, tests, or new items next to related code without needing the exact surrounding context. Preview by default; set apply=true to execute. Use `kind` / `file_path` to disambiguate when the name is shared.",
        annotations(
            title = "Insert After Symbol",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_insert_after_symbol(
        &self,
        Parameters(params): Parameters<SoulInsertSymbolParams>,
    ) -> Result<String, String> {
        self.do_insert(params, InsertPos::After)
    }
}

impl QartezServer {
    fn do_insert(&self, params: SoulInsertSymbolParams, pos: InsertPos) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let (sym, source_file) = resolve_unique_symbol(
            &conn,
            &params.symbol,
            params.kind.as_deref(),
            params.file_path.as_deref(),
        )?;
        drop(conn);

        let abs_path = self.safe_resolve(&source_file.path)?;
        let content = std::fs::read_to_string(&abs_path)
            .map_err(|e| format!("Cannot read {}: {e}", abs_path.display()))?;
        let lines: Vec<&str> = content.lines().collect();
        let apply = params.apply.unwrap_or(false);

        // Index column for the insertion point. `before` splices at
        // line_start (the symbol's first line, 1-based) - index line_start-1.
        // `after` splices at line_end + 1 - index line_end. Clamp both to
        // the file bounds so an apply on a stale range still succeeds.
        let insert_idx = match pos {
            InsertPos::Before => (sym.line_start as usize).saturating_sub(1).min(lines.len()),
            InsertPos::After => (sym.line_end as usize).min(lines.len()),
        };

        let pos_label = match pos {
            InsertPos::Before => "before",
            InsertPos::After => "after",
        };

        let new_code_trimmed = params.new_code.trim_end_matches('\n');
        let new_code_lines_count = if new_code_trimmed.is_empty() {
            0
        } else {
            new_code_trimmed.split('\n').count()
        };

        if !apply {
            let insert_line_1_based = insert_idx + 1;
            let mut out = format!(
                "Preview: insert {pos_label} '{}' ({}) in {} at L{} ({} line(s))\n\n",
                sym.name, sym.kind, source_file.path, insert_line_1_based, new_code_lines_count,
            );
            out.push_str("Code to insert:\n```\n");
            out.push_str(&params.new_code);
            if !params.new_code.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n");
            return Ok(out);
        }

        if new_code_trimmed.is_empty() {
            return Ok(format!(
                "No changes: `new_code` is empty for insert {pos_label} '{}' in {}.",
                sym.name, source_file.path,
            ));
        }

        let preserve_trailing_newline = content.ends_with('\n');
        let new_code_lines: Vec<&str> = new_code_trimmed.split('\n').collect();

        let mut rewritten: Vec<&str> = Vec::with_capacity(lines.len() + new_code_lines.len());
        rewritten.extend_from_slice(&lines[..insert_idx]);
        rewritten.extend(new_code_lines.iter().copied());
        rewritten.extend_from_slice(&lines[insert_idx..]);

        let new_content = join_lines_with_trailing(&rewritten, preserve_trailing_newline);
        if new_content == content {
            return Ok(format!(
                "No changes: insert at L{} produced identical file.",
                insert_idx + 1,
            ));
        }

        write_atomic(&abs_path, &new_content)?;
        Ok(format!(
            "Inserted {} line(s) {pos_label} '{}' ({}) in {} at L{}.\n",
            new_code_lines_count,
            sym.name,
            sym.kind,
            source_file.path,
            insert_idx + 1,
        ))
    }
}
