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

#[tool_router(router = qartez_grep_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_grep",
        description = "Search indexed symbols by name, kind, or file path using FTS5. Prefix matching is anchored to the symbol name column (e.g. 'Config*' matches symbols whose name starts with 'Config', not file paths that happen to contain it). Use the `kind` param to filter results by symbol kind (function / struct / method / etc.). Returns symbol locations with export status. Faster than Grep because it searches the pre-built index, not disk.",
        annotations(
            title = "Search Symbols",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_grep(
        &self,
        Parameters(params): Parameters<SoulGrepParams>,
    ) -> Result<String, String> {
        reject_mermaid(&params.format, "qartez_grep")?;
        if params.query.trim().is_empty() {
            return Err("query must be non-empty".to_string());
        }
        // Trailing/leading whitespace in an FTS query silently changed
        // the matcher semantics (quoted-phrase vs bareword). Trim
        // eagerly and warn so the caller sees the correction instead of
        // chasing "why does the same symbol return 0 rows with one
        // extra space".
        let raw_query = params.query.as_str();
        let trimmed_query = raw_query.trim();
        let mut prefix_notes = String::new();
        if trimmed_query != raw_query {
            prefix_notes.push_str("// note: query trimmed of surrounding whitespace\n");
        }
        let query_string = trimmed_query.to_string();
        // NOTE: case-insensitive - documented in params.rs description (pending).
        // Reject a bare `*` wildcard when FTS is the backend. FTS5
        // treats `*` as a prefix operator that must attach to a
        // non-empty token, so a lone `*` silently returned
        // "No symbols matching '*'" which reads as "the index is
        // empty" instead of "this is not a valid prefix query".
        // Only fires on the exact bare shape so real queries like
        // `Config*` still work.
        if !params.regex.unwrap_or(false) && query_string == "*" {
            return Err(
                "FTS wildcard must be a prefix (e.g. 'Config*'). Use `regex=true` for full patterns.".into(),
            );
        }
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        // Default is large so the token_budget stays the active governor
        // for output volume. `limit=0` follows the qartez "no cap"
        // convention already used by qartez_unused / qartez_cochange;
        // without this bypass, `limit=0` reached SQL as `LIMIT 0` and
        // returned "No symbols matching X" for symbols that
        // unquestionably exist - a silent divergence from the rest of
        // the tool surface.
        let limit: i64 = match params.limit {
            None => 200,
            Some(0) => i64::MAX,
            Some(n) => n as i64,
        };
        // `token_budget=0|1|…<256` produced payloads where even the
        // "Found N result(s)" header overran the budget, so callers
        // saw an empty truncation line with no rows. Reject anything
        // below the render floor explicitly.
        if let Some(n) = params.token_budget
            && n < 256
        {
            return Err(format!(
                "`token_budget` must be at least 256 (value {n} produces no usable output).",
            ));
        }
        let budget = params.token_budget.unwrap_or(4000) as usize;
        let concise = is_concise(&params.format);
        let use_regex = params.regex.unwrap_or(false);
        let search_bodies = params.search_bodies.unwrap_or(false);
        // `regex=true` runs over indexed symbol names; `search_bodies=true`
        // runs FTS over the body text index. Combining them silently routed
        // to the FTS branch and dropped the regex - leaving the caller
        // thinking "regex was applied" while every match was actually
        // FTS-matched. Reject the combination so the contradiction is
        // visible up front.
        if use_regex && search_bodies {
            return Err(
                "`regex=true` and `search_bodies=true` cannot be combined: regex matches symbol NAMES via the regex engine, while search_bodies matches BODY TEXT via FTS5. Pick one. (regex over body text is a future-work scenario.)"
                    .to_string(),
            );
        }
        let kind_filter = params
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        // FTS5 reserves AND / OR / NEAR / NOT as operators and treats
        // a bare `***` as a syntax error. A query consisting only of
        // these produced either zero rows or an opaque FTS error.
        // Warn loudly (do not reject) so the caller sees what the
        // engine did with their input. NOT shares the same FTS5
        // semantics as AND / OR but was previously absent from the
        // detector, so `parse NOT test` parsed as a boolean exclusion
        // and returned zero rows without any indication that the
        // engine had treated NOT as an operator.
        if !use_regex && !search_bodies {
            let tokens: Vec<&str> = query_string.split_whitespace().collect();
            let has_reserved = tokens.iter().any(|t| {
                matches!(
                    t.to_ascii_uppercase().as_str(),
                    "AND" | "OR" | "NEAR" | "NOT"
                )
            }) || query_string.contains("***");
            if has_reserved {
                prefix_notes.push_str(&format!(
                    "// warning: '{query_string}' contains FTS5 reserved tokens (AND/OR/NEAR/NOT/'***'). Quote the symbol name or use regex=true for literal matching.\n",
                ));
            }
        }

        let results: Vec<(crate::storage::models::SymbolRow, String)> = if search_bodies {
            let fts_query = sanitize_fts_query(&query_string);
            read::search_symbol_bodies_fts(&conn, &fts_query, limit).map_err(|e| {
                format!(
                    "body FTS error: {e}. Try regex=true or drop search_bodies for symbol-name search.",
                )
            })?
        } else if use_regex {
            let re = regex::Regex::new(&query_string).map_err(|e| format!("regex error: {e}"))?;
            let all =
                read::get_all_symbols_with_path(&conn).map_err(|e| format!("DB error: {e}"))?;
            all.into_iter()
                .filter(|(s, _)| re.is_match(&s.name))
                .take(limit as usize)
                .collect()
        } else {
            // Anchor plain prefix queries to the `name` column. Without
            // the `name:` prefix, FTS5 matches any indexed column
            // (name, kind, file_path), so `Parser*` reported unrelated
            // symbols from `parser.rs` (field `parser`, sibling methods
            // `new`, `default`) purely because the file path happened
            // to start with `parser`. The tool docstring promises
            // prefix matching on the symbol name, so every plain-token
            // query is rewritten to the column-qualified form. Quoted
            // phrases (from `sanitize_fts_query`) pass through
            // unchanged so callers who intentionally target
            // `file_path:"..."` or `kind:"..."` operators keep full
            // control.
            let fts_query = anchor_prefix_to_name_column(&sanitize_fts_query(&query_string));
            read::search_symbols_fts(&conn, &fts_query, limit).map_err(|e| {
                format!(
                    "FTS error: {e}. Try regex=true for source-code patterns like `#[tool` or `Foo::bar`.",
                )
            })?
        };

        // `kind` narrows the result set after the index fetch so both
        // name-search and body-search honour the same filter without
        // new SQL variants. The docstring advertised `kind` but the
        // param was silently discarded; this wires it up.
        let results: Vec<_> = if let Some(kind) = kind_filter {
            results
                .into_iter()
                .filter(|(s, _)| s.kind.eq_ignore_ascii_case(kind))
                .collect()
        } else {
            results
        };

        if results.is_empty() {
            let suffix = match kind_filter {
                Some(k) => format!(" (kind={k})"),
                None => String::new(),
            };
            // Wrap the echoed query in backticks instead of single
            // quotes so queries that themselves contain a quote
            // (`fn 'a`, `O'Neil`) do not render as `'O''Neil'` or worse.
            let mut msg = format!("No symbols matching `{query_string}`{suffix}");
            // When the caller explicitly asked for body FTS and got
            // zero rows, nudge them at the common failure modes so
            // they do not have to guess whether the index is empty.
            if search_bodies {
                // Cross-check against the symbol-name FTS index. The
                // body FTS table is rebuilt from `(file_path, symbol_id)`
                // pairs and can diverge from the name index when an
                // alias-prefixed row was indexed but the body
                // rebuilder failed to resolve the absolute path. Body
                // FTS is supposed to be a superset of name matches for
                // any literal token; when it isn't, the caller deserves
                // an explicit pointer at the gap rather than a flat
                // "no matches" that contradicts a parallel name search.
                let name_query = anchor_prefix_to_name_column(&sanitize_fts_query(&query_string));
                let name_hits = read::search_symbols_fts(&conn, &name_query, 1).unwrap_or_default();
                if !name_hits.is_empty() {
                    msg.push_str(&format!(
                        "\n// note: body FTS returned 0 rows but symbol-name FTS has {} match(es) for the same query. The body index may be stale for alias-prefixed paths; rerun with search_bodies=false or call qartez_maintenance to rebuild bodies.",
                        name_hits.len(),
                    ));
                } else if !use_regex {
                    // The "try regex=true" hint is only useful when the
                    // caller did not already pass it. Echoing it back to
                    // a regex caller reads as "your input was ignored"
                    // and wastes a round trip. We rejected
                    // `regex=true && search_bodies` earlier, so this
                    // branch only fires for `regex=false`, but keep the
                    // gate explicit so future routing changes do not
                    // regress the message text.
                    msg.push_str(
                        "\n// note: body FTS returned 0 rows. If you expected matches, verify the text exists literally in function bodies (not just identifiers) and try regex=true.",
                    );
                } else {
                    msg.push_str(
                        "\n// note: body FTS returned 0 rows. If you expected matches, verify the text exists literally in function bodies (not just identifiers).",
                    );
                }
            }
            if !prefix_notes.is_empty() {
                msg.insert_str(0, &prefix_notes);
            }
            return Ok(msg);
        }

        let mut out = format!(
            "Found {} result(s) for `{query_string}`:\n\n",
            results.len(),
        );
        if !prefix_notes.is_empty() {
            out.insert_str(0, &prefix_notes);
        }
        for (sym, file_path) in &results {
            let line = if concise {
                let marker = if sym.is_exported { "+" } else { " " };
                format!(
                    " {marker} {} - {} [L{}]\n",
                    sym.name, file_path, sym.line_start
                )
            } else {
                let exported = if sym.is_exported { "+" } else { " " };
                format!(
                    " {exported} {:<30} {:<12} {}  [L{}-L{}]\n",
                    sym.name, sym.kind, file_path, sym.line_start, sym.line_end,
                )
            };
            if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                out.push_str("  ... (truncated by token budget)\n");
                break;
            }
            out.push_str(&line);

            // When search_bodies=true, the symbol header only reports the
            // enclosing range. Add per-line snippets that show which lines
            // inside the body actually matched, so callers do not have to
            // follow up with qartez_read just to locate the hit.
            if search_bodies {
                let preview =
                    self.body_match_preview(file_path, sym, &query_string, use_regex, budget);
                if estimate_tokens(&out) + estimate_tokens(&preview) > budget {
                    out.push_str("  ... (truncated by token budget)\n");
                    break;
                }
                out.push_str(&preview);
            }
        }
        Ok(out)
    }

    /// Render up to a few concrete line-level matches inside a symbol's
    /// body. Best-effort: on read failure we simply skip the preview so
    /// the caller still sees the symbol-level hit.
    fn body_match_preview(
        &self,
        file_path: &str,
        sym: &crate::storage::models::SymbolRow,
        query: &str,
        use_regex: bool,
        budget: usize,
    ) -> String {
        const MAX_PREVIEW_LINES: usize = 5;
        const MAX_SNIPPET_LEN: usize = 120;

        let Ok(abs_path) = self.safe_resolve(file_path) else {
            return String::new();
        };
        let Ok(source) = std::fs::read_to_string(&abs_path) else {
            return String::new();
        };
        let lines: Vec<&str> = source.lines().collect();
        let start_idx = (sym.line_start as usize).saturating_sub(1);
        let end_idx = (sym.line_end as usize).min(lines.len());
        if start_idx >= end_idx {
            return String::new();
        }

        let re = if use_regex {
            regex::Regex::new(query).ok()
        } else {
            None
        };
        let needle_lower = query.to_lowercase();

        let mut out = String::new();
        let mut shown = 0usize;
        for (offset, raw_line) in lines[start_idx..end_idx].iter().enumerate() {
            let line_no = start_idx + offset + 1;
            let hit = match (&re, use_regex) {
                (Some(pat), true) => pat.is_match(raw_line),
                _ => raw_line.to_lowercase().contains(&needle_lower),
            };
            if !hit {
                continue;
            }
            let trimmed = raw_line.trim();
            let snippet = if trimmed.chars().count() > MAX_SNIPPET_LEN {
                let cut: String = trimmed.chars().take(MAX_SNIPPET_LEN).collect();
                format!("{cut}...")
            } else {
                trimmed.to_string()
            };
            let row = format!("      L{line_no}: {snippet}\n");
            if estimate_tokens(&out) + estimate_tokens(&row) > budget {
                break;
            }
            out.push_str(&row);
            shown += 1;
            if shown >= MAX_PREVIEW_LINES {
                break;
            }
        }
        out
    }
}

/// Restrict a sanitized FTS5 query to the `name` column when it is a
/// plain bareword (optionally ending in a `*` prefix wildcard). The
/// raw `symbols_fts` schema indexes `name`, `kind`, and `file_path`
/// together, so an unqualified `Parser*` matches any of the three.
/// Callers who want the historical docstring promise of "prefix
/// matching on the symbol name" get silent false positives from
/// paths like `parser.rs` (`field parser`, sibling `new`/`default`).
///
/// Behaviour:
/// - `Parser*` -> `name:Parser*` (anchored prefix on name)
/// - `Parser` -> `name:Parser` (anchored exact-token match on name)
/// - `"..."` -> unchanged so intentional phrase queries still hit
///   every indexed column (`file_path:"src/foo.rs"`,
///   `kind:"struct"`, etc.)
fn anchor_prefix_to_name_column(sanitized: &str) -> String {
    // Quoted phrase: respect the caller's intent, they may be
    // targeting another column explicitly.
    if sanitized.starts_with('"') {
        return sanitized.to_string();
    }
    // Already qualified with a column operator: leave alone.
    if sanitized.contains(':') {
        return sanitized.to_string();
    }
    // Plain token (optional trailing `*`): anchor to the name column
    // so the prefix match only fires on symbol names.
    let is_bareword = !sanitized.is_empty()
        && sanitized.char_indices().all(|(i, c)| {
            c.is_alphanumeric() || c == '_' || (c == '*' && i > 0 && i == sanitized.len() - 1)
        });
    if is_bareword {
        format!("name:{sanitized}")
    } else {
        sanitized.to_string()
    }
}

#[cfg(test)]
mod prefix_anchor_tests {
    use super::anchor_prefix_to_name_column;

    #[test]
    fn plain_prefix_is_anchored_to_name_column() {
        assert_eq!(anchor_prefix_to_name_column("Parser*"), "name:Parser*");
    }

    #[test]
    fn plain_token_without_wildcard_is_anchored() {
        assert_eq!(anchor_prefix_to_name_column("Parser"), "name:Parser");
    }

    #[test]
    fn quoted_phrase_is_not_rewritten() {
        assert_eq!(
            anchor_prefix_to_name_column("\"Parser::new\""),
            "\"Parser::new\""
        );
    }

    #[test]
    fn column_qualified_query_passes_through() {
        assert_eq!(
            anchor_prefix_to_name_column("file_path:foo"),
            "file_path:foo"
        );
    }
}
