// Live notify smoke test for the issue #34 watcher rewrite.
//
// This test actually spawns the OS file watcher and edits files on disk,
// then waits for the debouncer + outer drain + incremental reindex pipeline
// to surface the change in the DB. It is intentionally tolerant on timing
// because FSEvents and ReadDirectoryChangesW have OS-level coalescing
// windows that vary by hardware.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tempfile::TempDir;

use qartez_mcp::index;
use qartez_mcp::storage::open_db;
use qartez_mcp::watch::Watcher;

fn write_file(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn project_with_baseline(dir: &Path) {
    write_file(
        dir,
        "Cargo.toml",
        "[package]\nname=\"t\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
    );
    write_file(dir, "src/lib.rs", "pub fn alpha() {}\n");
}

fn wait_for_symbol(conn: &Arc<Mutex<Connection>>, name: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let found = {
            let guard = conn.lock().unwrap();
            guard
                .query_row(
                    "SELECT COUNT(*) FROM symbols WHERE name = ?1",
                    [name],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0)
        };
        if found > 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Block until the watcher pipeline is live, instead of sleeping a fixed
/// interval after spawning it.
///
/// `Watcher::run` shells out to `git config` for the global excludes file
/// before it registers the OS watcher, and an edit made before registration
/// is never reported. Under load (background QoS, a busy machine) that
/// setup outlasted the old fixed 400 ms sleep, so every edit in the run was
/// lost and all three tests failed together. Rewriting a sentinel file
/// with a fresh symbol until one gets indexed proves events flow end to end.
async fn wait_until_watching(root: &Path, db: &Arc<Mutex<Connection>>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut attempt = 0u32;
    while Instant::now() < deadline {
        let name = format!("watch_ready_{attempt}");
        write_file(
            root,
            "src/watch_ready.rs",
            &format!("pub fn {name}() {{}}\n"),
        );
        let seen = tokio::task::spawn_blocking({
            let db = db.clone();
            move || wait_for_symbol(&db, &name, Duration::from_secs(1))
        })
        .await
        .unwrap();
        if seen {
            return;
        }
        attempt += 1;
    }
    panic!("watcher never indexed the readiness sentinel within 30 s");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watcher_picks_up_a_file_modification() {
    let tmp = TempDir::new().unwrap();
    // macOS resolves /var → /private/var in FSEvents. Canonicalize once
    // so the ignore matcher's "path under root" check holds for every
    // event the OS forwards. Production goes through `config.rs`'s
    // `normalize_for_dedup` which does the same.
    let root = tmp.path().canonicalize().unwrap();
    project_with_baseline(&root);

    let db_path = root.join("idx.db");
    let conn = open_db(&db_path).unwrap();
    index::full_index(&conn, &root, false).unwrap();
    let db = Arc::new(Mutex::new(conn));

    let watcher = Watcher::new(db.clone(), root.clone());
    let handle = tokio::spawn(async move {
        let _ = watcher.run().await;
    });

    wait_until_watching(&root, &db).await;

    write_file(
        &root,
        "src/lib.rs",
        "pub fn alpha() {}\npub fn omega() {}\n",
    );

    // Allow up to 15 s, matching the delete test: debounce (200 ms) + drain
    // (500 ms) + reindex + FSEvents latency, which stretches past 5 s on an
    // overloaded runner.
    let saw_it = tokio::task::spawn_blocking({
        let db = db.clone();
        move || wait_for_symbol(&db, "omega", Duration::from_secs(15))
    })
    .await
    .unwrap();
    assert!(
        saw_it,
        "watcher must surface `omega` in the DB after editing src/lib.rs"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watcher_picks_up_a_new_file_in_a_subdir() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    project_with_baseline(&root);

    let db_path = root.join("idx.db");
    let conn = open_db(&db_path).unwrap();
    index::full_index(&conn, &root, false).unwrap();
    let db = Arc::new(Mutex::new(conn));

    let watcher = Watcher::new(db.clone(), root.clone());
    let handle = tokio::spawn(async move {
        let _ = watcher.run().await;
    });

    wait_until_watching(&root, &db).await;

    write_file(&root, "src/extra.rs", "pub fn zeta() {}\n");

    let saw_it = tokio::task::spawn_blocking({
        let db = db.clone();
        move || wait_for_symbol(&db, "zeta", Duration::from_secs(15))
    })
    .await
    .unwrap();
    assert!(
        saw_it,
        "watcher must index `zeta` after a new file is created"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watcher_processes_a_delete() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    project_with_baseline(&root);
    write_file(&root, "src/transient.rs", "pub fn doomed() {}\n");

    let db_path = root.join("idx.db");
    let conn = open_db(&db_path).unwrap();
    index::full_index(&conn, &root, false).unwrap();
    let db = Arc::new(Mutex::new(conn));

    // Sanity: starts present.
    {
        let g = db.lock().unwrap();
        let n: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM symbols WHERE name = 'doomed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    let watcher = Watcher::new(db.clone(), root.clone());
    let handle = tokio::spawn(async move {
        let _ = watcher.run().await;
    });

    wait_until_watching(&root, &db).await;
    std::fs::remove_file(root.join("src/transient.rs")).unwrap();

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut gone = false;
    while Instant::now() < deadline {
        let n: i64 = {
            let g = db.lock().unwrap();
            g.query_row(
                "SELECT COUNT(*) FROM symbols WHERE name = 'doomed'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
        };
        if n == 0 {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        gone,
        "watcher must drop `doomed` from the index after the file is deleted"
    );

    handle.abort();
}
