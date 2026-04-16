use rusqlite::{Connection, OptionalExtension};

use crate::error::Result;
use crate::storage::models::{self, CoChangeRow, EdgeRow, FileRow, SymbolRow};

/// Sanitize user input for FTS5 MATCH queries. Plain alphanumeric tokens
/// (with `_` and `*`) pass through; anything else is wrapped in a
/// double-quoted phrase so FTS5 treats it as a literal, preventing parse
/// errors from operators like `AND`, `OR`, `NOT`, or column filters (`:`).
pub fn sanitize_fts_query(raw: &str) -> String {
    let is_plain = !raw.is_empty()
        && raw
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '*');
    if is_plain {
        let upper = raw.to_uppercase();
        if matches!(upper.as_str(), "AND" | "OR" | "NOT" | "NEAR") {
            return format!("\"{raw}\"");
        }
        return raw.to_string();
    }
    let escaped = raw.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

/// One symbol resolution: the symbol itself, the file that defines it, and the
/// edges + source files that import the defining file.
pub type SymbolWithImporters = (SymbolRow, FileRow, Vec<(EdgeRow, FileRow)>);

fn row_to_file(row: &rusqlite::Row) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        id: row.get("id")?,
        path: row.get("path")?,
        mtime_ns: row.get("mtime_ns")?,
        size_bytes: row.get("size_bytes")?,
        language: row.get("language")?,
        line_count: row.get("line_count")?,
        pagerank: row.get("pagerank")?,
        indexed_at: row.get("indexed_at")?,
        change_count: row.get::<_, i64>("change_count").unwrap_or(0),
    })
}

fn row_to_symbol(row: &rusqlite::Row) -> rusqlite::Result<SymbolRow> {
    let is_exported_int: i32 = row.get("is_exported")?;
    Ok(SymbolRow {
        id: row.get("id")?,
        file_id: row.get("file_id")?,
        name: row.get("name")?,
        kind: row.get("kind")?,
        line_start: row.get("line_start")?,
        line_end: row.get("line_end")?,
        signature: row.get("signature")?,
        is_exported: is_exported_int != 0,
        shape_hash: row.get("shape_hash")?,
        parent_id: row.get::<_, Option<i64>>("parent_id")?,
        pagerank: row.get::<_, f64>("pagerank").unwrap_or(0.0),
        complexity: row.get::<_, Option<u32>>("complexity").unwrap_or(None),
        owner_type: row.get::<_, Option<String>>("owner_type").unwrap_or(None),
    })
}

/// Deserialize a `FileRow` from a JOINed query where file columns are aliased
/// with the `f_` prefix to avoid collisions with symbol columns (e.g. `id`,
/// `pagerank`). Pair with SQL: `f.id AS f_id, f.path AS f_path, ...`.
fn row_to_file_joined(row: &rusqlite::Row) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        id: row.get("f_id")?,
        path: row.get("f_path")?,
        mtime_ns: row.get("f_mtime_ns")?,
        size_bytes: row.get("f_size_bytes")?,
        language: row.get("f_language")?,
        line_count: row.get("f_line_count")?,
        pagerank: row.get("f_pagerank")?,
        indexed_at: row.get("f_indexed_at")?,
        change_count: row.get::<_, i64>("f_change_count").unwrap_or(0),
    })
}

const SYMBOL_FILE_JOIN_COLS: &str = "s.id, s.file_id, s.name, s.kind, s.line_start, s.line_end,
     s.signature, s.is_exported, s.shape_hash, s.parent_id, s.pagerank,
     s.complexity, s.owner_type,
     f.id AS f_id, f.path AS f_path, f.mtime_ns AS f_mtime_ns,
     f.size_bytes AS f_size_bytes, f.language AS f_language,
     f.line_count AS f_line_count, f.pagerank AS f_pagerank,
     f.indexed_at AS f_indexed_at, f.change_count AS f_change_count";

pub fn get_file_by_path(conn: &Connection, path: &str) -> Result<Option<FileRow>> {
    let result = conn
        .query_row(
            "SELECT id, path, mtime_ns, size_bytes, language, line_count, pagerank, indexed_at, change_count
             FROM files WHERE path = ?1",
            [path],
            row_to_file,
        )
        .optional()?;
    Ok(result)
}

pub fn get_all_files(conn: &Connection) -> Result<Vec<FileRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, mtime_ns, size_bytes, language, line_count, pagerank, indexed_at, change_count
         FROM files ORDER BY path",
    )?;
    let rows = stmt.query_map([], row_to_file)?;
    let mut files = Vec::new();
    for row in rows {
        files.push(row?);
    }
    Ok(files)
}

pub fn get_files_ranked(conn: &Connection, limit: i64) -> Result<Vec<FileRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, mtime_ns, size_bytes, language, line_count, pagerank, indexed_at, change_count
         FROM files ORDER BY pagerank DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], row_to_file)?;
    let mut files = Vec::new();
    for row in rows {
        files.push(row?);
    }
    Ok(files)
}

pub fn get_all_files_ranked(conn: &Connection) -> Result<Vec<FileRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, mtime_ns, size_bytes, language, line_count, pagerank, indexed_at, change_count
         FROM files ORDER BY pagerank DESC",
    )?;
    let rows = stmt.query_map([], row_to_file)?;
    let mut files = Vec::new();
    for row in rows {
        files.push(row?);
    }
    Ok(files)
}

pub fn get_all_symbols_with_path(conn: &Connection) -> Result<Vec<(SymbolRow, String)>> {
    let sql = format!(
        "SELECT {SYMBOL_FILE_JOIN_COLS}
         FROM symbols s
         JOIN files f ON s.file_id = f.id
         ORDER BY f.path, s.line_start"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((row_to_symbol(row)?, row.get::<_, String>("f_path")?))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

pub fn get_file_by_id(conn: &Connection, id: i64) -> Result<Option<FileRow>> {
    let result = conn
        .query_row(
            "SELECT id, path, mtime_ns, size_bytes, language, line_count, pagerank, indexed_at, change_count
             FROM files WHERE id = ?1",
            [id],
            row_to_file,
        )
        .optional()?;
    Ok(result)
}

pub fn get_symbols_for_file(conn: &Connection, file_id: i64) -> Result<Vec<SymbolRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_id, name, kind, line_start, line_end, signature, is_exported, shape_hash, parent_id, pagerank, complexity, owner_type
         FROM symbols WHERE file_id = ?1 ORDER BY line_start",
    )?;
    let rows = stmt.query_map([file_id], row_to_symbol)?;
    let mut symbols = Vec::new();
    for row in rows {
        symbols.push(row?);
    }
    Ok(symbols)
}

pub fn find_symbol_by_name(conn: &Connection, name: &str) -> Result<Vec<(SymbolRow, FileRow)>> {
    // Use the prepared-statement cache: this function is called in tight
    // loops by `qartez_calls` depth-2 resolution (~30 queries per invocation),
    // where re-compiling the SQL each time dominates the walk cost.
    let sql = format!(
        "SELECT {SYMBOL_FILE_JOIN_COLS}
         FROM symbols s
         JOIN files f ON s.file_id = f.id
         WHERE s.name = ?1"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([name], |row| {
        Ok((row_to_symbol(row)?, row_to_file_joined(row)?))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

pub fn search_symbols_fts(
    conn: &Connection,
    query: &str,
    limit: i64,
) -> Result<Vec<(SymbolRow, String)>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.file_id, s.name, s.kind, s.line_start, s.line_end,
                s.signature, s.is_exported, s.shape_hash, s.parent_id, s.pagerank,
                s.complexity, s.owner_type, fts.file_path
         FROM symbols_fts fts
         JOIN symbols s ON s.id = fts.rowid
         WHERE symbols_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![query, limit], |row| {
        Ok((row_to_symbol(row)?, row.get::<_, String>("file_path")?))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

#[allow(dead_code)]
pub fn get_edges_from(conn: &Connection, file_id: i64) -> Result<Vec<EdgeRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, from_file, to_file, kind, specifier
         FROM edges WHERE from_file = ?1",
    )?;
    let rows = stmt.query_map([file_id], |row| {
        Ok(EdgeRow {
            id: row.get(0)?,
            from_file: row.get(1)?,
            to_file: row.get(2)?,
            kind: row.get(3)?,
            specifier: row.get(4)?,
        })
    })?;
    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?);
    }
    Ok(edges)
}

pub fn get_edges_to(conn: &Connection, file_id: i64) -> Result<Vec<EdgeRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, from_file, to_file, kind, specifier
         FROM edges WHERE to_file = ?1",
    )?;
    let rows = stmt.query_map([file_id], |row| {
        Ok(EdgeRow {
            id: row.get(0)?,
            from_file: row.get(1)?,
            to_file: row.get(2)?,
            kind: row.get(3)?,
            specifier: row.get(4)?,
        })
    })?;
    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?);
    }
    Ok(edges)
}

pub fn get_all_edges(conn: &Connection) -> Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare("SELECT from_file, to_file FROM edges")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?);
    }
    Ok(edges)
}

/// All `(from_symbol_id, to_symbol_id)` tuples in the symbol-level graph,
/// used as the edge list for `compute_symbol_pagerank`. Kind is intentionally
/// dropped — PageRank does not distinguish between call / use / type edges
/// in v1; every edge contributes equally to the random walk.
pub fn get_all_symbol_refs(conn: &Connection) -> Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare("SELECT from_symbol_id, to_symbol_id FROM symbol_refs")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut refs = Vec::new();
    for row in rows {
        refs.push(row?);
    }
    Ok(refs)
}

/// All symbols in the DB, used as the node set for `compute_symbol_pagerank`
/// so even unreferenced symbols end up with a valid (zero-ish) rank. Wraps
/// `get_symbols_for_file` across every file with a single query instead of N.
pub fn get_all_symbols(conn: &Connection) -> Result<Vec<SymbolRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_id, name, kind, line_start, line_end, signature, is_exported, shape_hash, parent_id, pagerank, complexity, owner_type
         FROM symbols",
    )?;
    let rows = stmt.query_map([], row_to_symbol)?;
    let mut syms = Vec::new();
    for row in rows {
        syms.push(row?);
    }
    Ok(syms)
}

/// Top symbols by `symbols.pagerank`, joined with their defining file for
/// display in `qartez_map by=symbols` and benchmark targets. Returns at most
/// `limit` rows ordered by descending rank.
pub fn get_symbols_ranked(conn: &Connection, limit: i64) -> Result<Vec<(SymbolRow, FileRow)>> {
    let sql = format!(
        "SELECT {SYMBOL_FILE_JOIN_COLS}
         FROM symbols s
         JOIN files f ON s.file_id = f.id
         ORDER BY s.pagerank DESC
         LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([limit], |row| {
        Ok((row_to_symbol(row)?, row_to_file_joined(row)?))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Top symbols by `symbols.pagerank` scoped to a single file. Used by the
/// guard's deny-message enrichment (so blocks from a hot file tell Claude
/// which specific symbols matter) and by `qartez_impact`'s per-symbol
/// breakdown section.
pub fn get_symbols_ranked_for_file(
    conn: &Connection,
    file_id: i64,
    limit: i64,
) -> Result<Vec<SymbolRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_id, name, kind, line_start, line_end,
                signature, is_exported, shape_hash, parent_id, pagerank, complexity, owner_type
         FROM symbols
         WHERE file_id = ?1
         ORDER BY pagerank DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![file_id, limit], row_to_symbol)?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

pub fn get_cochanges(
    conn: &Connection,
    file_id: i64,
    limit: i64,
) -> Result<Vec<(CoChangeRow, FileRow)>> {
    let mut stmt = conn.prepare(
        "SELECT cc.file_a, cc.file_b, cc.count,
                f.id, f.path, f.mtime_ns, f.size_bytes, f.language,
                f.line_count, f.pagerank, f.indexed_at, f.change_count
         FROM co_changes cc
         JOIN files f ON (CASE WHEN cc.file_a = ?1 THEN cc.file_b ELSE cc.file_a END) = f.id
         WHERE cc.file_a = ?1 OR cc.file_b = ?1
         ORDER BY cc.count DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![file_id, limit], |row| {
        Ok((
            CoChangeRow {
                file_a: row.get(0)?,
                file_b: row.get(1)?,
                count: row.get(2)?,
            },
            FileRow {
                id: row.get(3)?,
                path: row.get(4)?,
                mtime_ns: row.get(5)?,
                size_bytes: row.get(6)?,
                language: row.get(7)?,
                line_count: row.get(8)?,
                pagerank: row.get(9)?,
                indexed_at: row.get(10)?,
                change_count: row.get(11)?,
            },
        ))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Total number of materialized unused exports (after index-time exclusion
/// of trait-impl methods and macro-invocation spans). Used by `qartez_unused`
/// to report the full count even when a paginated window is returned.
pub fn count_unused_exports(conn: &Connection) -> Result<i64> {
    // Fall back to the on-the-fly query when the materialized table is empty
    // (e.g. running against an index written by an older binary that didn't
    // populate `unused_exports`). The fallback is slow but keeps the tool
    // usable during the migration window.
    let materialized: i64 =
        conn.query_row("SELECT COUNT(*) FROM unused_exports", [], |r| r.get(0))?;
    if materialized > 0 {
        return Ok(materialized);
    }
    let fallback: i64 = conn.query_row(
        "SELECT COUNT(*) FROM symbols s
         WHERE s.is_exported = 1
           AND COALESCE(s.unused_excluded, 0) = 0
           AND NOT EXISTS (SELECT 1 FROM edges e WHERE e.to_file = s.file_id)
           AND NOT EXISTS (SELECT 1 FROM symbol_refs sr WHERE sr.to_symbol_id = s.id)",
        [],
        |r| r.get(0),
    )?;
    Ok(fallback)
}

/// Paginated slice of the pre-materialized unused-exports table. Populated
/// by `populate_unused_exports` at index time after edges have been built,
/// so query-time work is one JOIN-plus-LIMIT — no tree walk, no per-file
/// exclusion-zone recompute.
pub fn get_unused_exports_page(
    conn: &Connection,
    limit: i64,
    offset: i64,
) -> Result<Vec<(SymbolRow, FileRow)>> {
    let sql = format!(
        "SELECT {SYMBOL_FILE_JOIN_COLS}
         FROM unused_exports ue
         JOIN symbols s ON s.id = ue.symbol_id
         JOIN files f ON f.id = ue.file_id
         ORDER BY f.path, s.line_start
         LIMIT ?1 OFFSET ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let row_mapper = |row: &rusqlite::Row| Ok((row_to_symbol(row)?, row_to_file_joined(row)?));
    let rows = stmt.query_map(rusqlite::params![limit, offset], row_mapper)?;
    let mut results: Vec<(SymbolRow, FileRow)> = Vec::new();
    for row in rows {
        results.push(row?);
    }

    // Fallback: the materialized table is empty (freshly-migrated DB or
    // benchmark fixture that called `write::insert_symbols` without the
    // follow-up `populate_unused_exports`). Compute on the fly using the
    // `unused_excluded` column so callers still get correct results.
    if results.is_empty() && offset == 0 {
        let sql = format!(
            "SELECT {SYMBOL_FILE_JOIN_COLS}
             FROM symbols s
             JOIN files f ON s.file_id = f.id
             WHERE s.is_exported = 1
               AND COALESCE(s.unused_excluded, 0) = 0
               AND NOT EXISTS (SELECT 1 FROM edges e WHERE e.to_file = s.file_id)
               AND NOT EXISTS (SELECT 1 FROM symbol_refs sr WHERE sr.to_symbol_id = s.id)
             ORDER BY f.path, s.line_start
             LIMIT ?1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![limit], row_mapper)?;
        for row in rows {
            results.push(row?);
        }
    }
    Ok(results)
}

/// Legacy name kept for the in-crate test in `read::tests` (which still
/// exercises the on-the-fly fallback on a fixture that never calls the
/// materializer). Returns the unpaginated slice so assertions don't need
/// to thread limits through.
#[cfg(test)]
pub fn get_exported_symbols_not_imported(conn: &Connection) -> Result<Vec<(SymbolRow, FileRow)>> {
    get_unused_exports_page(conn, i64::MAX, 0)
}

/// Count distinct clone groups (shape hashes shared by 2+ symbols).
pub fn count_clone_groups(conn: &Connection, min_lines: u32) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (
             SELECT shape_hash FROM symbols
             WHERE shape_hash IS NOT NULL
               AND (line_end - line_start + 1) >= ?1
             GROUP BY shape_hash
             HAVING COUNT(*) >= 2
         )",
        [min_lines],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// A single clone group: the shared shape hash and all symbols that match it.
pub struct CloneGroup {
    pub shape_hash: String,
    pub symbols: Vec<(SymbolRow, FileRow)>,
}

/// Return clone groups ordered by group size (largest first), with pagination.
pub fn get_clone_groups(
    conn: &Connection,
    min_lines: u32,
    limit: i64,
    offset: i64,
) -> Result<Vec<CloneGroup>> {
    let mut hash_stmt = conn.prepare(
        "SELECT shape_hash, COUNT(*) as cnt FROM symbols
         WHERE shape_hash IS NOT NULL
           AND (line_end - line_start + 1) >= ?1
         GROUP BY shape_hash
         HAVING cnt >= 2
         ORDER BY cnt DESC
         LIMIT ?2 OFFSET ?3",
    )?;

    let hashes: Vec<(String, i64)> = hash_stmt
        .query_map(rusqlite::params![min_lines, limit, offset], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let members_sql = format!(
        "SELECT {SYMBOL_FILE_JOIN_COLS}
         FROM symbols s
         JOIN files f ON f.id = s.file_id
         WHERE s.shape_hash = ?1
         ORDER BY f.path, s.line_start"
    );
    let mut members_stmt = conn.prepare(&members_sql)?;

    let mut groups = Vec::with_capacity(hashes.len());
    for (hash, _cnt) in &hashes {
        let syms: Vec<(SymbolRow, FileRow)> = members_stmt
            .query_map([hash], |row| {
                Ok((row_to_symbol(row)?, row_to_file_joined(row)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        groups.push(CloneGroup {
            shape_hash: hash.clone(),
            symbols: syms,
        });
    }
    Ok(groups)
}

/// Find a symbol and its true symbol-level references, grouped by file.
///
/// Previously this walked the file-level `edges` table and returned every
/// file that imported the defining file, which is noisy for multi-symbol
/// files: every caller of any symbol in `src/utils.rs` used to appear as
/// a "reference" to every OTHER symbol in `src/utils.rs`. The v2 impl
/// queries `symbol_refs` directly, so only files that actually call,
/// use, or type-reference the target symbol show up.
///
/// The returned shape intentionally matches the old signature so existing
/// callers (`qartez_refs`, `qartez_rename`) keep working — the `EdgeRow` in
/// each `(EdgeRow, FileRow)` tuple is synthesised with `kind = "symbol_ref"`
/// so consumers that only read `FileRow` see unchanged behaviour.
pub fn get_symbol_references(
    conn: &Connection,
    symbol_name: &str,
) -> Result<Vec<SymbolWithImporters>> {
    let symbols = find_symbol_by_name(conn, symbol_name)?;
    let mut results = Vec::new();

    for (sym, file) in symbols {
        // Query symbol_refs for every symbol that points at this definition
        // and hydrate the caller's file row in a single JOIN.
        let mut stmt = conn.prepare_cached(
            "SELECT DISTINCT f.id, f.path, f.mtime_ns, f.size_bytes, f.language,
                             f.line_count, f.pagerank, f.indexed_at, f.change_count
             FROM symbol_refs r
             JOIN symbols s ON s.id = r.from_symbol_id
             JOIN files f ON f.id = s.file_id
             WHERE r.to_symbol_id = ?1",
        )?;
        let rows = stmt.query_map([sym.id], row_to_file)?;
        let mut importers: Vec<(EdgeRow, FileRow)> = Vec::new();
        for row in rows {
            let importer_file = row?;
            // Synthesise an EdgeRow so callers that still read
            // `edge.from_file` / `edge.kind` keep compiling. The `id` field
            // is meaningless for synthesised edges and is set to 0 so any
            // code that treats it as a DB rowid crashes early rather than
            // silently dereferencing a made-up row.
            importers.push((
                EdgeRow {
                    id: 0,
                    from_file: importer_file.id,
                    to_file: file.id,
                    kind: "symbol_ref".to_string(),
                    specifier: None,
                },
                importer_file,
            ));
        }
        results.push((sym, file, importers));
    }

    Ok(results)
}

#[allow(dead_code)]
pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let result = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(result)
}

#[allow(dead_code)]
pub fn get_stale_files(conn: &Connection) -> Result<Vec<FileRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, mtime_ns, size_bytes, language, line_count, pagerank, indexed_at, change_count
         FROM files
         WHERE NOT EXISTS (
             SELECT 1 FROM symbols WHERE symbols.file_id = files.id
         )
         ORDER BY path",
    )?;
    let rows = stmt.query_map([], row_to_file)?;
    let mut files = Vec::new();
    for row in rows {
        files.push(row?);
    }
    Ok(files)
}

pub fn get_file_count(conn: &Connection) -> Result<i64> {
    let count = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
    Ok(count)
}

pub fn get_symbol_count(conn: &Connection) -> Result<i64> {
    let count = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
    Ok(count)
}

pub fn get_language_stats(conn: &Connection) -> Result<Vec<LanguageStat>> {
    let mut stmt = conn.prepare(
        "SELECT f.language,
                COUNT(DISTINCT f.id) as count,
                COALESCE(SUM(f.line_count), 0) as lines,
                COALESCE(SUM(f.size_bytes), 0) as bytes,
                COALESCE(COUNT(s.id), 0) as symbols
         FROM files f
         LEFT JOIN symbols s ON s.file_id = f.id
         GROUP BY f.language
         ORDER BY count DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(LanguageStat {
            language: row.get::<_, String>(0)?,
            file_count: row.get::<_, i64>(1)?,
            line_count: row.get::<_, i64>(2)?,
            byte_count: row.get::<_, i64>(3)?,
            symbol_count: row.get::<_, i64>(4)?,
        })
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

#[derive(Debug, Clone)]
pub struct LanguageStat {
    pub language: String,
    pub file_count: i64,
    pub line_count: i64,
    pub byte_count: i64,
    pub symbol_count: i64,
}

pub fn get_most_imported_files(conn: &Connection, limit: i64) -> Result<Vec<(FileRow, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.path, f.mtime_ns, f.size_bytes, f.language, f.line_count,
                f.pagerank, f.indexed_at, COUNT(*) as importers, f.change_count
         FROM edges e
         JOIN files f ON e.to_file = f.id
         GROUP BY e.to_file
         ORDER BY importers DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], |row| {
        Ok((
            FileRow {
                id: row.get(0)?,
                path: row.get(1)?,
                mtime_ns: row.get(2)?,
                size_bytes: row.get(3)?,
                language: row.get(4)?,
                line_count: row.get(5)?,
                pagerank: row.get(6)?,
                indexed_at: row.get(7)?,
                change_count: row.get(9)?,
            },
            row.get::<_, i64>(8)?,
        ))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

pub fn get_edge_count(conn: &Connection) -> Result<i64> {
    let count = conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
    Ok(count)
}

pub fn search_file_ids_by_fts(conn: &Connection, query: &str) -> Result<Vec<i64>> {
    let safe = sanitize_fts_query(query);
    let mut stmt = conn.prepare(
        "SELECT DISTINCT s.file_id
         FROM symbols_fts fts
         JOIN symbols s ON s.id = fts.rowid
         WHERE symbols_fts MATCH ?1",
    )?;
    let rows = stmt.query_map([&safe], |row| row.get::<_, i64>(0))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(ids)
}

/// Returns the distinct file paths whose symbol bodies contain `term`,
/// via `symbols_body_fts`. The unicode61 tokenizer treats `_` as a word
/// separator, so identifier-style queries like `find_symbol_by_name` are
/// split into adjacent tokens and matched across all four terms — broad
/// enough to catch every legitimate caller, narrow enough that the AST
/// walk the caller runs afterwards filters out false positives cheaply.
///
/// Used by `qartez_refs` and `qartez_rename` as a fallback scan set when the
/// edge-graph based importer list is incomplete (external-crate `use`
/// statements, module-form imports whose resolver points at `mod.rs`, or
/// child modules that only inherit via `use super::*;`).
pub fn find_file_paths_by_body_text(conn: &Connection, term: &str) -> Result<Vec<String>> {
    let safe = sanitize_fts_query(term);
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.path
         FROM symbols_body_fts bfts
         JOIN symbols s ON s.id = bfts.rowid
         JOIN files f ON f.id = s.file_id
         WHERE symbols_body_fts MATCH ?1",
    )?;
    let rows = stmt.query_map([&safe], |row| row.get::<_, String>(0))?;
    let mut paths = Vec::new();
    for row in rows {
        paths.push(row?);
    }
    Ok(paths)
}

pub fn get_all_file_clusters(conn: &Connection) -> Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare("SELECT file_id, cluster_id FROM file_clusters")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn get_file_clusters_count(conn: &Connection) -> Result<i64> {
    let count = conn.query_row("SELECT COUNT(*) FROM file_clusters", [], |r| r.get(0))?;
    Ok(count)
}

/// FTS5 search over pre-indexed symbol bodies. Joins back into `symbols`
/// and `files` so callers get the same result shape as `search_symbols_fts`.
/// Returns an empty vector when the body index has not been populated
/// (older DBs predating the column).
pub fn search_symbol_bodies_fts(
    conn: &Connection,
    query: &str,
    limit: i64,
) -> Result<Vec<(SymbolRow, String)>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.file_id, s.name, s.kind, s.line_start, s.line_end,
                s.signature, s.is_exported, s.shape_hash, s.parent_id, s.pagerank,
                s.complexity, s.owner_type, f.path AS f_path
         FROM symbols_body_fts bfts
         JOIN symbols s ON s.id = bfts.rowid
         JOIN files f ON f.id = s.file_id
         WHERE symbols_body_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![query, limit], |row| {
        Ok((row_to_symbol(row)?, row.get::<_, String>("f_path")?))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Return all types that implement or extend the given supertype name.
pub fn get_subtypes(
    conn: &Connection,
    super_name: &str,
) -> Result<Vec<(models::TypeHierarchyRow, FileRow)>> {
    let mut stmt = conn.prepare(
        "SELECT h.id, h.file_id, h.sub_name, h.super_name, h.kind, h.line,
                f.id AS f_id, f.path AS f_path, f.mtime_ns AS f_mtime_ns,
                f.size_bytes AS f_size_bytes, f.language AS f_language,
                f.line_count AS f_line_count, f.pagerank AS f_pagerank,
                f.indexed_at AS f_indexed_at, f.change_count AS f_change_count
         FROM type_hierarchy h
         JOIN files f ON f.id = h.file_id
         WHERE h.super_name = ?1
         ORDER BY h.sub_name",
    )?;
    let rows = stmt.query_map([super_name], |row| {
        let h = models::TypeHierarchyRow {
            id: row.get(0)?,
            file_id: row.get(1)?,
            sub_name: row.get(2)?,
            super_name: row.get(3)?,
            kind: row.get(4)?,
            line: row.get(5)?,
        };
        let f = row_to_file_joined(row)?;
        Ok((h, f))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Return all supertypes (traits/interfaces/base classes) of a given type name.
pub fn get_supertypes(
    conn: &Connection,
    sub_name: &str,
) -> Result<Vec<(models::TypeHierarchyRow, FileRow)>> {
    let mut stmt = conn.prepare(
        "SELECT h.id, h.file_id, h.sub_name, h.super_name, h.kind, h.line,
                f.id AS f_id, f.path AS f_path, f.mtime_ns AS f_mtime_ns,
                f.size_bytes AS f_size_bytes, f.language AS f_language,
                f.line_count AS f_line_count, f.pagerank AS f_pagerank,
                f.indexed_at AS f_indexed_at, f.change_count AS f_change_count
         FROM type_hierarchy h
         JOIN files f ON f.id = h.file_id
         WHERE h.sub_name = ?1
         ORDER BY h.super_name",
    )?;
    let rows = stmt.query_map([sub_name], |row| {
        let h = models::TypeHierarchyRow {
            id: row.get(0)?,
            file_id: row.get(1)?,
            sub_name: row.get(2)?,
            super_name: row.get(3)?,
            kind: row.get(4)?,
            line: row.get(5)?,
        };
        let f = row_to_file_joined(row)?;
        Ok((h, f))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// One result from brute-force semantic (vector) search: the symbol, its
/// file path, and the cosine similarity score.
#[cfg(feature = "semantic")]
pub type SemanticHit = (SymbolRow, String, f64);

/// Brute-force cosine similarity search over all pre-computed embeddings.
///
/// Loads every embedding BLOB from `symbol_embeddings`, computes the dot
/// product with `query_vec` (both are L2-normalized so this equals cosine
/// similarity), and returns the top `limit` results sorted by descending
/// score.
///
/// For 50K symbols this takes under 5 ms on a modern CPU. The alternative
/// (approximate nearest-neighbor indices) adds external dependencies and
/// complexity that is not justified until the symbol count reaches the
/// hundreds of thousands.
#[cfg(feature = "semantic")]
pub fn semantic_search(
    conn: &Connection,
    query_vec: &[f32],
    limit: i64,
) -> Result<Vec<SemanticHit>> {
    let mut stmt = conn.prepare(
        "SELECT se.rowid, se.embedding,
                s.id, s.file_id, s.name, s.kind, s.line_start, s.line_end,
                s.signature, s.is_exported, s.shape_hash, s.parent_id, s.pagerank,
                s.complexity, s.owner_type,
                f.path
         FROM symbol_embeddings se
         JOIN symbols s ON s.id = se.rowid
         JOIN files f ON f.id = s.file_id",
    )?;

    let mut scored: Vec<(SymbolRow, String, f64)> = stmt
        .query_map([], |row| {
            let blob: Vec<u8> = row.get(1)?;
            let sym = row_to_symbol_at(row, 2)?;
            let path: String = row.get(15)?;
            Ok((sym, path, blob))
        })?
        .filter_map(|r| r.ok())
        .map(|(sym, path, blob)| {
            let emb = crate::embeddings::blob_to_vec(&blob);
            let score = crate::embeddings::cosine_similarity(query_vec, &emb) as f64;
            (sym, path, score)
        })
        .collect();

    scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit as usize);
    Ok(scored)
}

/// Hybrid search: combines FTS5 body search with vector semantic search
/// via Reciprocal Rank Fusion (RRF).
///
/// Runs both search paths, assigns RRF scores, and returns the top
/// `limit` results. Items appearing in both lists rank higher than
/// items in only one.
#[cfg(feature = "semantic")]
pub fn hybrid_search(
    conn: &Connection,
    query: &str,
    query_vec: &[f32],
    limit: i64,
) -> Result<Vec<SemanticHit>> {
    // Pull more candidates from each source than the final limit so RRF
    // has enough variety to merge effectively.
    let candidate_limit = (limit * 3).min(200);

    // Arm 1: vector search.
    let vector_hits = semantic_search(conn, query_vec, candidate_limit)?;
    let vector_list: Vec<(i64, (SymbolRow, String))> = vector_hits
        .into_iter()
        .map(|(sym, path, _score)| (sym.id, (sym, path)))
        .collect();

    // Arm 2: FTS5 body search.
    let fts_query = sanitize_fts_query(query);
    let fts_hits = search_symbol_bodies_fts(conn, &fts_query, candidate_limit)?;
    let fts_list: Vec<(i64, (SymbolRow, String))> = fts_hits
        .into_iter()
        .map(|(sym, path)| (sym.id, (sym, path)))
        .collect();

    // RRF merge with k=60 (standard parameter).
    let merged = crate::embeddings::rrf_merge(&[&vector_list, &fts_list], 60.0, limit as usize);

    Ok(merged
        .into_iter()
        .map(|(_id, (sym, path), score)| (sym, path, score))
        .collect())
}

/// Read a `SymbolRow` from a query row starting at a column offset.
/// Used by `semantic_search` where the symbol columns are not at position 0.
#[cfg(feature = "semantic")]
fn row_to_symbol_at(row: &rusqlite::Row, offset: usize) -> rusqlite::Result<SymbolRow> {
    let is_exported_int: i32 = row.get(offset + 7)?;
    Ok(SymbolRow {
        id: row.get(offset)?,
        file_id: row.get(offset + 1)?,
        name: row.get(offset + 2)?,
        kind: row.get(offset + 3)?,
        line_start: row.get(offset + 4)?,
        line_end: row.get(offset + 5)?,
        signature: row.get(offset + 6)?,
        is_exported: is_exported_int != 0,
        shape_hash: row.get(offset + 8)?,
        parent_id: row.get(offset + 9)?,
        pagerank: row.get(offset + 10)?,
        complexity: row.get(offset + 11)?,
        owner_type: row.get(offset + 12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::SymbolInsert;
    use crate::storage::schema::create_schema;
    use crate::storage::write;
    use rusqlite::Connection;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        create_schema(&conn).unwrap();
        conn
    }

    fn insert_test_file(conn: &Connection, path: &str) -> i64 {
        write::upsert_file(conn, path, 1000, 100, "rust", 10).unwrap()
    }

    #[test]
    fn test_get_file_by_path() {
        let conn = setup();
        insert_test_file(&conn, "src/main.rs");

        let file = get_file_by_path(&conn, "src/main.rs").unwrap();
        assert!(file.is_some());
        assert_eq!(file.unwrap().path, "src/main.rs");

        let missing = get_file_by_path(&conn, "nonexistent.rs").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_get_all_files() {
        let conn = setup();
        insert_test_file(&conn, "src/a.rs");
        insert_test_file(&conn, "src/b.rs");

        let files = get_all_files(&conn).unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_get_files_ranked() {
        let conn = setup();
        let f1 = insert_test_file(&conn, "src/a.rs");
        let f2 = insert_test_file(&conn, "src/b.rs");
        write::update_pagerank(&conn, f1, 0.5).unwrap();
        write::update_pagerank(&conn, f2, 0.9).unwrap();

        let ranked = get_files_ranked(&conn, 10).unwrap();
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].path, "src/b.rs");
        assert_eq!(ranked[1].path, "src/a.rs");
    }

    #[test]
    fn test_get_file_by_id() {
        let conn = setup();
        let id = insert_test_file(&conn, "src/main.rs");

        let file = get_file_by_id(&conn, id).unwrap();
        assert!(file.is_some());
        assert_eq!(file.unwrap().path, "src/main.rs");
    }

    #[test]
    fn test_get_symbols_for_file() {
        let conn = setup();
        let file_id = insert_test_file(&conn, "src/lib.rs");
        write::insert_symbols(
            &conn,
            file_id,
            &[
                SymbolInsert {
                    name: "foo".to_string(),
                    kind: "function".to_string(),
                    line_start: 1,
                    line_end: 5,
                    signature: None,
                    is_exported: true,
                    shape_hash: None,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                },
                SymbolInsert {
                    name: "bar".to_string(),
                    kind: "function".to_string(),
                    line_start: 7,
                    line_end: 10,
                    signature: None,
                    is_exported: false,
                    shape_hash: None,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                },
            ],
        )
        .unwrap();

        let symbols = get_symbols_for_file(&conn, file_id).unwrap();
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].name, "foo");
        assert_eq!(symbols[1].name, "bar");
    }

    #[test]
    fn test_find_symbol_by_name() {
        let conn = setup();
        let file_id = insert_test_file(&conn, "src/lib.rs");
        write::insert_symbols(
            &conn,
            file_id,
            &[SymbolInsert {
                name: "Config".to_string(),
                kind: "struct".to_string(),
                line_start: 1,
                line_end: 10,
                signature: Some("pub struct Config".to_string()),
                is_exported: true,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            }],
        )
        .unwrap();

        let results = find_symbol_by_name(&conn, "Config").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.name, "Config");
        assert_eq!(results[0].1.path, "src/lib.rs");
    }

    #[test]
    fn test_search_symbols_fts() {
        let conn = setup();
        let file_id = insert_test_file(&conn, "src/config.rs");
        write::insert_symbols(
            &conn,
            file_id,
            &[SymbolInsert {
                name: "DatabaseConfig".to_string(),
                kind: "struct".to_string(),
                line_start: 1,
                line_end: 10,
                signature: None,
                is_exported: true,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            }],
        )
        .unwrap();
        write::sync_fts(&conn).unwrap();

        let results = search_symbols_fts(&conn, "Database*", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.name, "DatabaseConfig");
        assert_eq!(results[0].1, "src/config.rs");
    }

    #[test]
    fn test_get_edges_from_and_to() {
        let conn = setup();
        let f1 = insert_test_file(&conn, "src/a.rs");
        let f2 = insert_test_file(&conn, "src/b.rs");
        write::insert_edge(&conn, f1, f2, "import", Some("crate::b")).unwrap();

        let from_edges = get_edges_from(&conn, f1).unwrap();
        assert_eq!(from_edges.len(), 1);
        assert_eq!(from_edges[0].to_file, f2);

        let to_edges = get_edges_to(&conn, f2).unwrap();
        assert_eq!(to_edges.len(), 1);
        assert_eq!(to_edges[0].from_file, f1);
    }

    #[test]
    fn test_get_all_edges() {
        let conn = setup();
        let f1 = insert_test_file(&conn, "src/a.rs");
        let f2 = insert_test_file(&conn, "src/b.rs");
        let f3 = insert_test_file(&conn, "src/c.rs");
        write::insert_edge(&conn, f1, f2, "import", None).unwrap();
        write::insert_edge(&conn, f2, f3, "import", None).unwrap();

        let edges = get_all_edges(&conn).unwrap();
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn test_get_cochanges() {
        let conn = setup();
        let f1 = insert_test_file(&conn, "src/a.rs");
        let f2 = insert_test_file(&conn, "src/b.rs");
        write::upsert_cochange(&conn, f1, f2).unwrap();
        write::upsert_cochange(&conn, f1, f2).unwrap();

        let cochanges = get_cochanges(&conn, f1, 10).unwrap();
        assert_eq!(cochanges.len(), 1);
        assert_eq!(cochanges[0].0.count, 2);
        assert_eq!(cochanges[0].1.path, "src/b.rs");
    }

    #[test]
    fn test_get_exported_symbols_not_imported() {
        let conn = setup();
        let f1 = insert_test_file(&conn, "src/a.rs");
        let f2 = insert_test_file(&conn, "src/b.rs");

        write::insert_symbols(
            &conn,
            f1,
            &[SymbolInsert {
                name: "used_fn".to_string(),
                kind: "function".to_string(),
                line_start: 1,
                line_end: 5,
                signature: None,
                is_exported: true,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            }],
        )
        .unwrap();
        write::insert_symbols(
            &conn,
            f2,
            &[SymbolInsert {
                name: "unused_fn".to_string(),
                kind: "function".to_string(),
                line_start: 1,
                line_end: 5,
                signature: None,
                is_exported: true,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            }],
        )
        .unwrap();

        let f3 = insert_test_file(&conn, "src/c.rs");
        write::insert_edge(&conn, f3, f1, "import", None).unwrap();

        let dead = get_exported_symbols_not_imported(&conn).unwrap();
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].0.name, "unused_fn");
    }

    #[test]
    fn test_get_meta() {
        let conn = setup();
        write::set_meta(&conn, "version", "42").unwrap();

        let val = get_meta(&conn, "version").unwrap();
        assert_eq!(val, Some("42".to_string()));

        let missing = get_meta(&conn, "nonexistent").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_get_stale_files() {
        let conn = setup();
        let f1 = insert_test_file(&conn, "src/indexed.rs");
        insert_test_file(&conn, "src/stale.rs");

        write::insert_symbols(
            &conn,
            f1,
            &[SymbolInsert {
                name: "main".to_string(),
                kind: "function".to_string(),
                line_start: 1,
                line_end: 5,
                signature: None,
                is_exported: false,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            }],
        )
        .unwrap();

        let stale = get_stale_files(&conn).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].path, "src/stale.rs");
    }

    #[test]
    fn test_get_file_count_and_symbol_count() {
        let conn = setup();
        assert_eq!(get_file_count(&conn).unwrap(), 0);
        assert_eq!(get_symbol_count(&conn).unwrap(), 0);

        let f1 = insert_test_file(&conn, "src/a.rs");
        assert_eq!(get_file_count(&conn).unwrap(), 1);

        write::insert_symbols(
            &conn,
            f1,
            &[
                SymbolInsert {
                    name: "a".to_string(),
                    kind: "function".to_string(),
                    line_start: 1,
                    line_end: 5,
                    signature: None,
                    is_exported: false,
                    shape_hash: None,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                },
                SymbolInsert {
                    name: "b".to_string(),
                    kind: "function".to_string(),
                    line_start: 7,
                    line_end: 10,
                    signature: None,
                    is_exported: false,
                    shape_hash: None,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                },
            ],
        )
        .unwrap();
        assert_eq!(get_symbol_count(&conn).unwrap(), 2);
    }
}
