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

#[tool_router(router = qartez_find_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_find",
        description = "Locate a symbol definition by exact name. Returns file path, line range, signature, and visibility for every match. Use kind filter to disambiguate (e.g., kind='struct').",
        annotations(
            title = "Find Symbol",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_find(
        &self,
        Parameters(params): Parameters<SoulFindParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_find")?;
        if params.name.trim().is_empty() {
            return Err("query must be non-empty".to_string());
        }
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let use_regex = params.regex.unwrap_or(false);
        let regex_limit = params.limit.unwrap_or(100) as usize;
        let kind_filter = params.kind.clone();
        let allowed_kinds: Option<Vec<String>> = kind_filter.as_deref().map(expand_kind_alias);
        let matches_kind = |k: &str| -> bool {
            allowed_kinds
                .as_ref()
                .is_none_or(|wanted| wanted.iter().any(|w| k.eq_ignore_ascii_case(w)))
        };
        let results: Vec<(
            crate::storage::models::SymbolRow,
            crate::storage::models::FileRow,
        )> = if use_regex {
            // User regex: cap compiled-program size so pathological patterns
            // cannot exhaust memory. Mirrors the cap in graph/security.rs.
            let re = regex::RegexBuilder::new(&params.name)
                .size_limit(1 << 20)
                .build()
                .map_err(|e| format!("regex error: {e}"))?;
            // Walk every indexed symbol once and keep regex hits. Scales
            // linearly with corpus size. The limit parameter caps the result
            // set so callers do not accidentally pull back thousands of hits.
            let all_paths: std::collections::HashMap<String, crate::storage::models::FileRow> =
                read::get_all_files(&conn)
                    .map_err(|e| format!("DB error: {e}"))?
                    .into_iter()
                    .map(|f| (f.path.clone(), f))
                    .collect();
            let all =
                read::get_all_symbols_with_path(&conn).map_err(|e| format!("DB error: {e}"))?;
            if all.len() > 100_000 {
                tracing::warn!(
                    "regex scan over {} symbols; consider exact-name lookup for large indexes",
                    all.len()
                );
            }
            all.into_iter()
                .filter(|(s, _)| re.is_match(&s.name))
                .filter(|(s, _)| matches_kind(&s.kind))
                .filter_map(|(s, p)| all_paths.get(&p).cloned().map(|f| (s, f)))
                .take(regex_limit)
                .collect()
        } else {
            read::find_symbol_by_name(&conn, &params.name).map_err(|e| format!("DB error: {e}"))?
        };

        if results.is_empty() {
            return Ok(format!("No symbol found with name '{}'", params.name));
        }

        let filtered: Vec<_> = if use_regex {
            // Regex branch already filtered by kind during streaming.
            results
        } else if params.kind.is_some() {
            results
                .into_iter()
                .filter(|(sym, _)| matches_kind(&sym.kind))
                .collect()
        } else {
            results
        };

        if filtered.is_empty() {
            return Ok(format!(
                "No symbol '{}' matching kind '{}'",
                params.name,
                params.kind.unwrap_or_default()
            ));
        }

        // Only look up blast radius for files that actually matched; the
        // full `compute_blast_radius` sweep is O(V*(V+E)) and wasteful when
        // the result set is small.
        let match_file_ids: Vec<i64> = filtered.iter().map(|(_, f)| f.id).collect();
        let blast_radii = blast::blast_radius_for_files(&conn, &match_file_ids).unwrap_or_default();

        let concise = is_concise(&params.format);
        let mut out = format!(
            "Found {} match(es) for '{}':\n\n",
            filtered.len(),
            params.name
        );
        for (sym, file) in &filtered {
            let blast_r = blast_radii.get(&file.id).copied().unwrap_or(0);
            if concise {
                let marker = if sym.is_exported { "+" } else { " " };
                out.push_str(&format!(
                    " {marker} {} — {} [L{}-L{}] →{}\n",
                    sym.name, file.path, sym.line_start, sym.line_end, blast_r,
                ));
            } else {
                let exported = if sym.is_exported {
                    "exported"
                } else {
                    "private"
                };
                let sig = sym.signature.as_deref().unwrap_or("-");
                out.push_str(&format!(
                    "  {} ({})\n  File: {} [L{}-L{}] →{}\n  Signature: {}\n  Status: {}\n\n",
                    sym.name,
                    sym.kind,
                    file.path,
                    sym.line_start,
                    sym.line_end,
                    blast_r,
                    sig,
                    exported,
                ));
            }
        }
        Ok(out)
    }
}

/// Expand a caller-supplied kind keyword into the set of indexed kinds
/// that should match. Callers routinely type the source-language keyword
/// (`fn`, `class`, `trait`, `var`) while the indexer stores the emitted
/// kind (`function`, `method`, `struct`, `interface`, `variable`, ...).
/// This table closes that gap so `kind='fn'` on a method name still
/// finds the symbol.
pub(super) fn expand_kind_alias(kind: &str) -> Vec<String> {
    let k = kind.trim().to_ascii_lowercase();
    let set: &[&str] = match k.as_str() {
        "fn" | "function" | "func" => &["function", "method"],
        "method" => &["method"],
        "class" => &["class", "struct"],
        "struct" => &["struct", "class"],
        "trait" | "interface" => &["trait", "interface"],
        "var" | "variable" => &["variable", "const", "let"],
        "const" | "constant" => &["const", "constant"],
        _ => return vec![k],
    };
    set.iter().map(|s| (*s).to_string()).collect()
}
