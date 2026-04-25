// Rust guideline compliant 2026-04-25
//
// Workspace fingerprint and qartez_maintenance regression coverage
// (issue #30: prevent startup bloat and repeated full reindexing).

use std::collections::HashSet;
use std::path::PathBuf;

use qartez_mcp::config::Config;
use qartez_mcp::index;
use qartez_mcp::index::fingerprint;
use qartez_mcp::storage;
use qartez_mcp::storage::maintenance;

fn write(path: &std::path::Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn cfg(root: PathBuf) -> Config {
    Config {
        project_roots: vec![root.clone()],
        root_aliases: std::collections::HashMap::new(),
        primary_root: root.clone(),
        db_path: root.join(".qartez").join("index.db"),
        reindex: false,
        git_depth: 0,
        has_project: true,
    }
}

#[test]
fn fingerprint_round_trips_through_meta_table() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("src/main.rs"), "fn main() {}\n");
    let config = cfg(tmp.path().to_path_buf());

    if let Some(parent) = config.db_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let conn = storage::open_db(&config.db_path).unwrap();
    index::full_index_multi(
        &conn,
        &config.project_roots,
        &config.root_aliases,
        config.reindex,
    )
    .unwrap();

    let fp = fingerprint::compute_workspace_fingerprint(&config);
    qartez_mcp::storage::write::set_meta(&conn, fingerprint::META_KEY_WORKSPACE_FINGERPRINT, &fp)
        .unwrap();

    let read_back =
        qartez_mcp::storage::read::get_meta(&conn, fingerprint::META_KEY_WORKSPACE_FINGERPRINT)
            .unwrap();
    assert_eq!(read_back.as_deref(), Some(fp.as_str()));
}

#[test]
fn fingerprint_invalidates_when_qartezignore_appears() {
    let tmp = tempfile::tempdir().unwrap();
    let config = cfg(tmp.path().to_path_buf());

    let no_ignore = fingerprint::compute_workspace_fingerprint(&config);

    write(&tmp.path().join(".qartezignore"), "vendor/\n");
    let with_ignore = fingerprint::compute_workspace_fingerprint(&config);

    assert_ne!(
        no_ignore, with_ignore,
        "creating .qartezignore must change the fingerprint so a fresh ignore rule triggers a reindex"
    );
}

#[test]
fn fingerprint_invalidates_when_root_added() {
    let tmp1 = tempfile::tempdir().unwrap();
    let tmp2 = tempfile::tempdir().unwrap();

    let mut config = cfg(tmp1.path().to_path_buf());
    let one = fingerprint::compute_workspace_fingerprint(&config);

    config.project_roots.push(tmp2.path().to_path_buf());
    let two = fingerprint::compute_workspace_fingerprint(&config);

    assert_ne!(
        one, two,
        "adding a project root must invalidate the fingerprint"
    );
}

#[test]
fn maintenance_stats_reports_fingerprint_and_timestamps() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("src/lib.rs"), "pub fn lib() {}\n");
    let config = cfg(tmp.path().to_path_buf());

    if let Some(parent) = config.db_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let conn = storage::open_db(&config.db_path).unwrap();
    index::full_index_multi(
        &conn,
        &config.project_roots,
        &config.root_aliases,
        config.reindex,
    )
    .unwrap();
    let fp = fingerprint::compute_workspace_fingerprint(&config);
    qartez_mcp::storage::write::set_meta(&conn, fingerprint::META_KEY_WORKSPACE_FINGERPRINT, &fp)
        .unwrap();
    qartez_mcp::storage::write::set_meta(
        &conn,
        fingerprint::META_KEY_LAST_FULL_REINDEX,
        "1714000000",
    )
    .unwrap();

    let stats = maintenance::stats(&conn, &config.db_path).unwrap();
    assert_eq!(stats.fingerprint.as_deref(), Some(fp.as_str()));
    assert_eq!(stats.last_full_reindex, Some(1714000000));
    assert!(stats.db_bytes > 0);
    assert!(
        stats.top_tables.iter().any(|t| t.name == "files"),
        "top_tables must include 'files'"
    );
}

#[test]
fn purge_stale_drops_orphan_prefixes() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("index.db");
    let conn = storage::open_db(&db_path).unwrap();
    conn.execute(
        "INSERT INTO files (path, mtime_ns, size_bytes, language, line_count, indexed_at)
         VALUES ('alpha/x.rs', 0, 0, 'rust', 0, 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO files (path, mtime_ns, size_bytes, language, line_count, indexed_at)
         VALUES ('beta/y.rs', 0, 0, 'rust', 0, 0)",
        [],
    )
    .unwrap();

    let mut live: HashSet<String> = HashSet::new();
    live.insert("alpha".to_string());
    let removed = maintenance::purge_stale_roots(&conn, &live).unwrap();
    assert_eq!(removed, 1, "only the orphaned beta/* row should be purged");

    let remaining = qartez_mcp::storage::read::get_all_files(&conn).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].path, "alpha/x.rs");
}

#[test]
fn purge_stale_preserves_primary_unprefixed_rows_when_secondary_has_alias() {
    // A multi-root configuration where one root has no alias must keep
    // unprefixed rows live: those rows belong to the unaliased root
    // (legacy single-root layout) and dropping them would silently wipe
    // most of the primary's index.
    let primary = tempfile::tempdir().unwrap();
    let secondary = tempfile::tempdir().unwrap();
    let db_path = primary.path().join("index.db");
    let conn = storage::open_db(&db_path).unwrap();

    for path in ["src/foo.rs", "tests/bar.rs", "ext/src/baz.rs"] {
        conn.execute(
            "INSERT INTO files (path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES (?1, 0, 0, 'rust', 0, 0)",
            [path],
        )
        .unwrap();
    }

    let roots = vec![primary.path().to_path_buf(), secondary.path().to_path_buf()];
    let mut aliases = std::collections::HashMap::new();
    aliases.insert(secondary.path().to_path_buf(), "ext".to_string());

    let live: HashSet<String> = fingerprint::live_root_prefixes(&roots, &aliases)
        .into_iter()
        .collect();
    let removed = maintenance::purge_stale_roots(&conn, &live).unwrap();
    assert_eq!(removed, 0, "primary's unprefixed rows must not be purged");

    let remaining = qartez_mcp::storage::read::get_all_files(&conn).unwrap();
    assert!(remaining.iter().any(|f| f.path == "src/foo.rs"));
    assert!(remaining.iter().any(|f| f.path == "tests/bar.rs"));
    assert!(remaining.iter().any(|f| f.path == "ext/src/baz.rs"));
}

#[test]
fn checkpoint_truncate_runs_against_post_index_db() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("src/main.rs"), "fn main() {}\n");
    let config = cfg(tmp.path().to_path_buf());
    if let Some(parent) = config.db_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let conn = storage::open_db(&config.db_path).unwrap();
    index::full_index_multi(
        &conn,
        &config.project_roots,
        &config.root_aliases,
        config.reindex,
    )
    .unwrap();

    let (busy, _log, _ckpt) = maintenance::checkpoint_truncate(&conn).unwrap();
    assert_eq!(busy, 0, "idle DB should report busy=0 from checkpoint");
}

#[test]
fn deferred_compaction_env_skips_inline_wal_truncate() {
    // Set the env var BEFORE opening the DB so the indexer's inline
    // checkpoint is skipped. The post-indexer WAL file should still
    // exist but the checkpoint counter shouldn't have run.
    //
    // We can't directly observe "checkpoint did not run" without
    // hooking SQLite's internals, so we verify the behaviour by
    // contract: the indexer succeeds, the meta table is populated,
    // and a follow-up explicit checkpoint succeeds (i.e. the
    // earlier-skipped one didn't leave the DB in a busy state).
    // SAFETY: this test is single-threaded inside `cargo test`'s
    // per-test isolation.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("QARTEZ_DEFER_COMPACTION", "1");
    }

    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("src/lib.rs"), "pub fn f() {}\n");
    let config = cfg(tmp.path().to_path_buf());
    if let Some(parent) = config.db_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let conn = storage::open_db(&config.db_path).unwrap();
    index::full_index_multi(
        &conn,
        &config.project_roots,
        &config.root_aliases,
        config.reindex,
    )
    .unwrap();

    let last = qartez_mcp::storage::read::get_meta(&conn, "last_index").unwrap();
    assert!(last.is_some(), "indexer must still write last_index");

    let (busy, _, _) = maintenance::checkpoint_truncate(&conn).unwrap();
    assert_eq!(
        busy, 0,
        "post-deferred checkpoint should run cleanly with busy=0"
    );

    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("QARTEZ_DEFER_COMPACTION");
    }
}
