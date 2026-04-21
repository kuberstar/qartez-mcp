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
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let budget = params.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET as u32) as usize;
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

        if concise {
            let mut emitted = 0usize;
            let mut skipped = 0usize;
            let mut next_offset: Option<usize> = None;
            for sym in &symbols {
                if sym.kind == "field" {
                    continue;
                }
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                let marker = if sym.is_exported { "+" } else { "-" };
                let line = format!("  {marker} {} [L{}]\n", sym.name, sym.line_start);
                if budget_exceeded(&mut out, &line, budget) {
                    next_offset = Some(offset + emitted);
                    break;
                }
                out.push_str(&line);
                emitted += 1;
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

        let mut by_kind: std::collections::BTreeMap<
            String,
            Vec<&crate::storage::models::SymbolRow>,
        > = std::collections::BTreeMap::new();
        for sym in &symbols {
            if sym.kind == "field" {
                continue;
            }
            let display_kind = capitalize_kind(&sym.kind);
            by_kind.entry(display_kind).or_default().push(sym);
        }

        let mut skipped = 0usize;
        let mut emitted = 0usize;
        let mut next_offset: Option<usize> = None;
        'outer: for (kind, syms) in &by_kind {
            let mut header_written = false;
            for sym in syms {
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
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
                if budget_exceeded(&mut out, &line, budget) {
                    next_offset = Some(offset + emitted);
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
                        if budget_exceeded(&mut out, &fline, budget) {
                            next_offset = Some(offset + emitted);
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
