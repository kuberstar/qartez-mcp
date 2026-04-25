//! Integration coverage for cross-process index serialization.
//!
//! These tests exercise the public `RepoLock` surface from
//! `qartez_mcp::lock` to validate that:
//!
//! 1. Two concurrent indexer threads racing on the same `.qartez`
//!    directory both succeed without `SQLITE_BUSY`, because the lock
//!    serializes them above the SQLite layer.
//! 2. A second contender observes the holder's PID via the lock file,
//!    matching the contention-message contract surfaced to users.
//! 3. The lock file is left on disk after the holder drops (the OS
//!    releases the advisory lock; we deliberately do not unlink it),
//!    and the next acquire reuses the same path.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use qartez_mcp::index;
use qartez_mcp::lock::{LockError, RepoLock};
use qartez_mcp::storage;
use tempfile::TempDir;

/// Build a minimal indexable project on disk: one Rust source file with
/// a single function. Enough surface area to exercise `full_index` end
/// to end while keeping the test fast.
fn make_project(dir: &std::path::Path) {
    fs::create_dir_all(dir).expect("project dir");
    fs::write(
        dir.join("lib.rs"),
        "pub fn answer() -> u32 { 42 }\npub fn greet() -> &'static str { \"hi\" }\n",
    )
    .expect("write lib.rs");
}

/// `qartez_dir` corresponds to `.qartez/` next to the database file - the
/// same convention `Config::from_cli` uses in `db_anchor.join(".qartez")`.
fn qartez_dir(project: &std::path::Path) -> PathBuf {
    let dir = project.join(".qartez");
    fs::create_dir_all(&dir).expect("qartez dir");
    dir
}

#[test]
fn concurrent_indexers_serialize_without_sqlite_busy() {
    let tmp = TempDir::new().expect("tempdir");
    let project = tmp.path().join("repo");
    make_project(&project);
    let qdir = qartez_dir(&project);
    let db_path = qdir.join("index.db");

    let barrier = Arc::new(Barrier::new(2));
    let errors = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let busy_observed = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let handles: Vec<_> = (0..2)
        .map(|i| {
            let qdir = qdir.clone();
            let db_path = db_path.clone();
            let project = project.clone();
            let barrier = Arc::clone(&barrier);
            let errors = Arc::clone(&errors);
            let busy_observed = Arc::clone(&busy_observed);

            thread::spawn(move || {
                barrier.wait();

                let lock = match RepoLock::acquire_with_deadline(&qdir, Duration::from_secs(30)) {
                    Ok(g) => g,
                    Err(LockError::Busy { .. }) => {
                        busy_observed.store(true, std::sync::atomic::Ordering::SeqCst);
                        errors
                            .lock()
                            .unwrap()
                            .push(format!("thread {i}: hit Busy unexpectedly"));
                        return;
                    }
                    Err(other) => {
                        errors
                            .lock()
                            .unwrap()
                            .push(format!("thread {i}: lock IO {other}"));
                        return;
                    }
                };

                let conn = match storage::open_db(&db_path) {
                    Ok(c) => c,
                    Err(e) => {
                        errors
                            .lock()
                            .unwrap()
                            .push(format!("thread {i}: open_db {e}"));
                        return;
                    }
                };

                if let Err(e) = index::full_index(&conn, &project, false) {
                    let msg = e.to_string();
                    if msg.contains("SQLITE_BUSY") || msg.contains("database is locked") {
                        errors
                            .lock()
                            .unwrap()
                            .push(format!("thread {i}: SQLITE_BUSY {msg}"));
                    } else {
                        errors
                            .lock()
                            .unwrap()
                            .push(format!("thread {i}: index error {msg}"));
                    }
                }

                drop(lock);
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread join");
    }

    let errs = errors.lock().unwrap();
    assert!(
        errs.is_empty(),
        "concurrent indexers must succeed; errors: {errs:?}"
    );
    assert!(
        !busy_observed.load(std::sync::atomic::Ordering::SeqCst),
        "neither thread should have given up with Busy when the deadline is generous"
    );
}

#[test]
fn second_acquire_surfaces_holder_pid_after_deadline() {
    let tmp = TempDir::new().expect("tempdir");
    let qdir = qartez_dir(tmp.path());

    let _holder = RepoLock::acquire(&qdir).expect("first acquire");
    let start = Instant::now();
    let err = RepoLock::acquire_with_deadline(&qdir, Duration::from_millis(250))
        .expect_err("must time out while holder is alive");
    let elapsed = start.elapsed();

    assert!(
        elapsed >= Duration::from_millis(200),
        "expected deadline to be respected (elapsed {elapsed:?})"
    );

    match err {
        LockError::Busy {
            holder_pid,
            elapsed_ms,
            ..
        } => {
            assert_eq!(
                holder_pid,
                Some(std::process::id()),
                "holder pid must match the current process"
            );
            assert!(
                elapsed_ms >= 200,
                "elapsed_ms must reflect the deadline (got {elapsed_ms})"
            );
        }
        other => panic!("expected Busy, got {other:?}"),
    }
}

#[test]
fn lock_file_persists_after_drop_and_can_be_reacquired() {
    let tmp = TempDir::new().expect("tempdir");
    let qdir = qartez_dir(tmp.path());
    let lock_path = qdir.join("index.lock");

    {
        let _g = RepoLock::acquire(&qdir).expect("first acquire");
        assert!(lock_path.exists(), "lock file must exist while held");
    }
    // Drop unlocks, but the file path stays so concurrent acquirers all
    // open the same inode. Cargo and git follow the same convention.
    assert!(
        lock_path.exists(),
        "lock file must remain on disk after holder drops"
    );

    let _g = RepoLock::acquire(&qdir).expect("re-acquire after drop");
    assert!(
        lock_path.exists(),
        "lock file must still exist after re-acquire"
    );
}
