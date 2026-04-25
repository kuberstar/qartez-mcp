// End-to-end verification of the body FTS fix.
//
// Bug being closed: `qartez_workspace add` for a secondary root used to wipe
// primary-root bodies because `rebuild_symbol_bodies_multi` does
// `DELETE FROM symbols_body_fts` and only repopulates files reachable from the
// given root. After the fix in `index/mod.rs:580-591`, secondary-root indexing
// must NOT touch primary bodies.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use rusqlite::Connection;
use tempfile::TempDir;

use qartez_mcp::index;
use qartez_mcp::server::QartezServer;
use qartez_mcp::storage::{read, schema, write};

fn setup_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    conn
}

fn make_primary_project(dir: &Path) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"primary\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/alpha.rs"),
        "pub fn alpha_fn() {\n    let _ = \"MARKER_PRIMARY_ALPHA\";\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/beta.rs"),
        "pub fn beta_fn() {\n    let _ = \"MARKER_PRIMARY_BETA\";\n}\n",
    )
    .unwrap();
}

fn body_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM symbols_body_fts", [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn marker_hits(conn: &Connection, marker: &str) -> usize {
    read::search_symbol_bodies_fts(conn, marker, 100)
        .unwrap()
        .len()
}

// ---------------------------------------------------------------------------
// Scenario 1: single-root full_index baseline
// ---------------------------------------------------------------------------
#[test]
fn baseline_full_index_populates_body_fts_for_all_files() {
    let primary = TempDir::new().unwrap();
    make_primary_project(primary.path());
    let conn = setup_db();
    index::full_index(&conn, primary.path(), false).unwrap();

    assert!(
        marker_hits(&conn, "MARKER_PRIMARY_ALPHA") >= 1,
        "alpha marker must be searchable in body FTS after full_index"
    );
    assert!(
        marker_hits(&conn, "MARKER_PRIMARY_BETA") >= 1,
        "beta marker must be searchable in body FTS after full_index"
    );
}

// ---------------------------------------------------------------------------
// Scenario 2: workspace-add must NOT wipe primary-root bodies (the bug repro)
// ---------------------------------------------------------------------------
#[test]
fn workspace_add_secondary_root_preserves_primary_body_fts() {
    let primary = TempDir::new().unwrap();
    make_primary_project(primary.path());

    let conn = setup_db();
    index::full_index(&conn, primary.path(), false).unwrap();

    let primary_alpha_pre = marker_hits(&conn, "MARKER_PRIMARY_ALPHA");
    let primary_beta_pre = marker_hits(&conn, "MARKER_PRIMARY_BETA");
    let body_pre = body_count(&conn);

    assert!(primary_alpha_pre >= 1);
    assert!(primary_beta_pre >= 1);

    // Now simulate `qartez_workspace add` of a secondary directory.
    let secondary = TempDir::new().unwrap();
    fs::write(
        secondary.path().join("delta.rs"),
        "pub fn delta_fn() {\n    let _ = \"MARKER_SECONDARY_DELTA\";\n}\n",
    )
    .unwrap();

    // Mirror what `add_root_inner` builds: extra_known is the set of paths
    // already in the DB so cross-root import resolution can find them.
    let extra_known: HashSet<String> = read::get_all_files(&conn)
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();

    index::full_index_root(&conn, secondary.path(), false, "secalias", &extra_known).unwrap();

    let primary_alpha_post = marker_hits(&conn, "MARKER_PRIMARY_ALPHA");
    let primary_beta_post = marker_hits(&conn, "MARKER_PRIMARY_BETA");
    let secondary_delta_post = marker_hits(&conn, "MARKER_SECONDARY_DELTA");
    let body_post = body_count(&conn);

    assert!(
        primary_alpha_post >= primary_alpha_pre,
        "primary alpha hits dropped after workspace-add: pre={primary_alpha_pre} post={primary_alpha_post} (the bug)"
    );
    assert!(
        primary_beta_post >= primary_beta_pre,
        "primary beta hits dropped after workspace-add: pre={primary_beta_pre} post={primary_beta_post} (the bug)"
    );
    assert!(
        secondary_delta_post >= 1,
        "secondary delta marker not searchable; expected workspace-add to populate the new root's bodies"
    );
    assert!(
        body_post >= body_pre,
        "body_fts row count dropped after workspace-add ({body_pre} -> {body_post}); secondary-root indexing must only ADD rows"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: force re-index still populates bodies
// ---------------------------------------------------------------------------
#[test]
fn force_full_index_repopulates_bodies() {
    let primary = TempDir::new().unwrap();
    make_primary_project(primary.path());
    let conn = setup_db();
    index::full_index(&conn, primary.path(), false).unwrap();
    assert!(marker_hits(&conn, "MARKER_PRIMARY_ALPHA") >= 1);

    // force=true re-ingests every file even when mtime matches.
    index::full_index(&conn, primary.path(), true).unwrap();

    assert!(
        marker_hits(&conn, "MARKER_PRIMARY_ALPHA") >= 1,
        "force reindex lost the alpha marker"
    );
    assert!(
        marker_hits(&conn, "MARKER_PRIMARY_BETA") >= 1,
        "force reindex lost the beta marker"
    );
}

// ---------------------------------------------------------------------------
// Scenario 4: incremental_index regression check
// ---------------------------------------------------------------------------
#[test]
fn incremental_index_updates_body_fts_for_changed_file() {
    let primary = TempDir::new().unwrap();
    fs::create_dir_all(primary.path().join("src")).unwrap();
    fs::write(
        primary.path().join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let target = primary.path().join("src").join("lib.rs");
    fs::write(
        &target,
        "pub fn before_fn() {\n    let _ = \"MARKER_BEFORE\";\n}\n",
    )
    .unwrap();
    let conn = setup_db();
    index::full_index(&conn, primary.path(), false).unwrap();

    assert!(marker_hits(&conn, "MARKER_BEFORE") >= 1);
    assert_eq!(marker_hits(&conn, "MARKER_AFTER"), 0);

    // Modify the file and run incremental.
    fs::write(
        &target,
        "pub fn after_fn() {\n    let _ = \"MARKER_AFTER\";\n}\n",
    )
    .unwrap();
    // Bump mtime so the unchanged-skip doesn't fire. Windows `SetFileTime`
    // (the syscall behind `File::set_modified`) requires the handle to have
    // write access, while Unix `futimens` accepts a read-only fd. Open with
    // write so the call works on every platform.
    let now = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    let f = fs::OpenOptions::new().write(true).open(&target).unwrap();
    f.set_modified(now).unwrap();
    drop(f);

    index::incremental_index(&conn, primary.path(), &[target.clone()], &[]).unwrap();

    assert!(
        marker_hits(&conn, "MARKER_AFTER") >= 1,
        "incremental did not insert MARKER_AFTER body"
    );
    assert_eq!(
        marker_hits(&conn, "MARKER_BEFORE"),
        0,
        "incremental did not delete MARKER_BEFORE body"
    );
}

// ---------------------------------------------------------------------------
// Scenario 5a: startup self-heal triggers when bodies are wiped
// ---------------------------------------------------------------------------
#[test]
fn startup_self_heal_fires_when_bodies_wiped() {
    let primary = TempDir::new().unwrap();
    make_primary_project(primary.path());
    let conn = setup_db();
    index::full_index(&conn, primary.path(), false).unwrap();
    let pre_count = body_count(&conn);
    assert!(pre_count >= 2, "expected at least 2 body rows pre-wipe");

    // Simulate the corrupted state from the bug: wipe most rows, leave 1
    // orphan to defeat the legacy `body_count == 0` heuristic.
    conn.execute(
        "DELETE FROM symbols_body_fts WHERE rowid IN (SELECT rowid FROM symbols_body_fts LIMIT ?1)",
        [pre_count - 1],
    )
    .unwrap();
    let mid_count = body_count(&conn);
    assert_eq!(mid_count, 1, "expected exactly 1 orphan row after wipe");

    // Reopen via `with_roots_and_sources` (the build path that runs the heal).
    let _server = QartezServer::with_roots_and_sources(
        conn,
        primary.path().to_path_buf(),
        vec![primary.path().to_path_buf()],
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        0,
        false,
        None,
    );

    // Reach back into the DB through a fresh connection on the same in-memory
    // backing won't share state, so we have to verify via the server's own
    // db_connection accessor.
    let post_count = _server
        .db_connection()
        .query_row("SELECT COUNT(*) FROM symbols_body_fts", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert!(
        post_count >= pre_count,
        "self-heal did not fire: pre={pre_count} mid=1 post={post_count}"
    );
    assert!(
        marker_hits(&_server.db_connection(), "MARKER_PRIMARY_ALPHA") >= 1,
        "self-heal restored body count but markers missing"
    );
}

// ---------------------------------------------------------------------------
// Scenario 5b: 1-symbol edge case (the integer-division pitfall I just fixed).
// ---------------------------------------------------------------------------
#[test]
fn startup_self_heal_fires_for_one_symbol_zero_bodies() {
    let primary = TempDir::new().unwrap();
    fs::create_dir_all(primary.path().join("src")).unwrap();
    fs::write(
        primary.path().join("Cargo.toml"),
        "[package]\nname = \"tiny\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        primary.path().join("src/lib.rs"),
        "pub fn solo_fn() {\n    let _ = \"MARKER_SOLO\";\n}\n",
    )
    .unwrap();
    let conn = setup_db();
    index::full_index(&conn, primary.path(), false).unwrap();

    let symbol_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    // Single-symbol projects can also have method/struct entries from Cargo.toml
    // (TOML symbols, e.g. [package]). Fail loudly if the assumption breaks so
    // the test stays meaningful.
    assert!(symbol_count >= 1, "expected at least 1 symbol post-index");

    // Wipe ALL bodies (covers the legacy `== 0` case with symbol_count = 1, 2, ...).
    conn.execute("DELETE FROM symbols_body_fts", []).unwrap();
    assert_eq!(body_count(&conn), 0);

    // Reopen.
    let _server = QartezServer::with_roots_and_sources(
        conn,
        primary.path().to_path_buf(),
        vec![primary.path().to_path_buf()],
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        0,
        false,
        None,
    );

    let post_count = _server
        .db_connection()
        .query_row("SELECT COUNT(*) FROM symbols_body_fts", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert!(
        post_count >= 1,
        "self-heal did not fire for symbol_count=N, body_count=0 (the integer-division pitfall regression)"
    );
}

// ---------------------------------------------------------------------------
// Scenario 6: heal must NOT fire when index is healthy
// ---------------------------------------------------------------------------
#[test]
fn startup_self_heal_skips_when_healthy() {
    let primary = TempDir::new().unwrap();
    make_primary_project(primary.path());
    let conn = setup_db();
    index::full_index(&conn, primary.path(), false).unwrap();
    let pre_count = body_count(&conn);
    assert!(pre_count >= 2);

    // Reopen on a healthy index.
    let _server = QartezServer::with_roots_and_sources(
        conn,
        primary.path().to_path_buf(),
        vec![primary.path().to_path_buf()],
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        0,
        false,
        None,
    );

    let post_count = _server
        .db_connection()
        .query_row("SELECT COUNT(*) FROM symbols_body_fts", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(
        post_count, pre_count,
        "heal fired on a healthy index (would mean a wholesale rebuild on every server start): pre={pre_count} post={post_count}"
    );
}

// ---------------------------------------------------------------------------
// Sanity: rebuild_symbol_bodies still works as a manual heal entry point.
// ---------------------------------------------------------------------------
#[test]
fn manual_rebuild_symbol_bodies_repopulates_after_wipe() {
    let primary = TempDir::new().unwrap();
    make_primary_project(primary.path());
    let conn = setup_db();
    index::full_index(&conn, primary.path(), false).unwrap();
    conn.execute("DELETE FROM symbols_body_fts", []).unwrap();
    assert_eq!(body_count(&conn), 0);

    write::rebuild_symbol_bodies(&conn, primary.path()).unwrap();

    assert!(marker_hits(&conn, "MARKER_PRIMARY_ALPHA") >= 1);
    assert!(marker_hits(&conn, "MARKER_PRIMARY_BETA") >= 1);
}
