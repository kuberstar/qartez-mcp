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

#[tool_router(router = qartez_rename_file_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_rename_file",
        description = "Rename/move a file and rewrite all import paths pointing to it in one atomic operation. Preview by default; set apply=true to execute.",
        annotations(
            title = "Rename File",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_rename_file(
        &self,
        Parameters(params): Parameters<SoulRenameFileParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let file = read::get_file_by_path(&conn, &params.from)
            .map_err(|e| format!("DB error: {e}"))?
            .ok_or_else(|| format!("File '{}' not found in index", params.from))?;

        let from_abs = self.safe_resolve(&params.from)?;
        let to_abs = self.safe_resolve(&params.to)?;

        if !from_abs.exists() {
            return Err(format!(
                "Source file does not exist: {}",
                from_abs.display()
            ));
        }

        let importers = read::get_edges_to(&conn, file.id).map_err(|e| format!("DB error: {e}"))?;

        let mut importer_paths: Vec<String> = Vec::new();
        for edge in &importers {
            if let Ok(Some(f)) = read::get_file_by_id(&conn, edge.from_file)
                && !importer_paths.contains(&f.path)
            {
                importer_paths.push(f.path);
            }
        }

        let old_stem = path_to_import_stem(&params.from);
        let new_stem = path_to_import_stem(&params.to);

        let old_rel_stem = relative_import_stem(&params.from);
        let new_rel_stem = relative_import_stem(&params.to);

        let apply = params.apply.unwrap_or(false);

        if !apply {
            if importer_paths.is_empty() {
                return Ok(format!("{} → {}: 0 importers\n", params.from, params.to,));
            }
            // Single line: summary + comma-separated importer list. For a
            // typical refactor preview (≤ a dozen importers) this stays well
            // under 200 tokens and parses trivially.
            return Ok(format!(
                "{} → {}: {} importer(s): {}\n",
                params.from,
                params.to,
                importer_paths.len(),
                importer_paths.join(", "),
            ));
        }

        if let Some(parent) = to_abs.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot create dirs for {}: {e}", to_abs.display()))?;
        }
        std::fs::rename(&from_abs, &to_abs).map_err(|e| {
            format!(
                "Cannot rename {} -> {}: {e}",
                from_abs.display(),
                to_abs.display()
            )
        })?;

        let mut updated_count = 0;
        let mut failed_writes: Vec<String> = Vec::new();
        let stem_re = regex::Regex::new(&format!(r"\b{}\b", regex::escape(&old_stem)))
            .map_err(|e| format!("regex error: {e}"))?;
        let rel_stem_re = if old_rel_stem != old_stem {
            Some(
                regex::Regex::new(&format!(r"\b{}\b", regex::escape(&old_rel_stem)))
                    .map_err(|e| format!("regex error: {e}"))?,
            )
        } else {
            None
        };
        for importer_path in &importer_paths {
            let importer_abs = match self.safe_resolve(importer_path) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let content = match std::fs::read_to_string(&importer_abs) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let mut updated = stem_re.replace_all(&content, new_stem.as_str()).to_string();
            if let Some(ref re) = rel_stem_re {
                updated = re.replace_all(&updated, new_rel_stem.as_str()).to_string();
            }

            if updated != content {
                if let Err(e) = std::fs::write(&importer_abs, &updated) {
                    failed_writes.push(format!("{}: {e}", importer_abs.display()));
                } else {
                    updated_count += 1;
                }
            }
        }

        // Rewrite `mod <old>;` in the parent module file. The edges table
        // only tracks `use` imports, so the parent that declares the module
        // never shows up as an importer — without this step, renaming
        // `src/foo.rs` → `src/bar.rs` leaves `mod foo;` dangling in
        // `src/lib.rs` and the crate fails to build.
        let mut mod_rewrite_note = String::new();
        if old_rel_stem != new_rel_stem
            && let Some(parent_rel) = find_parent_mod_file(&self.project_root, &params.from)
            && let Ok(parent_abs) = self.safe_resolve(&parent_rel.to_string_lossy())
        {
            if let Ok(content) = std::fs::read_to_string(&parent_abs) {
                let rewritten = rewrite_mod_decl(&content, &old_rel_stem, &new_rel_stem);
                if rewritten != content {
                    if let Err(e) = std::fs::write(&parent_abs, &rewritten) {
                        failed_writes.push(format!("{}: {e}", parent_abs.display()));
                    } else {
                        mod_rewrite_note =
                            format!(", parent mod decl updated in {}", parent_rel.display(),);
                    }
                }
            }
        }

        let warn = if failed_writes.is_empty() {
            String::new()
        } else {
            format!(
                "\nWARNING: {} file(s) failed to write:\n  {}\n",
                failed_writes.len(),
                failed_writes.join("\n  "),
            )
        };
        Ok(format!(
            "renamed {} → {} ({}/{} importers updated{})\n{warn}",
            params.from,
            params.to,
            updated_count,
            importer_paths.len(),
            mod_rewrite_note,
        ))
    }
}
