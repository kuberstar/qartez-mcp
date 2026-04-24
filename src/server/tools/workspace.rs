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
            WorkspaceAction::Remove => {
                // `path` is only meaningful for add: remove matches by
                // alias. Silently accepting the argument hid typos
                // where the caller thought they were targeting a
                // specific path rather than the alias. Emit a warning
                // before proceeding so the discrepancy is visible.
                let path_warning = params
                    .path
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|_| {
                        "// warning: 'path' is ignored for remove; the workspace entry is matched by alias only\n".to_string()
                    });
                let result = self.remove_workspace(&params.alias)?;
                Ok(match path_warning {
                    Some(warn) => format!("{warn}{result}"),
                    None => result,
                })
            }
        }
    }

    fn add_workspace(&self, alias: &str, path_str: Option<&str>) -> Result<String, String> {
        let Some(path_str) = path_str else {
            return Err(format!(
                "Workspace '{alias}' could not be added: path is required for 'add' action."
            ));
        };

        validate_alias(alias)?;

        let path = expand_path(path_str, &self.project_root);
        // Canonical wording template: every add-miss goes through
        // `add_miss_error` so the three distinct failure modes
        // (missing, non-directory, in-use) share a single prefix. The
        // audit found three different phrasings across this function
        // and the remove path; a unified message keeps automated
        // tooling from having to regex-match each variant.
        let canonical_path = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                return Err(format!(
                    "Workspace '{alias}' could not be added: path '{path_str}' does not exist."
                ));
            }
        };

        if !canonical_path.is_dir() {
            return Err(format!(
                "Workspace '{alias}' could not be added: path '{}' is not a directory.",
                canonical_path.display()
            ));
        }

        // Reject re-adding a path that is already registered under a different
        // alias. Two aliases pointing at the same root desynchronize the TOML
        // from in-memory state after restart (TOML iteration order wins).
        // Also reject reusing an alias that already maps to a different
        // path - silently overwriting the TOML entry hid stale-root
        // bugs behind a success message.
        {
            let aliases = self.root_aliases.read().map_err(|e| e.to_string())?;
            if let Some(existing) = aliases.get(&canonical_path) {
                if existing != alias {
                    return Err(format!(
                        "Workspace '{alias}' could not be added: path '{}' is already registered as '{existing}'.",
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
            if let Some((existing_path, _)) = aliases.iter().find(|(_, a)| *a == alias) {
                return Err(format!(
                    "Workspace '{alias}' is already registered (path: '{}'). Pass a different alias or call remove first.",
                    existing_path.display()
                ));
            }
        }

        // Prefix-collision guard. `delete_files_by_prefix('{alias}')` from a
        // later `remove` purges every file whose path starts with
        // '{alias}/' regardless of which root produced it. When the primary
        // root has a subdirectory whose name matches the new alias, the
        // primary indexer has already stored those files under that prefix,
        // so registering the alias would put primary-owned files on the
        // kill list of a subsequent `remove`. This was the path that let
        // `qartez_workspace remove qartez-public` wipe the entire index.
        {
            let conn = self.db.lock().map_err(|e| e.to_string())?;
            let prefix = format!("{alias}/");
            let all_files = read::get_all_files(&conn).map_err(|e| e.to_string())?;
            let colliding: Vec<String> = all_files
                .into_iter()
                .filter(|f| f.path.starts_with(&prefix))
                .map(|f| f.path)
                .collect();
            if !colliding.is_empty() {
                let shown = colliding.iter().take(3).cloned().collect::<Vec<_>>();
                let ellipsis = if colliding.len() > 3 {
                    format!(", +{} more", colliding.len() - 3)
                } else {
                    String::new()
                };
                return Err(format!(
                    "Refusing to add alias '{alias}': the index already contains {} file(s) under the '{alias}/' prefix (e.g. {}{ellipsis}). Registering this alias would let a later `remove` purge files owned by other roots. Pick a different alias (e.g. '{alias}-ext') or unindex the colliding paths first.",
                    colliding.len(),
                    shown.join(", "),
                ));
            }
        }

        // Canonicalize the stored path. Storing the caller's raw `path_str`
        // (e.g. `./foo`) broke subsequent tool calls from a different cwd:
        // expand_path resolved them against a different base, so the DB
        // alias mapped to an unrelated directory after `cd`. The
        // canonical absolute path is unambiguous across cwds.
        let stored_path = canonical_path.to_string_lossy().into_owned();

        // Only materialise `workspace.toml` after the full add pipeline
        // succeeds. A previous failure path (e.g. broken git repo during
        // cochange analysis) left an empty `[workspaces]` section on
        // disk even though the in-memory state was rolled back.
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

        {
            let mut roots = self.project_roots.write().map_err(|e| e.to_string())?;
            let mut aliases = self.root_aliases.write().map_err(|e| e.to_string())?;
            if !roots.contains(&canonical_path) {
                roots.push(canonical_path.clone());
            }
            aliases.insert(canonical_path.clone(), alias.to_string());
        }

        let config_path = self.project_root.join(".qartez").join("workspace.toml");
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        let mut doc: toml_edit::DocumentMut = content
            .parse()
            .map_err(|e| format!("failed to parse {}: {e}", config_path.display()))?;
        let workspaces = doc.entry("workspaces").or_insert(toml_edit::table());
        if let Some(table) = workspaces.as_table_mut() {
            table.insert(alias, toml_edit::value(&stored_path));
        }
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&config_path, doc.to_string()).map_err(|e| e.to_string())?;

        Ok(format!("Added domain '{alias}' at '{stored_path}'."))
    }

    fn remove_workspace(&self, alias: &str) -> Result<String, String> {
        // Belt-and-suspenders against the primary-root wipe scenario. The
        // add-time prefix-collision guard already rejects colliding aliases,
        // but existing workspace.toml files written before that guard may
        // still carry an alias whose path is inside the primary root. A
        // remove of such an alias would call `delete_files_by_prefix` and
        // purge the primary root's files too.
        let primary_canonical = self.project_root.canonicalize().ok();

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

            let Some(ref path) = path_to_remove else {
                return Err(format!("Workspace '{alias}' is not registered."));
            };

            if let Some(ref primary) = primary_canonical
                && path.starts_with(primary)
            {
                return Err(format!(
                    "Refusing to remove alias '{alias}': its path '{}' is inside the primary project root '{}'. Removing would purge primary-root files from the index. If this alias was registered before the prefix-collision guard, unregister it manually by editing .qartez/workspace.toml and re-run indexing.",
                    path.display(),
                    primary.display(),
                ));
            }

            aliases.remove(path);
            roots.retain(|r| r != path);
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
