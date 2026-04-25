// Rust guideline compliant 2026-04-25

//! Database maintenance helpers exposed to operators via the
//! `qartez_maintenance` MCP tool and used internally by the MCP
//! background indexer to keep the index DB compact without stalling the
//! `initialize`/`tools/list` critical path.
//!
//! Every helper is best-effort: failures are logged but do not abort the
//! caller. Multi-gigabyte SQLite files have been observed in the wild
//! when `auto_vacuum=NONE` was set on the original CREATE statement, so
//! routines that mutate page layout (`vacuum`, `vacuum_incremental`)
//! must be triggered explicitly. They are never run automatically on
//! startup.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

/// Snapshot of disk-level metrics for a `.qartez/index.db` file.
///
/// Returned by [`stats`] and surfaced through the `qartez_maintenance`
/// tool so operators can spot DB bloat without opening the file by
/// hand.
#[derive(Debug, Clone)]
pub struct IndexStats {
    pub db_path: String,
    pub db_bytes: u64,
    pub wal_bytes: u64,
    pub shm_bytes: u64,
    pub page_size: i64,
    pub page_count: i64,
    pub freelist_count: i64,
    pub auto_vacuum: i64,
    pub journal_mode: String,
    pub top_tables: Vec<TableStat>,
    pub fingerprint: Option<String>,
    pub last_full_reindex: Option<i64>,
    pub last_index: Option<i64>,
    /// Coverage gaps in derived tables. A non-zero
    /// `files_with_zero_pagerank` or `files_missing_body_fts` means
    /// hotspots, blast radius, or body-search results are degraded
    /// until a full reindex (or the appropriate rebuild pass) runs.
    pub derived_gaps: DerivedTableGaps,
}

/// One row in [`IndexStats::top_tables`].
#[derive(Debug, Clone)]
pub struct TableStat {
    pub name: String,
    pub row_count: i64,
}

/// Collect a maintenance-time snapshot of the database.
///
/// Reads filesystem sizes for `index.db`, `index.db-wal`, `index.db-shm`,
/// the standard SQLite pragmas, and `COUNT(*)` for the largest tables
/// known to dominate disk usage. The query list is fixed to avoid
/// building a `pragma_table_list` traversal on every call.
pub fn stats(conn: &Connection, db_path: &Path) -> Result<IndexStats> {
    let db_bytes = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    let wal_bytes = std::fs::metadata(wal_path(db_path))
        .map(|m| m.len())
        .unwrap_or(0);
    let shm_bytes = std::fs::metadata(shm_path(db_path))
        .map(|m| m.len())
        .unwrap_or(0);

    let page_size = pragma_i64(conn, "page_size");
    let page_count = pragma_i64(conn, "page_count");
    let freelist_count = pragma_i64(conn, "freelist_count");
    let auto_vacuum = pragma_i64(conn, "auto_vacuum");
    let journal_mode = pragma_string(conn, "journal_mode");

    let top_tables = collect_top_tables(conn);

    let fingerprint = crate::storage::read::get_meta(
        conn,
        crate::index::fingerprint::META_KEY_WORKSPACE_FINGERPRINT,
    )
    .unwrap_or(None);
    let last_full_reindex =
        crate::storage::read::get_meta(conn, crate::index::fingerprint::META_KEY_LAST_FULL_REINDEX)
            .unwrap_or(None)
            .and_then(|s| s.parse::<i64>().ok());
    let last_index = crate::storage::read::get_meta(conn, "last_index")
        .unwrap_or(None)
        .and_then(|s| s.parse::<i64>().ok());

    let derived_gaps = collect_derived_table_gaps(conn);

    Ok(IndexStats {
        db_path: db_path.display().to_string(),
        db_bytes,
        wal_bytes,
        shm_bytes,
        page_size,
        page_count,
        freelist_count,
        auto_vacuum,
        journal_mode,
        top_tables,
        fingerprint,
        last_full_reindex,
        last_index,
        derived_gaps,
    })
}

/// Tables whose row counts dominate `.qartez/index.db` and should be
/// reported by `stats`. Hand-curated rather than enumerated from
/// `sqlite_master` so we get a stable ordering and avoid surfacing
/// internal FTS shadow tables (`*_data`, `*_idx`, `*_docsize`,
/// `*_config`) which only confuse callers.
const REPORTED_TABLES: &[&str] = &[
    "files",
    "symbols",
    "symbol_refs",
    "edges",
    "co_changes",
    "symbols_fts",
    "symbols_body_fts",
    "unused_exports",
    "type_hierarchy",
    "file_clusters",
];

fn collect_top_tables(conn: &Connection) -> Vec<TableStat> {
    let mut out = Vec::with_capacity(REPORTED_TABLES.len());
    for name in REPORTED_TABLES {
        // Wrap the name in double quotes so a table-not-found error is
        // localised to the entry rather than aborting the whole sweep.
        let sql = format!("SELECT COUNT(*) FROM \"{name}\"");
        let count: i64 = conn.query_row(&sql, [], |r| r.get(0)).unwrap_or(0);
        out.push(TableStat {
            name: (*name).to_string(),
            row_count: count,
        });
    }
    out.sort_by(|a, b| b.row_count.cmp(&a.row_count));
    out
}

/// Run `PRAGMA wal_checkpoint(TRUNCATE)`.
///
/// Returns the result tuple from SQLite as `(busy, log_pages, checkpointed)`
/// so callers can surface "still busy" to the user.
pub fn checkpoint_truncate(conn: &Connection) -> Result<(i64, i64, i64)> {
    let row: (i64, i64, i64) = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    Ok(row)
}

/// Run `PRAGMA incremental_vacuum` and report freed pages.
///
/// Only meaningful when `auto_vacuum=INCREMENTAL`. On `auto_vacuum=NONE`
/// the pragma is a no-op (SQLite still accepts the statement); the
/// caller can detect that by reading `auto_vacuum` from [`stats`].
pub fn vacuum_incremental(conn: &Connection) -> Result<i64> {
    let before = pragma_i64(conn, "freelist_count");
    conn.execute_batch("PRAGMA incremental_vacuum;")?;
    let after = pragma_i64(conn, "freelist_count");
    Ok((before - after).max(0))
}

/// Run a full `VACUUM`.
///
/// This rewrites the entire database file. On a multi-gigabyte index
/// the operation can take minutes and require ~2x free disk space, so
/// it is never invoked automatically; the maintenance tool surfaces it
/// only when the operator asks for it explicitly.
pub fn vacuum_full(conn: &Connection) -> Result<()> {
    conn.execute_batch("VACUUM;")?;
    Ok(())
}

/// Outcome of [`convert_to_incremental_auto_vacuum`].
///
/// Distinguishes "already INCREMENTAL, did nothing" from "ran a full
/// VACUUM". The maintenance tool surfaces the former with a one-line
/// info message instead of the multi-GiB rewrite the action description
/// implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertIncrementalOutcome {
    /// Database was already configured for INCREMENTAL auto-vacuum;
    /// no rewrite occurred.
    AlreadyConfigured,
    /// Pragma was set and a full `VACUUM` was performed.
    Converted,
}

/// Convert an existing database to `auto_vacuum=INCREMENTAL`.
///
/// SQLite only honors a change in auto-vacuum mode after a full
/// `VACUUM`, so this helper sets the pragma and runs the rewrite in a
/// single call. Same caveats as [`vacuum_full`] regarding runtime and
/// disk space; callers should only use this when they have explicitly
/// chosen to compact a bloated DB.
///
/// Idempotent: when `PRAGMA auto_vacuum` already reports the
/// INCREMENTAL value (2) the helper returns
/// [`ConvertIncrementalOutcome::AlreadyConfigured`] without touching
/// the database. This protects callers from triggering a second
/// multi-gigabyte VACUUM by accident on a DB that has already been
/// migrated.
pub fn convert_to_incremental_auto_vacuum(conn: &Connection) -> Result<ConvertIncrementalOutcome> {
    // SQLite encodes `auto_vacuum`: 0 = NONE, 1 = FULL, 2 = INCREMENTAL.
    // Skip the rewrite only when we are already at INCREMENTAL; on FULL
    // the operator still wants a downgrade to INCREMENTAL plus the
    // accompanying VACUUM, so we let that path run.
    const AUTO_VACUUM_INCREMENTAL: i64 = 2;
    if pragma_i64(conn, "auto_vacuum") == AUTO_VACUUM_INCREMENTAL {
        return Ok(ConvertIncrementalOutcome::AlreadyConfigured);
    }
    conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL; VACUUM;")?;
    Ok(ConvertIncrementalOutcome::Converted)
}

/// Trigger the FTS5 segment-merge optimization on `symbols_body_fts`.
///
/// FTS5 indexes accumulate small segments after every batch INSERT.
/// `INSERT INTO <fts>(<fts>) VALUES('optimize')` instructs SQLite to
/// merge them into one large segment, which both shrinks the on-disk
/// footprint and speeds up future queries. The companion `symbols_fts`
/// table (kind/name/file_path) is much smaller in practice but we
/// optimize it too for consistency.
pub fn optimize_fts(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "INSERT INTO symbols_body_fts(symbols_body_fts) VALUES('optimize');
         INSERT INTO symbols_fts(symbols_fts) VALUES('optimize');",
    )?;
    Ok(())
}

/// Drop every file row whose root prefix is not claimed by any live
/// root.
///
/// `live_prefixes` should be the result of
/// [`crate::index::fingerprint::live_root_prefixes`] for the current
/// configuration. Returns the count of removed file rows; cascading
/// deletes drop the matching symbols, FTS rows, edges and refs.
///
/// Empty-string semantics: when `live_prefixes` contains the empty
/// string the caller is signalling "an unaliased primary root is
/// present". In that case any file whose first path segment is NOT
/// already an aliased sibling prefix is considered owned by the
/// primary and is preserved, mirroring the file-counting carve-out in
/// `qartez_list_roots`. This is what protects rows like
/// `src/lib.rs` (no slash) and rows like
/// `qartez-public/src/lib.rs` (basename collides with primary
/// directory name) from being treated as orphans when the primary's
/// alias entry is absent.
pub fn purge_stale_roots(conn: &Connection, live_prefixes: &HashSet<String>) -> Result<usize> {
    let db_files = crate::storage::read::get_all_files(conn)?;
    let unaliased_primary = live_prefixes.contains("");
    // Build the set of explicit (non-empty) live prefixes so the
    // unaliased-primary carve-out can distinguish "claimed by sibling"
    // from "claimed by primary".
    let aliased_prefixes: HashSet<&str> = live_prefixes
        .iter()
        .filter(|p| !p.is_empty())
        .map(String::as_str)
        .collect();
    let mut orphan_prefixes: HashSet<String> = HashSet::new();
    for f in &db_files {
        let Some(slash_idx) = f.path.find('/') else {
            // No slash means a top-level row from the legacy single-
            // root layout. Always live; the prefix-based purge cannot
            // safely drop it.
            continue;
        };
        let prefix = &f.path[..slash_idx];
        if aliased_prefixes.contains(prefix) {
            continue;
        }
        if unaliased_primary {
            // Primary owns every prefix that is not a sibling alias.
            continue;
        }
        // No live root claims this prefix. Mark it for purge.
        orphan_prefixes.insert(prefix.to_string());
    }
    let mut total = 0usize;
    for prefix in &orphan_prefixes {
        let removed = crate::storage::write::delete_files_by_prefix(conn, prefix)?;
        total += removed;
        tracing::info!("purge_stale_roots: removed {removed} file(s) under prefix '{prefix}'");
    }
    Ok(total)
}

/// Drop file rows whose canonical disk path no longer exists.
///
/// Companion to [`purge_stale_roots`]: where that helper drops rows
/// whose root prefix is no longer registered, this one drops rows whose
/// underlying file was deleted, moved, or whose root once existed under
/// a different working directory (e.g. the legacy `tmp_test/` and
/// `tilde_doc/GitHub/...` ghost paths reported by the audit).
///
/// `roots` and `aliases` mirror `live_root_prefixes`'s inputs so the
/// helper can resolve a prefixed path like `ext/src/lib.rs` back to
/// the absolute on-disk path `<root_for_ext>/src/lib.rs`. When a row's
/// prefix matches no known root the row is treated as orphaned and
/// removed; when no alias claims the row's first segment, we treat
/// the row as belonging to the unaliased primary and resolve the
/// entire path against `primary_root`.
///
/// Returns the count of removed file rows; cascading deletes drop the
/// matching symbols, FTS rows, edges and refs.
pub fn purge_orphaned_files(
    conn: &Connection,
    primary_root: &Path,
    roots: &[std::path::PathBuf],
    aliases: &std::collections::HashMap<std::path::PathBuf, String>,
) -> Result<usize> {
    let db_files = crate::storage::read::get_all_files(conn)?;
    // Aliased prefixes only: the empty prefix and unaliased primary
    // are handled separately so an unprefixed `src/lib.rs` is not
    // accidentally treated as having `src` for a prefix.
    let mut prefix_to_root: std::collections::HashMap<String, std::path::PathBuf> =
        std::collections::HashMap::new();
    let mut unaliased_root: Option<std::path::PathBuf> = None;
    for root in roots {
        match aliases.get(root) {
            Some(alias) => {
                prefix_to_root.insert(alias.clone(), root.clone());
            }
            None => {
                if unaliased_root.is_none() {
                    unaliased_root = Some(root.clone());
                }
            }
        }
    }
    // Fall back to `primary_root` when no root in the live list lacks
    // an alias; this also covers the empty `roots` slice that callers
    // may pass for the legacy single-root layout.
    let unaliased_root = unaliased_root.unwrap_or_else(|| primary_root.to_path_buf());

    // Collect orphan file ids first so we don't mutate the table while
    // iterating its snapshot.
    let mut to_delete: Vec<i64> = Vec::new();
    for f in &db_files {
        let abs = match f.path.find('/') {
            Some(idx) => {
                let prefix = &f.path[..idx];
                let rel = &f.path[idx + 1..];
                match prefix_to_root.get(prefix) {
                    Some(root_dir) => root_dir.join(rel),
                    None => {
                        // Prefix is not an aliased sibling. Treat the
                        // entire path as relative to the unaliased
                        // primary; this preserves rows like
                        // `src/lib.rs` while still flagging
                        // `tmp_test/...` ghosts when the primary tree
                        // does not contain that file.
                        unaliased_root.join(&f.path)
                    }
                }
            }
            None => unaliased_root.join(&f.path),
        };
        if !abs.exists() {
            to_delete.push(f.id);
        }
    }
    let total = to_delete.len();
    for id in to_delete {
        crate::storage::write::delete_file_data(conn, id)?;
    }
    if total > 0 {
        tracing::info!("purge_orphaned_files: removed {total} file row(s) with no on-disk path");
    }
    Ok(total)
}

/// Coverage gaps for derived tables relative to the file table.
///
/// Surface for `qartez_maintenance stats` so operators can see when
/// `qartez_workspace add/remove` left the index in a degraded state.
/// Derived tables (`pagerank` column on `files`, `symbol_refs`,
/// `co_changes`, `unused_exports`) are only fully accurate after a
/// graph-rebuild pass; mid-cycle gaps are silent without this report.
#[derive(Debug, Clone, Default)]
pub struct DerivedTableGaps {
    /// Files whose `pagerank` column is exactly 0.0. Excludes the
    /// case where the table is empty (then `total_files` is zero too).
    pub files_with_zero_pagerank: i64,
    /// Total file rows (denominator for the pagerank ratio).
    pub total_files: i64,
    /// Files that have at least one symbol but zero `symbols_body_fts`
    /// rows for those symbols. A non-zero count usually indicates a
    /// `body_fts` wipe that the per-file rebuild has not yet healed.
    pub files_missing_body_fts: i64,
}

/// Compute coverage gaps for derived tables in a single pass.
///
/// Cheap: every query is a single `COUNT(*)` over indexed columns. No
/// table scans beyond what the existing indexes already cover.
pub fn collect_derived_table_gaps(conn: &Connection) -> DerivedTableGaps {
    let total_files: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap_or(0);
    let files_with_zero_pagerank: i64 = if total_files > 0 {
        conn.query_row("SELECT COUNT(*) FROM files WHERE pagerank = 0.0", [], |r| {
            r.get(0)
        })
        .unwrap_or(0)
    } else {
        0
    };
    // Files that contain at least one symbol whose body FTS row is
    // missing. The standalone `symbols_body_fts` table is not joined by
    // FK so a wipe leaves dangling symbols visible to qartez_grep
    // search_bodies but unmatchable.
    let files_missing_body_fts: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT s.file_id) FROM symbols s \
             WHERE NOT EXISTS (SELECT 1 FROM symbols_body_fts b WHERE b.rowid = s.id)",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    DerivedTableGaps {
        files_with_zero_pagerank,
        total_files,
        files_missing_body_fts,
    }
}

/// Return a one-line summary of the DB and WAL sizes plus a warning
/// when either crosses our heuristic thresholds.
///
/// Designed for `tracing::info!` at startup: cheap (two filesystem
/// metadata calls), human-readable, and identifies the two most
/// common runaway-growth cases.
pub fn startup_telemetry(db_path: &Path) -> String {
    /// DB-size threshold above which we emit a warning suggesting
    /// `qartez_maintenance vacuum`. 1 GiB matches the issue's
    /// "ignore-rules + body FTS bloat" scenario.
    const DB_WARN_BYTES: u64 = 1024 * 1024 * 1024;
    /// WAL-size threshold above which we emit a checkpoint hint.
    /// 500 MiB is large enough to indicate something stalled the
    /// last run before its checkpoint completed.
    const WAL_WARN_BYTES: u64 = 500 * 1024 * 1024;

    let db_bytes = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    let wal_bytes = std::fs::metadata(wal_path(db_path))
        .map(|m| m.len())
        .unwrap_or(0);
    let mut msg = format!(
        "qartez DB telemetry: {} ({}), WAL {}",
        db_path.display(),
        human_bytes(db_bytes),
        human_bytes(wal_bytes),
    );
    if db_bytes > DB_WARN_BYTES {
        msg.push_str(" [DB > 1 GiB - run qartez_maintenance vacuum to compact]");
    }
    if wal_bytes > WAL_WARN_BYTES {
        msg.push_str(" [WAL > 500 MiB - run qartez_maintenance checkpoint]");
    }
    msg
}

fn pragma_i64(conn: &Connection, pragma: &str) -> i64 {
    conn.query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0))
        .unwrap_or(0)
}

fn pragma_string(conn: &Connection, pragma: &str) -> String {
    conn.query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0))
        .unwrap_or_default()
}

fn wal_path(db_path: &Path) -> std::path::PathBuf {
    let mut p = db_path.as_os_str().to_owned();
    p.push("-wal");
    std::path::PathBuf::from(p)
}

fn shm_path(db_path: &Path) -> std::path::PathBuf {
    let mut p = db_path.as_os_str().to_owned();
    p.push("-shm");
    std::path::PathBuf::from(p)
}

/// Render a byte count as a short human-readable string, e.g. `"1.4 GiB"`.
pub fn human_bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if n >= GIB {
        format!("{:.2} GiB", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.2} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.2} KiB", n as f64 / KIB as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tempfile::TempDir;

    fn populated_db(dir: &Path) -> std::path::PathBuf {
        let db_path = dir.join("index.db");
        let conn = crate::storage::open_db(&db_path).unwrap();
        // Single file insert to give purge_stale_roots and stats something
        // to count without dragging in the full indexer.
        conn.execute(
            "INSERT INTO files (path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES ('alpha/main.rs', 0, 0, 'rust', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES ('beta/lib.rs', 0, 0, 'rust', 0, 0)",
            [],
        )
        .unwrap();
        db_path
    }

    #[test]
    fn stats_returns_nonnegative_sizes() {
        let tmp = TempDir::new().unwrap();
        let db_path = populated_db(tmp.path());
        let conn = crate::storage::open_db(&db_path).unwrap();
        let s = stats(&conn, &db_path).expect("stats must succeed on fresh DB");
        assert!(s.db_bytes > 0, "fresh DB must have non-zero size");
        assert!(s.page_size > 0);
        assert!(s.top_tables.iter().any(|t| t.name == "files"));
        let files_row = s.top_tables.iter().find(|t| t.name == "files").unwrap();
        assert_eq!(files_row.row_count, 2);
    }

    #[test]
    fn purge_stale_roots_removes_orphan_prefixes() {
        let tmp = TempDir::new().unwrap();
        let db_path = populated_db(tmp.path());
        let conn = crate::storage::open_db(&db_path).unwrap();

        // Only "alpha" survives. "beta" is orphaned and must be purged.
        let mut live: HashSet<String> = HashSet::new();
        live.insert("alpha".to_string());
        let removed = purge_stale_roots(&conn, &live).expect("purge must succeed");
        assert_eq!(removed, 1, "exactly one beta/* file should be purged");

        let remaining = crate::storage::read::get_all_files(&conn).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].path, "alpha/main.rs");
    }

    #[test]
    fn checkpoint_truncate_succeeds_on_fresh_wal() {
        let tmp = TempDir::new().unwrap();
        let db_path = populated_db(tmp.path());
        let conn = crate::storage::open_db(&db_path).unwrap();
        let (busy, _log, _ckpt) =
            checkpoint_truncate(&conn).expect("checkpoint must succeed on idle DB");
        assert_eq!(busy, 0, "fresh idle DB must report busy=0");
    }

    #[test]
    fn optimize_fts_does_not_error_on_empty_tables() {
        let tmp = TempDir::new().unwrap();
        let db_path = populated_db(tmp.path());
        let conn = crate::storage::open_db(&db_path).unwrap();
        optimize_fts(&conn).expect("optimize_fts must accept empty tables");
    }

    #[test]
    fn human_bytes_renders_thresholds() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert!(human_bytes(2 * 1024).contains("KiB"));
        assert!(human_bytes(2 * 1024 * 1024).contains("MiB"));
        assert!(human_bytes(2 * 1024 * 1024 * 1024).contains("GiB"));
    }

    #[test]
    fn startup_telemetry_returns_string_for_missing_path() {
        let msg = startup_telemetry(Path::new("/nonexistent/path/index.db"));
        assert!(msg.contains("qartez DB telemetry"));
        assert!(msg.contains("0 B"));
    }
}
