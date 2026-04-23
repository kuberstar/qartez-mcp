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
        if params.old_name == params.new_name {
            return Ok(format!(
                "No-op: old_name and new_name are identical ('{}').",
                params.old_name,
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
            return Err(format!("No symbol found with name '{}'", params.old_name));
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
        if candidates.len() > 1 && !kind_set && !file_hint_set {
            let locations: Vec<String> = candidates
                .iter()
                .map(|(s, f)| {
                    format!(
                        "  {} ({}) in {} [L{}-L{}]",
                        s.name, s.kind, f.path, s.line_start, s.line_end
                    )
                })
                .collect();
            let hint = if distinct_kinds.len() > 1 {
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
            result.into_iter().collect()
        } else {
            file_set.into_iter().collect()
        };

        // Detect collisions with `new_name` before any write. A rename that
        // silently collides with an existing symbol in a touched file is
        // indistinguishable in the output from a legitimate merge, and the
        // resulting source typically won't compile. Require opt-in via
        // `allow_collision=true`.
        let allow_collision = params.allow_collision.unwrap_or(false);
        if !allow_collision {
            let mut collisions: Vec<String> = Vec::new();
            for rel_path in &files_to_scan {
                if let Ok(Some(file_row)) = read::get_file_by_path(&conn, rel_path)
                    && let Ok(file_syms) = read::get_symbols_for_file(&conn, file_row.id)
                {
                    for s in file_syms {
                        if s.name == params.new_name {
                            collisions.push(format!(
                                "  {} ({}) in {} [L{}-L{}]",
                                s.name, s.kind, rel_path, s.line_start, s.line_end
                            ));
                        }
                    }
                }
            }
            if !collisions.is_empty() {
                return Err(format!(
                    "Refusing to rename '{}' -> '{}': new_name already defined in touched file(s). Pass `allow_collision=true` to proceed anyway.\n{}",
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
            let mut out = format!(
                "{} → {}: {} occ in {} file(s)\n",
                params.old_name,
                params.new_name,
                total_occurrences,
                files_touched.len(),
            );
            let mut current_file = String::new();
            for (file, line_num, _before, after) in &changes {
                if *file != current_file {
                    out.push_str(&format!("{file}\n"));
                    current_file = file.clone();
                }
                out.push_str(&format!("  L{}: {}\n", line_num, after.trim()));
            }
            Ok(out)
        }
    }
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
    use super::scan_line_for_rename;

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
}
