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

#[tool_router(router = qartez_cochange_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_cochange",
        description = "Find files that historically change together (from git history). High co-change count means files are logically coupled — modifying one likely requires modifying the other.",
        annotations(
            title = "Co-change History",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_cochange(
        &self,
        Parameters(params): Parameters<SoulCochangeParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_cochange")?;
        let concise = is_concise(&params.format);
        // `limit=0` means "no cap" project-wide convention across qartez
        // query tools. `limit=None` keeps the historical default of 10.
        let limit = match params.limit {
            None => 10,
            Some(0) => usize::MAX,
            Some(n) => n as usize,
        };
        // `max_commit_size=0` is meaningless: `commit.files.len() <= 0`
        // matches nothing, so the tool would quietly fall through to
        // the default-fallback list instead of applying the filter the
        // caller asked for. Reject explicitly.
        if let Some(0) = params.max_commit_size {
            return Err(
                "max_commit_size must be >= 1 (0 matches no commits; pick a positive cap like 30 to exclude mega-commits).".into(),
            );
        }
        let max_commit_size = params.max_commit_size.unwrap_or(30) as usize;

        let file_indexed = {
            let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
            read::get_file_by_path(&conn, &params.file_path)
                .map_err(|e| format!("DB error: {e}"))?
                .is_some()
        };

        if !file_indexed {
            // All branches share the unified `File '<path>' not found in
            // index` prefix used by `qartez_stats` / `qartez_impact` /
            // `qartez_outline`. The trailing clause still distinguishes
            // the two failure modes callers hit in practice:
            //   (a) path does not exist on disk - typo or wrong working dir
            //   (b) path exists but was added after the last index run - the
            //       fix is to reindex, not to re-check the spelling.
            let resolved = self.project_root.join(&params.file_path);
            if !resolved.exists() {
                return Err(format!("File '{}' not found in index", params.file_path));
            }
            return Err(format!(
                "File '{}' not found in index (exists on disk, reindex the project)",
                params.file_path
            ));
        }

        let pairs = compute_cochange_pairs(
            &self.project_root,
            &params.file_path,
            max_commit_size,
            self.git_depth as usize,
            limit,
        );

        let pairs = match pairs {
            Some(p) if !p.is_empty() => p,
            _ => {
                // Fallback: pre-computed table from index time. Useful when git
                // is unavailable or has been modified since indexing.
                let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
                let file = read::get_file_by_path(&conn, &params.file_path)
                    .map_err(|e| format!("DB error: {e}"))?
                    .ok_or_else(|| format!("File '{}' not found in index", params.file_path))?;
                let cc = read::get_cochanges(&conn, file.id, limit as i64)
                    .map_err(|e| format!("DB error: {e}"))?;
                if cc.is_empty() {
                    // Split the ambiguous "no shared commits" message
                    // into two distinct cases. `file.change_count` is
                    // the commit count populated at index time by the
                    // git walk, so zero means the indexer never saw a
                    // commit touching this file (e.g. git was disabled
                    // at index time or history is truncated below the
                    // threshold). A non-zero count plus an empty
                    // co-change table means the file exists alone in
                    // every commit that touched it. Previously both
                    // cases collapsed to "no shared commits", forcing
                    // callers to guess whether to re-index or accept
                    // the file as a loner.
                    if file.change_count == 0 {
                        return Ok(format!(
                            "No git history indexed for '{}'. Re-index with git enabled to populate co-change data.",
                            params.file_path,
                        ));
                    }
                    return Ok(format!(
                        "No co-change partners for '{}'. It has {} commit(s) but none co-touched another indexed file.",
                        params.file_path, file.change_count,
                    ));
                }
                cc.into_iter()
                    .map(|(c, f)| (f.path, c.count as u32))
                    .collect()
            }
        };

        if concise {
            let rendered: Vec<String> = pairs.iter().map(|(p, c)| format!("{p} ({c})")).collect();
            return Ok(format!(
                "Co-changes for {} (max_commit_size={}): {}",
                params.file_path,
                max_commit_size,
                rendered.join(", ")
            ));
        }

        let mut out = format!(
            "# Co-changes for: {} (max_commit_size={})\n\n",
            params.file_path, max_commit_size,
        );
        out.push_str(" # | File                                | Count\n");
        out.push_str("---+-------------------------------------+------\n");
        for (i, (path, count)) in pairs.iter().enumerate() {
            out.push_str(&format!(
                "{:>2} | {:<35} | {}\n",
                i + 1,
                truncate_path(path, 35),
                count,
            ));
        }
        Ok(out)
    }
}
