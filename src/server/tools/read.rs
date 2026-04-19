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

#[tool_router(router = qartez_read_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_read",
        description = "Read one or more symbols' source code from disk with line numbers. Faster than Read — jumps directly to the symbol without scanning. Pass `symbol_name` for a single symbol, or `symbols=[...]` to batch-fetch multiple in one call. Use file_path to disambiguate. Passing just `file_path` (no symbol) reads the whole file or a slice via start_line/end_line/limit — replaces the built-in Read for module headers, imports, and small files.",
        annotations(
            title = "Read Symbol",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_read(
        &self,
        Parameters(params): Parameters<SoulReadParams>,
    ) -> Result<String, String> {
        // 25_000 bytes ≈ 6 KiB of tokens — a comfortable ceiling for two
        // or three mid-sized functions while still leaving headroom in a
        // 200k context window. Callers can raise it if they know they
        // want more.
        let max_bytes = params.max_bytes.unwrap_or(25_000) as usize;
        let context_lines = params.context_lines.unwrap_or(0) as usize;

        // Raw file-range mode: file_path given without any symbol. Dumps the
        // whole file by default, or a specific slice when start_line/end_line/
        // limit are set. Saves callers from falling back to the built-in Read
        // tool for imports, module headers, small files, or whole-file scans.
        let no_symbols_requested = params.symbol_name.as_deref().is_none_or(|s| s.is_empty())
            && params.symbols.as_ref().is_none_or(|v| v.is_empty());
        if no_symbols_requested && let Some(ref fp) = params.file_path {
            let abs_path = self.safe_resolve(fp)?;
            let source = std::fs::read_to_string(&abs_path)
                .map_err(|e| format!("Cannot read {}: {e}", abs_path.display()))?;
            let lines: Vec<&str> = source.lines().collect();
            let total_lines = lines.len();
            if total_lines == 0 {
                return Ok(format!("{fp} (empty file)\n"));
            }

            // Resolve the requested range. `limit` mirrors the built-in Read
            // tool: read `limit` lines starting at `start_line` (defaults to
            // 1). When none of start_line/end_line/limit are set, the whole
            // file is returned — max_bytes still bounds the output so huge
            // files don't blow the response budget.
            let mut start = params.start_line.unwrap_or(0);
            let mut end = params.end_line.unwrap_or(0);
            if let Some(lim) = params.limit
                && lim > 0
            {
                if start == 0 {
                    start = 1;
                }
                if end == 0 {
                    end = start.saturating_add(lim - 1);
                }
            }
            if start == 0 {
                start = 1;
            }
            if end == 0 {
                end = total_lines as u32;
            }
            let start_idx = (start as usize).saturating_sub(1);
            if start_idx >= total_lines {
                return Err(format!(
                    "start_line ({start}) exceeds file length ({total_lines})"
                ));
            }
            if start > end {
                return Err(format!("start_line ({start}) > end_line ({end})"));
            }
            let end_idx = (end as usize).min(total_lines);

            let mut out = format!("{fp} L{start}-{end_idx}\n");
            let mut truncated_at: Option<usize> = None;
            for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
                let formatted = format!("{:>4} | {}\n", start_idx + i + 1, line);
                if out.len() + formatted.len() > max_bytes {
                    truncated_at = Some(start_idx + i);
                    break;
                }
                out.push_str(&formatted);
            }
            if let Some(cut) = truncated_at {
                out.push_str(&format!(
                    "// ... (truncated at line {}, response reached {max_bytes}-byte cap; raise `max_bytes` or page with `start_line`/`limit`)\n",
                    cut + 1,
                ));
            }
            return Ok(out);
        }

        // Build the caller-requested query list. Batch mode takes priority when
        // both fields are set, so a caller migrating from single → batch does
        // not have to clear `symbol_name` explicitly. Unknown-but-present empty
        // strings in the list are dropped as no-ops rather than erroring, so
        // callers can freely splat variable-length arrays.
        let queries: Vec<String> = match (params.symbols, params.symbol_name) {
            (Some(list), _) if !list.is_empty() => {
                list.into_iter().filter(|s| !s.is_empty()).collect()
            }
            (_, Some(name)) if !name.is_empty() => vec![name],
            _ => {
                return Err(
                    "Either `symbol_name` or a non-empty `symbols` list is required".to_string(),
                );
            }
        };
        if queries.is_empty() {
            return Err("No non-empty symbol names provided".to_string());
        }

        // Normalize the file_path filter so Windows callers can pass either
        // separator style and still substring-match forward-slash DB keys.
        let file_filter: Option<String> = params
            .file_path
            .as_deref()
            .map(|s| crate::index::to_forward_slash(s.to_string()));

        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;

        // Two-pass: first resolve each query to its matching (symbol, file)
        // tuples, then batch the blast-radius lookup for only the files that
        // actually matched. Prevents an O(V*(V+E)) full sweep for every
        // invocation when batch mode often involves 1–5 files.
        let mut per_query: Vec<(
            usize,
            Vec<(
                crate::storage::models::SymbolRow,
                crate::storage::models::FileRow,
            )>,
        )> = Vec::with_capacity(queries.len());
        let mut missing: Vec<String> = Vec::new();
        for (idx, query) in queries.iter().enumerate() {
            let results = match read::find_symbol_by_name(&conn, query) {
                Ok(r) => r,
                Err(e) => return Err(format!("DB error: {e}")),
            };
            let filtered: Vec<_> = if let Some(ref fp) = file_filter {
                results
                    .into_iter()
                    .filter(|(_, file)| file.path.contains(fp.as_str()))
                    .collect()
            } else {
                results
            };
            if filtered.is_empty() {
                missing.push(query.clone());
            } else {
                per_query.push((idx, filtered));
            }
        }

        let mut match_file_ids: Vec<i64> = per_query
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|(_, f)| f.id))
            .collect();
        match_file_ids.sort_unstable();
        match_file_ids.dedup();
        let blast_radii = blast::blast_radius_for_files(&conn, &match_file_ids).unwrap_or_default();
        drop(conn);

        let total_symbols: usize = per_query.iter().map(|(_, f)| f.len()).sum();
        let mut out = String::new();
        let mut rendered_any = false;
        let mut rendered_count: usize = 0;
        let mut truncated = false;

        for (_idx, filtered) in &per_query {
            for (sym, file) in filtered {
                let abs_path = self.safe_resolve(&file.path)?;
                let source = match std::fs::read_to_string(&abs_path) {
                    Ok(s) => s,
                    Err(e) => return Err(format!("Cannot read {}: {e}", abs_path.display())),
                };

                let lines: Vec<&str> = source.lines().collect();
                // Expand the window by `context_lines` on the start side;
                // the end side is the symbol's real terminator (symbols
                // are closed units, rarely useful to trail beyond them).
                let sym_start = (sym.line_start as usize).saturating_sub(1);
                let start = sym_start.saturating_sub(context_lines);
                let end = (sym.line_end as usize).min(lines.len());

                let visibility = if sym.is_exported { "+" } else { "-" };
                let blast_r = blast_radii.get(&file.id).copied().unwrap_or(0);

                // Compact single-line header: marker name kind @ path:Lstart-end →blast
                // Replaces the old two-line `// name — kind (visibility) →X\n// path [Lx-Ly]`
                // format. Saves ~12 tokens per symbol; still carries every
                // field a caller needs.
                let mut section = format!(
                    "// {visibility} {} {} @ {}:L{}-{} →{}\n",
                    sym.name, sym.kind, file.path, sym.line_start, sym.line_end, blast_r,
                );
                for (i, line) in lines[start..end].iter().enumerate() {
                    section.push_str(&format!("{:>4} | {}\n", start + i + 1, line));
                }
                section.push('\n');

                // Stop before writing if this section would push us past the
                // cap. We still include at least one full section even if it
                // exceeds the budget alone — truncating a symbol mid-line is
                // worse than returning a single over-budget response.
                if !out.is_empty() && out.len() + section.len() > max_bytes {
                    truncated = true;
                    break;
                }
                out.push_str(&section);
                rendered_any = true;
                rendered_count += 1;
            }

            if truncated {
                break;
            }
        }

        if !rendered_any {
            let joined = queries.join(", ");
            if let Some(ref fp) = file_filter {
                return Err(format!(
                    "No symbols [{joined}] found in file matching '{fp}'"
                ));
            }
            return Err(format!("No symbol found with name(s) [{joined}]"));
        }

        if !missing.is_empty() {
            out.push_str(&format!(
                "// ({} not found: {})\n",
                missing.len(),
                missing.join(", ")
            ));
        }

        if truncated {
            let remaining = total_symbols.saturating_sub(rendered_count);
            out.push_str(&format!(
                "// ... (truncated: {remaining} symbol(s) skipped, response reached {max_bytes}-byte cap)\n",
            ));
        }

        Ok(out)
    }
}
