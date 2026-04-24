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

#[tool_router(router = qartez_stats_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_stats",
        description = "Codebase metrics at a glance: files, symbols, edges by language, most connected files, and index coverage percentage.",
        annotations(
            title = "Codebase Stats",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_stats(
        &self,
        Parameters(params): Parameters<SoulStatsParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;

        if let Some(ref target) = params.file_path {
            let file = read::get_file_by_path(&conn, target)
                .map_err(|e| format!("DB error: {e}"))?
                .ok_or_else(|| format!("File '{target}' not found in index"))?;
            let symbols =
                read::get_symbols_for_file(&conn, file.id).map_err(|e| format!("DB error: {e}"))?;
            let exported = symbols.iter().filter(|s| s.is_exported).count();
            let imports = read::get_edges_from(&conn, file.id)
                .unwrap_or_default()
                .len();
            let importers = read::get_edges_to(&conn, file.id).unwrap_or_default().len();
            // Blank line after the `# <path>` heading so markdown
            // renderers recognise the H1 section boundary. Without
            // the separator, the heading ran straight into the first
            // stat line and every markdown viewer rendered it as one
            // long inline sentence instead of a header plus body.
            return Ok(format!(
                "# {path}\n\nLOC: {loc} | Symbols: {syms} ({exp} exported) | Imports: {imp} | Importers: {importers}\nLanguage: {lang} | PageRank: {pr:.4}\n",
                path = file.path,
                loc = file.line_count,
                syms = symbols.len(),
                exp = exported,
                imp = imports,
                importers = importers,
                lang = file.language,
                pr = file.pagerank,
            ));
        }

        let file_count = read::get_file_count(&conn).map_err(|e| format!("DB error: {e}"))?;
        let symbol_count = read::get_symbol_count(&conn).map_err(|e| format!("DB error: {e}"))?;
        let edge_count = read::get_edge_count(&conn).map_err(|e| format!("DB error: {e}"))?;

        let all_files = read::get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;
        let (test_files, src_files): (Vec<_>, Vec<_>) =
            all_files.iter().partition(|f| is_test_path(&f.path));
        let test_loc: i64 = test_files.iter().map(|f| f.line_count).sum();
        let src_loc: i64 = src_files.iter().map(|f| f.line_count).sum();

        // Compact: single line per metric family, comma-separated values,
        // no padding. The LLM doesn't need ASCII alignment.
        let indexed_count = if file_count > 0 {
            conn.query_row("SELECT COUNT(DISTINCT file_id) FROM symbols", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0)
        } else {
            0
        };

        // `edges=N` is the number of distinct directed file-to-file import
        // edges. Per-file `importers` / `imports` counters are slices of
        // this same set grouped by the `to_file` / `from_file` column, so
        // summing the per-file importer counts over every file equals the
        // global edge total. The qualifier is spelled out so callers do
        // not confuse it with a pair count or a symbol-ref count.
        let mut out = format!(
            "files={} (src={}/test={}) loc={}/{} syms={} edges={} (distinct directed file imports) with_symbols={}/{}\n",
            file_count,
            src_files.len(),
            test_files.len(),
            src_loc,
            test_loc,
            symbol_count,
            edge_count,
            indexed_count,
            file_count,
        );

        // Drop zero-LOC / unnamed language buckets: lockfiles and empty
        // shell fragments leak in via the walker and contribute no signal.
        let lang_stats = read::get_language_stats(&conn).map_err(|e| format!("DB error: {e}"))?;
        let lang_parts: Vec<String> = lang_stats
            .iter()
            .filter(|s| !s.language.is_empty() && s.line_count > 0)
            .map(|s| {
                format!(
                    "{}={}f/{}L/{}/{}s",
                    s.language,
                    s.file_count,
                    s.line_count,
                    human_bytes(s.byte_count),
                    s.symbol_count,
                )
            })
            .collect();
        if !lang_parts.is_empty() {
            out.push_str(&format!("langs: {}\n", lang_parts.join(" ")));
        }

        // Top-5 most-imported files: enough to spot a hub, cheap in tokens.
        // Callers can pass a `file_path` for deep-dive stats on a specific
        // file (that branch early-returns above).
        let most_imported =
            read::get_most_imported_files(&conn, 5).map_err(|e| format!("DB error: {e}"))?;
        if !most_imported.is_empty() {
            let parts: Vec<String> = most_imported
                .iter()
                .map(|(file, n)| format!("{}×{}", file.path, n))
                .collect();
            out.push_str(&format!("top: {}\n", parts.join(" ")));
        }

        Ok(out)
    }
}
