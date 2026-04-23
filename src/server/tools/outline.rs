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

use crate::graph::blast;
use crate::guard;
use crate::storage::read;
use crate::storage::read::sanitize_fts_query;
use crate::toolchain;

#[tool_router(router = qartez_outline_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_outline",
        description = "List every symbol in a file grouped by kind (functions, classes, structs, etc.) with line numbers and signatures. Like a table of contents for the file.",
        annotations(
            title = "File Outline",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_outline(
        &self,
        Parameters(params): Parameters<SoulOutlineParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_outline")?;
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let budget = params.token_budget.unwrap_or(4000) as usize;
        let concise = is_concise(&params.format);
        let offset = params.offset.unwrap_or(0) as usize;
        let file = read::get_file_by_path(&conn, &params.file_path)
            .map_err(|e| format!("DB error: {e}"))?
            .ok_or_else(|| format!("File '{}' not found in index", params.file_path))?;

        let symbols =
            read::get_symbols_for_file(&conn, file.id).map_err(|e| format!("DB error: {e}"))?;

        if symbols.is_empty() {
            return Ok(format!(
                "No symbols found in '{}'. File may not be indexed yet.",
                params.file_path,
            ));
        }

        // Total non-field count drives the "next_offset" hint and the header.
        // We only page over non-field symbols because fields are rendered
        // inline underneath their parent struct, not as top-level entries.
        let total_non_fields = symbols.iter().filter(|s| s.kind != "field").count();
        let mut out = format!(
            "# Outline: {} ({} symbols)\n\n",
            params.file_path,
            symbols.len(),
        );

        // Stable source-order view of non-field symbols. Pagination must be
        // deterministic: "offset=N skips exactly N" has to hold regardless
        // of render mode, so we sort by line_start and keep a canonical
        // index for every non-field symbol.
        let mut non_fields: Vec<&crate::storage::models::SymbolRow> =
            symbols.iter().filter(|s| s.kind != "field").collect();
        non_fields.sort_by_key(|s| (s.line_start, s.id));

        if concise {
            let mut next_offset: Option<usize> = None;
            if offset >= non_fields.len() {
                return Ok(out);
            }
            for (i, sym) in non_fields.iter().skip(offset).enumerate() {
                let marker = if sym.is_exported { "+" } else { "-" };
                let line = format!("  {marker} {} [L{}]\n", sym.name, sym.line_start);
                if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                    next_offset = Some(offset + i);
                    out.push_str("  ... (truncated)\n");
                    break;
                }
                out.push_str(&line);
            }
            if let Some(next) = next_offset {
                out.push_str(&format!("next_offset: {next} (of {total_non_fields})\n",));
            }
            return Ok(out);
        }

        // Group fields under their parent struct: pre-index by parent id so
        // we can render struct → [fields] inline without blowing up the
        // top-level kind buckets.
        let mut fields_by_parent: HashMap<i64, Vec<&crate::storage::models::SymbolRow>> =
            HashMap::new();
        for sym in &symbols {
            if sym.kind == "field"
                && let Some(pid) = sym.parent_id
            {
                fields_by_parent.entry(pid).or_default().push(sym);
            }
        }

        // Honor the pagination contract up front: skip exactly `offset`
        // non-field symbols (source order), then render the remainder
        // grouped by kind in the established presentation order. This
        // keeps "offset=N skips N" semantics consistent with the concise
        // branch and with the schema description.
        if offset >= non_fields.len() {
            return Ok(out);
        }
        let visible: Vec<&crate::storage::models::SymbolRow> =
            non_fields.iter().copied().skip(offset).collect();
        let mut by_kind: std::collections::BTreeMap<
            String,
            Vec<&crate::storage::models::SymbolRow>,
        > = std::collections::BTreeMap::new();
        for sym in &visible {
            let display_kind = capitalize_kind(&sym.kind);
            by_kind.entry(display_kind).or_default().push(*sym);
        }

        let mut emitted = 0usize;
        let mut next_offset: Option<usize> = None;
        'outer: for (kind, syms) in &by_kind {
            let mut header_written = false;
            for sym in syms {
                if !header_written {
                    out.push_str(&format!("{kind}:\n"));
                    header_written = true;
                }
                let marker = if sym.is_exported { "+" } else { "-" };
                let fallback = format!("{} {}", sym.kind, sym.name);
                let sig = sym.signature.as_deref().unwrap_or(&fallback);
                let cc_tag = sym
                    .complexity
                    .map(|c| format!(" CC={c}"))
                    .unwrap_or_default();
                let line = format!(
                    "  {} {} [L{}-L{}]{} — {}\n",
                    marker, sym.name, sym.line_start, sym.line_end, cc_tag, sig,
                );
                if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                    next_offset = Some(offset + emitted);
                    out.push_str("  ... (truncated by token budget)\n");
                    break 'outer;
                }
                out.push_str(&line);
                emitted += 1;

                if let Some(fields) = fields_by_parent.get(&sym.id) {
                    for f in fields {
                        let fmarker = if f.is_exported { "+" } else { "-" };
                        let fline = format!(
                            "      {} {} — {}\n",
                            fmarker,
                            f.name,
                            f.signature.as_deref().unwrap_or(f.name.as_str()),
                        );
                        if estimate_tokens(&out) + estimate_tokens(&fline) > budget {
                            next_offset = Some(offset + emitted);
                            out.push_str("  ... (truncated by token budget)\n");
                            break 'outer;
                        }
                        out.push_str(&fline);
                    }
                }
            }
            if header_written {
                out.push('\n');
            }
        }

        if let Some(next) = next_offset {
            out.push_str(&format!("next_offset: {next} (of {total_non_fields})\n",));
        }

        Ok(out)
    }
}
