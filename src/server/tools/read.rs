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

#[tool_router(router = qartez_read_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_read",
        description = "Read one or more symbols' source code from disk with line numbers. Faster than Read — jumps directly to the symbol without scanning. Pass `symbol_name` for a single symbol, or `symbols=[...]` to batch-fetch multiple in one call. Use file_path to disambiguate. Passing just `file_path` (no symbol) reads the whole file or a slice via start_line/end_line/limit — replaces the built-in Read for module headers, imports, and small files.",
        annotations(
            title = "Read Symbol",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_read(
        &self,
        Parameters(params): Parameters<SoulReadParams>,
    ) -> Result<String, String> {
        // Reject contradictory requests up front. The schema advertises
        // `symbol_name` OR `symbols`, not both; silently ignoring one
        // when the other is set made multi-arg calls impossible to
        // debug. Empty placeholders on either field are still tolerated
        // so clients that always pass a default shape stay compatible.
        let both_set = matches!(&params.symbol_name, Some(s) if !s.is_empty())
            && matches!(&params.symbols, Some(v) if !v.is_empty());
        if both_set {
            return Err(
                "Pass either `symbol_name` OR `symbols=[...]`, not both. They were both provided; the combination is ambiguous."
                    .to_string(),
            );
        }

        // Reject `max_bytes=0` up front. Every rendered section is at
        // least one line long, so a zero cap deterministically produces
        // a "truncated at 0-byte cap" payload that wastes a round trip.
        // Non-zero caps below the section size simply render fewer
        // sections, which is a legitimate budget and not a bug.
        if matches!(params.max_bytes, Some(0)) {
            return Err(
                "`max_bytes=0` is not a useful budget (every rendered section is at least a few lines). Pass a positive byte cap, or omit `max_bytes` for the default 25 KiB."
                    .to_string(),
            );
        }

        // Reject `start_line=0` explicitly. Schema documents it as
        // 1-based; silently coercing 0 to 1 masked off-by-one bugs in
        // caller scripts so the observed "first line" semantics
        // depended on whether the caller had read the docstring.
        if matches!(params.start_line, Some(0)) {
            return Err(
                "`start_line` is 1-based. Use 1 for the first line or omit the parameter."
                    .to_string(),
            );
        }

        // Floor explicitly-requested `max_bytes` below 256 up to the
        // minimum that can render the "Lx-y shown" header plus at
        // least one content line. Without the floor, `max_bytes=1`
        // rendered a header and a truncation marker with zero actual
        // body lines - a payload that read as a pipeline bug. The
        // caller's "tight budget" intent is preserved via a note.
        const MIN_USEFUL_MAX_BYTES: usize = 256;
        let (max_bytes, max_bytes_note) = match params.max_bytes {
            Some(user_value) if (user_value as usize) < MIN_USEFUL_MAX_BYTES => (
                MIN_USEFUL_MAX_BYTES,
                Some(format!(
                    "// note: max_bytes={user_value} raised to {MIN_USEFUL_MAX_BYTES} (minimum to render one line with header)\n",
                )),
            ),
            Some(v) => (v as usize, None),
            None => (25_000usize, None),
        };
        let context_lines = params.context_lines.unwrap_or(0) as usize;

        // Raw file-range mode: file_path given without any symbol. Dumps the
        // whole file by default, or a specific slice when start_line/end_line/
        // limit are set. Saves callers from falling back to the built-in Read
        // tool for imports, module headers, small files, or whole-file scans.
        let no_symbols_requested = params.symbol_name.as_deref().is_none_or(|s| s.is_empty())
            && params.symbols.as_ref().is_none_or(|v| v.is_empty());
        if no_symbols_requested && let Some(ref fp) = params.file_path {
            // `limit` is documented as an alternative to `end_line`. Accepting
            // all three at once lets callers write unresolvable specifications
            // (e.g. `start=5 end=5 limit=10`). Reject the combination so the
            // mistake is visible immediately.
            if params.start_line.is_some() && params.end_line.is_some() && params.limit.is_some() {
                return Err(
                    "`limit` is mutually exclusive with `end_line`: pass `start_line + end_line` OR `start_line + limit`, not all three."
                        .to_string(),
                );
            }
            // Enforce start<=end ordering BEFORE any file-length check so the
            // error wording is independent of the target file size. Only run
            // when both are explicit (both > 0); unset/0 values resolve to
            // defaults inside `read_file_slice`.
            if let (Some(s), Some(e)) = (params.start_line, params.end_line)
                && s > 0
                && e > 0
                && s > e
            {
                return Err(format!("start_line ({s}) > end_line ({e})"));
            }
            let mut body = self.read_file_slice(
                fp,
                params.start_line,
                params.end_line,
                params.limit,
                max_bytes,
            )?;
            if let Some(note) = max_bytes_note {
                body.insert_str(0, &note);
            }
            return Ok(body);
        }

        let queries = parse_symbol_queries(params.symbols, params.symbol_name)?;

        // Normalize the file_path filter so Windows callers can pass either
        // separator style and still substring-match forward-slash DB keys.
        let file_filter: Option<String> = params
            .file_path
            .as_deref()
            .map(|s| crate::index::to_forward_slash(s.to_string()));

        let mut body =
            self.read_symbol_batch(&queries, file_filter.as_deref(), max_bytes, context_lines)?;
        if let Some(note) = max_bytes_note {
            body.insert_str(0, &note);
        }
        Ok(body)
    }

    /// Raw file-range read used when no symbol name is supplied. Returns
    /// the whole file or a `start_line..=end_line` slice (with optional
    /// `limit` lines from `start_line`). Honors the same `max_bytes` cap
    /// as symbol mode and emits an inline truncation marker when the cap
    /// is hit so callers know they need to page.
    fn read_file_slice(
        &self,
        fp: &str,
        start_line: Option<u32>,
        end_line: Option<u32>,
        limit: Option<u32>,
        max_bytes: usize,
    ) -> Result<String, String> {
        let abs_path = self.safe_resolve(fp)?;
        if looks_binary(&abs_path) {
            return Err(format!(
                "{fp} appears to be binary; qartez_read supports text only"
            ));
        }
        let source = std::fs::read_to_string(&abs_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                // Never echo `abs_path` when the file is missing - the
                // workspace root leaks `/Users/<name>/...` into whatever
                // client the caller is piping output into.
                format!(
                    "{fp} not found in project root. Check the path is relative to the project root and the file exists.",
                )
            } else if e.kind() == std::io::ErrorKind::InvalidData
                || e.to_string().contains("did not contain valid UTF-8")
            {
                format!("{fp} appears to be binary; qartez_read supports text only")
            } else {
                format!("Cannot read {fp}: {e}")
            }
        })?;
        let lines: Vec<&str> = source.lines().collect();
        let total_lines = lines.len();
        if total_lines == 0 {
            return Ok(format!("{fp} (empty file)\n"));
        }

        // Resolve the requested range. `limit` mirrors the built-in Read
        // tool: read `limit` lines starting at `start_line` (defaults to
        // 1). When none of start_line/end_line/limit are set, the whole
        // file is returned — max_bytes still bounds the output so huge
        // files don't blow the response budget.
        let mut start = start_line.unwrap_or(0);
        let mut end = end_line.unwrap_or(0);
        if let Some(lim) = limit
            && lim > 0
        {
            if start == 0 {
                start = 1;
            }
            if end == 0 {
                end = start.saturating_add(lim - 1);
            }
        }
        if start == 0 {
            start = 1;
        }
        if end == 0 {
            end = total_lines as u32;
        }
        let start_idx = (start as usize).saturating_sub(1);
        if start_idx >= total_lines {
            return Err(format!(
                "start_line ({start}) exceeds file length ({total_lines})"
            ));
        }
        if start > end {
            return Err(format!("start_line ({start}) > end_line ({end})"));
        }
        let end_idx = (end as usize).min(total_lines);

        let mut body = String::new();
        let mut truncated_at: Option<usize> = None;
        let mut last_written_line: Option<usize> = None;
        // Reserve a conservative header budget so the final response still
        // fits under max_bytes after we stamp the real range on top. The
        // header is bounded by file-path length + a few digits, so 160 is
        // plenty in practice.
        let header_reserve = 160usize;
        let body_cap = max_bytes.saturating_sub(header_reserve);
        for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
            let formatted = format!("{:>4} | {}\n", start_idx + i + 1, line);
            if body.len() + formatted.len() > body_cap {
                truncated_at = Some(start_idx + i);
                break;
            }
            body.push_str(&formatted);
            last_written_line = Some(start_idx + i + 1);
        }
        let shown_end = last_written_line.unwrap_or(end_idx);
        let header = if truncated_at.is_some() {
            format!(
                "{fp} L{start}-{shown_end} shown, full range L{start}-{end_idx} (total lines: {total_lines})\n",
            )
        } else {
            format!("{fp} L{start}-{shown_end}\n")
        };
        let mut out = header;
        out.push_str(&body);
        if let Some(cut) = truncated_at {
            out.push_str(&format!(
                "// ... (truncated at line {}, response reached {max_bytes}-byte cap; raise `max_bytes` or page with `start_line`/`limit`)\n",
                cut + 1,
            ));
        }
        Ok(out)
    }

    /// Symbol-mode read for one or many names. Resolves each query to its
    /// matching `(symbol, file)` rows in a single pass, batches the
    /// blast-radius lookup, then renders sections honoring the byte cap.
    /// Missing names are reported as a trailing comment instead of erroring
    /// out, so partial-hit batches still return useful output.
    fn read_symbol_batch(
        &self,
        queries: &[String],
        file_filter: Option<&str>,
        max_bytes: usize,
        context_lines: usize,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;

        // Two-pass: first resolve each query to its matching (symbol, file)
        // tuples, then batch the blast-radius lookup for only the files that
        // actually matched. Prevents an O(V*(V+E)) full sweep for every
        // invocation when batch mode often involves 1–5 files.
        let mut per_query: Vec<(
            usize,
            Vec<(
                crate::storage::models::SymbolRow,
                crate::storage::models::FileRow,
            )>,
        )> = Vec::with_capacity(queries.len());
        let mut missing: Vec<String> = Vec::new();
        for (idx, query) in queries.iter().enumerate() {
            let results = match read::find_symbol_by_name(&conn, query) {
                Ok(r) => r,
                Err(e) => return Err(format!("DB error: {e}")),
            };
            let filtered: Vec<_> = if let Some(fp) = file_filter {
                results
                    .into_iter()
                    .filter(|(_, file)| file.path.contains(fp))
                    .collect()
            } else {
                results
            };
            if filtered.is_empty() {
                missing.push(query.clone());
            } else {
                per_query.push((idx, filtered));
            }
        }

        let mut match_file_ids: Vec<i64> = per_query
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|(_, f)| f.id))
            .collect();
        match_file_ids.sort_unstable();
        match_file_ids.dedup();
        let blast_radii = blast::blast_radius_for_files(&conn, &match_file_ids).unwrap_or_default();
        drop(conn);

        let total_symbols: usize = per_query.iter().map(|(_, f)| f.len()).sum();
        let mut out = String::new();

        // Ambiguity policy.
        //
        // 2-4 matching files with no `file_path` filter emit a
        // warning and return every hit concatenated - common dual-
        // impl Rust patterns (e.g. `fn new` on `Foo` + `Bar`) stay
        // convenient.
        //
        // >= 5 matching files with no `file_path` filter refuses up
        // front. At that scale the concatenation burns many thousand
        // tokens and the refactor tools already require disambiguation
        // for the same name, so reading should not silently disagree.
        const MAX_IMPLICIT_AMBIGUOUS_FILES: usize = 4;
        if file_filter.is_none() {
            for (_idx, filtered) in &per_query {
                let mut seen_files: std::collections::BTreeSet<&str> =
                    std::collections::BTreeSet::new();
                for (_, file) in filtered.iter() {
                    seen_files.insert(file.path.as_str());
                }
                if seen_files.len() > MAX_IMPLICIT_AMBIGUOUS_FILES
                    && let Some((sym, _)) = filtered.first()
                {
                    let joined: Vec<String> = seen_files.iter().map(|s| (*s).to_string()).collect();
                    return Err(format!(
                        "Refusing to read '{}': {} distinct files define this name. Pass `file_path=<one-of>` to pick one:\n  {}\nThis matches the refactor-tool policy so `qartez_read` does not silently spray the same name across tiers.",
                        sym.name,
                        seen_files.len(),
                        joined.join("\n  "),
                    ));
                }
                if seen_files.len() >= 2
                    && let Some((sym, _)) = filtered.first()
                {
                    let joined: Vec<String> = seen_files.iter().map(|s| (*s).to_string()).collect();
                    out.push_str(&format!(
                        "// warning: symbol '{}' defined in {} files: {}. Pass file_path=<one-of> to disambiguate.\n",
                        sym.name,
                        seen_files.len(),
                        joined.join(", "),
                    ));
                }
            }
            if !out.is_empty() {
                out.push('\n');
            }
        }

        let mut rendered_any = false;
        let mut rendered_count: usize = 0;
        let mut truncated = false;

        'outer: for (_idx, filtered) in &per_query {
            for (sym, file) in filtered {
                let section = self.render_symbol_section(sym, file, context_lines, &blast_radii)?;

                // Stop before writing if this section would push us past the
                // cap. We still include at least one full section even if it
                // exceeds the budget alone - truncating a symbol mid-line is
                // worse than returning a single over-budget response. We key
                // off `rendered_any` (not `out.is_empty()`) so ambiguity
                // warnings prepended above do not short-circuit the first
                // section's "always render at least one" rule.
                if rendered_any && out.len() + section.len() > max_bytes {
                    truncated = true;
                    break 'outer;
                }
                out.push_str(&section);
                rendered_any = true;
                rendered_count += 1;
            }
        }

        if !rendered_any {
            // Single-name lookups keep the hard error so callers see a
            // clean `Err` instead of an empty payload; the query is a
            // point lookup and a 0-hit result is unambiguous. Batch
            // lookups (`symbols=[...]`) are lenient: partial-hit batches
            // already emit a `(N not found: ...)` notice, so total-miss
            // batches follow the same shape instead of flipping to Err.
            // This makes `symbols=[missing]` and `symbols=[A, missing]`
            // behave the same from a scripting caller's point of view.
            if queries.len() == 1 {
                let name = &queries[0];
                if let Some(fp) = file_filter {
                    return Err(format!("No symbol '{name}' found in file matching '{fp}'"));
                }
                return Err(format!("No symbol found with name '{name}'"));
            }
            let joined = queries.join(", ");
            let header = if let Some(fp) = file_filter {
                format!("No symbols [{joined}] found in file matching '{fp}'\n")
            } else {
                format!("No symbol found with name(s) [{joined}]\n")
            };
            let mut out = header;
            out.push_str(&format!(
                "// ({} not found: {})\n",
                missing.len(),
                missing.join(", ")
            ));
            return Ok(out);
        }

        if !missing.is_empty() {
            out.push_str(&format!(
                "// ({} not found: {})\n",
                missing.len(),
                missing.join(", ")
            ));
        }

        if truncated {
            let remaining = total_symbols.saturating_sub(rendered_count);
            out.push_str(&format!(
                "// ... (truncated: {remaining} symbol(s) skipped, response reached {max_bytes}-byte cap)\n",
            ));
        }

        Ok(out)
    }

    /// Render one `(symbol, file)` pair as the "// + name kind @ path:..."
    /// header followed by its source lines (with `context_lines` of leading
    /// context). Reads the file from disk; the caller owns the byte-budget
    /// decision of whether to keep this section.
    fn render_symbol_section(
        &self,
        sym: &crate::storage::models::SymbolRow,
        file: &crate::storage::models::FileRow,
        context_lines: usize,
        blast_radii: &HashMap<i64, i64>,
    ) -> Result<String, String> {
        let abs_path = self.safe_resolve(&file.path)?;
        if looks_binary(&abs_path) {
            return Err(format!(
                "{} appears to be binary; qartez_read supports text only",
                file.path
            ));
        }
        let source = std::fs::read_to_string(&abs_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                // Same rule as `read_file_slice`: never leak the
                // absolute workspace path when the file disappeared
                // between index time and read time.
                format!(
                    "{} not found in project root. Check the path is relative to the project root and the file exists.",
                    file.path
                )
            } else if e.kind() == std::io::ErrorKind::InvalidData
                || e.to_string().contains("did not contain valid UTF-8")
            {
                format!(
                    "{} appears to be binary; qartez_read supports text only",
                    file.path
                )
            } else {
                format!("Cannot read {}: {e}", file.path)
            }
        })?;

        let lines: Vec<&str> = source.lines().collect();
        // Expand the window by `context_lines` on the start side;
        // the end side is the symbol's real terminator (symbols
        // are closed units, rarely useful to trail beyond them).
        let sym_start = (sym.line_start as usize).saturating_sub(1);
        let start = sym_start.saturating_sub(context_lines);
        let end = (sym.line_end as usize).min(lines.len());

        let visibility = if sym.is_exported { "+" } else { "-" };
        let blast_r = blast_radii.get(&file.id).copied().unwrap_or(0);

        // Compact single-line header: marker name kind @ path:Lstart-end →blast
        // Replaces the old two-line `// name — kind (visibility) →X\n// path [Lx-Ly]`
        // format. Saves ~12 tokens per symbol; still carries every
        // field a caller needs.
        let mut section = format!(
            "// {visibility} {} {} @ {}:L{}-{} →{}\n",
            sym.name, sym.kind, file.path, sym.line_start, sym.line_end, blast_r,
        );
        for (i, line) in lines[start..end].iter().enumerate() {
            section.push_str(&format!("{:>4} | {}\n", start + i + 1, line));
        }
        section.push('\n');
        Ok(section)
    }
}

/// Pick the caller-requested query list. Batch mode takes priority when
/// both fields are set, so a caller migrating from single → batch does
/// not have to clear `symbol_name` explicitly. Empty strings in the list
/// are dropped as no-ops rather than erroring, so callers can freely
/// splat variable-length arrays.
/// Decide whether `path` points at binary content before we try to pull
/// it into a `String`. A two-layer probe: (1) extension blacklist for
/// the common visual / archive / compiled asset types, (2) NUL-byte scan
/// of the first 8 KiB, which is the same heuristic `git` and `grep -I`
/// use. Keeps us from leaking UTF-8 decode errors to callers and lets us
/// return a human-friendly "file is binary" message instead.
fn looks_binary(path: &std::path::Path) -> bool {
    const BINARY_EXTS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "bmp", "tiff", "ico", "webp", "pdf", "zip", "gz", "tgz",
        "bz2", "xz", "7z", "rar", "tar", "exe", "dll", "so", "dylib", "a", "lib", "class", "o",
        "wasm", "jar", "mp3", "mp4", "mov", "avi", "mkv", "webm", "ogg", "flac", "wav", "woff",
        "woff2", "ttf", "otf", "eot", "db", "sqlite", "bin",
    ];
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && BINARY_EXTS
            .iter()
            .any(|e| e.eq_ignore_ascii_case(ext.trim_start_matches('.')))
    {
        return true;
    }
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    let mut buf = [0u8; 8192];
    let n = f.read(&mut buf).unwrap_or(0);
    buf[..n].contains(&0u8)
}

fn parse_symbol_queries(
    symbols: Option<Vec<String>>,
    symbol_name: Option<String>,
) -> Result<Vec<String>, String> {
    let queries: Vec<String> = match (symbols, symbol_name) {
        (Some(list), _) if !list.is_empty() => list.into_iter().filter(|s| !s.is_empty()).collect(),
        (_, Some(name)) if !name.is_empty() => vec![name],
        _ => {
            return Err(
                "Either `symbol_name` or a non-empty `symbols` list is required".to_string(),
            );
        }
    };
    if queries.is_empty() {
        return Err("No non-empty symbol names provided".to_string());
    }
    Ok(queries)
}
