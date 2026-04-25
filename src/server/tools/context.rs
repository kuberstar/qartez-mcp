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
use crate::test_paths::is_testable_source_path;
use crate::toolchain;

#[tool_router(router = qartez_context_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_context",
        description = "Smart context builder: given files you plan to modify, returns the optimal set of related files to read first. Combines dependency graph, co-change history, and PageRank to prioritize what matters. Pass `include_impact=true` to append a one-line blast-radius summary per input file (direct importers, transitive count, cochange partner count) and `include_test_gaps=true` to append a one-line test-coverage status per input file - so a single call covers the prepare-change checklist without a follow-up qartez_impact / qartez_test_gaps round-trip.",
        annotations(
            title = "Smart Context",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_context(
        &self,
        Parameters(params): Parameters<SoulContextParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_context")?;
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let budget = params.token_budget.unwrap_or(4000) as usize;
        let concise = is_concise(&params.format);
        let explain = params.explain.unwrap_or(false);
        let include_impact = params.include_impact.unwrap_or(false);
        let include_test_gaps = params.include_test_gaps.unwrap_or(false);
        // `limit=0` means "no cap" - uniform across qartez query tools.
        let limit = match params.limit {
            None => 15,
            Some(0) => usize::MAX,
            Some(n) => n as usize,
        };

        // Task-only seed mode. When `files` is empty but `task` is set,
        // derive the initial file set via an FTS search of symbol names
        // against task terms longer than three characters. This mirrors
        // the wording in the tool description that `task` helps
        // prioritise context, and lets exploration sessions bootstrap
        // from a natural-language prompt without first running
        // `qartez_grep` by hand.
        let mut files_list: Vec<String> = params.files.clone();
        let seeded_from_task: Option<Vec<String>> = if files_list.is_empty() {
            match params.task.as_deref() {
                Some(task) if !task.trim().is_empty() => {
                    let mut seeds: Vec<String> = Vec::new();
                    let mut seen: HashSet<String> = HashSet::new();
                    for word in task.split_whitespace().filter(|w| w.len() > 3) {
                        let fts = if word.contains('*') {
                            word.to_string()
                        } else {
                            format!("{word}*")
                        };
                        if let Ok(results) = read::search_symbols_fts(&conn, &fts, 10) {
                            for (_, file_path) in results {
                                // Reject non-code seed matches. The
                                // FTS index covers every indexed
                                // file's symbol names, so a task
                                // like "pagerank scoring" was
                                // matching `.github/workflows/
                                // scorecard.yml` because
                                // `tree-sitter-yaml` exposes YAML
                                // keys as symbols. Workflow files,
                                // Cargo.toml, README, etc. are never
                                // useful context seeds - filter them
                                // with the shared source-path
                                // classifier used by test_gaps and
                                // diff_impact.
                                if !is_testable_source_path(&file_path) {
                                    continue;
                                }
                                if seen.insert(file_path.clone()) {
                                    seeds.push(file_path);
                                }
                            }
                        }
                    }
                    if seeds.is_empty() {
                        return Err(
                            "`task` seeded 0 files (no symbols matched the task terms) and `files` is empty. Pass at least one file path in `files`, or refine `task`."
                                .to_string(),
                        );
                    }
                    // Cap the seed set so scoring stays cheap; FTS ordering
                    // means the truncated tail is already the least relevant.
                    const MAX_TASK_SEEDS: usize = 5;
                    seeds.truncate(MAX_TASK_SEEDS);
                    files_list = seeds.clone();
                    Some(seeds)
                }
                _ => {
                    return Err(
                        "Provide at least one file path in 'files' parameter, or pass a non-empty `task` to seed the search from symbol names."
                            .to_string(),
                    );
                }
            }
        } else {
            None
        };

        // Verify every input path exists in the index before any scoring.
        // An unindexed path used to fall through to "No related context
        // files found. The specified files may be isolated." which reads
        // like a legitimate but empty answer; callers could not tell the
        // file was simply missing.
        let mut missing: Vec<&String> = Vec::new();
        for file_path in &files_list {
            if read::get_file_by_path(&conn, file_path)
                .map_err(|e| format!("DB error: {e}"))?
                .is_none()
            {
                missing.push(file_path);
            }
        }
        if !missing.is_empty() {
            // Singleton errors mirror the `File '<path>' not found in
            // index` format used across `qartez_stats` / `qartez_impact` /
            // `qartez_outline` / `qartez_cochange`. Multi-file errors keep
            // a compact list-form that still begins with "Files '<a>',
            // '<b>' not found in index" so callers can grep consistently.
            if missing.len() == 1 {
                return Err(format!("File '{}' not found in index", missing[0]));
            }
            return Err(format!(
                "Files {} not found in index",
                missing
                    .iter()
                    .map(|s| format!("'{s}'"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }

        // Per-reason breakdown. Keyed by path, each entry tracks the
        // contribution of every signal so `explain=true` can surface the
        // decomposition instead of only the final score.
        let mut scored: HashMap<String, ScoreBreakdown> = HashMap::new();
        let mut input_file_ids: Vec<i64> = Vec::new();
        // Track the languages represented in the seed set. Used downstream
        // to keep the `task_match` FTS scoring from crediting cross-language
        // hits (a Rust-focused seed should not surface JS plugins or CSS
        // files just because their FTS index has the same prefix).
        let mut seed_languages: HashSet<String> = HashSet::new();

        for file_path in &files_list {
            let file = match read::get_file_by_path(&conn, file_path)
                .map_err(|e| format!("DB error: {e}"))?
            {
                Some(f) => f,
                None => continue,
            };
            if !file.language.trim().is_empty() && file.language != "unknown" {
                seed_languages.insert(file.language.clone());
            }
            input_file_ids.push(file.id);

            let outgoing = read::get_edges_from(&conn, file.id).unwrap_or_default();
            for edge in &outgoing {
                if let Ok(Some(dep)) = read::get_file_by_id(&conn, edge.to_file)
                    && !files_list.contains(&dep.path)
                {
                    scored.entry(dep.path.clone()).or_default().imports +=
                        3.0 + dep.pagerank * 10.0;
                }
            }

            let incoming = read::get_edges_to(&conn, file.id).unwrap_or_default();
            for edge in &incoming {
                if let Ok(Some(imp)) = read::get_file_by_id(&conn, edge.from_file)
                    && !files_list.contains(&imp.path)
                {
                    scored.entry(imp.path.clone()).or_default().importer +=
                        2.0 + imp.pagerank * 5.0;
                }
            }

            let cochanges = read::get_cochanges(&conn, file.id, 10).unwrap_or_default();
            for (cc, partner) in &cochanges {
                if !files_list.contains(&partner.path) {
                    scored.entry(partner.path.clone()).or_default().cochange +=
                        cc.count as f64 * 1.5;
                }
            }

            let blast = blast::blast_radius_for_file(&conn, file.id).unwrap_or_else(|_| {
                blast::BlastResult {
                    file_id: file.id,
                    direct_importers: Vec::new(),
                    transitive_importers: Vec::new(),
                    transitive_count: 0,
                }
            });
            for &imp_id in &blast.transitive_importers {
                if input_file_ids.contains(&imp_id) {
                    continue;
                }
                if let Ok(Some(f)) = read::get_file_by_id(&conn, imp_id)
                    && !files_list.contains(&f.path)
                {
                    scored.entry(f.path.clone()).or_default().transitive += 0.5;
                }
            }
        }

        if let Some(ref task) = params.task {
            let words: Vec<&str> = task.split_whitespace().filter(|w| w.len() > 3).collect();
            for word in &words {
                let fts = if word.contains('*') {
                    word.to_string()
                } else {
                    format!("{word}*")
                };
                if let Ok(results) = read::search_symbols_fts(&conn, &fts, 10) {
                    for (sym, file_path) in &results {
                        // Mirror the seed-from-task filter: reject
                        // non-code files (CSS, lockfiles, JSON,
                        // workflow YAML) when crediting `task_match`
                        // signal. Without this, a task like
                        // "parse and analyze the rust source code"
                        // bumped opencode-plugin.ts and style.css
                        // into the seed list because the FTS index
                        // covers every indexed file and the score
                        // ladder weighted that match equally with a
                        // real Rust hit.
                        if !is_testable_source_path(file_path) {
                            continue;
                        }
                        // Cross-language guard. When the seed set has a
                        // clear language signal, skip FTS hits in other
                        // languages so a Rust-only seed does not pull in
                        // JS plugins or Python helpers via shared symbol
                        // prefixes. An empty `seed_languages` (e.g. the
                        // task-only seed mode hit a directory whose
                        // language detection collapsed to "unknown")
                        // disables the filter so the legacy behavior
                        // still applies.
                        if !seed_languages.is_empty()
                            && let Ok(Some(candidate)) = read::get_file_by_path(&conn, file_path)
                            && !seed_languages.contains(&candidate.language)
                        {
                            continue;
                        }
                        if !files_list.contains(file_path) {
                            scored.entry(file_path.clone()).or_default().task_match += 1.0;
                        }
                        let _ = sym;
                    }
                }
            }
        }

        let mut ranked: Vec<(String, ScoreBreakdown)> = scored.into_iter().collect();
        ranked.sort_by(|a, b| {
            b.1.total()
                .partial_cmp(&a.1.total())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let total_candidates = ranked.len();
        let dropped_by_limit = total_candidates.saturating_sub(limit);
        ranked.truncate(limit);

        // Empty-ranked early-return preserves the legacy "no related
        // context" wording when no compound flags are set so existing
        // callers see the same response shape. With at least one flag
        // on, the caller is asking for the prepare-change checklist
        // (impact / test-gaps); we keep going so those sections still
        // render even for isolated files.
        if ranked.is_empty() && !include_impact && !include_test_gaps {
            return Ok(
                "No related context files found. The specified files may be isolated.".to_string(),
            );
        }

        let header_subject = if let Some(ref seeds) = seeded_from_task {
            format!("task seed ({})", seeds.join(", "))
        } else {
            files_list.join(", ")
        };
        let mut out = if ranked.is_empty() {
            format!(
                "# Context for: {header_subject}\nNo related files via the dependency / co-change graph; compound annotations follow.\n",
            )
        } else {
            format!(
                "# Context for: {header_subject}\n{} related file(s) found:\n",
                ranked.len(),
            )
        };
        // When `task` is set and `explain=false`, surface the seed
        // count so callers know how the context list was derived
        // without flipping on full explain mode. `explain=true`
        // already decomposes every score, so the extra line would
        // only duplicate noise there.
        if params.task.is_some() && !explain {
            let seed_count = seeded_from_task
                .as_ref()
                .map(|s| s.len())
                .unwrap_or(files_list.len());
            out.push_str(&format!(
                "// seeded from {seed_count} task-matching files\n"
            ));
        }
        out.push('\n');

        let mut dropped_by_budget: usize = 0;
        for (i, (path, breakdown)) in ranked.iter().enumerate() {
            let line = if concise {
                format!("  {} {}\n", i + 1, path)
            } else if explain {
                format!(
                    "{:>2}. {} (score: {:.1}) — {}\n",
                    i + 1,
                    path,
                    breakdown.total(),
                    breakdown.explain(),
                )
            } else {
                format!(
                    "{:>2}. {} (score: {:.1}) — {}\n",
                    i + 1,
                    path,
                    breakdown.total(),
                    breakdown.reasons().join(", "),
                )
            };
            if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                dropped_by_budget = ranked.len() - i;
                out.push_str("  ... (truncated by token budget)\n");
                break;
            }
            out.push_str(&line);
        }

        // Optional compound annotations - blast-radius and test-coverage
        // summaries appended per input file so a single qartez_context
        // call can answer the prepare-change checklist without a follow
        // up qartez_impact / qartez_test_gaps round-trip.
        // Both sections are token-budget aware: each line is checked
        // against the same `budget` ceiling used for the ranked listing
        // so the response stays within the caller's contract.
        if include_impact {
            let header = "\n## Impact (per input file)\n";
            if estimate_tokens(&out) + estimate_tokens(header) <= budget {
                out.push_str(header);
                for path in &files_list {
                    let file = match read::get_file_by_path(&conn, path) {
                        Ok(Some(f)) => f,
                        _ => continue,
                    };
                    let direct = read::get_edges_to(&conn, file.id)
                        .map(|v| v.len())
                        .unwrap_or(0);
                    let transitive = blast::blast_radius_for_file(&conn, file.id)
                        .map(|b| b.transitive_count)
                        .unwrap_or(0);
                    let cochange = read::get_cochanges(&conn, file.id, 5)
                        .map(|v| v.len())
                        .unwrap_or(0);
                    let line = format!(
                        "  {path} - direct={direct} transitive={transitive} cochange={cochange}\n",
                    );
                    if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                        out.push_str("  ... (truncated by token budget)\n");
                        break;
                    }
                    out.push_str(&line);
                }
            }
        }

        if include_test_gaps {
            let header = "\n## Test gaps (per input file)\n";
            if estimate_tokens(&out) + estimate_tokens(header) <= budget {
                out.push_str(header);
                // Delegate to the canonical coverage helper in
                // tools::test_gaps so the answer matches what
                // `qartez_test_gaps mode=gaps` would say. Walking
                // direct edges alone misses Rust crate-rooted imports
                // (`use <crate>::<module>`) and inline `#[cfg(test)]`
                // blocks - both real coverage signals that should not
                // be reported as gaps.
                for path in &files_list {
                    let cov =
                        super::test_gaps::coverage_for_source(&conn, &self.project_root, path);
                    let line = if !cov.is_covered() {
                        format!("  {path} - untested\n")
                    } else {
                        let mut tests: Vec<String> = Vec::new();
                        tests.extend(cov.direct_test_paths.iter().cloned());
                        for p in &cov.stem_mentioned_in_tests {
                            if !tests.contains(p) {
                                tests.push(p.clone());
                            }
                        }
                        let inline_tag = if cov.inline_rust_tests {
                            " + inline tests"
                        } else {
                            ""
                        };
                        if tests.is_empty() {
                            // Inline-only coverage: no external test
                            // file mentions the source, but the file
                            // declares its own #[cfg(test)] block.
                            format!("  {path} - covered (inline tests only)\n")
                        } else {
                            let count = tests.len();
                            let preview: Vec<String> = tests.iter().take(3).cloned().collect();
                            let extra = if count > 3 {
                                format!(" (+{} more)", count - 3)
                            } else {
                                String::new()
                            };
                            format!(
                                "  {path} - {count} test(s): {}{extra}{inline_tag}\n",
                                preview.join(", "),
                            )
                        }
                    };
                    if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                        out.push_str("  ... (truncated by token budget)\n");
                        break;
                    }
                    out.push_str(&line);
                }
            }
        }

        if explain && (dropped_by_limit > 0 || dropped_by_budget > 0) {
            out.push_str(&format!(
                "\nExcluded: {dropped_by_limit} by limit, {dropped_by_budget} by token budget (candidates={total_candidates}, limit={limit}, budget={budget})\n",
            ));
        }

        Ok(out)
    }
}
