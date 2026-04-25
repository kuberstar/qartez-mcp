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

/// Categorical whitelist for the `kind` parameter of `qartez_smells`.
/// Callers pass a comma-separated selection; any token outside this set
/// is rejected with a diagnostic listing the valid values, matching the
/// cross-tool validation policy applied to all categorical params.
const VALID_SMELL_KINDS: &[&str] = &["god_function", "long_params", "feature_envy"];

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
        reject_mermaid(&params.format, "qartez_smells")?;
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        // `limit=0` means "no cap" project-wide convention; `None` keeps
        // the historical default of 30.
        let limit = match params.limit {
            None => 30,
            Some(0) => usize::MAX,
            Some(n) => n as usize,
        };
        let concise = matches!(params.format, Some(Format::Concise));

        let min_cc = params.min_complexity.unwrap_or(15);
        let min_lines = params.min_lines.unwrap_or(50);
        let min_params = params.min_params.unwrap_or(5) as usize;
        // Validate `envy_ratio` against the documented contract. The
        // detector compares `external_calls / own_calls > envy_ratio`,
        // so a value <= 0 makes every method match (the ratio is
        // non-negative by construction) and NaN/Inf produce a useless
        // wall of false positives. Reject explicitly to mirror the
        // already-strict `max_health` validation in qartez_health
        // rather than silently accepting an unusable threshold.
        if let Some(r) = params.envy_ratio
            && (!r.is_finite() || r <= 0.0)
        {
            return Err(format!(
                "envy_ratio must be a finite value > 0 (got {r}). The detector compares external_calls/own_calls > envy_ratio; values <= 0 match every method and produce noise."
            ));
        }
        let envy_ratio = params.envy_ratio.unwrap_or(2.0);

        // Parse the comma-separated kind selection leniently: empty
        // segments (from `"god_function,"` or `",,feature_envy"`) are
        // trimmed, and duplicate entries collapse to one. Before,
        // `"god_function,"` errored on the empty segment while
        // `"god_function,god_function"` silently deduped, which was
        // an inconsistent validation surface. Now both shapes parse
        // to the same intent.
        let requested: Vec<&str> = match &params.kind {
            Some(k) => {
                let mut seen: Vec<&str> = Vec::new();
                for token in k.split(',').map(str::trim) {
                    if token.is_empty() {
                        continue;
                    }
                    if !seen.contains(&token) {
                        seen.push(token);
                    }
                }
                seen
            }
            None => vec!["god_function", "long_params", "feature_envy"],
        };
        if requested.is_empty() {
            // After trimming empty segments the selection collapsed
            // to nothing (e.g. `kind=",,"`). Reject explicitly so the
            // caller does not fall through to an empty "no smells"
            // result that reads like success.
            return Err(format!(
                "kind must name at least one smell after trimming empty segments. valid: [{}]",
                VALID_SMELL_KINDS.join(", "),
            ));
        }
        // Partition the request into known and unknown kinds. An
        // all-unknown selection is still a hard reject (the caller
        // asked for nothing we can produce), but a mixed selection
        // such as `kind="god_function,unknown_smell"` now runs the
        // known kind(s) and surfaces the unknown ones as a warning
        // banner instead of refusing the whole call. This matches
        // the lenient parsing already applied to empty segments:
        // one typo should not destroy the valid request.
        let (known_kinds, unknown_kinds): (Vec<&str>, Vec<&str>) = requested
            .iter()
            .copied()
            .partition(|k| VALID_SMELL_KINDS.contains(k));
        if known_kinds.is_empty() {
            return Err(format!(
                "no known smell kinds in selection: [{}]. valid: [{}]",
                unknown_kinds.join(", "),
                VALID_SMELL_KINDS.join(", "),
            ));
        }
        let unknown_warning = if unknown_kinds.is_empty() {
            String::new()
        } else {
            format!(
                "// warning: {} unknown smell kind(s) ignored: [{}]. valid: [{}]\n\n",
                unknown_kinds.len(),
                unknown_kinds.join(", "),
                VALID_SMELL_KINDS.join(", "),
            )
        };
        let detect_god = known_kinds.contains(&"god_function");
        let detect_params = known_kinds.contains(&"long_params");
        let detect_envy = known_kinds.contains(&"feature_envy");

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
                .ok_or_else(|| format!("File '{fp}' not found in index"))?;
            let syms =
                read::get_symbols_for_file(&conn, file.id).map_err(|e| format!("DB error: {e}"))?;
            syms.into_iter()
                .map(|s| (s, rel.clone()))
                .collect::<Vec<_>>()
        } else {
            read::get_all_symbols_with_path(&conn).map_err(|e| format!("DB error: {e}"))?
        };

        let mut god_functions = if detect_god {
            detect_god_functions(&conn, &all_symbols, min_cc, min_lines)
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
            return Ok(format!(
                "{unknown_warning}No code smells detected with current thresholds. Adjust min_complexity, min_lines, min_params, or envy_ratio to widen the search.",
            ));
        }

        // Budget per-kind proportionally. When `limit == usize::MAX` (the
        // "no cap" sentinel) keep all rows unchanged, which also avoids the
        // `usize::MAX * count` overflow the proportional split would hit.
        if limit != usize::MAX {
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
        }

        let shown = god_functions.len() + long_params.len() + feature_envy.len();
        // Include only the categories the caller actually asked
        // about. Before this, `kind=god_function` still rendered
        // "god_functions: N, long_params: 0, feature_envy: 0" and
        // the zeros read as "searched, found none" instead of
        // "filtered out by kind".
        let mut counts: Vec<String> = Vec::new();
        if detect_god {
            counts.push(format!("{god_count} god functions"));
        }
        if detect_params {
            counts.push(format!("{params_count} long param lists"));
        }
        if detect_envy {
            counts.push(format!("{envy_count} feature envy"));
        }
        let mut out = format!(
            "{unknown_warning}# Code Smells ({total} found: {})\n\n",
            counts.join(", "),
        );
        if shown < total {
            out.push_str(&format!(
                "Showing {shown} of {total} (use limit= to see more).\n\n"
            ));
        }

        format_god_functions(&mut out, &god_functions, concise, min_cc, min_lines);
        if detect_god && god_functions.is_empty() {
            out.push_str("## God Functions: 0 found at current thresholds.\n\n");
        }
        format_long_params(&mut out, &long_params, concise, min_params);
        if detect_params && long_params.is_empty() {
            out.push_str("## Long Parameter Lists: 0 found at current thresholds.\n\n");
        }
        format_feature_envy(&mut out, &feature_envy, concise, envy_ratio);
        if detect_envy && feature_envy.is_empty() {
            // Zero-count markers make the asymmetric output
            // observable. Before, a scan that returned
            // `god=283, long_params=266, feature_envy=0` rendered the
            // first two as detailed tables and silently dropped the
            // feature_envy section, giving callers no explicit signal
            // that the detector ran. A terse "0 found" line keeps the
            // structural symmetry without bloating the output.
            out.push_str("## Feature Envy: 0 found at current thresholds.\n\n");
        }

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
    /// Sub-kind describing the shape of the function. `"god_function"` is the
    /// default; `"flat_dispatcher"` marks a function whose complexity budget
    /// is dominated by a flat match/switch over many trivial arms, which
    /// responds poorly to the standard "Extract Method" advice and deserves
    /// a split-by-kind recommendation instead.
    kind: &'static str,
    /// Number of match/if-else arms detected in the body when `kind` is
    /// `"flat_dispatcher"`. Zero for plain god functions.
    arm_count: u32,
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
    conn: &rusqlite::Connection,
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
            let (kind, arm_count) = classify_function_shape(conn, sym.id, cc);
            out.push(GodFunc {
                name: sym.name.clone(),
                path: path.clone(),
                cc,
                lines: body_lines,
                line_start: sym.line_start,
                line_end: sym.line_end,
                kind,
                arm_count,
            });
        }
    }
    out.sort_by(|a, b| b.cc.cmp(&a.cc).then(b.lines.cmp(&a.lines)));
    out
}

/// Arm-count threshold below which a flat dispatcher is indistinguishable
/// from an ordinary compound function. Chosen at 6 so a 4-arm match
/// followed by other control flow still classifies as a god function.
const FLAT_DISPATCHER_MIN_ARMS: u32 = 6;

/// Maximum gap between raw CC and the arm-count proxy for a tight match.
/// When CC exceeds (arms + slack) but arms still dominate, the looser
/// arm-fraction path picks up the slack.
const FLAT_DISPATCHER_CC_SLACK: u32 = 5;

/// Arm count above which the looser "arm fraction of CC" path kicks in.
/// Dispatchers like `build_tool_call` (22 arms, CC=48) have minor `if let`
/// branches inside arms but remain flat in shape; require a large arm
/// population to justify the looser threshold.
const FLAT_DISPATCHER_MIN_ARMS_DOMINANT: u32 = 12;

/// Fraction (in [0.0, 1.0]) of total CC that must come from match arms
/// under the dominant-arm path. 0.4 captures dispatchers whose arms carry
/// light `if let`/`Option` unwrapping without admitting deeply-nested
/// god-functions with a small outer match.
const FLAT_DISPATCHER_ARM_FRACTION: f64 = 0.4;

/// Classify a function as a plain god function or a flat-match dispatcher.
///
/// Returns `("flat_dispatcher", arm_count)` when the body's branching is
/// dominated by a single match/if-else chain of many trivial arms. Two
/// paths qualify:
///   1. Tight: `arms >= MIN_ARMS` and `cc <= arms + CC_SLACK` - the
///      canonical flat dispatch table with near-trivial arms.
///   2. Dominant: `arms >= MIN_ARMS_DOMINANT` and arms account for at
///      least `ARM_FRACTION` of total CC - catches real-world dispatchers
///      whose arms contain small conditionals (`if let Some(x) = ..`).
///
/// Falls back to `("god_function", 0)` when the body is unavailable or
/// neither path fires. Any failure downgrades to `god_function` rather
/// than silently muting a true god function.
fn classify_function_shape(
    conn: &rusqlite::Connection,
    symbol_id: i64,
    cc: u32,
) -> (&'static str, u32) {
    let Some(body) = fetch_symbol_body(conn, symbol_id) else {
        return ("god_function", 0);
    };
    let arms = count_match_arms(&body);
    if arms < FLAT_DISPATCHER_MIN_ARMS {
        return ("god_function", 0);
    }
    // Path 1: tight dispatcher - CC is within slack of arm count.
    if cc <= arms.saturating_add(FLAT_DISPATCHER_CC_SLACK) {
        return ("flat_dispatcher", arms);
    }
    // Path 2: dominant dispatcher - many arms, arms still carry the
    // majority-or-near-majority of CC. Keeps build_tool_call-style
    // dispatchers (22 arms, CC=48, in-arm `if let` branches) from being
    // reported as god functions while excluding nested god-functions with
    // a small outer match.
    if arms >= FLAT_DISPATCHER_MIN_ARMS_DOMINANT
        && (arms as f64) >= (cc as f64) * FLAT_DISPATCHER_ARM_FRACTION
    {
        return ("flat_dispatcher", arms);
    }
    ("god_function", 0)
}

/// Pull a symbol's body from the FTS content table. `symbols_body_fts.rowid`
/// matches `symbols.id` by construction (see `rebuild_symbol_bodies_multi`),
/// so a direct rowid lookup is O(1).
pub(super) fn fetch_symbol_body(conn: &rusqlite::Connection, symbol_id: i64) -> Option<String> {
    conn.prepare_cached("SELECT body FROM symbols_body_fts WHERE rowid = ?1")
        .ok()?
        .query_row([symbol_id], |row| row.get::<_, String>(0))
        .ok()
}

/// Count the number of match arms (and `=>` lambda arrows) in a Rust/Swift
/// style body. Ignores occurrences inside `//` line comments and string
/// literals so comments and in-arm docstrings don't inflate the count.
///
/// This is a line-level proxy, not a real parse. The caller must pair it
/// with a cyclomatic-complexity sanity check (see
/// [`classify_function_shape`]) to avoid classifying arbitrary closure
/// chains as dispatchers.
fn count_match_arms(body: &str) -> u32 {
    let mut count: u32 = 0;
    for raw_line in body.lines() {
        // Strip trailing line comments.
        let line = match raw_line.find("//") {
            Some(i) => &raw_line[..i],
            None => raw_line,
        };
        let bytes = line.as_bytes();
        let mut i = 0;
        let mut in_string = false;
        let mut escape = false;
        while i + 1 < bytes.len() {
            let b = bytes[i];
            if escape {
                escape = false;
                i += 1;
                continue;
            }
            match b {
                b'\\' if in_string => {
                    escape = true;
                }
                b'"' => {
                    in_string = !in_string;
                }
                b'=' if !in_string
                    && i + 1 < bytes.len()
                    && bytes[i + 1] == b'>'
                    && (i == 0 || (bytes[i - 1] != b'=' && bytes[i - 1] != b'>')) =>
                {
                    count = count.saturating_add(1);
                    i += 2;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
    }
    count
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
    // Build a method-name -> {owner_type} index so we can detect trait
    // dispatch fan-out. When one method name is implemented by many distinct
    // owner types (classic trait-object pattern), a single `dyn Trait` call
    // at the source expands into N refs with identical method names and
    // different owner types - that shape reads as envy to the naive
    // detector. Pre-computing once keeps per-caller analysis O(refs).
    let mut method_owners: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for (sym, _) in &full_symbols {
        if !FUNC_KINDS.contains(&sym.kind.as_str()) {
            continue;
        }
        if let Some(ref ot) = sym.owner_type {
            method_owners
                .entry(sym.name.clone())
                .or_default()
                .insert(ot.clone());
        }
    }

    let mut out: Vec<FeatureEnvy> = Vec::new();
    for (sym, path) in &methods_with_owner {
        let own_type = sym.owner_type.as_ref().unwrap();

        // Service-handler exclusion: types whose name matches a classical
        // service suffix (Server, Handler, Controller, Route, Service) or
        // whose suffix is a dispatch/router construct almost always
        // legitimately operate on DTO parameters. Flagging those as envy
        // produces noise on hand-rolled MVC layers without surfacing any
        // actionable refactor.
        if is_service_like_type(own_type) {
            continue;
        }

        // Only `call` refs count for envy, and we need the target signature so
        // we can exclude associated-function calls like `Step::new(...)`.
        // We also pull the target symbol name so we can spot trait dispatch
        // fan-out: a single source-level call through `dyn Trait` expands
        // into one ref per impl, all with the same method name but
        // different owner types.
        let refs: Vec<(i64, String, Option<String>, String)> = conn
            .prepare_cached(
                "SELECT sr.to_symbol_id, sr.kind, s.signature, s.name \
                 FROM symbol_refs sr \
                 JOIN symbols s ON s.id = sr.to_symbol_id \
                 WHERE sr.from_symbol_id = ?1",
            )
            .and_then(|mut stmt| {
                let rows = stmt.query_map([sym.id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?;
                rows.collect()
            })
            .map_err(|e| format!("DB error: {e}"))?;

        if refs.is_empty() {
            continue;
        }

        let mut own_calls: usize = 0;
        // External call breakdown per envied type, with the set of method
        // names involved. The set lets us check whether a type's contribution
        // is dominated by a single trait-style method.
        let mut external_by_type: std::collections::HashMap<
            String,
            (usize, std::collections::HashMap<String, usize>),
        > = std::collections::HashMap::new();

        for (to_id, ref_kind, target_sig, target_name) in &refs {
            // Non-`call` refs (type references, use imports) aren't envy.
            if ref_kind != "call" {
                continue;
            }
            // Feature envy per Fowler requires calling instance methods on a
            // foreign object. Associated-function calls (`Type::new(...)`,
            // `Step::god(...)`) are constructors/factories, not envy. Fail
            // closed: when the target signature is missing (cross-file
            // macro-generated target, trait impl without visible signature),
            // skip it rather than assume envy.
            let has_self = target_sig.as_deref().is_some_and(signature_has_self);
            if !has_self {
                continue;
            }
            match owner_lookup.get(to_id) {
                Some(target_type) if target_type == own_type => {
                    own_calls += 1;
                }
                Some(target_type) => {
                    let entry = external_by_type
                        .entry(target_type.clone())
                        .or_insert_with(|| (0, std::collections::HashMap::new()));
                    entry.0 += 1;
                    *entry.1.entry(target_name.clone()).or_insert(0) += 1;
                }
                None => {}
            }
        }

        // Identify the dominant trait-dispatch method (if any). A single
        // source-level `dyn Trait` call fans out across every impl; when
        // >=3 distinct owner types share the same method name, treat calls
        // to that name as dispatch rather than envy.
        let trait_dispatch_method = dominant_trait_method(&external_by_type, &method_owners);

        // Caller-level trait fan-out: when the caller's external-by-type
        // breakdown is mostly fanned-out impls of the same trait (many
        // distinct owner types each called with method names that are
        // themselves implemented by >= MIN_TRAIT_IMPLS owners), attribute
        // everything to dispatch and suppress envy for this caller.
        // Catches multi-method dispatch like `parse_file` calling
        // `{tree_sitter_language, extract, language_name}` on
        // `Box<dyn LanguageSupport>` - the per-type single-method check
        // missed that shape.
        if is_pure_trait_fanout(&external_by_type, &method_owners) {
            continue;
        }

        for (ext_type, (ext_count, methods)) in &external_by_type {
            // Per-type trait-dispatch suppression. Count how many of this
            // type's calls are to methods implemented by >= MIN_TRAIT_IMPLS
            // distinct owner types (trait methods). When all (or all but
            // one) of this type's calls are trait methods, the contribution
            // is pure fan-out and must be dropped, regardless of whether
            // any single method dominates.
            let mut trait_calls: usize = 0;
            for (mname, count) in methods {
                let impl_count = method_owners
                    .get(mname)
                    .map(std::collections::HashSet::len)
                    .unwrap_or(0);
                if impl_count >= MIN_TRAIT_IMPLS {
                    trait_calls = trait_calls.saturating_add(*count);
                }
            }
            if trait_calls.saturating_add(1) >= *ext_count {
                continue;
            }

            // Fallback: dominant-method check for single-method dispatch
            // shapes where the aggregate trait_calls sum misses a widely
            // implemented name (defensive, kept for backward compat).
            if let Some(ref dispatch) = trait_dispatch_method {
                if let Some(&m_count) = methods.get(dispatch.as_str()) {
                    if m_count + 1 >= *ext_count {
                        continue;
                    }
                }
            }

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

/// Service-handler suffix list. Owner types ending in one of these names
/// are container types for state + endpoint dispatch rather than data
/// objects, so envy ratios against DTO parameters are structurally
/// expected and not actionable. Kept conservative - only include suffixes
/// that are idiomatic markers of the pattern, not generic words.
const SERVICE_TYPE_SUFFIXES: &[&str] = &[
    "Server",
    "Handler",
    "Controller",
    "Route",
    "Router",
    "Service",
    "Endpoint",
    "Dispatcher",
];

/// Returns true when `type_name` looks like a service/handler/controller
/// container rather than a data type. Matches the ASCII suffix of the
/// unqualified type name; generic parameters and module paths are
/// stripped first so `crate::server::QartezServer<T>` still matches.
fn is_service_like_type(type_name: &str) -> bool {
    // Strip module path.
    let after_path = type_name.rsplit("::").next().unwrap_or(type_name);
    // Strip generic parameters.
    let bare = after_path.split('<').next().unwrap_or(after_path).trim();
    SERVICE_TYPE_SUFFIXES
        .iter()
        .any(|suffix| bare.ends_with(suffix))
}

/// A method must appear on at least this many distinct owner types before
/// we treat calls to it as trait dispatch. Three is the smallest population
/// that rules out accidental name collisions between two unrelated types.
const MIN_TRAIT_IMPLS: usize = 3;

/// When a caller's external-by-type breakdown lists at least this many
/// distinct owner types, and most of their call contributions go through
/// trait-level methods, the caller is doing a `dyn Trait`-style fan-out
/// and none of its per-type envy rows are actionable. Five matches the
/// industrial heuristic: rust-analyzer's semantic-tokens classifier
/// treats a receiver with >=5 impl candidates as polymorphic dispatch.
const MIN_TRAIT_FANOUT: usize = 5;

/// Fraction (in [0.0, 1.0]) of a caller's external calls that must route
/// through trait methods (i.e., methods implemented by >= MIN_TRAIT_IMPLS
/// owner types) before the whole breakdown is classified as fan-out.
/// 0.8 tolerates a few incidental non-trait helpers on the same caller.
const TRAIT_FANOUT_RATIO: f64 = 0.8;

/// Detect whether a caller's external-by-type breakdown is a trait-object
/// fan-out rather than real envy. Returns true when at least
/// `MIN_TRAIT_FANOUT` distinct envied types are present AND at least
/// `TRAIT_FANOUT_RATIO` of the caller's external calls route through
/// methods that are themselves implemented by >= `MIN_TRAIT_IMPLS`
/// distinct owner types (the classic trait-object shape).
fn is_pure_trait_fanout(
    external_by_type: &std::collections::HashMap<
        String,
        (usize, std::collections::HashMap<String, usize>),
    >,
    method_owners: &std::collections::HashMap<String, std::collections::HashSet<String>>,
) -> bool {
    if external_by_type.len() < MIN_TRAIT_FANOUT {
        return false;
    }
    let mut total_calls: usize = 0;
    let mut trait_calls: usize = 0;
    for (_, methods) in external_by_type.values() {
        for (mname, count) in methods {
            total_calls = total_calls.saturating_add(*count);
            let impl_count = method_owners
                .get(mname)
                .map(std::collections::HashSet::len)
                .unwrap_or(0);
            if impl_count >= MIN_TRAIT_IMPLS {
                trait_calls = trait_calls.saturating_add(*count);
            }
        }
    }
    if total_calls == 0 {
        return false;
    }
    (trait_calls as f64) / (total_calls as f64) >= TRAIT_FANOUT_RATIO
}

/// Pick the single method name that most plausibly represents a trait
/// dispatch at the call site. Heuristic: among all external methods called
/// by this caller, find one whose name is implemented by at least
/// `MIN_TRAIT_IMPLS` distinct owner types - the trait-object signature
/// shape. Returns `None` when no such method exists, disabling the
/// trait-dispatch suppression for this caller.
fn dominant_trait_method(
    external_by_type: &std::collections::HashMap<
        String,
        (usize, std::collections::HashMap<String, usize>),
    >,
    method_owners: &std::collections::HashMap<String, std::collections::HashSet<String>>,
) -> Option<String> {
    let mut best: Option<(String, usize)> = None;
    for (_total, methods) in external_by_type.values() {
        for mname in methods.keys() {
            let impl_count = method_owners
                .get(mname)
                .map(std::collections::HashSet::len)
                .unwrap_or(0);
            if impl_count < MIN_TRAIT_IMPLS {
                continue;
            }
            match &best {
                Some((_, prev)) if *prev >= impl_count => {}
                _ => best = Some((mname.clone(), impl_count)),
            }
        }
    }
    best.map(|(name, _)| name)
}

/// Returns true if the signature's first parameter is a `self` receiver
/// (`self`, `mut self`, `&self`, `&mut self`, or a lifetime-qualified
/// reference like `&'a self`). A missing or non-self first parameter means
/// this is an associated function (constructor/factory), not an instance
/// method - so it should not count toward feature envy.
fn signature_has_self(sig: &str) -> bool {
    let Some(open) = sig.find('(') else {
        return false;
    };
    let rest = &sig[open + 1..];
    let first_end = rest.find([',', ')']).unwrap_or(rest.len());
    let first = rest[..first_end].trim();
    // Peel off the type annotation to handle arbitrary-self-types like
    // `self: Box<Self>`, `self: Pin<&mut Self>`, `self: Rc<Self>` (RFC
    // 3324). The receiver token is whatever sits before the first `:`,
    // which must match one of the classical self-receiver forms.
    let head = first
        .split_once(':')
        .map(|(h, _)| h.trim())
        .unwrap_or(first);
    if matches!(head, "self" | "mut self" | "&self" | "&mut self") {
        return true;
    }
    // Lifetime-qualified receivers: `&'a self`, `&'a mut self`, etc.
    if head.starts_with('&') && head.contains("self") {
        return true;
    }
    false
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
            let tag = if g.kind == "flat_dispatcher" {
                format!(" [flat_dispatcher arms={}]", g.arm_count)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "  {} @ {} L{}-{} CC={} lines={}{}\n",
                g.name, g.path, g.line_start, g.line_end, g.cc, g.lines, tag,
            ));
        }
    } else {
        out.push_str(&format!(
            "## God Functions (CC >= {min_cc} AND lines >= {min_lines})\n\n"
        ));
        out.push_str("| Symbol | File | CC | Lines | Range | Kind |\n");
        out.push_str("|--------|------|----|-------|-------|------|\n");
        let mut any_flat = false;
        for g in god_functions {
            let kind_cell = if g.kind == "flat_dispatcher" {
                any_flat = true;
                format!("flat_dispatcher (arms={})", g.arm_count)
            } else {
                "god_function".to_string()
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | L{}-{} | {} |\n",
                g.name, g.path, g.cc, g.lines, g.line_start, g.line_end, kind_cell,
            ));
        }
        if any_flat {
            out.push('\n');
            out.push_str(
                "Note: `flat_dispatcher` entries are flat match/switch tables with many trivial arms. CC inflates linearly with arm count, so \"Extract Method on the largest branch\" rarely helps. Prefer splitting by variant into separate handlers, or accept the shape unless arms grow non-trivial.\n",
            );
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
pub(in crate::server) fn count_signature_params(sig: &str) -> usize {
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
    use super::{count_signature_params, signature_has_self};

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

    #[test]
    fn signature_has_self_classical_receivers() {
        for sig in &[
            "fn a(self)",
            "fn a(mut self)",
            "fn a(&self)",
            "fn a(&mut self)",
            "fn a(&self, x: i32)",
            "fn a(&'a self)",
            "fn a(&'a mut self)",
        ] {
            assert!(signature_has_self(sig), "must accept {sig}");
        }
    }

    #[test]
    fn signature_has_self_arbitrary_self_types() {
        for sig in &[
            "fn a(self: Box<Self>)",
            "fn a(self: Rc<Self>)",
            "fn a(self: Arc<Self>)",
            "fn a(self: Pin<&mut Self>, x: i32)",
            "async fn a(self: Pin<&Self>)",
            "fn a(mut self: Box<Self>)",
        ] {
            assert!(signature_has_self(sig), "must accept {sig}");
        }
    }

    #[test]
    fn signature_has_self_rejects_associated_fn() {
        for sig in &[
            "fn new(name: &str) -> Self",
            "fn factory(input: u32) -> Self",
            "fn no_args()",
            "fn with_colon(cls: Foo, x: i32)",
        ] {
            assert!(!signature_has_self(sig), "must reject {sig}");
        }
    }

    #[test]
    fn count_match_arms_counts_simple_arms() {
        let body = r#"match x {
            A => 1,
            B => 2,
            C => 3,
            D => 4,
            E => 5,
            F => 6,
        }"#;
        assert_eq!(super::count_match_arms(body), 6);
    }

    #[test]
    fn count_match_arms_ignores_comparison_operators() {
        let body = "if a >= 1 && b == 2 && c <= 3 { return; }";
        assert_eq!(super::count_match_arms(body), 0);
    }

    #[test]
    fn count_match_arms_skips_comment_arrows() {
        let body = r#"fn f() {
            // Note: => is fine inside comments
            let _ = 1;
        }"#;
        assert_eq!(super::count_match_arms(body), 0);
    }

    #[test]
    fn count_match_arms_skips_arrow_in_string_literal() {
        let body = r#"let s = "foo => bar";"#;
        assert_eq!(super::count_match_arms(body), 0);
    }

    #[test]
    fn is_service_like_type_recognizes_server_suffix() {
        assert!(super::is_service_like_type("QartezServer"));
        assert!(super::is_service_like_type("MyHandler"));
        assert!(super::is_service_like_type("UserController"));
        assert!(super::is_service_like_type("Route"));
        assert!(super::is_service_like_type("api::v1::UserRouter"));
        assert!(super::is_service_like_type("ApiService<T>"));
        assert!(super::is_service_like_type("JobDispatcher"));
    }

    #[test]
    fn is_service_like_type_rejects_plain_data_types() {
        assert!(!super::is_service_like_type("Step"));
        assert!(!super::is_service_like_type("User"));
        assert!(!super::is_service_like_type("Request"));
        assert!(!super::is_service_like_type("Response"));
        assert!(!super::is_service_like_type("Config"));
    }
}
