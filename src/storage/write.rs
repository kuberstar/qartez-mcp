use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;
use crate::storage::models::SymbolInsert;
use crate::storage::read::get_file_by_path;

/// Maximum lines stored per symbol body in `symbols_body_fts`. Caps FTS
/// storage at a bounded size — very large functions are truncated.
const MAX_BODY_LINES: usize = 500;

pub fn upsert_file(
    conn: &Connection,
    path: &str,
    mtime_ns: i64,
    size_bytes: i64,
    language: &str,
    line_count: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO files (path, mtime_ns, size_bytes, language, line_count, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, strftime('%s', 'now'))
         ON CONFLICT(path) DO UPDATE SET
             mtime_ns = excluded.mtime_ns,
             size_bytes = excluded.size_bytes,
             language = excluded.language,
             line_count = excluded.line_count,
             indexed_at = excluded.indexed_at",
        rusqlite::params![path, mtime_ns, size_bytes, language, line_count],
    )?;
    let id = conn.query_row(
        "SELECT id FROM files WHERE path = ?1",
        rusqlite::params![path],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Returns the file ID for the given path, creating a minimal record if it doesn't exist.
pub fn get_or_create_file(conn: &Connection, path: &str) -> Result<i64> {
    if let Some(f) = get_file_by_path(conn, path)? {
        return Ok(f.id);
    }
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let lang = crate::index::languages::get_language_for_ext(ext)
        .map(|s| s.language_name())
        .unwrap_or(ext);
    upsert_file(conn, path, 0, 0, lang, 0)
}

/// Insert a batch of symbols and return their new rowids in the same order
/// as the input. Callers that need the ids — for example, the reference
/// resolution pass in `full_index` — use these to translate the
/// parse-local `from_symbol_idx` into real DB ids. Older callers that only
/// care about the side effect can simply ignore the returned `Vec` via `?`.
pub fn insert_symbols(
    conn: &Connection,
    file_id: i64,
    symbols: &[SymbolInsert],
) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "INSERT INTO symbols (file_id, name, kind, line_start, line_end, signature, is_exported, shape_hash, parent_id, unused_excluded, complexity)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    // Resolve parent_idx -> real rowid on the fly: inserts happen in input
    // order, so a child's parent (lower index) is always written first.
    let mut idx_to_id: Vec<i64> = Vec::with_capacity(symbols.len());
    for sym in symbols {
        let parent_id: Option<i64> = sym.parent_idx.and_then(|i| idx_to_id.get(i).copied());
        stmt.execute(rusqlite::params![
            file_id,
            sym.name,
            sym.kind,
            sym.line_start,
            sym.line_end,
            sym.signature,
            sym.is_exported as i32,
            sym.shape_hash,
            parent_id,
            sym.unused_excluded as i32,
            sym.complexity,
        ])?;
        idx_to_id.push(conn.last_insert_rowid());
    }
    Ok(idx_to_id)
}

pub fn insert_edge(
    conn: &Connection,
    from_file: i64,
    to_file: i64,
    kind: &str,
    specifier: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO edges (from_file, to_file, kind, specifier)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![from_file, to_file, kind, specifier],
    )?;
    Ok(())
}

pub fn delete_file_data(conn: &Connection, file_id: i64) -> Result<()> {
    // FTS tables are standalone (not content-linked to `symbols`), so the
    // ON DELETE CASCADE from files→symbols does not touch them. Clean up
    // FTS entries before the cascade deletes the symbol rows we need to
    // identify which FTS rowids to remove.
    conn.execute(
        "DELETE FROM symbols_fts WHERE rowid IN (SELECT id FROM symbols WHERE file_id = ?1)",
        [file_id],
    )?;
    conn.execute(
        "DELETE FROM symbols_body_fts WHERE rowid IN (SELECT id FROM symbols WHERE file_id = ?1)",
        [file_id],
    )?;
    conn.execute("DELETE FROM files WHERE id = ?1", [file_id])?;
    Ok(())
}

/// Clear a file's derived content (symbols, outgoing edges, FTS entries,
/// unused-export markers) without deleting the file row itself. This
/// preserves the `file_id` and incoming edges from other files, which is
/// critical for incremental re-indexing: only the file that changed needs
/// its content refreshed while the rest of the dependency graph stays
/// intact.
pub fn clear_file_content(conn: &Connection, file_id: i64) -> Result<()> {
    // FTS tables are standalone — clean up before the symbol rows vanish.
    conn.execute(
        "DELETE FROM symbols_fts WHERE rowid IN (SELECT id FROM symbols WHERE file_id = ?1)",
        [file_id],
    )?;
    conn.execute(
        "DELETE FROM symbols_body_fts WHERE rowid IN (SELECT id FROM symbols WHERE file_id = ?1)",
        [file_id],
    )?;
    conn.execute("DELETE FROM unused_exports WHERE file_id = ?1", [file_id])?;
    // symbol_refs cascade from symbols(id) ON DELETE CASCADE.
    conn.execute("DELETE FROM symbols WHERE file_id = ?1", [file_id])?;
    // Only outgoing edges — incoming edges stay so other files' import
    // relationships remain valid.
    conn.execute("DELETE FROM edges WHERE from_file = ?1", [file_id])?;
    Ok(())
}

pub fn upsert_cochange(conn: &Connection, file_a: i64, file_b: i64) -> Result<()> {
    upsert_cochange_n(conn, file_a, file_b, 1)
}

pub fn upsert_cochange_n(conn: &Connection, file_a: i64, file_b: i64, n: u32) -> Result<()> {
    let (lo, hi) = if file_a <= file_b {
        (file_a, file_b)
    } else {
        (file_b, file_a)
    };
    conn.execute(
        "INSERT INTO co_changes (file_a, file_b, count)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(file_a, file_b) DO UPDATE SET count = count + ?3",
        rusqlite::params![lo, hi, n],
    )?;
    Ok(())
}

pub fn update_pagerank(conn: &Connection, file_id: i64, rank: f64) -> Result<()> {
    conn.execute(
        "UPDATE files SET pagerank = ?1 WHERE id = ?2",
        rusqlite::params![rank, file_id],
    )?;
    Ok(())
}

/// Bulk-insert resolved symbol→symbol edges. Callers pre-batch the full
/// (from, to, kind) tuples for the whole indexing pass and hand them to a
/// single prepared statement so we avoid the SQL compile cost per row.
/// Duplicate `(from, to, kind)` triples are silently ignored via
/// `INSERT OR IGNORE`, matching the table-level `UNIQUE` constraint.
pub fn insert_symbol_refs(conn: &Connection, refs: &[(i64, i64, &str)]) -> Result<()> {
    if refs.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO symbol_refs (from_symbol_id, to_symbol_id, kind)
         VALUES (?1, ?2, ?3)",
    )?;
    for &(from, to, kind) in refs {
        stmt.execute(rusqlite::params![from, to, kind])?;
    }
    Ok(())
}

/// Write the symbol-level PageRank computed from the `symbol_refs` graph.
/// Called by `compute_symbol_pagerank` once per symbol inside the same
/// transaction that file-level ranks are written, so readers never see a
/// half-rebuilt rank set.
pub fn update_symbol_pagerank(conn: &Connection, symbol_id: i64, rank: f64) -> Result<()> {
    conn.execute(
        "UPDATE symbols SET pagerank = ?1 WHERE id = ?2",
        rusqlite::params![rank, symbol_id],
    )?;
    Ok(())
}

pub fn sync_fts(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM symbols_fts;
         INSERT INTO symbols_fts (rowid, name, kind, file_path)
         SELECT s.id, s.name, s.kind, f.path
         FROM symbols s
         JOIN files f ON s.file_id = f.id",
    )?;
    Ok(())
}

/// Wipe and rebuild the symbol-body FTS table by pulling bodies from
/// disk for every symbol currently in `symbols`. Called at indexing time
/// so that `qartez_grep search_bodies=true` queries a pre-tokenized index
/// instead of streaming files at query time. Bounded by the per-symbol
/// line range, so the extra storage is ~1-2× the codebase size.
pub fn rebuild_symbol_bodies(conn: &Connection, project_root: &std::path::Path) -> Result<()> {
    conn.execute("DELETE FROM symbols_body_fts", [])?;

    let mut select = conn.prepare(
        "SELECT s.id, s.line_start, s.line_end, f.path
         FROM symbols s
         JOIN files f ON s.file_id = f.id",
    )?;
    let rows: Vec<(i64, u32, u32, String)> = select
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(select);

    // Group by file so we read each source file once, not once per symbol.
    let mut by_path: std::collections::HashMap<String, Vec<(i64, u32, u32)>> =
        std::collections::HashMap::new();
    for (id, s, e, p) in rows {
        by_path.entry(p).or_default().push((id, s, e));
    }

    let mut insert = conn.prepare("INSERT INTO symbols_body_fts (rowid, body) VALUES (?1, ?2)")?;
    for (rel_path, syms) in by_path {
        let abs = project_root.join(&rel_path);
        let Ok(text) = std::fs::read_to_string(&abs) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for (id, line_start, line_end) in syms {
            let start = (line_start as usize).saturating_sub(1);
            let end = (line_end as usize)
                .min(lines.len())
                .min(start + MAX_BODY_LINES);
            if start >= lines.len() || start >= end {
                continue;
            }
            let body = lines[start..end].join("\n");
            insert.execute(rusqlite::params![id, body])?;
        }
    }
    Ok(())
}

/// Insert `symbols_fts` entries for all symbols belonging to `file_id`.
/// Called after `insert_symbols` during an incremental re-index so that only
/// the affected file's entries are (re-)written rather than the whole table.
/// `clear_file_content` must have already removed the old FTS rows for this
/// file before this function is called.
pub fn insert_fts_for_file(conn: &Connection, file_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO symbols_fts (rowid, name, kind, file_path)
         SELECT s.id, s.name, s.kind, f.path
         FROM symbols s
         JOIN files f ON s.file_id = f.id
         WHERE s.file_id = ?1",
        [file_id],
    )?;
    Ok(())
}

/// Insert `symbols_body_fts` entries for the symbols in a single file.
/// Called during incremental re-indexing instead of `rebuild_symbol_bodies`
/// (which rebuilds every file) to avoid unbounded WAL growth on large
/// codebases. `clear_file_content` must have already removed the old body
/// FTS rows for this file before this function is called.
pub fn rebuild_symbol_bodies_for_file(
    conn: &Connection,
    project_root: &Path,
    file_id: i64,
    rel_path: &str,
) -> Result<()> {
    let abs = project_root.join(rel_path);
    let Ok(text) = std::fs::read_to_string(&abs) else {
        return Ok(());
    };
    let lines: Vec<&str> = text.lines().collect();

    let mut select =
        conn.prepare("SELECT id, line_start, line_end FROM symbols WHERE file_id = ?1")?;
    let syms: Vec<(i64, u32, u32)> = select
        .query_map([file_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(select);

    let mut insert =
        conn.prepare("INSERT INTO symbols_body_fts (rowid, body) VALUES (?1, ?2)")?;
    for (id, line_start, line_end) in syms {
        let start = (line_start as usize).saturating_sub(1);
        let end = (line_end as usize)
            .min(lines.len())
            .min(start + MAX_BODY_LINES);
        if start >= lines.len() || start >= end {
            continue;
        }
        let body = lines[start..end].join("\n");
        insert.execute(rusqlite::params![id, body])?;
    }
    Ok(())
}

/// Rebuild the `unused_exports` materialized table from the current
/// `symbols` / `edges` state. Must be called at the tail of the indexing
/// pipeline — after `full_index` has written every file's imports and the
/// edge graph is settled. Query-time `qartez_unused` then becomes a single
/// JOIN-plus-LIMIT instead of re-walking tree-sitter ASTs for every call.
pub fn populate_unused_exports(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM unused_exports;
         INSERT INTO unused_exports (symbol_id, file_id)
         SELECT s.id, s.file_id
         FROM symbols s
         WHERE s.is_exported = 1
           AND COALESCE(s.unused_excluded, 0) = 0
           AND NOT EXISTS (SELECT 1 FROM edges e WHERE e.to_file = s.file_id)
           AND NOT EXISTS (SELECT 1 FROM symbol_refs sr WHERE sr.to_symbol_id = s.id)",
    )?;
    Ok(())
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

pub fn clear_file_clusters(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM file_clusters", [])?;
    Ok(())
}

pub fn upsert_file_cluster(
    conn: &Connection,
    file_id: i64,
    cluster_id: i64,
    computed_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO file_clusters (file_id, cluster_id, computed_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(file_id) DO UPDATE SET
             cluster_id = excluded.cluster_id,
             computed_at = excluded.computed_at",
        rusqlite::params![file_id, cluster_id, computed_at],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::create_schema;
    use rusqlite::Connection;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        create_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn test_upsert_file_insert() {
        let conn = setup();
        let id = upsert_file(&conn, "src/main.rs", 1000, 500, "rust", 42).unwrap();
        assert!(id > 0);

        let path: String = conn
            .query_row("SELECT path FROM files WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(path, "src/main.rs");
    }

    #[test]
    fn test_upsert_file_update() {
        let conn = setup();
        let id1 = upsert_file(&conn, "src/main.rs", 1000, 500, "rust", 42).unwrap();
        let id2 = upsert_file(&conn, "src/main.rs", 2000, 600, "rust", 50).unwrap();
        assert_eq!(id1, id2);

        let size: i64 = conn
            .query_row("SELECT size_bytes FROM files WHERE id = ?1", [id1], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(size, 600);
    }

    #[test]
    fn test_insert_symbols() {
        let conn = setup();
        let file_id = upsert_file(&conn, "src/lib.rs", 1000, 200, "rust", 20).unwrap();

        let symbols = vec![
            SymbolInsert {
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
            },
            SymbolInsert {
                name: "new".to_string(),
                kind: "function".to_string(),
                line_start: 12,
                line_end: 20,
                signature: Some("pub fn new() -> Self".to_string()),
                is_exported: true,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
            },
        ];
        insert_symbols(&conn, file_id, &symbols).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM symbols WHERE file_id = ?1",
                [file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_insert_edge() {
        let conn = setup();
        let f1 = upsert_file(&conn, "src/a.rs", 1000, 100, "rust", 10).unwrap();
        let f2 = upsert_file(&conn, "src/b.rs", 1000, 100, "rust", 10).unwrap();
        insert_edge(&conn, f1, f2, "import", Some("crate::b")).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_insert_edge_duplicate_ignored() {
        let conn = setup();
        let f1 = upsert_file(&conn, "src/a.rs", 1000, 100, "rust", 10).unwrap();
        let f2 = upsert_file(&conn, "src/b.rs", 1000, 100, "rust", 10).unwrap();
        insert_edge(&conn, f1, f2, "import", Some("crate::b")).unwrap();
        insert_edge(&conn, f1, f2, "import", Some("crate::b")).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_delete_file_data_cascades() {
        let conn = setup();
        let f1 = upsert_file(&conn, "src/a.rs", 1000, 100, "rust", 10).unwrap();
        let f2 = upsert_file(&conn, "src/b.rs", 1000, 100, "rust", 10).unwrap();

        let symbols = vec![SymbolInsert {
            name: "foo".to_string(),
            kind: "function".to_string(),
            line_start: 1,
            line_end: 5,
            signature: None,
            is_exported: false,
            shape_hash: None,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
        }];
        insert_symbols(&conn, f1, &symbols).unwrap();
        insert_edge(&conn, f1, f2, "import", None).unwrap();

        delete_file_data(&conn, f1).unwrap();

        let sym_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        let edge_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sym_count, 0);
        assert_eq!(edge_count, 0);
    }

    #[test]
    fn test_upsert_cochange() {
        let conn = setup();
        let f1 = upsert_file(&conn, "src/a.rs", 1000, 100, "rust", 10).unwrap();
        let f2 = upsert_file(&conn, "src/b.rs", 1000, 100, "rust", 10).unwrap();

        upsert_cochange(&conn, f1, f2).unwrap();
        upsert_cochange(&conn, f1, f2).unwrap();
        upsert_cochange(&conn, f1, f2).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT count FROM co_changes WHERE file_a = ?1 AND file_b = ?2",
                rusqlite::params![f1, f2],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_update_pagerank() {
        let conn = setup();
        let id = upsert_file(&conn, "src/main.rs", 1000, 500, "rust", 42).unwrap();

        update_pagerank(&conn, id, 0.85).unwrap();

        let rank: f64 = conn
            .query_row("SELECT pagerank FROM files WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert!((rank - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn test_sync_fts() {
        let conn = setup();
        let file_id = upsert_file(&conn, "src/lib.rs", 1000, 200, "rust", 20).unwrap();

        let symbols = vec![SymbolInsert {
            name: "MyStruct".to_string(),
            kind: "struct".to_string(),
            line_start: 1,
            line_end: 5,
            signature: None,
            is_exported: true,
            shape_hash: None,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
        }];
        insert_symbols(&conn, file_id, &symbols).unwrap();
        sync_fts(&conn).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_insert_symbol_refs_round_trip() {
        let conn = setup();
        let file_id = upsert_file(&conn, "src/lib.rs", 0, 0, "rust", 0).unwrap();
        insert_symbols(
            &conn,
            file_id,
            &[
                SymbolInsert {
                    name: "caller".to_string(),
                    kind: "function".to_string(),
                    line_start: 1,
                    line_end: 5,
                    signature: None,
                    is_exported: false,
                    shape_hash: None,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                },
                SymbolInsert {
                    name: "callee".to_string(),
                    kind: "function".to_string(),
                    line_start: 7,
                    line_end: 10,
                    signature: None,
                    is_exported: false,
                    shape_hash: None,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                },
            ],
        )
        .unwrap();
        let caller: i64 = conn
            .query_row("SELECT id FROM symbols WHERE name = 'caller'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let callee: i64 = conn
            .query_row("SELECT id FROM symbols WHERE name = 'callee'", [], |r| {
                r.get(0)
            })
            .unwrap();

        // First insert succeeds; duplicate `(from, to, kind)` is silently ignored.
        insert_symbol_refs(&conn, &[(caller, callee, "call"), (caller, callee, "call")]).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbol_refs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Different `kind` on the same pair is a separate edge.
        insert_symbol_refs(&conn, &[(caller, callee, "type")]).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbol_refs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_update_symbol_pagerank_persists_rank() {
        let conn = setup();
        let file_id = upsert_file(&conn, "src/lib.rs", 0, 0, "rust", 0).unwrap();
        insert_symbols(
            &conn,
            file_id,
            &[SymbolInsert {
                name: "f".to_string(),
                kind: "function".to_string(),
                line_start: 1,
                line_end: 2,
                signature: None,
                is_exported: true,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
            }],
        )
        .unwrap();
        let sym_id: i64 = conn
            .query_row("SELECT id FROM symbols WHERE name = 'f'", [], |r| r.get(0))
            .unwrap();

        update_symbol_pagerank(&conn, sym_id, 0.42).unwrap();
        let pr: f64 = conn
            .query_row(
                "SELECT pagerank FROM symbols WHERE id = ?1",
                [sym_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!((pr - 0.42).abs() < f64::EPSILON);
    }

    #[test]
    fn test_insert_symbol_ref_cascades_on_symbol_delete() {
        let conn = setup();
        let file_id = upsert_file(&conn, "src/lib.rs", 0, 0, "rust", 0).unwrap();
        insert_symbols(
            &conn,
            file_id,
            &[
                SymbolInsert {
                    name: "a".to_string(),
                    kind: "function".to_string(),
                    line_start: 1,
                    line_end: 2,
                    signature: None,
                    is_exported: false,
                    shape_hash: None,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                },
                SymbolInsert {
                    name: "b".to_string(),
                    kind: "function".to_string(),
                    line_start: 3,
                    line_end: 4,
                    signature: None,
                    is_exported: false,
                    shape_hash: None,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                },
            ],
        )
        .unwrap();
        let a: i64 = conn
            .query_row("SELECT id FROM symbols WHERE name = 'a'", [], |r| r.get(0))
            .unwrap();
        let b: i64 = conn
            .query_row("SELECT id FROM symbols WHERE name = 'b'", [], |r| r.get(0))
            .unwrap();

        insert_symbol_refs(&conn, &[(a, b, "call")]).unwrap();
        // Dropping the file cascades to symbols, which cascades to symbol_refs.
        delete_file_data(&conn, file_id).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbol_refs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_set_meta() {
        let conn = setup();
        set_meta(&conn, "version", "1").unwrap();

        let val: String = conn
            .query_row("SELECT value FROM meta WHERE key = 'version'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(val, "1");

        set_meta(&conn, "version", "2").unwrap();
        let val: String = conn
            .query_row("SELECT value FROM meta WHERE key = 'version'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(val, "2");
    }

    #[test]
    fn test_clear_file_content_preserves_file_row() {
        let conn = setup();
        let file_id = upsert_file(&conn, "src/lib.rs", 1000, 200, "rust", 20).unwrap();
        let symbols = vec![SymbolInsert {
            name: "Foo".to_string(),
            kind: "struct".to_string(),
            line_start: 1,
            line_end: 5,
            signature: None,
            is_exported: true,
            shape_hash: None,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
        }];
        insert_symbols(&conn, file_id, &symbols).unwrap();
        insert_edge(&conn, file_id, file_id, "import", None).unwrap();

        clear_file_content(&conn, file_id).unwrap();

        // File row must still exist.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files WHERE id = ?1", [file_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "file row must survive clear_file_content");

        // Symbols must be gone.
        let sym_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM symbols WHERE file_id = ?1",
                [file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sym_count, 0);

        // Outgoing edges must be gone.
        let edge_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE from_file = ?1",
                [file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(edge_count, 0);
    }

    #[test]
    fn test_clear_file_content_preserves_incoming_edges() {
        let conn = setup();
        let a = upsert_file(&conn, "src/a.rs", 1000, 100, "rust", 10).unwrap();
        let b = upsert_file(&conn, "src/b.rs", 1000, 100, "rust", 10).unwrap();
        // a imports b
        insert_edge(&conn, a, b, "import", None).unwrap();

        // Clear b's content — the a→b edge must survive.
        clear_file_content(&conn, b).unwrap();

        let edge_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE from_file = ?1 AND to_file = ?2",
                [a, b],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(edge_count, 1, "incoming edge a→b must survive");
    }
}
