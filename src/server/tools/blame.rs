use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use super::super::QartezServer;
use super::super::params::{BlameMode, SoulBlameParams};

use crate::git::blame;
use crate::index::to_forward_slash;
use crate::storage::read;

#[tool_router(router = qartez_blame_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_blame",
        description = "Git blame scoped to a single symbol: resolves a function/type/method name to its file and line range, then shows who last touched those lines. mode='hunk' (default) lists per-hunk commits and authors; mode='aggregate' rolls up by author with each author's latest commit. Pass file_path to disambiguate a name defined in several files. Requires git history (index with --git-depth > 0).",
        annotations(
            title = "Symbol Blame",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_blame(
        &self,
        Parameters(params): Parameters<SoulBlameParams>,
    ) -> Result<String, String> {
        if self.git_depth == 0 {
            return Err("Blame requires git history. Re-index with --git-depth > 0.".into());
        }

        let name = params.symbol_name.trim();
        if name.is_empty() {
            return Err("symbol_name must not be empty".into());
        }

        // Resolve the symbol to a file and line range from the index, then
        // release the DB lock before the (potentially slow) git blame I/O.
        let (rel_path, line_start, line_end) = {
            let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
            let mut matches =
                read::find_symbol_by_name(&conn, name).map_err(|e| format!("DB error: {e}"))?;

            if let Some(ref fp) = params.file_path {
                let want = to_forward_slash(fp.clone());
                matches.retain(|(_, file)| file.path == want);
            }

            if matches.is_empty() {
                return Err(format!("Symbol '{name}' not found in index"));
            }

            // Multiple definitions in different files: ask the caller to pick.
            let mut files: Vec<String> = matches.iter().map(|(_, f)| f.path.clone()).collect();
            files.sort();
            files.dedup();
            if files.len() > 1 {
                let listing = files
                    .iter()
                    .map(|f| format!("  - {f}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Err(format!(
                    "Symbol '{name}' is defined in multiple files; pass file_path to disambiguate:\n{listing}"
                ));
            }

            // Same file (one definition, or overloads sharing a name): blame
            // the union of their line ranges.
            let path = matches[0].1.path.clone();
            let start = matches
                .iter()
                .map(|(s, _)| s.line_start)
                .min()
                .unwrap_or(matches[0].0.line_start);
            let end = matches
                .iter()
                .map(|(s, _)| s.line_end)
                .max()
                .unwrap_or(matches[0].0.line_end);
            (path, start, end)
        };

        let hunks = blame::symbol_blame(&self.project_root, &rel_path, line_start, line_end)
            .map_err(|e| format!("blame error: {e}"))?;

        let mut out = format!("# blame `{name}` ({rel_path}:{line_start}-{line_end})\n\n");
        if hunks.is_empty() {
            out.push_str("No blame data: the file is untracked or has no git history.\n");
            return Ok(out);
        }

        match params.mode.unwrap_or_default() {
            BlameMode::Hunk => {
                for h in &hunks {
                    let last = h.line_start + h.line_count.saturating_sub(1);
                    let plural = if h.line_count == 1 { "" } else { "s" };
                    out.push_str(&format!(
                        "{} {} L{}-{} ({} line{plural})\n",
                        h.commit, h.author, h.line_start, last, h.line_count,
                    ));
                }
            }
            BlameMode::Aggregate => {
                let authors = blame::aggregate_by_author(&hunks);
                let total: u32 = authors.iter().map(|a| a.lines).sum();
                out.push_str("Author | Lines | Share | Latest commit\n");
                for a in &authors {
                    let pct = if total > 0 {
                        (f64::from(a.lines) / f64::from(total)) * 100.0
                    } else {
                        0.0
                    };
                    out.push_str(&format!(
                        "{} | {} | {pct:.0}% | {}\n",
                        a.author, a.lines, a.latest_commit,
                    ));
                }
            }
        }
        Ok(out)
    }
}
