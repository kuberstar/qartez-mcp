pub mod fingerprint;
pub mod languages;
pub mod parser;
pub mod symbols;
pub mod walker;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::Result;
use crate::storage::models::SymbolInsert;
use crate::storage::read;
use crate::storage::write;

use parser::ParserPool;
use symbols::{ExtractedImport, ExtractedReference, ReferenceKind, compute_shape_hash};

struct IndexedFile {
    file_id: i64,
    /// DB-stored path (may include a root-name prefix in multi-root mode).
    /// Retained for debugging; import resolvers use `raw_rel` instead.
    #[allow(dead_code)]
    rel_path: String,
    /// Path relative to its own root, without any multi-root prefix.
    /// Used by import resolvers that compute parent directories on disk.
    raw_rel: String,
    language: String,
    imports: Vec<ExtractedImport>,
    /// DB rowids for the symbols this file contributed, in the same order
    /// as the `ExtractedReference::from_symbol_idx` indices emitted by the
    /// language extractor. Used by the reference-resolution pass to
    /// translate parse-local enclosing indices into real symbol ids.
    symbol_ids: Vec<i64>,
    references: Vec<ExtractedReference>,
}

/// Maximum file size to index (bytes). Files larger than this are skipped
/// because they are typically generated and inflate the index without
/// meaningful signal. Override via `QARTEZ_MAX_FILE_BYTES`.
fn max_file_bytes() -> u64 {
    std::env::var("QARTEZ_MAX_FILE_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000_000) // 1 MB default
}

/// Summary of what a (re)index pass changed on disk.
///
/// Returned by [`full_index`], [`full_index_root`], and [`full_index_multi`]
/// so callers can decide whether the expensive global derived tables
/// (PageRank, symbol PageRank, co-change) need recomputing. A pass that
/// touched no files leaves every count at zero, which lets the MCP-server
/// startup path keep running the cheap reconciliation walk on every start
/// while skipping the heavy recompute when nothing actually changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexOutcome {
    /// Files parsed and written because they were new or had a changed mtime.
    pub updated: usize,
    /// Files removed from the index because they no longer exist on disk.
    pub deleted: usize,
}

impl IndexOutcome {
    /// Returns `true` when the pass altered the indexed file set in any way.
    ///
    /// Drives the recompute decision: a pass that neither updated nor deleted
    /// any file cannot have invalidated PageRank or co-change, so those global
    /// recomputes can be skipped.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.updated > 0 || self.deleted > 0
    }

    /// Accumulates another pass's counts into this one.
    ///
    /// Used by [`full_index_multi`] to fold the per-root outcomes of a
    /// multi-root workspace into a single workspace-wide summary.
    fn merge(&mut self, other: IndexOutcome) {
        self.updated += other.updated;
        self.deleted += other.deleted;
    }
}

/// Single-root convenience: indexes one root with no path prefix and no
/// cross-root known paths. This is the common case and preserves the
/// original call signature so all existing callers and tests work unchanged.
pub fn full_index(conn: &Connection, root: &Path, force: bool) -> Result<IndexOutcome> {
    full_index_root(conn, root, force, "", &HashSet::new())
}

/// Index all project roots into the shared database.
///
/// For multi-root mode (more than one root), uses a two-pass approach:
///   1. First pass walks all roots to build a merged `known_paths` set
///   2. Second pass indexes each root with the full cross-root path set
///      so import resolution can find targets in sibling roots.
///
/// File paths are prefixed with the root's directory name to prevent
/// collision on the UNIQUE `files.path` column.
///
/// For single-root mode, delegates to `full_index` with no prefix.
pub fn full_index_multi(
    conn: &Connection,
    roots: &[PathBuf],
    aliases: &HashMap<PathBuf, String>,
    force: bool,
) -> Result<IndexOutcome> {
    if roots.len() <= 1 {
        if let Some(root) = roots.first() {
            return full_index(conn, root, force);
        }
        return Ok(IndexOutcome::default());
    }

    // Two-pass: first pass collects all file paths across every root, then
    // second pass indexes each root with the full cross-root path set so
    // import resolution can find targets in sibling roots.
    //
    // We insert BOTH the prefixed form (for DB lookups and stale-file
    // detection) and the unprefixed raw form (for import resolvers, which
    // generate candidates relative to a single root).
    let roots_with_prefixes: Vec<(&PathBuf, String)> = roots
        .iter()
        .map(|r| (r, root_prefix(r, aliases.get(r).map(|s| s.as_str()))))
        .collect();

    // Purge orphan root-prefixed files before indexing. A workspace
    // entry that was removed from `.qartez/workspace.toml` or whose
    // directory was deleted on disk leaves its files behind under the
    // old prefix - `qartez_unused`, `qartez_clones`, and `qartez_smells`
    // would then surface phantom paths like `relative_alias/src/lib.rs`.
    // Each root's own `remove_stale_files` pass only touches its own
    // prefix, so it cannot catch prefixes that no longer have a root.
    let live_prefixes: HashSet<String> = roots_with_prefixes
        .iter()
        .map(|(_, prefix)| prefix.clone())
        .collect();
    purge_orphan_prefixes(conn, &live_prefixes)?;

    let mut all_known: HashSet<String> = HashSet::new();
    for (root, prefix) in &roots_with_prefixes {
        for file_path in walker::walk_source_files(root) {
            let raw_rel = match file_path.strip_prefix(*root) {
                Ok(p) => to_forward_slash(p.to_string_lossy().into_owned()),
                Err(_) => to_forward_slash(file_path.to_string_lossy().into_owned()),
            };
            all_known.insert(format!("{prefix}/{raw_rel}"));
            all_known.insert(raw_rel);
        }
    }
    let mut outcome = IndexOutcome::default();
    for (root, prefix) in &roots_with_prefixes {
        tracing::info!("Indexing root: {} (prefix: {prefix})", root.display());
        outcome.merge(full_index_root(conn, root, force, prefix, &all_known)?);
    }
    Ok(outcome)
}

/// Delete files belonging to DB root prefixes that are no longer listed
/// in the live workspace roots.
///
/// A DB path is considered prefixed when its first path segment matches
/// a root prefix (e.g. `qartez-public/src/lib.rs` has prefix
/// `qartez-public`). Paths without a slash are always kept (top-level
/// files of the single-root case), as are paths whose prefix is listed
/// in `live_prefixes`. Every other path is removed via
/// `delete_files_by_prefix`, which also clears the associated symbols
/// and FTS rows.
fn purge_orphan_prefixes(conn: &Connection, live_prefixes: &HashSet<String>) -> Result<()> {
    let db_files = read::get_all_files(conn)?;
    let mut orphan_prefixes: HashSet<String> = HashSet::new();
    for f in &db_files {
        if let Some(slash_idx) = f.path.find('/') {
            let prefix = &f.path[..slash_idx];
            if !live_prefixes.contains(prefix) {
                orphan_prefixes.insert(prefix.to_string());
            }
        }
    }
    for prefix in &orphan_prefixes {
        let removed = crate::storage::write::delete_files_by_prefix(conn, prefix)?;
        tracing::info!(
            "purged {removed} file(s) from orphan workspace prefix '{prefix}' (no longer in workspace.toml)"
        );
    }
    Ok(())
}

/// Extract the directory name of a root, used as the path prefix in
/// multi-root mode (e.g. `/home/user/repo-a` -> `"repo-a"`).
///
/// Exposed at crate scope so the watcher can mirror `full_index_multi`'s
/// prefix derivation when handing paths back to `incremental_index`. When
/// an `alias` is provided (via `.qartez/workspace.toml`), it overrides the
/// folder-name derivation.
pub fn root_prefix(root: &Path, alias: Option<&str>) -> String {
    if let Some(a) = alias {
        return a.to_string();
    }
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".to_string())
}

/// Raw replacement: rewrites every `\` to `/` unconditionally.
///
/// Split out from [`to_forward_slash`] so the replacement logic can be
/// unit-tested on every platform without the `MAIN_SEPARATOR` guard masking
/// behavior on Unix. Do not call directly from production code - go through
/// [`to_forward_slash`] so Unix semantics are preserved.
#[inline]
pub(crate) fn replace_backslashes_with_slashes(s: String) -> String {
    if s.contains('\\') {
        s.replace('\\', "/")
    } else {
        s
    }
}

/// Normalize an OS path string so index keys are identical on Unix and Windows.
///
/// Index keys (strings stored in the `files` table and held in
/// `known_paths` / `known_files`) are always forward-slash separated. On
/// Windows `Path::to_string_lossy` and `Path::display` yield `\`-separated
/// strings, which would fail lookups against keys written elsewhere. Guarding
/// on [`std::path::MAIN_SEPARATOR`] keeps this a no-op on Unix, where `\` is
/// a legal filename character that must not be rewritten.
#[inline]
pub(crate) fn to_forward_slash(s: impl Into<String>) -> String {
    let s = s.into();
    if std::path::MAIN_SEPARATOR == '/' {
        s
    } else {
        replace_backslashes_with_slashes(s)
    }
}

/// How an existing DB row for the same path should be reconciled before the
/// re-index writes fresh content.
enum ExistingFileStrategy {
    /// Drop everything (symbols, outgoing edges, AND incoming edges).
    /// Used by full re-index where every file is being replaced anyway.
    DeleteAll,
    /// Drop only the file's own derived content (symbols, outgoing edges)
    /// while preserving incoming edges. Used by incremental re-index so
    /// unchanged files keep pointing at the now-rewritten file.
    ClearContentOnly,
}

/// Common per-file ingestion: count lines, clear stale rows, upsert the
/// `files` row, write symbols and type relations, and append a tracking
/// entry to `indexed`. Returns the number of symbols inserted so callers
/// can log progress without re-walking the parse result.
#[allow(clippy::too_many_arguments)]
fn ingest_parsed_file(
    tx: &Connection,
    rel_path: String,
    raw_rel: String,
    mtime_ns: i64,
    size_bytes: i64,
    source: &[u8],
    parse_result: symbols::ParseResult,
    language: String,
    strategy: ExistingFileStrategy,
    indexed: &mut Vec<IndexedFile>,
) -> Result<usize> {
    let newline_count = source.iter().filter(|&&b| b == b'\n').count();
    let line_count = if source.last() == Some(&b'\n') || source.is_empty() {
        newline_count as i64
    } else {
        newline_count as i64 + 1
    };

    if let Some(existing) = read::get_file_by_path(tx, &rel_path)? {
        match strategy {
            ExistingFileStrategy::DeleteAll => write::delete_file_data(tx, existing.id)?,
            ExistingFileStrategy::ClearContentOnly => write::clear_file_content(tx, existing.id)?,
        }
    }

    let file_id = write::upsert_file(tx, &rel_path, mtime_ns, size_bytes, &language, line_count)?;

    let symbol_inserts: Vec<SymbolInsert> = parse_result
        .symbols
        .iter()
        .map(|s| SymbolInsert {
            name: s.name.clone(),
            kind: s.kind.as_str().to_string(),
            line_start: s.line_start,
            line_end: s.line_end,
            signature: s.signature.clone(),
            is_exported: s.is_exported,
            shape_hash: compute_shape_hash(source, s.line_start, s.line_end, s.kind.as_str()),
            unused_excluded: s.unused_excluded,
            parent_idx: s.parent_idx,
            complexity: s.complexity,
            owner_type: s.owner_type.clone(),
        })
        .collect();
    let inserted = symbol_inserts.len();

    let symbol_ids = write::insert_symbols(tx, file_id, &symbol_inserts)?;

    if !parse_result.type_relations.is_empty() {
        let tuples: Vec<_> = parse_result
            .type_relations
            .iter()
            .map(|r| {
                (
                    r.sub_name.clone(),
                    r.super_name.clone(),
                    r.kind.as_str().to_string(),
                    r.line,
                )
            })
            .collect();
        write::insert_type_relations(tx, file_id, &tuples)?;
    }

    indexed.push(IndexedFile {
        file_id,
        rel_path,
        raw_rel,
        language,
        imports: parse_result.imports,
        symbol_ids,
        references: parse_result.references,
    });

    Ok(inserted)
}

/// Resolve every entry's import specifiers to target file ids, write the
/// `import` edges, and return a per-file map of resolved target ids that
/// the symbol-reference resolver consumes.
#[allow(clippy::too_many_arguments)]
fn resolve_and_write_import_edges(
    tx: &Connection,
    root: &Path,
    path_prefix: &str,
    indexed: &[IndexedFile],
    known_paths: &HashSet<String>,
    path_to_id: &HashMap<String, i64>,
    go_module: Option<&str>,
    dart_packages: &HashMap<String, String>,
) -> Result<HashMap<i64, HashSet<i64>>> {
    // C/C++ includes resolve against a basename index built once here, not
    // per-import, so `-I include` style lookups stay O(1). See resolve_c_import.
    let c_headers = CHeaderIndex::build(known_paths);
    // Python absolute imports resolve against the project's import roots,
    // derived once from the indexed `__init__.py` set. See
    // discover_python_import_roots.
    let python_roots = discover_python_import_roots(known_paths);
    let mut imports_by_file: HashMap<i64, HashSet<i64>> = HashMap::new();
    for entry in indexed {
        let targets_for_entry = imports_by_file.entry(entry.file_id).or_default();
        for import in &entry.imports {
            let targets = resolve_targets(
                &entry.language,
                &entry.raw_rel,
                &import.source,
                root,
                known_paths,
                go_module,
                Some(dart_packages),
                &c_headers,
                &python_roots,
            );
            for target_rel in &targets {
                // Resolvers work against a single root and yield keys relative
                // to THIS root. In single-root mode DB rows (and `path_to_id`)
                // are unprefixed, so look up directly. In multi-root mode DB
                // rows are root-prefixed, so look up ONLY the prefixed form: a
                // bare-key lookup could otherwise bind to a sibling root whose
                // prefix happens to equal a leading subdirectory segment of this
                // key, writing an edge into the wrong root.
                let target_id = if path_prefix.is_empty() {
                    path_to_id.get(target_rel.as_str()).copied()
                } else {
                    path_to_id
                        .get(format!("{path_prefix}/{target_rel}").as_str())
                        .copied()
                };
                if let Some(target_id) = target_id {
                    write::insert_edge(
                        tx,
                        entry.file_id,
                        target_id,
                        "import",
                        Some(&import.source),
                    )?;
                    targets_for_entry.insert(target_id);
                }
            }
        }
    }
    Ok(imports_by_file)
}

/// Index a single project root into the shared database.
///
/// `path_prefix` is prepended to every relative path stored in the DB. For
/// single-root mode pass `""` so behavior is unchanged. For multi-root mode
/// pass the root's directory name (e.g. `"repo-a"`) so that files from
/// different roots never collide on the UNIQUE `files.path` column.
///
/// `extra_known` is a pre-populated set of paths from other roots. It is
/// merged into the local `known_paths` before import resolution so that
/// cross-root imports can find their targets.
/// Outcome of processing a single source file during full-index ingestion.
enum FileIngestOutcome {
    /// File was parsed and its symbols were appended to `indexed`.
    Ingested,
    /// File exists on disk and in the DB with a matching mtime. Its paths
    /// were recorded in `known_paths` so stale-file cleanup won't touch it.
    Unchanged,
    /// Stat, read, parse failure, or oversized. The caller logged the cause.
    Skipped,
}

/// Process one source file for full indexing: stat, skip-if-unchanged, parse,
/// and ingest. Each fallible step either returns early with an outcome or
/// continues. Extracted from `full_index_root` so the per-file decisions stay
/// isolated from the surrounding pipeline.
#[allow(clippy::too_many_arguments)]
fn try_ingest_file(
    tx: &Connection,
    file_path: &Path,
    root: &Path,
    path_prefix: &str,
    force: bool,
    max_bytes: u64,
    pool: &ParserPool,
    indexed: &mut Vec<IndexedFile>,
    known_paths: &mut HashSet<String>,
) -> Result<FileIngestOutcome> {
    let raw_rel = match file_path.strip_prefix(root) {
        Ok(p) => to_forward_slash(p.to_string_lossy().into_owned()),
        Err(_) => to_forward_slash(file_path.to_string_lossy().into_owned()),
    };
    let rel_path = if path_prefix.is_empty() {
        raw_rel.clone()
    } else {
        format!("{path_prefix}/{raw_rel}")
    };

    let metadata = match std::fs::metadata(file_path) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("cannot stat {}: {e}", file_path.display());
            return Ok(FileIngestOutcome::Skipped);
        }
    };
    let mtime_ns = file_mtime_ns(&metadata);
    let size_bytes = metadata.len() as i64;

    if metadata.len() > max_bytes {
        tracing::debug!(
            "skipping oversized file {} ({} bytes)",
            file_path.display(),
            metadata.len()
        );
        return Ok(FileIngestOutcome::Skipped);
    }

    if !force
        && let Some(existing) = read::get_file_by_path(tx, &rel_path)?
        && existing.mtime_ns == mtime_ns
    {
        known_paths.insert(rel_path.clone());
        if !path_prefix.is_empty() {
            known_paths.insert(raw_rel);
        }
        return Ok(FileIngestOutcome::Unchanged);
    }

    let source = match std::fs::read(file_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("cannot read {}: {e}", file_path.display());
            return Ok(FileIngestOutcome::Skipped);
        }
    };

    let (parse_result, language) = match pool.parse_file(file_path, &source) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("parse failed for {}: {e}", file_path.display());
            return Ok(FileIngestOutcome::Skipped);
        }
    };

    known_paths.insert(rel_path.clone());
    if !path_prefix.is_empty() {
        known_paths.insert(raw_rel.clone());
    }

    let symbols_inserted = ingest_parsed_file(
        tx,
        rel_path,
        raw_rel,
        mtime_ns,
        size_bytes,
        &source,
        parse_result,
        language,
        ExistingFileStrategy::DeleteAll,
        indexed,
    )?;

    tracing::debug!(
        "indexed {} ({} symbols)",
        file_path.display(),
        symbols_inserted
    );

    Ok(FileIngestOutcome::Ingested)
}

/// Delete DB rows for files that exist in the index but are no longer on disk
/// under `root`. Files outside this root's `path_prefix` are left alone so
/// other roots in multi-root mode aren't affected. Returns the delete count.
fn remove_stale_files(
    tx: &Connection,
    root: &Path,
    path_prefix: &str,
    known_paths: &HashSet<String>,
) -> Result<usize> {
    let db_files = read::get_all_files(tx)?;
    let mut deleted: usize = 0;
    for db_file in &db_files {
        if !path_prefix.is_empty() && !db_file.path.starts_with(&format!("{path_prefix}/")) {
            continue;
        }
        if !known_paths.contains(&db_file.path) {
            let disk_rel = if path_prefix.is_empty() {
                db_file.path.clone()
            } else {
                db_file.path[path_prefix.len() + 1..].to_string()
            };
            let full_path = root.join(&disk_rel);
            if !full_path.exists() {
                write::delete_file_data(tx, db_file.id)?;
                deleted += 1;
                tracing::debug!("removed stale file from index: {}", db_file.path);
            }
        }
    }
    Ok(deleted)
}

/// Rebuild semantic embeddings when the model is available on disk.
/// Best-effort: if the model hasn't been downloaded yet, indexing succeeds
/// without embeddings and semantic search returns empty results.
#[cfg(feature = "semantic")]
fn rebuild_semantic_embeddings_if_available(tx: &Connection, root: &Path) {
    let Some(model_dir) = crate::embeddings::default_model_dir() else {
        return;
    };
    if !model_dir.join(crate::embeddings::MODEL_FILENAME).exists() {
        return;
    }
    match crate::embeddings::EmbeddingModel::load(&model_dir) {
        Ok(model) => {
            let roots = vec![root.to_path_buf()];
            if let Err(e) = write::rebuild_embeddings(tx, &model, &roots) {
                tracing::warn!("failed to rebuild embeddings: {e}");
            } else {
                tracing::info!("semantic embeddings rebuilt");
            }
        }
        Err(e) => {
            tracing::warn!("failed to load embedding model: {e}");
        }
    }
}

pub fn full_index_root(
    conn: &Connection,
    root: &Path,
    force: bool,
    path_prefix: &str,
    extra_known: &HashSet<String>,
) -> Result<IndexOutcome> {
    let files = walker::walk_source_files(root);
    let pool = ParserPool::new();
    let go_module = read_go_module(root);
    let dart_packages = read_dart_packages(root);
    let max_bytes = max_file_bytes();

    tracing::info!("found {} source files on disk", files.len());

    let tx = conn.unchecked_transaction()?;

    let mut indexed: Vec<IndexedFile> = Vec::new();
    let mut known_paths: HashSet<String> = extra_known.clone();
    let mut skipped: usize = 0;
    let mut updated: usize = 0;

    for file_path in &files {
        match try_ingest_file(
            &tx,
            file_path,
            root,
            path_prefix,
            force,
            max_bytes,
            &pool,
            &mut indexed,
            &mut known_paths,
        )? {
            FileIngestOutcome::Ingested => updated += 1,
            FileIngestOutcome::Unchanged => skipped += 1,
            FileIngestOutcome::Skipped => {}
        }
    }

    let deleted = remove_stale_files(&tx, root, path_prefix, &known_paths)?;

    // Skip the post-walk derived-table rebuilds when the walk touched
    // nothing. With the MCP-server startup path now running this
    // reconciliation on every start (see `main.rs`), an unchanged tree
    // must stay cheap: `sync_fts` and `populate_unused_exports` are
    // whole-table rewrites, and the import/reference passes scan the full
    // known-path set. None of that can change when no file was ingested or
    // removed, so the existing edges, symbol refs, FTS, and unused-exports
    // rows are still valid and are left untouched.
    if updated > 0 || deleted > 0 {
        let path_to_id: HashMap<String, i64> = {
            let all_files = read::get_all_files(&tx)?;
            all_files.into_iter().map(|f| (f.path, f.id)).collect()
        };

        // Import resolution pass: writes edge rows AND records, per file, the
        // set of files we actually imported from. The reference resolver below
        // uses that set as the Priority-2 lookup ("target symbol lives in a
        // file we import").
        let imports_by_file = resolve_and_write_import_edges(
            &tx,
            root,
            path_prefix,
            &indexed,
            &known_paths,
            &path_to_id,
            go_module.as_deref(),
            &dart_packages,
        )?;

        resolve_symbol_references(&tx, &indexed, &imports_by_file)?;

        write::sync_fts(&tx)?;
        // Per-file body FTS rebuild scoped to the files we just (re)ingested.
        // The wholesale `rebuild_symbol_bodies(&tx, root)` we used to call here
        // wipes the entire `symbols_body_fts` table and only repopulates files
        // reachable from `root`, which silently destroyed primary-root bodies
        // on every `qartez_workspace add` for a secondary root. Per-file is
        // safe because changed files already had their body_fts rows cleared
        // via `delete_file_data` / `clear_file_content` inside `try_ingest_file`,
        // and unchanged files retain valid bodies untouched.
        for entry in &indexed {
            write::rebuild_symbol_bodies_for_file(&tx, root, entry.file_id, &entry.raw_rel)?;
        }
        write::populate_unused_exports(&tx)?;

        #[cfg(feature = "semantic")]
        rebuild_semantic_embeddings_if_available(&tx, root);
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    write::set_meta(&tx, "last_index", &timestamp)?;

    tx.commit()?;
    crate::storage::verify_foreign_keys(conn)?;

    // Checkpoint the WAL so it doesn't grow unboundedly across indexing runs.
    // Failure is non-fatal - the next run or SQLite's auto-checkpoint will
    // eventually flush it. Skipped when compaction deferral is enabled
    // (see `set_defer_compaction`) so the MCP background indexer can hand
    // off readiness immediately and let a separate post-index step (or
    // qartez_maintenance checkpoint) flush the WAL off the critical path.
    if compaction_deferred() {
        tracing::debug!("WAL checkpoint deferred (compaction deferral enabled)");
    } else if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
        tracing::debug!("WAL checkpoint after full_index failed (non-fatal): {e}");
    }

    tracing::info!("indexing complete: {updated} updated, {skipped} skipped, {deleted} deleted");
    Ok(IndexOutcome { updated, deleted })
}

/// Process-global flag controlling whether the indexer skips its inline
/// WAL checkpoint. Set via [`set_defer_compaction`]; defaults to `false`.
static DEFER_COMPACTION: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Enable or disable deferral of the indexer's inline WAL checkpoints.
///
/// The MCP-server background indexer enables this so `tools/list` can
/// return as soon as parsing is done, then triggers
/// `wal_checkpoint(TRUNCATE)` itself once startup has handed off. CLI
/// and unit-test paths leave it disabled and keep the original
/// inline-checkpoint behaviour. Replaces the former
/// `QARTEZ_DEFER_COMPACTION` env var, whose `set_var` write was unsound
/// under the multi-threaded tokio runtime (Rust 2024 UB).
pub fn set_defer_compaction(deferred: bool) {
    DEFER_COMPACTION.store(deferred, std::sync::atomic::Ordering::Relaxed);
}

/// Returns true when [`set_defer_compaction`] has been called with `true`
/// so the indexer should skip its inline WAL checkpoint.
fn compaction_deferred() -> bool {
    DEFER_COMPACTION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Second-pass reference resolution. Runs after every file has been parsed
/// and every import edge inserted, so we can attribute each extracted
/// identifier to a concrete `symbols.id` via a same-file → imported-file →
/// global-unique priority. Results are batched and written to
/// `symbol_refs` in a single prepared-statement loop.
///
/// The approach intentionally mirrors Aider's heuristic symbol graph:
/// extractors capture identifiers liberally and this resolver decides -
/// using file-level import edges - which target is the most plausible.
/// Ambiguous names that match many symbols and no import are dropped to
/// keep the edge count manageable on large codebases.
/// Candidate entry in the resolver's name index: symbol id, its file id,
/// its declared symbol kind, and its parent symbol id (when the symbol is
/// nested, e.g. a method inside a class). Kind lets the resolver filter
/// candidates by reference kind. `parent_id` lets the receiver-type
/// heuristic narrow a method call to the class it was declared in.
type Candidate = (i64, i64, String, Option<i64>);

/// Returns true if a symbol of `sym_kind` is a plausible target for a
/// reference of `ref_kind`. Unknown kinds fall through conservatively
/// (we would rather keep a questionable edge than drop a valid one when
/// a language extractor emits a kind we have not mapped here yet).
fn kind_is_compatible(ref_kind: ReferenceKind, sym_kind: &str) -> bool {
    match ref_kind {
        // Plain functions + methods are the obvious case. Classes/structs/
        // enums/interfaces are included because languages like Dart, Java,
        // and Kotlin write constructor calls as `Foo(x)` - syntactically a
        // Call whose target is the type symbol. `type` covers typedefs
        // used as constructor aliases.
        ReferenceKind::Call => matches!(
            sym_kind,
            "function" | "method" | "class" | "struct" | "enum" | "interface" | "trait" | "type"
        ),
        // Type positions resolve only to type-like symbols.
        ReferenceKind::TypeRef => matches!(
            sym_kind,
            "class" | "struct" | "enum" | "interface" | "trait" | "type"
        ),
        // Bare identifier use is too underspecified to filter safely.
        ReferenceKind::Use => true,
    }
}

fn resolve_symbol_references(
    conn: &Connection,
    indexed: &[IndexedFile],
    imports_by_file: &HashMap<i64, HashSet<i64>>,
) -> Result<()> {
    // (name -> [(symbol_id, file_id, kind, parent_id)]) built once for the
    // whole project. `type_by_name` is a parallel index restricted to
    // type-like symbols; the receiver-type heuristic walks it to resolve a
    // hint like `Foo` to the set of symbol ids declaring a class/struct/
    // enum/interface/trait/type named `Foo`.
    let all_syms = read::get_all_symbols_with_path(conn)?;
    let mut name_index: HashMap<String, Vec<Candidate>> = HashMap::with_capacity(all_syms.len());
    let mut type_by_name: HashMap<String, HashSet<i64>> = HashMap::new();
    // Secondary index: symbol_id -> owner_type, for same-impl-block lookups.
    let mut owner_by_id: HashMap<i64, String> = HashMap::new();
    // file_id -> file stem (filename without extension). Used by the
    // qualifier heuristic to disambiguate same-named types defined in
    // different files: `cli::Cli::parse()` should resolve to `Cli` in
    // `cli.rs`, not any of the `Cli` structs in `bin/benchmark.rs`,
    // `bin/guard.rs`, or `bin/setup.rs`. The module segment of the
    // scoped path (extractor-side) matches the file stem here.
    let mut file_stem_by_id: HashMap<i64, String> = HashMap::new();
    for (sym, path) in &all_syms {
        name_index.entry(sym.name.clone()).or_default().push((
            sym.id,
            sym.file_id,
            sym.kind.clone(),
            sym.parent_id,
        ));
        if matches!(
            sym.kind.as_str(),
            "class" | "struct" | "enum" | "interface" | "trait" | "type"
        ) {
            type_by_name
                .entry(sym.name.clone())
                .or_default()
                .insert(sym.id);
        }
        if let Some(ref ot) = sym.owner_type {
            owner_by_id.insert(sym.id, ot.clone());
        }
        file_stem_by_id.entry(sym.file_id).or_insert_with(|| {
            std::path::Path::new(path.as_str())
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string()
        });
    }

    let mut batch: Vec<(i64, i64, &'static str)> = Vec::new();
    let mut resolved = 0usize;
    let mut resolved_by_qualifier = 0usize;
    let mut resolved_by_impl_block = 0usize;
    let mut dropped_no_enclosing = 0usize;
    let mut dropped_no_candidate = 0usize;
    let mut dropped_ambiguous = 0usize;
    let mut resolved_by_kind_filter = 0usize;
    let mut resolved_by_receiver_type = 0usize;

    for entry in indexed {
        let empty_imports = HashSet::new();
        let imported = imports_by_file
            .get(&entry.file_id)
            .unwrap_or(&empty_imports);

        for reference in &entry.references {
            // Module-scope references (no enclosing symbol) arise from
            // proc-macro DSLs emitted at file scope, e.g. `tool_router! {
            // QartezParams => qartez_handler, }`. Attribute them to the
            // file's first indexed symbol so the target still receives a
            // real `symbol_refs` row and is not flagged as unused. PageRank
            // drift is bounded because these synthetic edges land on a
            // symbol that already ranks high for the file.
            let from_id = match reference.from_symbol_idx {
                Some(idx) => match entry.symbol_ids.get(idx) {
                    Some(&id) => id,
                    None => {
                        dropped_no_enclosing += 1;
                        continue;
                    }
                },
                None => match entry.symbol_ids.first() {
                    Some(&id) => id,
                    None => {
                        dropped_no_enclosing += 1;
                        continue;
                    }
                },
            };

            let raw_candidates = match name_index.get(&reference.name) {
                Some(c) if !c.is_empty() => c.as_slice(),
                _ => {
                    dropped_no_candidate += 1;
                    continue;
                }
            };

            // Kind filter: restrict candidates to kinds that a reference
            // of this kind could plausibly resolve to. Keeps an ambiguous
            // name (e.g. a variable `length` and a method `length`) from
            // being dropped at P3 when one of the candidates is the only
            // plausible target given the call-vs-type context.
            let filtered: Vec<&Candidate> = raw_candidates
                .iter()
                .filter(|(_, _, k, _)| kind_is_compatible(reference.kind, k))
                .collect();
            let narrowed_by_kind = !filtered.is_empty() && filtered.len() < raw_candidates.len();
            // Fall back to the raw list if kind-filtering erased every
            // option - avoids silently dropping edges when a language
            // extractor emits a kind this resolver has not mapped.
            //
            // Exception: when the ref is a method-syntax Call
            // (`via_method_syntax`) and kind-filtering dropped everything,
            // the candidates are guaranteed to be the wrong kind (fields,
            // variables, or constants named `filter`, `map`, ...). Falling
            // back would resolve `.filter()` to a struct field named
            // `filter` in an imported file. Drop as no-candidate instead.
            let candidates: Vec<&Candidate> = if filtered.is_empty() {
                if reference.via_method_syntax && reference.kind == ReferenceKind::Call {
                    dropped_no_candidate += 1;
                    continue;
                }
                raw_candidates.iter().collect()
            } else {
                filtered
            };

            let mut picked: Vec<i64> = Vec::new();
            let mut via_receiver = false;

            // Heuristic 1: Qualifier-based matching (from scoped_identifier).
            // When the reference has a qualifier (e.g. `Foo::new()`,
            // qualifier = "Foo"), strongly prefer candidates whose
            // owner_type matches the qualifier. Two flavours of match,
            // tried in order before falling back to the broader priorities:
            //
            //   * owner_type match (`Foo::new` -> impl Foo { fn new }).
            //   * file-stem match (`cli::Cli` -> struct Cli in `cli.rs`).
            //     This is what lets us pick one file's `Cli` out of several
            //     same-named definitions spread across bin crates.
            //
            // Each match runs first in same-file, then imported-file, then
            // unique-global scope, so a local hit always wins over a distant
            // ambiguous one.
            let qualifier_matches = |sid: &i64, fid: &i64, qual: &str| -> bool {
                owner_by_id.get(sid).map(|o| o.as_str()) == Some(qual)
                    || file_stem_by_id.get(fid).map(|s| s.as_str()) == Some(qual)
            };
            if let Some(ref qual) = reference.qualifier {
                // First try: qualifier match in same file.
                picked = candidates
                    .iter()
                    .filter(|(sid, fid, _, _)| {
                        *fid == entry.file_id
                            && *sid != from_id
                            && qualifier_matches(sid, fid, qual)
                    })
                    .map(|(sid, _, _, _)| *sid)
                    .collect();
                // Second try: qualifier match in imported files.
                if picked.is_empty() {
                    picked = candidates
                        .iter()
                        .filter(|(sid, fid, _, _)| {
                            imported.contains(fid) && qualifier_matches(sid, fid, qual)
                        })
                        .map(|(sid, _, _, _)| *sid)
                        .collect();
                }
                // Third try: qualifier match anywhere (unique global).
                if picked.is_empty() {
                    let global: Vec<i64> = candidates
                        .iter()
                        .filter(|(sid, fid, _, _)| qualifier_matches(sid, fid, qual))
                        .map(|(sid, _, _, _)| *sid)
                        .collect();
                    if global.len() == 1 {
                        picked = global;
                    }
                }
                if !picked.is_empty() {
                    resolved_by_qualifier += picked.len();
                }
            }

            // Heuristic 2: Receiver-type hint (from typed locals/params/fields).
            // If the extractor attached a receiver-type hint (e.g. Dart's
            // `Foo foo; foo.method()`), narrow to candidates whose `parent_id`
            // points at a symbol named by the hint. Falls through when zero or
            // multiple candidates match (stays conservative).
            if picked.is_empty()
                && let Some(type_name) = reference.receiver_type_hint.as_deref()
                && let Some(type_ids) = type_by_name.get(type_name)
            {
                let hit: Vec<i64> = candidates
                    .iter()
                    .filter_map(|(sid, _, _, pid)| {
                        pid.filter(|p| type_ids.contains(p)).map(|_| *sid)
                    })
                    .collect();
                if hit.len() == 1 {
                    picked = hit;
                    via_receiver = true;
                }
            }

            // Heuristic 3: Same-impl-block priority.
            // When the calling symbol has an owner_type (e.g. it's inside `impl Foo`),
            // prefer targets that share the same owner_type. This handles `self.bar()`
            // calling another method on the same type.
            if picked.is_empty()
                && let Some(from_owner) = owner_by_id.get(&from_id)
            {
                let impl_matches: Vec<i64> = candidates
                    .iter()
                    .filter(|(sid, fid, _, _)| {
                        *fid == entry.file_id
                            && *sid != from_id
                            && owner_by_id.get(sid).map(|o| o.as_str()) == Some(from_owner)
                    })
                    .map(|(sid, _, _, _)| *sid)
                    .collect();
                if !impl_matches.is_empty() {
                    picked = impl_matches;
                    resolved_by_impl_block += picked.len();
                }
            }

            // Priority 4: target lives in the same file as the caller.
            if picked.is_empty() {
                picked = candidates
                    .iter()
                    .filter(|(sid, fid, _, _)| *fid == entry.file_id && *sid != from_id)
                    .map(|(sid, _, _, _)| *sid)
                    .collect();
            }

            // Priority 5: target lives in a file this caller imports from.
            if picked.is_empty() {
                picked = candidates
                    .iter()
                    .filter(|(_, fid, _, _)| imported.contains(fid))
                    .map(|(sid, _, _, _)| *sid)
                    .collect();
            }

            // Priority 6: unique global match. Ambiguous global names are
            // normally dropped - with no import evidence and multiple
            // candidates there is no principled way to pick, and keeping
            // them all would bury the signal under noise on large projects.
            //
            // Exception: bare-name method calls like `watcher.run()` arrive
            // here with no qualifier and no receiver-type hint because the
            // Rust extractor does not run local type inference. Dropping
            // these as ambiguous produces FP "unused" flags on every impl
            // method called only through an instance binding. Trade: accept
            // a small inflation of the use graph (dead methods with the
            // same name as a called one may pick up spurious inbound edges)
            // in exchange for eliminating the "method looks unused" FP
            // class. Candidates are narrowed to methods only before the
            // fan-out, so a same-named free function in the pool never gets
            // a phantom reference from what is syntactically a method call.
            //
            // Refinement for `via_method_syntax`: when the extractor
            // flagged the callee as `receiver.method(...)` we additionally
            // restrict the fan-out to methods in the SAME file as the
            // caller or in a file the caller imports. Global cross-file
            // fan-out on method syntax is the main FP source for generic
            // iterator / Option / Result method names (`filter`, `map`,
            // `collect`) that collide with same-named fields or functions
            // in unrelated files. Dropping those cases silently preserves
            // PageRank accuracy at the cost of the "method looks unused"
            // FP on types disconnected from the caller's import graph.
            if picked.is_empty() {
                if candidates.len() == 1 {
                    // The single-candidate shortcut is safe when either:
                    //  * the reference is NOT a method-syntax Call (plain
                    //    path references have a stable meaning), or
                    //  * the sole candidate is in the caller's file or one
                    //    it imports (the call graph can vouch for it).
                    // Otherwise a `.filter()` whose only same-named indexed
                    // symbol is a free fn in an unrelated file would bind
                    // to that FP. Drop those as ambiguous.
                    let (sole_sid, sole_fid, _, _) = candidates[0];
                    let locally_reachable =
                        *sole_fid == entry.file_id || imported.contains(sole_fid);
                    let method_syntax_cross_file = reference.via_method_syntax
                        && reference.kind == ReferenceKind::Call
                        && reference.qualifier.is_none()
                        && reference.receiver_type_hint.is_none()
                        && !locally_reachable;
                    if method_syntax_cross_file {
                        dropped_ambiguous += 1;
                        continue;
                    }
                    picked.push(*sole_sid);
                } else if reference.kind == ReferenceKind::Call
                    && reference.qualifier.is_none()
                    && reference.receiver_type_hint.is_none()
                {
                    let method_candidates: Vec<i64> = if reference.via_method_syntax {
                        candidates
                            .iter()
                            .filter(|(_, fid, k, _)| {
                                k == "method" && (*fid == entry.file_id || imported.contains(fid))
                            })
                            .map(|c| c.0)
                            .collect()
                    } else {
                        candidates
                            .iter()
                            .filter(|(_, _, k, _)| k == "method")
                            .map(|c| c.0)
                            .collect()
                    };
                    if !method_candidates.is_empty() {
                        picked.extend(method_candidates);
                    } else {
                        dropped_ambiguous += 1;
                        continue;
                    }
                } else {
                    dropped_ambiguous += 1;
                    continue;
                }
            }

            if via_receiver {
                resolved_by_receiver_type += 1;
            }
            if narrowed_by_kind {
                resolved_by_kind_filter += 1;
            }

            for target in picked {
                batch.push((from_id, target, reference.kind.as_str()));
                resolved += 1;
            }
        }
    }

    write::insert_symbol_refs(conn, &batch)?;

    tracing::info!(
        "symbol references: {} resolved ({} by qualifier, {} by impl-block, {} via kind filter, {} via receiver type), \
         {} dropped (no enclosing), {} dropped (no candidate), {} dropped (ambiguous)",
        resolved,
        resolved_by_qualifier,
        resolved_by_impl_block,
        resolved_by_kind_filter,
        resolved_by_receiver_type,
        dropped_no_enclosing,
        dropped_no_candidate,
        dropped_ambiguous,
    );

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn resolve_targets(
    language: &str,
    rel_path: &str,
    specifier: &str,
    root: &Path,
    known_files: &HashSet<String>,
    go_module: Option<&str>,
    dart_packages: Option<&HashMap<String, String>>,
    c_headers: &CHeaderIndex,
    python_roots: &[String],
) -> Vec<String> {
    match language {
        "rust" => resolve_rust_import(rel_path, specifier, known_files)
            .into_iter()
            .collect(),
        "python" => resolve_python_import(rel_path, specifier, known_files, python_roots)
            .into_iter()
            .collect(),
        "go" => resolve_go_import(specifier, known_files, go_module),
        // Infra-as-code: Kustomize/Helm/ArgoCD YAML and Terraform/OpenTofu HCL
        // reference files and directories, not dot-anchored module specifiers.
        "yaml" | "hcl" => {
            resolve_infra_import(rel_path, specifier, root, known_files, language == "hcl")
        }
        "dart" => resolve_dart_import(rel_path, specifier, root, known_files, dart_packages),
        // C/C++ quoted includes are file references, not dot-anchored module
        // specifiers, so they need their own resolver (see `resolve_c_import`).
        "c" | "cpp" => {
            let importing_file = root.join(rel_path);
            resolve_c_import(&importing_file, specifier, root, known_files, c_headers)
                .into_iter()
                .collect()
        }
        _ => {
            let importing_file = root.join(rel_path);
            resolve_import(&importing_file, specifier, root, known_files)
                .into_iter()
                .collect()
        }
    }
}

// --- C / C++ ---

/// File extensions claimed by the C and C++ parsers, kept in sync with
/// `c_lang::CSupport` and `cpp::CppSupport`. A quoted `#include` may target any
/// of them: headers in the overwhelming majority of cases, sources only in rare
/// inline-include setups.
const C_FAMILY_EXTENSIONS: [&str; 8] = ["c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx"];

/// Index of C/C++ files keyed by basename (the final path component).
///
/// Built once per indexing pass so that `resolve_c_import`'s `-I include` style
/// lookup is an O(1) hash hit instead of a full scan of every known path for
/// each `#include`. Non-C projects yield an empty index and never reach
/// `resolve_c_import`, so they pay only for the single pass that builds it.
struct CHeaderIndex {
    by_basename: HashMap<String, Vec<String>>,
}

impl CHeaderIndex {
    /// Build the basename index from the indexed file set, retaining only
    /// C/C++ files so an include never resolves to a same-named file in
    /// another language.
    fn build(known_paths: &HashSet<String>) -> Self {
        let mut by_basename: HashMap<String, Vec<String>> = HashMap::new();
        for path in known_paths {
            if !is_c_family_path(path) {
                continue;
            }
            if let Some(base) = path.rsplit('/').next() {
                by_basename
                    .entry(base.to_string())
                    .or_default()
                    .push(path.clone());
            }
        }
        Self { by_basename }
    }

    /// Return the single indexed file whose path equals `spec` or ends with
    /// `/<spec>`.
    ///
    /// Returns `None` when zero or more than one file matches, so an ambiguous
    /// `#include "util.h"` present under two directories is left unresolved
    /// rather than wired to an arbitrary target.
    fn unique_suffix_match(&self, spec: &str) -> Option<String> {
        let base = spec.rsplit('/').next()?;
        let candidates = self.by_basename.get(base)?;
        let mut hit: Option<&String> = None;
        for cand in candidates {
            // Exact match, or `spec` is a trailing path segment of `cand`. The
            // `/` boundary check stops `bar.h` from matching `foobar.h`.
            let is_match = cand.as_str() == spec
                || cand
                    .strip_suffix(spec)
                    .is_some_and(|prefix| prefix.ends_with('/'));
            if is_match {
                if hit.is_some() {
                    return None;
                }
                hit = Some(cand);
            }
        }
        hit.cloned()
    }
}

/// Whether `path` (a forward-slash, root-relative index key) names a C or C++
/// source or header file.
fn is_c_family_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| C_FAMILY_EXTENSIONS.contains(&ext))
}

/// Resolve a C/C++ quoted `#include "..."` specifier to an indexed file path.
///
/// Unlike JS/TS module specifiers, C include paths are not dot-anchored:
/// `#include "db.h"` and `#include "net/socket.h"` are the common forms and must
/// resolve even though they begin with neither `.` nor `/`. We approximate the
/// compiler's quoted-include search order - without the build system's `-I`
/// flags - by trying, in order:
///
/// 1. relative to the including file's own directory (same-directory includes
///    and `../inc/foo.h` parent-relative ones);
/// 2. relative to the project root (`-I .` style include roots);
/// 3. a unique basename/suffix match across indexed C/C++ files (`-I include`
///    style roots, where `"db.h"` lives at `include/db.h`).
///
/// Returns `None` when nothing resolves or the suffix match is ambiguous.
fn resolve_c_import(
    importing_file: &Path,
    specifier: &str,
    root: &Path,
    known_files: &HashSet<String>,
    c_headers: &CHeaderIndex,
) -> Option<String> {
    // Index keys are always forward-slash; a Windows include may use `\`.
    let spec = specifier.replace('\\', "/");

    // 1. Relative to the including file's own directory.
    if let Some(base_dir) = importing_file.parent() {
        let candidate = normalize_path(&base_dir.join(&spec));
        if let Ok(rel) = candidate.strip_prefix(root) {
            let rel = to_forward_slash(rel.to_string_lossy().into_owned());
            if known_files.contains(&rel) {
                return Some(rel);
            }
        }
    }

    // 2. Relative to the project root.
    let root_candidate = normalize_path(&root.join(&spec));
    if let Ok(rel) = root_candidate.strip_prefix(root) {
        let rel = to_forward_slash(rel.to_string_lossy().into_owned());
        if known_files.contains(&rel) {
            return Some(rel);
        }
    }

    // 3. Unique suffix match against the indexed C/C++ file set.
    c_headers.unique_suffix_match(&spec)
}

// --- Infrastructure-as-code (Kustomize / Helm / ArgoCD / Terraform) ---

/// Resolve an infra path reference to indexed file(s).
///
/// Infra references are plain filesystem paths, not dot-anchored module
/// specifiers: Kustomize `resources: [../base, deployment.yaml]`, a Helm
/// `Chart.yaml` `file://` dependency, an ArgoCD `spec.source.path` (repo-root
/// relative), or a Terraform `module { source = "../modules/x" }`. The extractor
/// stores the raw path; this resolver maps it to file(s) by trying, in order:
///
/// 1. relative to the referencing file's own directory (`./x`, `../x`, bare
///    sibling `deployment.yaml`);
/// 2. relative to the repo root (leading-slash includes, ArgoCD app paths).
///
/// For each base it accepts an exact file, then directory forms gated by
/// language so a reference never crosses file types: for YAML (`for_hcl =
/// false`) a directory containing a `kustomization.yaml`/`Chart.yaml`
/// entrypoint, else a directory of raw manifests (all its top-level
/// `.yaml`/`.yml` files, covering an ArgoCD `path:` to a plain manifest folder);
/// for HCL (`for_hcl = true`) a Terraform module directory (all its top-level
/// `.tf` files). Returns every matching indexed path; empty when nothing local
/// resolves
/// (remote `github.com/...?ref=`, `oci://`, `https://` refs are dropped by the
/// extractor before they reach here).
fn resolve_infra_import(
    rel_path: &str,
    specifier: &str,
    root: &Path,
    known_files: &HashSet<String>,
    for_hcl: bool,
) -> Vec<String> {
    let spec = specifier.strip_prefix("file://").unwrap_or(specifier);
    if spec.contains("://") || spec.starts_with("git@") {
        return Vec::new();
    }
    let spec = spec.trim().trim_end_matches('/');
    if spec.is_empty() || spec == "." {
        return Vec::new();
    }
    // Root-relative form for the repo-root base (strip a leading `/`).
    let spec_rel = spec.trim_start_matches('/');

    // Bases to try, in priority order. A leading `/` means repo-root only.
    let mut bases: Vec<PathBuf> = Vec::new();
    if !spec.starts_with('/')
        && let Some(dir) = Path::new(rel_path).parent()
    {
        bases.push(root.join(dir));
    }
    bases.push(root.to_path_buf());

    for base in bases {
        let abs = normalize_path(&base.join(spec_rel));
        let rel = match abs.strip_prefix(root) {
            Ok(r) => to_forward_slash(r.to_string_lossy().into_owned()),
            Err(_) => continue,
        };
        if rel == rel_path {
            continue;
        }

        // 1. Exact file.
        if known_files.contains(&rel) {
            return vec![rel];
        }

        // 2. Directory entrypoint: kustomize dir or Helm chart dir. YAML only -
        //    an HCL `module` source never points at a kustomization/chart.
        if !for_hcl {
            for entry in [
                "kustomization.yaml",
                "kustomization.yml",
                "Kustomization",
                "Chart.yaml",
            ] {
                let candidate = if rel.is_empty() {
                    entry.to_string()
                } else {
                    format!("{rel}/{entry}")
                };
                if candidate != rel_path && known_files.contains(&candidate) {
                    return vec![candidate];
                }
            }
        }

        let prefix = if rel.is_empty() {
            String::new()
        } else {
            format!("{rel}/")
        };

        // 3. Terraform module directory: every top-level `.tf` file under it.
        //    HCL only - a YAML manifest never references a `.tf` module, and
        //    gating avoids a full scan for the common YAML case.
        if for_hcl {
            let mut tf: Vec<String> = known_files
                .iter()
                .filter(|p| {
                    p.len() > prefix.len()
                        && p.starts_with(&prefix)
                        && p.ends_with(".tf")
                        && !p[prefix.len()..].contains('/')
                        && p.as_str() != rel_path
                })
                .cloned()
                .collect();
            if !tf.is_empty() {
                tf.sort();
                return tf;
            }
        }

        // 4. Directory of raw manifests (e.g. an ArgoCD `path:` pointing at a
        //    directory with no kustomization entrypoint): link to every
        //    top-level YAML manifest in it. YAML only. Skipped for the repo root
        //    so a root-relative miss never links to the entire tree.
        if !for_hcl && !rel.is_empty() {
            let mut manifests: Vec<String> = known_files
                .iter()
                .filter(|p| {
                    p.len() > prefix.len()
                        && p.starts_with(&prefix)
                        && (p.ends_with(".yaml") || p.ends_with(".yml"))
                        && !p[prefix.len()..].contains('/')
                        && p.as_str() != rel_path
                })
                .cloned()
                .collect();
            if !manifests.is_empty() {
                manifests.sort();
                return manifests;
            }
        }
    }

    Vec::new()
}

// --- TypeScript / JavaScript ---

fn resolve_import(
    importing_file: &Path,
    specifier: &str,
    root: &Path,
    known_files: &HashSet<String>,
) -> Option<String> {
    if !specifier.starts_with('.') && !specifier.starts_with('/') {
        return None;
    }

    let base_dir = importing_file.parent()?;
    let resolved = base_dir.join(specifier);
    let resolved = normalize_path(&resolved);
    let resolved_str = resolved.to_string_lossy();

    // ESM fix: .js/.mjs/.jsx/.cjs → .ts/.tsx
    if let Some(base) = resolved_str
        .strip_suffix(".js")
        .or_else(|| resolved_str.strip_suffix(".mjs"))
        .or_else(|| resolved_str.strip_suffix(".cjs"))
    {
        for ext in [".ts", ".tsx", ".d.ts"] {
            let candidate = format!("{base}{ext}");
            if let Ok(rel) = Path::new(&candidate).strip_prefix(root) {
                let rel = to_forward_slash(rel.to_string_lossy().into_owned());
                if known_files.contains(&rel) {
                    return Some(rel);
                }
            }
        }
    }

    if let Some(base) = resolved_str.strip_suffix(".jsx") {
        for ext in [".tsx", ".ts", ".jsx"] {
            let candidate = format!("{base}{ext}");
            if let Ok(rel) = Path::new(&candidate).strip_prefix(root) {
                let rel = to_forward_slash(rel.to_string_lossy().into_owned());
                if known_files.contains(&rel) {
                    return Some(rel);
                }
            }
        }
    }

    let extensions = &["", ".ts", ".tsx", ".js", ".jsx"];
    let index_files = &["/index.ts", "/index.tsx", "/index.js", "/index.jsx"];

    for ext in extensions {
        let candidate = format!("{}{ext}", resolved.to_string_lossy());
        let candidate_path = Path::new(&candidate);
        let rel = match candidate_path.strip_prefix(root) {
            Ok(r) => to_forward_slash(r.to_string_lossy().into_owned()),
            Err(_) => continue,
        };
        if known_files.contains(&rel) {
            return Some(rel);
        }
    }

    for idx in index_files {
        let candidate = format!("{}{idx}", resolved.to_string_lossy());
        let candidate_path = Path::new(&candidate);
        let rel = match candidate_path.strip_prefix(root) {
            Ok(r) => to_forward_slash(r.to_string_lossy().into_owned()),
            Err(_) => continue,
        };
        if known_files.contains(&rel) {
            return Some(rel);
        }
    }

    None
}

// --- Rust ---

fn resolve_rust_import(
    rel_path: &str,
    specifier: &str,
    known_files: &HashSet<String>,
) -> Option<String> {
    let segments: Vec<&str> = specifier.split("::").collect();

    let rest = if segments.len() > 1 {
        segments[1..].join("/")
    } else {
        String::new()
    };

    match segments[0] {
        "crate" => {
            if rest.is_empty() {
                for name in ["src/lib.rs", "src/main.rs", "lib.rs", "main.rs"] {
                    if known_files.contains(name) {
                        return Some(name.to_string());
                    }
                }
                None
            } else {
                try_rust_module(&rest, known_files, &["src/", ""])
            }
        }
        "super" => {
            let file_path = Path::new(rel_path);
            let file_name = file_path.file_name()?.to_str()?;
            let parent = file_path.parent()?;

            let base = if matches!(file_name, "mod.rs" | "lib.rs" | "main.rs") {
                parent.parent()?
            } else {
                parent
            };

            if rest.is_empty() {
                try_rust_module_file(base, known_files)
            } else {
                let target = if base.as_os_str().is_empty() {
                    rest
                } else {
                    format!(
                        "{}/{rest}",
                        to_forward_slash(base.to_string_lossy().into_owned())
                    )
                };
                try_rust_module(&target, known_files, &[""])
            }
        }
        "self" => {
            if rest.is_empty() {
                return None;
            }
            let file_path = Path::new(rel_path);
            let file_name = file_path.file_name()?.to_str()?;
            let parent = file_path.parent()?;

            let self_dir = if matches!(file_name, "mod.rs" | "lib.rs" | "main.rs") {
                to_forward_slash(parent.to_string_lossy().into_owned())
            } else {
                let stem = file_path.file_stem()?.to_str()?;
                if parent.as_os_str().is_empty() {
                    stem.to_string()
                } else {
                    format!(
                        "{}/{stem}",
                        to_forward_slash(parent.to_string_lossy().into_owned())
                    )
                }
            };

            let target = if self_dir.is_empty() {
                rest
            } else {
                format!("{self_dir}/{rest}")
            };
            try_rust_module(&target, known_files, &[""])
        }
        _ => None,
    }
}

fn try_rust_module(path: &str, known_files: &HashSet<String>, prefixes: &[&str]) -> Option<String> {
    for prefix in prefixes {
        for suffix in [".rs", "/mod.rs"] {
            let candidate = format!("{prefix}{path}{suffix}");
            if known_files.contains(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn try_rust_module_file(dir: &Path, known_files: &HashSet<String>) -> Option<String> {
    let dir_str = to_forward_slash(dir.to_string_lossy().into_owned());
    for name in ["mod.rs", "lib.rs", "main.rs"] {
        let candidate = if dir_str.is_empty() {
            name.to_string()
        } else {
            format!("{dir_str}/{name}")
        };
        if known_files.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

// --- Python ---

fn resolve_python_import(
    rel_path: &str,
    specifier: &str,
    known_files: &HashSet<String>,
    import_roots: &[String],
) -> Option<String> {
    // Absolute imports (`from pkg.mod import x`, `import pkg.mod`) do not begin
    // with a dot. Resolve them against the discovered import roots - the
    // directories that sit on the interpreter's `sys.path` for in-repo code
    // (repo root for a flat layout, `src` for a src-layout, and so on). See
    // `discover_python_import_roots`.
    if !specifier.starts_with('.') {
        return resolve_python_absolute(specifier, known_files, import_roots);
    }

    let dot_count = specifier.chars().take_while(|&c| c == '.').count();
    let module_part = &specifier[dot_count..];

    let file_path = Path::new(rel_path);
    let mut base = file_path.parent()?.to_path_buf();

    for _ in 0..dot_count.saturating_sub(1) {
        base = base.parent()?.to_path_buf();
    }

    let module_path = module_part.replace('.', "/");
    let target = if module_path.is_empty() {
        to_forward_slash(base.to_string_lossy().into_owned())
    } else if base.as_os_str().is_empty() {
        module_path
    } else {
        format!(
            "{}/{module_path}",
            to_forward_slash(base.to_string_lossy().into_owned())
        )
    };

    for suffix in [".py", "/__init__.py"] {
        let candidate = format!("{target}{suffix}");
        if known_files.contains(&candidate) {
            return Some(candidate);
        }
    }

    None
}

/// Resolve a non-relative Python import (`pkg.sub.mod`) to an indexed file by
/// trying each import root in turn. Returns the first
/// `<root>/pkg/sub/mod.py` or `<root>/pkg/sub/mod/__init__.py` present in the
/// index. Returns `None` for standard-library and third-party imports, since
/// those modules live outside the indexed tree.
fn resolve_python_absolute(
    specifier: &str,
    known_files: &HashSet<String>,
    import_roots: &[String],
) -> Option<String> {
    let module_path = specifier.replace('.', "/");
    for root in import_roots {
        let base = if root.is_empty() {
            module_path.clone()
        } else {
            format!("{root}/{module_path}")
        };
        for suffix in [".py", "/__init__.py"] {
            let candidate = format!("{base}{suffix}");
            if known_files.contains(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Discover the Python import roots for a project: the directories from which
/// absolute imports resolve (the interpreter's in-repo `sys.path` entries).
///
/// We derive them from the indexed `__init__.py` files rather than parsing
/// every build backend's config. A package directory is one that contains
/// `__init__.py`; an import root is the parent of a *top-level* package (one
/// whose parent directory is not itself a package). A flat layout yields `""`
/// (repo root), a `src/` layout yields `"src"`, and a monorepo yields one root
/// per project. Namespace packages without `__init__.py` are not discovered -
/// those projects would need explicit build-config parsing.
fn discover_python_import_roots(known_paths: &HashSet<String>) -> Vec<String> {
    let package_dirs: HashSet<&str> = known_paths
        .iter()
        .filter_map(|p| p.strip_suffix("/__init__.py"))
        .collect();

    let mut roots: HashSet<String> = HashSet::new();
    for dir in &package_dirs {
        // Parent of this package directory ("" when it sits at the repo root).
        let parent = dir.rsplit_once('/').map_or("", |(head, _)| head);
        // Only a top-level package contributes an import root; a nested
        // package shares the root of its ancestor.
        if !package_dirs.contains(parent) {
            roots.insert(parent.to_string());
        }
    }

    let mut roots: Vec<String> = roots.into_iter().collect();
    roots.sort(); // deterministic resolution order across runs
    roots
}

// --- Go ---

fn resolve_go_import(
    specifier: &str,
    known_files: &HashSet<String>,
    go_module: Option<&str>,
) -> Vec<String> {
    let module_prefix = match go_module {
        Some(m) => m,
        None => return vec![],
    };

    let rel_dir = match specifier.strip_prefix(module_prefix) {
        Some(rest) => rest.trim_start_matches('/'),
        None => return vec![],
    };

    if rel_dir.is_empty() {
        return vec![];
    }

    known_files
        .iter()
        .filter(|f| {
            if !f.ends_with(".go") {
                return false;
            }
            match Path::new(f.as_str()).parent() {
                Some(p) => to_forward_slash(p.to_string_lossy().into_owned()) == rel_dir,
                None => false,
            }
        })
        .cloned()
        .collect()
}

fn read_go_module(root: &Path) -> Option<String> {
    let content = std::fs::read_to_string(root.join("go.mod")).ok()?;
    content.lines().find_map(|line| {
        line.trim()
            .strip_prefix("module ")
            .map(|m| m.trim().to_string())
    })
}

// --- Dart ---

/// Resolves a Dart `import`/`part` specifier to a file path relative to `root`.
///
/// Handles three specifier shapes:
///   * `dart:io`, `dart:async` - SDK, not in the workspace, return empty.
///   * `package:NAME/a/b.dart` - look up NAME in the workspace package map
///     and rewrite to `<pkg-dir>/lib/a/b.dart`.
///   * relative (`./x.dart`, `../x.dart`, `x.dart`) - including `part`
///     directives, which always carry a relative URI - fall through to the
///     generic relative resolver.
///
/// **Scope:** workspace-only. Only packages whose `pubspec.yaml` lives inside
/// `root` are resolvable; path-/git-dependencies outside the workspace and
/// pub-cache packages are intentionally ignored. We do not consult
/// `.dart_tool/package_config.json` - it requires `pub get` to be fresh and
/// would pull cache paths that are irrelevant for symbol indexing. A
/// `package:` import whose package name is not in the workspace map returns
/// no edge.
fn resolve_dart_import(
    rel_path: &str,
    specifier: &str,
    root: &Path,
    known_files: &HashSet<String>,
    dart_packages: Option<&HashMap<String, String>>,
) -> Vec<String> {
    if specifier.starts_with("dart:") {
        return vec![];
    }

    if let Some(rest) = specifier.strip_prefix("package:") {
        let packages = match dart_packages {
            Some(p) => p,
            None => return vec![],
        };
        let (name, tail) = match rest.split_once('/') {
            Some(parts) => parts,
            None => return vec![],
        };
        let pkg_dir = match packages.get(name) {
            Some(d) => d,
            None => return vec![],
        };
        let candidate = if pkg_dir.is_empty() {
            format!("lib/{tail}")
        } else {
            format!("{pkg_dir}/lib/{tail}")
        };
        let normalized = to_forward_slash(
            normalize_path(Path::new(&candidate))
                .to_string_lossy()
                .into_owned(),
        );
        if known_files.contains(&normalized) {
            return vec![normalized];
        }
        return vec![];
    }

    let importing_file = root.join(rel_path);
    resolve_import(&importing_file, specifier, root, known_files)
        .into_iter()
        .collect()
}

/// Walks the workspace for `pubspec.yaml` files and returns a map from each
/// declared package name to its directory (relative to `root`, forward-slash
/// form, empty string for a pubspec at the root). Used by
/// `resolve_dart_import` to translate `package:foo/…` imports to real files.
fn read_dart_packages(root: &Path) -> HashMap<String, String> {
    let mut packages = HashMap::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if path.file_name().and_then(|n| n.to_str()) != Some("pubspec.yaml") {
            continue;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let Some(name) = parse_pubspec_name(&content) else {
            continue;
        };

        let rel_dir = match path.parent().and_then(|p| p.strip_prefix(root).ok()) {
            Some(p) => to_forward_slash(p.to_string_lossy().into_owned()),
            None => continue,
        };

        packages.insert(name, rel_dir);
    }

    packages
}

/// Extracts the top-level `name:` field from a `pubspec.yaml` body. Only
/// unindented `name:` keys count - an indented `name:` under some other
/// mapping must not hijack the package identity.
fn parse_pubspec_name(pubspec: &str) -> Option<String> {
    for raw in pubspec.lines() {
        let line = raw.split('#').next().unwrap_or("");
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        if let Some(rest) = line.strip_prefix("name:") {
            let value = rest.trim().trim_matches(|c| c == '"' || c == '\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

// --- Helpers ---

fn normalize_path(path: &Path) -> std::path::PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => {
                components.push(other);
            }
        }
    }
    components.iter().collect()
}

fn file_mtime_ns(metadata: &std::fs::Metadata) -> i64 {
    use std::time::UNIX_EPOCH;
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Incrementally re-index only the files that the watcher reported as
/// changed or deleted. Avoids the O(n) filesystem walk that `full_index`
/// performs, and only re-parses the files that actually changed.
///
/// `changed` - paths that were created or modified on disk.
/// `deleted` - paths that were removed from disk.
///
/// After updating the per-file rows, the function re-resolves import
/// edges and symbol references for the changed files, then rebuilds the
/// global FTS and unused-export tables.
/// Remove a single deleted file's rows from the index, if present.
///
/// `path_prefix` must match the root's prefix from [`root_prefix`] in
/// multi-root mode (empty string for single-root). Without it, the lookup
/// key would be the raw relative path and the prefixed row would leak.
/// A `symbol_refs` row whose `to_symbol_id` will be wiped by the
/// about-to-run `clear_file_content` cascade. The snapshot records enough
/// to re-link the ref to the new symbol id (`(to_file, to_name, to_kind)`
/// identifies the target across the re-ingest, and `from_symbol_id` is
/// stable because the caller lives in an unchanged file).
struct PreservedRef {
    from_symbol_id: i64,
    to_file_path: String,
    to_name: String,
    to_kind: String,
    ref_kind: String,
}

/// Snapshot every `symbol_refs` row whose `to_symbol_id` is in a file that
/// is about to be re-ingested while the `from_symbol_id` lives in a file
/// that is NOT being re-ingested. Those are the rows that CASCADE would
/// delete and `resolve_symbol_references` would not recreate.
fn snapshot_cross_file_refs(
    tx: &Connection,
    changed_paths: &[String],
) -> Result<Vec<PreservedRef>> {
    if changed_paths.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = changed_paths
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT sr.from_symbol_id, ft.path, st.name, st.kind, sr.kind
         FROM symbol_refs sr
         JOIN symbols st ON sr.to_symbol_id = st.id
         JOIN files ft ON st.file_id = ft.id
         JOIN symbols sf ON sr.from_symbol_id = sf.id
         JOIN files ff ON sf.file_id = ff.id
         WHERE ft.path IN ({placeholders})
           AND ff.path NOT IN ({placeholders})"
    );
    let mut stmt = tx.prepare(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(changed_paths.len() * 2);
    for p in changed_paths {
        params.push(p);
    }
    for p in changed_paths {
        params.push(p);
    }
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok(PreservedRef {
            from_symbol_id: row.get(0)?,
            to_file_path: row.get(1)?,
            to_name: row.get(2)?,
            to_kind: row.get(3)?,
            ref_kind: row.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Re-insert preserved refs by looking the `(file, name, kind)` target up
/// in the post-reingest symbol table. A unique match relinks the ref; a
/// missing or ambiguous match is dropped because any guess would point at
/// the wrong symbol and bury real call-graph signal.
fn restore_cross_file_refs(tx: &Connection, preserved: &[PreservedRef]) -> Result<()> {
    if preserved.is_empty() {
        return Ok(());
    }
    let mut lookup = tx.prepare(
        "SELECT s.id FROM symbols s
         JOIN files f ON s.file_id = f.id
         WHERE f.path = ?1 AND s.name = ?2 AND s.kind = ?3
         LIMIT 2",
    )?;
    let mut from_exists = tx.prepare("SELECT 1 FROM symbols WHERE id = ?1")?;
    let mut batch: Vec<(i64, i64, String)> = Vec::new();
    for pref in preserved {
        let matches: Vec<i64> = lookup
            .query_map(
                rusqlite::params![pref.to_file_path, pref.to_name, pref.to_kind],
                |row| row.get::<_, i64>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if matches.len() != 1 {
            continue;
        }
        if !from_exists.exists(rusqlite::params![pref.from_symbol_id])? {
            continue;
        }
        batch.push((pref.from_symbol_id, matches[0], pref.ref_kind.clone()));
    }
    drop(lookup);
    drop(from_exists);
    if batch.is_empty() {
        return Ok(());
    }
    let batch_slice: Vec<(i64, i64, &str)> =
        batch.iter().map(|(f, t, k)| (*f, *t, k.as_str())).collect();
    write::insert_symbol_refs(tx, &batch_slice)?;
    Ok(())
}

fn delete_single_file(
    tx: &Connection,
    root: &Path,
    path_prefix: &str,
    path: &Path,
) -> Result<bool> {
    // Paths outside `root` cannot be mapped to a DB row: concatenating an
    // absolute path onto `path_prefix` would yield nonsense like
    // "workspace1//tmp/foo.rs" that never matches a stored `files.path`.
    // Surface the unusual event (typically a symlink or mount-point escape
    // out of the watched tree) instead of silently returning Ok(false).
    let raw_rel = match path.strip_prefix(root) {
        Ok(p) => to_forward_slash(p.to_string_lossy().into_owned()),
        Err(_) => {
            tracing::warn!(
                "incremental: delete target {} is outside root {}; skipping",
                path.display(),
                root.display()
            );
            return Ok(false);
        }
    };
    let rel_path = if path_prefix.is_empty() {
        raw_rel
    } else {
        format!("{path_prefix}/{raw_rel}")
    };
    if let Some(existing) = read::get_file_by_path(tx, &rel_path)? {
        write::delete_file_data(tx, existing.id)?;
        return Ok(true);
    }
    Ok(false)
}

/// Re-index a single file that was reported as changed by the watcher.
/// Returns `true` if the file was ingested, `false` if it was skipped due to
/// stat/read/parse failure or oversize. The caller tracks the update count.
fn try_reingest_changed_file(
    tx: &Connection,
    file_path: &Path,
    root: &Path,
    path_prefix: &str,
    max_bytes: u64,
    pool: &ParserPool,
    indexed: &mut Vec<IndexedFile>,
) -> Result<bool> {
    // Same invariant as `delete_single_file`: paths outside `root` would
    // produce a garbage `rel_path` after prefix concatenation and the file
    // would be ingested under a key that can never be looked up again.
    let raw_rel = match file_path.strip_prefix(root) {
        Ok(p) => to_forward_slash(p.to_string_lossy().into_owned()),
        Err(_) => {
            tracing::warn!(
                "incremental: changed file {} is outside root {}; skipping",
                file_path.display(),
                root.display()
            );
            return Ok(false);
        }
    };
    let rel_path = if path_prefix.is_empty() {
        raw_rel.clone()
    } else {
        format!("{path_prefix}/{raw_rel}")
    };

    let metadata = match std::fs::metadata(file_path) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("incremental: cannot stat {}: {e}", file_path.display());
            return Ok(false);
        }
    };
    let mtime_ns = file_mtime_ns(&metadata);
    let size_bytes = metadata.len() as i64;

    if metadata.len() > max_bytes {
        tracing::debug!(
            "incremental: skipping oversized file {} ({} bytes)",
            file_path.display(),
            metadata.len()
        );
        return Ok(false);
    }

    let source = match std::fs::read(file_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("incremental: cannot read {}: {e}", file_path.display());
            return Ok(false);
        }
    };

    let (parse_result, language) = match pool.parse_file(file_path, &source) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("incremental: parse failed for {}: {e}", file_path.display());
            return Ok(false);
        }
    };

    ingest_parsed_file(
        tx,
        rel_path,
        raw_rel,
        mtime_ns,
        size_bytes,
        &source,
        parse_result,
        language,
        ExistingFileStrategy::ClearContentOnly,
        indexed,
    )?;
    Ok(true)
}

/// Incremental re-index for single-root projects. Delegates to
/// [`incremental_index_with_prefix`] with an empty prefix so existing
/// callers and tests keep the original signature.
pub fn incremental_index(
    conn: &Connection,
    root: &Path,
    changed: &[PathBuf],
    deleted: &[PathBuf],
) -> Result<()> {
    incremental_index_with_prefix(conn, root, "", changed, deleted)
}

/// Incremental re-index for a single root. `path_prefix` is the same
/// prefix that [`full_index_multi`] used for this root (from
/// [`root_prefix`]); pass `""` when there is only one root in the project.
pub fn incremental_index_with_prefix(
    conn: &Connection,
    root: &Path,
    path_prefix: &str,
    changed: &[PathBuf],
    deleted: &[PathBuf],
) -> Result<()> {
    if changed.is_empty() && deleted.is_empty() {
        return Ok(());
    }

    let pool = ParserPool::new();
    let go_module = read_go_module(root);
    let dart_packages = read_dart_packages(root);
    let max_bytes = max_file_bytes();

    let tx = conn.unchecked_transaction()?;

    // --- Phase 1: remove deleted files ---
    let mut removed = 0usize;
    for path in deleted {
        if delete_single_file(&tx, root, path_prefix, path)? {
            removed += 1;
        }
    }

    // --- Phase 1.5: snapshot cross-file symbol_refs that would cascade ---
    // When a changed file is re-ingested, `clear_file_content` deletes its
    // old symbols and SQLite CASCADEs every `symbol_refs(*, to=old_sym, *)`
    // row. `resolve_symbol_references` below only iterates files we are
    // about to re-parse, so refs whose `from_symbol_id` lives in an
    // unchanged file would be lost forever without this snapshot.
    let changed_rel_paths: Vec<String> = changed
        .iter()
        .filter_map(|p| {
            let rel = p.strip_prefix(root).ok()?;
            let rel_str = to_forward_slash(rel.to_string_lossy().into_owned());
            if path_prefix.is_empty() {
                Some(rel_str)
            } else {
                Some(format!("{path_prefix}/{rel_str}"))
            }
        })
        .collect();
    let preserved_refs = if changed_rel_paths.is_empty() {
        Vec::new()
    } else {
        snapshot_cross_file_refs(&tx, &changed_rel_paths)?
    };

    // --- Phase 2: re-index changed files ---
    let mut indexed: Vec<IndexedFile> = Vec::new();
    let mut updated = 0usize;

    for file_path in changed {
        if try_reingest_changed_file(
            &tx,
            file_path,
            root,
            path_prefix,
            max_bytes,
            &pool,
            &mut indexed,
        )? {
            updated += 1;
        }
    }

    // --- Phase 3: resolve edges & references for changed files ---
    // Guard the whole edge/reference pass behind a non-empty `indexed` set.
    // On pure-delete watcher events (or when every changed file was
    // skipped) `indexed` is empty, so this block would build the full
    // path→id map, then `resolve_symbol_references` would run its
    // `get_all_symbols_with_path` scan and four whole-project HashMaps only
    // to loop over nothing. `full_index_root` guards the analogous block
    // the same way (`if updated > 0 || deleted > 0`).
    if !indexed.is_empty() {
        // Build the full path→id map from the DB (includes unchanged files).
        let path_to_id: HashMap<String, i64> = {
            let all_files = read::get_all_files(&tx)?;
            all_files.into_iter().map(|f| (f.path, f.id)).collect()
        };
        // Resolvers generate candidates relative to a single root (unprefixed),
        // but DB keys are root-prefixed in multi-root mode. Mirror
        // full_index_root by also exposing this root's files under their
        // unprefixed form, so incremental re-resolution finds same-root targets
        // instead of silently dropping the edited file's edges.
        let known_paths: HashSet<String> = {
            let mut paths: HashSet<String> = path_to_id.keys().cloned().collect();
            if !path_prefix.is_empty() {
                let pfx = format!("{path_prefix}/");
                let unprefixed: Vec<String> = path_to_id
                    .keys()
                    .filter_map(|p| p.strip_prefix(&pfx).map(str::to_string))
                    .collect();
                paths.extend(unprefixed);
            }
            paths
        };

        let imports_by_file = resolve_and_write_import_edges(
            &tx,
            root,
            path_prefix,
            &indexed,
            &known_paths,
            &path_to_id,
            go_module.as_deref(),
            &dart_packages,
        )?;

        resolve_symbol_references(&tx, &indexed, &imports_by_file)?;
    }

    // Restore the snapshot: look each preserved ref up by (file, name,
    // kind) against the newly-inserted symbols. Ambiguous or missing
    // matches are dropped rather than pointing at the wrong symbol.
    // Kept OUTSIDE the guard above: on a pure delete `preserved_refs` is
    // empty so this is a harmless no-op, and any snapshotted refs still
    // need restoring even if this pass ingested no new symbols.
    restore_cross_file_refs(&tx, &preserved_refs)?;

    // --- Phase 4: update derived tables ---
    // Update FTS and body index only for the files that actually changed.
    // This avoids the O(whole-codebase) full-table DELETE+re-insert that
    // sync_fts / rebuild_symbol_bodies would trigger on every file-save
    // event - the primary cause of unbounded WAL growth on large codebases.
    // clear_file_content (called above per file) already removed the old
    // FTS rows, so here we only need to insert the new ones.
    for entry in &indexed {
        write::insert_fts_for_file(&tx, entry.file_id)?;
        write::rebuild_symbol_bodies_for_file(&tx, root, entry.file_id, &entry.rel_path)?;
    }
    // Mark unused_exports stale instead of paying the full DELETE+INSERT
    // scan on every save. The next read through count_unused_exports or
    // get_unused_exports_page rematerializes lazily.
    write::mark_unused_exports_dirty(&tx)?;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    write::set_meta(&tx, "last_index", &timestamp)?;

    tx.commit()?;
    crate::storage::verify_foreign_keys(conn)?;

    // Flush the WAL with a non-blocking PASSIVE checkpoint after each
    // incremental index. PASSIVE merges WAL pages into the main file
    // without waiting for readers/writers and without the fsync+truncate
    // pass that makes TRUNCATE expensive on Windows under NTFS + Defender.
    // The watcher path runs a periodic TRUNCATE on its own cadence so the
    // WAL file still shrinks. Skipped entirely when compaction deferral is
    // enabled (see `set_defer_compaction`) so the MCP startup path can
    // flush the WAL once after the indexing burst finishes instead of N
    // times.
    if compaction_deferred() {
        tracing::debug!("incremental WAL checkpoint deferred (compaction deferral enabled)");
    } else if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);") {
        tracing::debug!("WAL checkpoint after incremental_index failed (non-fatal): {e}");
    }

    tracing::info!(
        "incremental index: {updated} updated, {removed} removed ({} changed, {} deleted input)",
        changed.len(),
        deleted.len(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage;
    use std::collections::HashSet;
    use std::fs;
    use tempfile::TempDir;

    // --- resolve_infra_import: Kustomize / Helm / ArgoCD / Terraform ---------

    fn infra_known(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn infra_resolves_bare_sibling_file() {
        let root = Path::new("/repo");
        let known = infra_known(&["k8s/prod/kustomization.yaml", "k8s/prod/deployment.yaml"]);
        let hit = resolve_infra_import(
            "k8s/prod/kustomization.yaml",
            "deployment.yaml",
            root,
            &known,
            false,
        );
        assert_eq!(hit, vec!["k8s/prod/deployment.yaml"]);
    }

    #[test]
    fn infra_resolves_parent_dir_to_kustomization() {
        let root = Path::new("/repo");
        let known = infra_known(&[
            "k8s/overlays/prod/kustomization.yaml",
            "k8s/base/kustomization.yaml",
        ]);
        let hit = resolve_infra_import(
            "k8s/overlays/prod/kustomization.yaml",
            "../../base",
            root,
            &known,
            false,
        );
        assert_eq!(hit, vec!["k8s/base/kustomization.yaml"]);
    }

    #[test]
    fn infra_resolves_argocd_root_relative_path() {
        let root = Path::new("/repo");
        let known = infra_known(&[
            "argocd/apps/my-app.yaml",
            "k8s-apps/prod/my-app/kustomization.yaml",
        ]);
        // ArgoCD paths are repo-root relative, not relative to the manifest.
        let hit = resolve_infra_import(
            "argocd/apps/my-app.yaml",
            "k8s-apps/prod/my-app",
            root,
            &known,
            false,
        );
        assert_eq!(hit, vec!["k8s-apps/prod/my-app/kustomization.yaml"]);
    }

    #[test]
    fn infra_resolves_terraform_module_dir_to_tf_files() {
        let root = Path::new("/repo");
        let known = infra_known(&[
            "infra/main.tf",
            "modules/vpc/main.tf",
            "modules/vpc/variables.tf",
            "modules/vpc/outputs.tf",
            "modules/vpc/nested/deep.tf",
        ]);
        let mut hit = resolve_infra_import("infra/main.tf", "../modules/vpc", root, &known, true);
        hit.sort();
        // Only top-level .tf files of the module dir, not nested ones.
        assert_eq!(
            hit,
            vec![
                "modules/vpc/main.tf".to_string(),
                "modules/vpc/outputs.tf".to_string(),
                "modules/vpc/variables.tf".to_string(),
            ]
        );
    }

    #[test]
    fn infra_strips_file_scheme_and_skips_remote() {
        let root = Path::new("/repo");
        let known = infra_known(&["charts/app/Chart.yaml", "charts/common/Chart.yaml"]);
        // file:// local dependency resolves to the chart dir entrypoint.
        let hit = resolve_infra_import(
            "charts/app/Chart.yaml",
            "file://../common",
            root,
            &known,
            false,
        );
        assert_eq!(hit, vec!["charts/common/Chart.yaml"]);
        // Remote references never resolve.
        assert!(
            resolve_infra_import(
                "charts/app/Chart.yaml",
                "https://charts.example.com",
                root,
                &known,
                false,
            )
            .is_empty()
        );
        assert!(
            resolve_infra_import(
                "k8s/kustomization.yaml",
                "github.com/org/repo//overlay?ref=v1",
                root,
                &known,
                false,
            )
            .is_empty()
        );
    }

    #[test]
    fn infra_resolves_argocd_dir_of_raw_manifests() {
        let root = Path::new("/repo");
        // An ArgoCD app whose `path:` points at a directory of raw manifests
        // (no kustomization.yaml). Link to every top-level manifest, minus self.
        let known = infra_known(&[
            "apps/foo/application.yaml",
            "apps/foo/deployment.yaml",
            "apps/foo/service.yaml",
            "apps/foo/values.yaml",
            "apps/foo/nested/extra.yaml",
        ]);
        let mut hit =
            resolve_infra_import("apps/foo/application.yaml", "apps/foo", root, &known, false);
        hit.sort();
        assert_eq!(
            hit,
            vec![
                "apps/foo/deployment.yaml".to_string(),
                "apps/foo/service.yaml".to_string(),
                "apps/foo/values.yaml".to_string(),
            ],
            "should link to top-level manifests only, excluding self and nested"
        );
    }

    #[test]
    fn infra_kustomization_dir_wins_over_raw_manifest_fallback() {
        let root = Path::new("/repo");
        // When a directory has a kustomization.yaml, that is the entrypoint -
        // do not also fan out to every raw manifest.
        let known = infra_known(&[
            "apps/bar/application.yaml",
            "apps/bar/kustomization.yaml",
            "apps/bar/deployment.yaml",
        ]);
        let hit =
            resolve_infra_import("apps/bar/application.yaml", "apps/bar", root, &known, false);
        assert_eq!(hit, vec!["apps/bar/kustomization.yaml"]);
    }

    #[test]
    fn infra_language_gating_prevents_cross_language_edges() {
        let root = Path::new("/repo");
        // A YAML ref to a directory that holds only .tf files must NOT link to
        // them (a manifest never depends on a Terraform module).
        let tf_only = infra_known(&["k8s/kustomization.yaml", "mod/main.tf", "mod/vars.tf"]);
        assert!(
            resolve_infra_import("k8s/kustomization.yaml", "../mod", root, &tf_only, false)
                .is_empty(),
            "YAML must not resolve to .tf files"
        );
        // An HCL module ref to a directory that holds only YAML must NOT link to
        // it, and must not treat a kustomization.yaml as a module entrypoint.
        let yaml_only = infra_known(&["infra/main.tf", "mod/kustomization.yaml", "mod/x.yaml"]);
        assert!(
            resolve_infra_import("infra/main.tf", "./mod", root, &yaml_only, true).is_empty(),
            "HCL must not resolve to YAML manifests or kustomization dirs"
        );
    }

    #[test]
    fn infra_never_returns_self_edge() {
        let root = Path::new("/repo");
        let known = infra_known(&["k8s/kustomization.yaml"]);
        // A `.` or same-dir reference must not link the file to itself.
        assert!(
            resolve_infra_import("k8s/kustomization.yaml", ".", root, &known, false).is_empty()
        );
    }

    // --- replace_backslashes_with_slashes: platform-independent ----------
    //
    // These assertions run on every platform because they target the raw
    // replacement, not the `MAIN_SEPARATOR`-guarded wrapper. They pin the
    // exact behavior that Windows builds will see at runtime.

    #[test]
    fn raw_replace_empty_string_is_empty() {
        assert_eq!(replace_backslashes_with_slashes(String::new()), "");
    }

    #[test]
    fn raw_replace_pure_forward_slash_is_unchanged() {
        assert_eq!(
            replace_backslashes_with_slashes("src/lib.rs".to_string()),
            "src/lib.rs"
        );
    }

    #[test]
    fn raw_replace_pure_backslash_becomes_forward_slash() {
        assert_eq!(
            replace_backslashes_with_slashes("src\\lib.rs".to_string()),
            "src/lib.rs"
        );
        assert_eq!(
            replace_backslashes_with_slashes("crates\\foo\\src\\mod.rs".to_string()),
            "crates/foo/src/mod.rs"
        );
    }

    #[test]
    fn raw_replace_mixed_separators_normalize() {
        // Path::join on Windows against a forward-slash base yields mixed.
        assert_eq!(
            replace_backslashes_with_slashes("src/sub\\leaf.rs".to_string()),
            "src/sub/leaf.rs"
        );
        assert_eq!(
            replace_backslashes_with_slashes("a\\b/c\\d/e.rs".to_string()),
            "a/b/c/d/e.rs"
        );
    }

    #[test]
    fn raw_replace_consecutive_backslashes_each_rewritten() {
        // No collapsing - a literal `\\` becomes `//`. Paths like UNC would
        // be mangled, but index keys never contain UNC prefixes, and matching
        // produces the same transformation on both sides.
        assert_eq!(
            replace_backslashes_with_slashes("a\\\\b.rs".to_string()),
            "a//b.rs"
        );
    }

    #[test]
    fn raw_replace_preserves_unicode() {
        assert_eq!(
            replace_backslashes_with_slashes("src\\тест\\файл.rs".to_string()),
            "src/тест/файл.rs"
        );
    }

    #[test]
    fn raw_replace_drive_letter_path_is_normalized() {
        // Realistic Windows absolute path after some code converted via `display()`.
        assert_eq!(
            replace_backslashes_with_slashes("C:\\project\\src\\lib.rs".to_string()),
            "C:/project/src/lib.rs"
        );
    }

    // --- to_forward_slash helper (platform-guarded wrapper) --------------

    #[test]
    fn to_forward_slash_leaves_forward_slashes_alone() {
        assert_eq!(to_forward_slash("src/lib.rs"), "src/lib.rs");
        assert_eq!(to_forward_slash(""), "");
    }

    #[test]
    #[cfg(unix)]
    fn to_forward_slash_preserves_backslashes_on_unix() {
        // On Unix, `\` is a legal filename character. Rewriting would corrupt paths.
        assert_eq!(to_forward_slash("weird\\name.rs"), "weird\\name.rs");
    }

    #[test]
    #[cfg(windows)]
    fn to_forward_slash_rewrites_backslashes_on_windows() {
        assert_eq!(to_forward_slash("src\\lib.rs"), "src/lib.rs");
        assert_eq!(
            to_forward_slash("crates\\foo\\src\\mod.rs"),
            "crates/foo/src/mod.rs"
        );
        // Mixed separators (as produced by Path::join over a forward-slash base)
        // also normalize cleanly.
        assert_eq!(to_forward_slash("src/sub\\leaf.rs"), "src/sub/leaf.rs");
    }

    /// Simulation test: exercises the Windows code path on Unix by feeding
    /// hand-crafted backslash candidate strings through the raw replacement
    /// and matching them against a forward-slash `known_files` set. This
    /// proves that if a Windows build produces a backslash candidate from
    /// `Path::join`, the normalization is sufficient to match the index key.
    #[test]
    fn simulated_windows_candidate_matches_forward_slash_known_key() {
        let mut known: HashSet<String> = HashSet::new();
        known.insert("src/utils.ts".to_string());
        known.insert("crates/foo/src/mod.rs".to_string());

        // Candidate as Windows would produce it after Path::join + strip_prefix.
        let winlike = "src\\utils.ts".to_string();
        let normalized = replace_backslashes_with_slashes(winlike);
        assert!(known.contains(&normalized), "got: {normalized}");

        let winlike2 = "crates\\foo\\src\\mod.rs".to_string();
        let normalized2 = replace_backslashes_with_slashes(winlike2);
        assert!(known.contains(&normalized2), "got: {normalized2}");

        // Negative: a path NOT in the set must still miss.
        let winlike3 = "src\\missing.ts".to_string();
        let normalized3 = replace_backslashes_with_slashes(winlike3);
        assert!(!known.contains(&normalized3));
    }

    /// Platform-agnostic sanity: a PathBuf built with the native separator
    /// must round-trip to a forward-slash index key. On Unix this is trivial,
    /// on Windows it exercises the replacement path.
    #[test]
    fn to_forward_slash_normalizes_native_pathbuf() {
        let mut p = std::path::PathBuf::new();
        p.push("src");
        p.push("lib.rs");
        assert_eq!(
            to_forward_slash(p.to_string_lossy().into_owned()),
            "src/lib.rs"
        );
    }

    /// Regression guard: every file key persisted by `full_index` must use
    /// forward slashes on every platform. Runs on real disk + SQLite so any
    /// future ingest path that forgets to normalize will fail here instead
    /// of silently breaking Windows import resolution again.
    #[test]
    fn full_index_persists_forward_slash_keys() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("nested").join("inside")).unwrap();
        fs::write(root.join("top.rs"), "fn main() {}").unwrap();
        fs::write(root.join("nested/mid.rs"), "fn a() {}").unwrap();
        fs::write(root.join("nested/inside/deep.rs"), "fn b() {}").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, true).unwrap();

        let paths: Vec<String> = read::get_all_files(&conn)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();

        for p in &paths {
            assert!(!p.contains('\\'), "index key contains backslash: {p}");
        }
        assert!(paths.iter().any(|p| p == "top.rs"), "paths: {paths:?}");
        assert!(
            paths.iter().any(|p| p == "nested/mid.rs"),
            "paths: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p == "nested/inside/deep.rs"),
            "paths: {paths:?}"
        );
    }

    // --- TS/JS resolver ---

    #[test]
    fn test_resolve_import_relative() {
        let root = Path::new("/project");
        let importing = Path::new("/project/src/app.ts");
        let mut known = HashSet::new();
        known.insert("src/utils.ts".to_string());

        let result = resolve_import(importing, "./utils", root, &known);
        assert_eq!(result, Some("src/utils.ts".to_string()));
    }

    #[test]
    fn test_resolve_import_parent_dir() {
        let root = Path::new("/project");
        let importing = Path::new("/project/src/components/Button.tsx");
        let mut known = HashSet::new();
        known.insert("src/utils.ts".to_string());

        let result = resolve_import(importing, "../utils", root, &known);
        assert_eq!(result, Some("src/utils.ts".to_string()));
    }

    #[test]
    fn test_resolve_import_index_file() {
        let root = Path::new("/project");
        let importing = Path::new("/project/src/app.ts");
        let mut known = HashSet::new();
        known.insert("src/components/index.ts".to_string());

        let result = resolve_import(importing, "./components", root, &known);
        assert_eq!(result, Some("src/components/index.ts".to_string()));
    }

    #[test]
    fn test_resolve_import_skips_bare_specifier() {
        let root = Path::new("/project");
        let importing = Path::new("/project/src/app.ts");
        let known = HashSet::new();

        let result = resolve_import(importing, "react", root, &known);
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_import_js_to_ts() {
        let root = Path::new("/project");
        let importing = Path::new("/project/src/actions/cpu.ts");
        let mut known = HashSet::new();
        known.insert("src/metrics/cpu.ts".to_string());

        let result = resolve_import(importing, "../metrics/cpu.js", root, &known);
        assert_eq!(result, Some("src/metrics/cpu.ts".to_string()));
    }

    #[test]
    fn test_resolve_import_mjs_to_ts() {
        let root = Path::new("/project");
        let importing = Path::new("/project/src/app.ts");
        let mut known = HashSet::new();
        known.insert("src/utils.ts".to_string());

        let result = resolve_import(importing, "./utils.mjs", root, &known);
        assert_eq!(result, Some("src/utils.ts".to_string()));
    }

    #[test]
    fn test_resolve_import_jsx_to_tsx() {
        let root = Path::new("/project");
        let importing = Path::new("/project/src/App.tsx");
        let mut known = HashSet::new();
        known.insert("src/Button.tsx".to_string());

        let result = resolve_import(importing, "./Button.jsx", root, &known);
        assert_eq!(result, Some("src/Button.tsx".to_string()));
    }

    // --- C / C++ resolver ---

    fn c_index(paths: &[&str]) -> (HashSet<String>, CHeaderIndex) {
        let known: HashSet<String> = paths.iter().map(|&p| p.to_string()).collect();
        let headers = CHeaderIndex::build(&known);
        (known, headers)
    }

    #[test]
    fn test_resolve_c_import_same_directory() {
        let root = Path::new("/project");
        let importing = Path::new("/project/src/util/util.c");
        let (known, headers) = c_index(&["src/util/util.h"]);

        let result = resolve_c_import(importing, "util.h", root, &known, &headers);
        assert_eq!(result, Some("src/util/util.h".to_string()));
    }

    #[test]
    fn test_resolve_c_import_parent_relative() {
        let root = Path::new("/project");
        let importing = Path::new("/project/src/db/db.c");
        let (known, headers) = c_index(&["include/db.h"]);

        let result = resolve_c_import(importing, "../../include/db.h", root, &known, &headers);
        assert_eq!(result, Some("include/db.h".to_string()));
    }

    #[test]
    fn test_resolve_c_import_root_relative() {
        let root = Path::new("/project");
        let importing = Path::new("/project/src/main.c");
        let (known, headers) = c_index(&["include/config.h"]);

        let result = resolve_c_import(importing, "include/config.h", root, &known, &headers);
        assert_eq!(result, Some("include/config.h".to_string()));
    }

    #[test]
    fn test_resolve_c_import_include_root_suffix() {
        // `-I include` layout: `#include "db.h"` from src/, header in include/.
        let root = Path::new("/project");
        let importing = Path::new("/project/src/db/db.c");
        let (known, headers) = c_index(&["include/db.h", "src/db/db.c"]);

        let result = resolve_c_import(importing, "db.h", root, &known, &headers);
        assert_eq!(result, Some("include/db.h".to_string()));
    }

    #[test]
    fn test_resolve_c_import_subdir_suffix() {
        let root = Path::new("/project");
        let importing = Path::new("/project/src/app.c");
        let (known, headers) = c_index(&["src/net/socket.h"]);

        let result = resolve_c_import(importing, "net/socket.h", root, &known, &headers);
        assert_eq!(result, Some("src/net/socket.h".to_string()));
    }

    #[test]
    fn test_resolve_c_import_ambiguous_suffix_is_unresolved() {
        // Two headers share the basename; without `-I` knowledge we refuse to
        // guess which one `#include "util.h"` means.
        let root = Path::new("/project");
        let importing = Path::new("/project/src/app.c");
        let (known, headers) = c_index(&["lib/a/util.h", "lib/b/util.h"]);

        let result = resolve_c_import(importing, "util.h", root, &known, &headers);
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_c_import_suffix_respects_segment_boundary() {
        // `bar.h` must not resolve to `foobar.h`.
        let root = Path::new("/project");
        let importing = Path::new("/project/src/app.c");
        let (known, headers) = c_index(&["vendor/foobar.h"]);

        let result = resolve_c_import(importing, "bar.h", root, &known, &headers);
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_c_import_unknown_is_unresolved() {
        let root = Path::new("/project");
        let importing = Path::new("/project/src/app.c");
        let (known, headers) = c_index(&["src/other.h"]);

        let result = resolve_c_import(importing, "missing.h", root, &known, &headers);
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_c_import_same_dir_wins_over_include_root() {
        // The compiler checks the including file's directory before any `-I`
        // path, so a same-dir header shadows a same-named one under include/.
        let root = Path::new("/project");
        let importing = Path::new("/project/src/db/db.c");
        let (known, headers) = c_index(&["src/db/db.h", "include/db.h"]);

        let result = resolve_c_import(importing, "db.h", root, &known, &headers);
        assert_eq!(result, Some("src/db/db.h".to_string()));
    }

    #[test]
    fn test_resolve_c_import_dot_slash_same_dir() {
        // `#include "./util.h"` - the dotted same-dir form the old generic
        // resolver handled. Must still resolve now that C routes through
        // resolve_c_import (regression guard).
        let root = Path::new("/project");
        let importing = Path::new("/project/src/util/util.c");
        let (known, headers) = c_index(&["src/util/util.h"]);

        let result = resolve_c_import(importing, "./util.h", root, &known, &headers);
        assert_eq!(result, Some("src/util/util.h".to_string()));
    }

    #[test]
    fn test_resolve_c_import_backslash_separator_normalized() {
        // Windows-style backslash separators in the include resolve against the
        // forward-slash index keys.
        let root = Path::new("/project");
        let importing = Path::new("/project/src/app.c");
        let (known, headers) = c_index(&["src/net/socket.h"]);

        let result = resolve_c_import(importing, "net\\socket.h", root, &known, &headers);
        assert_eq!(result, Some("src/net/socket.h".to_string()));
    }

    // --- Rust resolver ---

    #[test]
    fn test_rust_crate_import() {
        let mut known = HashSet::new();
        known.insert("src/storage/read.rs".to_string());

        let result = resolve_rust_import("src/server/mod.rs", "crate::storage::read", &known);
        assert_eq!(result, Some("src/storage/read.rs".to_string()));
    }

    #[test]
    fn test_rust_crate_import_mod() {
        let mut known = HashSet::new();
        known.insert("src/storage/mod.rs".to_string());

        let result = resolve_rust_import("src/server/mod.rs", "crate::storage", &known);
        assert_eq!(result, Some("src/storage/mod.rs".to_string()));
    }

    #[test]
    fn test_rust_crate_import_no_src_prefix() {
        let mut known = HashSet::new();
        known.insert("utils.rs".to_string());

        let result = resolve_rust_import("main.rs", "crate::utils", &known);
        assert_eq!(result, Some("utils.rs".to_string()));
    }

    #[test]
    fn test_rust_super_bare_from_regular_file() {
        let mut known = HashSet::new();
        known.insert("src/index/languages/mod.rs".to_string());

        let result = resolve_rust_import("src/index/languages/rust_lang.rs", "super", &known);
        assert_eq!(result, Some("src/index/languages/mod.rs".to_string()));
    }

    #[test]
    fn test_rust_super_submodule_from_regular_file() {
        let mut known = HashSet::new();
        known.insert("src/storage/models.rs".to_string());

        let result = resolve_rust_import("src/storage/read.rs", "super::models", &known);
        assert_eq!(result, Some("src/storage/models.rs".to_string()));
    }

    #[test]
    fn test_rust_super_from_mod_rs() {
        let mut known = HashSet::new();
        known.insert("src/error.rs".to_string());

        let result = resolve_rust_import("src/storage/mod.rs", "super::error", &known);
        assert_eq!(result, Some("src/error.rs".to_string()));
    }

    #[test]
    fn test_rust_super_bare_from_mod_rs() {
        let mut known = HashSet::new();
        known.insert("src/lib.rs".to_string());

        let result = resolve_rust_import("src/storage/mod.rs", "super", &known);
        assert_eq!(result, Some("src/lib.rs".to_string()));
    }

    #[test]
    fn test_rust_self_from_mod_rs() {
        let mut known = HashSet::new();
        known.insert("src/storage/read.rs".to_string());

        let result = resolve_rust_import("src/storage/mod.rs", "self::read", &known);
        assert_eq!(result, Some("src/storage/read.rs".to_string()));
    }

    #[test]
    fn test_rust_self_from_root_lib_rs_resolves_sibling() {
        let mut known = HashSet::new();
        known.insert("lib.rs".to_string());
        known.insert("bar.rs".to_string());

        let result = resolve_rust_import("lib.rs", "self::bar", &known);
        assert_eq!(result, Some("bar.rs".to_string()));
    }

    #[test]
    fn test_rust_self_from_root_lib_rs_resolves_nested() {
        let mut known = HashSet::new();
        known.insert("lib.rs".to_string());
        known.insert("sub/thing.rs".to_string());

        let result = resolve_rust_import("lib.rs", "self::sub::thing", &known);
        assert_eq!(result, Some("sub/thing.rs".to_string()));
    }

    #[test]
    fn test_rust_external_crate_ignored() {
        let known = HashSet::new();
        let result = resolve_rust_import("src/main.rs", "serde::Serialize", &known);
        assert_eq!(result, None);
    }

    #[test]
    fn test_rust_empty_specifier_returns_none() {
        let mut known = HashSet::new();
        known.insert("lib.rs".to_string());
        let result = resolve_rust_import("lib.rs", "", &known);
        assert_eq!(result, None);
    }

    #[test]
    fn test_rust_unrecognized_root_returns_none() {
        let mut known = HashSet::new();
        known.insert("lib.rs".to_string());
        let result = resolve_rust_import("lib.rs", "external::mod", &known);
        assert_eq!(result, None);
    }

    #[test]
    fn test_rust_self_from_src_lib_rs_resolves_sibling() {
        let mut known = HashSet::new();
        known.insert("src/lib.rs".to_string());
        known.insert("src/bar.rs".to_string());
        let result = resolve_rust_import("src/lib.rs", "self::bar", &known);
        assert_eq!(result, Some("src/bar.rs".to_string()));
    }

    #[test]
    fn test_rust_self_from_src_module_rs_resolves_nested() {
        let mut known = HashSet::new();
        known.insert("src/foo.rs".to_string());
        known.insert("src/foo/bar.rs".to_string());
        let result = resolve_rust_import("src/foo.rs", "self::bar", &known);
        assert_eq!(result, Some("src/foo/bar.rs".to_string()));
    }

    #[test]
    fn test_rust_super_from_src_module_rs() {
        let mut known = HashSet::new();
        known.insert("src/foo.rs".to_string());
        known.insert("src/bar.rs".to_string());
        let result = resolve_rust_import("src/foo.rs", "super::bar", &known);
        assert_eq!(result, Some("src/bar.rs".to_string()));
    }

    #[test]
    fn test_rust_super_from_root_lib_rs() {
        let mut known = HashSet::new();
        known.insert("lib.rs".to_string());
        known.insert("bar.rs".to_string());
        let result = resolve_rust_import("lib.rs", "super::bar", &known);
        assert_eq!(result, None);
    }

    #[test]
    fn test_rust_crate_from_any_file_resolves_root() {
        let mut known = HashSet::new();
        known.insert("src/lib.rs".to_string());
        known.insert("src/bar.rs".to_string());
        let result = resolve_rust_import("src/some/deep/file.rs", "crate::bar", &known);
        assert_eq!(result, Some("src/bar.rs".to_string()));
    }

    // --- Python resolver ---

    #[test]
    fn test_python_relative_single_dot() {
        let mut known = HashSet::new();
        known.insert("pkg/utils.py".to_string());

        let result = resolve_python_import("pkg/main.py", ".utils", &known, &[]);
        assert_eq!(result, Some("pkg/utils.py".to_string()));
    }

    #[test]
    fn test_python_relative_double_dot() {
        let mut known = HashSet::new();
        known.insert("pkg/models.py".to_string());

        let result = resolve_python_import("pkg/sub/module.py", "..models", &known, &[]);
        assert_eq!(result, Some("pkg/models.py".to_string()));
    }

    #[test]
    fn test_python_relative_init() {
        let mut known = HashSet::new();
        known.insert("pkg/utils/__init__.py".to_string());

        let result = resolve_python_import("pkg/main.py", ".utils", &known, &[]);
        assert_eq!(result, Some("pkg/utils/__init__.py".to_string()));
    }

    #[test]
    fn test_python_dotted_module() {
        let mut known = HashSet::new();
        known.insert("pkg/sub/helpers.py".to_string());

        let result = resolve_python_import("pkg/main.py", ".sub.helpers", &known, &[]);
        assert_eq!(result, Some("pkg/sub/helpers.py".to_string()));
    }

    #[test]
    fn test_python_absolute_flat_layout() {
        let mut known = HashSet::new();
        known.insert("chitta/config.py".to_string());
        let roots = ["".to_string()];

        let result = resolve_python_import("chitta/main.py", "chitta.config", &known, &roots);
        assert_eq!(result, Some("chitta/config.py".to_string()));
    }

    #[test]
    fn test_python_absolute_src_layout() {
        let mut known = HashSet::new();
        known.insert("src/chitta/config.py".to_string());
        let roots = ["src".to_string()];

        let result = resolve_python_import("src/chitta/main.py", "chitta.config", &known, &roots);
        assert_eq!(result, Some("src/chitta/config.py".to_string()));
    }

    #[test]
    fn test_python_absolute_package_init() {
        let mut known = HashSet::new();
        known.insert("chitta/config/__init__.py".to_string());
        let roots = ["".to_string()];

        let result = resolve_python_import("chitta/main.py", "chitta.config", &known, &roots);
        assert_eq!(result, Some("chitta/config/__init__.py".to_string()));
    }

    #[test]
    fn test_python_absolute_stdlib_unresolved() {
        // Standard-library and third-party modules live outside the indexed
        // tree, so absolute resolution returns None instead of a phantom file.
        let mut known = HashSet::new();
        known.insert("chitta/config.py".to_string());
        let roots = ["".to_string()];

        assert_eq!(
            resolve_python_import("chitta/main.py", "os", &known, &roots),
            None
        );
        assert_eq!(
            resolve_python_import("chitta/main.py", "requests.adapters", &known, &roots),
            None
        );
    }

    #[test]
    fn test_discover_python_import_roots_flat_and_src() {
        let mut flat = HashSet::new();
        flat.insert("chitta/__init__.py".to_string());
        flat.insert("chitta/config.py".to_string());
        assert_eq!(discover_python_import_roots(&flat), vec!["".to_string()]);

        let mut src = HashSet::new();
        src.insert("src/chitta/__init__.py".to_string());
        src.insert("src/chitta/sub/__init__.py".to_string());
        assert_eq!(discover_python_import_roots(&src), vec!["src".to_string()]);
    }

    #[test]
    fn test_discover_python_import_roots_monorepo_sorted() {
        let mut known = HashSet::new();
        known.insert("services/a/a/__init__.py".to_string());
        known.insert("libs/b/__init__.py".to_string());
        assert_eq!(
            discover_python_import_roots(&known),
            vec!["libs".to_string(), "services/a".to_string()]
        );
    }

    // --- Go resolver ---

    #[test]
    fn test_go_internal_import() {
        let mut known = HashSet::new();
        known.insert("internal/utils/helpers.go".to_string());
        known.insert("internal/utils/math.go".to_string());

        let mut result = resolve_go_import(
            "github.com/user/project/internal/utils",
            &known,
            Some("github.com/user/project"),
        );
        result.sort();
        assert_eq!(
            result,
            vec![
                "internal/utils/helpers.go".to_string(),
                "internal/utils/math.go".to_string(),
            ]
        );
    }

    #[test]
    fn test_go_external_import() {
        let known = HashSet::new();
        let result = resolve_go_import("fmt", &known, Some("github.com/user/project"));
        assert!(result.is_empty());
    }

    #[test]
    fn test_go_no_module() {
        let known = HashSet::new();
        let result = resolve_go_import("pkg/utils", &known, None);
        assert!(result.is_empty());
    }

    // --- Dart resolver ---

    #[test]
    fn test_parse_pubspec_name_simple() {
        let yaml = "name: arrow_core\nversion: 0.1.0\n";
        assert_eq!(parse_pubspec_name(yaml), Some("arrow_core".to_string()));
    }

    #[test]
    fn test_parse_pubspec_name_quoted() {
        assert_eq!(
            parse_pubspec_name("name: 'arrow_core'\n"),
            Some("arrow_core".to_string())
        );
        assert_eq!(
            parse_pubspec_name("name: \"arrow_core\"\n"),
            Some("arrow_core".to_string())
        );
    }

    #[test]
    fn test_parse_pubspec_name_ignores_indented() {
        let yaml = "dev_dependencies:\n  pkg:\n    name: nested\n";
        assert_eq!(parse_pubspec_name(yaml), None);
    }

    #[test]
    fn test_parse_pubspec_name_ignores_comments() {
        let yaml = "# name: wrong\nname: right\n";
        assert_eq!(parse_pubspec_name(yaml), Some("right".to_string()));
    }

    #[test]
    fn test_read_dart_packages_monorepo() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("options")).unwrap();
        fs::create_dir_all(root.join("core")).unwrap();
        fs::write(root.join("options/pubspec.yaml"), "name: arrow_options\n").unwrap();
        fs::write(root.join("core/pubspec.yaml"), "name: arrow_core\n").unwrap();

        let packages = read_dart_packages(root);
        assert_eq!(packages.get("arrow_options"), Some(&"options".to_string()));
        assert_eq!(packages.get("arrow_core"), Some(&"core".to_string()));
    }

    #[test]
    fn test_read_dart_packages_root_pubspec() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("pubspec.yaml"), "name: arrow\n").unwrap();

        let packages = read_dart_packages(root);
        assert_eq!(packages.get("arrow"), Some(&"".to_string()));
    }

    #[test]
    fn test_resolve_dart_import_package() {
        let mut pkgs = HashMap::new();
        pkgs.insert("arrow_options".to_string(), "options".to_string());
        let mut known = HashSet::new();
        known.insert("options/lib/src/body.dart".to_string());
        known.insert("options/lib/arrow_options.dart".to_string());

        let result = resolve_dart_import(
            "core/lib/src/chart.dart",
            "package:arrow_options/src/body.dart",
            Path::new("/"),
            &known,
            Some(&pkgs),
        );
        assert_eq!(result, vec!["options/lib/src/body.dart".to_string()]);

        let result = resolve_dart_import(
            "core/lib/src/chart.dart",
            "package:arrow_options/arrow_options.dart",
            Path::new("/"),
            &known,
            Some(&pkgs),
        );
        assert_eq!(result, vec!["options/lib/arrow_options.dart".to_string()]);
    }

    #[test]
    fn test_resolve_dart_import_package_at_root() {
        let mut pkgs = HashMap::new();
        pkgs.insert("arrow".to_string(), "".to_string());
        let mut known = HashSet::new();
        known.insert("lib/src/chart.dart".to_string());

        let result = resolve_dart_import(
            "lib/main.dart",
            "package:arrow/src/chart.dart",
            Path::new("/"),
            &known,
            Some(&pkgs),
        );
        assert_eq!(result, vec!["lib/src/chart.dart".to_string()]);
    }

    #[test]
    fn test_resolve_dart_import_sdk_is_empty() {
        let pkgs = HashMap::new();
        let known = HashSet::new();
        assert!(
            resolve_dart_import(
                "lib/main.dart",
                "dart:async",
                Path::new("/"),
                &known,
                Some(&pkgs)
            )
            .is_empty()
        );
    }

    #[test]
    fn test_resolve_dart_import_unknown_package_is_empty() {
        let pkgs = HashMap::new();
        let known = HashSet::new();
        assert!(
            resolve_dart_import(
                "lib/main.dart",
                "package:flutter/material.dart",
                Path::new("/"),
                &known,
                Some(&pkgs)
            )
            .is_empty()
        );
    }

    #[test]
    fn test_resolve_dart_import_relative() {
        let pkgs = HashMap::new();
        let mut known = HashSet::new();
        known.insert("lib/src/helper.dart".to_string());

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let result = resolve_dart_import(
            "lib/src/main.dart",
            "./helper.dart",
            root,
            &known,
            Some(&pkgs),
        );
        assert_eq!(result, vec!["lib/src/helper.dart".to_string()]);
    }

    #[test]
    fn test_full_index_resolves_dart_package_imports() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("options/lib/src")).unwrap();
        fs::create_dir_all(root.join("core/lib/src")).unwrap();
        fs::write(root.join("options/pubspec.yaml"), "name: arrow_options\n").unwrap();
        fs::write(root.join("core/pubspec.yaml"), "name: arrow_core\n").unwrap();
        fs::write(
            root.join("options/lib/src/body.dart"),
            "enum Body { sun, moon }\n",
        )
        .unwrap();
        fs::write(
            root.join("core/lib/src/chart.dart"),
            "import 'package:arrow_options/src/body.dart';\n\
             class Chart { Body? sun; }\n",
        )
        .unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, true).unwrap();

        let chart_id = read::get_file_by_path(&conn, "core/lib/src/chart.dart")
            .unwrap()
            .unwrap()
            .id;
        let body_id = read::get_file_by_path(&conn, "options/lib/src/body.dart")
            .unwrap()
            .unwrap()
            .id;

        let edges = read::get_all_edges(&conn).unwrap();
        let has_edge = edges.iter().any(|e| e.0 == chart_id && e.1 == body_id);
        assert!(
            has_edge,
            "expected chart.dart → body.dart import edge, got edges {edges:?}"
        );
    }

    // --- Integration tests ---

    #[test]
    fn test_multi_root_no_cross_root_edge_on_prefix_collision() {
        // A sibling root's prefix ("shared") equals a subdirectory name inside
        // another root ("app/shared"). The resolver yields the current-root
        // relative key "shared/deployment.yaml", which must resolve to
        // app/shared/deployment.yaml - NOT to the sibling root's
        // shared/deployment.yaml. Guards against a bare-key lookup binding an
        // edge to the wrong root.
        let tmp = TempDir::new().unwrap();
        let root_shared = tmp.path().join("shared");
        let root_app = tmp.path().join("app");
        fs::create_dir_all(&root_shared).unwrap();
        fs::create_dir_all(root_app.join("shared")).unwrap();

        // Sibling root "shared" with a top-level deployment.yaml → DB path
        // "shared/deployment.yaml".
        fs::write(root_shared.join("deployment.yaml"), "kind: Deployment\n").unwrap();

        // Root "app" with a subdirectory literally named "shared".
        fs::write(
            root_app.join("shared/kustomization.yaml"),
            "kind: Kustomization\nresources:\n  - deployment.yaml\n",
        )
        .unwrap();
        fs::write(
            root_app.join("shared/deployment.yaml"),
            "kind: Deployment\n",
        )
        .unwrap();

        let conn = storage::open_in_memory().unwrap();
        let roots = vec![root_shared.clone(), root_app.clone()];
        let aliases = HashMap::new();
        full_index_multi(&conn, &roots, &aliases, true).unwrap();

        let id = |p: &str| read::get_file_by_path(&conn, p).unwrap().unwrap().id;
        let edges = read::get_all_edges(&conn).unwrap();
        let linked = |from: &str, to: &str| {
            let (f, t) = (id(from), id(to));
            edges.iter().any(|e| e.0 == f && e.1 == t)
        };

        assert!(
            linked(
                "app/shared/kustomization.yaml",
                "app/shared/deployment.yaml"
            ),
            "must link to the SAME-root deployment; edges {edges:?}"
        );
        assert!(
            !linked("app/shared/kustomization.yaml", "shared/deployment.yaml"),
            "must NOT link across roots to the sibling 'shared' root's file"
        );
    }

    #[test]
    fn test_multi_root_incremental_reresolves_infra_edges() {
        // After the initial multi-root full index, a live edit to a manifest
        // goes through incremental_index. That path must re-resolve the edited
        // file's edges under the root prefix - otherwise a save in a multi-root
        // (submodule) workspace silently drops the file's edges until a full
        // reindex.
        let tmp = TempDir::new().unwrap();
        let root_a = tmp.path().join("submod-a");
        let root_b = tmp.path().join("submod-b");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();

        fs::write(
            root_a.join("kustomization.yaml"),
            "kind: Kustomization\nresources:\n  - deployment.yaml\n",
        )
        .unwrap();
        fs::write(root_a.join("deployment.yaml"), "kind: Deployment\n").unwrap();
        fs::write(root_a.join("service.yaml"), "kind: Service\n").unwrap();
        fs::write(
            root_b.join("kustomization.yaml"),
            "kind: Kustomization\nresources:\n  - cfg.yaml\n",
        )
        .unwrap();
        fs::write(root_b.join("cfg.yaml"), "kind: ConfigMap\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        let roots = vec![root_a.clone(), root_b.clone()];
        let aliases = HashMap::new();
        full_index_multi(&conn, &roots, &aliases, true).unwrap();

        // Live edit: kustomization now also pulls in service.yaml.
        fs::write(
            root_a.join("kustomization.yaml"),
            "kind: Kustomization\nresources:\n  - deployment.yaml\n  - service.yaml\n",
        )
        .unwrap();
        incremental_index_with_prefix(
            &conn,
            &root_a,
            "submod-a",
            &[root_a.join("kustomization.yaml")],
            &[],
        )
        .unwrap();

        let id = |p: &str| read::get_file_by_path(&conn, p).unwrap().unwrap().id;
        let edges = read::get_all_edges(&conn).unwrap();
        let linked = |from: &str, to: &str| {
            let (f, t) = (id(from), id(to));
            edges.iter().any(|e| e.0 == f && e.1 == t)
        };
        assert!(
            linked("submod-a/kustomization.yaml", "submod-a/deployment.yaml"),
            "existing edge must survive incremental re-index; edges {edges:?}"
        );
        assert!(
            linked("submod-a/kustomization.yaml", "submod-a/service.yaml"),
            "newly added edge must be resolved by incremental re-index in multi-root mode"
        );
    }

    #[test]
    fn test_multi_root_infra_edges_resolve_within_each_root() {
        // Two roots (as `qartez_workspace add` / submodules produce), each with
        // an internal Kustomize edge. In multi-root mode DB paths are prefixed
        // with the root dir name; the resolver works on unprefixed paths, so the
        // edge write must reconcile the two. Regression for multi-root/submodule
        // resolution returning zero edges.
        let tmp = TempDir::new().unwrap();
        let root_a = tmp.path().join("submod-a");
        let root_b = tmp.path().join("submod-b");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();

        fs::write(
            root_a.join("kustomization.yaml"),
            "kind: Kustomization\nresources:\n  - deployment.yaml\n",
        )
        .unwrap();
        fs::write(
            root_a.join("deployment.yaml"),
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: a\n",
        )
        .unwrap();
        fs::write(
            root_b.join("kustomization.yaml"),
            "kind: Kustomization\nresources:\n  - service.yaml\n",
        )
        .unwrap();
        fs::write(
            root_b.join("service.yaml"),
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: b\n",
        )
        .unwrap();

        let conn = storage::open_in_memory().unwrap();
        let roots = vec![root_a.clone(), root_b.clone()];
        let aliases = HashMap::new();
        full_index_multi(&conn, &roots, &aliases, true).unwrap();

        let id = |p: &str| read::get_file_by_path(&conn, p).unwrap().unwrap().id;
        let edges = read::get_all_edges(&conn).unwrap();
        let linked = |from: &str, to: &str| {
            let (f, t) = (id(from), id(to));
            edges.iter().any(|e| e.0 == f && e.1 == t)
        };

        assert!(
            linked("submod-a/kustomization.yaml", "submod-a/deployment.yaml"),
            "root A internal edge missing in multi-root mode; edges {edges:?}"
        );
        assert!(
            linked("submod-b/kustomization.yaml", "submod-b/service.yaml"),
            "root B internal edge missing in multi-root mode"
        );
    }

    #[test]
    fn test_full_index_infra_dependency_edges() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Kustomize base.
        fs::create_dir_all(root.join("k8s/base")).unwrap();
        fs::write(
            root.join("k8s/base/kustomization.yaml"),
            "kind: Kustomization\nresources:\n  - deployment.yaml\n",
        )
        .unwrap();
        fs::write(
            root.join("k8s/base/deployment.yaml"),
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: app\n",
        )
        .unwrap();

        // Kustomize prod overlay pointing back at the base.
        fs::create_dir_all(root.join("k8s/overlays/prod")).unwrap();
        fs::write(
            root.join("k8s/overlays/prod/kustomization.yaml"),
            "kind: Kustomization\nresources:\n  - ../../base\npatchesStrategicMerge:\n  - replicas.yaml\n",
        )
        .unwrap();
        fs::write(
            root.join("k8s/overlays/prod/replicas.yaml"),
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: app\nspec:\n  replicas: 3\n",
        )
        .unwrap();

        // ArgoCD Application pointing at the overlay (repo-root relative path).
        fs::create_dir_all(root.join("argocd")).unwrap();
        fs::write(
            root.join("argocd/app.yaml"),
            "apiVersion: argoproj.io/v1alpha1\nkind: Application\nmetadata:\n  name: app\nspec:\n  source:\n    repoURL: https://example.com/repo.git\n    path: k8s/overlays/prod\n",
        )
        .unwrap();

        // Terraform root module referencing a local child module.
        fs::create_dir_all(root.join("tf/modules/vpc")).unwrap();
        fs::write(
            root.join("tf/main.tf"),
            "module \"vpc\" {\n  source = \"./modules/vpc\"\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("tf/modules/vpc/main.tf"),
            "resource \"x\" \"y\" {}\n",
        )
        .unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, true).unwrap();

        let id = |p: &str| read::get_file_by_path(&conn, p).unwrap().unwrap().id;
        let edges = read::get_all_edges(&conn).unwrap();
        let linked = |from: &str, to: &str| {
            let (f, t) = (id(from), id(to));
            edges.iter().any(|e| e.0 == f && e.1 == t)
        };

        assert!(
            linked(
                "k8s/overlays/prod/kustomization.yaml",
                "k8s/base/kustomization.yaml"
            ),
            "overlay → base kustomize edge missing; edges {edges:?}"
        );
        assert!(
            linked(
                "k8s/overlays/prod/kustomization.yaml",
                "k8s/overlays/prod/replicas.yaml"
            ),
            "overlay → patch edge missing"
        );
        assert!(
            linked("k8s/base/kustomization.yaml", "k8s/base/deployment.yaml"),
            "base → resource edge missing"
        );
        assert!(
            linked("argocd/app.yaml", "k8s/overlays/prod/kustomization.yaml"),
            "argocd → overlay edge missing"
        );
        assert!(
            linked("tf/main.tf", "tf/modules/vpc/main.tf"),
            "terraform module edge missing"
        );
    }

    #[test]
    fn test_full_index_with_temp_dir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let src_dir = root.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        fs::write(
            src_dir.join("utils.ts"),
            "export function add(a: number, b: number): number { return a + b; }\n\
             export const PI = 3.14;\n",
        )
        .unwrap();

        fs::write(
            src_dir.join("app.ts"),
            "import { add, PI } from './utils';\n\
             \n\
             export class App {\n\
                 run() { console.log(add(1, 2)); }\n\
             }\n",
        )
        .unwrap();

        fs::write(src_dir.join("index.ts"), "export { App } from './app';\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let file_count = read::get_file_count(&conn).unwrap();
        assert_eq!(file_count, 3);

        let sym_count = read::get_symbol_count(&conn).unwrap();
        assert!(
            sym_count >= 4,
            "expected at least 4 symbols, got {sym_count}"
        );

        let edges = read::get_all_edges(&conn).unwrap();
        assert!(
            edges.len() >= 2,
            "expected at least 2 import edges, got {}",
            edges.len()
        );

        let last_index = read::get_meta(&conn, "last_index").unwrap();
        assert!(last_index.is_some());
    }

    #[test]
    fn test_full_index_esm_js_imports() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let src_dir = root.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        fs::write(
            src_dir.join("utils.ts"),
            "export function add(a: number, b: number) { return a + b; }\n",
        )
        .unwrap();

        fs::write(
            src_dir.join("app.ts"),
            "import { add } from './utils.js';\nconsole.log(add(1, 2));\n",
        )
        .unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let edges = read::get_all_edges(&conn).unwrap();
        assert_eq!(edges.len(), 1, "ESM .js import should create an edge");
    }

    #[test]
    fn test_full_index_rust_crate_imports() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let src_dir = root.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        fs::write(src_dir.join("lib.rs"), "pub mod error;\npub mod config;\n").unwrap();

        fs::write(
            src_dir.join("error.rs"),
            "pub enum AppError {\n    NotFound,\n    Internal,\n}\n\
             pub type Result<T> = std::result::Result<T, AppError>;\n",
        )
        .unwrap();

        fs::write(
            src_dir.join("config.rs"),
            "use crate::error::Result;\n\n\
             pub struct Config {\n    pub name: String,\n}\n\n\
             pub fn load() -> Result<Config> {\n    Ok(Config { name: \"test\".into() })\n}\n",
        )
        .unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let file_count = read::get_file_count(&conn).unwrap();
        assert_eq!(file_count, 3);

        let edges = read::get_all_edges(&conn).unwrap();
        assert!(
            !edges.is_empty(),
            "Rust crate:: import should create edges, got 0"
        );
    }

    #[test]
    fn test_full_index_rust_super_imports() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let src_dir = root.join("src");
        let models_dir = src_dir.join("models");
        fs::create_dir_all(&models_dir).unwrap();

        fs::write(src_dir.join("lib.rs"), "pub mod models;\n").unwrap();

        fs::write(
            models_dir.join("mod.rs"),
            "pub mod user;\npub struct Config;\n",
        )
        .unwrap();

        fs::write(
            models_dir.join("user.rs"),
            "use super::Config;\n\npub struct User {\n    pub name: String,\n}\n",
        )
        .unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let edges = read::get_all_edges(&conn).unwrap();
        assert!(
            !edges.is_empty(),
            "Rust super:: import should create edges, got 0"
        );
    }

    #[test]
    fn test_full_index_skips_unchanged() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join("main.ts"), "export function main() {}\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let count1 = read::get_file_count(&conn).unwrap();
        assert_eq!(count1, 1);

        full_index(&conn, root, false).unwrap();

        let count2 = read::get_file_count(&conn).unwrap();
        assert_eq!(count2, 1);
    }

    #[test]
    fn test_full_index_force_reindex() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join("lib.ts"), "export const VERSION = '1.0';\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();
        full_index(&conn, root, true).unwrap();

        let count = read::get_file_count(&conn).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_incremental_deletes_removed_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join("a.ts"), "export const A = 1;\n").unwrap();
        fs::write(root.join("b.ts"), "export const B = 2;\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();
        assert_eq!(read::get_file_count(&conn).unwrap(), 2);

        fs::remove_file(root.join("b.ts")).unwrap();
        full_index(&conn, root, false).unwrap();

        assert_eq!(read::get_file_count(&conn).unwrap(), 1);
        assert!(read::get_file_by_path(&conn, "b.ts").unwrap().is_none());
        assert!(read::get_file_by_path(&conn, "a.ts").unwrap().is_some());
    }

    #[test]
    fn test_incremental_reindexes_modified_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join("mod.ts"), "export function old() {}\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let syms_before = read::get_symbol_count(&conn).unwrap();

        // Sleep briefly so mtime changes
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            root.join("mod.ts"),
            "export function newA() {}\nexport function newB() {}\n",
        )
        .unwrap();

        full_index(&conn, root, false).unwrap();

        let syms_after = read::get_symbol_count(&conn).unwrap();
        assert!(
            syms_after >= 2,
            "expected at least 2 symbols after modification, got {syms_after}"
        );
        assert!(
            syms_after > syms_before,
            "symbols should increase after adding functions ({syms_before} -> {syms_after})"
        );
    }

    #[test]
    fn test_incremental_adds_new_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join("first.ts"), "export const X = 1;\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();
        assert_eq!(read::get_file_count(&conn).unwrap(), 1);

        fs::write(root.join("second.ts"), "export const Y = 2;\n").unwrap();
        full_index(&conn, root, false).unwrap();
        assert_eq!(read::get_file_count(&conn).unwrap(), 2);
    }

    /// The MCP-server startup path always runs the reconciliation walk and
    /// uses [`IndexOutcome::changed`] to decide whether to recompute the heavy
    /// global derived tables. This pins that signal: a fresh index reports
    /// changes, a no-op re-index reports none, and an added/deleted file flips
    /// it back on - so files appearing while the server was down (a matching
    /// fingerprint but a changed tree) still trigger the recompute.
    #[test]
    fn full_index_outcome_reports_changes() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("first.ts"), "export const X = 1;\n").unwrap();

        let conn = storage::open_in_memory().unwrap();

        // First index: the new file counts as updated, so changed() is true.
        let fresh = full_index(&conn, root, false).unwrap();
        assert!(fresh.changed(), "first index of a new file should change");
        assert_eq!(fresh.updated, 1);
        assert_eq!(fresh.deleted, 0);

        // Re-index with nothing touched: no updates, no deletes, no recompute.
        let noop = full_index(&conn, root, false).unwrap();
        assert!(
            !noop.changed(),
            "re-indexing an unchanged tree must report no changes ({noop:?})"
        );
        assert_eq!(noop, IndexOutcome::default());

        // A file added while "down" is the downtime case: a plain re-index
        // (no force) must discover it and report a change.
        fs::write(root.join("second.ts"), "export const Y = 2;\n").unwrap();
        let added = full_index(&conn, root, false).unwrap();
        assert!(added.changed(), "an added file should change");
        assert_eq!(added.updated, 1);

        // Deleting a file is also a change, via the stale-removal path.
        fs::remove_file(root.join("second.ts")).unwrap();
        let removed = full_index(&conn, root, false).unwrap();
        assert!(removed.changed(), "a deleted file should change");
        assert_eq!(removed.deleted, 1);
    }

    /// A no-op re-index (force=false, nothing changed on disk) must leave the
    /// derived tables - symbols FTS, import edges, and unused-exports - fully
    /// intact. `full_index_root` skips the whole-table rebuilds in that case
    /// (the startup path runs this walk on every start), so this pins that the
    /// skip preserves, rather than wipes, the existing derived state.
    #[test]
    fn noop_reindex_preserves_derived_tables() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // lib.ts is imported by app.ts -> yields an import edge. orphan.ts is
        // imported by nobody and its export is never referenced -> an unused
        // export (the heuristic only flags exports of files with no incoming
        // edge, so the unused case needs a separate orphan file).
        fs::write(root.join("lib.ts"), "export function used() {}\n").unwrap();
        fs::write(
            root.join("app.ts"),
            "import { used } from \"./lib\";\nused();\n",
        )
        .unwrap();
        fs::write(root.join("orphan.ts"), "export function lonely() {}\n").unwrap();

        let conn = storage::open_in_memory().unwrap();

        let fresh = full_index(&conn, root, true).unwrap();
        assert!(fresh.changed(), "first full index should change");

        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        let fts_before = count("SELECT COUNT(*) FROM symbols_fts");
        let edges_before = count("SELECT COUNT(*) FROM edges");
        let unused_before = count("SELECT COUNT(*) FROM unused_exports");
        assert!(fts_before > 0, "FTS should be populated after first index");
        assert!(edges_before > 0, "the import should produce an edge");
        assert!(unused_before > 0, "`lonely` should be an unused export");

        // True no-op: same files, same mtimes, force=false.
        let noop = full_index(&conn, root, false).unwrap();
        assert!(!noop.changed(), "an unchanged tree must report no change");

        // The skip must preserve every derived table exactly.
        assert_eq!(
            count("SELECT COUNT(*) FROM symbols_fts"),
            fts_before,
            "no-op reindex must not wipe symbols_fts"
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM edges"),
            edges_before,
            "no-op reindex must not wipe edges"
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM unused_exports"),
            unused_before,
            "no-op reindex must not wipe unused_exports"
        );
    }

    // -- Symbol reference resolution --

    fn count_symbol_refs(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM symbol_refs", [], |r| r.get(0))
            .unwrap()
    }

    fn symbol_ref_names(conn: &Connection) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT sf.name, st.name
                 FROM symbol_refs r
                 JOIN symbols sf ON sf.id = r.from_symbol_id
                 JOIN symbols st ON st.id = r.to_symbol_id
                 ORDER BY sf.name, st.name",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap();
        rows.filter_map(|r| r.ok()).collect()
    }

    #[test]
    fn test_full_index_resolves_same_file_rust_refs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            "pub fn helper() -> i32 { 42 }\n\
             pub fn caller() -> i32 { helper() + 1 }\n",
        )
        .unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let refs = symbol_ref_names(&conn);
        assert!(
            refs.iter().any(|(f, t)| f == "caller" && t == "helper"),
            "expected (caller -> helper) edge, got {refs:?}"
        );
    }

    #[test]
    fn test_full_index_resolves_cross_file_rust_refs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();

        // `lib.rs` is the crate root referencing the helper module.
        fs::write(
            src.join("lib.rs"),
            "pub mod helper;\n\
             use crate::helper::do_work;\n\
             pub fn run() { do_work(); }\n",
        )
        .unwrap();
        fs::write(src.join("helper.rs"), "pub fn do_work() {}\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let refs = symbol_ref_names(&conn);
        assert!(
            refs.iter().any(|(f, t)| f == "run" && t == "do_work"),
            "expected (run -> do_work) edge across files, got {refs:?}"
        );
    }

    #[test]
    fn test_full_index_cascades_symbol_refs_on_delete() {
        // When a file is removed from disk and reindexed, its symbol_refs
        // rows must be cleaned up via the ON DELETE CASCADE foreign key.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            "pub fn a() { b(); }\n\
             pub fn b() {}\n",
        )
        .unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();
        assert!(count_symbol_refs(&conn) >= 1);

        // Remove the file and force a reindex; symbol_refs should go to 0.
        fs::remove_file(src.join("lib.rs")).unwrap();
        full_index(&conn, root, true).unwrap();
        assert_eq!(count_symbol_refs(&conn), 0);
    }

    #[test]
    fn test_full_index_symbol_refs_python() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(
            root.join("a.py"),
            "def helper():\n    return 1\n\n\
             def caller():\n    return helper()\n",
        )
        .unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let refs = symbol_ref_names(&conn);
        assert!(
            refs.iter().any(|(f, t)| f == "caller" && t == "helper"),
            "expected (caller -> helper) edge for Python, got {refs:?}"
        );
    }

    #[test]
    fn test_full_index_drops_ambiguous_global() {
        // Two unrelated files each define a function called `common`, and
        // a third file calls `common()` without importing either. The
        // resolver should drop the reference because the global name is
        // ambiguous and there is no import evidence.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.rs"), "pub fn common() {}\n").unwrap();
        fs::write(src.join("b.rs"), "pub fn common() {}\n").unwrap();
        // `c.rs` calls common but does not `use` either module, so neither
        // definition is in the imports-by-file set.
        fs::write(src.join("c.rs"), "pub fn caller() { common(); }\n").unwrap();
        // Crate root binding modules so they get indexed (not strictly
        // required but avoids the "unreachable file" warning noise).
        fs::write(src.join("lib.rs"), "pub mod a;\npub mod b;\npub mod c;\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let refs = symbol_ref_names(&conn);
        let caller_to_common: Vec<&(String, String)> = refs
            .iter()
            .filter(|(f, t)| f == "caller" && t == "common")
            .collect();
        assert!(
            caller_to_common.is_empty(),
            "ambiguous global `common` should not resolve, got {caller_to_common:?}"
        );
    }

    // --- incremental_index ---

    #[test]
    fn test_incremental_index_adds_new_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn hello() {}\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        // Add a new file and run incremental index.
        fs::write(src.join("new.rs"), "pub fn world() {}\n").unwrap();
        incremental_index(&conn, root, &[src.join("new.rs")], &[]).unwrap();

        let file = read::get_file_by_path(&conn, "src/new.rs").unwrap();
        assert!(file.is_some(), "new file must appear in the index");
        let syms = read::get_symbols_for_file(&conn, file.unwrap().id).unwrap();
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "world");
    }

    #[test]
    fn test_incremental_index_updates_modified_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn hello() {}\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let old_file = read::get_file_by_path(&conn, "src/lib.rs")
            .unwrap()
            .unwrap();
        let old_id = old_file.id;

        // Modify the file.
        fs::write(
            src.join("lib.rs"),
            "pub fn hello() {}\npub fn goodbye() {}\n",
        )
        .unwrap();
        incremental_index(&conn, root, &[src.join("lib.rs")], &[]).unwrap();

        let new_file = read::get_file_by_path(&conn, "src/lib.rs")
            .unwrap()
            .unwrap();
        // File id must be preserved (clear_file_content + upsert, not delete+insert).
        assert_eq!(
            new_file.id, old_id,
            "file_id must be stable across incremental updates"
        );
        let syms = read::get_symbols_for_file(&conn, new_file.id).unwrap();
        assert_eq!(syms.len(), 2);
    }

    #[test]
    fn test_incremental_index_removes_deleted_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn hello() {}\n").unwrap();
        fs::write(src.join("old.rs"), "pub fn gone() {}\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        assert!(
            read::get_file_by_path(&conn, "src/old.rs")
                .unwrap()
                .is_some()
        );

        // Delete the file on disk, then tell incremental it was deleted.
        fs::remove_file(src.join("old.rs")).unwrap();
        incremental_index(&conn, root, &[], &[src.join("old.rs")]).unwrap();

        assert!(
            read::get_file_by_path(&conn, "src/old.rs")
                .unwrap()
                .is_none(),
            "deleted file must be removed from the index"
        );
    }

    #[test]
    fn test_incremental_preserves_incoming_edges() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        // a.rs imports b via `use crate::b;`
        fs::write(src.join("lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
        fs::write(
            src.join("a.rs"),
            "use crate::b;\npub fn caller() { b::helper(); }\n",
        )
        .unwrap();
        fs::write(src.join("b.rs"), "pub fn helper() {}\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let b_file = read::get_file_by_path(&conn, "src/b.rs").unwrap().unwrap();
        let incoming_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE to_file = ?1",
                [b_file.id],
                |r| r.get(0),
            )
            .unwrap();

        // Modify b.rs and run incremental.
        fs::write(
            src.join("b.rs"),
            "pub fn helper() {}\npub fn helper2() {}\n",
        )
        .unwrap();
        incremental_index(&conn, root, &[src.join("b.rs")], &[]).unwrap();

        let incoming_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE to_file = ?1",
                [b_file.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            incoming_before, incoming_after,
            "incoming edges to b.rs must be preserved after incremental re-index"
        );
    }

    #[test]
    fn test_incremental_empty_is_noop() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn hello() {}\n").unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let count_before = read::get_file_count(&conn).unwrap();
        incremental_index(&conn, root, &[], &[]).unwrap();
        let count_after = read::get_file_count(&conn).unwrap();
        assert_eq!(count_before, count_after);
    }

    #[test]
    fn test_qualifier_resolves_correct_impl() {
        // Two types both define `new()`. A caller uses `Foo::new()`. The
        // resolver should pick Foo's new, not Bar's, thanks to qualifier matching.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            "pub struct Foo;\n\
             pub struct Bar;\n\
             impl Foo { pub fn new() -> Self { Foo } }\n\
             impl Bar { pub fn new() -> Self { Bar } }\n\
             pub fn caller() { let _x = Foo::new(); }\n",
        )
        .unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let refs = symbol_ref_names(&conn);
        let caller_new: Vec<&(String, String)> = refs
            .iter()
            .filter(|(f, t)| f == "caller" && t == "new")
            .collect();
        assert_eq!(
            caller_new.len(),
            1,
            "Foo::new() should resolve to exactly one target, got {caller_new:?}"
        );
    }

    #[test]
    fn test_impl_block_self_reference() {
        // A method inside `impl Foo` calls another method on the same type.
        // The same-impl-block heuristic should resolve it correctly.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            "pub struct Foo;\n\
             impl Foo {\n\
                 pub fn helper(&self) {}\n\
                 pub fn run(&self) { self.helper(); }\n\
             }\n",
        )
        .unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let refs = symbol_ref_names(&conn);
        assert!(
            refs.iter().any(|(f, t)| f == "run" && t == "helper"),
            "self.helper() inside impl Foo should resolve run -> helper, got {refs:?}"
        );
    }

    #[test]
    fn test_owner_type_stored_in_db() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            "pub struct Widget;\n\
             impl Widget { pub fn render(&self) {} }\n",
        )
        .unwrap();

        let conn = storage::open_in_memory().unwrap();
        full_index(&conn, root, false).unwrap();

        let all = read::get_all_symbols_with_path(&conn).unwrap();
        let render = all
            .iter()
            .find(|(s, _)| s.name == "render")
            .expect("render method should exist");
        assert_eq!(
            render.0.owner_type.as_deref(),
            Some("Widget"),
            "owner_type should be persisted to DB"
        );
    }

    // --- Helper-level tests for the refactor ---
    //
    // The end-to-end tests above transitively exercise try_ingest_file,
    // remove_stale_files, try_reingest_changed_file, and delete_single_file.
    // These tests pin down their direct contracts so accidental changes to
    // outcome semantics get caught at the unit level.

    fn open_test_conn() -> rusqlite::Connection {
        storage::open_in_memory().unwrap()
    }

    #[test]
    fn try_ingest_file_returns_ingested_for_new_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file_path = root.join("a.ts");
        fs::write(&file_path, "export const x = 1;\n").unwrap();

        let conn = open_test_conn();
        let tx = conn.unchecked_transaction().unwrap();
        let pool = ParserPool::new();
        let mut indexed: Vec<IndexedFile> = Vec::new();
        let mut known: HashSet<String> = HashSet::new();

        let outcome = try_ingest_file(
            &tx,
            &file_path,
            root,
            "",
            false,
            max_file_bytes(),
            &pool,
            &mut indexed,
            &mut known,
        )
        .unwrap();
        tx.commit().unwrap();

        assert!(matches!(outcome, FileIngestOutcome::Ingested));
        assert_eq!(indexed.len(), 1);
        assert!(known.contains("a.ts"));
    }

    #[test]
    fn try_ingest_file_returns_unchanged_when_mtime_matches() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file_path = root.join("a.ts");
        fs::write(&file_path, "export const x = 1;\n").unwrap();

        let conn = open_test_conn();
        // First ingestion populates the file row in the DB.
        full_index(&conn, root, true).unwrap();

        let tx = conn.unchecked_transaction().unwrap();
        let pool = ParserPool::new();
        let mut indexed: Vec<IndexedFile> = Vec::new();
        let mut known: HashSet<String> = HashSet::new();

        let outcome = try_ingest_file(
            &tx,
            &file_path,
            root,
            "",
            false,
            max_file_bytes(),
            &pool,
            &mut indexed,
            &mut known,
        )
        .unwrap();
        tx.commit().unwrap();

        assert!(matches!(outcome, FileIngestOutcome::Unchanged));
        assert!(indexed.is_empty(), "Unchanged must not append to indexed");
        assert!(
            known.contains("a.ts"),
            "Unchanged path must still be recorded so stale-cleanup leaves it alone"
        );
    }

    #[test]
    fn try_ingest_file_force_reingests_unchanged_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file_path = root.join("a.ts");
        fs::write(&file_path, "export const x = 1;\n").unwrap();

        let conn = open_test_conn();
        full_index(&conn, root, true).unwrap();

        let tx = conn.unchecked_transaction().unwrap();
        let pool = ParserPool::new();
        let mut indexed: Vec<IndexedFile> = Vec::new();
        let mut known: HashSet<String> = HashSet::new();

        let outcome = try_ingest_file(
            &tx,
            &file_path,
            root,
            "",
            true, // force
            max_file_bytes(),
            &pool,
            &mut indexed,
            &mut known,
        )
        .unwrap();
        tx.commit().unwrap();

        assert!(matches!(outcome, FileIngestOutcome::Ingested));
    }

    #[test]
    fn try_ingest_file_skips_stat_failure() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let missing = root.join("does_not_exist.ts");

        let conn = open_test_conn();
        let tx = conn.unchecked_transaction().unwrap();
        let pool = ParserPool::new();
        let mut indexed: Vec<IndexedFile> = Vec::new();
        let mut known: HashSet<String> = HashSet::new();

        let outcome = try_ingest_file(
            &tx,
            &missing,
            root,
            "",
            false,
            max_file_bytes(),
            &pool,
            &mut indexed,
            &mut known,
        )
        .unwrap();

        assert!(matches!(outcome, FileIngestOutcome::Skipped));
        assert!(indexed.is_empty());
        assert!(known.is_empty());
    }

    #[test]
    fn try_ingest_file_skips_oversize() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file_path = root.join("big.ts");
        fs::write(&file_path, b"export const x = 1;\n").unwrap();

        let conn = open_test_conn();
        let tx = conn.unchecked_transaction().unwrap();
        let pool = ParserPool::new();
        let mut indexed: Vec<IndexedFile> = Vec::new();
        let mut known: HashSet<String> = HashSet::new();

        // max_bytes = 5 forces the 20-byte file to be skipped.
        let outcome = try_ingest_file(
            &tx,
            &file_path,
            root,
            "",
            false,
            5,
            &pool,
            &mut indexed,
            &mut known,
        )
        .unwrap();

        assert!(matches!(outcome, FileIngestOutcome::Skipped));
        assert!(indexed.is_empty());
    }

    #[test]
    fn try_ingest_file_records_both_paths_in_multi_root_mode() {
        // In multi-root mode, path_prefix is non-empty. The Unchanged branch
        // moves `raw_rel` into known_paths after inserting `rel_path`, so both
        // the prefixed and unprefixed paths must be present.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file_path = root.join("a.ts");
        fs::write(&file_path, "export const x = 1;\n").unwrap();

        let conn = open_test_conn();
        // Seed the DB with a prefixed path so the Unchanged branch fires.
        full_index_root(&conn, root, true, "alpha", &HashSet::new()).unwrap();

        let tx = conn.unchecked_transaction().unwrap();
        let pool = ParserPool::new();
        let mut indexed: Vec<IndexedFile> = Vec::new();
        let mut known: HashSet<String> = HashSet::new();

        let outcome = try_ingest_file(
            &tx,
            &file_path,
            root,
            "alpha",
            false,
            max_file_bytes(),
            &pool,
            &mut indexed,
            &mut known,
        )
        .unwrap();
        tx.commit().unwrap();

        assert!(matches!(outcome, FileIngestOutcome::Unchanged));
        assert!(
            known.contains("alpha/a.ts"),
            "prefixed path must be recorded"
        );
        assert!(
            known.contains("a.ts"),
            "unprefixed raw_rel must also be recorded"
        );
    }

    #[test]
    fn remove_stale_files_deletes_missing_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("keep.ts"), "export const k = 1;\n").unwrap();
        fs::write(root.join("drop.ts"), "export const d = 1;\n").unwrap();

        let conn = open_test_conn();
        full_index(&conn, root, true).unwrap();
        assert_eq!(read::get_file_count(&conn).unwrap(), 2);

        // Delete one file from disk so it becomes stale relative to the index.
        fs::remove_file(root.join("drop.ts")).unwrap();

        let tx = conn.unchecked_transaction().unwrap();
        let mut known = HashSet::new();
        known.insert("keep.ts".to_string());
        let removed = remove_stale_files(&tx, root, "", &known).unwrap();
        tx.commit().unwrap();

        assert_eq!(removed, 1);
        assert!(read::get_file_by_path(&conn, "drop.ts").unwrap().is_none());
        assert!(read::get_file_by_path(&conn, "keep.ts").unwrap().is_some());
    }

    #[test]
    fn remove_stale_files_skips_files_outside_path_prefix() {
        // Multi-root invariant: removal must only touch files belonging to
        // the current root (matching its prefix), so other roots' files
        // aren't deleted by this root's cleanup pass.
        let tmp = TempDir::new().unwrap();
        let root_a = tmp.path().join("a");
        let root_b = tmp.path().join("b");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();
        fs::write(root_a.join("a.ts"), "export const a = 1;\n").unwrap();
        fs::write(root_b.join("b.ts"), "export const b = 1;\n").unwrap();

        let conn = open_test_conn();
        full_index_root(&conn, &root_a, true, "a", &HashSet::new()).unwrap();
        full_index_root(&conn, &root_b, true, "b", &HashSet::new()).unwrap();
        assert_eq!(read::get_file_count(&conn).unwrap(), 2);

        // Delete b.ts from disk. Cleanup with prefix "a" must NOT remove it.
        fs::remove_file(root_b.join("b.ts")).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        let mut known_a = HashSet::new();
        known_a.insert("a/a.ts".to_string());
        let removed = remove_stale_files(&tx, &root_a, "a", &known_a).unwrap();
        tx.commit().unwrap();

        assert_eq!(
            removed, 0,
            "remove_stale_files must not touch files outside its prefix"
        );
        assert!(
            read::get_file_by_path(&conn, "b/b.ts").unwrap().is_some(),
            "b/b.ts must survive cleanup of root a"
        );
    }

    #[test]
    fn delete_single_file_returns_false_for_unknown_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let conn = open_test_conn();
        let tx = conn.unchecked_transaction().unwrap();

        let result = delete_single_file(&tx, root, "", &root.join("never_indexed.ts")).unwrap();
        assert!(!result);
    }

    #[test]
    fn delete_single_file_removes_indexed_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.ts"), "export const a = 1;\n").unwrap();

        let conn = open_test_conn();
        full_index(&conn, root, true).unwrap();
        assert!(read::get_file_by_path(&conn, "a.ts").unwrap().is_some());

        let tx = conn.unchecked_transaction().unwrap();
        let removed = delete_single_file(&tx, root, "", &root.join("a.ts")).unwrap();
        tx.commit().unwrap();

        assert!(removed);
        assert!(read::get_file_by_path(&conn, "a.ts").unwrap().is_none());
    }

    #[test]
    fn delete_single_file_skips_path_outside_root() {
        // Previously the strip_prefix fallback concatenated the absolute
        // path onto path_prefix and a DB lookup for "workspace1//tmp/foo"
        // silently missed, hiding symlink / mount-point escapes.
        let tmp_root = TempDir::new().unwrap();
        let tmp_other = TempDir::new().unwrap();
        let outside = tmp_other.path().join("outside.ts");
        fs::write(&outside, "export const a = 1;\n").unwrap();

        let conn = open_test_conn();
        let tx = conn.unchecked_transaction().unwrap();

        let result = delete_single_file(&tx, tmp_root.path(), "workspace1", &outside).unwrap();
        assert!(
            !result,
            "out-of-root delete must return Ok(false) without touching the DB"
        );
    }

    #[test]
    fn try_reingest_changed_file_reports_skip_for_missing_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let conn = open_test_conn();
        let tx = conn.unchecked_transaction().unwrap();
        let pool = ParserPool::new();
        let mut indexed: Vec<IndexedFile> = Vec::new();

        let result = try_reingest_changed_file(
            &tx,
            &root.join("missing.ts"),
            root,
            "",
            max_file_bytes(),
            &pool,
            &mut indexed,
        )
        .unwrap();

        assert!(!result, "missing file must report skip");
        assert!(indexed.is_empty());
    }

    #[test]
    fn try_reingest_changed_file_skips_path_outside_root() {
        // Previously a strip_prefix failure fell through to the absolute
        // path, ingesting the file under a garbage rel_path that could
        // never be looked up again. The fix makes this an explicit
        // warn-and-skip.
        let tmp_root = TempDir::new().unwrap();
        let tmp_other = TempDir::new().unwrap();
        let outside = tmp_other.path().join("outside.ts");
        fs::write(&outside, "export const x = 1;\n").unwrap();

        let conn = open_test_conn();
        let tx = conn.unchecked_transaction().unwrap();
        let pool = ParserPool::new();
        let mut indexed: Vec<IndexedFile> = Vec::new();

        let result = try_reingest_changed_file(
            &tx,
            &outside,
            tmp_root.path(),
            "workspace1",
            max_file_bytes(),
            &pool,
            &mut indexed,
        )
        .unwrap();

        assert!(
            !result,
            "out-of-root file must report skip instead of ingesting under a garbage rel_path"
        );
        assert!(
            indexed.is_empty(),
            "no IndexedFile entry must be produced for an out-of-root path"
        );
    }

    #[test]
    fn try_reingest_changed_file_ingests_existing_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file_path = root.join("a.ts");
        fs::write(&file_path, "export const x = 1;\n").unwrap();

        let conn = open_test_conn();
        let tx = conn.unchecked_transaction().unwrap();
        let pool = ParserPool::new();
        let mut indexed: Vec<IndexedFile> = Vec::new();

        let result = try_reingest_changed_file(
            &tx,
            &file_path,
            root,
            "",
            max_file_bytes(),
            &pool,
            &mut indexed,
        )
        .unwrap();
        tx.commit().unwrap();

        assert!(result);
        assert_eq!(indexed.len(), 1);
    }

    #[test]
    fn try_reingest_changed_file_skips_oversize() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let file_path = root.join("big.ts");
        fs::write(&file_path, "export const x = 1;\n").unwrap();

        let conn = open_test_conn();
        let tx = conn.unchecked_transaction().unwrap();
        let pool = ParserPool::new();
        let mut indexed: Vec<IndexedFile> = Vec::new();

        let result =
            try_reingest_changed_file(&tx, &file_path, root, "", 5, &pool, &mut indexed).unwrap();

        assert!(!result);
        assert!(indexed.is_empty());
    }

    // --- Multi-root prefix behavior --------------------------------------
    //
    // Regression tests for the bug where `incremental_index` wrote rows
    // without the per-root prefix that `full_index_multi` uses, orphaning
    // the original prefixed row on the first save in multi-root mode.

    #[test]
    fn incremental_index_with_prefix_writes_prefixed_row() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("root2");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("foo.ts");
        fs::write(&file, "export const x = 1;\n").unwrap();

        let conn = open_test_conn();
        // Simulate the full_index_multi pass: the DB already has a row at
        // "root2/foo.ts" because that is how full_index_multi keys rows.
        let prefix = root_prefix(&root, None);
        full_index_root(&conn, &root, true, &prefix, &HashSet::new()).unwrap();
        assert!(
            read::get_file_by_path(&conn, &format!("{prefix}/foo.ts"))
                .unwrap()
                .is_some(),
            "full_index_multi must store the prefixed path"
        );

        // Save event: rewrite the file, re-run incremental with the same
        // prefix, and confirm the existing prefixed row is the one updated
        // (no orphan "foo.ts" row is created).
        fs::write(&file, "export const x = 2;\n").unwrap();
        incremental_index_with_prefix(&conn, &root, &prefix, &[file.clone()], &[]).unwrap();

        assert!(
            read::get_file_by_path(&conn, &format!("{prefix}/foo.ts"))
                .unwrap()
                .is_some(),
            "prefixed row must remain after incremental save"
        );
        assert!(
            read::get_file_by_path(&conn, "foo.ts").unwrap().is_none(),
            "incremental must not write an unprefixed orphan"
        );
    }

    #[test]
    fn incremental_index_with_prefix_deletes_prefixed_row() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("root2");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("foo.ts");
        fs::write(&file, "export const x = 1;\n").unwrap();

        let conn = open_test_conn();
        let prefix = root_prefix(&root, None);
        full_index_root(&conn, &root, true, &prefix, &HashSet::new()).unwrap();
        assert!(
            read::get_file_by_path(&conn, &format!("{prefix}/foo.ts"))
                .unwrap()
                .is_some()
        );

        fs::remove_file(&file).unwrap();
        incremental_index_with_prefix(&conn, &root, &prefix, &[], &[file]).unwrap();

        assert!(
            read::get_file_by_path(&conn, &format!("{prefix}/foo.ts"))
                .unwrap()
                .is_none(),
            "delete path must match the prefix to actually remove the row"
        );
    }

    /// Regression for the missing `/` in `changed_rel_paths` under a
    /// multi-root prefix. Pre-fix, reindexing `helper.rs` under prefix
    /// `alpha` built the key `"alphasrc/helper.rs"`, which
    /// `snapshot_cross_file_refs` could never match, so the cross-ref from
    /// `lib.rs::run` -> `helper.rs::do_work` was CASCADEd away by
    /// `clear_file_content` and never recreated (because lib.rs was
    /// unchanged, `resolve_symbol_references` never re-parsed it).
    ///
    /// Post-fix, the key is `"alpha/src/helper.rs"`, the snapshot preserves
    /// the ref, and `restore_cross_file_refs` re-links it to the new
    /// helper-symbol id.
    #[test]
    fn incremental_with_prefix_preserves_cross_file_refs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("subroot");
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();

        fs::write(
            src.join("lib.rs"),
            "pub mod helper;\n\
             use crate::helper::do_work;\n\
             pub fn run() { do_work(); }\n",
        )
        .unwrap();
        let helper_path = src.join("helper.rs");
        fs::write(&helper_path, "pub fn do_work() {}\n").unwrap();

        let conn = open_test_conn();
        let prefix = "alpha".to_string();
        full_index_root(&conn, &root, true, &prefix, &HashSet::new()).unwrap();

        // Precondition: both prefixed rows exist and the cross-ref was
        // resolved during the initial index.
        assert!(
            read::get_file_by_path(&conn, "alpha/src/lib.rs")
                .unwrap()
                .is_some()
        );
        assert!(
            read::get_file_by_path(&conn, "alpha/src/helper.rs")
                .unwrap()
                .is_some()
        );

        let initial_refs: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT sf.name, st.name FROM symbol_refs r
                     JOIN symbols sf ON sf.id = r.from_symbol_id
                     JOIN symbols st ON st.id = r.to_symbol_id
                     ORDER BY sf.name, st.name",
                )
                .unwrap();
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
        };
        assert!(
            initial_refs
                .iter()
                .any(|(f, t)| f == "run" && t == "do_work"),
            "precondition: initial cross-ref must exist, got {initial_refs:?}"
        );

        // Modify only helper.rs. lib.rs stays untouched on disk, so
        // resolve_symbol_references will NOT re-parse it. The only way the
        // (run -> do_work) ref survives is via snapshot_cross_file_refs.
        fs::write(&helper_path, "pub fn do_work() { /* edited */ }\n").unwrap();
        incremental_index_with_prefix(&conn, &root, &prefix, &[helper_path.clone()], &[]).unwrap();

        // The cross-ref MUST survive.
        let final_refs: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT sf.name, st.name FROM symbol_refs r
                     JOIN symbols sf ON sf.id = r.from_symbol_id
                     JOIN symbols st ON st.id = r.to_symbol_id
                     ORDER BY sf.name, st.name",
                )
                .unwrap();
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
        };
        assert!(
            final_refs.iter().any(|(f, t)| f == "run" && t == "do_work"),
            "post-incremental cross-ref must be preserved, got {final_refs:?}"
        );
    }
}
