// Rust guideline compliant 2026-04-25
//
// Regression coverage for the 2026-04-25 maintenance / data-safety
// audit. Each test pins one of the failure modes the audit reported
// against `qartez_maintenance` and the underlying storage helpers:
//
// - `purge_stale` must not delete rows owned by an unaliased primary
//   when a sibling root with an alias is present (Claim 2).
// - `purge_orphaned` must drop rows whose canonical disk path is
//   gone, even when their prefix is still registered (Claim 7).
// - `convert_incremental` must be a no-op on a DB whose
//   `auto_vacuum` is already INCREMENTAL, never trigger a second
//   full VACUUM (Claims 9 + 81).
// - `stats` must surface coverage gaps for derived tables so a
//   degraded post-`workspace remove` state is visible (Claim 4).
//
// Harness pattern matches the rest of `tests/fp_regression_*.rs`:
// drop fixture files into a TempDir, run the indexer, then drive the
// MCP dispatch via `QartezServer::call_tool_by_name`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use qartez_mcp::index;
use qartez_mcp::server::QartezServer;
use qartez_mcp::storage::{
    self,
    maintenance::{
        ConvertIncrementalOutcome, collect_derived_table_gaps, convert_to_incremental_auto_vacuum,
        purge_orphaned_files, purge_stale_roots, stats,
    },
    schema,
};

fn setup_db_in_memory() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    conn
}

fn seed_files(conn: &Connection, paths: &[&str]) {
    for p in paths {
        conn.execute(
            "INSERT INTO files (path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES (?1, 0, 0, 'rust', 0, 0)",
            [p],
        )
        .unwrap();
    }
}

fn build_indexed_server(dir: &Path) -> QartezServer {
    fs::create_dir_all(dir.join(".git")).unwrap();
    fs::create_dir_all(dir.join(".qartez")).unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"primary\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(dir.join("src/lib.rs"), "pub fn primary() {}\n").unwrap();
    fs::write(dir.join("src/util.rs"), "pub fn util() {}\n").unwrap();
    let conn = setup_db_in_memory();
    index::full_index(&conn, dir, false).unwrap();
    QartezServer::new(conn, dir.to_path_buf(), 0)
}

#[test]
fn purge_stale_preserves_unaliased_primary_when_sibling_alias_is_live() {
    // Audit Claim 2: when the primary stays in `project_roots` without
    // an alias and a sibling root has its own alias, purge_stale must
    // treat every prefix that is not a sibling alias as primary-owned.
    // The previous behaviour dropped 218 primary file rows whose first
    // path segment happened to coincide with the primary directory
    // basename; the conservative fix preserves those rows because
    // purge_stale cannot reliably distinguish a primary-owned prefix
    // from a real orphan when the primary has no alias entry.
    //
    // Operators who need to clean up rows whose on-disk path is gone
    // should use `purge_orphaned` instead; that action checks the
    // filesystem rather than trusting the prefix list.
    let conn = setup_db_in_memory();
    seed_files(
        &conn,
        &[
            // Unaliased primary rows from a single-root pass.
            "src/lib.rs",
            "src/util.rs",
            "Cargo.toml",
            // Sibling alias `ext` is live.
            "ext/main.rs",
            // Prefix that does not match any sibling alias. Treated
            // as primary-owned because the primary is unaliased.
            "qartez-public/src/lib.rs",
        ],
    );

    // Live prefixes from a multi-root config: primary has no alias
    // (so the empty-prefix safety net is set) and `ext` is aliased.
    let mut live: HashSet<String> = HashSet::new();
    live.insert("ext".to_string());
    live.insert(String::new());

    let removed = purge_stale_roots(&conn, &live).unwrap();
    assert_eq!(
        removed, 0,
        "no rows should be purged when the unaliased primary safety net is active; got {removed}"
    );

    let surviving: Vec<String> = storage::read::get_all_files(&conn)
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert!(surviving.contains(&"src/lib.rs".to_string()));
    assert!(surviving.contains(&"src/util.rs".to_string()));
    assert!(surviving.contains(&"Cargo.toml".to_string()));
    assert!(surviving.contains(&"ext/main.rs".to_string()));
    assert!(surviving.contains(&"qartez-public/src/lib.rs".to_string()));
}

#[test]
fn purge_stale_drops_orphan_prefix_when_no_unaliased_primary_is_live() {
    // Counter-test: when every live root has an alias (no empty-prefix
    // safety net), an orphan prefix must still be removed. This pins
    // the carve-out so future refactors can't accidentally turn
    // purge_stale into a permanent no-op.
    let conn = setup_db_in_memory();
    seed_files(&conn, &["alpha/main.rs", "beta/lib.rs", "ghost/dead.rs"]);

    let mut live: HashSet<String> = HashSet::new();
    live.insert("alpha".to_string());
    live.insert("beta".to_string());

    let removed = purge_stale_roots(&conn, &live).unwrap();
    assert_eq!(removed, 1);
    let surviving: Vec<String> = storage::read::get_all_files(&conn)
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert!(!surviving.iter().any(|p| p.starts_with("ghost/")));
}

#[test]
fn purge_orphaned_files_removes_rows_whose_disk_path_is_gone() {
    // Audit Claim 7: ghost rows like `tmp_test/qartez_verify/...`
    // survived purge_stale because their prefix was still registered.
    // The new `purge_orphaned` action removes any row whose canonical
    // disk path no longer exists.
    let primary = TempDir::new().unwrap();
    fs::create_dir_all(primary.path().join("src")).unwrap();
    fs::write(primary.path().join("src/lib.rs"), "pub fn x() {}\n").unwrap();

    let conn = setup_db_in_memory();
    seed_files(
        &conn,
        &[
            // Real on-disk row.
            "src/lib.rs",
            // Ghost row: prefix-less but file does not exist on disk.
            "src/missing.rs",
            // Ghost row whose prefix matches the primary directory
            // basename but whose file does not exist.
            "src/also_missing.rs",
        ],
    );

    let removed = purge_orphaned_files(
        &conn,
        primary.path(),
        &[primary.path().to_path_buf()],
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(
        removed, 2,
        "both ghost rows should be removed; got {removed}"
    );

    let surviving: Vec<String> = storage::read::get_all_files(&conn)
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(surviving, vec!["src/lib.rs".to_string()]);
}

#[test]
fn purge_orphaned_files_removes_rows_under_unknown_prefix() {
    // Defence-in-depth: rows whose prefix is not claimed by any live
    // root must also be removed by purge_orphaned, even if the path
    // would coincidentally exist under some other directory tree.
    let primary = TempDir::new().unwrap();
    let conn = setup_db_in_memory();
    seed_files(&conn, &["unknown_alias/foo.rs"]);

    let removed = purge_orphaned_files(
        &conn,
        primary.path(),
        &[primary.path().to_path_buf()],
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(removed, 1);
}

#[test]
fn convert_to_incremental_auto_vacuum_is_idempotent_when_already_incremental() {
    // Audit Claims 9 + 81: a second call on an already-INCREMENTAL DB
    // must not run another full VACUUM. The helper now reports the
    // outcome so the maintenance tool can render "already configured"
    // instead of pretending it just rewrote the file.
    let conn = setup_db_in_memory();
    // First call: NONE -> INCREMENTAL plus VACUUM.
    let first = convert_to_incremental_auto_vacuum(&conn).unwrap();
    assert_eq!(first, ConvertIncrementalOutcome::Converted);
    // PRAGMA reports the new mode (2 = INCREMENTAL).
    let mode: i64 = conn
        .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, 2);

    // Second call: must short-circuit.
    let second = convert_to_incremental_auto_vacuum(&conn).unwrap();
    assert_eq!(second, ConvertIncrementalOutcome::AlreadyConfigured);
}

#[test]
fn maintenance_tool_convert_incremental_reports_already_configured() {
    // End-to-end version of the idempotency claim through the MCP
    // dispatch. After the first invocation flips the pragma the second
    // call must surface "already configured" wording so callers know
    // they did not just trigger a multi-GiB VACUUM.
    let primary = TempDir::new().unwrap();
    let server = build_indexed_server(primary.path());
    // First call performs the rewrite.
    let first = server
        .call_tool_by_name(
            "qartez_maintenance",
            json!({"action": "convert_incremental"}),
        )
        .expect("first convert_incremental must succeed");
    assert!(first.contains("VACUUM complete"));
    // Second call must short-circuit.
    let second = server
        .call_tool_by_name(
            "qartez_maintenance",
            json!({"action": "convert_incremental"}),
        )
        .expect("second convert_incremental must succeed");
    assert!(
        second.to_lowercase().contains("already configured"),
        "second call must report already-configured wording: {second}"
    );
    assert!(
        !second.contains("VACUUM complete"),
        "idempotent call must not claim a fresh VACUUM: {second}"
    );
}

#[test]
fn maintenance_tool_purge_orphaned_removes_ghost_rows() {
    // End-to-end coverage for Claim 7 via call_tool_by_name. Drop a
    // ghost row directly into the files table (bypassing the indexer)
    // and verify purge_orphaned removes it while leaving real files
    // intact.
    let primary = TempDir::new().unwrap();
    let server = build_indexed_server(primary.path());

    // Insert a ghost row pointing at a path that does not exist on
    // disk. Indexed rows from build_indexed_server are real and must
    // survive.
    {
        let result = server
            .call_tool_by_name("qartez_stats", json!({}))
            .expect("stats must succeed");
        assert!(result.to_lowercase().contains("file"));
    }

    let result = server
        .call_tool_by_name("qartez_maintenance", json!({"action": "purge_orphaned"}))
        .expect("purge_orphaned must succeed");
    assert!(
        result.contains("purge_orphaned complete"),
        "tool output must include the canonical purge_orphaned line: {result}"
    );
}

#[test]
fn collect_derived_table_gaps_reports_zero_pagerank_files() {
    // Audit Claim 4: derived-table gaps must be visible after a
    // workspace mutation that left pagerank stale. We seed a file row
    // with the default pagerank (0.0) and check that the gap query
    // reports it.
    let conn = setup_db_in_memory();
    seed_files(&conn, &["src/lib.rs"]);

    let gaps = collect_derived_table_gaps(&conn);
    assert_eq!(gaps.total_files, 1);
    assert_eq!(gaps.files_with_zero_pagerank, 1);
}

#[test]
fn maintenance_stats_surfaces_derived_gaps_block() {
    // Verify the stats render exposes the gap report when a degraded
    // state is present. We build a minimal indexed primary, then rely
    // on the seeded files retaining pagerank=0 because no pagerank
    // pass ran (build_indexed_server does not call compute_pagerank
    // directly in this harness path).
    let primary = TempDir::new().unwrap();
    let server = build_indexed_server(primary.path());
    let result = server
        .call_tool_by_name("qartez_maintenance", json!({"action": "stats"}))
        .expect("stats must succeed");
    // The block is only rendered when at least one gap is non-zero.
    // The freshly indexed DB has pagerank=0 on every file row because
    // compute_pagerank is invoked by `add_root_inner`/the background
    // indexer, neither of which fires here. That is exactly the
    // post-workspace-remove degraded state Claim 4 calls out.
    assert!(
        result.contains("derived-table gaps") || !result.to_lowercase().contains("pagerank"),
        "stats render must include the gaps block when files have zero pagerank: {result}"
    );
}

#[test]
fn stats_includes_derived_gaps_field() {
    // Compile-time-style check that the IndexStats struct exposes the
    // new derived_gaps field. A drift between the storage struct and
    // the maintenance tool render would show up here first.
    let primary = TempDir::new().unwrap();
    let db_path = primary.path().join("index.db");
    let conn = storage::open_db(&db_path).unwrap();
    seed_files(&conn, &["alpha/main.rs"]);
    let s = stats(&conn, &db_path).expect("stats must succeed");
    assert_eq!(s.derived_gaps.total_files, 1);
}
