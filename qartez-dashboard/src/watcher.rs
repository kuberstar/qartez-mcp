//! Notify-based file watcher.
//!
//! M1 scope: watch the project root, coalesce events, and broadcast
//! `Event::FileChanged` to subscribed WebSocket clients.
//! Incremental reindex pipeline lands in M3 (`indexer::Indexer`).

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{RecursiveMode, Watcher as _};
use tokio::sync::mpsc;

use crate::state::{AppState, Event};

/// Inner debounce window. Coalesces duplicate save events from editors that
/// emit "modify metadata" + "modify data" + "create" + "rename" in quick
/// succession (every editor on macOS, basically).
const DEBOUNCE_MS: u64 = 50;

/// Upper bound on how long one batch may keep growing while events keep
/// arriving inside the quiet window, so a steady trickle cannot postpone the
/// broadcast forever.
const DEBOUNCE_MAX_WAIT_MS: u64 = 1_000;

/// Spawn the watcher in the background. Returns the join handle so the
/// caller can await it during graceful shutdown.
pub fn spawn(state: AppState) -> Result<()> {
    let root = state.project_root().to_path_buf();
    let events = state.events();
    let shutdown = state.shutdown();

    let (tx, mut rx) = mpsc::channel::<Vec<PathBuf>>(256);

    std::thread::Builder::new()
        .name("qartez-watcher".into())
        .spawn(move || {
            if let Err(error) = run_blocking(&root, tx) {
                tracing::error!(?error, "watcher.thread.failed");
            }
        })
        .context("spawning watcher thread")?;

    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                maybe = rx.recv() => {
                    let Some(paths) = maybe else { break };
                    let payload = paths
                        .into_iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                    tracing::info!(count = payload.len(), "watcher.batch");
                    let _ = events.send(Event::FileChanged { paths: payload });
                }
            }
        }
    });

    Ok(())
}

/// Run the watcher until the batch receiver is dropped.
///
/// Ignored paths are dropped in the raw notify callback, before anything is
/// queued. Filtering after a debouncer meant every event under a churning
/// build tree (`target/`, `DerivedData/`) still went through its queue and
/// file-id cache, which cost most of a core on macOS (issue #52).
fn run_blocking(root: &Path, tx: mpsc::Sender<Vec<PathBuf>>) -> Result<()> {
    let filter = IgnoreFilter::for_root(root);
    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<PathBuf>();

    let mut watcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
            Ok(event) => {
                for path in event.paths {
                    if !filter.is_ignored(&path) && raw_tx.send(path).is_err() {
                        return;
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "watcher.event.error"),
        })
        .context("creating watcher")?;

    watcher
        .watch(root, RecursiveMode::Recursive)
        .with_context(|| format!("watching {}", root.display()))?;

    tracing::info!(root = %root.display(), "watcher.started");

    coalesce(
        &raw_rx,
        &tx,
        Duration::from_millis(DEBOUNCE_MS),
        Duration::from_millis(DEBOUNCE_MAX_WAIT_MS),
    );
    drop(watcher);
    Ok(())
}

/// Collect paths into sorted, de-duplicated batches. A batch is sent once no
/// path has arrived for `quiet`, or once it has been open for `max_wait`.
/// Returns when the watcher's sender or the batch receiver goes away.
fn coalesce(
    raw_rx: &Receiver<PathBuf>,
    tx: &mpsc::Sender<Vec<PathBuf>>,
    quiet: Duration,
    max_wait: Duration,
) {
    while let Ok(first) = raw_rx.recv() {
        let opened = Instant::now();
        let mut batch = BTreeSet::from([first]);
        let mut disconnected = false;
        while let Some(left) = max_wait.checked_sub(opened.elapsed()) {
            match raw_rx.recv_timeout(quiet.min(left)) {
                Ok(path) => {
                    batch.insert(path);
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if tx.blocking_send(batch.into_iter().collect()).is_err() || disconnected {
            return;
        }
    }
}

/// Path filter for the watcher, mirroring the gitignore sources consumed by
/// `walker::walk_source_files` so the initial scan and the live event stream
/// stay consistent.
struct IgnoreFilter {
    root: PathBuf,
    local: Gitignore,
    global: Gitignore,
}

impl IgnoreFilter {
    fn for_root(root: &Path) -> Self {
        let mut builder = GitignoreBuilder::new(root);
        // Missing files are expected (many projects have no `.qartezignore`
        // or no `.git/info/exclude`); the builder simply keeps going.
        let _ = builder.add(root.join(".gitignore"));
        let _ = builder.add(root.join(".git").join("info").join("exclude"));
        let _ = builder.add(root.join(".qartezignore"));
        // `core.excludesfile` overrides the XDG default in git itself, but
        // `ignore::Gitignore::global()` only sees one of the two sources.
        // Read the path explicitly so users with both files configured stay
        // consistent with the initial walker scan.
        if let Some(path) = excludesfile_from_git_config() {
            let _ = builder.add(path);
        }
        let local = builder.build().unwrap_or_else(|_| Gitignore::empty());
        // The global builder reads `$XDG_CONFIG_HOME/git/ignore` when present;
        // we keep it as a second matcher so XDG-only setups still work without
        // an explicit `core.excludesfile`. It is rooted at the project, not at
        // the cwd as `Gitignore::global()` would be, so every watched path is
        // under its root.
        let (global, _) = GitignoreBuilder::new(root).build_global();
        Self {
            root: root.to_path_buf(),
            local,
            global,
        }
    }

    /// A path outside the project root counts as ignored: it is never part of
    /// the project, and `matched_path_or_any_parents` panics on it, which
    /// inside the notify FFI callback would abort the daemon.
    fn is_ignored(&self, path: &Path) -> bool {
        // Hard-skip mirrors `walker::walk_source_files` `filter_entry`:
        // `.git` and `.qartez` are tool-cache directories never useful to
        // index, even if they are absent from every gitignore source.
        for component in path.components() {
            if let Component::Normal(name) = component
                && matches!(name.to_str(), Some(".git") | Some(".qartez"))
            {
                return true;
            }
        }
        if !path.starts_with(&self.root) {
            return true;
        }
        let is_dir = path.is_dir();
        self.local
            .matched_path_or_any_parents(path, is_dir)
            .is_ignore()
            || self
                .global
                .matched_path_or_any_parents(path, is_dir)
                .is_ignore()
    }
}

fn excludesfile_from_git_config() -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["config", "--global", "core.excludesfile"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // `git config` returns the literal string set in the config, including
    // a leading `~/`. Expand it so `GitignoreBuilder::add` can open the file.
    if let Some(rest) = trimmed.strip_prefix("~/") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(rest))
    } else {
        Some(PathBuf::from(trimmed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    /// A path outside the project root must read as ignored rather than
    /// panic inside `matched_path_or_any_parents`: the filter runs in the
    /// notify FFI callback, where a panic aborts the daemon.
    #[test]
    fn filter_treats_paths_outside_root_as_ignored_without_panicking() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        let filter = IgnoreFilter::for_root(&root);
        assert!(filter.is_ignored(&tmp.path().join("elsewhere/src/lib.rs")));
        assert!(!filter.is_ignored(&root.join("src/lib.rs")));
        assert!(filter.is_ignored(&root.join("target/debug/x.rs")));
    }

    #[test]
    fn coalesce_sorts_and_dedups_one_burst() {
        let (raw_tx, raw_rx) = std::sync::mpsc::channel();
        let (tx, mut rx) = mpsc::channel(8);
        for p in ["b.rs", "a.rs", "b.rs"] {
            raw_tx.send(PathBuf::from(p)).unwrap();
        }
        drop(raw_tx);
        coalesce(
            &raw_rx,
            &tx,
            Duration::from_millis(30),
            Duration::from_secs(5),
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")]
        );
        assert!(rx.try_recv().is_err(), "one burst must be one batch");
    }

    #[test]
    fn coalesce_flushes_at_max_wait_under_a_trickle() {
        let (raw_tx, raw_rx) = std::sync::mpsc::channel::<PathBuf>();
        let (tx, mut rx) = mpsc::channel(64);
        let producer = thread::spawn(move || {
            for i in 0..30 {
                if raw_tx.send(PathBuf::from(format!("{i}.rs"))).is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        coalesce(
            &raw_rx,
            &tx,
            Duration::from_millis(100),
            Duration::from_millis(200),
        );
        producer.join().unwrap();
        let mut batches = 0;
        let mut paths = 0;
        while let Ok(b) = rx.try_recv() {
            batches += 1;
            paths += b.len();
        }
        assert_eq!(paths, 30, "no path may be lost across flushes");
        assert!(batches >= 2, "max_wait must force a flush, got {batches}");
    }

    /// Writes under a gitignored directory must never reach a batch, while a
    /// real source edit still does.
    #[test]
    fn ignored_paths_are_dropped_before_batching() {
        let tmp = tempfile::TempDir::new().unwrap();
        // FSEvents reports `/private/var/...` for `/var/...`.
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join(".gitignore"), "DerivedData/\n").unwrap();
        std::fs::create_dir_all(root.join("DerivedData/Build")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();

        let (tx, mut rx) = mpsc::channel(64);
        let thread_root = root.clone();
        thread::spawn(move || run_blocking(&thread_root, tx));
        thread::sleep(Duration::from_millis(300));

        for i in 0..50 {
            std::fs::write(root.join(format!("DerivedData/Build/o{i}.o")), "x").unwrap();
        }
        std::fs::write(root.join("src/lib.rs"), "pub fn a() {}\n").unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen: Vec<PathBuf> = Vec::new();
        while Instant::now() < deadline && !seen.iter().any(|p| p.ends_with("src/lib.rs")) {
            match rx.try_recv() {
                Ok(batch) => seen.extend(batch),
                Err(_) => thread::sleep(Duration::from_millis(20)),
            }
        }
        // Dropping the receiver lets the watcher thread exit on its next send.
        drop(rx);
        assert!(
            seen.iter().any(|p| p.ends_with("src/lib.rs")),
            "the source edit must be broadcast, saw {seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|p| p.components().any(|c| c.as_os_str() == "DerivedData")),
            "ignored paths must never be batched, saw {seen:?}"
        );
    }
}
