use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    Annotated, ErrorData, GetPromptRequestParams, GetPromptResult, Implementation,
    ListPromptsResult, ListResourcesResult, PaginatedRequestParams, RawResource,
    ReadResourceRequestParams, ReadResourceResult, ResourceContents, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, prompt_handler, tool, tool_handler, tool_router};

mod prompts;
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::graph::blast;
use crate::guard;
use crate::index::languages;
use crate::storage::read;
use crate::toolchain;

/// Tolerant deserializers for MCP tool parameters.
///
/// Some MCP clients (notably Claude Code as of 2026-04) serialize numeric
/// and boolean tool arguments as JSON strings before forwarding them over
/// the JSON-RPC bridge — e.g. `{"limit":"30"}` instead of `{"limit":30}`.
/// Strict serde then rejects the string where a `u32` or `bool` is expected,
/// which surfaces to the user as a cryptic `invalid type: string "30"` error.
///
/// These helpers accept either the native JSON form or the stringified form
/// and produce the same value. `schemars` still emits `{"type":"integer"}`
/// / `{"type":"boolean"}` in the tool schema, so well-behaved clients are
/// unaffected.
mod flexible {
    use serde::{Deserialize, Deserializer, de::Error};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum U32OrStr {
        Num(u32),
        Str(String),
    }

    pub(super) fn u32_opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
        match Option::<U32OrStr>::deserialize(d)? {
            None => Ok(None),
            Some(U32OrStr::Num(n)) => Ok(Some(n)),
            Some(U32OrStr::Str(s)) => s
                .parse::<u32>()
                .map(Some)
                .map_err(|e| D::Error::custom(format!("expected u32, got \"{s}\": {e}"))),
        }
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BoolOrStr {
        Bool(bool),
        Str(String),
    }

    pub(super) fn bool_opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
        match Option::<BoolOrStr>::deserialize(d)? {
            None => Ok(None),
            Some(BoolOrStr::Bool(b)) => Ok(Some(b)),
            Some(BoolOrStr::Str(s)) => match s.as_str() {
                "true" | "True" | "TRUE" | "1" => Ok(Some(true)),
                "false" | "False" | "FALSE" | "0" => Ok(Some(false)),
                _ => Err(D::Error::custom(format!("expected bool, got \"{s}\""))),
            },
        }
    }
}

/// Per-file tree-sitter parse cache, keyed by relative path. Each entry is
/// mtime-stamped so a stale file triggers reparse on next access.
///
/// Why: `qartez_rename` and `qartez_calls` both walk tree-sitter ASTs of large
/// files (e.g. `src/server/mod.rs`, ~2300 lines of Rust). A cold parse of
/// that file runs in the 3-6 ms range, and walking it for call names is
/// another ~0.5 ms. On repeated invocations (benchmark warmup + measured
/// runs, multi-file renames that revisit the definition file, depth-2 call
/// hierarchies), those costs dominate. Caching source / tree / call sites
/// turns the steady-state cost into a HashMap lookup plus a shallow clone
/// of `Arc` handles.
///
/// Fields are populated lazily: a caller that only needs the raw source
/// (e.g. the text prefilter in `qartez_calls`) does not pay the parse cost,
/// and a caller that needs pre-extracted call sites gets them walked once
/// per file lifetime.
#[derive(Default)]
struct ParseCache {
    entries: HashMap<String, ParseEntry>,
}

/// Per-file identifier map keyed by identifier text. Each occurrence is
/// `(row, start_byte, end_byte)`.
type IdentMap = HashMap<String, Vec<(usize, usize, usize)>>;

#[derive(Default)]
struct ParseEntry {
    mtime_ns: i64,
    source: Option<Arc<String>>,
    tree: Option<Arc<tree_sitter::Tree>>,
    calls: Option<Arc<Vec<(String, usize)>>>,
    /// Full name→occurrences map built from a single AST walk. Used by
    /// `qartez_rename` to skip the per-name walk on repeat invocations.
    idents: Option<Arc<IdentMap>>,
}

#[derive(Clone)]
pub struct QartezServer {
    db: Arc<Mutex<Connection>>,
    project_root: PathBuf,
    git_depth: u32,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    parse_cache: Arc<Mutex<ParseCache>>,
}

impl QartezServer {
    pub fn new(conn: Connection, project_root: PathBuf, git_depth: u32) -> Self {
        // Self-heal the body FTS index. Existing `.qartez/index.db` files
        // built before the schema-migration fix have an empty
        // `symbols_body_fts` because the old migration wiped it on every
        // open. qartez_refs and qartez_rename need it populated to find call
        // sites in files with no direct import edge (external-crate `use`,
        // Rust module-form `use` resolving to `mod.rs`, child modules via
        // `use super::*;`). A one-time rebuild on startup is cheap — it
        // reads each indexed file once and inserts a row per symbol body
        // — and subsequent opens short-circuit via the count check.
        let body_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols_body_fts", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);
        let symbol_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .unwrap_or(0);
        if body_count == 0
            && symbol_count > 0
            && let Err(e) = crate::storage::write::rebuild_symbol_bodies(&conn, &project_root)
        {
            tracing::warn!("failed to rebuild symbols_body_fts on server start: {e}");
        }

        Self {
            db: Arc::new(Mutex::new(conn)),
            project_root,
            git_depth,
            tool_router: Self::tool_router(),
            parse_cache: Arc::new(Mutex::new(ParseCache::default())),
        }
    }

    /// Acquire the server's shared SQLite connection under its mutex.
    ///
    /// Added for the benchmark harness's grounding verifier (slice B of
    /// `docs/benchmark-v2/PLAN.md`), which needs a `&Connection` to call
    /// `storage::read::get_file_by_path` and `find_symbol_by_name` from
    /// outside the server's own tool handlers. The guard's lifetime is
    /// tied to `&self` so the borrow checker keeps it from outliving the
    /// server. Panics on lock poison, matching `M-PANIC-ON-BUG`: a poisoned
    /// lock indicates a prior panic inside a server method, which is an
    /// unrecoverable programming error rather than a recoverable I/O failure.
    #[allow(dead_code)]
    pub fn db_connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().expect("qartez db mutex poisoned")
    }

    /// Clone the shared database handle for use by background tasks (e.g.
    /// the file watcher). The returned `Arc` shares the same connection and
    /// mutex as the server's own tool handlers.
    pub fn db_arc(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.db)
    }

    /// Read a file's mtime in nanoseconds, or `None` if the file is missing
    /// or the filesystem does not expose a modification time.
    fn file_mtime_ns(&self, rel_path: &str) -> Option<i64> {
        let abs_path = self.project_root.join(rel_path);
        std::fs::metadata(&abs_path)
            .ok()?
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| i64::try_from(d.as_nanos()).unwrap_or(0))
    }

    /// Return the cached source for `rel_path`, reading and caching it on
    /// the first miss. Returns `None` only when the file cannot be read.
    ///
    /// Used by the `qartez_calls` callers-loop prefilter: reading is cheap,
    /// parsing is not, so we check whether the identifier is textually
    /// present before committing to a parse.
    fn cached_source(&self, rel_path: &str) -> Option<Arc<String>> {
        let mtime_ns = self.file_mtime_ns(rel_path)?;
        if let Ok(cache) = self.parse_cache.lock()
            && let Some(entry) = cache.entries.get(rel_path)
            && entry.mtime_ns == mtime_ns
            && let Some(src) = &entry.source
        {
            return Some(src.clone());
        }

        let abs_path = self.project_root.join(rel_path);
        let raw = std::fs::read_to_string(&abs_path).ok()?;
        let arc = Arc::new(raw);

        if let Ok(mut cache) = self.parse_cache.lock() {
            let entry = cache.entries.entry(rel_path.to_string()).or_default();
            if entry.mtime_ns != mtime_ns {
                // File changed since the last cached extract — drop stale
                // per-file artifacts and re-key the entry.
                *entry = ParseEntry::default();
                entry.mtime_ns = mtime_ns;
            }
            entry.source = Some(arc.clone());
        }
        Some(arc)
    }

    /// Return a parsed tree-sitter tree for `rel_path`, along with its
    /// source bytes. Parses on first miss, returns from cache on subsequent
    /// calls with matching mtime.
    ///
    /// Returns `None` when the file cannot be read, the extension has no
    /// language support, or the parse itself fails. Callers must fall back
    /// to non-AST paths in those cases.
    fn cached_tree(&self, rel_path: &str) -> Option<(Arc<String>, Arc<tree_sitter::Tree>)> {
        let mtime_ns = self.file_mtime_ns(rel_path)?;
        if let Ok(cache) = self.parse_cache.lock()
            && let Some(entry) = cache.entries.get(rel_path)
            && entry.mtime_ns == mtime_ns
            && let (Some(src), Some(tree)) = (&entry.source, &entry.tree)
        {
            return Some((src.clone(), tree.clone()));
        }

        let source_arc = self.cached_source(rel_path)?;
        let ext = rel_path.rsplit('.').next().unwrap_or("");
        let lang_support = languages::get_language_for_ext(ext)?;
        let ts_lang = lang_support.tree_sitter_language(ext);
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).ok()?;
        let tree = parser.parse(source_arc.as_bytes(), None)?;
        let tree_arc = Arc::new(tree);

        if let Ok(mut cache) = self.parse_cache.lock() {
            let entry = cache.entries.entry(rel_path.to_string()).or_default();
            if entry.mtime_ns != mtime_ns {
                *entry = ParseEntry::default();
                entry.mtime_ns = mtime_ns;
                entry.source = Some(source_arc.clone());
            }
            entry.tree = Some(tree_arc.clone());
        }
        Some((source_arc, tree_arc))
    }

    /// Return the per-file identifier map for `rel_path`, keyed by
    /// identifier text. Walks the cached tree exactly once per file
    /// mtime; subsequent lookups (any name, any tool) are just a
    /// `HashMap::get`.
    ///
    /// Returns `None` when the file cannot be parsed (unsupported
    /// language or read error). Callers must fall back to a word-boundary
    /// scan in that case.
    fn cached_idents(&self, rel_path: &str) -> Option<Arc<IdentMap>> {
        let mtime_ns = self.file_mtime_ns(rel_path)?;
        if let Ok(cache) = self.parse_cache.lock()
            && let Some(entry) = cache.entries.get(rel_path)
            && entry.mtime_ns == mtime_ns
            && let Some(idents) = &entry.idents
        {
            return Some(idents.clone());
        }

        let (source_arc, tree_arc) = self.cached_tree(rel_path)?;
        let mut map: IdentMap = HashMap::new();
        collect_identifiers_grouped(&mut tree_arc.walk(), source_arc.as_bytes(), &mut map);
        let arc = Arc::new(map);

        if let Ok(mut cache) = self.parse_cache.lock()
            && let Some(entry) = cache.entries.get_mut(rel_path)
        {
            entry.idents = Some(arc.clone());
        }
        Some(arc)
    }

    /// Return the pre-extracted call sites for `rel_path`. Walks the cached
    /// tree on first miss, returns the cached vector on subsequent calls.
    /// Returns an empty vector when the file cannot be parsed.
    fn cached_calls(&self, rel_path: &str) -> Arc<Vec<(String, usize)>> {
        if let Some(mtime_ns) = self.file_mtime_ns(rel_path)
            && let Ok(cache) = self.parse_cache.lock()
            && let Some(entry) = cache.entries.get(rel_path)
            && entry.mtime_ns == mtime_ns
            && let Some(calls) = &entry.calls
        {
            return calls.clone();
        }

        let Some((source_arc, tree_arc)) = self.cached_tree(rel_path) else {
            return Arc::new(Vec::new());
        };
        let mut results = Vec::new();
        collect_call_names(&mut tree_arc.walk(), source_arc.as_bytes(), &mut results);
        let arc = Arc::new(results);

        if let Ok(mut cache) = self.parse_cache.lock()
            && let Some(entry) = cache.entries.get_mut(rel_path)
        {
            entry.calls = Some(arc.clone());
        }
        arc
    }

    fn project_name(&self) -> String {
        self.project_root
            .canonicalize()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .or_else(|| {
                self.project_root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Render a symbol-centric overview: top symbols by symbol-level
    /// PageRank, grouped by their defining file. Invoked from `qartez_map`
    /// when the caller passes `by=symbols`. Designed to be a drop-in
    /// replacement for the file-ranked overview on the same token budget,
    /// so the output structure intentionally mirrors `build_overview`'s
    /// header/table shape.
    fn build_symbol_overview(&self, top_n: i64, token_budget: usize, concise: bool) -> String {
        let conn = match self.db.lock() {
            Ok(c) => c,
            Err(e) => return format!("DB lock error: {e}"),
        };
        let file_count = read::get_file_count(&conn).unwrap_or(0);
        let symbol_count = read::get_symbol_count(&conn).unwrap_or(0);
        let effective_limit = if top_n == i64::MAX { 1000 } else { top_n };

        let symbols = match read::get_symbols_ranked(&conn, effective_limit) {
            Ok(s) => s,
            Err(e) => return format!("Error reading symbols: {e}"),
        };

        // If the symbol PageRank column is entirely zero (legacy DB, or no
        // symbol_refs were extracted for this language), fall back to the
        // file-ranked view with a short explanatory note rather than
        // returning a section of zero-rank junk.
        let any_nonzero = symbols.iter().any(|(s, _)| s.pagerank > 0.0);
        if !any_nonzero {
            let mut out = String::from(
                "# Symbol PageRank unavailable\n\
                 No symbol-level PageRank data in the index. This is expected for \
                 languages without a reference extractor yet (see docs) or DBs that \
                 predate symbol PageRank. Falling back to file ranking.\n\n",
            );
            out.push_str(&self.build_overview(top_n, token_budget, None, None, concise, false));
            return out;
        }

        let mut out = String::new();
        if concise {
            out.push_str(&format!(
                "{} files, {} symbols (rank name kind file PR)\n",
                file_count, symbol_count,
            ));
        } else {
            out.push_str(&format!(
                "# Codebase: {} ({} files, {} symbols indexed) — by symbols\n\n",
                self.project_name(),
                file_count,
                symbol_count,
            ));
            out.push_str(" # | Symbol                         | Kind       | File                               | PageRank\n");
            out.push_str("---+--------------------------------+------------+------------------------------------+---------\n");
        }

        for (i, (sym, file)) in symbols.iter().enumerate() {
            if sym.pagerank <= 0.0 {
                // Skip the trailing tail of unranked symbols — they carry no
                // signal and only bloat the output.
                break;
            }
            let line = if concise {
                format!(
                    "{} {} {} {} {:.4}\n",
                    i + 1,
                    sym.name,
                    sym.kind,
                    file.path,
                    sym.pagerank,
                )
            } else {
                format!(
                    "{:>2} | {:<30} | {:<10} | {:<34} | {:>8.4}\n",
                    i + 1,
                    truncate_path(&sym.name, 30),
                    truncate_path(&sym.kind, 10),
                    truncate_path(&file.path, 34),
                    sym.pagerank,
                )
            };
            if estimate_tokens(&out) + estimate_tokens(&line) > token_budget {
                break;
            }
            out.push_str(&line);
        }

        out
    }

    fn build_overview(
        &self,
        top_n: i64,
        token_budget: usize,
        boost_files: Option<&[String]>,
        boost_terms: Option<&[String]>,
        concise: bool,
        all_files: bool,
    ) -> String {
        let conn = match self.db.lock() {
            Ok(c) => c,
            Err(e) => return format!("DB lock error: {e}"),
        };
        let file_count = read::get_file_count(&conn).unwrap_or(0);
        let symbol_count = read::get_symbol_count(&conn).unwrap_or(0);

        let has_boosts = boost_files.is_some() || boost_terms.is_some();
        let fetch_limit = if has_boosts {
            top_n.saturating_mul(3)
        } else {
            top_n
        };

        let mut files = if all_files {
            match read::get_all_files_ranked(&conn) {
                Ok(f) => f,
                Err(e) => return format!("Error reading files: {e}"),
            }
        } else {
            match read::get_files_ranked(&conn, fetch_limit) {
                Ok(f) => f,
                Err(e) => return format!("Error reading files: {e}"),
            }
        };

        if !all_files && has_boosts {
            let mut boosted_ids: HashSet<i64> = HashSet::new();

            if let Some(paths) = boost_files {
                for file in &files {
                    for bp in paths {
                        if file.path.contains(bp.as_str()) {
                            boosted_ids.insert(file.id);
                        }
                    }
                }
            }

            if let Some(terms) = boost_terms {
                for term in terms {
                    let fts_query = if term.contains('*') {
                        term.clone()
                    } else {
                        format!("{term}*")
                    };
                    if let Ok(ids) = read::search_file_ids_by_fts(&conn, &fts_query) {
                        for id in ids {
                            boosted_ids.insert(id);
                        }
                    }
                }
            }

            if !boosted_ids.is_empty() {
                for file in &mut files {
                    if boosted_ids.contains(&file.id) {
                        file.pagerank *= 10.0;
                    }
                }
                files.sort_by(|a, b| {
                    b.pagerank
                        .partial_cmp(&a.pagerank)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }

            files.truncate(top_n as usize);
        }

        let visible_ids: Vec<i64> = files.iter().map(|f| f.id).collect();
        let blast_radii = blast::blast_radius_for_files(&conn, &visible_ids).unwrap_or_default();

        let mut out = String::new();
        if concise {
            // Terse header: no project name, no ASCII table framing. Columns are
            // positional — rank, path, pagerank, exports, blast radius — and
            // labeled once in the hint so callers know the schema without
            // paying per-row framing bytes. The arrow on the blast column is
            // read as "this file ripples out to N files downstream".
            out.push_str(&format!(
                "{} files, {} symbols (rank path PR exp →blast)\n",
                file_count, symbol_count,
            ));
        } else {
            out.push_str(&format!(
                "# Codebase: {} ({} files, {} symbols indexed)\n\n",
                self.project_name(),
                file_count,
                symbol_count,
            ));
            out.push_str(" # | File                                | PageRank | Exports | Blast\n");
            out.push_str("---+-------------------------------------+----------+---------+------\n");
        }

        let mut file_symbols: Vec<(String, Vec<crate::storage::models::SymbolRow>)> = Vec::new();

        for (i, file) in files.iter().enumerate() {
            let symbols = read::get_symbols_for_file(&conn, file.id).unwrap_or_default();
            let export_count = symbols.iter().filter(|s| s.is_exported).count();
            let blast_r = blast_radii.get(&file.id).copied().unwrap_or(0);

            let line = if concise {
                format!(
                    "{} {} {:.3} {} →{}\n",
                    i + 1,
                    file.path,
                    file.pagerank,
                    export_count,
                    blast_r,
                )
            } else {
                format!(
                    "{:>2} | {:<35} | {:>8.4} | {:>7} | →{}\n",
                    i + 1,
                    truncate_path(&file.path, 35),
                    file.pagerank,
                    export_count,
                    blast_r,
                )
            };

            if estimate_tokens(&out) + estimate_tokens(&line) > token_budget {
                break;
            }
            out.push_str(&line);
            file_symbols.push((file.path.clone(), symbols));
        }

        if !concise {
            out.push('\n');
        }

        if !concise {
            for (path, symbols) in &file_symbols {
                let exported: Vec<&crate::storage::models::SymbolRow> =
                    symbols.iter().filter(|s| s.is_exported).collect();
                if exported.is_empty() {
                    continue;
                }

                let section_header = format!("## {path}\n");
                if estimate_tokens(&out) + estimate_tokens(&section_header) > token_budget {
                    break;
                }
                out.push_str(&section_header);

                let remaining = token_budget.saturating_sub(estimate_tokens(&out));
                if let Some(elided) =
                    elide_file_source(&self.project_root, path, symbols, remaining)
                {
                    out.push_str(&elided);
                } else {
                    for sym in &exported {
                        let fallback = format!("{} {}", sym.kind, sym.name);
                        let sig = sym.signature.as_deref().unwrap_or(&fallback);
                        let line = format!("  + {sig}\n");
                        if estimate_tokens(&out) + estimate_tokens(&line) > token_budget {
                            break;
                        }
                        out.push_str(&line);
                    }
                }
                out.push('\n');
            }
        }

        out
    }
}

fn elide_file_source(
    project_root: &std::path::Path,
    file_path: &str,
    symbols: &[crate::storage::models::SymbolRow],
    token_budget_remaining: usize,
) -> Option<String> {
    let abs_path = project_root.join(file_path);
    let source = std::fs::read_to_string(&abs_path).ok()?;
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return None;
    }

    let mut sorted: Vec<&crate::storage::models::SymbolRow> =
        symbols.iter().filter(|s| s.is_exported).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by_key(|s| s.line_start);

    let mut out = String::new();
    let mut last_shown_line: usize = 0;

    for sym in &sorted {
        let start = (sym.line_start as usize).saturating_sub(1);
        let end = (sym.line_end as usize).min(lines.len());
        if start >= lines.len() || start >= end {
            continue;
        }

        if start > last_shown_line + 1 {
            out.push_str("⋯\n");
        }

        let body_kinds = ["function", "method", "constructor"];
        if body_kinds.contains(&sym.kind.as_str()) {
            let sym_text: String = lines[start..end].join("\n");
            if let Some(brace_pos) = sym_text.find('{') {
                let before = sym_text[..brace_pos].trim_end();
                out.push_str(before);
                out.push_str(" {⋯}\n");
            } else {
                out.push_str(lines[start]);
                out.push_str(" {⋯}\n");
            }
        } else {
            let span = end - start;
            if span <= 5 {
                for line in &lines[start..end] {
                    out.push_str(line);
                    out.push('\n');
                }
            } else {
                for line in &lines[start..(start + 2).min(end)] {
                    out.push_str(line);
                    out.push('\n');
                }
                out.push_str("    ⋯\n");
                if end > 0 {
                    out.push_str(lines[end - 1]);
                    out.push('\n');
                }
            }
        }

        last_shown_line = end;

        if estimate_tokens(&out) > token_budget_remaining {
            out.push_str("⋯\n");
            break;
        }
    }

    if !out.is_empty() { Some(out) } else { None }
}

fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        path.to_string()
    } else if max_len <= 3 {
        path[..path.floor_char_boundary(max_len)].to_string()
    } else {
        let start = path.floor_char_boundary(path.len() - (max_len - 3));
        format!("...{}", &path[start..])
    }
}

fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

fn human_bytes(bytes: i64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let n = bytes as f64;
    if n >= GB {
        format!("{:.1}G", n / GB)
    } else if n >= MB {
        format!("{:.1}M", n / MB)
    } else if n >= KB {
        format!("{:.1}K", n / KB)
    } else {
        format!("{bytes}B")
    }
}

/// Per-signal contribution to a `qartez_context` candidate score. Kept as a
/// separate struct (rather than a flat f64) so `explain=true` can print the
/// breakdown and callers can reason about why a given file ranked where it did.
#[derive(Debug, Default, Clone)]
struct ScoreBreakdown {
    imports: f64,
    importer: f64,
    cochange: f64,
    transitive: f64,
    task_match: f64,
}

impl ScoreBreakdown {
    fn total(&self) -> f64 {
        self.imports + self.importer + self.cochange + self.transitive + self.task_match
    }

    fn reasons(&self) -> Vec<&'static str> {
        let mut r = Vec::new();
        if self.imports > 0.0 {
            r.push("imports");
        }
        if self.importer > 0.0 {
            r.push("importer");
        }
        if self.cochange > 0.0 {
            r.push("cochange");
        }
        if self.transitive > 0.0 {
            r.push("transitive");
        }
        if self.task_match > 0.0 {
            r.push("task-match");
        }
        r
    }

    fn explain(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.imports > 0.0 {
            parts.push(format!("imports={:.1}", self.imports));
        }
        if self.importer > 0.0 {
            parts.push(format!("importer={:.1}", self.importer));
        }
        if self.cochange > 0.0 {
            parts.push(format!("cochange={:.1}", self.cochange));
        }
        if self.transitive > 0.0 {
            parts.push(format!("transitive={:.1}", self.transitive));
        }
        if self.task_match > 0.0 {
            parts.push(format!("task-match={:.1}", self.task_match));
        }
        parts.join(" + ")
    }
}

fn replace_whole_word(text: &str, old: &str, new: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut start = 0;
    while let Some(pos) = text[start..].find(old) {
        let abs_pos = start + pos;
        let before_ok = if abs_pos == 0 {
            true
        } else {
            let ch = text[..abs_pos].chars().next_back().unwrap();
            !ch.is_alphanumeric() && ch != '_'
        };
        let after_pos = abs_pos + old.len();
        let after_ok = if after_pos >= text.len() {
            true
        } else {
            let ch = text[after_pos..].chars().next().unwrap();
            !ch.is_alphanumeric() && ch != '_'
        };

        if before_ok && after_ok {
            result.push_str(&text[start..abs_pos]);
            result.push_str(new);
        } else {
            result.push_str(&text[start..abs_pos + old.len()]);
        }
        start = after_pos;
    }
    result.push_str(&text[start..]);
    result
}

const IDENTIFIER_NODE_KINDS: &[&str] = &[
    "identifier",
    "type_identifier",
    "field_identifier",
    "property_identifier",
    "simple_identifier",
    "shorthand_property_identifier_pattern",
    "shorthand_property_identifier",
];

/// Walk the tree once and group every identifier occurrence by its source
/// text. Used to populate the cross-invocation identifier cache so later
/// `qartez_rename` calls turn into O(1) HashMap lookups.
fn collect_identifiers_grouped(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    results: &mut IdentMap,
) {
    loop {
        let node = cursor.node();
        if IDENTIFIER_NODE_KINDS.contains(&node.kind())
            && let Ok(text) = node.utf8_text(source)
        {
            let line = node.start_position().row + 1;
            results.entry(text.to_string()).or_default().push((
                line,
                node.start_byte(),
                node.end_byte(),
            ));
        }

        if cursor.goto_first_child() {
            collect_identifiers_grouped(cursor, source, results);
            cursor.goto_parent();
        }

        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

/// Output verbosity for query tools. Encoded as a proper JSON Schema enum so
/// clients see the allowed values at tool-listing time instead of having to
/// try-and-fail on a free-form string.
#[derive(Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Format {
    #[default]
    Detailed,
    Concise,
}

/// Toolchain command selector for `qartez_project`. `Info` is the default so a
/// caller can probe the detected toolchain with a bare `qartez_project({})`.
#[derive(Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum ProjectAction {
    #[default]
    Info,
    Run,
    Test,
    Build,
    Lint,
    Typecheck,
}

/// Call hierarchy direction for `qartez_calls`. `Both` is the default because it
/// is the most useful on a cold exploration.
#[derive(Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum CallDirection {
    Callers,
    Callees,
    #[default]
    Both,
}

fn is_concise(format: &Option<Format>) -> bool {
    matches!(format, Some(Format::Concise))
}

/// Convert a user-supplied string into an FTS5-safe query.
///
/// FTS5 treats `#`, `(`, `)`, `:`, `^`, `"`, `[`, `]`, and `{`, `}` as syntax
/// and rejects them in bareword queries — so a caller asking for `#[tool` or
use crate::storage::read::sanitize_fts_query;

/// Walk up to `commit_limit` recent commits from HEAD, count co-change pairs
/// involving `target_path`, and return the top `limit` partners descending.
///
/// Commits touching more than `max_commit_size` files are skipped — they are
/// typically format passes, bulk renames, or lockfile bumps whose pair counts
/// drown the signal from real feature work.
///
/// Returns `None` only when git is unavailable in `project_root`.
fn compute_cochange_pairs(
    project_root: &std::path::Path,
    target_path: &str,
    max_commit_size: usize,
    commit_limit: usize,
    limit: usize,
) -> Option<Vec<(String, u32)>> {
    let repo = git2::Repository::open(project_root).ok()?;
    let head_oid = repo.head().ok()?.target()?;
    let mut revwalk = repo.revwalk().ok()?;
    revwalk.set_sorting(git2::Sort::TIME).ok()?;
    revwalk.push(head_oid).ok()?;

    let mut counts: HashMap<String, u32> = HashMap::new();
    for oid_result in revwalk.take(commit_limit) {
        let Ok(oid) = oid_result else { continue };
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        let Ok(tree) = commit.tree() else { continue };
        let parent_tree = if commit.parent_count() > 0 {
            commit.parent(0).ok().and_then(|p| p.tree().ok())
        } else {
            None
        };
        let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) else {
            continue;
        };
        let mut files: Vec<String> = diff
            .deltas()
            .filter_map(|d| {
                d.new_file()
                    .path()
                    .and_then(|p| p.to_str())
                    .map(|s| s.to_string())
            })
            .collect();
        files.sort();
        files.dedup();
        if files.len() < 2 || files.len() > max_commit_size {
            continue;
        }
        if !files.iter().any(|f| f == target_path) {
            continue;
        }
        for f in &files {
            if f != target_path {
                *counts.entry(f.clone()).or_insert(0) += 1;
            }
        }
    }

    let mut pairs: Vec<(String, u32)> = counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs.truncate(limit);
    Some(pairs)
}

/// Heuristic: return true for paths that look like test files (so they can be
/// excluded from blast radius and aggregate counts by default). Covers common
/// conventions across Rust, Go, TypeScript/JavaScript, Python, Java, and Ruby.
fn is_test_path(path: &str) -> bool {
    // Directory-based patterns (all languages)
    if path.starts_with("tests/")
        || path.starts_with("test/")
        || path.starts_with("benches/")
        || path.starts_with("__tests__/")
        || path.starts_with("spec/")
    {
        return true;
    }
    if path.contains("/tests/")
        || path.contains("/test/")
        || path.contains("/benches/")
        || path.contains("/__tests__/")
        || path.contains("/spec/")
    {
        return true;
    }
    if let Some(name) = path.rsplit('/').next() {
        // Rust: test.rs, tests.rs, _test.rs, _tests.rs
        if matches!(name, "test.rs" | "tests.rs") {
            return true;
        }
        if name.ends_with("_test.rs") || name.ends_with("_tests.rs") {
            return true;
        }
        // Go: _test.go
        if name.ends_with("_test.go") {
            return true;
        }
        // Dart: _test.dart
        if name.ends_with("_test.dart") {
            return true;
        }
        // TypeScript/JavaScript: .test.ts, .spec.ts, .test.tsx, .spec.tsx,
        //                        .test.js, .spec.js, .test.jsx, .spec.jsx
        if name.ends_with(".test.ts")
            || name.ends_with(".spec.ts")
            || name.ends_with(".test.tsx")
            || name.ends_with(".spec.tsx")
            || name.ends_with(".test.js")
            || name.ends_with(".spec.js")
            || name.ends_with(".test.jsx")
            || name.ends_with(".spec.jsx")
        {
            return true;
        }
        // Python: test_*.py, *_test.py
        if (name.starts_with("test_") && name.ends_with(".py")) || name.ends_with("_test.py") {
            return true;
        }
        // Java/Kotlin: *Test.java, *Tests.java, *Test.kt, *Tests.kt
        if name.ends_with("Test.java")
            || name.ends_with("Tests.java")
            || name.ends_with("Test.kt")
            || name.ends_with("Tests.kt")
        {
            return true;
        }
        // Ruby: _spec.rb
        if name.ends_with("_spec.rb") {
            return true;
        }
        // C#: *Tests.cs, *Test.cs
        if name.ends_with("Test.cs") || name.ends_with("Tests.cs") {
            return true;
        }
    }
    false
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct QartezParams {
    #[schemars(
        description = "Number of top files to show (default: 20). Pass 0 or set all_files=true to return every file PageRank-sorted."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    top_n: Option<u32>,
    #[schemars(
        description = "If true, return all files sorted by PageRank (ignores top_n). Watch for token-budget truncation on large repos."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    all_files: Option<bool>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    token_budget: Option<u32>,
    #[schemars(
        description = "File paths to boost in ranking (e.g., recently edited or mentioned files)"
    )]
    boost_files: Option<Vec<String>>,
    #[schemars(description = "Search terms to boost files containing matching symbols")]
    boost_terms: Option<Vec<String>>,
    #[schemars(
        description = "'concise' = file list only, 'detailed' (default) = files + exported symbols"
    )]
    format: Option<Format>,
    #[schemars(
        description = "Ranking axis: 'files' (default) shows top files by PageRank; 'symbols' shows top symbols by symbol-level PageRank + their defining file."
    )]
    by: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulFindParams {
    #[schemars(
        description = "Exact symbol name to search for (or regex when regex=true). Accepts aliases `symbol`, `symbol_name`, and `query`."
    )]
    #[serde(alias = "symbol", alias = "symbol_name", alias = "query")]
    name: String,
    #[schemars(description = "Filter by symbol kind (function, struct, class, etc.)")]
    kind: Option<String>,
    #[schemars(
        description = "'concise' = name + file only, 'detailed' (default) = full info with signatures"
    )]
    format: Option<Format>,
    #[schemars(
        description = "If true, interpret `name` as a regex applied to indexed symbol names (anchored match semantics: `is_match`). Default false (exact)."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    regex: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulReadParams {
    #[schemars(
        description = "Name of a single symbol to read. For batch reads, use `symbols` instead. Accepts the aliases `symbol` and `name`."
    )]
    #[serde(alias = "symbol", alias = "name")]
    symbol_name: Option<String>,
    #[schemars(
        description = "Batch mode: list of symbol names to read in one call. Results are concatenated in order. Cheaper than multiple qartez_read calls. Either `symbols` or `symbol_name` must be set."
    )]
    symbols: Option<Vec<String>>,
    #[schemars(
        description = "Filter all symbols to a specific file path. When set without any symbol, reads the raw file contents — the whole file by default, or the slice defined by start_line/end_line/limit. max_bytes still bounds the output. Aliases: `file`, `path`."
    )]
    #[serde(alias = "file", alias = "path")]
    file_path: Option<String>,
    #[schemars(
        description = "Max response size in bytes (default: 25000). Symbols past the cap are omitted with a truncation marker."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    max_bytes: Option<u32>,
    #[schemars(
        description = "Lines of source context to include BEFORE the symbol's own range (default: 0). Use when you need to see surrounding use-blocks or comments."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    context_lines: Option<u32>,
    #[schemars(
        description = "Read partial body: 1-based start line. Combined with `end_line` or `limit`. When set together with `file_path` but without any symbol, dumps that raw line range from the file — lets you read non-symbol code (imports, module headers) without falling back to Read. Alias: `offset`."
    )]
    #[serde(default, alias = "offset", deserialize_with = "flexible::u32_opt")]
    start_line: Option<u32>,
    #[schemars(description = "Partial-body end line (inclusive). Pairs with `start_line`.")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    end_line: Option<u32>,
    #[schemars(
        description = "Partial-body line count. Alternative to `end_line`: when set, reads `limit` lines starting at `start_line` (defaults to 1). Mirrors the built-in Read tool's `limit` parameter so callers can copy-paste the same shape."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    limit: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulImpactParams {
    #[schemars(
        description = "Relative file path to analyze blast radius for. Aliases: `file`, `path`, `name`."
    )]
    #[serde(alias = "file", alias = "path", alias = "name")]
    file_path: String,
    #[schemars(description = "'concise' = counts only, 'detailed' (default) = full file lists")]
    format: Option<Format>,
    #[schemars(description = "Include test files in the transitive blast radius (default: false)")]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    include_tests: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulCochangeParams {
    #[schemars(
        description = "Relative file path to find co-change partners for. Aliases: `file`, `path`."
    )]
    #[serde(alias = "file", alias = "path")]
    file_path: String,
    #[schemars(description = "Max number of results (default: 10)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    limit: Option<u32>,
    #[schemars(
        description = "'concise' = file paths + counts only, 'detailed' (default) = table format"
    )]
    format: Option<Format>,
    #[schemars(
        description = "Skip commits touching more than this many files when recomputing pair counts from git (default: 30). Guards against huge refactor commits inflating counts."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    max_commit_size: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulGrepParams {
    #[schemars(
        description = "FTS5 search query (supports prefix* matching) or regex when regex=true. Accepts the alias `pattern` for parity with Grep."
    )]
    #[serde(alias = "pattern")]
    query: String,
    #[schemars(description = "Max number of results (default: 20)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    limit: Option<u32>,
    #[schemars(
        description = "'concise' = names only, 'detailed' (default) = names + kind + file + lines"
    )]
    format: Option<Format>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    token_budget: Option<u32>,
    #[schemars(
        description = "If true, interpret query as a regex applied to indexed symbol names (not bodies). Default false (FTS5 prefix)."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    regex: Option<bool>,
    #[schemars(
        description = "If true, search pre-indexed function bodies via FTS5 instead of symbol names. Useful for finding strings/comments/identifiers that don't appear in declarations. Default false."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    search_bodies: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulRefsParams {
    #[schemars(
        description = "Symbol name to find references for. Accepts aliases `name` and `symbol_name`."
    )]
    #[serde(alias = "name", alias = "symbol_name")]
    symbol: String,
    #[schemars(description = "Include transitive dependents (default: false)")]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    transitive: Option<bool>,
    #[schemars(
        description = "'concise' = file paths only, 'detailed' (default) = full import chain"
    )]
    format: Option<Format>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    token_budget: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulRenameParams {
    #[schemars(description = "Current symbol name to rename")]
    old_name: String,
    #[schemars(description = "New name for the symbol")]
    new_name: String,
    #[schemars(
        description = "If true, apply the rename. If false (default), show a preview of changes."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    apply: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulProjectParams {
    #[schemars(
        description = "Which project command to run. Defaults to `info`, which prints the detected toolchain without executing anything. `run` dry-prints the resolved command; the other variants execute it."
    )]
    action: Option<ProjectAction>,
    #[schemars(description = "Optional filter (e.g., test name pattern, specific package)")]
    filter: Option<String>,
    #[schemars(description = "Timeout in seconds (default: 60)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    timeout: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulMoveParams {
    #[schemars(description = "Symbol name to move. Accepts aliases `name` and `symbol_name`.")]
    #[serde(alias = "name", alias = "symbol_name")]
    symbol: String,
    #[schemars(
        description = "Target file path (relative to project root). Created if it doesn't exist."
    )]
    to_file: String,
    #[schemars(description = "If true, apply the move. If false (default), show a preview.")]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    apply: Option<bool>,
    #[schemars(
        description = "Disambiguate by symbol kind when the name is shared (e.g. 'function' vs 'method'). Accepts the kinds returned by qartez_find."
    )]
    kind: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulRenameFileParams {
    #[schemars(description = "Current file path (relative to project root)")]
    from: String,
    #[schemars(description = "New file path (relative to project root)")]
    to: String,
    #[schemars(description = "If true, apply the rename. If false (default), show a preview.")]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    apply: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulOutlineParams {
    #[schemars(description = "Relative file path to get outline for. Aliases: `file`, `path`.")]
    #[serde(alias = "file", alias = "path")]
    file_path: String,
    #[schemars(
        description = "'concise' = names + lines only, 'detailed' (default) = grouped by kind with signatures"
    )]
    format: Option<Format>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    token_budget: Option<u32>,
    #[schemars(
        description = "Skip the first N non-field symbols before rendering. Pair with token_budget to page through very large files."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    offset: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulDepsParams {
    #[schemars(
        description = "Relative file path to show dependencies for. Aliases: `file`, `path`."
    )]
    #[serde(alias = "file", alias = "path")]
    file_path: String,
    #[schemars(
        description = "'concise' = file paths only, 'detailed' (default) = paths + edge kinds"
    )]
    format: Option<Format>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    token_budget: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulStatsParams {
    #[schemars(
        description = "Optional relative file path for per-file stats: LOC, symbol count, imports, importers. Aliases: `file`, `path`."
    )]
    #[serde(alias = "file", alias = "path")]
    file_path: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulUnusedParams {
    #[schemars(
        description = "Max number of unused exports to return (default: 50). Pre-materialized at index time; paging through this list is O(1)."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    limit: Option<u32>,
    #[schemars(description = "Pagination offset into the unused-exports list (default: 0)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    offset: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulCallsParams {
    #[schemars(
        description = "Function/method name to analyze call hierarchy for. Accepts aliases `symbol` and `symbol_name`."
    )]
    #[serde(alias = "symbol", alias = "symbol_name")]
    name: String,
    #[schemars(
        description = "Which edges to walk. `both` is the default and shows callers and callees."
    )]
    direction: Option<CallDirection>,
    #[schemars(
        description = "Max depth for call chain traversal (default: 1). Pass 2 to also see transitive chains — this can emit many lines on hub functions."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    depth: Option<u32>,
    #[schemars(
        description = "'concise' = names only, 'detailed' (default) = with file paths and lines"
    )]
    format: Option<Format>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulContextParams {
    #[schemars(description = "File paths to analyze context for (files you plan to modify)")]
    files: Vec<String>,
    #[schemars(description = "Optional task description to help prioritize relevant context")]
    task: Option<String>,
    #[schemars(description = "Max number of context files to return (default: 15)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    limit: Option<u32>,
    #[schemars(
        description = "'concise' = file list only, 'detailed' (default) = files with reasons"
    )]
    format: Option<Format>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    token_budget: Option<u32>,
    #[schemars(
        description = "When true, show score breakdown per component (imports, importer, cochange, transitive, task-match) and count of files excluded by limit / budget. Use to diagnose why a file was or was not surfaced."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    explain: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulHotspotsParams {
    #[schemars(
        description = "Max number of hotspot results to return (default: 20). Hotspots are sorted by composite score = complexity × coupling × change_frequency."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    limit: Option<u32>,
    #[schemars(
        description = "Granularity: 'file' (default) ranks whole files, 'symbol' ranks individual functions/methods."
    )]
    level: Option<HotspotLevel>,
    #[schemars(
        description = "'concise' = compact table, 'detailed' (default) = full breakdown with per-metric scores"
    )]
    format: Option<Format>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum HotspotLevel {
    File,
    Symbol,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulClonesParams {
    #[schemars(
        description = "Max number of clone groups to return (default: 20). Groups are sorted by size (most duplicates first)."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    limit: Option<u32>,
    #[schemars(
        description = "Page offset for pagination — skip this many groups before returning (default: 0)."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    offset: Option<u32>,
    #[schemars(
        description = "Minimum number of source lines for a symbol to be considered (default: 5). Filters out trivial getters."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    min_lines: Option<u32>,
    #[schemars(
        description = "'concise' = compact list, 'detailed' (default) = grouped output with file paths and line ranges"
    )]
    format: Option<Format>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulWikiParams {
    #[schemars(
        description = "File path to write the wiki to (relative to project root). If omitted, returns the markdown inline."
    )]
    write_to: Option<String>,
    #[schemars(
        description = "Leiden resolution parameter (default: 1.0). Larger values produce more, smaller clusters; smaller values merge clusters."
    )]
    resolution: Option<f64>,
    #[schemars(
        description = "Minimum cluster size (default: 3). Clusters smaller than this are folded into the `misc` bucket."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    min_cluster_size: Option<u32>,
    #[schemars(
        description = "Max files listed per cluster section (default: 20). Remaining files are summarised as `... and N more`."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    max_files_per_section: Option<u32>,
    #[schemars(
        description = "Recompute clusters even if the file_clusters table is already populated (default: false)."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    recompute: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SoulBoundariesParams {
    #[schemars(
        description = "Path to the boundary config (TOML), relative to the project root. Defaults to `.qartez/boundaries.toml`."
    )]
    config_path: Option<String>,
    #[schemars(
        description = "If true, skip the checker and emit a starter config derived from the current Leiden clustering instead."
    )]
    #[serde(default, deserialize_with = "flexible::bool_opt")]
    suggest: Option<bool>,
    #[schemars(
        description = "When `suggest` is true and `write_to` is set, write the generated TOML to this path (relative to the project root) instead of returning it inline."
    )]
    write_to: Option<String>,
    #[schemars(
        description = "'concise' = one-line-per-violation summary, 'detailed' (default) = grouped output with rule text."
    )]
    format: Option<Format>,
}

#[tool_router(router = tool_router)]
impl QartezServer {
    #[tool(
        name = "qartez_map",
        description = "Start here. Returns the codebase skeleton: files ranked by importance (PageRank), their exports, and blast radii. Use boost_files/boost_terms to focus on areas relevant to your current task.",
        annotations(
            title = "Project Map",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn qartez_map(&self, Parameters(params): Parameters<QartezParams>) -> String {
        let requested_top = params.top_n.unwrap_or(20);
        let all_files = params.all_files.unwrap_or(false) || requested_top == 0;
        let top_n = if all_files {
            i64::MAX
        } else {
            requested_top as i64
        };
        let token_budget = params.token_budget.unwrap_or(4000) as usize;
        let concise = is_concise(&params.format);
        // `by=symbols` swaps the file ranking out for a symbol-level view.
        // Any other value (including the default) keeps the historical
        // file-ranked output — that path is the baseline every existing
        // benchmark scenario expects, so changing it silently would skew
        // regression reports.
        let by_symbols = params
            .by
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("symbols"))
            .unwrap_or(false);
        if by_symbols {
            return self.build_symbol_overview(top_n, token_budget, concise);
        }
        self.build_overview(
            top_n,
            token_budget,
            params.boost_files.as_deref(),
            params.boost_terms.as_deref(),
            concise,
            all_files,
        )
    }

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
    fn qartez_find(
        &self,
        Parameters(params): Parameters<SoulFindParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let use_regex = params.regex.unwrap_or(false);
        let results: Vec<(
            crate::storage::models::SymbolRow,
            crate::storage::models::FileRow,
        )> = if use_regex {
            let re = regex::Regex::new(&params.name).map_err(|e| format!("regex error: {e}"))?;
            // Walk every indexed symbol once and keep regex hits. Scales
            // linearly with corpus size — acceptable for the typical code
            // base range (≤100k symbols) since the match itself is cheap,
            // and exact-name lookups keep using the index.
            let all_paths: std::collections::HashMap<String, crate::storage::models::FileRow> =
                read::get_all_files(&conn)
                    .map_err(|e| format!("DB error: {e}"))?
                    .into_iter()
                    .map(|f| (f.path.clone(), f))
                    .collect();
            let all =
                read::get_all_symbols_with_path(&conn).map_err(|e| format!("DB error: {e}"))?;
            all.into_iter()
                .filter(|(s, _)| re.is_match(&s.name))
                .filter_map(|(s, p)| all_paths.get(&p).cloned().map(|f| (s, f)))
                .collect()
        } else {
            read::find_symbol_by_name(&conn, &params.name).map_err(|e| format!("DB error: {e}"))?
        };

        if results.is_empty() {
            return Ok(format!("No symbol found with name '{}'", params.name));
        }

        let filtered: Vec<_> = if let Some(ref kind) = params.kind {
            results
                .into_iter()
                .filter(|(sym, _)| sym.kind.eq_ignore_ascii_case(kind))
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
    fn qartez_read(
        &self,
        Parameters(params): Parameters<SoulReadParams>,
    ) -> Result<String, String> {
        // 25_000 bytes ≈ 6 KiB of tokens — a comfortable ceiling for two
        // or three mid-sized functions while still leaving headroom in a
        // 200k context window. Callers can raise it if they know they
        // want more.
        let max_bytes = params.max_bytes.unwrap_or(25_000) as usize;
        let context_lines = params.context_lines.unwrap_or(0) as usize;

        // Raw file-range mode: file_path given without any symbol. Dumps the
        // whole file by default, or a specific slice when start_line/end_line/
        // limit are set. Saves callers from falling back to the built-in Read
        // tool for imports, module headers, small files, or whole-file scans.
        let no_symbols_requested = params.symbol_name.as_deref().is_none_or(|s| s.is_empty())
            && params.symbols.as_ref().is_none_or(|v| v.is_empty());
        if no_symbols_requested && let Some(ref fp) = params.file_path {
            let abs_path = self.project_root.join(fp);
            let source = std::fs::read_to_string(&abs_path)
                .map_err(|e| format!("Cannot read {}: {e}", abs_path.display()))?;
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
            let mut start = params.start_line.unwrap_or(0);
            let mut end = params.end_line.unwrap_or(0);
            if let Some(lim) = params.limit
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

            let mut out = format!("{fp} L{start}-{end_idx}\n");
            let mut truncated_at: Option<usize> = None;
            for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
                let formatted = format!("{:>4} | {}\n", start_idx + i + 1, line);
                if out.len() + formatted.len() > max_bytes {
                    truncated_at = Some(start_idx + i);
                    break;
                }
                out.push_str(&formatted);
            }
            if let Some(cut) = truncated_at {
                out.push_str(&format!(
                    "// ... (truncated at line {}, response reached {max_bytes}-byte cap; raise `max_bytes` or page with `start_line`/`limit`)\n",
                    cut + 1,
                ));
            }
            return Ok(out);
        }

        // Build the caller-requested query list. Batch mode takes priority when
        // both fields are set, so a caller migrating from single → batch does
        // not have to clear `symbol_name` explicitly. Unknown-but-present empty
        // strings in the list are dropped as no-ops rather than erroring, so
        // callers can freely splat variable-length arrays.
        let queries: Vec<String> = match (params.symbols, params.symbol_name) {
            (Some(list), _) if !list.is_empty() => {
                list.into_iter().filter(|s| !s.is_empty()).collect()
            }
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

        let file_filter = params.file_path.as_deref();

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

        let total_symbols: usize = per_query.iter().map(|(_, f)| f.len()).sum();
        let mut out = String::new();
        let mut rendered_any = false;
        let mut rendered_count: usize = 0;
        let mut truncated = false;

        for (_idx, filtered) in &per_query {
            for (sym, file) in filtered {
                let abs_path = self.project_root.join(&file.path);
                let source = match std::fs::read_to_string(&abs_path) {
                    Ok(s) => s,
                    Err(e) => return Err(format!("Cannot read {}: {e}", abs_path.display())),
                };

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

                // Stop before writing if this section would push us past the
                // cap. We still include at least one full section even if it
                // exceeds the budget alone — truncating a symbol mid-line is
                // worse than returning a single over-budget response.
                if !out.is_empty() && out.len() + section.len() > max_bytes {
                    truncated = true;
                    break;
                }
                out.push_str(&section);
                rendered_any = true;
                rendered_count += 1;
            }

            if truncated {
                break;
            }
        }

        if !rendered_any {
            let joined = queries.join(", ");
            if let Some(fp) = file_filter {
                return Err(format!(
                    "No symbols [{joined}] found in file matching '{fp}'"
                ));
            }
            return Err(format!("No symbol found with name(s) [{joined}]"));
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
                "// ... (truncated: {} symbol(s) skipped, response reached {}-byte cap)\n",
                remaining, max_bytes,
            ));
        }

        Ok(out)
    }

    #[tool(
        name = "qartez_impact",
        description = "MUST call before modifying any file with exports. Shows direct importers, transitive dependents, and co-change partners — the full set of files that could break.",
        annotations(
            title = "Impact Analysis",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn qartez_impact(
        &self,
        Parameters(params): Parameters<SoulImpactParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let concise = is_concise(&params.format);
        let include_tests = params.include_tests.unwrap_or(false);
        let file = read::get_file_by_path(&conn, &params.file_path)
            .map_err(|e| format!("DB error: {e}"))?
            .ok_or_else(|| format!("File '{}' not found in index", params.file_path))?;

        // Record that Claude has now reviewed the impact of editing this
        // file. The guard binary reads this as an acknowledgment and lets
        // subsequent Edit/Write calls on the same file through for a short
        // TTL window (see `qartez_mcp::guard`).
        guard::touch_ack(&self.project_root, &file.path);

        let blast_result =
            blast::blast_radius_for_file(&conn, file.id).map_err(|e| format!("Error: {e}"))?;

        let direct_names: Vec<String> = blast_result
            .direct_importers
            .iter()
            .filter_map(|&id| {
                read::get_file_by_id(&conn, id)
                    .ok()
                    .flatten()
                    .map(|f| f.path)
            })
            .filter(|p| include_tests || !is_test_path(p))
            .collect();

        let transitive_names: Vec<String> = blast_result
            .transitive_importers
            .iter()
            .filter_map(|&id| {
                read::get_file_by_id(&conn, id)
                    .ok()
                    .flatten()
                    .map(|f| f.path)
            })
            .filter(|p| include_tests || !is_test_path(p))
            .collect();

        let cochanges = read::get_cochanges(&conn, file.id, 10).unwrap_or_default();

        if concise {
            let mut out = format!(
                "Impact: {} | direct: {} | transitive: {} | cochange: {}\n",
                params.file_path,
                direct_names.len(),
                transitive_names.len(),
                cochanges.len(),
            );
            if !direct_names.is_empty() {
                out.push_str(&format!("Direct: {}\n", direct_names.join(", ")));
            }
            return Ok(out);
        }

        let mut out = format!("# Impact analysis: {}\n\n", params.file_path);
        out.push_str(&format!(
            "Direct importers ({}): {}\n",
            direct_names.len(),
            if direct_names.is_empty() {
                "none".to_string()
            } else {
                direct_names.join(", ")
            }
        ));

        out.push_str(&format!(
            "Transitive blast radius: {} file(s)\n",
            transitive_names.len(),
        ));
        for name in &transitive_names {
            out.push_str(&format!("  - {name}\n"));
        }

        if !cochanges.is_empty() {
            out.push_str(&format!("\nCo-change partners ({}):\n", cochanges.len()));
            for (cc, partner) in &cochanges {
                out.push_str(&format!(
                    "  {} (changed together {} times)\n",
                    partner.path, cc.count
                ));
            }
        }

        // Per-symbol breakdown: top symbols inside this file by
        // symbol-level PageRank. Helps Claude focus on the specific
        // symbols that matter when deciding how to edit safely. Only
        // emitted when there is actual signal (nonzero ranks) so legacy
        // DBs and languages without reference extraction do not produce
        // a confusing "all zeros" block.
        let hot_syms = read::get_symbols_ranked_for_file(&conn, file.id, 5).unwrap_or_default();
        let hot_syms_with_rank: Vec<&crate::storage::models::SymbolRow> =
            hot_syms.iter().filter(|s| s.pagerank > 0.0).collect();
        if !hot_syms_with_rank.is_empty() {
            out.push_str(&format!(
                "\nHot symbols in this file ({}):\n",
                hot_syms_with_rank.len()
            ));
            for sym in &hot_syms_with_rank {
                out.push_str(&format!(
                    "  {} ({}) pr={:.4} L{}-L{}\n",
                    sym.name, sym.kind, sym.pagerank, sym.line_start, sym.line_end,
                ));
            }
        }

        Ok(out)
    }

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
    fn qartez_cochange(
        &self,
        Parameters(params): Parameters<SoulCochangeParams>,
    ) -> Result<String, String> {
        let concise = is_concise(&params.format);
        let limit = params.limit.unwrap_or(10) as usize;
        let max_commit_size = params.max_commit_size.unwrap_or(30) as usize;

        {
            let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
            if read::get_file_by_path(&conn, &params.file_path)
                .map_err(|e| format!("DB error: {e}"))?
                .is_none()
            {
                return Err(format!("File '{}' not found in index", params.file_path));
            }
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
                    return Ok(format!(
                        "No co-change data found for '{}'. Run with git history available.",
                        params.file_path,
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

    #[tool(
        name = "qartez_grep",
        description = "Search indexed symbols by name, kind, or file path using FTS5. Use prefix matching (e.g., 'Config*') for fuzzy search. Returns symbol locations with export status. Faster than Grep — searches the index, not disk.",
        annotations(
            title = "Search Symbols",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn qartez_grep(
        &self,
        Parameters(params): Parameters<SoulGrepParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let limit = params.limit.unwrap_or(20) as i64;
        let budget = params.token_budget.unwrap_or(4000) as usize;
        let concise = is_concise(&params.format);
        let use_regex = params.regex.unwrap_or(false);
        let search_bodies = params.search_bodies.unwrap_or(false);

        let results: Vec<(crate::storage::models::SymbolRow, String)> = if search_bodies {
            let fts_query = sanitize_fts_query(&params.query);
            read::search_symbol_bodies_fts(&conn, &fts_query, limit).map_err(|e| {
                format!(
                    "body FTS error: {e}. Try regex=true or drop search_bodies for symbol-name search.",
                )
            })?
        } else if use_regex {
            let re = regex::Regex::new(&params.query).map_err(|e| format!("regex error: {e}"))?;
            let all =
                read::get_all_symbols_with_path(&conn).map_err(|e| format!("DB error: {e}"))?;
            all.into_iter()
                .filter(|(s, _)| re.is_match(&s.name))
                .take(limit as usize)
                .collect()
        } else {
            let fts_query = sanitize_fts_query(&params.query);
            read::search_symbols_fts(&conn, &fts_query, limit).map_err(|e| {
                format!(
                    "FTS error: {e}. Try regex=true for source-code patterns like `#[tool` or `Foo::bar`.",
                )
            })?
        };

        if results.is_empty() {
            return Ok(format!("No symbols matching '{}'", params.query));
        }

        let mut out = format!(
            "Found {} result(s) for '{}':\n\n",
            results.len(),
            params.query,
        );
        for (sym, file_path) in &results {
            let line = if concise {
                let marker = if sym.is_exported { "+" } else { " " };
                format!(
                    " {marker} {} — {} [L{}]\n",
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
        }
        Ok(out)
    }

    #[tool(
        name = "qartez_unused",
        description = "Find dead code: exported symbols with zero importers in the codebase. Safe candidates for removal or inlining. Pre-materialized at index time, so the whole-repo scan is a single indexed SELECT. Pass `limit` / `offset` to page through large result sets.",
        annotations(
            title = "Find Unused Exports",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn qartez_unused(
        &self,
        Parameters(params): Parameters<SoulUnusedParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;

        let limit = params.limit.unwrap_or(50).max(1) as i64;
        let offset = params.offset.unwrap_or(0) as i64;

        let total = read::count_unused_exports(&conn).map_err(|e| format!("DB error: {e}"))?;
        if total == 0 {
            return Ok("No unused exported symbols detected.".to_string());
        }

        let page = read::get_unused_exports_page(&conn, limit, offset)
            .map_err(|e| format!("DB error: {e}"))?;

        if page.is_empty() {
            return Ok(format!(
                "No unused exports in page (total={total}, offset={offset})."
            ));
        }

        let shown = page.len() as i64;
        let mut out = if shown < total {
            format!(
                "{total} unused export(s); showing {shown} from offset {offset} (next: offset={}).\n",
                offset + shown
            )
        } else {
            format!("{total} unused export(s).\n")
        };

        // Compact per-file format: one header per file, one line per symbol
        // without the parenthesized kind (it's redundant with the kind-letter
        // prefix). Saves ~40% tokens vs the old `  - name (kind) [L-L]` shape.
        let mut current_path: &str = "";
        for (sym, file) in &page {
            if file.path != current_path {
                out.push_str(&format!("{}\n", file.path));
                current_path = file.path.as_str();
            }
            out.push_str(&format!(
                "  {} {} L{}\n",
                sym.kind.chars().next().unwrap_or(' '),
                sym.name,
                sym.line_start,
            ));
        }
        Ok(out)
    }

    #[tool(
        name = "qartez_refs",
        description = "Trace all usages of a symbol: which files import it, and (with transitive=true) the full dependency chain. Essential before renaming, moving, or deleting a symbol.",
        annotations(
            title = "Symbol References",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn qartez_refs(
        &self,
        Parameters(params): Parameters<SoulRefsParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let budget = params.token_budget.unwrap_or(4000) as usize;
        let concise = is_concise(&params.format);
        let refs = read::get_symbol_references(&conn, &params.symbol)
            .map_err(|e| format!("DB error: {e}"))?;

        if refs.is_empty() {
            return Ok(format!("No symbol found with name '{}'", params.symbol));
        }

        let transitive = params.transitive.unwrap_or(false);
        let mut out = String::new();

        // FTS fallback: files whose symbol bodies mention the target
        // identifier. Supplements the edge graph because not every caller
        // shows up as a direct importer — external-crate `use` lines are
        // dropped at index time, `use crate::a::sub;` resolves to `a/mod.rs`
        // not `a/sub.rs`, and child modules pulled in via `use super::*;`
        // carry a wildcard specifier that the old importer filter dropped.
        // Failures are non-fatal: if FTS is missing we still have the
        // edge-based scan set below.
        let fts_fallback_paths: Vec<String> =
            read::find_file_paths_by_body_text(&conn, &params.symbol).unwrap_or_default();

        for (sym, file, importers) in &refs {
            if concise {
                let paths: Vec<&str> = importers.iter().map(|(_, f)| f.path.as_str()).collect();
                out.push_str(&format!(
                    "{} ({}) in {} — {} ref(s): {}\n",
                    sym.name,
                    sym.kind,
                    file.path,
                    paths.len(),
                    if paths.is_empty() {
                        "none".to_string()
                    } else {
                        paths.join(", ")
                    },
                ));
                continue;
            }

            out.push_str(&format!(
                "# Symbol: {} ({})\n  Defined in: {} [L{}-L{}]\n\n",
                sym.name, sym.kind, file.path, sym.line_start, sym.line_end,
            ));

            if importers.is_empty() {
                out.push_str("  No direct references found.\n\n");
            } else {
                out.push_str(&format!("  Direct references ({}):\n", importers.len()));
                for (edge, importer_file) in importers {
                    let line = format!(
                        "    {} — imports via '{}' ({})\n",
                        importer_file.path,
                        edge.specifier.as_deref().unwrap_or("(unspecified)"),
                        edge.kind,
                    );
                    if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                        out.push_str("    ... (truncated by token budget)\n");
                        break;
                    }
                    out.push_str(&line);
                }
                out.push('\n');
            }

            // Union the def file, every importer, and every FTS-body-match
            // into the scan set. BTreeSet gives us dedup plus stable order
            // for reproducible output.
            let mut scan_paths: BTreeSet<String> = BTreeSet::new();
            scan_paths.insert(file.path.clone());
            for (_, importer_file) in importers {
                scan_paths.insert(importer_file.path.clone());
            }
            for path in &fts_fallback_paths {
                scan_paths.insert(path.clone());
            }
            let mut call_sites: Vec<(String, usize)> = Vec::new();
            for scan_path in &scan_paths {
                let calls = self.cached_calls(scan_path);
                for (name, line_no) in calls.iter() {
                    if name == &params.symbol {
                        call_sites.push((scan_path.clone(), *line_no));
                    }
                }
            }
            call_sites.sort();
            if !call_sites.is_empty() {
                out.push_str(&format!(
                    "  Direct call sites ({} — AST-resolved):\n",
                    call_sites.len()
                ));
                let mut last_path = String::new();
                for (path, line_no) in &call_sites {
                    let line = if path == &last_path {
                        format!("        L{}\n", line_no)
                    } else {
                        last_path = path.clone();
                        format!("    {} [L{}]\n", path, line_no)
                    };
                    if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                        out.push_str("    ... (truncated by token budget)\n");
                        break;
                    }
                    out.push_str(&line);
                }
                out.push('\n');
            }

            if transitive {
                let all_edges = read::get_all_edges(&conn).map_err(|e| format!("DB error: {e}"))?;

                let mut reverse: HashMap<i64, Vec<i64>> = HashMap::new();
                for &(from, to) in &all_edges {
                    reverse.entry(to).or_default().push(from);
                }

                let mut visited: HashSet<i64> = HashSet::new();
                let mut queue: VecDeque<(i64, u32)> = VecDeque::new();
                let mut by_depth: HashMap<u32, Vec<String>> = HashMap::new();

                if let Some(neighbors) = reverse.get(&file.id) {
                    for &n in neighbors {
                        if visited.insert(n) {
                            queue.push_back((n, 1));
                        }
                    }
                }

                while let Some((current, depth)) = queue.pop_front() {
                    if let Ok(Some(f)) = read::get_file_by_id(&conn, current) {
                        by_depth.entry(depth).or_default().push(f.path);
                    }
                    if let Some(neighbors) = reverse.get(&current) {
                        for &n in neighbors {
                            if n != file.id && visited.insert(n) {
                                queue.push_back((n, depth + 1));
                            }
                        }
                    }
                }

                if by_depth.is_empty() {
                    out.push_str("  No transitive dependents.\n\n");
                } else {
                    let total: usize = by_depth.values().map(|v| v.len()).sum();
                    out.push_str(&format!("  Transitive dependents ({} total):\n", total));
                    let mut depths: Vec<u32> = by_depth.keys().copied().collect();
                    depths.sort();
                    let mut truncated = false;
                    'trans: for depth in depths {
                        if let Some(files) = by_depth.get(&depth) {
                            for f in files {
                                let line = format!("    [depth {}] {}\n", depth, f);
                                if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                                    out.push_str("    ... (truncated by token budget)\n");
                                    truncated = true;
                                    break 'trans;
                                }
                                out.push_str(&line);
                            }
                        }
                    }
                    if !truncated {
                        out.push('\n');
                    }
                }
            }
        }

        Ok(out)
    }

    #[tool(
        name = "qartez_rename",
        description = "Rename a symbol across the entire codebase: definition, imports, and all usages. Uses tree-sitter AST matching when available, falls back to word-boundary matching. Preview by default; set apply=true to execute.",
        annotations(
            title = "Rename Symbol",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn qartez_rename(
        &self,
        Parameters(params): Parameters<SoulRenameParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let refs = read::get_symbol_references(&conn, &params.old_name)
            .map_err(|e| format!("DB error: {e}"))?;

        if refs.is_empty() {
            return Err(format!("No symbol found with name '{}'", params.old_name));
        }

        // Union every file that could host an occurrence: the def file,
        // every edge-graph importer (unfiltered — the previous
        // `specifier.contains(old_name)` filter dropped real callers when
        // the `use` statement imported the parent module, e.g.
        // `use crate::storage::read;` followed by `read::symbol(...)`, or
        // `use super::*;` in child test modules), and every file surfaced
        // by the body-FTS fallback (catches external-crate imports and
        // Rust module-form `use` statements whose resolver mis-routes the
        // edge to `mod.rs`). Preview-mode renames ship to the caller as
        // the ground truth for an apply step — missing a site here means
        // the apply breaks the build.
        let mut file_set: BTreeSet<String> = BTreeSet::new();
        for (_, def_file, importers) in &refs {
            file_set.insert(def_file.path.clone());
            for (_, importer_file) in importers {
                file_set.insert(importer_file.path.clone());
            }
        }
        if let Ok(paths) = read::find_file_paths_by_body_text(&conn, &params.old_name) {
            for path in paths {
                file_set.insert(path);
            }
        }
        let files_to_scan: Vec<String> = file_set.into_iter().collect();

        let apply = params.apply.unwrap_or(false);
        // (file_path, line_number, old_line_text, new_line_text)
        let mut changes: Vec<(String, usize, String, String)> = Vec::new();
        // Per-file AST-based byte ranges: file_path -> [(line, byte_start, byte_end)]
        let mut ast_ranges: HashMap<String, Vec<(usize, usize, usize)>> = HashMap::new();

        // Files where we actually found a rename target. Kept separate
        // from `files_to_scan` because the FTS-based scan set is
        // deliberately generous — it includes files that mention the name
        // only inside strings or comments — and we must not rewrite those
        // false positives on apply.
        let mut files_touched: Vec<String> = Vec::new();

        for rel_path in &files_to_scan {
            // Prefer the shared parse cache so repeat invocations (warmup +
            // measured benchmark runs, or multi-file renames that revisit
            // the definition file) skip tree-sitter reparsing entirely. The
            // cache is keyed by relative path + mtime, so a file edited on
            // disk forces a reparse on the next call. `cached_idents`
            // performs a single grouped walk per file lifetime; a lookup
            // for any name is then an O(1) HashMap hit.
            match self.cached_idents(rel_path) {
                Some(idents_map) => {
                    // AST-supported language (tree-sitter parsed the file).
                    // Missing from the map means there is no identifier
                    // with that name in this file — the FTS hit was in a
                    // string literal or comment. Skip the file entirely;
                    // falling through to substring matching would rewrite
                    // those non-code mentions and corrupt the build.
                    let Some(occurrences) = idents_map.get(&params.old_name) else {
                        continue;
                    };
                    if occurrences.is_empty() {
                        continue;
                    }
                    let source_arc = self.cached_source(rel_path).ok_or_else(|| {
                        format!("Cannot read {}", self.project_root.join(rel_path).display())
                    })?;
                    let content: &str = source_arc.as_str();
                    let lines: Vec<&str> = content.lines().collect();
                    for &(line_num, start, end) in occurrences.iter() {
                        let line_idx = line_num - 1;
                        if line_idx < lines.len() {
                            let old_line = lines[line_idx].to_string();
                            let line_byte_start =
                                content[..start].rfind('\n').map(|p| p + 1).unwrap_or(0);
                            let offset_in_line = start - line_byte_start;
                            let end_offset = end - line_byte_start;
                            let new_line = format!(
                                "{}{}{}",
                                &old_line[..offset_in_line],
                                &params.new_name,
                                &old_line[end_offset..],
                            );
                            changes.push((rel_path.clone(), line_num, old_line, new_line));
                        }
                    }
                    ast_ranges.insert(rel_path.clone(), occurrences.clone());
                    files_touched.push(rel_path.clone());
                }
                None => {
                    // Language not supported by tree-sitter — use a
                    // word-boundary text scan as the only available signal.
                    let source_arc = self.cached_source(rel_path).ok_or_else(|| {
                        format!("Cannot read {}", self.project_root.join(rel_path).display())
                    })?;
                    let content: &str = source_arc.as_str();
                    let mut file_had_hit = false;
                    for (line_num, line) in content.lines().enumerate() {
                        let mut start = 0;
                        while let Some(pos) = line[start..].find(&params.old_name) {
                            let abs_pos = start + pos;
                            let before_ok = if abs_pos == 0 {
                                true
                            } else {
                                let ch = line[..abs_pos].chars().next_back().unwrap();
                                !ch.is_alphanumeric() && ch != '_'
                            };
                            let after_pos = abs_pos + params.old_name.len();
                            let after_ok = if after_pos >= line.len() {
                                true
                            } else {
                                let ch = line[after_pos..].chars().next().unwrap();
                                !ch.is_alphanumeric() && ch != '_'
                            };

                            if before_ok && after_ok {
                                let new_line = format!(
                                    "{}{}{}",
                                    &line[..abs_pos],
                                    &params.new_name,
                                    &line[after_pos..],
                                );
                                changes.push((
                                    rel_path.clone(),
                                    line_num + 1,
                                    line.to_string(),
                                    new_line,
                                ));
                                file_had_hit = true;
                            }
                            start = abs_pos + params.old_name.len();
                        }
                    }
                    if file_had_hit {
                        files_touched.push(rel_path.clone());
                    }
                }
            }
        }

        if changes.is_empty() {
            return Ok(format!(
                "No occurrences of '{}' found in relevant files.",
                params.old_name,
            ));
        }

        if apply {
            let mut files_modified: HashSet<String> = HashSet::new();
            // Only rewrite files that had real identifier hits. An FTS
            // candidate that matched in a string or comment made it into
            // `files_to_scan` but was skipped during the AST walk above;
            // those files must stay untouched.
            for rel_path in &files_touched {
                let abs_path = self.project_root.join(rel_path);
                let content = std::fs::read_to_string(&abs_path)
                    .map_err(|e| format!("Cannot read {}: {e}", abs_path.display()))?;

                let new_content = if let Some(ranges) = ast_ranges.get(rel_path) {
                    let mut sorted = ranges.clone();
                    sorted.sort_by_key(|&(_, start, _)| start);
                    let mut buf = content.clone();
                    for &(_, start, end) in sorted.iter().rev() {
                        buf.replace_range(start..end, &params.new_name);
                    }
                    buf
                } else {
                    replace_whole_word(&content, &params.old_name, &params.new_name)
                };

                if new_content != content {
                    let tmp_path = abs_path.with_extension("qartez_rename_tmp");
                    std::fs::write(&tmp_path, &new_content)
                        .map_err(|e| format!("Cannot write {}: {e}", tmp_path.display()))?;
                    std::fs::rename(&tmp_path, &abs_path).map_err(|e| {
                        let _ = std::fs::remove_file(&tmp_path);
                        format!("Cannot rename temp file to {}: {e}", abs_path.display())
                    })?;
                    files_modified.insert(rel_path.clone());
                }
            }

            let mut out = format!(
                "Renamed '{}' → '{}'. All references updated.\n",
                params.old_name, params.new_name,
            );
            out.push_str(&format!(
                "{} file(s) modified, {} occurrence(s) replaced:\n",
                files_modified.len(),
                changes.len(),
            ));
            for f in &files_modified {
                let count = changes.iter().filter(|(p, _, _, _)| p == f).count();
                out.push_str(&format!("  {} ({} changes)\n", f, count));
            }
            Ok(out)
        } else {
            // Compact preview: "old → new: N occurrences in M files", then
            // for each file a single line per occurrence with just the line
            // number and the trimmed after-text. The before-line is omitted
            // (reader has the file) — delivers the same actionable info at
            // ~40% fewer tokens than the diff-style output used previously.
            let mut out = format!(
                "{} → {}: {} occ in {} file(s)\n",
                params.old_name,
                params.new_name,
                changes.len(),
                files_touched.len(),
            );
            let mut current_file = String::new();
            for (file, line_num, _before, after) in &changes {
                if *file != current_file {
                    out.push_str(&format!("{}\n", file));
                    current_file = file.clone();
                }
                out.push_str(&format!("  L{}: {}\n", line_num, after.trim()));
            }
            Ok(out)
        }
    }

    #[tool(
        name = "qartez_project",
        description = "Run project commands (test, build, lint, typecheck) with auto-detected toolchain (Cargo, npm/bun/yarn/pnpm, Go, Python, Dart/Flutter, Maven, Gradle, sbt, Ruby, Make). Use action='info' to see detected commands. Use filter for targeted runs (e.g., test name).",
        annotations(
            title = "Run Project Command",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    fn qartez_project(
        &self,
        Parameters(params): Parameters<SoulProjectParams>,
    ) -> Result<String, String> {
        let all_toolchains = toolchain::detect_all_toolchains(&self.project_root);
        let action = params.action.unwrap_or_default();

        if action == ProjectAction::Info {
            if all_toolchains.is_empty() {
                return Err("No recognized toolchain found. Looked for: Cargo.toml, package.json, go.mod, pyproject.toml, setup.py, pubspec.yaml, Gemfile, Makefile, pom.xml, build.gradle(.kts), build.sbt".to_string());
            }
            let mut out = String::new();
            for (i, tc) in all_toolchains.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                let available = toolchain::binary_available(&tc.build_tool);
                let marker = if available {
                    ""
                } else {
                    " (not found on PATH)"
                };
                out.push_str(&format!("# Project toolchain: {}{}\n\n", tc.name, marker,));
                out.push_str(&format!("Build tool: {}\n", tc.build_tool));
                out.push_str(&format!("Test:       {}\n", tc.test_cmd.join(" ")));
                out.push_str(&format!("Build:      {}\n", tc.build_cmd.join(" ")));
                if let Some(ref lint) = tc.lint_cmd {
                    out.push_str(&format!("Lint:       {}\n", lint.join(" ")));
                }
                if let Some(ref typecheck) = tc.typecheck_cmd {
                    out.push_str(&format!("Typecheck:  {}\n", typecheck.join(" ")));
                }
            }
            return Ok(out);
        }

        let tc = all_toolchains.into_iter().next().ok_or_else(|| {
            "No recognized toolchain found. Looked for: Cargo.toml, package.json, go.mod, pyproject.toml, setup.py, pubspec.yaml, Gemfile, Makefile, pom.xml, build.gradle(.kts), build.sbt".to_string()
        })?;

        if action == ProjectAction::Run {
            let subcommand = params.filter.as_deref().unwrap_or("test");
            let resolved: &Vec<String> = match subcommand {
                "test" => &tc.test_cmd,
                "build" => &tc.build_cmd,
                "lint" => tc.lint_cmd.as_ref().ok_or_else(|| {
                    format!("No lint command configured for {} toolchain", tc.name)
                })?,
                "typecheck" => tc.typecheck_cmd.as_ref().ok_or_else(|| {
                    format!("No typecheck command configured for {} toolchain", tc.name)
                })?,
                other => {
                    return Err(format!(
                        "Unknown run subcommand '{}'. Supported: test, build, lint, typecheck",
                        other,
                    ));
                }
            };
            return Ok(format!(
                "# {toolchain} {sub} (dry-run — command not executed)\n$ {cmd}\n",
                toolchain = tc.name,
                sub = subcommand,
                cmd = resolved.join(" "),
            ));
        }

        let (cmd, action_label): (&Vec<String>, &'static str) = match action {
            ProjectAction::Test => (&tc.test_cmd, "TEST"),
            ProjectAction::Build => (&tc.build_cmd, "BUILD"),
            ProjectAction::Lint => (
                tc.lint_cmd.as_ref().ok_or_else(|| {
                    format!("No lint command configured for {} toolchain", tc.name)
                })?,
                "LINT",
            ),
            ProjectAction::Typecheck => (
                tc.typecheck_cmd.as_ref().ok_or_else(|| {
                    format!("No typecheck command configured for {} toolchain", tc.name)
                })?,
                "TYPECHECK",
            ),
            ProjectAction::Info | ProjectAction::Run => {
                // Handled by the early-return branches above.
                unreachable!()
            }
        };

        let timeout = params.timeout.unwrap_or(60).min(600);
        let filter = params.filter.as_deref();
        if let Some(f) = filter
            && f.starts_with('-')
        {
            return Err(format!("Filter must not start with '-': {f}"));
        }

        let (exit_code, output) = toolchain::run_command(&self.project_root, cmd, filter, timeout)?;

        let status = if exit_code == 0 { "SUCCESS" } else { "FAILED" };
        let mut out = format!(
            "# {} {} (exit code: {})\n$ {}{}\n\n",
            action_label,
            status,
            exit_code,
            cmd.join(" "),
            filter.map(|f| format!(" {}", f)).unwrap_or_default(),
        );
        out.push_str(&output);
        Ok(out)
    }

    #[tool(
        name = "qartez_move",
        description = "Move a symbol to another file and update all import paths automatically. Handles extraction, insertion, and importer rewrites in one step. Preview by default; set apply=true to execute.",
        annotations(
            title = "Move Symbol",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn qartez_move(
        &self,
        Parameters(params): Parameters<SoulMoveParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let mut results = read::find_symbol_by_name(&conn, &params.symbol)
            .map_err(|e| format!("DB error: {e}"))?;

        if results.is_empty() {
            return Err(format!("No symbol found with name '{}'", params.symbol));
        }

        // Narrow by kind when the caller supplies one. The SQL layer only
        // matches on name, so free `fn foo()` and `impl Foo { fn foo() }`
        // arrive together — a `kind` hint lets the caller pick exactly one
        // without touching the DB query path.
        if let Some(k) = params.kind.as_deref().filter(|s| !s.is_empty()) {
            let available: Vec<String> = results
                .iter()
                .map(|(s, _)| s.kind.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            results.retain(|(s, _)| s.kind.eq_ignore_ascii_case(k));
            if results.is_empty() {
                return Err(format!(
                    "No symbol '{}' with kind '{}'. Available kinds: {}",
                    params.symbol,
                    k,
                    available.join(", "),
                ));
            }
        }

        if results.len() > 1 {
            let locations: Vec<String> = results
                .iter()
                .map(|(s, f)| {
                    format!(
                        "  {} ({}) in {} [L{}-L{}]",
                        s.name, s.kind, f.path, s.line_start, s.line_end
                    )
                })
                .collect();
            return Err(format!(
                "Multiple definitions of '{}' found. Pass `kind` to disambiguate or specify a unique name:\n{}",
                params.symbol,
                locations.join("\n"),
            ));
        }

        let (sym, source_file) = &results[0];
        let source_abs = self.project_root.join(&source_file.path);
        let target_abs = self.project_root.join(&params.to_file);

        if source_file.path != params.to_file
            && let Ok(Some(target_file)) = read::get_file_by_path(&conn, &params.to_file)
            && let Ok(target_syms) = read::get_symbols_for_file(&conn, target_file.id)
            && let Some(conflict) = target_syms
                .iter()
                .find(|s| s.name == sym.name && s.kind == sym.kind)
        {
            return Err(format!(
                "Cannot move '{}' ({}): destination '{}' already defines a {} '{}' at L{}-L{}. Refusing to overwrite.",
                sym.name,
                sym.kind,
                params.to_file,
                conflict.kind,
                conflict.name,
                conflict.line_start,
                conflict.line_end,
            ));
        }

        let source_content = std::fs::read_to_string(&source_abs)
            .map_err(|e| format!("Cannot read {}: {e}", source_abs.display()))?;

        let lines: Vec<&str> = source_content.lines().collect();
        let start_idx = (sym.line_start as usize).saturating_sub(1);
        let end_idx = (sym.line_end as usize).min(lines.len());

        if start_idx >= lines.len() {
            return Err(format!(
                "Symbol line range L{}-L{} out of bounds for {} ({} lines)",
                sym.line_start,
                sym.line_end,
                source_file.path,
                lines.len(),
            ));
        }

        let extracted_code: String = lines[start_idx..end_idx].join("\n");

        let importers =
            read::get_edges_to(&conn, source_file.id).map_err(|e| format!("DB error: {e}"))?;

        let mut importer_files: Vec<(String, Option<String>)> = Vec::new();
        for edge in &importers {
            let spec_matches = edge
                .specifier
                .as_ref()
                .map(|s| s.contains(&params.symbol))
                .unwrap_or(true);
            if spec_matches && let Ok(Some(f)) = read::get_file_by_id(&conn, edge.from_file) {
                importer_files.push((f.path.clone(), edge.specifier.clone()));
            }
        }

        let target_stem = &params.to_file;

        let apply = params.apply.unwrap_or(false);

        if !apply {
            let mut out = format!(
                "Preview: move '{}' ({}) from {} to {}\n\n",
                sym.name, sym.kind, source_file.path, params.to_file,
            );

            out.push_str(&format!(
                "Code to extract (L{}-L{}, {} lines):\n",
                sym.line_start,
                sym.line_end,
                end_idx - start_idx
            ));
            out.push_str("```\n");
            let preview = if extracted_code.len() > 500 {
                let end = extracted_code.floor_char_boundary(500);
                format!("{}...\n(truncated)", &extracted_code[..end])
            } else {
                extracted_code.clone()
            };
            out.push_str(&preview);
            out.push_str("\n```\n\n");

            if importer_files.is_empty() {
                out.push_str("No files import this symbol.\n");
            } else {
                out.push_str(&format!(
                    "Files that import this symbol ({}):\n",
                    importer_files.len()
                ));
                for (path, spec) in &importer_files {
                    let spec_str = spec.as_deref().unwrap_or("(unspecified)");
                    out.push_str(&format!("  {} — via '{}'\n", path, spec_str));
                }
                out.push_str(
                    "\nImport paths in these files will be updated to point to the new location.\n",
                );
            }

            return Ok(out);
        }

        let remaining_lines: Vec<&str> = lines
            .iter()
            .enumerate()
            .filter(|(i, _)| *i < start_idx || *i >= end_idx)
            .map(|(_, l)| *l)
            .collect();
        let new_source = remaining_lines.join("\n");
        if new_source.trim().is_empty() && remaining_lines.len() <= 1 {
            std::fs::write(&source_abs, "")
                .map_err(|e| format!("Cannot write {}: {e}", source_abs.display()))?;
        } else {
            let mut cleaned = new_source.clone();
            while cleaned.contains("\n\n\n") {
                cleaned = cleaned.replace("\n\n\n", "\n\n");
            }
            std::fs::write(&source_abs, &cleaned)
                .map_err(|e| format!("Cannot write {}: {e}", source_abs.display()))?;
        }

        if target_abs.exists() {
            let existing = std::fs::read_to_string(&target_abs)
                .map_err(|e| format!("Cannot read {}: {e}", target_abs.display()))?;
            let new_content = if existing.ends_with('\n') {
                format!("{}\n{}\n", existing.trim_end(), extracted_code)
            } else {
                format!("{}\n\n{}\n", existing, extracted_code)
            };
            std::fs::write(&target_abs, new_content)
                .map_err(|e| format!("Cannot write {}: {e}", target_abs.display()))?;
        } else {
            if let Some(parent) = target_abs.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create dirs for {}: {e}", target_abs.display()))?;
            }
            std::fs::write(&target_abs, format!("{}\n", extracted_code))
                .map_err(|e| format!("Cannot write {}: {e}", target_abs.display()))?;
        }

        let mut import_updates = 0;
        let mut failed_writes: Vec<String> = Vec::new();
        for (importer_path, _) in &importer_files {
            let importer_abs = self.project_root.join(importer_path);
            let content = match std::fs::read_to_string(&importer_abs) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let old_import_path = path_to_import_stem(&source_file.path);
            let new_import_path = path_to_import_stem(target_stem);

            if old_import_path != new_import_path {
                let updated =
                    match regex::Regex::new(&format!(r"\b{}\b", regex::escape(&old_import_path))) {
                        Ok(re) => re
                            .replace_all(&content, new_import_path.as_str())
                            .to_string(),
                        Err(_) => content.clone(),
                    };
                if updated != content {
                    if let Err(e) = std::fs::write(&importer_abs, &updated) {
                        failed_writes.push(format!("{}: {e}", importer_abs.display()));
                    } else {
                        import_updates += 1;
                    }
                }
            }
        }

        let status = if failed_writes.is_empty() {
            "All imports updated.".to_string()
        } else {
            format!(
                "WARNING: {} import(s) failed to write:\n  {}",
                failed_writes.len(),
                failed_writes.join("\n  "),
            )
        };
        let mut out = format!(
            "Moved '{}' ({}) from {} → {}. {status}\n\n",
            sym.name, sym.kind, source_file.path, params.to_file,
        );
        out.push_str(&format!(
            "{} lines extracted, {} importer(s) rewritten.\n",
            end_idx - start_idx,
            import_updates
        ));

        Ok(out)
    }

    #[tool(
        name = "qartez_rename_file",
        description = "Rename/move a file and rewrite all import paths pointing to it in one atomic operation. Preview by default; set apply=true to execute.",
        annotations(
            title = "Rename File",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn qartez_rename_file(
        &self,
        Parameters(params): Parameters<SoulRenameFileParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let file = read::get_file_by_path(&conn, &params.from)
            .map_err(|e| format!("DB error: {e}"))?
            .ok_or_else(|| format!("File '{}' not found in index", params.from))?;

        let from_abs = self.project_root.join(&params.from);
        let to_abs = self.project_root.join(&params.to);

        if !from_abs.exists() {
            return Err(format!(
                "Source file does not exist: {}",
                from_abs.display()
            ));
        }

        let importers = read::get_edges_to(&conn, file.id).map_err(|e| format!("DB error: {e}"))?;

        let mut importer_paths: Vec<String> = Vec::new();
        for edge in &importers {
            if let Ok(Some(f)) = read::get_file_by_id(&conn, edge.from_file)
                && !importer_paths.contains(&f.path)
            {
                importer_paths.push(f.path);
            }
        }

        let old_stem = path_to_import_stem(&params.from);
        let new_stem = path_to_import_stem(&params.to);

        let old_rel_stem = relative_import_stem(&params.from);
        let new_rel_stem = relative_import_stem(&params.to);

        let apply = params.apply.unwrap_or(false);

        if !apply {
            if importer_paths.is_empty() {
                return Ok(format!("{} → {}: 0 importers\n", params.from, params.to,));
            }
            // Single line: summary + comma-separated importer list. For a
            // typical refactor preview (≤ a dozen importers) this stays well
            // under 200 tokens and parses trivially.
            return Ok(format!(
                "{} → {}: {} importer(s): {}\n",
                params.from,
                params.to,
                importer_paths.len(),
                importer_paths.join(", "),
            ));
        }

        if let Some(parent) = to_abs.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot create dirs for {}: {e}", to_abs.display()))?;
        }
        std::fs::rename(&from_abs, &to_abs).map_err(|e| {
            format!(
                "Cannot rename {} -> {}: {e}",
                from_abs.display(),
                to_abs.display()
            )
        })?;

        let mut updated_count = 0;
        let mut failed_writes: Vec<String> = Vec::new();
        let stem_re = regex::Regex::new(&format!(r"\b{}\b", regex::escape(&old_stem)))
            .map_err(|e| format!("regex error: {e}"))?;
        let rel_stem_re = if old_rel_stem != old_stem {
            Some(
                regex::Regex::new(&format!(r"\b{}\b", regex::escape(&old_rel_stem)))
                    .map_err(|e| format!("regex error: {e}"))?,
            )
        } else {
            None
        };
        for importer_path in &importer_paths {
            let importer_abs = self.project_root.join(importer_path);
            let content = match std::fs::read_to_string(&importer_abs) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let mut updated = stem_re.replace_all(&content, new_stem.as_str()).to_string();
            if let Some(ref re) = rel_stem_re {
                updated = re.replace_all(&updated, new_rel_stem.as_str()).to_string();
            }

            if updated != content {
                if let Err(e) = std::fs::write(&importer_abs, &updated) {
                    failed_writes.push(format!("{}: {e}", importer_abs.display()));
                } else {
                    updated_count += 1;
                }
            }
        }

        // Rewrite `mod <old>;` in the parent module file. The edges table
        // only tracks `use` imports, so the parent that declares the module
        // never shows up as an importer — without this step, renaming
        // `src/foo.rs` → `src/bar.rs` leaves `mod foo;` dangling in
        // `src/lib.rs` and the crate fails to build.
        let mut mod_rewrite_note = String::new();
        if old_rel_stem != new_rel_stem
            && let Some(parent_rel) = find_parent_mod_file(&self.project_root, &params.from)
        {
            let parent_abs = self.project_root.join(&parent_rel);
            if let Ok(content) = std::fs::read_to_string(&parent_abs) {
                let rewritten = rewrite_mod_decl(&content, &old_rel_stem, &new_rel_stem);
                if rewritten != content {
                    if let Err(e) = std::fs::write(&parent_abs, &rewritten) {
                        failed_writes.push(format!("{}: {e}", parent_abs.display()));
                    } else {
                        mod_rewrite_note =
                            format!(", parent mod decl updated in {}", parent_rel.display(),);
                    }
                }
            }
        }

        let warn = if failed_writes.is_empty() {
            String::new()
        } else {
            format!(
                "\nWARNING: {} file(s) failed to write:\n  {}\n",
                failed_writes.len(),
                failed_writes.join("\n  "),
            )
        };
        Ok(format!(
            "renamed {} → {} ({}/{} importers updated{})\n{warn}",
            params.from,
            params.to,
            updated_count,
            importer_paths.len(),
            mod_rewrite_note,
        ))
    }

    #[tool(
        name = "qartez_outline",
        description = "List every symbol in a file grouped by kind (functions, classes, structs, etc.) with line numbers and signatures. Like a table of contents for the file.",
        annotations(
            title = "File Outline",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn qartez_outline(
        &self,
        Parameters(params): Parameters<SoulOutlineParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let budget = params.token_budget.unwrap_or(4000) as usize;
        let concise = is_concise(&params.format);
        let offset = params.offset.unwrap_or(0) as usize;
        let file = read::get_file_by_path(&conn, &params.file_path)
            .map_err(|e| format!("DB error: {e}"))?
            .ok_or_else(|| format!("File '{}' not found in index", params.file_path))?;

        let symbols =
            read::get_symbols_for_file(&conn, file.id).map_err(|e| format!("DB error: {e}"))?;

        if symbols.is_empty() {
            return Ok(format!(
                "No symbols found in '{}'. File may not be indexed yet.",
                params.file_path,
            ));
        }

        // Total non-field count drives the "next_offset" hint and the header.
        // We only page over non-field symbols because fields are rendered
        // inline underneath their parent struct, not as top-level entries.
        let total_non_fields = symbols.iter().filter(|s| s.kind != "field").count();
        let mut out = format!(
            "# Outline: {} ({} symbols)\n\n",
            params.file_path,
            symbols.len(),
        );

        if concise {
            let mut emitted = 0usize;
            let mut skipped = 0usize;
            let mut next_offset: Option<usize> = None;
            for sym in &symbols {
                if sym.kind == "field" {
                    continue;
                }
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                let marker = if sym.is_exported { "+" } else { "-" };
                let line = format!("  {marker} {} [L{}]\n", sym.name, sym.line_start);
                if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                    next_offset = Some(offset + emitted);
                    out.push_str("  ... (truncated)\n");
                    break;
                }
                out.push_str(&line);
                emitted += 1;
            }
            if let Some(next) = next_offset {
                out.push_str(&format!("next_offset: {next} (of {total_non_fields})\n",));
            }
            return Ok(out);
        }

        // Group fields under their parent struct: pre-index by parent id so
        // we can render struct → [fields] inline without blowing up the
        // top-level kind buckets.
        let mut fields_by_parent: HashMap<i64, Vec<&crate::storage::models::SymbolRow>> =
            HashMap::new();
        for sym in &symbols {
            if sym.kind == "field"
                && let Some(pid) = sym.parent_id
            {
                fields_by_parent.entry(pid).or_default().push(sym);
            }
        }

        let mut by_kind: std::collections::BTreeMap<
            String,
            Vec<&crate::storage::models::SymbolRow>,
        > = std::collections::BTreeMap::new();
        for sym in &symbols {
            if sym.kind == "field" {
                continue;
            }
            let display_kind = capitalize_kind(&sym.kind);
            by_kind.entry(display_kind).or_default().push(sym);
        }

        let mut skipped = 0usize;
        let mut emitted = 0usize;
        let mut next_offset: Option<usize> = None;
        'outer: for (kind, syms) in &by_kind {
            let mut header_written = false;
            for sym in syms {
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if !header_written {
                    out.push_str(&format!("{}:\n", kind));
                    header_written = true;
                }
                let marker = if sym.is_exported { "+" } else { "-" };
                let fallback = format!("{} {}", sym.kind, sym.name);
                let sig = sym.signature.as_deref().unwrap_or(&fallback);
                let cc_tag = sym
                    .complexity
                    .map(|c| format!(" CC={c}"))
                    .unwrap_or_default();
                let line = format!(
                    "  {} {} [L{}-L{}]{} — {}\n",
                    marker, sym.name, sym.line_start, sym.line_end, cc_tag, sig,
                );
                if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                    next_offset = Some(offset + emitted);
                    out.push_str("  ... (truncated by token budget)\n");
                    break 'outer;
                }
                out.push_str(&line);
                emitted += 1;

                if let Some(fields) = fields_by_parent.get(&sym.id) {
                    for f in fields {
                        let fmarker = if f.is_exported { "+" } else { "-" };
                        let fline = format!(
                            "      {} {} — {}\n",
                            fmarker,
                            f.name,
                            f.signature.as_deref().unwrap_or(f.name.as_str()),
                        );
                        if estimate_tokens(&out) + estimate_tokens(&fline) > budget {
                            next_offset = Some(offset + emitted);
                            out.push_str("  ... (truncated by token budget)\n");
                            break 'outer;
                        }
                        out.push_str(&fline);
                    }
                }
            }
            if header_written {
                out.push('\n');
            }
        }

        if let Some(next) = next_offset {
            out.push_str(&format!("next_offset: {next} (of {total_non_fields})\n",));
        }

        Ok(out)
    }

    #[tool(
        name = "qartez_deps",
        description = "Show a file's dependency graph: what it imports (outgoing) and what imports it (incoming). Reveals coupling and helps plan safe changes.",
        annotations(
            title = "File Dependencies",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn qartez_deps(
        &self,
        Parameters(params): Parameters<SoulDepsParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let budget = params.token_budget.unwrap_or(4000) as usize;
        let concise = is_concise(&params.format);
        let file = read::get_file_by_path(&conn, &params.file_path)
            .map_err(|e| format!("DB error: {e}"))?
            .ok_or_else(|| format!("File '{}' not found in index", params.file_path))?;

        let outgoing =
            read::get_edges_from(&conn, file.id).map_err(|e| format!("DB error: {e}"))?;
        let incoming = read::get_edges_to(&conn, file.id).map_err(|e| format!("DB error: {e}"))?;

        let blast_r = blast::blast_radius_for_file(&conn, file.id)
            .map(|r| r.transitive_count as i64)
            .unwrap_or(0);

        if concise {
            let out_paths: Vec<String> = outgoing
                .iter()
                .filter_map(|e| {
                    read::get_file_by_id(&conn, e.to_file)
                        .ok()
                        .flatten()
                        .map(|f| f.path)
                })
                .collect();
            let in_paths: Vec<String> = incoming
                .iter()
                .filter_map(|e| {
                    read::get_file_by_id(&conn, e.from_file)
                        .ok()
                        .flatten()
                        .map(|f| f.path)
                })
                .collect();
            return Ok(format!(
                "{} (→{}): imports {} → [{}] | imported by {} ← [{}]",
                params.file_path,
                blast_r,
                out_paths.len(),
                out_paths.join(", "),
                in_paths.len(),
                in_paths.join(", "),
            ));
        }

        let mut out = format!(
            "# Dependencies: {} (blast →{})\n\n",
            params.file_path, blast_r
        );

        out.push_str(&format!("Imports from ({}):\n", outgoing.len()));
        if outgoing.is_empty() {
            out.push_str("  (no imports)\n");
        } else {
            for edge in &outgoing {
                let target_path = read::get_file_by_id(&conn, edge.to_file)
                    .ok()
                    .flatten()
                    .map(|f| f.path)
                    .unwrap_or_else(|| format!("file#{}", edge.to_file));
                let line = match edge.specifier.as_deref() {
                    Some(spec) if !spec.is_empty() => {
                        format!("  -> {} ({}: {})\n", target_path, edge.kind, spec)
                    }
                    _ => format!("  -> {} ({})\n", target_path, edge.kind),
                };
                if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                    out.push_str("  ... (truncated)\n");
                    break;
                }
                out.push_str(&line);
            }
        }

        out.push('\n');
        out.push_str(&format!("Imported by ({}):\n", incoming.len()));
        if incoming.is_empty() {
            out.push_str("  (no files import this file)\n");
        } else {
            for edge in &incoming {
                let source_path = read::get_file_by_id(&conn, edge.from_file)
                    .ok()
                    .flatten()
                    .map(|f| f.path)
                    .unwrap_or_else(|| format!("file#{}", edge.from_file));
                let line = match edge.specifier.as_deref() {
                    Some(spec) if !spec.is_empty() => {
                        format!("  <- {} ({}: {})\n", source_path, edge.kind, spec)
                    }
                    _ => format!("  <- {} ({})\n", source_path, edge.kind),
                };
                if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                    out.push_str("  ... (truncated)\n");
                    break;
                }
                out.push_str(&line);
            }
        }

        Ok(out)
    }

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
    fn qartez_stats(
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
            return Ok(format!(
                "# {path}\nLOC: {loc} | Symbols: {syms} ({exp} exported) | Imports: {imp} | Importers: {importers}\nLanguage: {lang} | PageRank: {pr:.4}\n",
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

        let mut out = format!(
            "files={} (src={}/test={}) loc={}/{} syms={} edges={} with_symbols={}/{}\n",
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

    #[tool(
        name = "qartez_calls",
        description = "Show call hierarchy for a function: who calls it (callers) and what it calls (callees). Uses tree-sitter AST analysis. Distinguishes actual calls from type annotations, unlike qartez_refs.",
        annotations(
            title = "Call Hierarchy",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn qartez_calls(
        &self,
        Parameters(params): Parameters<SoulCallsParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let concise = is_concise(&params.format);
        let direction = params.direction.unwrap_or_default();
        let want_callers = matches!(direction, CallDirection::Callers | CallDirection::Both);
        let want_callees = matches!(direction, CallDirection::Callees | CallDirection::Both);
        // Depth=1 is the default after the 2026-04 compaction: depth=2 can
        // explode on hub functions, so callers opt in explicitly.
        let max_depth = params.depth.unwrap_or(1) as usize;

        let symbols =
            read::find_symbol_by_name(&conn, &params.name).map_err(|e| format!("DB error: {e}"))?;

        if symbols.is_empty() {
            return Err(format!("No symbol '{}' found in index", params.name));
        }

        let func_symbols: Vec<_> = symbols
            .iter()
            .filter(|(s, _)| matches!(s.kind.as_str(), "function" | "method" | "constructor"))
            .collect();

        if func_symbols.is_empty() {
            return Err(format!(
                "'{}' exists but is not a function/method",
                params.name
            ));
        }

        let mut out = String::new();
        // Per-invocation caches. Both sets overlap heavily inside a single
        // tool call, so memoizing avoids re-running SQL.
        let mut resolve_cache: HashMap<
            String,
            Vec<(
                crate::storage::models::SymbolRow,
                crate::storage::models::FileRow,
            )>,
        > = HashMap::new();
        let mut file_syms_cache: HashMap<i64, Vec<crate::storage::models::SymbolRow>> =
            HashMap::new();

        for (sym, def_file) in &func_symbols {
            // Compact header: "fn @ file:Lx-Ly" fits on one line.
            out.push_str(&format!(
                "{} ({}) @ {}:L{}-{}\n",
                sym.name, sym.kind, def_file.path, sym.line_start, sym.line_end,
            ));

            if want_callers {
                let all_files = read::get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;

                // Collect all call sites up-front, then render as grouped
                // one-liners. Grouping elides the per-file header.
                let mut sites: Vec<(String, usize, Option<String>)> = Vec::new();
                for file in &all_files {
                    let source = match self.cached_source(&file.path) {
                        Some(s) => s,
                        None => continue,
                    };
                    if !source.contains(params.name.as_str()) {
                        continue;
                    }
                    let calls = self.cached_calls(&file.path);
                    let matching: Vec<usize> = calls
                        .iter()
                        .filter(|(name, _)| name == &params.name)
                        .map(|(_, l)| *l)
                        .collect();
                    if matching.is_empty() {
                        continue;
                    }
                    let file_syms = file_syms_cache.entry(file.id).or_insert_with(|| {
                        read::get_symbols_for_file(&conn, file.id).unwrap_or_default()
                    });
                    for line in &matching {
                        let enclosing = file_syms
                            .iter()
                            .filter(|s| {
                                s.line_start as usize <= *line
                                    && *line <= s.line_end as usize
                                    && matches!(
                                        s.kind.as_str(),
                                        "function" | "method" | "constructor"
                                    )
                            })
                            .max_by_key(|s| s.line_start)
                            .map(|s| s.name.clone());
                        sites.push((file.path.clone(), *line, enclosing));
                    }
                }

                if sites.is_empty() {
                    out.push_str("callers: none\n");
                } else {
                    out.push_str(&format!("callers: {}\n", sites.len()));
                    if !concise {
                        for (path, line, encl) in &sites {
                            match encl {
                                Some(fn_name) => {
                                    out.push_str(&format!("  {fn_name} @ {path}:L{line}\n"))
                                }
                                None => out.push_str(&format!("  (top) @ {path}:L{line}\n")),
                            }
                        }
                    }
                }
            }

            if want_callees {
                let all_calls = self.cached_calls(&def_file.path);
                let start = sym.line_start as usize;
                let end = sym.line_end as usize;
                // Dedup by name; keep the first-seen line number only — the
                // detail format includes it but repeats would blow up output
                // on long functions.
                let mut seen_order: Vec<String> = Vec::new();
                let mut first_line: HashMap<String, usize> = HashMap::new();
                for (name, line) in all_calls.iter() {
                    if *line < start || *line > end {
                        continue;
                    }
                    if !first_line.contains_key(name) {
                        first_line.insert(name.clone(), *line);
                        seen_order.push(name.clone());
                    }
                }

                if seen_order.is_empty() {
                    out.push_str("callees: none\n");
                } else {
                    out.push_str(&format!("callees: {}\n", seen_order.len()));
                    if !concise {
                        for callee_name in &seen_order {
                            let _line = first_line[callee_name];
                            let resolved =
                                resolve_cache.entry(callee_name.clone()).or_insert_with(|| {
                                    read::find_symbol_by_name(&conn, callee_name)
                                        .unwrap_or_default()
                                });
                            match resolved.first() {
                                Some((_, f)) => {
                                    out.push_str(&format!("  {callee_name} @ {}\n", f.path))
                                }
                                None => out.push_str(&format!("  {callee_name} (extern)\n")),
                            }
                        }
                    }
                }
            }

            if max_depth > 1 && want_callees {
                let all_calls = self.cached_calls(&def_file.path);
                let start = sym.line_start as usize;
                let end = sym.line_end as usize;
                let direct: Vec<String> = {
                    let mut seen = HashSet::new();
                    let mut ordered = Vec::new();
                    for (n, l) in all_calls.iter() {
                        if *l >= start && *l <= end && seen.insert(n.clone()) {
                            ordered.push(n.clone());
                        }
                    }
                    ordered
                };

                // Global visited set protects against cycles and hub blow-up:
                // the root function and every direct callee are seeded so
                // A → B → A or self-recursion doesn't reappear at depth 2,
                // and a target reached from one root isn't re-listed under
                // another. Without this, hub functions (util!, log, unwrap)
                // would repeat across every branch.
                let mut visited: HashSet<String> = HashSet::new();
                visited.insert(sym.name.clone());
                for d in &direct {
                    visited.insert(d.clone());
                }

                // Group depth-2 chains by their root callee so repeats get
                // elided: `callee → {a, b, c}` instead of three lines.
                let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
                for callee_name in &direct {
                    let resolved = resolve_cache.entry(callee_name.clone()).or_insert_with(|| {
                        read::find_symbol_by_name(&conn, callee_name).unwrap_or_default()
                    });
                    let mut targets: Vec<String> = Vec::new();
                    for (s2, f2) in resolved.iter() {
                        if !matches!(s2.kind.as_str(), "function" | "method") {
                            continue;
                        }
                        let calls2 = self.cached_calls(&f2.path);
                        let s2start = s2.line_start as usize;
                        let s2end = s2.line_end as usize;
                        for (n, l) in calls2.iter() {
                            if *l >= s2start && *l <= s2end && !visited.contains(n) {
                                visited.insert(n.clone());
                                targets.push(n.clone());
                            }
                        }
                    }
                    if !targets.is_empty() {
                        grouped.push((callee_name.clone(), targets));
                    }
                }
                if grouped.is_empty() {
                    out.push_str("depth2: none\n");
                } else {
                    out.push_str("depth2:\n");
                    for (root, targets) in &grouped {
                        if targets.len() == 1 {
                            out.push_str(&format!("  {} → {}\n", root, targets[0]));
                        } else {
                            out.push_str(&format!("  {} → {{{}}}\n", root, targets.join(", ")));
                        }
                    }
                }
            }
        }

        Ok(out)
    }

    #[tool(
        name = "qartez_context",
        description = "Smart context builder: given files you plan to modify, returns the optimal set of related files to read first. Combines dependency graph, co-change history, and PageRank to prioritize what matters.",
        annotations(
            title = "Smart Context",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn qartez_context(
        &self,
        Parameters(params): Parameters<SoulContextParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let budget = params.token_budget.unwrap_or(4000) as usize;
        let concise = is_concise(&params.format);
        let explain = params.explain.unwrap_or(false);
        let limit = params.limit.unwrap_or(15) as usize;

        if params.files.is_empty() {
            return Err("Provide at least one file path in 'files' parameter".to_string());
        }

        // Per-reason breakdown. Keyed by path, each entry tracks the
        // contribution of every signal so `explain=true` can surface the
        // decomposition instead of only the final score.
        let mut scored: HashMap<String, ScoreBreakdown> = HashMap::new();
        let mut input_file_ids: Vec<i64> = Vec::new();

        for file_path in &params.files {
            let file = match read::get_file_by_path(&conn, file_path)
                .map_err(|e| format!("DB error: {e}"))?
            {
                Some(f) => f,
                None => continue,
            };
            input_file_ids.push(file.id);

            let outgoing = read::get_edges_from(&conn, file.id).unwrap_or_default();
            for edge in &outgoing {
                if let Ok(Some(dep)) = read::get_file_by_id(&conn, edge.to_file)
                    && !params.files.contains(&dep.path)
                {
                    scored.entry(dep.path.clone()).or_default().imports +=
                        3.0 + dep.pagerank * 10.0;
                }
            }

            let incoming = read::get_edges_to(&conn, file.id).unwrap_or_default();
            for edge in &incoming {
                if let Ok(Some(imp)) = read::get_file_by_id(&conn, edge.from_file)
                    && !params.files.contains(&imp.path)
                {
                    scored.entry(imp.path.clone()).or_default().importer +=
                        2.0 + imp.pagerank * 5.0;
                }
            }

            let cochanges = read::get_cochanges(&conn, file.id, 10).unwrap_or_default();
            for (cc, partner) in &cochanges {
                if !params.files.contains(&partner.path) {
                    scored.entry(partner.path.clone()).or_default().cochange +=
                        cc.count as f64 * 1.5;
                }
            }

            let blast = blast::blast_radius_for_file(&conn, file.id).unwrap_or_else(|_| {
                blast::BlastResult {
                    file_id: file.id,
                    direct_importers: Vec::new(),
                    transitive_importers: Vec::new(),
                    transitive_count: 0,
                }
            });
            for &imp_id in &blast.transitive_importers {
                if input_file_ids.contains(&imp_id) {
                    continue;
                }
                if let Ok(Some(f)) = read::get_file_by_id(&conn, imp_id)
                    && !params.files.contains(&f.path)
                {
                    scored.entry(f.path.clone()).or_default().transitive += 0.5;
                }
            }
        }

        if let Some(ref task) = params.task {
            let words: Vec<&str> = task.split_whitespace().filter(|w| w.len() > 3).collect();
            for word in &words {
                let fts = if word.contains('*') {
                    word.to_string()
                } else {
                    format!("{word}*")
                };
                if let Ok(results) = read::search_symbols_fts(&conn, &fts, 10) {
                    for (sym, file_path) in &results {
                        if !params.files.contains(file_path) {
                            scored.entry(file_path.clone()).or_default().task_match += 1.0;
                        }
                        let _ = sym;
                    }
                }
            }
        }

        let mut ranked: Vec<(String, ScoreBreakdown)> = scored.into_iter().collect();
        ranked.sort_by(|a, b| {
            b.1.total()
                .partial_cmp(&a.1.total())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let total_candidates = ranked.len();
        let dropped_by_limit = total_candidates.saturating_sub(limit);
        ranked.truncate(limit);

        if ranked.is_empty() {
            return Ok(
                "No related context files found. The specified files may be isolated.".to_string(),
            );
        }

        let mut out = format!(
            "# Context for: {}\n{} related file(s) found:\n\n",
            params.files.join(", "),
            ranked.len(),
        );

        let mut dropped_by_budget: usize = 0;
        for (i, (path, breakdown)) in ranked.iter().enumerate() {
            let line = if concise {
                format!("  {} {}\n", i + 1, path)
            } else if explain {
                format!(
                    "{:>2}. {} (score: {:.1}) — {}\n",
                    i + 1,
                    path,
                    breakdown.total(),
                    breakdown.explain(),
                )
            } else {
                format!(
                    "{:>2}. {} (score: {:.1}) — {}\n",
                    i + 1,
                    path,
                    breakdown.total(),
                    breakdown.reasons().join(", "),
                )
            };
            if estimate_tokens(&out) + estimate_tokens(&line) > budget {
                dropped_by_budget = ranked.len() - i;
                out.push_str("  ... (truncated by token budget)\n");
                break;
            }
            out.push_str(&line);
        }

        if explain && (dropped_by_limit > 0 || dropped_by_budget > 0) {
            out.push_str(&format!(
                "\nExcluded: {} by limit, {} by token budget (candidates={}, limit={}, budget={})\n",
                dropped_by_limit, dropped_by_budget, total_candidates, limit, budget,
            ));
        }

        Ok(out)
    }

    #[tool(
        name = "qartez_hotspots",
        description = "Find hotspot files or functions — code that is simultaneously complex, highly-coupled, and frequently changed. Hotspot score = avg_complexity × pagerank × (1 + change_count). Use before refactoring to identify the highest-risk areas. Requires a prior index with git depth > 0.",
        annotations(
            title = "Hotspot Analysis",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn qartez_hotspots(
        &self,
        Parameters(params): Parameters<SoulHotspotsParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let limit = params.limit.unwrap_or(20) as usize;
        let concise = matches!(params.format, Some(Format::Concise));
        let level = params.level.unwrap_or(HotspotLevel::File);

        match level {
            HotspotLevel::File => {
                let all_files = read::get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;

                // For each file, compute avg complexity of its functions.
                let mut scored: Vec<(String, f64, f64, f64, i64, f64)> = Vec::new();
                for file in &all_files {
                    let symbols = read::get_symbols_for_file(&conn, file.id).unwrap_or_default();
                    let complexities: Vec<u32> =
                        symbols.iter().filter_map(|s| s.complexity).collect();
                    if complexities.is_empty() {
                        continue;
                    }
                    let avg_cc = complexities.iter().copied().sum::<u32>() as f64
                        / complexities.len() as f64;
                    let max_cc = complexities.iter().copied().max().unwrap_or(1) as f64;
                    let coupling = file.pagerank;
                    let churn = file.change_count;
                    // Hotspot score: use max complexity (worst function in the
                    // file), weighted by coupling and change frequency. Adding
                    // 1 to churn avoids zeroing out files with no git history.
                    let score = max_cc * coupling * (1.0 + churn as f64);
                    if score > 0.0 {
                        scored.push((file.path.clone(), score, avg_cc, max_cc, churn, coupling));
                    }
                }

                scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                scored.truncate(limit);

                if scored.is_empty() {
                    return Ok("No hotspots found. Re-index with git history (--git-depth > 0) and imperative language files for complexity data.".to_string());
                }

                let mut out = String::new();
                if concise {
                    out.push_str("# score file avg_cc max_cc churn pagerank\n");
                    for (i, (path, score, avg, max, churn, pr)) in scored.iter().enumerate() {
                        out.push_str(&format!(
                            "{} {:.2} {} {:.1} {:.0} {} {:.4}\n",
                            i + 1,
                            score,
                            path,
                            avg,
                            max,
                            churn,
                            pr,
                        ));
                    }
                } else {
                    out.push_str("# Hotspot Analysis (file level)\n\n");
                    out.push_str(
                        "Hotspot score = max_complexity × pagerank × (1 + change_count)\n\n",
                    );
                    out.push_str("  # | Score     | File                               | AvgCC | MaxCC | Churn | PageRank\n");
                    out.push_str("----+-----------+------------------------------------+-------+-------+-------+---------\n");
                    for (i, (path, score, avg, max, churn, pr)) in scored.iter().enumerate() {
                        out.push_str(&format!(
                            "{:>3} | {:>9.2} | {:<34} | {:>5.1} | {:>5.0} | {:>5} | {:>8.4}\n",
                            i + 1,
                            score,
                            truncate_path(path, 34),
                            avg,
                            max,
                            churn,
                            pr,
                        ));
                    }
                }
                Ok(out)
            }
            HotspotLevel::Symbol => {
                let all_symbols =
                    read::get_all_symbols_with_path(&conn).map_err(|e| format!("DB error: {e}"))?;

                // Pre-load file change counts.
                let all_files = read::get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;
                let file_churn: HashMap<i64, i64> =
                    all_files.iter().map(|f| (f.id, f.change_count)).collect();

                let mut scored: Vec<(String, String, String, f64, u32, f64, i64)> = Vec::new();
                for (sym, file_path) in &all_symbols {
                    let cc = match sym.complexity {
                        Some(c) if c > 0 => c,
                        _ => continue,
                    };
                    let churn = file_churn.get(&sym.file_id).copied().unwrap_or(0);
                    let score = cc as f64 * sym.pagerank * (1.0 + churn as f64);
                    if score > 0.0 {
                        scored.push((
                            sym.name.clone(),
                            sym.kind.clone(),
                            file_path.clone(),
                            score,
                            cc,
                            sym.pagerank,
                            churn,
                        ));
                    }
                }

                scored.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
                scored.truncate(limit);

                if scored.is_empty() {
                    return Ok("No symbol hotspots found. Complexity data requires imperative language files (Rust, TS, Python, Go, etc.).".to_string());
                }

                let mut out = String::new();
                if concise {
                    out.push_str("# score name kind file cc pagerank churn\n");
                    for (i, (name, kind, path, score, cc, pr, churn)) in scored.iter().enumerate() {
                        out.push_str(&format!(
                            "{} {:.4} {} {} {} {} {:.4} {}\n",
                            i + 1,
                            score,
                            name,
                            kind,
                            path,
                            cc,
                            pr,
                            churn,
                        ));
                    }
                } else {
                    out.push_str("# Hotspot Analysis (symbol level)\n\n");
                    out.push_str("Hotspot score = complexity × symbol_pagerank × (1 + file_change_count)\n\n");
                    out.push_str("  # | Score    | Symbol                    | Kind     | File                          | CC | PageRank | Churn\n");
                    out.push_str("----+----------+---------------------------+----------+-------------------------------+----+----------+------\n");
                    for (i, (name, kind, path, score, cc, pr, churn)) in scored.iter().enumerate() {
                        out.push_str(&format!(
                            "{:>3} | {:>8.4} | {:<25} | {:<8} | {:<29} | {:>2} | {:>8.4} | {:>5}\n",
                            i + 1,
                            score,
                            truncate_path(name, 25),
                            truncate_path(kind, 8),
                            truncate_path(path, 29),
                            cc,
                            pr,
                            churn,
                        ));
                    }
                }
                Ok(out)
            }
        }
    }

    #[tool(
        name = "qartez_clones",
        description = "Detect duplicate code: groups of symbols with identical structural shape (same AST skeleton after normalizing identifiers, literals, and comments). Each group is a refactoring opportunity — extract the common logic into a shared function. Use min_lines to filter out trivial matches.",
        annotations(
            title = "Code Clone Detection",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn qartez_clones(
        &self,
        Parameters(params): Parameters<SoulClonesParams>,
    ) -> Result<String, String> {
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let limit = params.limit.unwrap_or(20).max(1) as i64;
        let offset = params.offset.unwrap_or(0) as i64;
        let min_lines = params.min_lines.unwrap_or(5);
        let concise = matches!(params.format, Some(Format::Concise));

        let total =
            read::count_clone_groups(&conn, min_lines).map_err(|e| format!("DB error: {e}"))?;
        if total == 0 {
            return Ok(
                "No code clones detected. All symbols have unique structural shapes.".to_string(),
            );
        }

        let groups = read::get_clone_groups(&conn, min_lines, limit, offset)
            .map_err(|e| format!("DB error: {e}"))?;

        if groups.is_empty() {
            return Ok(format!(
                "No clones in page (total={total}, offset={offset})."
            ));
        }

        let shown = groups.len() as i64;
        let mut out = if shown < total {
            format!(
                "{total} clone group(s) (min {min_lines} lines); showing {shown} from offset {offset} (next: offset={}).\n\n",
                offset + shown
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

            if concise {
                out.push_str(&format!("#{group_num} ({size}x, ~{lines}L):"));
                for (sym, file) in &group.symbols {
                    out.push_str(&format!(" {}:{}", file.path, sym.line_start));
                }
                out.push('\n');
            } else {
                out.push_str(&format!(
                    "## Clone group #{group_num} — {size} duplicates, ~{lines} lines each\n"
                ));
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

    #[tool(
        name = "qartez_wiki",
        description = "Generate a markdown architecture wiki from Leiden-style community detection on the import graph. Partitions files into clusters, names each by the shared path prefix or top-PageRank file, and emits ARCHITECTURE.md with per-cluster file lists, top exported symbols, and inter-cluster edges. Use write_to=null to return the markdown as a string, or write_to=<path> to save to disk. Resolution controls cluster granularity (default 1.0; higher = more clusters).",
        annotations(
            title = "Architecture Wiki",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn qartez_wiki(
        &self,
        Parameters(params): Parameters<SoulWikiParams>,
    ) -> Result<String, String> {
        use crate::graph::boundaries::{check_boundaries, load_config};
        use crate::graph::leiden::LeidenConfig;
        use crate::graph::wiki::{WikiConfig, render_wiki};
        use crate::storage::read::{get_all_edges, get_all_files};

        let leiden = LeidenConfig {
            resolution: params.resolution.unwrap_or(1.0),
            min_cluster_size: params.min_cluster_size.unwrap_or(3),
            ..Default::default()
        };
        let mut wiki_cfg = WikiConfig {
            project_name: self.project_name(),
            max_files_per_section: params
                .max_files_per_section
                .map(|v| v as usize)
                .unwrap_or(20),
            recompute: params.recompute.unwrap_or(false),
            leiden,
            ..Default::default()
        };

        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;

        let boundary_config_path = self.project_root.join(".qartez/boundaries.toml");
        if boundary_config_path.exists() {
            match load_config(&boundary_config_path) {
                Ok(cfg) => {
                    let files = get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;
                    let edges = get_all_edges(&conn).map_err(|e| format!("DB error: {e}"))?;
                    wiki_cfg.boundary_violations = Some(check_boundaries(&cfg, &files, &edges));
                }
                Err(e) => {
                    tracing::warn!("boundary config parse failed: {e}");
                }
            }
        }

        let (markdown, modularity) =
            render_wiki(&conn, &wiki_cfg).map_err(|e| format!("Wiki render error: {e}"))?;
        drop(conn);

        let mod_line = modularity
            .map(|q| format!(", modularity {q:.2}"))
            .unwrap_or_default();
        let cluster_count = markdown
            .lines()
            .filter(|l| l.starts_with("## ") && !l.starts_with("## Table of contents"))
            .count();

        if let Some(path) = params.write_to.as_deref().map(str::trim)
            && !path.is_empty()
        {
            let abs = self.project_root.join(path);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
            }
            std::fs::write(&abs, &markdown)
                .map_err(|e| format!("Cannot write {}: {e}", abs.display()))?;
            return Ok(format!(
                "Wrote {} bytes to {} ({} clusters{})",
                markdown.len(),
                path,
                cluster_count,
                mod_line,
            ));
        }

        Ok(markdown)
    }

    #[tool(
        name = "qartez_boundaries",
        description = "Check architecture boundary rules defined in `.qartez/boundaries.toml` against the import graph. Each rule says files matching `from` must not import files matching any `deny` pattern (with optional `allow` overrides). Returns the list of violating edges. Pass `suggest=true` to emit a starter config derived from the current Leiden clustering instead of running the checker.",
        annotations(
            title = "Architecture Boundaries",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn qartez_boundaries(
        &self,
        Parameters(params): Parameters<SoulBoundariesParams>,
    ) -> Result<String, String> {
        use crate::graph::boundaries::{
            check_boundaries, load_config, render_config_toml, suggest_boundaries,
        };
        use crate::storage::read::{get_all_edges, get_all_file_clusters, get_all_files};

        let rel_path = params
            .config_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(".qartez/boundaries.toml");
        let abs_path = self.project_root.join(rel_path);
        let concise = is_concise(&params.format);

        if params.suggest.unwrap_or(false) {
            let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
            let files = get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;
            let clusters = get_all_file_clusters(&conn).map_err(|e| format!("DB error: {e}"))?;
            let edges = get_all_edges(&conn).map_err(|e| format!("DB error: {e}"))?;
            drop(conn);

            if clusters.is_empty() {
                return Ok(
                    "No cluster assignment found. Run `qartez_wiki` first to compute \
                     clusters, then re-run `qartez_boundaries suggest=true`."
                        .to_string(),
                );
            }

            let cfg = suggest_boundaries(&files, &clusters, &edges);
            let toml = render_config_toml(&cfg);

            if let Some(write_rel) = params.write_to.as_deref().map(str::trim)
                && !write_rel.is_empty()
            {
                let write_abs = self.project_root.join(write_rel);
                if let Some(parent) = write_abs.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
                }
                std::fs::write(&write_abs, &toml)
                    .map_err(|e| format!("Cannot write {}: {e}", write_abs.display()))?;
                return Ok(format!(
                    "Wrote {} rule(s) to {} ({} bytes).",
                    cfg.boundary.len(),
                    write_rel,
                    toml.len(),
                ));
            }

            if cfg.boundary.is_empty() {
                return Ok(
                    "No candidate rules: the current clustering has no directory-aligned \
                     partitions to derive rules from. Try `qartez_wiki recompute=true` first."
                        .to_string(),
                );
            }

            return Ok(toml);
        }

        if !abs_path.exists() {
            return Ok(format!(
                "No boundary config at {rel_path}. Run `qartez_boundaries suggest=true write_to={rel_path}` to generate a starter file."
            ));
        }
        let config = load_config(&abs_path).map_err(|e| format!("Config error: {e}"))?;

        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let files = get_all_files(&conn).map_err(|e| format!("DB error: {e}"))?;
        let edges = get_all_edges(&conn).map_err(|e| format!("DB error: {e}"))?;
        drop(conn);

        let violations = check_boundaries(&config, &files, &edges);
        if violations.is_empty() {
            return Ok(format!(
                "No boundary violations across {} rule(s) and {} edges.",
                config.boundary.len(),
                edges.len(),
            ));
        }

        let mut out = String::new();
        out.push_str(&format!(
            "{} violation(s) across {} rule(s):\n",
            violations.len(),
            config.boundary.len(),
        ));

        if concise {
            for v in &violations {
                out.push_str(&format!(
                    "{} -> {} (rule #{}: deny {})\n",
                    v.from_file, v.to_file, v.rule_index, v.deny_pattern,
                ));
            }
            return Ok(out);
        }

        let mut current_rule: Option<usize> = None;
        for v in &violations {
            if current_rule != Some(v.rule_index) {
                current_rule = Some(v.rule_index);
                let rule = &config.boundary[v.rule_index];
                out.push_str(&format!(
                    "\nRule #{}: {} !-> {}\n",
                    v.rule_index,
                    rule.from,
                    rule.deny.join(" | "),
                ));
            }
            out.push_str(&format!(
                "  {} -> {} (matched deny pattern: {})\n",
                v.from_file, v.to_file, v.deny_pattern,
            ));
        }
        Ok(out)
    }
}

const CALL_NODE_KINDS: &[&str] = &[
    "call_expression",
    "method_invocation",
    "function_call",
    "member_expression",
];

const CALLEE_FIELD_NAMES: &[&str] = &["function", "name", "method"];

fn collect_call_names(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    results: &mut Vec<(String, usize)>,
) {
    loop {
        let node = cursor.node();
        if CALL_NODE_KINDS.contains(&node.kind()) {
            for field in CALLEE_FIELD_NAMES {
                if let Some(callee) = node.child_by_field_name(field) {
                    let name = extract_callee_name(callee, source);
                    if !name.is_empty() {
                        let line = node.start_position().row + 1;
                        results.push((name, line));
                    }
                    break;
                }
            }
            if results
                .last()
                .map(|(_, l)| *l != node.start_position().row + 1)
                .unwrap_or(true)
                && let Some(first_child) = node.child(0)
            {
                let name = extract_callee_name(first_child, source);
                if !name.is_empty() {
                    let line = node.start_position().row + 1;
                    results.push((name, line));
                }
            }
        }

        if cursor.goto_first_child() {
            collect_call_names(cursor, source, results);
            cursor.goto_parent();
        }

        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn extract_callee_name(node: tree_sitter::Node, source: &[u8]) -> String {
    match node.kind() {
        "identifier" | "simple_identifier" | "property_identifier" => {
            node.utf8_text(source).unwrap_or("").to_string()
        }
        "field_expression" | "member_expression" | "scoped_identifier" | "attribute" => {
            if let Some(field) = node
                .child_by_field_name("field")
                .or_else(|| node.child_by_field_name("property"))
                .or_else(|| node.child_by_field_name("name"))
            {
                field.utf8_text(source).unwrap_or("").to_string()
            } else {
                let count = node.child_count();
                if count > 0 {
                    if let Some(last) = node.child((count - 1) as u32) {
                        last.utf8_text(source).unwrap_or("").to_string()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            }
        }
        _ => node.utf8_text(source).unwrap_or("").to_string(),
    }
}

fn capitalize_kind(kind: &str) -> String {
    let mut chars = kind.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => {
            let upper: String = c.to_uppercase().collect();
            let rest: String = chars.collect();
            let singular = format!("{}{}", upper, rest);
            if singular.ends_with('s') || singular.ends_with("sh") || singular.ends_with("ch") {
                format!("{}es", singular)
            } else {
                format!("{}s", singular)
            }
        }
    }
}

fn path_to_import_stem(file_path: &str) -> String {
    let without_ext = file_path
        .rsplit_once('.')
        .map(|(base, _)| base)
        .unwrap_or(file_path);
    without_ext.replace('/', "::")
}

fn relative_import_stem(file_path: &str) -> String {
    let without_ext = file_path
        .rsplit_once('.')
        .map(|(base, _)| base)
        .unwrap_or(file_path);
    let stem = without_ext
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(without_ext);
    stem.to_string()
}

/// Resolve the parent module file that declares `mod <name>;` for a given
/// Rust source file. Covers both the `foo/mod.rs` and flat `foo.rs` module
/// layouts, falling back to the crate root (`lib.rs` / `main.rs`) when the
/// file lives directly under a crate source directory.
///
/// Returns `None` when the file is not a Rust source file or no parent
/// declaration file can be located — callers should skip the mod-decl
/// rewrite in that case rather than erroring out.
fn find_parent_mod_file(
    project_root: &std::path::Path,
    rel_path: &str,
) -> Option<std::path::PathBuf> {
    if !rel_path.ends_with(".rs") {
        return None;
    }
    let path = std::path::Path::new(rel_path);
    let parent = path.parent()?;
    let file_name = path.file_name()?.to_str()?;

    // `foo/mod.rs` declares the `foo` module itself, so the parent is one
    // level up from the directory containing mod.rs.
    let effective_parent: std::path::PathBuf = if file_name == "mod.rs" {
        parent.parent()?.to_path_buf()
    } else {
        parent.to_path_buf()
    };

    // Try in priority order: sibling `<parent>.rs`, nested `<parent>/mod.rs`,
    // then crate roots. Whichever exists on disk wins — Rust's own resolver
    // uses the same candidate set.
    let candidates: Vec<std::path::PathBuf> = if effective_parent.as_os_str().is_empty() {
        // Flat layout: the file sits at the crate root, look for lib/main.
        vec![
            std::path::PathBuf::from("lib.rs"),
            std::path::PathBuf::from("main.rs"),
        ]
    } else {
        let mut v = vec![effective_parent.join("mod.rs")];
        if let Some(parent_of_parent) = effective_parent.parent()
            && let Some(dir_name) = effective_parent.file_name()
        {
            let mut flat = parent_of_parent.to_path_buf();
            flat.push(format!("{}.rs", dir_name.to_string_lossy()));
            v.push(flat);
        }
        // Crate roots as last resort — handles files directly under `src/`.
        v.push(effective_parent.join("lib.rs"));
        v.push(effective_parent.join("main.rs"));
        v
    };

    for cand in candidates {
        let abs = project_root.join(&cand);
        if abs.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Rewrite `mod <old>;` / `pub mod <old>;` declarations in `content` to use
/// `<new>`. Preserves visibility, attributes, and whitespace. Inline modules
/// (`mod foo { ... }`) are left alone because they are not backed by a file
/// and renaming the file has no effect on them.
fn rewrite_mod_decl(content: &str, old: &str, new: &str) -> String {
    // Match `mod foo ;` at start of line or after whitespace, with optional
    // `pub`/`pub(crate)`/etc, and only when terminated by `;` (a file-backed
    // declaration) — never by `{` (an inline module body).
    let pattern = format!(
        r"(?m)^(?P<prefix>\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+){}(?P<suffix>\s*;)",
        regex::escape(old),
    );
    match regex::Regex::new(&pattern) {
        Ok(re) => re
            .replace_all(content, format!("${{prefix}}{new}${{suffix}}"))
            .to_string(),
        Err(_) => content.to_string(),
    }
}

#[cfg(feature = "benchmark")]
impl QartezServer {
    /// Dispatch a tool call by name with JSON arguments.
    ///
    /// Provides a single in-process entry point so the benchmark harness can
    /// invoke any tool without going through the rmcp stdio transport,
    /// keeping latency measurements free of JSON-RPC framing overhead.
    // The `qartez-mcp` binary itself never calls this — only the
    // `benchmark` binary does — so per-bin dead_code analysis sees it as
    // unused when compiling the main server. Suppressing at this scope.
    #[allow(
        dead_code,
        reason = "called only from the feature-gated benchmark binary"
    )]
    pub fn call_tool_by_name(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> std::result::Result<String, String> {
        let de = |v: serde_json::Value| -> std::result::Result<serde_json::Value, String> {
            if v.is_null() {
                Ok(serde_json::json!({}))
            } else {
                Ok(v)
            }
        };
        let args = de(args)?;
        match name {
            "qartez_map" => {
                let p: QartezParams = serde_json::from_value(args).map_err(|e| e.to_string())?;
                Ok(self.qartez_map(Parameters(p)))
            }
            "qartez_find" => {
                let p: SoulFindParams = serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_find(Parameters(p))
            }
            "qartez_read" => {
                let p: SoulReadParams = serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_read(Parameters(p))
            }
            "qartez_impact" => {
                let p: SoulImpactParams =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_impact(Parameters(p))
            }
            "qartez_cochange" => {
                let p: SoulCochangeParams =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_cochange(Parameters(p))
            }
            "qartez_grep" => {
                let p: SoulGrepParams = serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_grep(Parameters(p))
            }
            "qartez_unused" => {
                let p: SoulUnusedParams =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_unused(Parameters(p))
            }
            "qartez_refs" => {
                let p: SoulRefsParams = serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_refs(Parameters(p))
            }
            "qartez_rename" => {
                let p: SoulRenameParams =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_rename(Parameters(p))
            }
            "qartez_project" => {
                let p: SoulProjectParams =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_project(Parameters(p))
            }
            "qartez_move" => {
                let p: SoulMoveParams = serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_move(Parameters(p))
            }
            "qartez_rename_file" => {
                let p: SoulRenameFileParams =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_rename_file(Parameters(p))
            }
            "qartez_outline" => {
                let p: SoulOutlineParams =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_outline(Parameters(p))
            }
            "qartez_deps" => {
                let p: SoulDepsParams = serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_deps(Parameters(p))
            }
            "qartez_stats" => {
                let p: SoulStatsParams = serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_stats(Parameters(p))
            }
            "qartez_calls" => {
                let p: SoulCallsParams = serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_calls(Parameters(p))
            }
            "qartez_context" => {
                let p: SoulContextParams =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_context(Parameters(p))
            }
            "qartez_wiki" => {
                let p: SoulWikiParams = serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_wiki(Parameters(p))
            }
            "qartez_hotspots" => {
                let p: SoulHotspotsParams =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_hotspots(Parameters(p))
            }
            "qartez_clones" => {
                let p: SoulClonesParams =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_clones(Parameters(p))
            }
            "qartez_boundaries" => {
                let p: SoulBoundariesParams =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                self.qartez_boundaries(Parameters(p))
            }
            _ => Err(format!("unknown tool: {name}")),
        }
    }
}

#[tool_handler]
#[prompt_handler]
impl ServerHandler for QartezServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("qartez-mcp", env!("CARGO_PKG_VERSION")))
    }

    fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, ErrorData>> + Send + '_ {
        let resource = Annotated {
            raw: RawResource {
                uri: "qartez://overview".to_string(),
                name: "Codebase Overview".to_string(),
                title: Some("Qartez Codebase Overview".to_string()),
                description: Some(
                    "Ranked overview of the most important files, symbols, and dependency structure"
                        .to_string(),
                ),
                mime_type: Some("text/plain".to_string()),
                size: None,
                icons: None,
                meta: None,
            },
            annotations: None,
        };
        std::future::ready(Ok(ListResourcesResult {
            meta: None,
            resources: vec![resource],
            next_cursor: None,
        }))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResult, ErrorData>> + Send + '_ {
        let result = if request.uri == "qartez://overview" {
            let overview = self.build_overview(20, 4000, None, None, false, false);
            Ok(ReadResourceResult::new(vec![ResourceContents::text(
                overview,
                "qartez://overview",
            )]))
        } else {
            Err(ErrorData::resource_not_found(
                format!("Unknown resource URI: {}", request.uri),
                None,
            ))
        };
        std::future::ready(result)
    }
}

#[cfg(test)]
#[allow(
    clippy::needless_update,
    reason = "test constructions use ..Default::default() uniformly so future field additions don't require touching every site"
)]
mod quality_tests;

#[cfg(test)]
mod prompt_tests;
