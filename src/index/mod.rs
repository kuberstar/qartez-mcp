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
use symbols::{ExtractedImport, ExtractedReference, compute_shape_hash};

struct IndexedFile {
    file_id: i64,
    rel_path: String,
    language: String,
    imports: Vec<ExtractedImport>,
    /// DB rowids for the symbols this file contributed, in the same order
    /// as the `ExtractedReference::from_symbol_idx` indices emitted by the
    /// language extractor. Used by the reference-resolution pass to
    /// translate parse-local enclosing indices into real symbol ids.
    symbol_ids: Vec<i64>,
    references: Vec<ExtractedReference>,
}

/// Maximum file size to index. Files larger than this are skipped — they are
/// typically generated code and inflate the index without meaningful signal.
/// Override via the `QARTEZ_MAX_FILE_BYTES` environment variable.
fn max_file_bytes() -> u64 {
    std::env::var("QARTEZ_MAX_FILE_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000_000) // 1 MB default
}

pub fn full_index(conn: &Connection, root: &Path, force: bool) -> Result<()> {
    let files = walker::walk_source_files(root);
    let pool = ParserPool::new();
    let go_module = read_go_module(root);

    tracing::info!("found {} source files on disk", files.len());

    let tx = conn.unchecked_transaction()?;

    let mut indexed: Vec<IndexedFile> = Vec::new();
    let mut known_paths: HashSet<String> = HashSet::new();
    let mut skipped: usize = 0;
    let mut updated: usize = 0;

    for file_path in &files {
        let rel_path = match file_path.strip_prefix(root) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => file_path.to_string_lossy().to_string(),
        };

        let metadata = match std::fs::metadata(file_path) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("cannot stat {}: {e}", file_path.display());
                continue;
            }
        };
        let mtime_ns = file_mtime_ns(&metadata);
        let size_bytes = metadata.len() as i64;

        if metadata.len() > max_file_bytes() {
            tracing::debug!(
                "skipping oversized file {} ({} bytes)",
                file_path.display(),
                metadata.len()
            );
            continue;
        }

        if !force
            && let Some(existing) = read::get_file_by_path(&tx, &rel_path)?
            && existing.mtime_ns == mtime_ns
        {
            known_paths.insert(rel_path.clone());
            skipped += 1;
            continue;
        }

        let source = match std::fs::read(file_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("cannot read {}: {e}", file_path.display());
                continue;
            }
        };

        let (parse_result, language) = match pool.parse_file(file_path, &source) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("parse failed for {}: {e}", file_path.display());
                continue;
            }
        };

        let newline_count = source.iter().filter(|&&b| b == b'\n').count();
        let line_count = if source.last() == Some(&b'\n') || source.is_empty() {
            newline_count as i64
        } else {
            newline_count as i64 + 1
        };

        if let Some(existing) = read::get_file_by_path(&tx, &rel_path)? {
            write::delete_file_data(&tx, existing.id)?;
        }

        let file_id =
            write::upsert_file(&tx, &rel_path, mtime_ns, size_bytes, &language, line_count)?;

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
                shape_hash: compute_shape_hash(&source, s.line_start, s.line_end),
                unused_excluded: s.unused_excluded,
                parent_idx: s.parent_idx,
                complexity: s.complexity,
            })
            .collect();

        let symbol_ids = write::insert_symbols(&tx, file_id, &symbol_inserts)?;
        known_paths.insert(rel_path.clone());
        updated += 1;

        indexed.push(IndexedFile {
            file_id,
            rel_path,
            language,
            imports: parse_result.imports,
            symbol_ids,
            references: parse_result.references,
        });

        tracing::debug!(
            "indexed {} ({} symbols)",
            file_path.display(),
            symbol_inserts.len()
        );
    }

    let db_files = read::get_all_files(&tx)?;
    let mut deleted: usize = 0;
    for db_file in &db_files {
        if !known_paths.contains(&db_file.path) {
            let full_path = root.join(&db_file.path);
            if !full_path.exists() {
                write::delete_file_data(&tx, db_file.id)?;
                deleted += 1;
                tracing::debug!("removed stale file from index: {}", db_file.path);
            }
        }
    }

    let path_to_id: HashMap<String, i64> = {
        let all_files = read::get_all_files(&tx)?;
        all_files.into_iter().map(|f| (f.path, f.id)).collect()
    };

    // Import resolution pass: writes edge rows AND records, per file, the
    // set of files we actually imported from. The reference resolver below
    // uses that set as the Priority-2 lookup ("target symbol lives in a
    // file we import").
    let mut imports_by_file: HashMap<i64, HashSet<i64>> = HashMap::new();
    for entry in &indexed {
        let targets_for_entry = imports_by_file.entry(entry.file_id).or_default();
        for import in &entry.imports {
            let targets = resolve_targets(
                &entry.language,
                &entry.rel_path,
                &import.source,
                root,
                &known_paths,
                go_module.as_deref(),
            );
            for target_rel in &targets {
                if let Some(&target_id) = path_to_id.get(target_rel.as_str()) {
                    write::insert_edge(
                        &tx,
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

    resolve_symbol_references(&tx, &indexed, &imports_by_file)?;

    write::sync_fts(&tx)?;
    write::rebuild_symbol_bodies(&tx, root)?;
    write::populate_unused_exports(&tx)?;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    write::set_meta(&tx, "last_index", &timestamp)?;

    tx.commit()?;

    // Checkpoint the WAL so it doesn't grow unboundedly across indexing runs.
    // Failure is non-fatal — the next run or SQLite's auto-checkpoint will
    // eventually flush it.
    if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
        tracing::debug!("WAL checkpoint after full_index failed (non-fatal): {e}");
    }

    tracing::info!("indexing complete: {updated} updated, {skipped} skipped, {deleted} deleted");
    Ok(())
}

/// Second-pass reference resolution. Runs after every file has been parsed
/// and every import edge inserted, so we can attribute each extracted
/// identifier to a concrete `symbols.id` via a same-file → imported-file →
/// global-unique priority. Results are batched and written to
/// `symbol_refs` in a single prepared-statement loop.
///
/// The approach intentionally mirrors Aider's heuristic symbol graph:
/// extractors capture identifiers liberally and this resolver decides —
/// using file-level import edges — which target is the most plausible.
/// Ambiguous names that match many symbols and no import are dropped to
/// keep the edge count manageable on large codebases.
fn resolve_symbol_references(
    conn: &Connection,
    indexed: &[IndexedFile],
    imports_by_file: &HashMap<i64, HashSet<i64>>,
) -> Result<()> {
    // (name -> [(symbol_id, file_id)]) built once for the whole project.
    let all_syms = read::get_all_symbols_with_path(conn)?;
    let mut name_index: HashMap<String, Vec<(i64, i64)>> = HashMap::with_capacity(all_syms.len());
    for (sym, _path) in &all_syms {
        name_index
            .entry(sym.name.clone())
            .or_default()
            .push((sym.id, sym.file_id));
    }

    let mut batch: Vec<(i64, i64, &'static str)> = Vec::new();
    let mut resolved = 0usize;
    let mut dropped_no_enclosing = 0usize;
    let mut dropped_no_candidate = 0usize;
    let mut dropped_ambiguous = 0usize;

    for entry in indexed {
        let empty_imports = HashSet::new();
        let imported = imports_by_file
            .get(&entry.file_id)
            .unwrap_or(&empty_imports);

        for reference in &entry.references {
            // Module-scope references (no enclosing symbol) are dropped in
            // v1 because PageRank only ranks the `(from_symbol, to_symbol)`
            // edges it can attribute. Wiring up a synthetic "module" node
            // per file is a v2 idea.
            let Some(from_idx) = reference.from_symbol_idx else {
                dropped_no_enclosing += 1;
                continue;
            };
            let Some(&from_id) = entry.symbol_ids.get(from_idx) else {
                dropped_no_enclosing += 1;
                continue;
            };

            let candidates = match name_index.get(&reference.name) {
                Some(c) if !c.is_empty() => c,
                _ => {
                    dropped_no_candidate += 1;
                    continue;
                }
            };

            // Priority 1: target lives in the same file as the caller.
            let mut picked: Vec<i64> = candidates
                .iter()
                .filter(|(sid, fid)| *fid == entry.file_id && *sid != from_id)
                .map(|(sid, _)| *sid)
                .collect();

            // Priority 2: target lives in a file this caller imports from.
            if picked.is_empty() {
                picked = candidates
                    .iter()
                    .filter(|(_, fid)| imported.contains(fid))
                    .map(|(sid, _)| *sid)
                    .collect();
            }

            // Priority 3: unique global match. Ambiguous global names are
            // dropped — with no import evidence and multiple candidates
            // there is no principled way to pick, and keeping them all
            // would bury the signal under noise on large projects.
            if picked.is_empty() {
                if candidates.len() == 1 {
                    picked.push(candidates[0].0);
                } else {
                    dropped_ambiguous += 1;
                    continue;
                }
            }

            for target in picked {
                batch.push((from_id, target, reference.kind.as_str()));
                resolved += 1;
            }
        }
    }

    write::insert_symbol_refs(conn, &batch)?;

    tracing::info!(
        "symbol references: {} resolved, {} dropped (no enclosing), {} dropped (no candidate), {} dropped (ambiguous)",
        resolved,
        dropped_no_enclosing,
        dropped_no_candidate,
        dropped_ambiguous,
    );

    Ok(())
}

fn resolve_targets(
    language: &str,
    rel_path: &str,
    specifier: &str,
    root: &Path,
    known_files: &HashSet<String>,
    go_module: Option<&str>,
) -> Vec<String> {
    match language {
        "rust" => resolve_rust_import(rel_path, specifier, known_files)
            .into_iter()
            .collect(),
        "python" => resolve_python_import(rel_path, specifier, known_files)
            .into_iter()
            .collect(),
        "go" => resolve_go_import(specifier, known_files, go_module),
        _ => {
            let importing_file = root.join(rel_path);
            resolve_import(&importing_file, specifier, root, known_files)
                .into_iter()
                .collect()
        }
    }
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
                let rel = rel.to_string_lossy().to_string();
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
                let rel = rel.to_string_lossy().to_string();
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
            Ok(r) => r.to_string_lossy().to_string(),
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
            Ok(r) => r.to_string_lossy().to_string(),
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
    if segments.is_empty() {
        return None;
    }

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
                    format!("{}/{rest}", base.display())
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
                parent.to_string_lossy().to_string()
            } else {
                let stem = file_path.file_stem()?.to_str()?;
                format!("{}/{stem}", parent.display())
            };

            let target = format!("{self_dir}/{rest}");
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
    let dir_str = dir.to_string_lossy();
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
) -> Option<String> {
    if !specifier.starts_with('.') {
        return None;
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
        base.to_string_lossy().to_string()
    } else if base.as_os_str().is_empty() {
        module_path
    } else {
        format!("{}/{module_path}", base.display())
    };

    for suffix in [".py", "/__init__.py"] {
        let candidate = format!("{target}{suffix}");
        if known_files.contains(&candidate) {
            return Some(candidate);
        }
    }

    None
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
                Some(p) => p.to_string_lossy() == rel_dir,
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
/// `changed` — paths that were created or modified on disk.
/// `deleted` — paths that were removed from disk.
///
/// After updating the per-file rows, the function re-resolves import
/// edges and symbol references for the changed files, then rebuilds the
/// global FTS and unused-export tables.
pub fn incremental_index(
    conn: &Connection,
    root: &Path,
    changed: &[PathBuf],
    deleted: &[PathBuf],
) -> Result<()> {
    if changed.is_empty() && deleted.is_empty() {
        return Ok(());
    }

    let pool = ParserPool::new();
    let go_module = read_go_module(root);

    let tx = conn.unchecked_transaction()?;

    // --- Phase 1: remove deleted files ---
    let mut removed = 0usize;
    for path in deleted {
        let rel_path = match path.strip_prefix(root) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => path.to_string_lossy().to_string(),
        };
        if let Some(existing) = read::get_file_by_path(&tx, &rel_path)? {
            write::delete_file_data(&tx, existing.id)?;
            removed += 1;
        }
    }

    // --- Phase 2: re-index changed files ---
    let mut indexed: Vec<IndexedFile> = Vec::new();
    let mut updated = 0usize;

    for file_path in changed {
        let rel_path = match file_path.strip_prefix(root) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => file_path.to_string_lossy().to_string(),
        };

        let metadata = match std::fs::metadata(file_path) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("incremental: cannot stat {}: {e}", file_path.display());
                continue;
            }
        };
        let mtime_ns = file_mtime_ns(&metadata);
        let size_bytes = metadata.len() as i64;

        if metadata.len() > max_file_bytes() {
            tracing::debug!(
                "incremental: skipping oversized file {} ({} bytes)",
                file_path.display(),
                metadata.len()
            );
            continue;
        }

        let source = match std::fs::read(file_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("incremental: cannot read {}: {e}", file_path.display());
                continue;
            }
        };

        let (parse_result, language) = match pool.parse_file(file_path, &source) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("incremental: parse failed for {}: {e}", file_path.display());
                continue;
            }
        };

        let newline_count = source.iter().filter(|&&b| b == b'\n').count();
        let line_count = if source.last() == Some(&b'\n') || source.is_empty() {
            newline_count as i64
        } else {
            newline_count as i64 + 1
        };

        // If the file already exists, clear its derived content (symbols,
        // outgoing edges) while preserving the file_id and incoming edges.
        if let Some(existing) = read::get_file_by_path(&tx, &rel_path)? {
            write::clear_file_content(&tx, existing.id)?;
        }

        let file_id =
            write::upsert_file(&tx, &rel_path, mtime_ns, size_bytes, &language, line_count)?;

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
                shape_hash: compute_shape_hash(&source, s.line_start, s.line_end),
                unused_excluded: s.unused_excluded,
                parent_idx: s.parent_idx,
                complexity: s.complexity,
            })
            .collect();

        let symbol_ids = write::insert_symbols(&tx, file_id, &symbol_inserts)?;
        updated += 1;

        indexed.push(IndexedFile {
            file_id,
            rel_path,
            language,
            imports: parse_result.imports,
            symbol_ids,
            references: parse_result.references,
        });
    }

    // --- Phase 3: resolve edges & references for changed files ---
    // Build the full path→id map from the DB (includes unchanged files).
    let path_to_id: HashMap<String, i64> = {
        let all_files = read::get_all_files(&tx)?;
        all_files.into_iter().map(|f| (f.path, f.id)).collect()
    };
    let known_paths: HashSet<String> = path_to_id.keys().cloned().collect();

    let mut imports_by_file: HashMap<i64, HashSet<i64>> = HashMap::new();
    for entry in &indexed {
        let targets_for_entry = imports_by_file.entry(entry.file_id).or_default();
        for import in &entry.imports {
            let targets = resolve_targets(
                &entry.language,
                &entry.rel_path,
                &import.source,
                root,
                &known_paths,
                go_module.as_deref(),
            );
            for target_rel in &targets {
                if let Some(&target_id) = path_to_id.get(target_rel.as_str()) {
                    write::insert_edge(
                        &tx,
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

    resolve_symbol_references(&tx, &indexed, &imports_by_file)?;

    // --- Phase 4: update derived tables ---
    // Update FTS and body index only for the files that actually changed.
    // This avoids the O(whole-codebase) full-table DELETE+re-insert that
    // sync_fts / rebuild_symbol_bodies would trigger on every file-save
    // event — the primary cause of unbounded WAL growth on large codebases.
    // clear_file_content (called above per file) already removed the old
    // FTS rows, so here we only need to insert the new ones.
    for entry in &indexed {
        write::insert_fts_for_file(&tx, entry.file_id)?;
        write::rebuild_symbol_bodies_for_file(&tx, root, entry.file_id, &entry.rel_path)?;
    }
    write::populate_unused_exports(&tx)?;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    write::set_meta(&tx, "last_index", &timestamp)?;

    tx.commit()?;

    // Checkpoint the WAL after each incremental index to prevent unbounded
    // growth on large codebases with an active file watcher.
    if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
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
    fn test_rust_external_crate_ignored() {
        let known = HashSet::new();
        let result = resolve_rust_import("src/main.rs", "serde::Serialize", &known);
        assert_eq!(result, None);
    }

    // --- Python resolver ---

    #[test]
    fn test_python_relative_single_dot() {
        let mut known = HashSet::new();
        known.insert("pkg/utils.py".to_string());

        let result = resolve_python_import("pkg/main.py", ".utils", &known);
        assert_eq!(result, Some("pkg/utils.py".to_string()));
    }

    #[test]
    fn test_python_relative_double_dot() {
        let mut known = HashSet::new();
        known.insert("pkg/models.py".to_string());

        let result = resolve_python_import("pkg/sub/module.py", "..models", &known);
        assert_eq!(result, Some("pkg/models.py".to_string()));
    }

    #[test]
    fn test_python_relative_init() {
        let mut known = HashSet::new();
        known.insert("pkg/utils/__init__.py".to_string());

        let result = resolve_python_import("pkg/main.py", ".utils", &known);
        assert_eq!(result, Some("pkg/utils/__init__.py".to_string()));
    }

    #[test]
    fn test_python_absolute_skipped() {
        let known = HashSet::new();
        let result = resolve_python_import("pkg/main.py", "os", &known);
        assert_eq!(result, None);
    }

    #[test]
    fn test_python_dotted_module() {
        let mut known = HashSet::new();
        known.insert("pkg/sub/helpers.py".to_string());

        let result = resolve_python_import("pkg/main.py", ".sub.helpers", &known);
        assert_eq!(result, Some("pkg/sub/helpers.py".to_string()));
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

    // --- Integration tests ---

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
            "expected (caller -> helper) edge, got {:?}",
            refs
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
            "expected (run -> do_work) edge across files, got {:?}",
            refs
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
            "expected (caller -> helper) edge for Python, got {:?}",
            refs
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
            "ambiguous global `common` should not resolve, got {:?}",
            caller_to_common
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
}
