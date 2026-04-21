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

#[tool_router(router = qartez_smells_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_smells",
        description = "Detect code smells: god functions (high complexity + long body), long parameter lists (too many args), and feature envy (methods that call another type more than their own). Thresholds are configurable. Feature envy detection relies on owner_type, which is only well-populated for Rust and Java.",
        annotations(
            title = "Code Smell Detection",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_smells(
        &self,
        Parameters(params): Parameters<SoulSmellsParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let limit = params.limit.unwrap_or(30) as usize;
        let concise = matches!(params.format, Some(Format::Concise));

        let min_cc = params.min_complexity.unwrap_or(15);
        let min_lines = params.min_lines.unwrap_or(50);
        let min_params = params.min_params.unwrap_or(5) as usize;
        let envy_ratio = params.envy_ratio.unwrap_or(2.0);

        let requested: Vec<&str> = match &params.kind {
            Some(k) => k.split(',').map(|s| s.trim()).collect(),
            None => vec!["god_function", "long_params", "feature_envy"],
        };
        let detect_god = requested.contains(&"god_function");
        let detect_params = requested.contains(&"long_params");
        let detect_envy = requested.contains(&"feature_envy");

        let all_symbols = if let Some(ref fp) = params.file_path {
            let resolved = self.safe_resolve(fp).map_err(|e| e.to_string())?;
            let rel = crate::index::to_forward_slash(
                resolved
                    .strip_prefix(&self.project_root)
                    .unwrap_or(&resolved)
                    .to_string_lossy()
                    .into_owned(),
            );
            let file = read::get_file_by_path(&conn, &rel)
                .map_err(|e| format!("DB error: {e}"))?
                .ok_or_else(|| format!("File not found: {fp}"))?;
            let syms =
                read::get_symbols_for_file(&conn, file.id).map_err(|e| format!("DB error: {e}"))?;
            syms.into_iter()
                .map(|s| (s, rel.clone()))
                .collect::<Vec<_>>()
        } else {
            read::get_all_symbols_with_path(&conn).map_err(|e| format!("DB error: {e}"))?
        };

        let mut god_functions = if detect_god {
            detect_god_functions(&all_symbols, min_cc, min_lines)
        } else {
            Vec::new()
        };
        let mut long_params = if detect_params {
            detect_long_params(&all_symbols, min_params)
        } else {
            Vec::new()
        };
        let mut feature_envy = if detect_envy {
            detect_feature_envy(&conn, &all_symbols, envy_ratio)?
        } else {
            Vec::new()
        };

        let god_count = god_functions.len();
        let params_count = long_params.len();
        let envy_count = feature_envy.len();
        let total = god_count + params_count + envy_count;
        if total == 0 {
            return Ok(
                "No code smells detected with current thresholds. Adjust min_complexity, min_lines, min_params, or envy_ratio to widen the search."
                    .to_string(),
            );
        }

        let god_limit = (limit * god_count)
            .checked_div(total)
            .unwrap_or(limit)
            .max(1);
        let params_limit = (limit * params_count)
            .checked_div(total)
            .unwrap_or(limit)
            .max(1);
        let envy_limit = limit
            .saturating_sub(god_limit)
            .saturating_sub(params_limit)
            .max(1);
        god_functions.truncate(god_limit);
        long_params.truncate(params_limit);
        feature_envy.truncate(envy_limit);

        let shown = god_functions.len() + long_params.len() + feature_envy.len();
        let mut out = format!(
            "# Code Smells ({total} found: {god_count} god functions, {params_count} long param lists, {envy_count} feature envy)\n\n",
        );
        if shown < total {
            out.push_str(&format!(
                "Showing {shown} of {total} (use limit= to see more).\n\n"
            ));
        }

        format_god_functions(&mut out, &god_functions, concise, min_cc, min_lines);
        format_long_params(&mut out, &long_params, concise, min_params);
        format_feature_envy(&mut out, &feature_envy, concise, envy_ratio);

        Ok(out)
    }
}

struct GodFunc {
    name: String,
    path: String,
    cc: u32,
    lines: u32,
    line_start: u32,
    line_end: u32,
}

struct LongParams {
    name: String,
    path: String,
    param_count: usize,
    signature: String,
}

struct FeatureEnvy {
    name: String,
    path: String,
    own_type: String,
    envied_type: String,
    own_calls: usize,
    external_calls: usize,
    ratio: f64,
}

const FUNC_KINDS: &[&str] = &["function", "method"];

fn detect_god_functions(
    all_symbols: &[(crate::storage::models::SymbolRow, String)],
    min_cc: u32,
    min_lines: u32,
) -> Vec<GodFunc> {
    let mut out: Vec<GodFunc> = Vec::new();
    for (sym, path) in all_symbols {
        if !FUNC_KINDS.contains(&sym.kind.as_str()) {
            continue;
        }
        let cc = match sym.complexity {
            Some(c) => c,
            None => continue,
        };
        let body_lines = sym.line_end.saturating_sub(sym.line_start) + 1;
        if cc >= min_cc && body_lines >= min_lines {
            out.push(GodFunc {
                name: sym.name.clone(),
                path: path.clone(),
                cc,
                lines: body_lines,
                line_start: sym.line_start,
                line_end: sym.line_end,
            });
        }
    }
    out.sort_by(|a, b| b.cc.cmp(&a.cc).then(b.lines.cmp(&a.lines)));
    out
}

fn detect_long_params(
    all_symbols: &[(crate::storage::models::SymbolRow, String)],
    min_params: usize,
) -> Vec<LongParams> {
    let mut out: Vec<LongParams> = Vec::new();
    for (sym, path) in all_symbols {
        if !FUNC_KINDS.contains(&sym.kind.as_str()) {
            continue;
        }
        let sig = match &sym.signature {
            Some(s) => s,
            None => continue,
        };
        let count = count_signature_params(sig);
        if count >= min_params {
            out.push(LongParams {
                name: sym.name.clone(),
                path: path.clone(),
                param_count: count,
                signature: sig.clone(),
            });
        }
    }
    out.sort_by(|a, b| b.param_count.cmp(&a.param_count));
    out
}

fn detect_feature_envy(
    conn: &rusqlite::Connection,
    all_symbols: &[(crate::storage::models::SymbolRow, String)],
    envy_ratio: f64,
) -> Result<Vec<FeatureEnvy>, String> {
    let methods_with_owner: Vec<&(crate::storage::models::SymbolRow, String)> = all_symbols
        .iter()
        .filter(|(s, _)| FUNC_KINDS.contains(&s.kind.as_str()) && s.owner_type.is_some())
        .collect();

    if methods_with_owner.is_empty() {
        return Ok(Vec::new());
    }

    let full_symbols =
        read::get_all_symbols_with_path(conn).map_err(|e| format!("DB error: {e}"))?;
    let mut owner_lookup: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    for (sym, _) in &full_symbols {
        if let Some(ref ot) = sym.owner_type {
            owner_lookup.insert(sym.id, ot.clone());
        }
    }

    let mut out: Vec<FeatureEnvy> = Vec::new();
    for (sym, path) in &methods_with_owner {
        let own_type = sym.owner_type.as_ref().unwrap();

        let refs: Vec<i64> = conn
            .prepare_cached("SELECT to_symbol_id FROM symbol_refs WHERE from_symbol_id = ?1")
            .and_then(|mut stmt| {
                let rows = stmt.query_map([sym.id], |row| row.get(0))?;
                rows.collect()
            })
            .map_err(|e| format!("DB error: {e}"))?;

        if refs.is_empty() {
            continue;
        }

        let mut own_calls: usize = 0;
        let mut external_by_type: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        for to_id in &refs {
            match owner_lookup.get(to_id) {
                Some(target_type) if target_type == own_type => {
                    own_calls += 1;
                }
                Some(target_type) => {
                    *external_by_type.entry(target_type.clone()).or_insert(0) += 1;
                }
                None => {}
            }
        }

        for (ext_type, ext_count) in &external_by_type {
            let ratio = if own_calls == 0 {
                *ext_count as f64
            } else {
                *ext_count as f64 / own_calls as f64
            };
            if ratio >= envy_ratio && *ext_count >= 2 {
                out.push(FeatureEnvy {
                    name: sym.name.clone(),
                    path: (*path).clone(),
                    own_type: own_type.clone(),
                    envied_type: ext_type.clone(),
                    own_calls,
                    external_calls: *ext_count,
                    ratio,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        a.ratio
            .partial_cmp(&b.ratio)
            .unwrap_or(std::cmp::Ordering::Equal)
            .reverse()
    });
    Ok(out)
}

fn format_god_functions(
    out: &mut String,
    god_functions: &[GodFunc],
    concise: bool,
    min_cc: u32,
    min_lines: u32,
) {
    if god_functions.is_empty() {
        return;
    }
    if concise {
        out.push_str("## God Functions\n");
        for g in god_functions {
            out.push_str(&format!(
                "  {} @ {} L{}-{} CC={} lines={}\n",
                g.name, g.path, g.line_start, g.line_end, g.cc, g.lines,
            ));
        }
    } else {
        out.push_str(&format!(
            "## God Functions (CC >= {min_cc} AND lines >= {min_lines})\n\n"
        ));
        out.push_str("| Symbol | File | CC | Lines | Range |\n");
        out.push_str("|--------|------|----|-------|-------|\n");
        for g in god_functions {
            out.push_str(&format!(
                "| {} | {} | {} | {} | L{}-{} |\n",
                g.name, g.path, g.cc, g.lines, g.line_start, g.line_end,
            ));
        }
    }
    out.push('\n');
}

fn format_long_params(
    out: &mut String,
    long_params: &[LongParams],
    concise: bool,
    min_params: usize,
) {
    if long_params.is_empty() {
        return;
    }
    if concise {
        out.push_str("## Long Parameter Lists\n");
        for lp in long_params {
            out.push_str(&format!(
                "  {} @ {} params={}\n",
                lp.name, lp.path, lp.param_count,
            ));
        }
    } else {
        out.push_str(&format!(
            "## Long Parameter Lists (>= {min_params} params, excluding self)\n\n"
        ));
        out.push_str("| Symbol | File | Params | Signature |\n");
        out.push_str("|--------|------|--------|-----------|\n");
        for lp in long_params {
            let sig_display = if lp.signature.len() > 80 {
                let end = crate::str_utils::floor_char_boundary(&lp.signature, 77);
                format!("{}...", &lp.signature[..end])
            } else {
                lp.signature.clone()
            };
            out.push_str(&format!(
                "| {} | {} | {} | `{}` |\n",
                lp.name, lp.path, lp.param_count, sig_display,
            ));
        }
    }
    out.push('\n');
}

fn format_feature_envy(
    out: &mut String,
    feature_envy: &[FeatureEnvy],
    concise: bool,
    envy_ratio: f64,
) {
    if feature_envy.is_empty() {
        return;
    }
    if concise {
        out.push_str("## Feature Envy\n");
        for fe in feature_envy {
            out.push_str(&format!(
                "  {} @ {} own={} ext={}({}) ratio={:.1}\n",
                fe.name, fe.path, fe.own_type, fe.envied_type, fe.external_calls, fe.ratio,
            ));
        }
    } else {
        out.push_str(&format!(
            "## Feature Envy (external/own ratio >= {envy_ratio:.1})\n\n"
        ));
        out.push_str(
            "| Symbol | File | Own Type | Envied Type | Own Calls | Ext Calls | Ratio |\n",
        );
        out.push_str(
            "|--------|------|----------|-------------|-----------|-----------|-------|\n",
        );
        for fe in feature_envy {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {:.1} |\n",
                fe.name,
                fe.path,
                fe.own_type,
                fe.envied_type,
                fe.own_calls,
                fe.external_calls,
                fe.ratio,
            ));
        }
    }
    out.push('\n');
}
/// Count the number of parameters in a function signature string, excluding
/// receiver params (`self`, `&self`, `&mut self` in Rust, `self`/`cls` in
/// Python). Handles nested generics (`HashMap<K, V>`) and nested parens so
/// commas inside type parameters are not miscounted.
pub(super) fn count_signature_params(sig: &str) -> usize {
    // Find the first '(' and its matching ')'
    let start = match sig.find('(') {
        Some(i) => i + 1,
        None => return 0,
    };
    let mut depth: u32 = 1;
    let mut end = start;
    for (i, &byte) in sig.as_bytes().iter().enumerate().skip(start) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    let params_str = sig[start..end].trim();
    if params_str.is_empty() {
        return 0;
    }
    // Split by commas, respecting angle brackets `<>` and nested parens
    let mut params = Vec::new();
    let mut angle_depth: u32 = 0;
    let mut paren_depth: u32 = 0;
    let mut seg_start = 0;
    for (i, ch) in params_str.char_indices() {
        match ch {
            '<' => angle_depth += 1,
            '>' if angle_depth > 0 => angle_depth -= 1,
            '(' => paren_depth += 1,
            ')' if paren_depth > 0 => paren_depth -= 1,
            ',' if angle_depth == 0 && paren_depth == 0 => {
                params.push(params_str[seg_start..i].trim());
                seg_start = i + 1;
            }
            _ => {}
        }
    }
    params.push(params_str[seg_start..].trim());
    // Filter out receiver params and empty segments
    params
        .into_iter()
        .filter(|p| {
            if p.is_empty() {
                return false;
            }
            // Rust receiver variants
            let base = p.split(':').next().unwrap_or(p).trim();
            !matches!(base, "self" | "&self" | "&mut self" | "mut self" | "cls")
        })
        .count()
}
#[cfg(test)]
mod param_count_tests {
    use super::count_signature_params;

    #[test]
    fn empty_params() {
        assert_eq!(count_signature_params("fn foo()"), 0);
    }

    #[test]
    fn simple_params() {
        assert_eq!(count_signature_params("fn foo(a: i32, b: String)"), 2);
    }

    #[test]
    fn excludes_self() {
        assert_eq!(
            count_signature_params("fn foo(&self, a: i32, b: String)"),
            2
        );
        assert_eq!(count_signature_params("fn foo(&mut self, a: i32)"), 1);
        assert_eq!(count_signature_params("fn foo(self)"), 0);
        assert_eq!(count_signature_params("fn foo(mut self, x: u8)"), 1);
    }

    #[test]
    fn nested_generics() {
        assert_eq!(
            count_signature_params("fn foo(map: HashMap<K, V>, list: Vec<String>)"),
            2,
        );
        assert_eq!(
            count_signature_params("fn foo(x: Result<Vec<u8>, Box<dyn Error>>)"),
            1,
        );
    }

    #[test]
    fn many_params() {
        assert_eq!(
            count_signature_params("fn build(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32)"),
            6,
        );
    }

    #[test]
    fn no_parens() {
        assert_eq!(count_signature_params("struct Foo"), 0);
    }

    #[test]
    fn excludes_python_cls() {
        assert_eq!(count_signature_params("def foo(cls, bar, baz)"), 2);
    }

    #[test]
    fn nested_parens_in_type() {
        assert_eq!(
            count_signature_params("fn foo(f: fn(i32) -> bool, x: i32)"),
            2,
        );
    }
}
