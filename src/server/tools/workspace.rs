use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};

use super::super::QartezServer;
use super::super::RootSource;
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

/// Replace any character that would fail `validate_alias` with `_`.
/// Used when deriving an alias from a path basename so that a folder
/// like `My Project!` still produces a usable alias of `My_Project_`.
fn sanitize_alias_chars(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "root".to_string() } else { s }
}

/// Derive a usable alias from a path basename and disambiguate it
/// against the registered alias set with a numeric suffix.
///
/// Returns `Err` only when basename extraction itself fails, which
/// means the caller passed something like `/`. The disambiguation
/// loop is bounded at 1000 to prevent runaway behaviour if the
/// alias map is somehow saturated.
fn derive_alias_from_path(
    path: &Path,
    existing_aliases: &std::collections::HashMap<PathBuf, String>,
) -> Result<String, String> {
    let basename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| {
            format!(
                "Cannot derive an alias from path '{}': no basename component. Pass `alias` explicitly.",
                path.display()
            )
        })?;
    let base = sanitize_alias_chars(&basename);
    let in_use: std::collections::HashSet<&str> =
        existing_aliases.values().map(String::as_str).collect();
    if !in_use.contains(base.as_str()) {
        return Ok(base);
    }
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        if !in_use.contains(candidate.as_str()) {
            return Ok(candidate);
        }
    }
    Err(format!(
        "Could not derive a unique alias from '{}': basename collides with too many existing aliases. Pass `alias` explicitly.",
        path.display()
    ))
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
        // Workspace add is the legacy entry point: persist to TOML,
        // attach a watcher when the server runs in watch mode, and
        // tag the source as Runtime since this path is only reachable
        // from a live tool call.
        self.add_root_inner(
            alias,
            path_str,
            true,
            self.watch_enabled(),
            RootSource::Runtime,
        )
    }

    /// Shared implementation behind `qartez_workspace add` and
    /// `qartez_add_root`.
    ///
    /// `persist` controls whether the new entry is written to
    /// `.qartez/workspace.toml`. `attach_watcher` controls whether a
    /// `notify` watcher is spawned for the new root after indexing
    /// succeeds. `source` tags the in-memory origin so
    /// `qartez_list_roots` can label it.
    pub(in crate::server) fn add_root_inner(
        &self,
        alias: &str,
        path_str: &str,
        persist: bool,
        attach_watcher: bool,
        source: RootSource,
    ) -> Result<String, String> {
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

        // Symmetrical guard against the primary-root wipe scenario.
        // `remove_workspace` already refuses to remove an alias whose
        // path is inside the primary root because `delete_files_by_prefix`
        // would purge primary-owned files. The add side previously had
        // no such check, which let `qartez_add_root path=qartez-public`
        // (a subdirectory of the primary) and `qartez_add_root path=../..`
        // (an ancestor that contains the primary) succeed and produce
        // duplicate / overlapping rows. Reject both up front: a path
        // that is the primary, lives inside the primary, or contains
        // the primary cannot be a separate root.
        let primary_canonical = self.project_root.canonicalize().ok();
        if let Some(ref primary) = primary_canonical {
            if canonical_path == *primary {
                return Err(format!(
                    "Workspace '{alias}' could not be added: path '{}' is the primary project root.",
                    canonical_path.display(),
                ));
            }
            if canonical_path.starts_with(primary) {
                return Err(format!(
                    "Workspace '{alias}' could not be added: path '{}' is inside the primary project root '{}'. A nested directory cannot be registered as a separate root - its files are already indexed under the primary. If you need a sibling root, pass a path outside the primary tree.",
                    canonical_path.display(),
                    primary.display(),
                ));
            }
            if primary.starts_with(&canonical_path) {
                return Err(format!(
                    "Workspace '{alias}' could not be added: path '{}' contains the primary project root '{}'. Adding an ancestor directory would re-index the primary tree under a duplicate prefix.",
                    canonical_path.display(),
                    primary.display(),
                ));
            }
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

        // Tag the root's origin so `qartez_list_roots` can render it.
        // Done after the project_roots / root_aliases write to keep
        // the three maps in sync if any earlier step bailed out.
        self.record_root_source(&canonical_path, source);

        if persist {
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
        }

        // Hot-attach a watcher so saves under the new root reindex
        // without a restart. The startup watcher loop captures the
        // initial root list as a closure; without this hook a
        // runtime-added root sees no incremental updates.
        let mut watch_warning: Option<String> = None;
        if attach_watcher {
            // Multi-root indexing keys files with a `<alias>/` prefix
            // so sibling roots don't collide on `files.path`. Detect
            // the multi-root state by reading `project_roots` after
            // the new root has been appended.
            let multi_root = {
                let roots = self.project_roots.read().map_err(|e| e.to_string())?;
                roots.len() > 1
            };
            let prefix = if multi_root {
                index::root_prefix(&canonical_path, Some(alias))
            } else {
                String::new()
            };
            if let Err(e) = self.attach_watcher(canonical_path.clone(), prefix) {
                // A watcher failure must not roll back the index/registry
                // mutations. The new root is fully usable for queries; only
                // live-reload of subsequent edits is impacted.
                watch_warning = Some(format!(
                    "// warning: indexed and registered, but failed to attach watcher: {e}\n"
                ));
            }
        }

        let mut out = String::new();
        if let Some(w) = watch_warning {
            out.push_str(&w);
        }
        out.push_str(&format!("Added domain '{alias}' at '{stored_path}'."));
        Ok(out)
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

#[tool_router(router = qartez_add_root_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_add_root",
        description = "Register an additional project root at runtime. Indexes the directory, updates pagerank/co-change, and (by default) hot-attaches a file watcher so subsequent edits reindex live. Distinct from `qartez_workspace add` in that the alias is optional (derived from the path basename) and persistence to `.qartez/workspace.toml` can be toggled off for ephemeral roots.",
        annotations(
            title = "Add Project Root",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub(in crate::server) fn qartez_add_root(
        &self,
        Parameters(params): Parameters<SoulAddRootParams>,
    ) -> Result<String, String> {
        let path_str = params.path.trim();
        if path_str.is_empty() {
            return Err("`path` must not be empty.".to_string());
        }

        // Resolve the alias up-front so we can disambiguate against
        // the live aliases map if the caller did not pass one. The
        // canonicalisation + collision checks inside `add_root_inner`
        // still apply to the resolved name.
        let alias = match params.alias.as_deref().map(str::trim) {
            Some(a) if !a.is_empty() => a.to_string(),
            _ => {
                let expanded = expand_path(path_str, &self.project_root);
                let canonical = expanded.canonicalize().unwrap_or(expanded);
                let aliases = self.root_aliases.read().map_err(|e| e.to_string())?.clone();
                derive_alias_from_path(&canonical, &aliases)?
            }
        };

        let persist = params.persist.unwrap_or(true);
        let attach = params.watch.unwrap_or_else(|| self.watch_enabled());

        self.add_root_inner(&alias, path_str, persist, attach, RootSource::Runtime)
    }
}

#[tool_router(router = qartez_list_roots_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_list_roots",
        description = "List every project root currently tracked by the server. Each entry shows the canonical path, alias, source (cli / config / runtime), whether a file watcher is attached, the file count under that root, and the last index timestamp when available.",
        annotations(
            title = "List Project Roots",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_list_roots(
        &self,
        Parameters(params): Parameters<SoulListRootsParams>,
    ) -> Result<String, String> {
        let concise = matches!(params.format, Some(Format::Concise));

        let roots = self
            .project_roots
            .read()
            .map_err(|e| e.to_string())?
            .clone();
        let aliases = self.root_aliases.read().map_err(|e| e.to_string())?.clone();

        if roots.is_empty() {
            return Ok("No project roots are currently registered.".to_string());
        }

        let watcher_set: std::collections::HashSet<PathBuf> =
            self.watcher_roots().into_iter().collect();

        let last_index = {
            let conn = self.db.lock().map_err(|e| e.to_string())?;
            read::get_meta(&conn, "last_index")
                .ok()
                .flatten()
                .unwrap_or_default()
        };

        // Compute one file count per root.
        //
        // Multi-root mode normally prefixes every file row with
        // `<alias>/`, so each root's files are the rows that start with
        // its prefix. A root with no alias entry is treated as the
        // legacy unprefixed primary: its files are the rows that do
        // NOT start with any sibling root's prefix. Without this carve-
        // out the count for the unprefixed primary collapsed to 0 even
        // though its rows were intact in the DB - `root_prefix(root, None)`
        // returned the directory basename (e.g. `qartez`), which the
        // legacy unprefixed paths (`src/lib.rs`, ...) never matched.
        let file_counts: std::collections::HashMap<PathBuf, usize> = {
            let conn = self.db.lock().map_err(|e| e.to_string())?;
            let all_files = read::get_all_files(&conn).map_err(|e| e.to_string())?;
            let mut counts: std::collections::HashMap<PathBuf, usize> =
                std::collections::HashMap::new();
            if roots.len() <= 1 {
                if let Some(root) = roots.first() {
                    counts.insert(root.clone(), all_files.len());
                }
            } else {
                // Build the set of sibling prefixes once so the legacy-
                // primary count can subtract every aliased prefix.
                let aliased_prefixes: Vec<String> = roots
                    .iter()
                    .filter_map(|r| {
                        aliases
                            .get(r)
                            .map(|alias| format!("{}/", index::root_prefix(r, Some(alias))))
                    })
                    .collect();
                for root in &roots {
                    let count = if let Some(alias) = aliases.get(root) {
                        let with_slash = format!("{}/", index::root_prefix(root, Some(alias)));
                        all_files
                            .iter()
                            .filter(|f| f.path.starts_with(&with_slash))
                            .count()
                    } else {
                        all_files
                            .iter()
                            .filter(|f| !aliased_prefixes.iter().any(|p| f.path.starts_with(p)))
                            .count()
                    };
                    counts.insert(root.clone(), count);
                }
            }
            counts
        };

        let mut out = String::new();
        out.push_str(&format!("# Project Roots ({} registered)\n\n", roots.len()));

        if concise {
            for root in &roots {
                let alias = aliases
                    .get(root)
                    .cloned()
                    .unwrap_or_else(|| "(primary)".to_string());
                out.push_str(&format!("- {alias} -> {}\n", root.display()));
            }
            return Ok(out);
        }

        out.push_str("| alias | path | source | watcher | files | last_index |\n");
        out.push_str("|---|---|---|---|---|---|\n");
        for root in &roots {
            let alias = aliases
                .get(root)
                .cloned()
                .unwrap_or_else(|| "(primary)".to_string());
            let source = self.root_source_for(root).as_str();
            let watching = if watcher_set.contains(root) {
                "yes"
            } else {
                "no"
            };
            let files = file_counts.get(root).copied().unwrap_or(0);
            let stamp = if last_index.is_empty() {
                "(never)".to_string()
            } else {
                last_index.clone()
            };
            out.push_str(&format!(
                "| {alias} | {} | {source} | {watching} | {files} | {stamp} |\n",
                root.display(),
            ));
        }

        Ok(out)
    }
}
