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

#[tool_router(router = qartez_knowledge_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_knowledge",
        description = "Git-blame-based authorship analysis: find single-author files, knowledge silos, and bus factor per module. Bus factor = minimum authors who own >50% of lines. Use level='file' for per-file breakdown or level='module' for per-directory summary. Useful before modifying code with concentrated ownership.",
        annotations(
            title = "Knowledge / Bus Factor",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_knowledge(
        &self,
        Parameters(params): Parameters<SoulKnowledgeParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_knowledge")?;
        if self.git_depth == 0 {
            return Err(
                "Knowledge analysis requires git history. Re-index with --git-depth > 0.".into(),
            );
        }

        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        // `limit=0` means "no cap" project-wide convention. `limit=None`
        // keeps the historical default of 20.
        let limit = match params.limit {
            None => 20,
            Some(0) => usize::MAX,
            Some(n) => n as usize,
        };
        let concise = matches!(params.format, Some(Format::Concise));
        let level = params.level.unwrap_or(KnowledgeLevel::File);

        let all_files = read::get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;

        let file_paths: Vec<String> = if let Some(ref prefix) = params.file_path {
            // Normalize user input so Windows callers can pass either
            // separator style and still match forward-slash DB keys.
            let normalized = crate::index::to_forward_slash(prefix.clone());
            all_files
                .iter()
                .filter(|f| f.path.starts_with(normalized.as_str()))
                .map(|f| f.path.clone())
                .collect()
        } else {
            all_files.iter().map(|f| f.path.clone()).collect()
        };

        if file_paths.is_empty() {
            return Ok(format!(
                "No indexed files match '{}'.",
                params.file_path.as_deref().unwrap_or("*"),
            ));
        }

        drop(conn);

        // Run blame without the author filter up front. Applying the filter
        // after the sweep lets us detect "no matches" and emit the real
        // roster instead of the old misleading "no blame data" message.
        let mut full_authorships =
            crate::git::knowledge::analyze_knowledge(&self.project_root, &file_paths, None)
                .map_err(|e| format!("knowledge analysis failed: {e}"))?;

        if full_authorships.is_empty() {
            return Ok("No blame data available. Ensure the repository has commit history.".into());
        }

        let file_authorships = if let Some(author_query) = params.author.as_deref() {
            let filter_lower = author_query.to_lowercase();
            let any_match = full_authorships.iter().any(|f| {
                f.authors
                    .iter()
                    .any(|(name, _)| name.to_lowercase().contains(&filter_lower))
            });
            if !any_match {
                let roster = top_authors(&full_authorships, 5);
                return Ok(format!(
                    "No files touched by author matching '{author_query}'. Available authors: {roster}.",
                ));
            }
            full_authorships.retain(|f| {
                f.authors
                    .iter()
                    .any(|(name, _)| name.to_lowercase().contains(&filter_lower))
            });
            full_authorships
        } else {
            full_authorships
        };

        match level {
            KnowledgeLevel::File => {
                let mut files = file_authorships;
                // Sort: lowest bus factor first (riskiest), then largest file.
                files.sort_by(|a, b| {
                    a.bus_factor
                        .cmp(&b.bus_factor)
                        .then(b.total_lines.cmp(&a.total_lines))
                });
                if limit != usize::MAX {
                    files.truncate(limit);
                }

                if concise {
                    let mut out = String::from("# bus_factor lines authors file\n");
                    for (i, f) in files.iter().enumerate() {
                        let author_list: Vec<&str> =
                            f.authors.iter().map(|(n, _)| n.as_str()).collect();
                        out.push_str(&format!(
                            "{} {} {} {} {}\n",
                            i + 1,
                            f.bus_factor,
                            f.total_lines,
                            author_list.join(";"),
                            f.path,
                        ));
                    }
                    Ok(out)
                } else {
                    let total_analyzed = file_paths.len();
                    let single_author_count = files.iter().filter(|f| f.bus_factor == 1).count();
                    let mut out = format!(
                        "# Knowledge / Bus Factor (file level)\n\n\
                         Analyzed {} files. Showing top {} by risk (lowest bus factor first).\n\
                         Single-author files in view: {}\n\n",
                        total_analyzed,
                        files.len(),
                        single_author_count,
                    );
                    out.push_str(
                        "  # | BF | Lines | File                               | Top Authors\n",
                    );
                    out.push_str(
                        "----+----+-------+------------------------------------+------------\n",
                    );
                    for (i, f) in files.iter().enumerate() {
                        let top: Vec<String> = f
                            .authors
                            .iter()
                            .take(3)
                            .map(|(name, lines)| {
                                let pct = if f.total_lines > 0 {
                                    *lines as f64 / f.total_lines as f64 * 100.0
                                } else {
                                    0.0
                                };
                                format!("{name} ({pct:.0}%)")
                            })
                            .collect();
                        out.push_str(&format!(
                            "{:>3} | {:>2} | {:>5} | {:<34} | {}\n",
                            i + 1,
                            f.bus_factor,
                            f.total_lines,
                            truncate_path(&f.path, 34),
                            top.join(", "),
                        ));
                    }
                    Ok(out)
                }
            }
            KnowledgeLevel::Module => {
                let mut modules = crate::git::knowledge::rollup_modules(&file_authorships);
                if limit != usize::MAX {
                    modules.truncate(limit);
                }

                if modules.is_empty() {
                    return Ok("No module data available.".into());
                }

                if concise {
                    let mut out =
                        String::from("# bus_factor files single_author_files lines module\n");
                    for (i, m) in modules.iter().enumerate() {
                        out.push_str(&format!(
                            "{} {} {} {} {} {}\n",
                            i + 1,
                            m.bus_factor,
                            m.file_count,
                            m.single_author_files,
                            m.total_lines,
                            m.module,
                        ));
                    }
                    Ok(out)
                } else {
                    let mut out = String::from(
                        "# Knowledge / Bus Factor (module level)\n\n\
                         Bus factor = minimum authors to cover >50% of lines. Lower = riskier.\n\n",
                    );
                    out.push_str("  # | BF | Files | Solo | Lines | Module                          | Top Authors\n");
                    out.push_str("----+----+-------+------+-------+---------------------------------+------------\n");
                    for (i, m) in modules.iter().enumerate() {
                        let top: Vec<String> = m
                            .top_authors
                            .iter()
                            .take(3)
                            .map(|(name, lines)| {
                                let pct = if m.total_lines > 0 {
                                    *lines as f64 / m.total_lines as f64 * 100.0
                                } else {
                                    0.0
                                };
                                format!("{name} ({pct:.0}%)")
                            })
                            .collect();
                        out.push_str(&format!(
                            "{:>3} | {:>2} | {:>5} | {:>4} | {:>5} | {:<31} | {}\n",
                            i + 1,
                            m.bus_factor,
                            m.file_count,
                            m.single_author_files,
                            m.total_lines,
                            truncate_path(&m.module, 31),
                            top.join(", "),
                        ));
                    }
                    Ok(out)
                }
            }
        }
    }
}

/// Sum per-author line counts across every file in `authorships` and
/// return the top `n` names as a comma-separated roster. Used to salvage
/// a useful error message when the caller's `author=` filter matches no
/// one - surfacing the real roster lets them correct a typo without
/// re-running blame manually.
fn top_authors(authorships: &[crate::git::knowledge::FileAuthorship], n: usize) -> String {
    let mut totals: HashMap<String, u32> = HashMap::new();
    for f in authorships {
        for (name, lines) in &f.authors {
            *totals.entry(name.clone()).or_insert(0) += *lines;
        }
    }
    let mut ranked: Vec<(String, u32)> = totals.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    ranked.truncate(n);
    if ranked.is_empty() {
        return "(none)".to_string();
    }
    ranked
        .into_iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>()
        .join(", ")
}
