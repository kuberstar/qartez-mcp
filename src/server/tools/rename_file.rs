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
use super::refactor_common;

use crate::graph::blast;
use crate::guard;
use crate::storage::read;
use crate::storage::read::sanitize_fts_query;
use crate::toolchain;

#[tool_router(router = qartez_rename_file_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_rename_file",
        description = "Rename/move a file and rewrite all import paths pointing to it in one atomic operation. Also rewrites the `mod <stem>;` declaration in the parent module file so renaming a .rs file keeps the crate compiling. Refuses to rename `mod.rs` (module root) or to create a `mod.rs` over an existing one. Preview by default; set apply=true to execute.",
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
        // Canonicalize caller inputs before any disk-touching work so a
        // malformed `to` (absolute path, trailing slash, `..` traversal,
        // nonexistent parent) fails with a precise message instead of
        // tripping a later rename/write with a confusing OS error. The
        // same checks apply symmetrically to `from` so callers cannot
        // smuggle `..` traversals through the source argument either.
        validate_rename_path_arg(&params.from, "from")?;
        validate_rename_path_arg(&params.to, "to")?;

        // Refuse to rename a Rust `mod.rs` away from its directory-bound
        // name - doing so breaks the module resolver. Likewise, refuse to
        // create a NEW `mod.rs` if the target directory already contains
        // one, which would silently clobber the existing module root.
        let from_basename = std::path::Path::new(&params.from)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let to_basename = std::path::Path::new(&params.to)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if from_basename == "mod.rs" {
            return Err(format!(
                "Refusing to rename '{}': 'mod.rs' is a Rust module root - renaming it breaks module resolution. Restructure the module by moving contents, not by renaming mod.rs.",
                params.from,
            ));
        }
        if to_basename == "mod.rs" {
            let to_abs_preflight = self.safe_resolve(&params.to)?;
            if to_abs_preflight.exists() {
                return Err(format!(
                    "Refusing to rename '{}' -> '{}': target 'mod.rs' already exists in that directory.",
                    params.from, params.to,
                ));
            }
        }
        // Crate roots are registered in Cargo.toml by name, not module
        // resolution. Renaming `lib.rs` / `main.rs` at the crate root
        // quietly detaches the crate from its manifest and the build
        // fails with a cryptic `[[bin]]` error rather than the precise
        // refusal the caller would get for `mod.rs`. Symmetry demands
        // the same guard here.
        if is_crate_root_file(&params.from) {
            return Err(format!(
                "Refusing to rename '{}': Rust crate roots ('lib.rs' / 'main.rs' directly under a crate source dir) are registered in Cargo.toml by name. Renaming breaks the build. Restructure the crate by moving contents, not the entry point.",
                params.from,
            ));
        }
        // Build-system manifests (Cargo.toml, package.json, pyproject.toml,
        // go.mod, etc.) are discovered by exact basename by every toolchain
        // that owns them. Renaming detaches the package from its resolver
        // and the build fails with a cryptic error rather than the precise
        // refusal callers get for `mod.rs` / `lib.rs`. Symmetry with
        // `is_crate_root_file` demands the same guard here, applied to BOTH
        // `from` and `to` so a rename cannot smuggle a build-breaking
        // destination basename either.
        if let Some(manifest) = is_protected_manifest_file(&params.from) {
            return Err(format!(
                "Refusing to rename '{}': '{manifest}' is a build-system manifest discovered by basename (Cargo, npm, Python build backends, Go modules, etc.). Renaming detaches the package from its resolver. Restructure the package by moving its directory, not the manifest itself.",
                params.from,
            ));
        }
        if let Some(manifest) = is_protected_manifest_file(&params.to) {
            return Err(format!(
                "Refusing to rename '{}' -> '{}': target basename '{manifest}' is a build-system manifest. Creating one by rename clobbers any existing manifest discovery and breaks the build. Pick a non-manifest destination basename.",
                params.from, params.to,
            ));
        }

        let from_norm = crate::index::to_forward_slash(params.from.clone());
        let to_norm = crate::index::to_forward_slash(params.to.clone());
        if from_norm == to_norm {
            return Err(format!(
                "Refusing to rename '{}' -> '{}': source and target are the same path. Pass a distinct `to` to rename.",
                params.from, params.to,
            ));
        }

        let to_abs_precheck = self.safe_resolve(&params.to)?;
        if to_abs_precheck.exists() {
            return Err(format!(
                "Refusing to rename '{}' -> '{}': target file already exists. Delete it first or pick a different destination - a rename would overwrite unrelated contents.",
                params.from, params.to,
            ));
        }

        // Absolute `from` paths are rejected up front so callers get a
        // precise diagnostic instead of the generic "not found in
        // index" message, which previously masked the real failure
        // mode. The indexer always stores project-relative paths, so
        // `get_file_by_path` with an absolute key would return `None`
        // for every valid file and the caller had no way to tell the
        // mistake from a genuine missing-file case. `is_absolute`
        // handles POSIX `/abs` and Windows `C:\abs` uniformly.
        if std::path::Path::new(&params.from).is_absolute() {
            return Err(format!(
                "absolute paths not supported; pass a relative path under the project root (got '{}')",
                params.from,
            ));
        }
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

        // Parent mod.rs / lib.rs / main.rs that declares `mod <stem>;`.
        // The edges table only records `use` imports, so a sibling-parent
        // `pub mod parser;` line never shows up as an importer - without
        // this step `rename_file` would report "0 importers" for a file
        // whose sole entry point is the parent module declaration, and
        // the apply would still rewrite the decl but leave the caller
        // blind during preview.
        let parent_mod_importer: Option<String> = if old_rel_stem != new_rel_stem {
            find_parent_mod_file(&self.project_root, &params.from).and_then(|p| {
                // The rest of the repo treats relative paths with forward
                // slashes (index rows, `use` import stems, caller-facing
                // previews). `PathBuf::to_string_lossy` preserves the OS
                // separator, so on Windows `p` ends up as `src/index\mod.rs`
                // when `find_parent_mod_file` joined the parent directory
                // to a `mod.rs` basename. Normalise to `/` so preview
                // output matches the format the rest of the tool surface
                // uses - and the Windows CI assertion that broke v0.9.4.
                let rel = p.to_string_lossy().replace('\\', "/");
                let abs = self.project_root.join(&p);
                match std::fs::read_to_string(&abs) {
                    Ok(content) => {
                        // Confirm a matching `mod <old>;` line actually
                        // exists in the parent before advertising it as
                        // an importer. `rewrite_mod_decl` is a no-op
                        // when nothing matches, but the preview should
                        // not mention a file that won't be touched.
                        let rewritten = rewrite_mod_decl(&content, &old_rel_stem, &new_rel_stem);
                        if rewritten != content {
                            Some(rel)
                        } else {
                            None
                        }
                    }
                    Err(_) => None,
                }
            })
        } else {
            None
        };
        if let Some(p) = parent_mod_importer.as_deref()
            && !importer_paths.iter().any(|ip| ip == p)
        {
            importer_paths.push(p.to_string());
        }

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

        let mut updated_count = 0;
        let mut failed_writes: Vec<String> = Vec::new();
        // Replace the longest matching stem first so `src::foo::bar →
        // src::baz::qux` swaps whole-path imports before the fallback
        // `foo::bar → baz::qux` sweep catches `use crate::foo::bar`. The
        // previous approach did a bare `foo` → `qux` pass that accidentally
        // renamed any identifier sharing the file stem.
        let stem_pairs = rename_stem_pairs(&old_stem, &new_stem);
        for importer_path in &importer_paths {
            let importer_abs = match self.safe_resolve(importer_path) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let content = match std::fs::read_to_string(&importer_abs) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let updated = apply_rename_pairs(&content, &stem_pairs)?;

            if updated != content {
                // `write_atomic` writes to a sibling temp file then renames
                // it over the importer, so a kill-9 mid-rewrite leaves the
                // original file intact rather than a half-written one. The
                // mv/replace/insert tools share this discipline; using
                // plain `std::fs::write` here would give rename_file a
                // weaker durability profile than its peers.
                if let Err(e) = refactor_common::write_atomic(&importer_abs, &updated) {
                    failed_writes.push(e);
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
                    if let Err(e) = refactor_common::write_atomic(&parent_abs, &rewritten) {
                        failed_writes.push(e);
                    } else {
                        mod_rewrite_note =
                            format!(", parent mod decl updated in {}", parent_rel.display(),);
                    }
                }
            }
        }

        // Auto-create any missing parent directories so rename_file can
        // move a file into a new subdirectory in one step. `validate_rename_path_arg`
        // already rejected `..` traversal and malformed paths above, so
        // everything reaching this line is a legitimate project-relative
        // target. The previous "parent must pre-exist" guard broke
        // refactor flows that create the destination directory as part
        // of the rename.
        if let Some(parent) = to_abs.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot create parent directory {}: {e}", parent.display()))?;
        }
        // Rename happens LAST so a mid-way write failure above leaves the
        // source file at its original path and the user can re-run the
        // tool to finish the partial operation - apply_rename_pairs is
        // idempotent over already-rewritten content. The previous order
        // (rename first, importers second) stranded importers pointing
        // at a vanished path on any partial write failure, matching the
        // target-first discipline already used by qartez_mv.
        std::fs::rename(&from_abs, &to_abs).map_err(|e| {
            format!(
                "Cannot rename {} -> {}: {e}",
                from_abs.display(),
                to_abs.display()
            )
        })?;

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

/// Validate a caller-supplied relative path argument (`from` / `to`) for
/// `qartez_rename_file`. Rejects empty strings, absolute paths, `..`
/// parent traversals, and trailing slashes (which hint at a directory
/// target and never at a file rename). The `arg_name` is included in
/// every error so callers can tell which side tripped the guard.
fn validate_rename_path_arg(raw: &str, arg_name: &str) -> Result<(), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("`{arg_name}` must be non-empty"));
    }
    let norm = crate::index::to_forward_slash(trimmed.to_string());
    if std::path::Path::new(&norm).is_absolute() || norm.starts_with('/') {
        return Err(format!(
            "`{arg_name}` absolute path not supported; pass a relative path under the project root. Got: '{raw}'",
        ));
    }
    if norm.ends_with('/') {
        return Err(format!(
            "`{arg_name}` trailing slash not supported; pass a file path, not a directory. Got: '{raw}'",
        ));
    }
    for seg in norm.split('/') {
        if seg == ".." {
            return Err(format!(
                "`{arg_name}` `..` parent-directory components are not supported; pass a path under the project root. Got: '{raw}'",
            ));
        }
    }
    Ok(())
}

/// Return true when `rel_path` points to a Rust crate root entry point
/// (`lib.rs` / `main.rs`) directly under a crate's `src/` directory.
/// Matches `src/lib.rs`, `src/main.rs`, `crates/foo/src/lib.rs`, and
/// workspace-nested variants. Single-segment paths like `lib.rs` in a
/// flat crate root also match.
fn is_crate_root_file(rel_path: &str) -> bool {
    let norm = rel_path.replace('\\', "/");
    let basename = match norm.rsplit('/').next() {
        Some(b) => b,
        None => return false,
    };
    if basename != "lib.rs" && basename != "main.rs" {
        return false;
    }
    // A bare `lib.rs` / `main.rs` in a flat layout is always a crate root.
    let parent: &str = match norm.rfind('/') {
        Some(i) => &norm[..i],
        None => return true,
    };
    if parent.is_empty() {
        return true;
    }
    // Match the conventional Cargo layout: files that sit directly under
    // a directory named `src` (or `src/bin`, which registers named
    // binaries) are crate entry points. Comparing segments instead of
    // doing a substring match handles bare `src/bin/main.rs` (no leading
    // path prefix) and workspace-nested `foo/src/bin/main.rs` uniformly.
    let segments: Vec<&str> = parent.split('/').filter(|s| !s.is_empty()).collect();
    let last = segments.last().copied().unwrap_or("");
    let second_last = if segments.len() >= 2 {
        segments[segments.len() - 2]
    } else {
        ""
    };
    last == "src" || (last == "bin" && second_last == "src")
}

/// Detect build-system manifests that toolchains (Cargo, npm, Python build
/// backends, Go modules, etc.) discover by exact basename. Renaming such
/// a file detaches the package from its resolver and surfaces as a cryptic
/// build error rather than a precise refusal. The check is basename-only -
/// directory layout does not matter, because every one of these names is
/// reserved by basename across the entire ecosystem.
fn is_protected_manifest_file(rel_path: &str) -> Option<&'static str> {
    const MANIFEST_BASENAMES: &[&str] = &[
        "Cargo.toml",
        "Cargo.lock",
        "package.json",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "go.mod",
        "go.sum",
        "build.gradle",
        "build.gradle.kts",
        "pom.xml",
        "Gemfile",
        "Gemfile.lock",
        "composer.json",
        "composer.lock",
        "Pipfile",
        "Pipfile.lock",
    ];
    let norm = rel_path.replace('\\', "/");
    let basename = norm.rsplit('/').next()?;
    MANIFEST_BASENAMES.iter().find(|n| **n == basename).copied()
}

#[cfg(test)]
mod tests {
    use super::{is_crate_root_file, is_protected_manifest_file, validate_rename_path_arg};

    #[test]
    fn validate_path_arg_rejects_absolute_and_traversal() {
        assert!(validate_rename_path_arg("/etc/passwd", "to").is_err());
        assert!(validate_rename_path_arg("../escape.rs", "to").is_err());
        assert!(validate_rename_path_arg("src/../a.rs", "to").is_err());
        assert!(validate_rename_path_arg("src/a/", "to").is_err());
        assert!(validate_rename_path_arg("", "to").is_err());
        assert!(validate_rename_path_arg("   ", "to").is_err());
        assert!(validate_rename_path_arg("src/a.rs", "to").is_ok());
        assert!(validate_rename_path_arg("a.rs", "to").is_ok());
    }

    #[test]
    fn crate_root_detection_matches_canonical_layouts() {
        assert!(is_crate_root_file("qartez-public/src/lib.rs"));
        assert!(is_crate_root_file("qartez-public/src/main.rs"));
        assert!(is_crate_root_file("src/lib.rs"));
        assert!(is_crate_root_file("lib.rs"));
        assert!(is_crate_root_file("crates/foo/src/main.rs"));
        assert!(is_crate_root_file(
            "src/bin/tool.rs".replace("tool.rs", "main.rs").as_str()
        ));
    }

    #[test]
    fn crate_root_detection_rejects_module_files() {
        assert!(!is_crate_root_file("qartez-public/src/index/mod.rs"));
        assert!(!is_crate_root_file("qartez-public/src/index/parser.rs"));
        assert!(!is_crate_root_file("qartez-public/tests/main.rs"));
        assert!(!is_crate_root_file("qartez-public/examples/lib.rs"));
    }

    #[test]
    fn manifest_detection_recognizes_canonical_basenames() {
        assert_eq!(is_protected_manifest_file("Cargo.toml"), Some("Cargo.toml"));
        assert_eq!(
            is_protected_manifest_file("crates/foo/Cargo.toml"),
            Some("Cargo.toml")
        );
        assert_eq!(
            is_protected_manifest_file("frontend/package.json"),
            Some("package.json")
        );
        assert_eq!(
            is_protected_manifest_file("services/svc/go.mod"),
            Some("go.mod")
        );
        assert_eq!(
            is_protected_manifest_file("libs/x/pyproject.toml"),
            Some("pyproject.toml")
        );
    }

    #[test]
    fn manifest_detection_rejects_non_manifest_files() {
        assert!(is_protected_manifest_file("src/lib.rs").is_none());
        assert!(is_protected_manifest_file("README.md").is_none());
        assert!(is_protected_manifest_file("Cargo.toml.bak").is_none());
        assert!(is_protected_manifest_file("my_package.json").is_none());
    }
}
