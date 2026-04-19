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
        let mut results = read::find_symbol_by_name(&conn, &params.symbol)
            .map_err(|e| format!("DB error: {e}"))?;

        if results.is_empty() {
            return Err(format!("No symbol found with name '{}'", params.symbol));
        }

        // Narrow by kind when the caller supplies one. The SQL layer only
        // matches on name, so free `fn foo()` and `impl Foo { fn foo() }`
        // arrive together — a `kind` hint lets the caller pick exactly one
        // without touching the DB query path.
        if let Some(k) = params.kind.as_deref().filter(|s| !s.is_empty()) {
            let available: Vec<String> = results
                .iter()
                .map(|(s, _)| s.kind.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            results.retain(|(s, _)| s.kind.eq_ignore_ascii_case(k));
            if results.is_empty() {
                return Err(format!(
                    "No symbol '{}' with kind '{}'. Available kinds: {}",
                    params.symbol,
                    k,
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
                "Multiple definitions of '{}' found. Pass `kind` to disambiguate or specify a unique name:\n{}",
                params.symbol,
                locations.join("\n"),
            ));
        }

        let (sym, source_file) = &results[0];
        let source_abs = self.safe_resolve(&source_file.path)?;
        let target_abs = self.safe_resolve(&params.to_file)?;

        if source_file.path != params.to_file
            && let Ok(Some(target_file)) = read::get_file_by_path(&conn, &params.to_file)
            && let Ok(target_syms) = read::get_symbols_for_file(&conn, target_file.id)
            && let Some(conflict) = target_syms
                .iter()
                .find(|s| s.name == sym.name && s.kind == sym.kind)
        {
            return Err(format!(
                "Cannot move '{}' ({}): destination '{}' already defines a {} '{}' at L{}-L{}. Refusing to overwrite.",
                sym.name,
                sym.kind,
                params.to_file,
                conflict.kind,
                conflict.name,
                conflict.line_start,
                conflict.line_end,
            ));
        }

        let source_content = std::fs::read_to_string(&source_abs)
            .map_err(|e| format!("Cannot read {}: {e}", source_abs.display()))?;

        let lines: Vec<&str> = source_content.lines().collect();
        let start_idx = (sym.line_start as usize).saturating_sub(1);
        let end_idx = (sym.line_end as usize).min(lines.len());

        if start_idx >= lines.len() {
            return Err(format!(
                "Symbol line range L{}-L{} out of bounds for {} ({} lines)",
                sym.line_start,
                sym.line_end,
                source_file.path,
                lines.len(),
            ));
        }

        let extracted_code: String = lines[start_idx..end_idx].join("\n");

        let importers =
            read::get_edges_to(&conn, source_file.id).map_err(|e| format!("DB error: {e}"))?;

        let mut importer_files: Vec<(String, Option<String>)> = Vec::new();
        for edge in &importers {
            let spec_matches = edge
                .specifier
                .as_ref()
                .map(|s| s.contains(&params.symbol))
                .unwrap_or(true);
            if spec_matches && let Ok(Some(f)) = read::get_file_by_id(&conn, edge.from_file) {
                importer_files.push((f.path.clone(), edge.specifier.clone()));
            }
        }

        let target_stem = &params.to_file;

        let apply = params.apply.unwrap_or(false);

        if !apply {
            let mut out = format!(
                "Preview: move '{}' ({}) from {} to {}\n\n",
                sym.name, sym.kind, source_file.path, params.to_file,
            );

            out.push_str(&format!(
                "Code to extract (L{}-L{}, {} lines):\n",
                sym.line_start,
                sym.line_end,
                end_idx - start_idx
            ));
            out.push_str("```\n");
            let preview = if extracted_code.len() > 500 {
                let end = crate::str_utils::floor_char_boundary(&extracted_code, 500);
                format!("{}...\n(truncated)", &extracted_code[..end])
            } else {
                extracted_code.clone()
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
                for (path, spec) in &importer_files {
                    let spec_str = spec.as_deref().unwrap_or("(unspecified)");
                    out.push_str(&format!("  {path} — via '{spec_str}'\n"));
                }
                out.push_str(
                    "\nImport paths in these files will be updated to point to the new location.\n",
                );
            }

            return Ok(out);
        }

        let remaining_lines: Vec<&str> = lines
            .iter()
            .enumerate()
            .filter(|(i, _)| *i < start_idx || *i >= end_idx)
            .map(|(_, l)| *l)
            .collect();
        let new_source = remaining_lines.join("\n");
        if new_source.trim().is_empty() && remaining_lines.len() <= 1 {
            std::fs::write(&source_abs, "")
                .map_err(|e| format!("Cannot write {}: {e}", source_abs.display()))?;
        } else {
            let mut cleaned = new_source.clone();
            while cleaned.contains("\n\n\n") {
                cleaned = cleaned.replace("\n\n\n", "\n\n");
            }
            std::fs::write(&source_abs, &cleaned)
                .map_err(|e| format!("Cannot write {}: {e}", source_abs.display()))?;
        }

        if target_abs.exists() {
            let existing = std::fs::read_to_string(&target_abs)
                .map_err(|e| format!("Cannot read {}: {e}", target_abs.display()))?;
            let new_content = if existing.ends_with('\n') {
                format!("{}\n{}\n", existing.trim_end(), extracted_code)
            } else {
                format!("{existing}\n\n{extracted_code}\n")
            };
            std::fs::write(&target_abs, new_content)
                .map_err(|e| format!("Cannot write {}: {e}", target_abs.display()))?;
        } else {
            if let Some(parent) = target_abs.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create dirs for {}: {e}", target_abs.display()))?;
            }
            std::fs::write(&target_abs, format!("{extracted_code}\n"))
                .map_err(|e| format!("Cannot write {}: {e}", target_abs.display()))?;
        }

        let mut import_updates = 0;
        let mut failed_writes: Vec<String> = Vec::new();
        for (importer_path, _) in &importer_files {
            let importer_abs = match self.safe_resolve(importer_path) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let content = match std::fs::read_to_string(&importer_abs) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let old_import_path = path_to_import_stem(&source_file.path);
            let new_import_path = path_to_import_stem(target_stem);

            if old_import_path != new_import_path {
                let updated =
                    match regex::Regex::new(&format!(r"\b{}\b", regex::escape(&old_import_path))) {
                        Ok(re) => re
                            .replace_all(&content, new_import_path.as_str())
                            .to_string(),
                        Err(_) => content.clone(),
                    };
                if updated != content {
                    if let Err(e) = std::fs::write(&importer_abs, &updated) {
                        failed_writes.push(format!("{}: {e}", importer_abs.display()));
                    } else {
                        import_updates += 1;
                    }
                }
            }
        }

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
}
