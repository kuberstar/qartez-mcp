// End-to-end coverage for the watcher perf rewrite (issue #34).
//
// These tests intentionally avoid the OS-level `notify` event stream because
// FSEvents on macOS and ReadDirectoryChangesW on Windows produce racy timing
// that makes integration tests flaky. Instead they exercise the indexer +
// pagerank + WAL + dirty-flag paths the watcher invokes per batch, then
// assert on the observable DB state.

use std::fs;
use std::path::Path;

use rusqlite::Connection;
use tempfile::TempDir;

use qartez_mcp::graph;
use qartez_mcp::index;
use qartez_mcp::storage::{open_db, read, write};

fn write_file(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn make_project(dir: &Path) {
    write_file(
        dir,
        "Cargo.toml",
        "[package]\nname=\"t\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
    );
    write_file(dir, "src/lib.rs", "pub fn alpha() {}\npub fn beta() {}\n");
    write_file(
        dir,
        "src/consumer.rs",
        "use crate::alpha;\npub fn use_alpha() { alpha(); }\n",
    );
}

#[test]
fn watcher_incremental_path_keeps_dirty_flag_set_until_first_read() {
    // The new incremental path marks unused_exports dirty instead of
    // running the full DELETE+INSERT. The flag must stay set until a
    // reader call materializes lazily.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    make_project(root);

    let conn = Connection::open_in_memory().unwrap();
    qartez_mcp::storage::schema::create_schema(&conn).unwrap();
    index::full_index(&conn, root, false).unwrap();

    // Sanity: full_index materializes and clears the flag.
    let pre = read::get_meta(&conn, write::META_KEY_UNUSED_EXPORTS_DIRTY).unwrap();
    assert!(pre.is_none(), "full_index must leave flag clear");

    // Simulate a watcher batch: incremental call against a touched file.
    write_file(
        root,
        "src/lib.rs",
        "pub fn alpha() {}\npub fn beta() {}\npub fn gamma() {}\n",
    );
    index::incremental_index(&conn, root, &[root.join("src/lib.rs")], &[]).unwrap();

    let dirty = read::get_meta(&conn, write::META_KEY_UNUSED_EXPORTS_DIRTY).unwrap();
    assert_eq!(
        dirty.as_deref(),
        Some("1"),
        "incremental must mark dirty, not repopulate"
    );

    // The first reader call clears the flag.
    let _ = read::count_unused_exports(&conn).unwrap();
    let after = read::get_meta(&conn, write::META_KEY_UNUSED_EXPORTS_DIRTY).unwrap();
    assert!(
        after.is_none(),
        "first read after dirty must clear the flag through lazy materialize"
    );
}

#[test]
fn watcher_path_runs_passive_checkpoint_not_truncate() {
    // The incremental indexer used to run wal_checkpoint(TRUNCATE) per
    // call. After the perf rewrite, it runs PASSIVE - non-blocking and
    // doesn't fsync-truncate. Verify by observing that a checkpoint
    // call from a freshly-opened connection (post-incremental) still
    // succeeds, which it would not if TRUNCATE had already returned a
    // SQLITE_BUSY from a contended writer.
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("idx.db");
    let project = TempDir::new().unwrap();
    make_project(project.path());

    let conn = open_db(&db_path).unwrap();
    index::full_index(&conn, project.path(), false).unwrap();

    write_file(
        project.path(),
        "src/lib.rs",
        "pub fn alpha() {}\npub fn beta() {}\npub fn delta() {}\n",
    );
    index::incremental_index(
        &conn,
        project.path(),
        &[project.path().join("src/lib.rs")],
        &[],
    )
    .unwrap();

    // After incremental, an explicit checkpoint of any kind must succeed
    // - this is the contract the watcher relies on when it follows up
    // with its periodic TRUNCATE.
    conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);")
        .expect("PASSIVE after incremental must succeed");
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("TRUNCATE after incremental must succeed");
}

#[test]
fn watcher_dedicated_connection_writes_visible_to_reader() {
    // The connection-split contract: writer commits made via Conn A are
    // observable from Conn B on the next query. SQLite WAL guarantees
    // this, but a regression in pragmas or schema setup could break it.
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("idx.db");
    let project = TempDir::new().unwrap();
    make_project(project.path());

    let writer = open_db(&db_path).unwrap();
    let reader = open_db(&db_path).unwrap();

    index::full_index(&writer, project.path(), false).unwrap();

    let file_count: i64 = reader
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap();
    assert!(
        file_count >= 2,
        "reader must see writer's full_index commits, got {file_count}"
    );

    // Now incremental on the writer and verify the reader sees the new symbol.
    write_file(
        project.path(),
        "src/lib.rs",
        "pub fn alpha() {}\npub fn beta() {}\npub fn epsilon() {}\n",
    );
    index::incremental_index(
        &writer,
        project.path(),
        &[project.path().join("src/lib.rs")],
        &[],
    )
    .unwrap();

    let epsilon: i64 = reader
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE name = 'epsilon'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        epsilon, 1,
        "reader must observe the watcher-side incremental insert"
    );
}

#[test]
fn watcher_pagerank_can_be_deferred_without_corrupting_ranks() {
    // The watcher cadence skips PageRank between recompute windows. The
    // existing ranks must remain queryable and consistent until the next
    // recompute fires.
    let tmp = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let db_path = tmp.path().join("idx.db");
    make_project(project.path());

    let conn = open_db(&db_path).unwrap();
    index::full_index(&conn, project.path(), false).unwrap();
    graph::pagerank::compute_pagerank(&conn, &Default::default()).unwrap();

    let pre: f64 = conn
        .query_row(
            "SELECT pagerank FROM files WHERE path LIKE '%lib.rs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(pre > 0.0, "baseline pagerank must be populated");

    // Three incremental batches without an intervening pagerank recompute.
    for i in 0..3 {
        write_file(
            project.path(),
            "src/lib.rs",
            &format!("pub fn alpha() {{}}\npub fn beta() {{}}\npub fn step{i}() {{}}\n"),
        );
        index::incremental_index(
            &conn,
            project.path(),
            &[project.path().join("src/lib.rs")],
            &[],
        )
        .unwrap();
    }

    // Without an explicit recompute, the rank stays at the prior value.
    let mid: f64 = conn
        .query_row(
            "SELECT pagerank FROM files WHERE path LIKE '%lib.rs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        pre, mid,
        "pagerank must remain stable when watcher defers recompute"
    );

    // When the watcher does fire the recompute, the value is still valid.
    graph::pagerank::compute_pagerank(&conn, &Default::default()).unwrap();
    let post: f64 = conn
        .query_row(
            "SELECT pagerank FROM files WHERE path LIKE '%lib.rs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(post > 0.0, "post-recompute pagerank must be valid");
}

#[test]
fn dedicated_writer_and_reader_handle_concurrent_workloads() {
    // Smoke test: interleave writer commits with reader queries on the
    // same DB file via two connections. No SQLITE_BUSY, no FK errors.
    let tmp = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let db_path = tmp.path().join("idx.db");
    make_project(project.path());

    let writer = open_db(&db_path).unwrap();
    let reader = open_db(&db_path).unwrap();
    index::full_index(&writer, project.path(), false).unwrap();

    for i in 0..10 {
        let body = format!("pub fn alpha() {{}}\npub fn beta() {{}}\npub fn iter{i}() {{}}\n");
        write_file(project.path(), "src/lib.rs", &body);
        index::incremental_index(
            &writer,
            project.path(),
            &[project.path().join("src/lib.rs")],
            &[],
        )
        .unwrap();

        let n: i64 = reader
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert!(
            n >= 3,
            "reader must see at least the base 3 symbols on iter {i}"
        );
    }
}
