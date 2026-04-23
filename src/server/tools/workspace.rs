use std::collections::HashSet;
use std::path::PathBuf;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};

use super::super::QartezServer;
use super::super::params::*;

use crate::config::expand_path;
use crate::git;
use crate::graph;
use crate::index;
use crate::storage::{read, write};

/// Reject aliases that would break the `LIKE '<alias>/%'` purge in
/// `delete_files_by_prefix` or collide with path separators. TOML keys are
/// otherwise permissive, so we validate before persisting.
fn validate_alias(alias: &str) -> Result<(), String> {
    if alias.is_empty() {
        return Err("Alias cannot be empty".to_string());
    }
    if !alias
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(format!(
            "Invalid alias '{alias}': use only ASCII letters, digits, '-', '_', '.'"
        ));
    }
    Ok(())
}

#[tool_router(router = qartez_workspace_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_workspace",
        description = "Manage project domains (workspaces) dynamically. Add or remove external directories with custom aliases.",
        annotations(
            title = "Workspace Management",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub(in crate::server) fn qartez_workspace(
        &self,
        Parameters(params): Parameters<SoulWorkspaceParams>,
    ) -> Result<String, String> {
        match params.action {
            WorkspaceAction::Add => self.add_workspace(&params.alias, params.path.as_deref()),
            WorkspaceAction::Remove => self.remove_workspace(&params.alias),
        }
    }

    fn add_workspace(&self, alias: &str, path_str: Option<&str>) -> Result<String, String> {
        let Some(path_str) = path_str else {
            return Err("Path is required for 'add' action".to_string());
        };

        validate_alias(alias)?;

        let path = expand_path(path_str, &self.project_root);
        let canonical_path = path
            .canonicalize()
            .map_err(|e| format!("Failed to resolve path '{path_str}': {e}"))?;

        if !canonical_path.is_dir() {
            return Err(format!(
                "Path '{}' is not a directory",
                canonical_path.display()
            ));
        }

        // Reject re-adding a path that is already registered under a different
        // alias. Two aliases pointing at the same root desynchronize the TOML
        // from in-memory state after restart (TOML iteration order wins).
        {
            let aliases = self.root_aliases.read().map_err(|e| e.to_string())?;
            if let Some(existing) = aliases.get(&canonical_path) {
                if existing != alias {
                    return Err(format!(
                        "Path '{}' is already registered as '{existing}'. Remove it first.",
                        canonical_path.display()
                    ));
                }
                // Same alias + same path = idempotent no-op. Surface a
                // distinct info message so automation can see "already
                // present" without the silent-success ambiguity the old
                // path produced.
                return Ok(format!(
                    "alias '{alias}' already registered at '{}' - no-op.",
                    canonical_path.display()
                ));
            }
        }

        let config_path = self.project_root.join(".qartez").join("workspace.toml");
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        let mut doc: toml_edit::DocumentMut = content
            .parse()
            .map_err(|e| format!("failed to parse {}: {e}", config_path.display()))?;
        let workspaces = doc.entry("workspaces").or_insert(toml_edit::table());
        if let Some(table) = workspaces.as_table_mut() {
            table.insert(alias, toml_edit::value(path_str));
        }
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&config_path, doc.to_string()).map_err(|e| e.to_string())?;

        {
            let mut roots = self.project_roots.write().map_err(|e| e.to_string())?;
            let mut aliases = self.root_aliases.write().map_err(|e| e.to_string())?;
            if !roots.contains(&canonical_path) {
                roots.push(canonical_path.clone());
            }
            aliases.insert(canonical_path.clone(), alias.to_string());
        }

        let extra_known: HashSet<String> = {
            let conn = self.db.lock().map_err(|e| e.to_string())?;
            read::get_all_files(&conn)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|f| f.path)
                .collect()
        };

        {
            let conn = self.db.lock().map_err(|e| e.to_string())?;
            index::full_index_root(&conn, &canonical_path, false, alias, &extra_known)
                .map_err(|e| e.to_string())?;
            // Keep PageRank and co-change current so qartez_map / qartez_impact
            // report correct rankings for the new root without a restart.
            graph::pagerank::compute_pagerank(&conn, &Default::default())
                .map_err(|e| e.to_string())?;
            graph::pagerank::compute_symbol_pagerank(&conn, &Default::default())
                .map_err(|e| e.to_string())?;
            git::cochange::analyze_cochanges(
                &conn,
                &self.project_root,
                &git::cochange::CoChangeConfig {
                    commit_limit: self.git_depth,
                    ..Default::default()
                },
            )
            .map_err(|e| e.to_string())?;
        }

        Ok(format!("Added domain '{alias}' at '{path_str}'."))
    }

    fn remove_workspace(&self, alias: &str) -> Result<String, String> {
        let mut path_to_remove: Option<PathBuf> = None;

        {
            let mut roots = self.project_roots.write().map_err(|e| e.to_string())?;
            let mut aliases = self.root_aliases.write().map_err(|e| e.to_string())?;

            for (path, a) in aliases.iter() {
                if a == alias {
                    path_to_remove = Some(path.clone());
                    break;
                }
            }

            if let Some(ref path) = path_to_remove {
                aliases.remove(path);
                roots.retain(|r| r != path);
            } else {
                return Err(format!("Domain '{alias}' not found in workspace"));
            }
        }

        let config_path = self.project_root.join(".qartez").join("workspace.toml");
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path).map_err(|e| e.to_string())?;
            let mut doc: toml_edit::DocumentMut = content
                .parse()
                .map_err(|e| format!("failed to parse {}: {e}", config_path.display()))?;
            if let Some(workspaces) = doc.get_mut("workspaces").and_then(|w| w.as_table_mut()) {
                workspaces.remove(alias);
                std::fs::write(&config_path, doc.to_string()).map_err(|e| e.to_string())?;
            }
        }

        {
            let conn = self.db.lock().map_err(|e| e.to_string())?;
            write::delete_files_by_prefix(&conn, alias).map_err(|e| e.to_string())?;
        }

        Ok(format!(
            "Removed domain '{alias}'. All associated symbols and files have been purged from the index."
        ))
    }
}
