//! Cross-process advisory lock for write-heavy index phases.
//!
//! `RepoLock` wraps a per-repository lock file at `<qartez_dir>/index.lock` and
//! uses an OS-level advisory file lock (`fs4::fs_std::FileExt`) to serialize
//! concurrent qartez processes that would otherwise race against the same
//! `.qartez/index.db`. Even with `journal_mode=WAL`, SQLite still serializes
//! writers via the database header lock, so two qartez processes performing
//! a `full_index_multi` (or PageRank, co-change, or WAL checkpoint) in
//! parallel can still surface `SQLITE_BUSY`. This lock pre-serializes the
//! writers above the SQLite layer, leaving read-only MCP serving free to
//! continue (the lock is only held around explicitly write-heavy phases).
//!
//! The lock is **advisory**: only programs that opt-in (i.e., other qartez
//! processes calling into this module) honour it. External writers to the
//! database file would not be blocked. This matches Cargo's `flock.rs`
//! pattern and is the recommended approach for cross-process coordination
//! between cooperating instances of the same binary.
//!
//! On acquire, the holder writes its PID into the lock file so peers can
//! surface a meaningful "held by PID N" message after the deadline expires.
//! On drop, the OS releases the file lock; the lock file itself is left on
//! disk (this is intentional - removing it on drop creates a TOCTOU race
//! where another process can acquire its own lock on a different inode
//! concurrently).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;

/// Default total deadline for `RepoLock::acquire`. Long enough to cover a
/// full re-index of a moderately-sized repository on the holder side, short
/// enough that a stuck holder surfaces a clear error rather than blocking
/// indefinitely. Callers can override via `acquire_with_deadline`.
pub const DEFAULT_ACQUIRE_DEADLINE: Duration = Duration::from_secs(30);

/// Default short deadline for `RepoLock::try_acquire_briefly`, used by the
/// file watcher. Watcher events fire on every save; piling them up behind a
/// long-running indexer would balloon memory and produce stale events.
/// 2 seconds is enough to clear a small concurrent write phase but short
/// enough to skip with a log message when a full index is in progress.
pub const WATCHER_BRIEF_DEADLINE: Duration = Duration::from_secs(2);

const LOCK_FILE_NAME: &str = "index.lock";
// PID is stored in a sibling file rather than inside the lock file itself.
// On Windows an exclusive byte-range lock denies all other handles read
// access to the locked region, so a peer trying to read the holder PID
// from the lock file would get `Access is denied`. The sibling .pid file
// is never locked, so any process (including the holder, in single-process
// tests) can read it freely.
const PID_FILE_NAME: &str = "index.lock.pid";
const PID_TMP_FILE_NAME: &str = "index.lock.pid.tmp";
const INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BACKOFF: Duration = Duration::from_millis(1000);

/// Errors returned by [`RepoLock::acquire`] and friends.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another qartez process holds the lock and the deadline expired before
    /// it was released. `holder_pid` is the PID recorded in the lock file at
    /// the time of the last failed attempt, or `None` if the file was empty
    /// or unreadable.
    #[error(
        "Another qartez process is indexing this repo (held by PID {holder_pid_display}, waited {elapsed_ms} ms). Try again or stop the other process."
    )]
    Busy {
        holder_pid: Option<u32>,
        holder_pid_display: String,
        elapsed_ms: u128,
    },

    /// IO error opening, reading, or writing the lock file. Distinct from
    /// `Busy` so callers can distinguish "another holder" from "filesystem
    /// is broken".
    #[error("Lock file IO error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl LockError {
    fn busy(holder_pid: Option<u32>, elapsed: Duration) -> Self {
        let holder_pid_display = match holder_pid {
            Some(pid) => pid.to_string(),
            None => "unknown".to_string(),
        };
        LockError::Busy {
            holder_pid,
            holder_pid_display,
            elapsed_ms: elapsed.as_millis(),
        }
    }
}

/// RAII guard for an exclusive cross-process lock on a qartez repository.
/// Drop releases the OS-level advisory lock.
#[derive(Debug)]
pub struct RepoLock {
    file: File,
    /// Path to the lock file. Carried for diagnostics (test assertions and
    /// future log messages) and to keep the originating directory pinned
    /// across the guard's lifetime.
    #[allow(dead_code)]
    path: PathBuf,
}

impl RepoLock {
    /// Acquire the lock with the [`DEFAULT_ACQUIRE_DEADLINE`] total budget.
    /// Retries with bounded exponential backoff between attempts.
    pub fn acquire(qartez_dir: &Path) -> Result<Self, LockError> {
        Self::acquire_with_deadline(qartez_dir, DEFAULT_ACQUIRE_DEADLINE)
    }

    /// Try once without blocking. Returns `Ok(Some(lock))` if acquired,
    /// `Ok(None)` if another process holds it, or `Err` for real IO errors.
    /// Equivalent to a zero-millisecond deadline but distinguishes "busy"
    /// (no error) from "filesystem broken" (error).
    pub fn try_acquire(qartez_dir: &Path) -> Result<Option<Self>, LockError> {
        let path = lock_path(qartez_dir);
        let file = open_lock_file(&path)?;
        match file.try_lock_exclusive() {
            Ok(true) => {
                write_pid(qartez_dir)?;
                Ok(Some(RepoLock { file, path }))
            }
            Ok(false) => Ok(None),
            Err(e) => Err(LockError::Io {
                path: path.clone(),
                source: e,
            }),
        }
    }

    /// Try to acquire with a short deadline. Returns `Ok(Some(lock))` on
    /// success, `Ok(None)` if the deadline expired with the lock still held
    /// elsewhere. Used by the watcher to skip-with-log during long indexing
    /// runs rather than queueing every file-save event.
    pub fn try_acquire_briefly(qartez_dir: &Path) -> Result<Option<Self>, LockError> {
        match Self::acquire_with_deadline(qartez_dir, WATCHER_BRIEF_DEADLINE) {
            Ok(lock) => Ok(Some(lock)),
            Err(LockError::Busy { .. }) => Ok(None),
            Err(other) => Err(other),
        }
    }

    /// Acquire with an explicit total deadline. Implements bounded
    /// exponential backoff: starts at 100 ms, doubles each retry, caps at
    /// 1 s, and surfaces [`LockError::Busy`] once `deadline` elapses. The
    /// backoff schedule is intentionally coarse-grained; SQLite's own
    /// `busy_timeout=5000` already covers the sub-second contention window
    /// once we hold the file lock and start writing.
    pub fn acquire_with_deadline(qartez_dir: &Path, deadline: Duration) -> Result<Self, LockError> {
        let path = lock_path(qartez_dir);
        let file = open_lock_file(&path)?;
        let start = Instant::now();
        let mut backoff = INITIAL_BACKOFF;

        loop {
            match file.try_lock_exclusive() {
                Ok(true) => {
                    write_pid(qartez_dir)?;
                    return Ok(RepoLock { file, path });
                }
                Ok(false) => {
                    let elapsed = start.elapsed();
                    if elapsed >= deadline {
                        let holder_pid = read_pid(qartez_dir);
                        return Err(LockError::busy(holder_pid, elapsed));
                    }
                    let remaining = deadline.saturating_sub(elapsed);
                    let sleep_for = backoff.min(remaining);
                    std::thread::sleep(sleep_for);
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
                Err(e) => {
                    return Err(LockError::Io {
                        path: path.clone(),
                        source: e,
                    });
                }
            }
        }
    }

    /// Path to the lock file backing this guard. Test-only helper; exposed
    /// because the integration test asserts cleanup behaviour.
    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        // The OS releases the advisory lock when the file descriptor closes.
        // We deliberately do NOT remove the lock file: a delete-on-drop
        // creates a TOCTOU race where another process opens a different
        // inode and gets its own lock concurrently. Cargo's `flock.rs` and
        // git's `.lock` files follow the same leave-on-disk convention.
        let _ = FileExt::unlock(&self.file);
    }
}

fn lock_path(qartez_dir: &Path) -> PathBuf {
    qartez_dir.join(LOCK_FILE_NAME)
}

fn pid_path(qartez_dir: &Path) -> PathBuf {
    qartez_dir.join(PID_FILE_NAME)
}

fn open_lock_file(path: &Path) -> Result<File, LockError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| LockError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| LockError::Io {
            path: path.to_path_buf(),
            source: e,
        })
}

// Atomic write via tmp + rename. The destination is a SIDE file (`index.lock.pid`),
// not the lock file itself, so peers can read the PID without colliding with the
// holder's exclusive byte-range lock - which on Windows would deny read access.
fn write_pid(qartez_dir: &Path) -> Result<(), LockError> {
    let pid = std::process::id();
    let final_path = pid_path(qartez_dir);
    let tmp_path = qartez_dir.join(PID_TMP_FILE_NAME);
    {
        let mut tmp = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(|e| LockError::Io {
                path: tmp_path.clone(),
                source: e,
            })?;
        write!(tmp, "{pid}").map_err(|e| LockError::Io {
            path: tmp_path.clone(),
            source: e,
        })?;
        tmp.flush().map_err(|e| LockError::Io {
            path: tmp_path.clone(),
            source: e,
        })?;
    }
    // On Windows `rename` fails if the destination exists; remove it first.
    // Unix `rename` is atomic-replace, so this branch is harmless there but
    // unnecessary - we only run it for cross-platform parity.
    if final_path.exists() {
        let _ = std::fs::remove_file(&final_path);
    }
    std::fs::rename(&tmp_path, &final_path).map_err(|e| LockError::Io {
        path: final_path.clone(),
        source: e,
    })?;
    Ok(())
}

fn read_pid(qartez_dir: &Path) -> Option<u32> {
    std::fs::read_to_string(pid_path(qartez_dir))
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn acquire_creates_lock_file_and_records_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _lock = RepoLock::acquire(dir.path()).expect("acquire must succeed");
        let p = lock_path(dir.path());
        assert!(p.exists(), "lock file must exist after acquire");
        let pid = read_pid(dir.path()).expect("pid must be parseable from sidecar file");
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn try_acquire_returns_none_when_held() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _held = RepoLock::acquire(dir.path()).expect("first acquire");
        let second = RepoLock::try_acquire(dir.path()).expect("try_acquire must not error");
        assert!(second.is_none(), "second try_acquire must observe Busy");
    }

    #[test]
    fn drop_releases_lock_for_next_acquirer() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let _held = RepoLock::acquire(dir.path()).expect("first acquire");
        }
        let second = RepoLock::try_acquire(dir.path()).expect("try_acquire must not error");
        assert!(second.is_some(), "drop must release the lock");
    }

    #[test]
    fn acquire_with_deadline_times_out_with_busy_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _held = RepoLock::acquire(dir.path()).expect("first acquire");
        let err = RepoLock::acquire_with_deadline(dir.path(), Duration::from_millis(150))
            .expect_err("must time out while held");
        match err {
            LockError::Busy { holder_pid, .. } => {
                assert_eq!(
                    holder_pid,
                    Some(std::process::id()),
                    "busy error must report the holding pid"
                );
            }
            other => panic!("expected Busy, got {other:?}"),
        }
    }

    #[test]
    fn try_acquire_briefly_returns_none_when_held() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _held = RepoLock::acquire(dir.path()).expect("first acquire");
        let result =
            RepoLock::try_acquire_briefly(dir.path()).expect("try_acquire_briefly must not error");
        assert!(result.is_none(), "brief deadline must surface as None");
    }

    #[test]
    fn concurrent_acquire_serializes_holders() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(4));
        let counter = Arc::new(std::sync::Mutex::new(0u32));
        let max_concurrent = Arc::new(std::sync::Mutex::new(0u32));

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                let counter = Arc::clone(&counter);
                let max_concurrent = Arc::clone(&max_concurrent);
                thread::spawn(move || {
                    barrier.wait();
                    let _lock = RepoLock::acquire_with_deadline(&path, Duration::from_secs(10))
                        .expect("acquire must succeed within deadline");
                    {
                        let mut c = counter.lock().expect("counter mutex");
                        *c += 1;
                        let mut m = max_concurrent.lock().expect("max mutex");
                        *m = (*m).max(*c);
                    }
                    thread::sleep(Duration::from_millis(50));
                    {
                        let mut c = counter.lock().expect("counter mutex");
                        *c -= 1;
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread join");
        }

        let observed = *max_concurrent.lock().expect("max mutex");
        assert_eq!(
            observed, 1,
            "advisory lock must serialize holders (saw {observed} concurrent)"
        );
    }
}
