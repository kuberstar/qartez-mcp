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
        // Trim leading/trailing whitespace up front. Previously
        // `name="   Foo   "` reached the SQL layer verbatim and
        // returned 0 hits even when `Foo` was indexed - a silent
        // footgun for callers that interpolate user input.
        let name_trimmed = params.name.trim();
        if name_trimmed.is_empty() {
            // Field is called `name` in the schema. The old wording
            // said "query must be non-empty" which didn't match the
            // param name and confused callers reading the error.
            return Err("`name` must be non-empty".to_string());
        }
        let name = name_trimmed.to_string();
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let use_regex = params.regex.unwrap_or(false);
        // `limit=0` with `regex=true` follows the qartez no-cap
        // convention (same as qartez_grep/qartez_unused): promote to
        // unbounded so callers who want every hit do not have to
        // guess a safe-but-large value. Annotation appended to the
        // output so it's clear nothing was truncated.
        let (regex_limit, no_cap) = match params.limit {
            Some(0) if use_regex => (usize::MAX, true),
            Some(n) => (n as usize, false),
            None => (100usize, false),
        };
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
            // Default to case-insensitive matching so `regex=true`
            // aligns with FTS grep behaviour; callers who want strict
            // casing can prepend `(?-i)` themselves. Skip the wrap
            // when the pattern already carries an explicit case flag
            // so we don't double-specify.
            let pattern = if name.contains("(?i)") || name.contains("(?-i)") {
                name.clone()
            } else {
                format!("(?i){name}")
            };
            // User regex: cap compiled-program size so pathological patterns
            // cannot exhaust memory. Mirrors the cap in graph/security.rs.
            let re = regex::RegexBuilder::new(&pattern)
                .size_limit(1 << 20)
                .build()
                .map_err(|e| {
                    format!(
                        "Invalid regex '{name}': {e}. Did you mean to set regex=false for literal matching?",
                    )
                })?;
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
            read::find_symbol_by_name(&conn, &name).map_err(|e| format!("DB error: {e}"))?
        };

        if results.is_empty() {
            // Exact-name misses are the single most common qartez_find
            // stumble: `Parser` returns 0 hits because every real match
            // is `RustParser`, `GoParser`, etc. Suggest the FTS
            // prefix-search fallback plus top-3 closest indexed names
            // by Levenshtein so callers see a usable path without
            // having to know `qartez_grep` exists.
            if use_regex {
                return Ok(format!("No symbol found with name '{name}'"));
            }
            let suggestions = suggest_similar_names(&conn, &name);
            let hint = format!(
                "\nTry `qartez_grep query={name}*` for prefix search, or pass `regex=true` for a pattern match.",
            );
            if suggestions.is_empty() {
                return Ok(format!("No symbol found with name '{name}'{hint}"));
            }
            return Ok(format!(
                "No symbol found with name '{name}'. Did you mean: {}?{hint}",
                suggestions.join(", "),
            ));
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
                "No symbol '{name}' matching kind '{}'",
                params.kind.unwrap_or_default()
            ));
        }

        // Only look up blast radius for files that actually matched; the
        // full `compute_blast_radius` sweep is O(V*(V+E)) and wasteful when
        // the result set is small.
        let match_file_ids: Vec<i64> = filtered.iter().map(|(_, f)| f.id).collect();
        let blast_radii = blast::blast_radius_for_files(&conn, &match_file_ids).unwrap_or_default();

        let concise = is_concise(&params.format);
        let mut header_notes = String::new();
        if use_regex && !name.contains("(?i)") && !name.contains("(?-i)") {
            header_notes.push_str(
                "// note: regex matching is case-insensitive by default. Prepend '(?-i)' to force case-sensitive.\n",
            );
        }
        let mut out = format!("Found {} match(es) for '{name}':\n\n", filtered.len());
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
        if !header_notes.is_empty() {
            out.insert_str(0, &header_notes);
        }
        if no_cap {
            // No-cap convention surfaces explicitly so callers know
            // their `limit=0` was honoured and the result set is
            // truly unbounded (no silent truncation happened).
            out.push_str(&format!("// returned {} (no-cap)\n", filtered.len(),));
        }
        Ok(out)
    }
}

/// Return up to 3 indexed symbol names with the smallest Levenshtein
/// distance to `needle`. Bounded at 5000 candidates so the O(N*L)
/// distance scan does not dominate request latency on huge indexes.
/// Returns empty when the miss has no close neighbour (distance > 3).
fn suggest_similar_names(conn: &rusqlite::Connection, needle: &str) -> Vec<String> {
    const CANDIDATE_CAP: usize = 5_000;
    const MAX_DISTANCE: usize = 3;
    let Ok(symbols) = read::get_all_symbols_with_path(conn) else {
        return Vec::new();
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut scored: Vec<(usize, String)> = Vec::new();
    for (sym, _) in symbols.into_iter().take(CANDIDATE_CAP) {
        if !seen.insert(sym.name.clone()) {
            continue;
        }
        let d = levenshtein(needle, &sym.name);
        if d <= MAX_DISTANCE {
            scored.push((d, sym.name));
        }
    }
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().take(3).map(|(_, n)| n).collect()
}

/// Classic iterative Levenshtein with a rolling two-row matrix. Case
/// folded once so `Parser` vs `parser` is a 0-distance match (callers
/// routinely forget indexed casing).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.to_lowercase().chars().collect();
    let b: Vec<char> = b.to_lowercase().chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
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
