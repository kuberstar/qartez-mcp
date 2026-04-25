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

#[tool_router(router = qartez_rename_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_rename",
        description = "Rename a symbol across the entire codebase: definition, imports, and all usages. Uses tree-sitter AST matching when available, falls back to word-boundary matching. When the name is shared by multiple kinds or defined in multiple files, pass `kind` and/or `file_path` to disambiguate - the tool refuses to run otherwise. Set `allow_collision=true` to proceed when `new_name` already exists as a defined symbol in a touched file. Preview by default; set apply=true to execute.",
        annotations(
            title = "Rename Symbol",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_rename(
        &self,
        Parameters(params): Parameters<SoulRenameParams>,
    ) -> Result<String, String> {
        // Surface empty inputs as a validation error instead of letting
        // the resolver fall through to `"No symbol found with name ''"`.
        // That message framed the shape bug as a data miss, which cost
        // minutes of caller debugging for what is a two-word fix.
        if params.old_name.trim().is_empty() {
            return Err("Refusing to rename: `old_name` is empty.".to_string());
        }
        if params.old_name == params.new_name {
            return Ok(format!(
                "No-op: old_name and new_name are identical ('{}').",
                params.old_name,
            ));
        }

        // Identifier-shape validation. A rename that slips a Rust keyword,
        // a leading digit, or an identifier-illegal character into the
        // target file does not produce a "rename broke one place" bug, it
        // produces a parse failure the caller has to unpick by hand.
        if let Some(err) = validate_new_name(&params.new_name) {
            return Err(err);
        }

        // Builtin-method-name guard. Names like `new`, `default`, `from`,
        // `clone`, `len`, etc. live on every other trait impl in the
        // codebase; even a kind-filtered rename FROM one of these names
        // hits dozens of unrelated `.new()` / `.clone()` sites because
        // tree-sitter cannot bind receiver types. Renaming TO one of
        // these names is almost as hostile because every file that
        // already calls a same-named builtin will still compile but now
        // routes through the renamed symbol. Require `allow_collision=true`
        // as an explicit override.
        let allow_collision = params.allow_collision.unwrap_or(false);
        if !allow_collision
            && let Some(name) = is_builtin_method_name(&params.old_name)
                .or_else(|| is_builtin_method_name(&params.new_name))
        {
            return Err(format!(
                "Refusing to rename: '{name}' is a builtin trait/inherent method name (new, default, from, clone, len, iter, next, ...). A plain rename would rewrite every unrelated `.{name}()` call site because tree-sitter cannot bind receiver types across the project - even with `file_path` set, call sites in OTHER files that hit a same-named builtin on a DIFFERENT type would still be rewritten and produce wrong-target bindings. Pass `allow_collision=true` to proceed anyway (you are asserting you have audited the unrelated sites).",
            ));
        }

        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;

        // Resolve candidate definitions with the caller-supplied kind /
        // file_path filters. When multiple definitions remain AND the caller
        // has not narrowed via either filter, refuse - a silent match-all
        // rename across method/free-fn or cross-file same-name symbols was
        // the root cause of the rewrite-every-HashMap::new() incident.
        let candidates = read::find_symbol_by_name_filtered(
            &conn,
            &params.old_name,
            params.kind.as_deref(),
            params.file_path.as_deref(),
        )
        .map_err(|e| format!("DB error: {e}"))?;

        if candidates.is_empty() {
            // Distinguish "symbol does not exist" from "symbol exists
            // but the disambiguator hint excluded every candidate".
            // Before this split, a bad `kind` / `file_path` looked
            // identical to a typo in the symbol name and the caller
            // had no way to tell whether to fix the name, the kind,
            // or the path.
            let any_match = read::find_symbol_by_name(&conn, &params.old_name)
                .map_err(|e| format!("DB error: {e}"))?;
            if any_match.is_empty() {
                return Err(format!("No symbol found with name '{}'", params.old_name));
            }
            let available_kinds: std::collections::BTreeSet<String> =
                any_match.iter().map(|(s, _)| s.kind.clone()).collect();
            let available_files: std::collections::BTreeSet<String> =
                any_match.iter().map(|(_, f)| f.path.clone()).collect();
            // Cap the file list shown to the caller. A `file_path` typo on
            // a heavily-imported symbol (`HashMap`, `Result`, `Option`)
            // could otherwise dump hundreds of paths from every indexed
            // root into the error message, which is both noisy and a
            // gratuitous information leak when the workspace contains
            // multiple roots the caller did not deliberately query.
            const MAX_AVAILABLE_FILES_SHOWN: usize = 20;
            let total_available = available_files.len();
            let shown_files: Vec<String> = available_files
                .iter()
                .take(MAX_AVAILABLE_FILES_SHOWN)
                .cloned()
                .collect();
            let mut parts: Vec<String> = Vec::new();
            if let Some(k) = params.kind.as_deref().filter(|s| !s.is_empty()) {
                parts.push(format!(
                    "kind='{k}' did not match any definition (available kinds: {})",
                    available_kinds.into_iter().collect::<Vec<_>>().join(", "),
                ));
            }
            if let Some(fp) = params.file_path.as_deref().filter(|s| !s.is_empty()) {
                let suffix = if total_available > MAX_AVAILABLE_FILES_SHOWN {
                    format!(
                        ", ... ({} more)",
                        total_available - MAX_AVAILABLE_FILES_SHOWN,
                    )
                } else {
                    String::new()
                };
                parts.push(format!(
                    "file_path='{fp}' did not match any definition (available files: {}{suffix})",
                    shown_files.join(", "),
                ));
            }
            return Err(format!(
                "Symbol '{}' exists in the index but the disambiguator filter excluded every candidate: {}",
                params.old_name,
                parts.join("; "),
            ));
        }

        let distinct_kinds: std::collections::BTreeSet<String> =
            candidates.iter().map(|(s, _)| s.kind.clone()).collect();
        let distinct_files: std::collections::BTreeSet<String> =
            candidates.iter().map(|(_, f)| f.path.clone()).collect();

        let kind_set = params.kind.as_deref().filter(|s| !s.is_empty()).is_some();
        let file_hint_set = params
            .file_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .is_some();
        // Disambiguate whenever more than one candidate survives the filters.
        // The previous guard tripped only when NEITHER `kind` NOR `file_path`
        // was set, which meant `kind=method` alone happily covered 7 distinct
        // `impl ... { fn same_name() }` blocks and renamed every one at once.
        // Requiring `file_path` once the caller already picked `kind` keeps
        // the rename scoped to a single definition.
        if candidates.len() > 1 && !file_hint_set {
            let locations: Vec<String> = candidates
                .iter()
                .map(|(s, f)| {
                    format!(
                        "  {} ({}) in {} [L{}-L{}]",
                        s.name, s.kind, f.path, s.line_start, s.line_end
                    )
                })
                .collect();
            let hint = if !kind_set && distinct_kinds.len() > 1 {
                "Pass `kind` (e.g. 'function', 'method') to pick one, or `file_path` to scope to a single file."
            } else if distinct_files.len() > 1 {
                "Pass `file_path` to pick a single defining file."
            } else {
                "Pass `kind` and/or `file_path` to disambiguate."
            };
            return Err(format!(
                "Refusing to rename '{}': multiple definitions found. {hint}\n{}",
                params.old_name,
                locations.join("\n"),
            ));
        }

        // Restrict the rename to the disambiguated defining-file set. When
        // fallback (text-only) scanning is the sole signal for a file, we
        // demand a `file_path` filter so the scan never crosses into code
        // the caller did not explicitly name.
        let allowed_def_files: std::collections::BTreeSet<String> =
            candidates.iter().map(|(_, f)| f.path.clone()).collect();

        // Fetch reference graph limited to the disambiguated symbol slot.
        let refs = read::get_symbol_references_filtered(
            &conn,
            &params.old_name,
            params.kind.as_deref(),
            params.file_path.as_deref(),
        )
        .map_err(|e| format!("DB error: {e}"))?;

        if refs.is_empty() {
            return Err(format!(
                "No symbol found with name '{}' after applying kind / file_path filter.",
                params.old_name,
            ));
        }

        // Union every file that could host an occurrence: the def file,
        // every edge-graph importer (unfiltered - the previous
        // `specifier.contains(old_name)` filter dropped real callers when
        // the `use` statement imported the parent module, e.g.
        // `use crate::storage::read;` followed by `read::symbol(...)`, or
        // `use super::*;` in child test modules), and every file surfaced
        // by the body-FTS fallback (catches external-crate imports and
        // Rust module-form `use` statements whose resolver mis-routes the
        // edge to `mod.rs`). Preview-mode renames ship to the caller as
        // the ground truth for an apply step - missing a site here means
        // the apply breaks the build.
        let mut file_set: BTreeSet<String> = BTreeSet::new();
        for (_, def_file, importers) in &refs {
            file_set.insert(def_file.path.clone());
            for (_, importer_file, _from_symbol_id) in importers {
                file_set.insert(importer_file.path.clone());
            }
        }
        if let Ok(paths) = read::find_file_paths_by_body_text(&conn, &params.old_name) {
            for path in paths {
                file_set.insert(path);
            }
        }
        // Trait-impl awareness: when the rename target is a trait or
        // interface, every `impl OldTrait for X` block carries the trait
        // name as a bare identifier. `get_symbol_references_filtered` and
        // body FTS together do not reliably surface those files when the
        // implementor relies on a prelude re-export or lives in the same
        // module as the trait. The `type_hierarchy` table is the
        // authoritative record of those sites; pull every subtype's file
        // into the scan set so the per-file tree-sitter walk below
        // rewrites the `impl OldTrait for X` line along with everything
        // else.
        let defining_kinds: std::collections::BTreeSet<String> = candidates
            .iter()
            .map(|(s, _)| s.kind.to_ascii_lowercase())
            .collect();
        let is_trait_rename = defining_kinds
            .iter()
            .any(|k| k == "trait" || k == "interface");
        let mut trait_impl_files: BTreeSet<String> = BTreeSet::new();
        if is_trait_rename && let Ok(subtypes) = read::get_subtypes(&conn, &params.old_name) {
            for (_rel, file) in subtypes {
                trait_impl_files.insert(file.path.clone());
                file_set.insert(file.path);
            }
        }
        // When the caller pinned to a single file via `file_path`, drop
        // homonym files from the scan set. Cross-file same-name symbols
        // (e.g. `is_test_path` defined in both src/a.rs and src/b.rs)
        // are legitimately distinct, and the body-FTS sweep surfaces
        // every file that mentions the name - a rewrite there would
        // corrupt the sibling symbol.
        let files_to_scan: Vec<String> = if file_hint_set {
            let mut result: BTreeSet<String> = allowed_def_files.clone();
            if let Some(fp) = params.file_path.as_deref() {
                result.insert(crate::index::to_forward_slash(fp.to_string()));
            }
            for (_, _, importers) in &refs {
                for (_, importer_file, _from_symbol_id) in importers {
                    result.insert(importer_file.path.clone());
                }
            }
            // Trait-impl sites must survive the file_path narrowing.
            // Dropping them would leave `impl OldTrait for X` untouched
            // in concrete-implementor files even though the caller's
            // narrowing was for the trait definition, not a call-site
            // filter.
            for impl_file in &trait_impl_files {
                result.insert(impl_file.clone());
            }
            result.into_iter().collect()
        } else {
            file_set.into_iter().collect()
        };

        // Detect collisions with `new_name` before any write. A rename that
        // silently collides with an existing symbol is indistinguishable in
        // the output from a legitimate merge, and the resulting source
        // typically won't compile. Require opt-in via `allow_collision=true`.
        //
        // Scope: check the full project index, not just touched files. A
        // rename from `parse_file` to `Parser` must fail when `Parser`
        // already exists as a struct anywhere in the codebase, because the
        // renamed call-sites will start binding to that type.
        if !allow_collision {
            let mut collisions: Vec<String> = Vec::new();
            let touched_set: std::collections::BTreeSet<&str> =
                files_to_scan.iter().map(|s| s.as_str()).collect();
            let index_wide = read::find_symbol_by_name(&conn, &params.new_name).unwrap_or_default();
            for (s, f) in &index_wide {
                let in_touched = touched_set.contains(f.path.as_str());
                collisions.push(format!(
                    "  {} ({}) in {} [L{}-L{}]{}",
                    s.name,
                    s.kind,
                    f.path,
                    s.line_start,
                    s.line_end,
                    if in_touched { " (touched)" } else { "" },
                ));
            }
            if !collisions.is_empty() {
                return Err(format!(
                    "Refusing to rename '{}' -> '{}': new_name is already defined in the codebase. Pass `allow_collision=true` to proceed anyway.\n{}",
                    params.old_name,
                    params.new_name,
                    collisions.join("\n"),
                ));
            }
        }
        drop(conn);

        let apply = params.apply.unwrap_or(false);
        // (file_path, line_number, old_line_text, new_line_text)
        let mut changes: Vec<(String, usize, String, String)> = Vec::new();
        // Per-file AST-based byte ranges: file_path -> [(line, byte_start, byte_end)]
        let mut ast_ranges: HashMap<String, Vec<(usize, usize, usize)>> = HashMap::new();

        // Files where we actually found a rename target. Kept separate
        // from `files_to_scan` because the FTS-based scan set is
        // deliberately generous - it includes files that mention the name
        // only inside strings or comments - and we must not rewrite those
        // false positives on apply.
        let mut files_touched: Vec<String> = Vec::new();

        // Occurrence counts tracked separately from `changes`. The non-AST
        // branch collapses multiple word-boundary matches on the same line
        // into a single preview row whose `new_line` rewrites every site
        // (via `replace_whole_word`), so `changes.len()` no longer equals
        // the occurrence count. The summary numbers below read these maps.
        let mut total_occurrences: usize = 0;
        let mut per_file_occurrences: HashMap<String, usize> = HashMap::new();

        for rel_path in &files_to_scan {
            // Prefer the shared parse cache so repeat invocations (warmup +
            // measured benchmark runs, or multi-file renames that revisit
            // the definition file) skip tree-sitter reparsing entirely. The
            // cache is keyed by relative path + mtime, so a file edited on
            // disk forces a reparse on the next call. `cached_idents`
            // performs a single grouped walk per file lifetime; a lookup
            // for any name is then an O(1) HashMap hit.
            match self.cached_idents(rel_path) {
                Some(idents_map) => {
                    // AST-supported language (tree-sitter parsed the file).
                    // Missing from the map means there is no identifier
                    // with that name in this file — the FTS hit was in a
                    // string literal or comment. Skip the file entirely;
                    // falling through to substring matching would rewrite
                    // those non-code mentions and corrupt the build.
                    let Some(occurrences) = idents_map.get(&params.old_name) else {
                        continue;
                    };
                    if occurrences.is_empty() {
                        continue;
                    }
                    let source_arc = self.cached_source(rel_path).ok_or_else(|| {
                        let display = self
                            .safe_resolve(rel_path)
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|_| rel_path.to_string());
                        format!("Cannot read {display}")
                    })?;
                    let content: &str = source_arc.as_str();
                    let lines: Vec<&str> = content.lines().collect();
                    for &(line_num, start, end) in occurrences.iter() {
                        let line_idx = line_num - 1;
                        if line_idx < lines.len() {
                            let old_line = lines[line_idx].to_string();
                            let line_byte_start =
                                content[..start].rfind('\n').map(|p| p + 1).unwrap_or(0);
                            let offset_in_line = start - line_byte_start;
                            let end_offset = end - line_byte_start;
                            let new_line = format!(
                                "{}{}{}",
                                &old_line[..offset_in_line],
                                &params.new_name,
                                &old_line[end_offset..],
                            );
                            changes.push((rel_path.clone(), line_num, old_line, new_line));
                        }
                    }
                    total_occurrences += occurrences.len();
                    *per_file_occurrences.entry(rel_path.clone()).or_insert(0) += occurrences.len();
                    ast_ranges.insert(rel_path.clone(), occurrences.clone());
                    files_touched.push(rel_path.clone());
                }
                None => {
                    // Language not supported by tree-sitter - the only
                    // available signal is a word-boundary text scan. That's
                    // dangerously coarse: a bare name like `new` hits
                    // `HashMap::new()`, `Vec::new()`, `Regex::new()` and
                    // every docstring mention. Refuse to run this branch
                    // unless the caller pinned the rename to a single file
                    // AND the current file is one of the defining files or
                    // the caller's explicit file_path.
                    let is_defining_file = allowed_def_files.contains(rel_path);
                    let is_hinted_file = params
                        .file_path
                        .as_deref()
                        .map(|fp| crate::index::to_forward_slash(fp.to_string()) == *rel_path)
                        .unwrap_or(false);
                    if !file_hint_set || !(is_defining_file || is_hinted_file) {
                        // Skip the file silently - FTS hits in a file the
                        // caller did not disambiguate are not enough to
                        // justify a text-only rewrite. For AST-unsupported
                        // languages the caller must pass `file_path`.
                        continue;
                    }
                    let source_arc = self.cached_source(rel_path).ok_or_else(|| {
                        let display = self
                            .safe_resolve(rel_path)
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|_| rel_path.to_string());
                        format!("Cannot read {display}")
                    })?;
                    let content: &str = source_arc.as_str();
                    let mut file_had_hit = false;
                    for (line_num, line) in content.lines().enumerate() {
                        let Some((hits, new_line)) =
                            scan_line_for_rename(line, &params.old_name, &params.new_name)
                        else {
                            continue;
                        };
                        // One preview row per line: the line rewritten as
                        // `apply` would emit it. Counts tracked separately
                        // so the summary still reports true occurrences.
                        changes.push((rel_path.clone(), line_num + 1, line.to_string(), new_line));
                        total_occurrences += hits;
                        *per_file_occurrences.entry(rel_path.clone()).or_insert(0) += hits;
                        file_had_hit = true;
                    }
                    if file_had_hit {
                        files_touched.push(rel_path.clone());
                    }
                }
            }
        }

        if changes.is_empty() {
            return Ok(format!(
                "No occurrences of '{}' found in relevant files.",
                params.old_name,
            ));
        }

        if apply {
            let mut files_modified: HashSet<String> = HashSet::new();
            // Only rewrite files that had real identifier hits. An FTS
            // candidate that matched in a string or comment made it into
            // `files_to_scan` but was skipped during the AST walk above;
            // those files must stay untouched.
            for rel_path in &files_touched {
                let abs_path = self.safe_resolve(rel_path)?;
                let content = std::fs::read_to_string(&abs_path)
                    .map_err(|e| format!("Cannot read {}: {e}", abs_path.display()))?;

                let new_content = if let Some(ranges) = ast_ranges.get(rel_path) {
                    let mut sorted = ranges.clone();
                    sorted.sort_by_key(|&(_, start, _)| start);
                    let mut buf = content.clone();
                    for &(_, start, end) in sorted.iter().rev() {
                        buf.replace_range(start..end, &params.new_name);
                    }
                    buf
                } else {
                    replace_whole_word(&content, &params.old_name, &params.new_name)
                };

                if new_content != content {
                    let tmp_path = abs_path.with_extension("qartez_rename_tmp");
                    std::fs::write(&tmp_path, &new_content)
                        .map_err(|e| format!("Cannot write {}: {e}", tmp_path.display()))?;
                    std::fs::rename(&tmp_path, &abs_path).map_err(|e| {
                        let _ = std::fs::remove_file(&tmp_path);
                        format!("Cannot rename temp file to {}: {e}", abs_path.display())
                    })?;
                    files_modified.insert(rel_path.clone());
                }
            }

            let mut out = format!(
                "Renamed '{}' → '{}'. All references updated.\n",
                params.old_name, params.new_name,
            );
            out.push_str(&format!(
                "{} file(s) modified, {} occurrence(s) replaced:\n",
                files_modified.len(),
                total_occurrences,
            ));
            for f in &files_modified {
                let count = per_file_occurrences.get(f).copied().unwrap_or(0);
                out.push_str(&format!("  {f} ({count} changes)\n"));
            }
            Ok(out)
        } else {
            // Compact preview: "old -> new: N occurrences in M files", then
            // for each file a single line per changed line with just the
            // line number and the trimmed after-text. The before-line is
            // omitted (reader has the file) - delivers the same actionable
            // info at ~40% fewer tokens than the diff-style output used
            // previously.
            //
            // MCP transports cap response bytes; common-name renames can
            // otherwise dump >100 KB of occurrence rows and blow past the
            // cap, losing the header summary too. Bound the body at a
            // generous threshold and emit a truncation footer that tells
            // the caller how to narrow the scope or commit directly.
            const MAX_PREVIEW_BYTES: usize = 48 * 1024;
            let header = format!(
                "{} → {}: {} occ in {} file(s)\n",
                params.old_name,
                params.new_name,
                total_occurrences,
                files_touched.len(),
            );
            let mut body = String::new();
            let mut current_file = String::new();
            let mut emitted_files: HashSet<String> = HashSet::new();
            let mut emitted_occurrences: usize = 0;
            let mut truncated = false;
            for (file, line_num, _before, after) in &changes {
                let trimmed = after.trim();
                let mut row = String::new();
                if *file != current_file {
                    row.push_str(file);
                    row.push('\n');
                }
                row.push_str(&format!("  L{line_num}: {trimmed}\n"));
                if header.len() + body.len() + row.len() > MAX_PREVIEW_BYTES {
                    truncated = true;
                    break;
                }
                body.push_str(&row);
                if *file != current_file {
                    current_file = file.clone();
                    emitted_files.insert(file.clone());
                }
                emitted_occurrences += 1;
            }
            let mut out = header;
            out.push_str(&body);
            if truncated {
                let remaining_occ = total_occurrences.saturating_sub(emitted_occurrences);
                let remaining_files = files_touched.len().saturating_sub(emitted_files.len());
                out.push_str(&format!(
                    "... {remaining_files} more file(s) / {remaining_occ} more occurrence(s) truncated by preview cap. Pass `file_path` to narrow, or apply=true to execute without preview.\n",
                ));
            }
            Ok(out)
        }
    }
}

/// Reject `new_name` values that would never compile in any of the
/// languages qartez currently supports. Identifier shape is conservative:
/// every supported language admits `[A-Za-z_][A-Za-z0-9_]*`, so enforcing
/// that on the Rust side also prevents obvious TS/Python/Go parse errors.
/// Rust strict / reserved keywords are rejected explicitly so callers do
/// not rename `parse_file` to `if` and paint themselves into a corner.
fn validate_new_name(new_name: &str) -> Option<String> {
    if new_name.is_empty() {
        return Some("Refusing to rename: `new_name` is empty.".to_string());
    }
    // Bare `_` is a pattern placeholder, not a legal symbol name at module
    // scope. `fn _() {}` emits E0424 at parse time; a rename producing this
    // silently wedges the caller with a downstream build break.
    if new_name == "_" {
        return Some(
            "Refusing to rename: `new_name` '_' is a reserved placeholder identifier and cannot be used as a function, type, or module-scope name.".to_string(),
        );
    }
    // Raw-identifier support. `r#<keyword>` is Rust-legal syntax that lets
    // callers reuse reserved words as symbol names (`r#fn`, `r#type`). The
    // previous shape check treated `#` as illegal and rejected every raw
    // identifier even though the compiler accepts them; strip the prefix
    // so the downstream checks run against the identifier core only.
    let (prefix, core) = if let Some(stripped) = new_name.strip_prefix("r#") {
        ("r#", stripped)
    } else {
        ("", new_name)
    };
    if core.is_empty() {
        return Some(format!(
            "Refusing to rename: `new_name` '{new_name}' is the bare `r#` raw-identifier prefix without an identifier core.",
        ));
    }
    let first = core.chars().next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Some(format!(
            "Refusing to rename: `new_name` '{new_name}' is not a valid identifier (must start with an ASCII letter or underscore, optionally preceded by the `r#` raw-identifier prefix).",
        ));
    }
    if !core.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Some(format!(
            "Refusing to rename: `new_name` '{new_name}' contains characters outside [A-Za-z0-9_] (ignoring optional `r#` prefix).",
        ));
    }
    // Reserved-word gate fires ONLY for the non-prefixed form. `r#fn` is
    // the whole point of the `r#` escape hatch, so allow it through.
    if prefix.is_empty() && RUST_RESERVED.contains(&new_name) {
        return Some(format!(
            "Refusing to rename: `new_name` '{new_name}' is a Rust keyword or reserved word. Pick a different identifier, or prepend `r#` to use a raw identifier.",
        ));
    }
    None
}

/// Rust keywords (strict + reserved). A rename target that matches any of
/// these would emit a parse error at the first call site.
const RUST_RESERVED: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "union",
];

/// Return the matched name when `candidate` is a name that exists as a
/// builtin trait method (Iterator, IntoIterator, AsRef, Default, Clone,
/// From, Into, Deref, Drop, std::fmt::Display, ...) or an inherent method
/// present on most stdlib containers. A rename that touches one of these
/// without the caller's explicit consent ends up rewriting every
/// `.new()` / `.clone()` / `.iter()` site in every importer file, which
/// is never the user's intent.
fn is_builtin_method_name(candidate: &str) -> Option<&'static str> {
    const BUILTIN_METHOD_NAMES: &[&str] = &[
        "new",
        "default",
        "from",
        "into",
        "try_from",
        "try_into",
        "as_ref",
        "as_mut",
        "as_str",
        "as_slice",
        "clone",
        "drop",
        "deref",
        "deref_mut",
        "len",
        "is_empty",
        "iter",
        "iter_mut",
        "into_iter",
        "next",
        "fmt",
        "hash",
        "eq",
        "cmp",
        "partial_cmp",
        "to_string",
        "to_owned",
        "borrow",
        "borrow_mut",
        "unwrap",
        "expect",
        "map",
        "filter",
        "collect",
    ];
    BUILTIN_METHOD_NAMES
        .iter()
        .find(|n| **n == candidate)
        .copied()
}

/// Count whole-word occurrences of `old` in `line` and, when at least one
/// hit is found, return the fully-rewritten line (mirroring what the apply
/// step writes via `replace_whole_word`). Returns `None` for lines with no
/// word-boundary hit so callers can skip them cheaply.
///
/// Extracted from the non-AST branch of `qartez_rename` so the per-line
/// logic is unit-testable without standing up a full `QartezServer` with a
/// database. The behavior mirrors the previous inline loop except that
/// multiple matches on the same line now produce a single rewritten line
/// rather than one divergent `new_line` per site.
fn scan_line_for_rename(line: &str, old: &str, new: &str) -> Option<(usize, String)> {
    if old.is_empty() {
        return None;
    }
    let mut hits = 0usize;
    let mut start = 0;
    while let Some(pos) = line[start..].find(old) {
        let abs_pos = start + pos;
        let before_ok = line[..abs_pos]
            .chars()
            .next_back()
            .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_');
        let after_pos = abs_pos + old.len();
        let after_ok = line[after_pos..]
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_');
        if before_ok && after_ok {
            hits += 1;
        }
        start = after_pos;
    }
    if hits == 0 {
        None
    } else {
        Some((hits, replace_whole_word(line, old, new)))
    }
}

#[cfg(test)]
mod tests {
    use super::{is_builtin_method_name, scan_line_for_rename, validate_new_name};

    #[test]
    fn multi_occurrence_line_returns_single_fully_rewritten_line() {
        // Regression: previously two separate preview rows were emitted,
        // each with a `new_line` that replaced only its own site.
        let (hits, new_line) = scan_line_for_rename("foo foo bar", "foo", "qux").unwrap();
        assert_eq!(hits, 2);
        assert_eq!(new_line, "qux qux bar");
    }

    #[test]
    fn single_word_boundary_hit_rewrites_that_site() {
        let (hits, new_line) = scan_line_for_rename("bar foo baz", "foo", "qux").unwrap();
        assert_eq!(hits, 1);
        assert_eq!(new_line, "bar qux baz");
    }

    #[test]
    fn substring_only_match_is_ignored() {
        assert!(scan_line_for_rename("foobar", "foo", "qux").is_none());
    }

    #[test]
    fn empty_needle_returns_none() {
        assert!(scan_line_for_rename("foo bar", "", "qux").is_none());
    }

    #[test]
    fn validate_new_name_accepts_ordinary_identifier() {
        assert!(validate_new_name("parse_file").is_none());
        assert!(validate_new_name("_private").is_none());
        assert!(validate_new_name("Parser").is_none());
    }

    #[test]
    fn validate_new_name_rejects_digit_leading_and_spaces_and_keywords() {
        assert!(validate_new_name("123_bad").is_some());
        assert!(validate_new_name("has space").is_some());
        assert!(validate_new_name("if").is_some());
        assert!(validate_new_name("").is_some());
    }

    #[test]
    fn builtin_method_name_detection() {
        assert_eq!(is_builtin_method_name("new"), Some("new"));
        assert_eq!(is_builtin_method_name("clone"), Some("clone"));
        assert_eq!(is_builtin_method_name("parse_file"), None);
    }
}
