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

#[tool_router(router = qartez_diff_impact_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_diff_impact",
        description = "Batch impact analysis for a git diff range. Pass a revspec like 'main..HEAD' to get a unified report: changed files with PageRank, union blast radius, convergence points (files affected by 2+ changes), and co-change omissions (historically coupled files missing from the diff). Pass risk=true to add per-file risk scoring (health, boundary violations, test coverage). Single call replaces N calls to qartez_impact + qartez_cochange.",
        annotations(
            title = "Diff Impact Analysis",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_diff_impact(
        &self,
        Parameters(params): Parameters<SoulDiffImpactParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_diff_impact")?;
        let concise = is_concise(&params.format);
        let include_tests = params.include_tests.unwrap_or(false);
        // Read-only by default. Guard-ACK side effects are opt-in via
        // `ack=true`; the previous behaviour wrote files under
        // `.qartez/acks/` on every read call, which surprised callers
        // doing static analysis and broke the tool's
        // `read_only_hint = true` contract.
        let ack_enabled = params.ack.unwrap_or(false);

        let changed = crate::git::diff::changed_files_in_range(&self.project_root, &params.base)
            .map_err(|e| friendly_git_error(&params.base, &e))?;

        if changed.is_empty() {
            // `base="HEAD"` (no `..`) expands to `HEAD..HEAD` inside
            // the git layer, which is always empty. Callers land here
            // expecting the working-tree diff against HEAD; point
            // them at the right tool instead of the "fresh worktree"
            // hint which does not apply.
            if !params.base.contains("..") && is_head_self_compare(&params.base) {
                return Ok(format!(
                    "No files changed in range '{}'. The tool compares two indexed trees; `base=HEAD` expands to `HEAD..HEAD` (empty by definition). For working-tree changes, run `qartez_impact` on each edited file, or pass `base=origin/main` / `base=HEAD~1` for committed changes.",
                    params.base,
                ));
            }
            // Worktree-vs-remote hint: `main..HEAD` is the canonical
            // range, but in a fresh worktree `main` typically points at
            // the same commit as `HEAD`, so the range resolves to zero
            // deltas and the user gets a silent empty report. The hint
            // steers them toward `origin/main..HEAD` instead.
            let hint = diff_impact_worktree_hint(&self.project_root, &params.base);
            return Ok(match hint {
                Some(h) => format!("No files changed in range '{}'.\n{h}", params.base),
                None => format!("No files changed in range '{}'.", params.base),
            });
        }

        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let changed_set: HashSet<&str> = changed.iter().map(|s| s.as_str()).collect();

        // Idempotency marker for `ack=true`: before writing any
        // per-file ack rows, check whether a deterministic marker for
        // this exact diff already exists. The marker name is derived
        // from (base revspec, sorted changed-files list), so calling
        // `qartez_diff_impact ack=true` twice on the same diff hits
        // the same marker file and skips touching the per-file ack
        // rows on the second call. Without this, consumers that
        // periodically re-run diff_impact on a stable PR branch saw
        // the ack directory appear to "grow" because every call
        // advanced mtimes on every changed file.
        let ack_marker: Option<std::path::PathBuf> = if ack_enabled {
            Some(diff_ack_marker_path(
                &self.project_root,
                &params.base,
                &changed,
            ))
        } else {
            None
        };
        let ack_marker_fresh: bool = ack_marker.as_ref().map(|p| p.exists()).unwrap_or(false);

        let mut indexed = Vec::new();
        let mut not_indexed = Vec::new();
        for path in &changed {
            match read::get_file_by_path(&conn, path) {
                Ok(Some(file)) => {
                    if ack_enabled && !ack_marker_fresh {
                        guard::touch_ack(&self.project_root, &file.path);
                    }
                    indexed.push(file);
                }
                _ => not_indexed.push(path.as_str()),
            }
        }
        if ack_enabled
            && !ack_marker_fresh
            && let Some(marker) = ack_marker.as_ref()
        {
            if let Some(parent) = marker.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(marker, "");
        }

        let file_ids: Vec<i64> = indexed.iter().map(|f| f.id).collect();
        let blast_results = blast::blast_radius_for_file_set(&conn, &file_ids)
            .map_err(|e| format!("Blast radius error: {e}"))?;

        let changed_ids: HashSet<i64> = file_ids.iter().copied().collect();

        // Union of direct importers: importer_id -> source file paths that cause it.
        let mut direct_union: HashMap<i64, Vec<String>> = HashMap::new();
        let mut transitive_union: HashSet<i64> = HashSet::new();

        for (file, br) in indexed.iter().zip(blast_results.iter()) {
            for &imp_id in &br.direct_importers {
                if !changed_ids.contains(&imp_id) {
                    direct_union
                        .entry(imp_id)
                        .or_default()
                        .push(file.path.clone());
                }
            }
            for &tid in &br.transitive_importers {
                if !changed_ids.contains(&tid) {
                    transitive_union.insert(tid);
                }
            }
        }

        let resolve_path = |id: i64| -> Option<String> {
            read::get_file_by_id(&conn, id)
                .ok()
                .flatten()
                .map(|f| f.path)
                .filter(|p| include_tests || !is_test_path(p))
        };

        let mut direct_entries: Vec<(String, Vec<String>)> = direct_union
            .iter()
            .filter_map(|(&id, sources)| resolve_path(id).map(|path| (path, sources.clone())))
            .collect();
        direct_entries.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));

        let transitive_count = transitive_union
            .iter()
            .filter_map(|&id| resolve_path(id))
            .count();

        let convergence: Vec<&(String, Vec<String>)> = direct_entries
            .iter()
            .filter(|(_, sources)| sources.len() >= 2)
            .collect();

        // Co-change omissions: partners not in the diff set. Walk the
        // `indexed` slice deterministically (sorted by path) so the
        // partner list shape does not depend on HashMap iteration order;
        // without this, `Cargo.toml` partner counts drifted (4 -> 12 ->
        // 12) across consecutive calls against the same SHA.
        let mut indexed_sorted: Vec<&crate::storage::models::FileRow> = indexed.iter().collect();
        indexed_sorted.sort_by(|a, b| a.path.cmp(&b.path));
        let mut omissions_map: HashMap<String, Vec<(String, u32)>> = HashMap::new();
        for file in &indexed_sorted {
            let cochanges = read::get_cochanges(&conn, file.id, 10).unwrap_or_default();
            for (cc, partner) in cochanges {
                if !changed_set.contains(partner.path.as_str())
                    && (include_tests || !is_test_path(&partner.path))
                {
                    omissions_map
                        .entry(partner.path)
                        .or_default()
                        .push((file.path.clone(), cc.count as u32));
                }
            }
        }
        // Deterministic inner ordering too: per-partner pairs are sorted
        // by source-file path so the rendered report is identical for the
        // same git SHA + index.
        for pairs in omissions_map.values_mut() {
            pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        }
        let mut omissions: Vec<(String, Vec<(String, u32)>)> = omissions_map.into_iter().collect();
        omissions.sort_by(|a, b| {
            let max_a = a.1.iter().map(|(_, c)| c).max().unwrap_or(&0);
            let max_b = b.1.iter().map(|(_, c)| c).max().unwrap_or(&0);
            max_b.cmp(max_a).then_with(|| a.0.cmp(&b.0))
        });

        let risk_data: Option<Vec<(f64, f64, usize, bool)>> = if params.risk.unwrap_or(false) {
            Some(compute_risk_data(
                &conn,
                &self.project_root,
                &indexed,
                &changed_set,
            ))
        } else {
            None
        };

        if concise {
            return Ok(format_diff_concise(
                &params.base,
                &changed,
                &direct_entries,
                &convergence,
                &omissions,
                risk_data.as_deref(),
            ));
        }

        let mut out = format!(
            "# Diff impact: {} ({} files changed)\n\n",
            params.base,
            changed.len(),
        );

        out.push_str("## Changed files\n");
        if risk_data.is_some() {
            // WHY: Risk and Health share a 0-10 scale but invert each
            // other's polarity (high Risk is bad, high Health is good).
            // Without this legend, callers conflated the two and read
            // a risk=8.5 row as "healthy" because 8.5 is high.
            out.push_str("legend: Risk=higher-is-worse (0-10), Health=higher-is-better (0-10)\n");
            out.push_str(
                " # | File                                | PageRank | Blast | Risk | Health\n",
            );
            out.push_str(
                "---+-------------------------------------+----------+-------+------+-------\n",
            );
        } else {
            out.push_str(" # | File                                | PageRank | Blast\n");
            out.push_str("---+-------------------------------------+----------+------\n");
        }
        let mut row_idx = 0usize;
        for (i, file) in indexed.iter().enumerate() {
            row_idx += 1;
            let blast_count = blast_results[i].transitive_importers.len();
            if let Some(ref risks) = risk_data {
                let (health, risk, _, _) = risks[i];
                let blast_str = format!("->{blast_count}");
                out.push_str(&format!(
                    "{:>2} | {:<35} | {:>8.4} | {:<5} | {:>4.1} | {:>6.1}\n",
                    row_idx,
                    truncate_path(&file.path, 35),
                    file.pagerank,
                    blast_str,
                    risk,
                    health,
                ));
            } else {
                out.push_str(&format!(
                    "{:>2} | {:<35} | {:>8.4} | {}{}\n",
                    row_idx,
                    truncate_path(&file.path, 35),
                    file.pagerank,
                    "->",
                    blast_count,
                ));
            }
        }
        for path in &not_indexed {
            row_idx += 1;
            out.push_str(&format!(
                "{row_idx:>2} | {:<35} | {:>8} | not indexed\n",
                truncate_path(path, 35),
                "-",
            ));
        }

        out.push_str(&format!(
            "\n## Union blast radius: {} direct, {} transitive\n",
            direct_entries.len(),
            transitive_count,
        ));
        if direct_entries.is_empty() {
            out.push_str("No external importers affected.\n");
        } else {
            for (path, sources) in &direct_entries {
                let short_sources: Vec<&str> = sources
                    .iter()
                    .map(|s| s.rsplit('/').next().unwrap_or(s))
                    .collect();
                out.push_str(&format!(
                    "  - {} (from: {})\n",
                    path,
                    short_sources.join(", "),
                ));
            }
        }

        if !convergence.is_empty() {
            out.push_str(&format!(
                "\n## Convergence points ({} files affected by 2+ changes)\n",
                convergence.len(),
            ));
            for (path, sources) in &convergence {
                out.push_str(&format!("  - {} ({} sources)\n", path, sources.len()));
            }
        }

        if !omissions.is_empty() {
            out.push_str(&format!(
                "\n## Co-change omissions ({} files)\n",
                omissions.len(),
            ));
            out.push_str(
                "Files that historically change with the diff set but are NOT included:\n",
            );
            for (partner, pairs) in omissions.iter().take(15) {
                let detail: Vec<String> = pairs
                    .iter()
                    .map(|(src, count)| {
                        format!("{} x{count}", src.rsplit('/').next().unwrap_or(src))
                    })
                    .collect();
                out.push_str(&format!("  - {} ({})\n", partner, detail.join(", ")));
            }
        }

        if let Some(ref risks) = risk_data {
            format_risk_summary(&mut out, &indexed, risks);
        }

        if ack_enabled && !indexed.is_empty() {
            out.push_str(&format!(
                "\nGuard ACK written for {} indexed file(s).\n",
                indexed.len(),
            ));
        }

        Ok(out)
    }
}

/// When a diff range resolves to zero deltas AND the two endpoints point
/// at the same commit, emit a hint pointing the caller at the most
/// common remedy (`origin/<branch>..HEAD`). This catches the worktree
/// case where a freshly-checked-out branch tracks the same SHA as its
/// upstream and `main..HEAD` therefore resolves to no commits. Returns
/// `None` for ranges that legitimately produced no changes (same tree
/// content across a real commit range).
/// True when `base` is a single-rev spec equivalent to HEAD (the git
/// layer expands `HEAD` to `HEAD..HEAD`, which is always empty). Used
/// by `qartez_diff_impact` to distinguish the "you picked an empty-
/// by-design range" case from a genuinely clean branch.
fn is_head_self_compare(base: &str) -> bool {
    let b = base.trim();
    matches!(b, "HEAD" | "@" | "HEAD^0" | "HEAD~0")
}

/// Replace a raw libgit2 error with a caller-friendly summary. Before
/// this, passing an out-of-range revspec like `HEAD~999` leaked the
/// bare `"Git error: parent 0 does not exist; class=Invalid (3);
/// code=NotFound (-3)"` string straight through. Keep the underlying
/// message as a trailing "(git:…)" suffix so operators who grep for
/// the raw libgit2 codes still have them.
fn friendly_git_error(base: &str, err: &impl std::fmt::Display) -> String {
    let raw = err.to_string();
    let lower = raw.to_ascii_lowercase();
    // WHY: `changed_files_in_range` rejects descendant-to-ancestor
    // revspecs to prevent the silent "reversed range returns identical
    // output" bug. Surface that hint unchanged instead of wrapping it
    // in the generic "Git error for revspec..." envelope, which would
    // bury the suggested forward form the user needs to copy.
    if lower.contains("range reversed") {
        return raw;
    }
    if lower.contains("parent") && lower.contains("does not exist") {
        return format!(
            "Cannot resolve revspec '{base}': the history does not reach that many parents (e.g. `HEAD~999` on a shallow clone or newly-created branch). Pick a smaller offset, or use a branch name like `origin/main..HEAD`. (git: {raw})"
        );
    }
    if lower.contains("not found") || lower.contains("unable to parse") || lower.contains("invalid")
    {
        return format!(
            "Cannot resolve revspec '{base}' to a git range. Typical forms: `main..HEAD`, `origin/main..HEAD`, `HEAD~3`, or a commit SHA. (git: {raw})"
        );
    }
    format!("Git error for revspec '{base}': {raw}")
}

fn diff_impact_worktree_hint(project_root: &std::path::Path, base: &str) -> Option<String> {
    let repo = git2::Repository::discover(project_root).ok()?;
    let effective = if base.contains("..") {
        base.to_string()
    } else {
        format!("{base}..HEAD")
    };
    let parsed = repo.revparse(&effective).ok()?;
    let from = parsed.from()?.id();
    let to = parsed.to()?.id();
    if from != to {
        return None;
    }
    // Extract the lhs of the revspec (`main` from `main..HEAD`) so the
    // hint names the actual branch the caller passed in.
    let lhs = effective.split("..").next().unwrap_or(base);

    // Only emit the `origin/<lhs>` suggestion when there actually is an
    // `origin` remote AND `origin/<lhs>` exists AND resolves to a
    // commit different from the local `<lhs>`. Without these gates,
    // every `base=main` call on a fresh worktree where `main == HEAD`
    // printed "Did you mean origin/main?" even when `origin/main` was
    // absent or identical - misleading callers into chasing an
    // upstream divergence that did not exist.
    let has_origin = repo.find_remote("origin").is_ok();
    if !has_origin {
        return None;
    }
    let origin_ref = format!("refs/remotes/origin/{lhs}");
    let origin_oid = repo
        .find_reference(&origin_ref)
        .ok()
        .and_then(|r| r.target())?;
    if origin_oid == to {
        return None;
    }
    Some(format!(
        "Range resolved to no commits ({lhs}=HEAD={}). Did you mean to diff against origin/{lhs}?",
        &to.to_string()[..7.min(to.to_string().len())],
    ))
}

fn compute_risk_data(
    conn: &rusqlite::Connection,
    project_root: &std::path::Path,
    indexed: &[crate::storage::models::FileRow],
    changed_set: &HashSet<&str>,
) -> Vec<(f64, f64, usize, bool)> {
    use crate::graph::boundaries::{Violation, check_boundaries, load_config};

    let all_files = read::get_all_files(conn).unwrap_or_default();
    let all_edges = read::get_all_edges(conn).unwrap_or_default();
    let id_to_path: HashMap<i64, &str> =
        all_files.iter().map(|f| (f.id, f.path.as_str())).collect();

    let mut reverse: HashMap<i64, Vec<i64>> = HashMap::new();
    for &(from, to) in &all_edges {
        if from != to {
            reverse.entry(to).or_default().push(from);
        }
    }

    let boundary_path = project_root.join(".qartez/boundaries.toml");
    let violations: Vec<Violation> = if boundary_path.exists() {
        load_config(&boundary_path)
            .ok()
            .map(|cfg| {
                check_boundaries(&cfg, &all_files, &all_edges)
                    .into_iter()
                    .filter(|v| {
                        changed_set.contains(v.from_file.as_str())
                            || changed_set.contains(v.to_file.as_str())
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // Same health formula as hotspots and test_gaps.
    let health_of = |max_cc: f64, coupling: f64, churn: i64| -> f64 {
        let cc_h = 10.0 / (1.0 + max_cc / 10.0);
        let coupling_h = 10.0 / (1.0 + coupling * 50.0);
        let churn_h = 10.0 / (1.0 + churn as f64 / 8.0);
        (cc_h + coupling_h + churn_h) / 3.0
    };

    let mut risks = Vec::new();
    for file in indexed {
        let symbols = read::get_symbols_for_file(conn, file.id).unwrap_or_default();
        let max_cc = symbols
            .iter()
            .filter_map(|s| s.complexity)
            .max()
            .unwrap_or(0) as f64;
        let health = health_of(max_cc, file.pagerank, file.change_count);

        let bv_count = violations
            .iter()
            .filter(|v| v.from_file == file.path || v.to_file == file.path)
            .count();

        let has_test = if is_test_path(&file.path) {
            true
        } else {
            reverse.get(&file.id).is_some_and(|importers| {
                importers
                    .iter()
                    .any(|&imp_id| id_to_path.get(&imp_id).is_some_and(|p| is_test_path(p)))
            })
        };

        let risk =
            ((10.0 - health) + (bv_count.min(3) as f64 * 0.5) + if !has_test { 1.5 } else { 0.0 })
                .clamp(0.0, 10.0);

        risks.push((health, risk, bv_count, has_test));
    }
    risks
}

fn format_diff_concise(
    base: &str,
    changed: &[String],
    direct_entries: &[(String, Vec<String>)],
    convergence: &[&(String, Vec<String>)],
    omissions: &[(String, Vec<(String, u32)>)],
    risk_data: Option<&[(f64, f64, usize, bool)]>,
) -> String {
    let files_list = changed
        .iter()
        .map(|p| truncate_path(p, 40))
        .collect::<Vec<_>>()
        .join(", ");
    let omission_list: String = omissions
        .iter()
        .take(5)
        .map(|(p, pairs)| {
            let max_count = pairs.iter().map(|(_, c)| c).max().unwrap_or(&0);
            format!("{} (x{max_count})", truncate_path(p, 35))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let risk_tag = if let Some(risks) = risk_data {
        let avg = if risks.is_empty() {
            0.0
        } else {
            risks.iter().map(|(_, r, _, _)| r).sum::<f64>() / risks.len() as f64
        };
        format!(" | risk: {avg:.1}")
    } else {
        String::new()
    };
    format!(
        "Diff: {} | {} files | blast union: {} | convergence: {} | omissions: {}{}\nFiles: {}\nOmissions: {}",
        base,
        changed.len(),
        direct_entries.len(),
        convergence.len(),
        omissions.len(),
        risk_tag,
        files_list,
        if omissions.is_empty() {
            "none".to_string()
        } else {
            omission_list
        },
    )
}

fn format_risk_summary(
    out: &mut String,
    indexed: &[crate::storage::models::FileRow],
    risks: &[(f64, f64, usize, bool)],
) {
    // Both the numerator and denominator must exclude test files so a
    // diff that changes 19 tests and 20 production files does not
    // report "Untested files: 38 / 39" (previously every test file
    // appeared in the numerator because `has_test` returned `false`
    // and the denominator was the full changed set). `is_test_path`
    // matches the same `tests/`, `_test.rs`, `_tests.rs`,
    // `test_*.rs`, `/tests/` patterns the rest of the analyzer uses.
    let total_violations: usize = risks.iter().map(|(_, _, bv, _)| *bv).sum();
    let untested: usize = indexed
        .iter()
        .zip(risks.iter())
        .filter(|(f, (_, _, _, has_test))| !is_test_path(&f.path) && !has_test)
        .count();
    let non_test_count = indexed.iter().filter(|f| !is_test_path(&f.path)).count();
    let avg_risk: f64 = if risks.is_empty() {
        0.0
    } else {
        risks.iter().map(|(_, r, _, _)| r).sum::<f64>() / risks.len() as f64
    };
    // WHY: "Highest risk" is a prioritization pointer for follow-up
    // work. A complex test file can still dominate the ranking (low
    // health from high CC pulls risk up), which misleads callers into
    // thinking a test needs more tests. Prefer production files when
    // any exist; only fall back to the full set for tests-only diffs.
    let highest = risks
        .iter()
        .enumerate()
        .filter(|(i, _)| !is_test_path(&indexed[*i].path))
        .max_by(|a, b| {
            a.1.1
                .partial_cmp(&b.1.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .or_else(|| {
            risks.iter().enumerate().max_by(|a, b| {
                a.1.1
                    .partial_cmp(&b.1.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });

    out.push_str(&format!(
        "\n## Risk summary\nOverall risk: {avg_risk:.1} / 10\n",
    ));
    if total_violations > 0 {
        out.push_str(&format!(
            "Boundary violations: {total_violations} (in changed files)\n",
        ));
    }
    if non_test_count == 0 {
        // Every file in the diff is a test file. Emitting `0 / 0`
        // read as a successful "all production files have coverage"
        // when the truth is that there ARE no production files to
        // evaluate. Name the shape instead so callers understand the
        // denominator.
        out.push_str("Diff is tests-only; no production files to evaluate for test coverage.\n");
    } else {
        out.push_str(&format!("Untested files: {untested} / {non_test_count}\n",));
    }
    if let Some((idx, (health, risk, bv, has_test))) = highest {
        let mut reasons = Vec::new();
        if *health < 4.0 {
            reasons.push("low health");
        }
        if !has_test && !is_test_path(&indexed[idx].path) {
            reasons.push("no test coverage");
        }
        if *bv > 0 {
            reasons.push("boundary violations");
        }
        if reasons.is_empty() {
            // Every risk dimension came back clean. Emit `low coupling`
            // instead of the old `high coupling` default, which read
            // as a contradiction on tests-only diffs where blast
            // radius is zero and no production file participates in
            // the scoring. The label now reflects the actual
            // condition the formula measured (no elevated signal on
            // any axis).
            reasons.push("low coupling");
        }
        out.push_str(&format!(
            "Highest risk: {} ({:.1}) - {}\n",
            truncate_path(&indexed[idx].path, 40),
            risk,
            reasons.join(", "),
        ));
    }
}

/// Deterministic marker path for an `ack=true` call: derives from the
/// base revspec and the sorted list of changed files so two calls on
/// the same diff resolve to the same file. Calling `qartez_diff_impact
/// ack=true` twice is then a no-op on the filesystem because the
/// marker already exists and the caller skips touching the per-file
/// ack rows. The marker itself is empty - consumers only check its
/// existence.
fn diff_ack_marker_path(
    project_root: &std::path::Path,
    base: &str,
    changed: &[String],
) -> std::path::PathBuf {
    // FNV-1a 64 over (base\n<sorted rel-paths joined by \n>). Mirrors
    // the construction already used by `guard::ack_path` so the two
    // hashing schemes stay consistent.
    let mut sorted: Vec<&str> = changed.iter().map(String::as_str).collect();
    sorted.sort();
    let mut hash: u64 = 0xcbf29ce4_84222325;
    for byte in base.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash ^= b'\n' as u64;
    hash = hash.wrapping_mul(0x100000001b3);
    for p in &sorted {
        for byte in p.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= b'\n' as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    project_root
        .join(".qartez")
        .join("acks")
        .join("diff-markers")
        .join(format!("{hash:016x}"))
}
