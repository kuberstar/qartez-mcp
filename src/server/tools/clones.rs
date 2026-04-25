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

#[tool_router(router = qartez_clones_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_clones",
        description = "Detect duplicate code: groups of symbols with identical structural shape (same AST skeleton after normalizing identifiers, literals, and comments). Each group is a refactoring opportunity — extract the common logic into a shared function. Use min_lines to filter out trivial matches. Test files and inline `#[cfg(test)]` modules are excluded by default (parallel parser-fixture tests share AST shapes on purpose); set `include_tests=true` to scan them too.",
        annotations(
            title = "Code Clone Detection",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_clones(
        &self,
        Parameters(params): Parameters<SoulClonesParams>,
    ) -> Result<String, String> {
        // `qartez_clones` does not render Mermaid graphs; reject the
        // shared `format=mermaid` value so the contract matches every
        // other non-graph tool (qartez_smells, qartez_hotspots, etc).
        // Without this guard the caller silently received the default
        // text output and saw what looked like a no-op format change.
        reject_mermaid(&params.format, "qartez_clones")?;
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        // `limit=0` previously coerced to 1 via `.max(1)`, which
        // silently shaped the caller's `limit=0` intent into a
        // one-row response. Reject explicitly so `qartez_clones`
        // speaks the same contract as `qartez_cochange` and the
        // newly-aligned `qartez_unused`.
        if let Some(0) = params.limit {
            return Err(
                "limit must be > 0 (use a positive integer; there is no 'no-cap' mode).".into(),
            );
        }
        let limit = params.limit.unwrap_or(20) as i64;
        let offset = params.offset.unwrap_or(0) as i64;
        // `min_lines=0` is meaningless: every symbol has `line_end >=
        // line_start` so the SQL predicate `(line_end - line_start + 1)
        // >= 0` matches every indexed symbol, inflating the raw count but
        // producing nothing actionable after the group-by + HAVING filter.
        // Silently coercing to 1 hid the caller's intent; reject with a
        // clear message instead.
        if let Some(0) = params.min_lines {
            return Err(
                "min_lines must be >= 1 (0 matches every symbol and produces nothing useful after the duplicate-group filter).".into(),
            );
        }
        // Default raised from 5 to 8 because short dispatch boilerplate
        // (e.g. 37 parallel `fn parse_X(source) -> Tree` helpers that all
        // wrap a tree-sitter parser on a language-specific `LANGUAGE`
        // constant) dominated the top groups without being refactorable -
        // each call site binds a LANGUAGE from a different crate and
        // cannot collapse into a single generic helper without a typeid
        // map. Callers who still want the aggressive cutoff pass
        // `min_lines=5` explicitly.
        let min_lines = params.min_lines.unwrap_or(8);
        let include_tests = params.include_tests.unwrap_or(false);
        let concise = matches!(params.format, Some(Format::Concise));

        let total =
            read::count_clone_groups(&conn, min_lines).map_err(|e| format!("DB error: {e}"))?;
        if total == 0 {
            // Name the filter instead of claiming the project has no
            // clones: `min_lines=8` (default) and `min_lines=100000`
            // produce the same "no clones detected" string otherwise,
            // which hides 293 real groups behind an unrelated assertion
            // about structural uniqueness.
            return Ok(format!(
                "No clone groups at min_lines={min_lines}. Lower the threshold (e.g. min_lines=5) to widen the search, or pass include_tests=true if the expected clones live in test files."
            ));
        }

        // Default behaviour mirrors `qartez_security`: drop symbols whose
        // file path looks like a test file and symbols whose line range
        // sits inside a Rust `#[cfg(test)] mod tests {}` block. Parallel
        // parser-fixture tests (21+ near-identical `test_module` /
        // `test_simple_function` functions in `src/index/languages/*.rs`)
        // are AST-shape-identical by design and dominate the top groups
        // without being refactorable; keep them out of the default view
        // so real production duplicates surface. Pass
        // `include_tests=true` to restore the old behaviour.
        //
        // When the test filter is active we oversample the DB: a narrow
        // `LIMIT N OFFSET M` query against raw clone groups returned
        // empty pages for small `limit` values when the top groups were
        // all tests. Loop-fetch pages of FETCH_PAGE_SIZE until we have
        // `limit` post-filter groups or the source is exhausted. The
        // reported "total" still reflects the raw group count so the
        // pagination contract stays compatible with older callers.
        let cfg_test_cache = CfgTestBlockCache::new(&self.project_root);
        const FETCH_PAGE_SIZE: i64 = 64;
        let mut groups: Vec<read::CloneGroup> = Vec::new();
        let mut fetch_offset = offset;
        'batches: loop {
            let batch = read::get_clone_groups(&conn, min_lines, FETCH_PAGE_SIZE, fetch_offset)
                .map_err(|e| format!("DB error: {e}"))?;
            if batch.is_empty() {
                break;
            }
            let batch_len = batch.len() as i64;
            fetch_offset += batch_len;
            // Iterate per-row so we can stop exactly at the row that
            // fills `limit`. Before, the loop truncated groups AFTER
            // extending them with the full batch, which silently
            // skipped the remaining candidates on the follow-up page.
            for g in batch {
                let kept = if include_tests {
                    (!is_entry_point_boilerplate(&self.project_root, &g)).then_some(g)
                } else {
                    filter_test_members(g, &cfg_test_cache)
                        .filter(|g| !is_entry_point_boilerplate(&self.project_root, g))
                };
                if let Some(kept) = kept {
                    groups.push(kept);
                    if groups.len() as i64 >= limit {
                        break 'batches;
                    }
                }
            }
            if batch_len < FETCH_PAGE_SIZE {
                break;
            }
        }
        // Pagination contract: `next_offset = offset + limit` mirrors
        // the rest of the tool surface (qartez_unused, qartez_health,
        // qartez_refs). The previous scheme reported `offset +
        // raw_consumed_to_last_kept` which jumped ahead by 64 even
        // when only 2 post-filter groups were kept, hiding the
        // remaining candidates behind a "next: offset=8" instead of
        // "next: offset=4".
        let next_raw_offset = offset + limit;

        if groups.is_empty() {
            return Ok(format!(
                "No clones in page (total={total}, offset={offset})."
            ));
        }

        let shown = groups.len() as i64;
        let mut out = if shown < total {
            format!(
                "{total} clone group(s) (min {min_lines} lines); showing {shown} from offset {offset} (next: offset={next_raw_offset}).\n\n",
            )
        } else {
            format!("{total} clone group(s) (min {min_lines} lines).\n\n")
        };

        let total_dup_symbols: usize = groups.iter().map(|g| g.symbols.len()).sum();
        out.push_str(&format!(
            "{total_dup_symbols} duplicate symbols across {shown} group(s).\n\n"
        ));

        for (i, group) in groups.iter().enumerate() {
            let group_num = offset as usize + i + 1;
            let size = group.symbols.len();
            let lines = group
                .symbols
                .first()
                .map(|(s, _)| s.line_end.saturating_sub(s.line_start) + 1)
                .unwrap_or(0);
            let boilerplate = detect_trait_boilerplate(&conn, group);

            if concise {
                out.push_str(&format!("#{group_num} ({size}x, ~{lines}L"));
                if boilerplate.is_some() {
                    out.push_str(", trait-boilerplate");
                }
                out.push_str("):");
                for (sym, file) in &group.symbols {
                    out.push_str(&format!(" {}:{}", file.path, sym.line_start));
                }
                out.push('\n');
            } else {
                out.push_str(&format!(
                    "## Clone group #{group_num} - {size} duplicates, ~{lines} lines each\n"
                ));
                if let Some(ref label) = boilerplate {
                    let method_name = group
                        .symbols
                        .first()
                        .map(|(s, _)| s.name.as_str())
                        .unwrap_or("method");
                    let trait_hint = label
                        .trait_name
                        .as_deref()
                        .map(|t| format!(" ({t})"))
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "[trait boilerplate{trait_hint} - candidate for default method]\n"
                    ));
                    if label.default_method_viable {
                        out.push_str(&format!(
                            "  Consider promoting `fn {method_name}` to a default \
                             method on the trait so impls opt out only when needed.\n"
                        ));
                    } else {
                        // Bodies diverge on per-impl literals (e.g. each
                        // impl returns a different string constant), so a
                        // single default-method body cannot replace them.
                        // Point callers at the remaining refactor lever
                        // instead of the incorrect default-method advice.
                        out.push_str(&format!(
                            "  Per-impl literals differ across members, so `fn {method_name}` \
                             cannot collapse into a single default method. Consider lifting the \
                             varying value into an associated constant or a trait-method return \
                             that each impl overrides.\n"
                        ));
                    }
                }
                for (sym, file) in &group.symbols {
                    let kind_char = sym.kind.chars().next().unwrap_or(' ');
                    out.push_str(&format!(
                        "  {kind_char} {} @ {} L{}-{}\n",
                        sym.name, file.path, sym.line_start, sym.line_end,
                    ));
                }
                out.push('\n');
            }
        }
        Ok(out)
    }
}

/// Verdict for a clone group that matches the trait-boilerplate pattern.
///
/// `trait_name` is populated when every member resolves to the same
/// `impl <Trait> for <Struct>` block via `type_hierarchy`. When the
/// enclosing trait cannot be resolved cheaply the proxy heuristic
/// (same method name, distinct owner types, distinct files, method
/// kind on every member) still fires with `trait_name = None`.
///
/// `default_method_viable` is `false` when the bodies diverge on
/// per-impl literals that cannot be captured by a single default
/// method body. For example, `fn language_name() -> &str { "bash" }`
/// and `fn language_name() -> &str { "rust" }` share an AST skeleton
/// but each returns a distinct string literal; promoting to a default
/// on the trait would require picking one literal for all impls, which
/// collapses semantics. The report suppresses the "promote to default
/// method" suggestion in that case.
struct TraitBoilerplate {
    trait_name: Option<String>,
    default_method_viable: bool,
}

/// Detect clone groups that are trait-impl boilerplate and recommend a
/// default method implementation instead.
///
/// A group qualifies when every member is a method sharing the same
/// base name, every member lives in a distinct file, and every member
/// has a distinct `owner_type`. When all members also share a single
/// `super_name` in `type_hierarchy`, that trait name is returned
/// alongside the verdict so the report can name it explicitly.
fn detect_trait_boilerplate(
    conn: &rusqlite::Connection,
    group: &read::CloneGroup,
) -> Option<TraitBoilerplate> {
    if group.symbols.len() < 2 {
        return None;
    }

    let first_name = &group.symbols[0].0.name;
    let mut seen_files: HashSet<i64> = HashSet::new();
    let mut seen_owners: HashSet<String> = HashSet::new();
    for (sym, file) in &group.symbols {
        // Every member must be a method/function: trait impls surface
        // as `method` on Rust-style backends and `function` on some
        // other language backends.
        let is_method = matches!(sym.kind.as_str(), "method" | "function");
        if !is_method {
            return None;
        }
        if &sym.name != first_name {
            return None;
        }
        let owner = sym.owner_type.as_deref()?;
        if !seen_files.insert(file.id) {
            return None;
        }
        if !seen_owners.insert(owner.to_string()) {
            return None;
        }
    }

    // The proxy heuristic holds. Try to recover the enclosing trait
    // name by intersecting `type_hierarchy` rows for every
    // (owner_type, file_id) pair. A single shared `super_name` means
    // every member sits inside `impl <Trait> for <Struct>` with the
    // same trait; anything else falls back to the proxy label.
    let trait_name = intersect_trait_names(conn, &group.symbols).ok().flatten();
    let default_method_viable = bodies_agree_on_literals(conn, group);
    Some(TraitBoilerplate {
        trait_name,
        default_method_viable,
    })
}

/// Returns `true` when every member of `group` has a body whose
/// literal tokens (strings, numbers) match across impls, i.e. a
/// single default method body could replace every override without
/// losing information. Returns `false` when at least one literal
/// differs between members: the classic `LanguageSupport` case where
/// `language_name` returns `"bash"` in one impl and `"rust"` in
/// another. In that case the bodies share an AST skeleton but the
/// constants they carry are load-bearing, so suggesting "promote to
/// default method" is incorrect advice.
///
/// When the body text cannot be fetched for any member (missing
/// `symbols_body_fts` row on a legacy index) we conservatively return
/// `true` - falling back to the historical behaviour - because the
/// comparison cannot be performed.
fn bodies_agree_on_literals(conn: &rusqlite::Connection, group: &read::CloneGroup) -> bool {
    let mut first: Option<Vec<String>> = None;
    for (sym, _) in &group.symbols {
        let Some(body) = super::smells::fetch_symbol_body(conn, sym.id) else {
            return true;
        };
        let lits = extract_body_literals(&body);
        match &first {
            None => first = Some(lits),
            Some(prev) if prev == &lits => {}
            Some(_) => return false,
        }
    }
    true
}

/// Extract the ordered list of literal tokens (quoted strings, byte
/// strings, numeric literals) from a body string. We match lexically
/// rather than via tree-sitter because the clone detector already
/// guarantees AST shape equality - any remaining difference must
/// live in the lexical stream.
fn extract_body_literals(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            // String literal: capture contents (including the closing
            // quote) up to the matching unescaped double quote.
            let start = i;
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                if c == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if c == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(String::from_utf8_lossy(&bytes[start..i]).into_owned());
            continue;
        }
        if b.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.') {
                i += 1;
            }
            out.push(String::from_utf8_lossy(&bytes[start..i]).into_owned());
            continue;
        }
        i += 1;
    }
    out
}

/// Intersect `type_hierarchy.super_name` rows across every group member.
///
/// Returns `Ok(Some(trait))` when exactly one trait is shared by all
/// members, `Ok(None)` when the intersection is empty or ambiguous, and
/// `Err` when the SQL query fails. Each lookup is scoped to the
/// member's `(file_id, sub_name)` so implementations of the same trait
/// in unrelated files do not cross-pollinate.
#[allow(clippy::question_mark)]
fn intersect_trait_names(
    conn: &rusqlite::Connection,
    symbols: &[(
        crate::storage::models::SymbolRow,
        crate::storage::models::FileRow,
    )],
) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT super_name
         FROM type_hierarchy
         WHERE sub_name = ?1 AND file_id = ?2 AND kind = 'implements'",
    )?;

    let mut common: Option<HashSet<String>> = None;
    for (sym, file) in symbols {
        // `?` on `Option` would short-circuit this `Result<Option<_>>`
        // function to `Ok(Some(None))`, not `Ok(None)`, so keep the
        // explicit `let...else` here.
        let Some(owner) = sym.owner_type.as_deref() else {
            return Ok(None);
        };
        let rows = stmt
            .query_map(rusqlite::params![owner, file.id], |row| {
                row.get::<_, String>(0)
            })?
            .filter_map(|r| r.ok())
            .collect::<HashSet<_>>();
        if rows.is_empty() {
            return Ok(None);
        }
        common = Some(match common {
            Some(prev) => prev.intersection(&rows).cloned().collect(),
            None => rows,
        });
        if common.as_ref().is_some_and(|s| s.is_empty()) {
            return Ok(None);
        }
    }

    match common {
        Some(set) if set.len() == 1 => Ok(set.into_iter().next()),
        _ => Ok(None),
    }
}

/// Lazy per-file cache of Rust `#[cfg(test)]` block line ranges. Clone
/// groups often cite the same file more than once (parser-fixture files
/// contain many parallel test functions) and tree-sitter parsing is the
/// dominant cost here, so cache the result keyed by relative path.
struct CfgTestBlockCache<'a> {
    project_root: &'a std::path::Path,
    inner: std::cell::RefCell<HashMap<String, Vec<(u32, u32)>>>,
}

impl<'a> CfgTestBlockCache<'a> {
    fn new(project_root: &'a std::path::Path) -> Self {
        Self {
            project_root,
            inner: std::cell::RefCell::new(HashMap::new()),
        }
    }

    /// Return cached `(start_line, end_line)` ranges for every
    /// `#[cfg(test)] mod ...` block in `rel_path`. Non-Rust files,
    /// unreadable files, and files with no inline test modules cache as
    /// an empty vector so each path parses at most once per call.
    fn ranges_for(&self, rel_path: &str, language: &str) -> Vec<(u32, u32)> {
        if language != "rust" {
            return Vec::new();
        }
        if let Some(cached) = self.inner.borrow().get(rel_path) {
            return cached.clone();
        }
        let abs = self.project_root.join(rel_path);
        let ranges = std::fs::read_to_string(&abs)
            .ok()
            .map(|src| crate::graph::security::find_cfg_test_blocks(&src))
            .unwrap_or_default();
        self.inner
            .borrow_mut()
            .insert(rel_path.to_string(), ranges.clone());
        ranges
    }
}

/// Returns `true` when every member of a clone group is a shallow
/// dispatcher entry-point that wraps a language-specific constant
/// with otherwise-identical AST shape - the kind of parallel
/// `fn parse_foo() -> Tree { dispatch::parse_generic(SRC, "foo") }`
/// pattern that dominates qartez's parser-fixture layer and is not
/// mechanically refactorable without a typeid map.
///
/// Three gates must all pass:
///   1. Every member shares a common alphanumeric prefix of >= 4
///      characters ending in `_` (e.g. `parse_`, `extract_`,
///      `handle_`). Shorter shared prefixes trigger too many false
///      positives on incidental naming collisions.
///   2. Every member's body has at most MAX_ENTRY_POINT_LINES
///      non-comment lines after stripping the signature and closing
///      brace.
///   3. Every body literally mentions its per-member stem (either
///      as an identifier or as a string literal). Bodies that do
///      unique per-member work will not carry the stem, so this
///      gate reliably distinguishes parallel entry-points from
///      genuine parallel duplicates.
///
/// Heuristic is conservative: any member that fails any gate keeps
/// the group visible, so genuine 40-line duplicates sharing a prefix
/// still surface as refactor candidates. This handles Issue 18
/// (40-way `parse_<lang>` group) without suppressing real tech debt.
fn is_entry_point_boilerplate(project_root: &std::path::Path, group: &read::CloneGroup) -> bool {
    const MAX_ENTRY_POINT_LINES: usize = 2;

    if group.symbols.len() < 2 {
        return false;
    }
    // All members must be free functions - methods with an owner type
    // already participate in the trait-boilerplate detector.
    if group
        .symbols
        .iter()
        .any(|(s, _)| s.kind != "function" || s.owner_type.is_some())
    {
        return false;
    }
    let names: Vec<&str> = group.symbols.iter().map(|(s, _)| s.name.as_str()).collect();
    let prefix = common_prefix(&names);
    // Require a strong shared prefix (>= 4 ascii chars, ending in
    // `_`). Shorter values like `do_` are common accidents that
    // should not trigger suppression.
    if prefix.len() < 4 || !prefix.ends_with('_') {
        return false;
    }
    let mut stems: Vec<String> = Vec::new();
    for n in &names {
        let stem = n.strip_prefix(prefix).unwrap_or(n);
        if stem.is_empty() || !stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
        stems.push(stem.to_string());
    }

    // Gate 2 + gate 3: each body is tiny AND mentions the stem.
    // Read each source file at most once.
    let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();
    for ((sym, file), stem) in group.symbols.iter().zip(stems.iter()) {
        let body_lines = file_cache.entry(file.path.clone()).or_insert_with(|| {
            let abs = project_root.join(&file.path);
            std::fs::read_to_string(&abs)
                .ok()
                .map(|s| s.lines().map(str::to_string).collect())
                .unwrap_or_default()
        });
        let start = sym.line_start as usize;
        let end = sym.line_end as usize;
        if start == 0 || start > body_lines.len() {
            return false;
        }
        let end = end.min(body_lines.len());
        let span = &body_lines[start - 1..end];
        let effective_lines = span
            .iter()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with("//")
            })
            .count();
        // Subtract 2 for the function signature + closing brace so
        // the comparison is against pure body content.
        let body_only = effective_lines.saturating_sub(2);
        if body_only > MAX_ENTRY_POINT_LINES {
            return false;
        }
        // Gate 3: the body must literally mention the member's stem.
        // Accept either an identifier mention (`dispatch::foo`) or a
        // string-literal mention (`dispatch::parse("foo")`).
        let body_text = span.join("\n");
        if !body_text.contains(stem.as_str()) {
            return false;
        }
    }
    true
}

/// Longest leading substring shared by every entry in `names`. Returns
/// an empty slice when the input is empty or shares no common start.
fn common_prefix<'a>(names: &[&'a str]) -> &'a str {
    let Some(first) = names.first() else {
        return "";
    };
    let mut end = first.len();
    for n in names.iter().skip(1) {
        end = end.min(n.len());
        let a = first.as_bytes();
        let b = n.as_bytes();
        let mut i = 0;
        while i < end && a[i] == b[i] {
            i += 1;
        }
        end = i;
        if end == 0 {
            break;
        }
    }
    &first[..end]
}

/// Drop every member of `group` whose file path looks like a test file or
/// whose line range sits inside a Rust `#[cfg(test)] mod tests {}` block.
/// Returns `None` when fewer than two distinct spans survive (a clone
/// group needs at least two).
fn filter_test_members(
    group: read::CloneGroup,
    cfg_test_cache: &CfgTestBlockCache<'_>,
) -> Option<read::CloneGroup> {
    let read::CloneGroup {
        shape_hash,
        symbols,
    } = group;
    let kept: Vec<_> = symbols
        .into_iter()
        .filter(|(sym, file)| {
            if helpers::is_test_path(&file.path) {
                return false;
            }
            let ranges = cfg_test_cache.ranges_for(&file.path, &file.language);
            !ranges
                .iter()
                .any(|(s, e)| sym.line_start >= *s && sym.line_end <= *e)
        })
        .collect();
    let distinct_spans: HashSet<(i64, u32, u32)> = kept
        .iter()
        .map(|(sym, _)| (sym.file_id, sym.line_start, sym.line_end))
        .collect();
    if distinct_spans.len() < 2 {
        return None;
    }
    Some(read::CloneGroup {
        shape_hash,
        symbols: kept,
    })
}
