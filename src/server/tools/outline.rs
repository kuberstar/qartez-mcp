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

        // Language-aware kind override. The tree-sitter-toml backend
        // surfaces every `[section]` header as `class`, which reads
        // as "this Cargo.toml has 7 classes" on the rendered outline
        // and misleads callers into thinking the indexer is broken.
        // Relabel those rows as `table` locally so the outline matches
        // TOML vocabulary without requiring a re-index.
        let is_toml_file = file.language == "toml";

        if symbols.is_empty() {
            // The file row exists (the `get_file_by_path` above
            // returned `Some`), so "may not be indexed yet" was
            // misleading - lib.rs-style modules that hold only
            // `mod foo;` / `use` declarations legitimately have no
            // top-level symbols. Describe the real state so the
            // caller does not chase a phantom index issue.
            //
            // For `.rs` files, fall back to a lightweight regex
            // scan of the raw source for `mod X;` declarations:
            // the indexer never surfaces module declarations as
            // symbols, so a `lib.rs` with only `pub mod X;` lines
            // renders as an empty outline even though `qartez_stats`
            // ranks it as the top-PageRank node. Emitting the
            // declarations under a `## Modules` section makes the
            // real purpose of the file visible.
            if params.file_path.ends_with(".rs") {
                let abs = self.project_root.join(&params.file_path);
                if let Ok(source) = std::fs::read_to_string(&abs) {
                    let mods = extract_rust_module_decls(&source);
                    if !mods.is_empty() {
                        let mut out = format!(
                            "# Outline: {}\n\n## Modules ({})\n",
                            params.file_path,
                            mods.len(),
                        );
                        for (prefix, name) in &mods {
                            out.push_str(&format!("  {prefix} {name}\n"));
                        }
                        return Ok(out);
                    }
                }
            }
            return Ok(format!(
                "No top-level symbols in '{}'. The file is indexed but exposes only module/use declarations (no functions, types, or constants).",
                params.file_path,
            ));
        }

        // Total non-field count drives the "next_offset" hint and the
        // header. We only page over non-field symbols because fields
        // are rendered inline underneath their parent struct, not as
        // top-level entries. Record the field count so the header
        // reports BOTH counters instead of conflating them.
        let total_non_fields = symbols.iter().filter(|s| s.kind != "field").count();
        let field_count = symbols.len() - total_non_fields;

        // Harmonised header: one counter for the TOTAL symbols (fields
        // included, for parity with raw index dumps) and a second
        // counter for the pageable non-field symbols (the number
        // `offset`/`next_offset` operate on). Previously the header
        // reported `(symbols.len())` while the pagination hint used
        // `(total_non_fields)`; the two unexplained numbers drifted
        // apart on any file with struct fields, leaving callers to
        // reverse-engineer which counter to trust.
        let pageable_suffix = if field_count > 0 {
            format!(" ({total_non_fields} pageable, {field_count} field(s) inlined)")
        } else {
            String::new()
        };
        let out_header = format!(
            "# Outline: {} ({} symbols{pageable_suffix})\n\n",
            params.file_path,
            symbols.len(),
        );
        let mut out = out_header.clone();

        // Stable source-order view of non-field symbols. Pagination must be
        // deterministic: "offset=N skips exactly N" has to hold regardless
        // of render mode, so we sort by line_start and keep a canonical
        // index for every non-field symbol.
        let mut non_fields: Vec<&crate::storage::models::SymbolRow> =
            symbols.iter().filter(|s| s.kind != "field").collect();
        non_fields.sort_by_key(|s| (s.line_start, s.id));

        // Signal out-of-range offset as a hard error so callers do
        // not interpret an empty body as "this file has no symbols at
        // all". Before, offset=99999 silently fell through to an
        // empty-body success path that rendered as "232 symbols" +
        // zero rows - a contradiction the header could not explain.
        if offset >= total_non_fields {
            return Err(format!(
                "offset={offset} exceeds the {total_non_fields} pageable symbol(s) in '{}' (total {} including inlined fields). Pass a smaller offset or omit it to start from the top.",
                params.file_path,
                symbols.len(),
            ));
        }

        if concise {
            let mut next_offset: Option<usize> = None;
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
        let visible: Vec<&crate::storage::models::SymbolRow> =
            non_fields.iter().copied().skip(offset).collect();
        let mut by_kind: std::collections::BTreeMap<
            String,
            Vec<&crate::storage::models::SymbolRow>,
        > = std::collections::BTreeMap::new();
        for sym in &visible {
            let raw_kind = display_kind_for(sym.kind.as_str(), is_toml_file);
            let display_kind = capitalize_kind(raw_kind);
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

/// Remap a raw indexer kind to the vocabulary appropriate for the
/// host language. The upstream TOML backend emits `class` for every
/// `[section]` / `[[array]]` header because tree-sitter-toml has no
/// dedicated "table" node kind; the outline tool reads that as "seven
/// classes in Cargo.toml", which is confusing. Relabel to `table`
/// here so the final outline matches TOML vocabulary without touching
/// the indexer or the stored symbol rows.
fn display_kind_for(raw_kind: &str, is_toml_file: bool) -> &str {
    if is_toml_file && raw_kind == "class" {
        return "table";
    }
    raw_kind
}

/// Scan Rust source for top-level `mod X;` declarations. Returns the
/// full leading display prefix (`pub mod`, `pub(crate) mod`,
/// `pub(super) mod`, or bare `mod`) paired with the module name, in
/// source order so the caller can render `{prefix} {name}` without
/// re-joining the keyword. The regex is intentionally narrow (anchored
/// to start-of-line after whitespace, requires a trailing `;`) so
/// inline `mod foo { ... }` blocks - which the indexer already
/// surfaces as symbols - stay out, and a stray `mod foo;` inside a
/// docstring is a tolerable false positive.
fn extract_rust_module_decls(source: &str) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    for raw in source.lines() {
        let line = raw.trim_start();
        let (prefix, rest): (&'static str, &str) = if let Some(r) = line.strip_prefix("pub mod ") {
            ("pub mod", r)
        } else if let Some(r) = line.strip_prefix("pub(crate) mod ") {
            ("pub(crate) mod", r)
        } else if let Some(r) = line.strip_prefix("pub(super) mod ") {
            ("pub(super) mod", r)
        } else if let Some(r) = line.strip_prefix("mod ") {
            ("mod", r)
        } else {
            continue;
        };
        let Some(semi_idx) = rest.find(';') else {
            continue;
        };
        let name = rest[..semi_idx].trim();
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        out.push((prefix, name.to_string()));
    }
    out
}
