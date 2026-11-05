use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{EventKind, RecursiveMode, Watcher as _};
use rusqlite::Connection;
use tokio::sync::mpsc;

use crate::graph;
use crate::index;
use crate::index::languages;
use crate::lock::RepoLock;

const QARTEZIGNORE_FILENAME: &str = ".qartezignore";
const GITIGNORE_FILENAME: &str = ".gitignore";

/// Quiet window of the watcher's own debouncer. Coalesces the burst of
/// `ReadDirectoryChangesW` / `inotify` / FSEvents events that every editor
/// emits per save (metadata + content + close, plus the macOS coalesced
/// rename pair) so one logical save yields one batch instead of three or
/// four.
const DEBOUNCE_MS: u64 = 200;

/// Upper bound on how long a pending batch may keep growing while events
/// keep arriving inside the quiet window. Without it a steady trickle of
/// indexable writes would postpone the re-index forever.
const DEBOUNCE_MAX_WAIT_MS: u64 = 2_000;

/// Outer batch drain window. After a debounced batch arrives, this loop
/// keeps draining additional batches that land inside the same window so
/// a multi-file save (formatter, codemod) collapses into a single
/// re-index cycle.
const BATCH_DRAIN_MS: u64 = 500;

/// Minimum interval between PageRank recomputes inside the watcher.
/// PageRank iterates the full edge graph and is the main contributor to
/// per-save latency on large repos. Skipping it temporarily only leaves
/// the ranks slightly stale - readers see the last published values
/// until the next eligible recompute.
const PAGERANK_MIN_INTERVAL_MS: u64 = 30_000;

/// Backstop on the number of consecutive incremental batches that may
/// skip the PageRank refresh. Caps staleness during long bursts of saves
/// where the time-based gate never fires alone.
const PAGERANK_MAX_BATCHES: u64 = 32;

/// Minimum interval between full `wal_checkpoint(TRUNCATE)` calls in the
/// watcher path. Per-save TRUNCATE on Windows hits NTFS fsync + Defender
/// and dominates the re-index time; the per-batch path runs a non-blocking
/// PASSIVE checkpoint instead and lets the periodic TRUNCATE here recover
/// disk space.
const WAL_TRUNCATE_MIN_INTERVAL_MS: u64 = 60_000;

/// How often the ignore-cache rechecks the mtimes of the local ignore
/// files. NTFS stat is slow under AV, so checking on every event
/// dominates the cost of small batches. Hot-reload stays responsive
/// because user edits to `.gitignore` arrive within seconds of the next
/// save anyway.
const IGNORE_REFRESH_MIN_INTERVAL_MS: u64 = 2_000;

/// A batch of filesystem events, separated into changed (created/modified)
/// and deleted paths so the incremental indexer can handle them differently.
struct WatchBatch {
    changed: Vec<PathBuf>,
    deleted: Vec<PathBuf>,
}

pub struct Watcher {
    db: Arc<Mutex<Connection>>,
    project_root: PathBuf,
    /// Path prefix to prepend to each file's relative path when writing
    /// index rows. Must match the prefix `full_index_multi` used for this
    /// root (empty in single-root mode). Without it, incremental rows in
    /// multi-root projects would orphan the original full-index rows.
    path_prefix: String,
    /// Directory hosting the cross-process index lock file. When set,
    /// `reindex` acquires the lock with a short deadline and skips with a
    /// log message if another qartez process holds it. When `None`, the
    /// watcher writes without coordination (used by tests that drive
    /// indexing through an in-memory connection only).
    lock_dir: Option<PathBuf>,
    /// Cadence state for the debounced PageRank refresh and the periodic
    /// `wal_checkpoint(TRUNCATE)`. Shared between calls so successive
    /// re-indexes coordinate even though `reindex` itself is `&self`.
    cadence: Mutex<WatcherCadence>,
}

struct WatcherCadence {
    last_pagerank: Option<Instant>,
    batches_since_pagerank: u64,
    last_wal_truncate: Option<Instant>,
}

#[derive(Debug, PartialEq, Eq)]
struct CadenceDecision {
    run_pagerank: bool,
    run_truncate: bool,
}

impl WatcherCadence {
    fn new() -> Self {
        Self {
            last_pagerank: None,
            batches_since_pagerank: 0,
            last_wal_truncate: None,
        }
    }

    /// Decide whether this batch should run PageRank and/or a TRUNCATE
    /// checkpoint, then update the state. Pure with respect to `now`,
    /// which lets the unit tests drive synthetic timelines.
    fn tick(
        &mut self,
        now: Instant,
        pagerank_interval_ms: u64,
        pagerank_max_batches: u64,
        truncate_interval_ms: u64,
    ) -> CadenceDecision {
        self.batches_since_pagerank = self.batches_since_pagerank.saturating_add(1);

        let due_by_time = self
            .last_pagerank
            .is_none_or(|t| now.duration_since(t).as_millis() as u64 >= pagerank_interval_ms);
        let due_by_batches = self.batches_since_pagerank >= pagerank_max_batches;
        let run_pagerank = due_by_time || due_by_batches;
        if run_pagerank {
            self.last_pagerank = Some(now);
            self.batches_since_pagerank = 0;
        }

        let run_truncate = self
            .last_wal_truncate
            .is_none_or(|t| now.duration_since(t).as_millis() as u64 >= truncate_interval_ms);
        if run_truncate {
            self.last_wal_truncate = Some(now);
        }

        CadenceDecision {
            run_pagerank,
            run_truncate,
        }
    }
}

impl Watcher {
    pub fn new(db: Arc<Mutex<Connection>>, project_root: PathBuf) -> Self {
        Self::with_prefix(db, project_root, String::new())
    }

    pub fn with_prefix(
        db: Arc<Mutex<Connection>>,
        project_root: PathBuf,
        path_prefix: String,
    ) -> Self {
        Self {
            db,
            project_root,
            path_prefix,
            lock_dir: None,
            cadence: Mutex::new(WatcherCadence::new()),
        }
    }

    /// Set the directory hosting the cross-process index lock. The watcher
    /// will acquire the lock briefly before each re-index and skip the
    /// cycle if another qartez process is already writing.
    pub fn with_lock_dir(mut self, lock_dir: PathBuf) -> Self {
        self.lock_dir = Some(lock_dir);
        self
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let supported_ext: HashSet<&str> = languages::supported_extensions().into_iter().collect();
        let supported_names: HashSet<&str> = languages::supported_filenames().into_iter().collect();
        let supported_prefixes: Vec<&str> = languages::supported_prefixes();

        let (tx, mut rx) = mpsc::channel::<WatchBatch>(64);

        let project_root = self.project_root.clone();
        let _watcher = start_notify_watcher(
            project_root.clone(),
            supported_ext,
            supported_names,
            supported_prefixes,
            tx,
        )?;

        tracing::info!("file watcher active on {}", self.project_root.display());

        loop {
            let batch = match rx.recv().await {
                Some(b) => b,
                None => break,
            };

            let mut changed = batch.changed;
            let mut deleted = batch.deleted;

            // The debounce thread already collapses per-save bursts.
            // A short outer drain still folds successive batches that
            // arrive while a multi-file save (formatter, codemod) is
            // landing, so the re-index runs once per logical unit of
            // work instead of once per debounced event.
            while let Ok(Some(more)) =
                tokio::time::timeout(Duration::from_millis(BATCH_DRAIN_MS), rx.recv()).await
            {
                changed.extend(more.changed);
                deleted.extend(more.deleted);
            }

            changed.sort();
            changed.dedup();
            deleted.sort();
            deleted.dedup();
            // A file that was deleted then re-created within the same batch
            // should only appear in `changed`.
            deleted.retain(|p| !changed.contains(p));

            let total = changed.len() + deleted.len();
            tracing::info!(
                "watcher: {total} events ({} changed, {} deleted), re-indexing",
                changed.len(),
                deleted.len(),
            );

            if let Err(e) = self.reindex(&changed, &deleted) {
                tracing::error!("re-index after watch event failed: {e}");
            }
        }

        Ok(())
    }

    fn reindex(&self, changed: &[PathBuf], deleted: &[PathBuf]) -> anyhow::Result<()> {
        // Acquire the cross-process lock briefly. If another qartez process
        // is in the middle of a full index, skip this cycle rather than
        // pile up watcher events behind a multi-second writer. The next
        // file save will retry, and `incremental_index` is idempotent over
        // the actual on-disk state, so missing one cycle does not lose
        // information - it just defers the index update.
        let _index_lock = if let Some(dir) = self.lock_dir.as_ref() {
            match RepoLock::try_acquire_briefly(dir) {
                Ok(Some(g)) => Some(g),
                Ok(None) => {
                    tracing::info!(
                        "watcher: another qartez process is indexing; skipping this batch"
                    );
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!("watcher: lock IO error, proceeding without lock: {e}");
                    None
                }
            }
        } else {
            None
        };

        // Mirror the `into_inner()` recovery already used by the ignore-cache
        // lock at start_notify_watcher: a poisoned db mutex means a prior
        // indexing operation panicked mid-way, but the Connection is still
        // usable (sqlite rolls the open transaction back when the guard drops).
        // Panicking here would kill the watcher task for the rest of the
        // session - a long-running background loop should recover from a
        // one-off parse or encode panic instead of going silent.
        let conn = match self.db.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::warn!("watcher db mutex was poisoned; recovering");
                poisoned.into_inner()
            }
        };
        index::incremental_index_with_prefix(
            &conn,
            &self.project_root,
            &self.path_prefix,
            changed,
            deleted,
        )?;

        let now = Instant::now();
        let decision = {
            let mut cadence = match self.cadence.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            cadence.tick(
                now,
                PAGERANK_MIN_INTERVAL_MS,
                PAGERANK_MAX_BATCHES,
                WAL_TRUNCATE_MIN_INTERVAL_MS,
            )
        };

        if decision.run_pagerank {
            graph::pagerank::compute_pagerank(&conn, &Default::default())?;
            graph::pagerank::compute_symbol_pagerank(&conn, &Default::default())?;
        } else {
            tracing::debug!(
                "watcher: PageRank deferred; will recompute after {PAGERANK_MIN_INTERVAL_MS}ms or {PAGERANK_MAX_BATCHES} batches"
            );
        }

        // We pay the TRUNCATE cost at most once per `WAL_TRUNCATE_MIN_INTERVAL_MS`
        // here, falling back to a non-blocking PASSIVE checkpoint in between
        // to keep the WAL from drifting unbounded. `incremental_index_with_prefix`
        // also runs a PASSIVE itself; the back-to-back call is idempotent and
        // dominated by the disk-side work it skipped relative to the previous
        // TRUNCATE every save.
        let checkpoint_sql = if decision.run_truncate {
            "PRAGMA wal_checkpoint(TRUNCATE);"
        } else {
            "PRAGMA wal_checkpoint(PASSIVE);"
        };
        if let Err(e) = conn.execute_batch(checkpoint_sql) {
            tracing::debug!("watcher WAL checkpoint failed (non-fatal): {e}");
        }

        Ok(())
    }
}

/// Local ignore-source paths whose mtimes are tracked for hot-reload. The
/// XDG-global gitignore and `core.excludesfile` are not tracked because they
/// rarely change during a watcher session and rebuilding `Gitignore::global()`
/// on every event would dominate the cost of small batches.
fn local_ignore_sources(root: &Path) -> [PathBuf; 3] {
    [
        root.join(QARTEZIGNORE_FILENAME),
        root.join(GITIGNORE_FILENAME),
        root.join(".git").join("info").join("exclude"),
    ]
}

/// Resolve the global ignore file path. Prefers `core.excludesfile`, falls
/// back to `$XDG_CONFIG_HOME/git/ignore` (or `~/.config/git/ignore`), which
/// matches what `git` itself reads.
fn global_ignore_path() -> Option<PathBuf> {
    if let Some(p) = excludesfile_from_git_config() {
        return Some(p);
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let candidate = PathBuf::from(xdg).join("git").join("ignore");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home)
            .join(".config")
            .join("git")
            .join("ignore");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Build the combined matcher from `.gitignore`, `.git/info/exclude`,
/// `.qartezignore`, and the resolved global ignore file. All sources are
/// added through one `GitignoreBuilder` rooted at the project root, so
/// `matched_path_or_any_parents` works on every path under the project
/// regardless of where the project lives on disk. Mirrors the dashboard
/// watcher's `IgnoreFilter::for_root` so the MCP and dashboard event streams
/// stay consistent with the indexer's initial walker scan.
fn build_local_ignore(root: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root);
    for path in local_ignore_sources(root) {
        if let Some(e) = builder.add(&path) {
            tracing::warn!(path = %path.display(), error = %e, "partial parse of ignore file");
        }
    }
    if let Some(path) = global_ignore_path()
        && let Some(e) = builder.add(&path)
    {
        tracing::warn!(path = %path.display(), error = %e, "partial parse of global ignore file");
    }
    builder.build().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "failed to build combined gitignore matcher; falling back to empty");
        Gitignore::empty()
    })
}

/// Hot-reload wrapper for the combined ignore matcher. Holds the matcher
/// together with the mtimes that were observed when it was built, so the
/// closure can refresh the cache after the user edits any local ignore file
/// during a live watcher session (rather than requiring a full restart).
struct QartezIgnoreCache {
    /// Combined matcher built from every source resolved by
    /// `build_local_ignore`, all rooted at the project root.
    matcher: Gitignore,
    /// Mtimes of every path returned by `local_ignore_sources`, in the same
    /// order. `None` means the file did not exist when last observed. The
    /// global ignore file is not tracked because it rarely changes during a
    /// watcher session.
    mtimes: [Option<SystemTime>; 3],
    /// Last time the cache stat'd the ignore sources. The refresh is
    /// throttled because `std::fs::metadata` is expensive on Windows
    /// under Defender and a 1k-event burst would otherwise cost 3k
    /// syscalls.
    last_check: Option<Instant>,
}

impl QartezIgnoreCache {
    fn new(root: &Path) -> Self {
        let sources = local_ignore_sources(root);
        let mtimes = [
            fs_mtime(&sources[0]),
            fs_mtime(&sources[1]),
            fs_mtime(&sources[2]),
        ];
        Self {
            matcher: build_local_ignore(root),
            mtimes,
            last_check: None,
        }
    }

    fn refresh_if_changed(&mut self, root: &Path) {
        let now = Instant::now();
        if let Some(last) = self.last_check
            && (now.duration_since(last).as_millis() as u64) < IGNORE_REFRESH_MIN_INTERVAL_MS
        {
            return;
        }
        self.last_check = Some(now);

        let sources = local_ignore_sources(root);
        let current = [
            fs_mtime(&sources[0]),
            fs_mtime(&sources[1]),
            fs_mtime(&sources[2]),
        ];
        if current != self.mtimes {
            self.matcher = build_local_ignore(root);
            self.mtimes = current;
        }
    }

    /// True if the path is excluded by any ignore source, or lives inside a
    /// hard-skip directory (`.git`, `.qartez`). The hard-skip mirrors
    /// `walker::walk_source_files`'s `filter_entry` so the watcher cannot
    /// resurrect tool-cache rows the indexer already excludes.
    ///
    /// A path outside the matcher root counts as ignored: it is never
    /// indexable, and `matched_path_or_any_parents` panics on it, which inside
    /// the notify FFI callback would abort the server.
    fn is_ignored(&self, path: &Path) -> bool {
        for component in path.components() {
            if let Component::Normal(name) = component
                && matches!(name.to_str(), Some(".git") | Some(".qartez"))
            {
                return true;
            }
        }
        if !path.starts_with(self.matcher.path()) {
            return true;
        }
        self.matcher
            .matched_path_or_any_parents(path, path.is_dir())
            .is_ignore()
    }
}

/// Resolve `core.excludesfile` from the user's git config. Expanding `~/`
/// makes the path usable by `GitignoreBuilder::add`, which opens it directly
/// without going through a shell.
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
    if let Some(rest) = trimmed.strip_prefix("~/") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(rest))
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn fs_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Test whether `path` looks like a source file the indexer can consume.
/// Mirrors the walker's three-tier match (known extension, known name,
/// known prefix) so the watcher does not silently drop `Makefile`,
/// `Dockerfile`, `CMakeLists.txt` and friends.
fn is_indexable_path(
    p: &Path,
    exts: &HashSet<&str>,
    names: &HashSet<&str>,
    prefixes: &[&str],
) -> bool {
    if let Some(ext) = p.extension().and_then(|e| e.to_str())
        && exts.contains(ext)
    {
        return true;
    }
    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
        if names.contains(name) {
            return true;
        }
        if prefixes.iter().any(|pre| name.starts_with(pre)) {
            return true;
        }
    }
    false
}

/// Start the recursive OS watcher on `root` and the debounce thread behind it.
///
/// Raw events are filtered in the notify callback itself, before anything is
/// queued: a path that is not indexable or is ignored (`.gitignore`,
/// `.qartezignore`, hard-skipped `.git` / `.qartez`) is dropped right there.
/// A build tree such as `DerivedData/` or `target/` can emit hundreds of
/// events per second; filtering after a debouncer meant every one of them
/// still went through its queue, rename tracking and file-id cache, which
/// cost 35-90% of a core per server process on macOS.
///
/// The survivors are coalesced by [`debounce_paths`] and classified by
/// existence on disk at flush time, which also covers every rename shape the
/// backends emit (`Name::Both`, split `From`/`To`, `Any`) and the macOS case
/// where a `Remove` is coalesced into a trailing `Modify`.
fn start_notify_watcher(
    root: PathBuf,
    supported_ext: HashSet<&'static str>,
    supported_names: HashSet<&'static str>,
    supported_prefixes: Vec<&'static str>,
    tx: mpsc::Sender<WatchBatch>,
) -> anyhow::Result<notify::RecommendedWatcher> {
    let mut ignore_cache = QartezIgnoreCache::new(&root);
    let ignore_root = root.clone();
    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<PathBuf>();

    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let event = match result {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!("watch error: {error}");
                return;
            }
        };
        if !matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        ) {
            return;
        }

        let mut refreshed = false;
        for path in event.paths {
            // The extension test is pure string work, so it runs first
            // and rejects most build output without touching the
            // ignore matcher or the filesystem.
            if !is_indexable_path(&path, &supported_ext, &supported_names, &supported_prefixes) {
                continue;
            }
            if !refreshed {
                ignore_cache.refresh_if_changed(&ignore_root);
                refreshed = true;
            }
            if ignore_cache.is_ignored(&path) {
                continue;
            }
            if raw_tx.send(path).is_err() {
                return;
            }
        }
    })?;

    std::thread::Builder::new()
        .name("qartez-watch-debounce".into())
        .spawn(move || {
            debounce_paths(
                &raw_rx,
                &tx,
                Duration::from_millis(DEBOUNCE_MS),
                Duration::from_millis(DEBOUNCE_MAX_WAIT_MS),
            );
        })?;

    watcher.watch(&root, RecursiveMode::Recursive)?;
    Ok(watcher)
}

/// Coalesce raw paths into batches: a batch is flushed once no new path has
/// arrived for `quiet`, or once it has been open for `max_wait`. Returns when
/// the watcher drops its sender or the consumer drops the batch receiver.
fn debounce_paths(
    raw_rx: &std::sync::mpsc::Receiver<PathBuf>,
    tx: &mpsc::Sender<WatchBatch>,
    quiet: Duration,
    max_wait: Duration,
) {
    use std::sync::mpsc::RecvTimeoutError;

    while let Ok(first) = raw_rx.recv() {
        let opened = Instant::now();
        let mut pending: HashSet<PathBuf> = HashSet::from([first]);
        let mut disconnected = false;
        loop {
            let Some(left) = max_wait.checked_sub(opened.elapsed()) else {
                break;
            };
            match raw_rx.recv_timeout(quiet.min(left)) {
                Ok(path) => {
                    pending.insert(path);
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if tx.blocking_send(classify_by_existence(pending)).is_err() || disconnected {
            return;
        }
    }
}

/// Split debounced paths into changed (still on disk) and deleted (gone).
/// Sorted so a batch is deterministic regardless of event arrival order.
fn classify_by_existence(paths: HashSet<PathBuf>) -> WatchBatch {
    let mut paths: Vec<PathBuf> = paths.into_iter().collect();
    paths.sort();
    let (changed, deleted) = paths.into_iter().partition(|p| p.exists());
    WatchBatch { changed, deleted }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::thread;
    use tempfile::TempDir;

    fn ext_set() -> HashSet<&'static str> {
        let mut s = HashSet::new();
        s.insert("rs");
        s.insert("yml");
        s.insert("toml");
        s
    }

    fn name_set() -> HashSet<&'static str> {
        let mut s = HashSet::new();
        s.insert("Makefile");
        s.insert("Dockerfile");
        s.insert("CMakeLists.txt");
        s
    }

    #[test]
    fn is_indexable_path_matches_extension() {
        let exts = ext_set();
        let names = name_set();
        let prefixes: Vec<&str> = vec!["Dockerfile."];
        assert!(is_indexable_path(
            Path::new("src/lib.rs"),
            &exts,
            &names,
            &prefixes
        ));
        assert!(!is_indexable_path(
            Path::new("note.txt"),
            &exts,
            &names,
            &prefixes
        ));
    }

    #[test]
    fn is_indexable_path_matches_exact_filename() {
        let exts = ext_set();
        let names = name_set();
        let prefixes: Vec<&str> = vec!["Dockerfile."];
        // Extensionless files used to be silently dropped by the watcher.
        assert!(is_indexable_path(
            Path::new("Makefile"),
            &exts,
            &names,
            &prefixes
        ));
        assert!(is_indexable_path(
            Path::new("subdir/Dockerfile"),
            &exts,
            &names,
            &prefixes
        ));
        assert!(is_indexable_path(
            Path::new("nested/CMakeLists.txt"),
            &exts,
            &names,
            &prefixes
        ));
    }

    #[test]
    fn is_indexable_path_matches_prefix() {
        let exts = ext_set();
        let names = name_set();
        let prefixes: Vec<&str> = vec!["Dockerfile."];
        assert!(is_indexable_path(
            Path::new("Dockerfile.prod"),
            &exts,
            &names,
            &prefixes
        ));
        assert!(!is_indexable_path(
            Path::new("Dockerizer"),
            &exts,
            &names,
            &prefixes
        ));
    }

    #[test]
    fn qartezignore_cache_reloads_on_mtime_change() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let ignore_path = root.join(QARTEZIGNORE_FILENAME);

        // Start with an ignore file that excludes `generated/`.
        std::fs::write(&ignore_path, "generated/\n").unwrap();
        let mut cache = QartezIgnoreCache::new(root);

        let target_before = root.join("generated/file.rs");
        assert!(
            cache.is_ignored(&target_before),
            "initial ignore pattern should block generated/"
        );
        let target_other = root.join("other/file.rs");
        assert!(
            !cache.is_ignored(&target_other),
            "non-matching path should not be ignored initially"
        );

        // Sleep long enough that the filesystem reports a new mtime;
        // on fast SSDs the metadata resolution can be as coarse as 1s.
        thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&ignore_path, "other/\n").unwrap();

        cache.refresh_if_changed(root);
        assert!(
            cache.is_ignored(&target_other),
            "cache must have reloaded and now ignore other/"
        );
        assert!(
            !cache.is_ignored(&target_before),
            "old pattern (generated/) must be dropped after reload"
        );
    }

    #[test]
    fn qartezignore_cache_starts_empty_when_file_absent() {
        let tmp = TempDir::new().unwrap();
        let cache = QartezIgnoreCache::new(tmp.path());
        assert!(cache.mtimes.iter().all(Option::is_none));
        let any_path = tmp.path().join("anything");
        assert!(!cache.is_ignored(&any_path), "empty cache matches nothing");
    }

    #[test]
    fn qartezignore_cache_picks_up_newly_created_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut cache = QartezIgnoreCache::new(root);
        let target = root.join("vendor/dep.rs");

        assert!(!cache.is_ignored(&target));

        std::fs::write(root.join(QARTEZIGNORE_FILENAME), "vendor/\n").unwrap();
        cache.refresh_if_changed(root);

        assert!(
            cache.is_ignored(&target),
            "cache must notice a freshly-written .qartezignore"
        );
    }

    #[test]
    fn qartezignore_cache_honors_gitignore() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(root.join(GITIGNORE_FILENAME), "build/\n").unwrap();
        let cache = QartezIgnoreCache::new(root);
        let inside = root.join("build/artifact.rs");
        assert!(
            cache.is_ignored(&inside),
            ".gitignore patterns must be honored by the watcher"
        );
    }

    #[test]
    fn qartezignore_cache_hard_skips_dot_git_and_dot_qartez() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let cache = QartezIgnoreCache::new(root);
        assert!(cache.is_ignored(&root.join(".git/config")));
        assert!(cache.is_ignored(&root.join(".qartez/index.db")));
        assert!(cache.is_ignored(&root.join("nested/.git/HEAD")));
    }

    #[test]
    fn cadence_first_tick_runs_pagerank_and_truncate() {
        let mut cadence = WatcherCadence::new();
        let now = Instant::now();
        let decision = cadence.tick(now, 30_000, 32, 60_000);
        assert!(
            decision.run_pagerank,
            "first tick must run PageRank (last_pagerank is None)"
        );
        assert!(
            decision.run_truncate,
            "first tick must run TRUNCATE (last_wal_truncate is None)"
        );
        assert_eq!(cadence.batches_since_pagerank, 0);
        assert!(cadence.last_pagerank.is_some());
        assert!(cadence.last_wal_truncate.is_some());
    }

    #[test]
    fn cadence_consecutive_ticks_inside_window_skip_both() {
        let mut cadence = WatcherCadence::new();
        let t0 = Instant::now();
        cadence.tick(t0, 30_000, 32, 60_000);

        let t1 = t0 + Duration::from_millis(100);
        let decision = cadence.tick(t1, 30_000, 32, 60_000);
        assert!(
            !decision.run_pagerank,
            "tick 100ms after first must skip PageRank"
        );
        assert!(
            !decision.run_truncate,
            "tick 100ms after first must skip TRUNCATE"
        );
        assert_eq!(
            cadence.batches_since_pagerank, 1,
            "batch counter must increment when PageRank skipped"
        );
    }

    #[test]
    fn cadence_time_gate_fires_after_pagerank_interval() {
        let mut cadence = WatcherCadence::new();
        let t0 = Instant::now();
        cadence.tick(t0, 30_000, 32, 60_000);

        let t1 = t0 + Duration::from_millis(30_001);
        let decision = cadence.tick(t1, 30_000, 32, 60_000);
        assert!(
            decision.run_pagerank,
            "PageRank must fire once the time interval elapses"
        );
        assert_eq!(
            cadence.batches_since_pagerank, 0,
            "counter must reset after PageRank fires"
        );
    }

    #[test]
    fn cadence_batch_backstop_fires_before_time_gate() {
        let mut cadence = WatcherCadence::new();
        let t0 = Instant::now();
        cadence.tick(t0, 30_000, 4, 60_000);

        let mut last = None;
        for i in 1..=4 {
            let t = t0 + Duration::from_millis(i * 100);
            last = Some(cadence.tick(t, 30_000, 4, 60_000));
        }
        let decision = last.unwrap();
        assert!(
            decision.run_pagerank,
            "PageRank must fire after PAGERANK_MAX_BATCHES even when time gate not met"
        );
    }

    #[test]
    fn cadence_truncate_skipped_inside_window() {
        let mut cadence = WatcherCadence::new();
        let t0 = Instant::now();
        cadence.tick(t0, 30_000, 32, 60_000);

        let t1 = t0 + Duration::from_millis(45_000);
        let decision = cadence.tick(t1, 30_000, 32, 60_000);
        assert!(
            decision.run_pagerank,
            "PageRank should fire after 45s (>30s)"
        );
        assert!(
            !decision.run_truncate,
            "TRUNCATE must wait the full 60s window"
        );
    }

    #[test]
    fn cadence_truncate_fires_after_truncate_interval() {
        let mut cadence = WatcherCadence::new();
        let t0 = Instant::now();
        cadence.tick(t0, 30_000, 32, 60_000);

        let t1 = t0 + Duration::from_millis(60_001);
        let decision = cadence.tick(t1, 30_000, 32, 60_000);
        assert!(
            decision.run_truncate,
            "TRUNCATE must fire once its interval elapses"
        );
    }

    #[test]
    fn cadence_saturating_counter_does_not_overflow() {
        let mut cadence = WatcherCadence::new();
        cadence.batches_since_pagerank = u64::MAX;
        let t = Instant::now();
        // Should not panic even with saturated counter.
        let decision = cadence.tick(t, 30_000, 32, 60_000);
        assert!(decision.run_pagerank);
    }

    #[test]
    fn ignore_cache_throttle_no_op_within_window() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let ignore_path = root.join(QARTEZIGNORE_FILENAME);

        std::fs::write(&ignore_path, "first/\n").unwrap();
        let mut cache = QartezIgnoreCache::new(root);
        assert!(cache.is_ignored(&root.join("first/file.rs")));

        // First refresh: marks `last_check`.
        cache.refresh_if_changed(root);

        // Sleep past mtime resolution but inside the throttle window.
        thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&ignore_path, "second/\n").unwrap();

        // Second refresh: throttled, must NOT pick up the change.
        cache.refresh_if_changed(root);
        assert!(
            cache.is_ignored(&root.join("first/file.rs")),
            "throttle window must keep the old matcher in place"
        );
        assert!(
            !cache.is_ignored(&root.join("second/file.rs")),
            "throttle window must NOT pick up the new pattern yet"
        );
    }

    #[test]
    fn ignore_cache_throttle_fires_after_window() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let ignore_path = root.join(QARTEZIGNORE_FILENAME);

        std::fs::write(&ignore_path, "first/\n").unwrap();
        let mut cache = QartezIgnoreCache::new(root);
        cache.refresh_if_changed(root);

        thread::sleep(std::time::Duration::from_millis(
            IGNORE_REFRESH_MIN_INTERVAL_MS + 200,
        ));
        std::fs::write(&ignore_path, "second/\n").unwrap();

        cache.refresh_if_changed(root);
        assert!(
            cache.is_ignored(&root.join("second/file.rs")),
            "after the throttle window expires, the cache must reload"
        );
        assert!(
            !cache.is_ignored(&root.join("first/file.rs")),
            "old pattern must be replaced after reload"
        );
    }

    /// `matched_path_or_any_parents` panics on a path outside the matcher
    /// root, and `is_ignored` runs inside the notify FFI callback, where a
    /// panic aborts the whole server. A path outside the root is never
    /// indexable, so it must read as ignored instead.
    #[test]
    fn ignore_cache_treats_paths_outside_root_as_ignored_without_panicking() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(GITIGNORE_FILENAME), "target/\n").unwrap();
        let cache = QartezIgnoreCache::new(&root);
        assert!(cache.is_ignored(&tmp.path().join("elsewhere/src/lib.rs")));
        assert!(!cache.is_ignored(&root.join("src/lib.rs")));
        assert!(cache.is_ignored(&root.join("target/debug/x.rs")));
    }

    #[test]
    fn debounce_coalesces_a_burst_into_one_batch() {
        let tmp = TempDir::new().unwrap();
        let kept = tmp.path().join("kept.rs");
        std::fs::write(&kept, "").unwrap();
        let gone = tmp.path().join("gone.rs");

        let (raw_tx, raw_rx) = std::sync::mpsc::channel();
        let (tx, mut rx) = mpsc::channel(8);
        for p in [&kept, &gone, &kept, &kept] {
            raw_tx.send(p.clone()).unwrap();
        }
        drop(raw_tx);
        debounce_paths(
            &raw_rx,
            &tx,
            Duration::from_millis(50),
            Duration::from_secs(5),
        );

        let batch = rx.try_recv().expect("one batch is flushed");
        assert_eq!(batch.changed, vec![kept]);
        assert_eq!(batch.deleted, vec![gone]);
        assert!(
            rx.try_recv().is_err(),
            "a burst must yield exactly one batch"
        );
    }

    #[test]
    fn debounce_flushes_at_max_wait_under_a_steady_trickle() {
        let (raw_tx, raw_rx) = std::sync::mpsc::channel::<PathBuf>();
        let (tx, mut rx) = mpsc::channel(64);
        let producer = thread::spawn(move || {
            // One path every 20 ms for ~600 ms: never quiet for 100 ms.
            for i in 0..30 {
                if raw_tx
                    .send(PathBuf::from(format!("/nonexistent/{i}.rs")))
                    .is_err()
                {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        debounce_paths(
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
            paths += b.deleted.len() + b.changed.len();
        }
        assert_eq!(paths, 30, "no path may be lost across flushes");
        assert!(
            batches >= 2,
            "max_wait must force a flush during a trickle, got {batches} batch(es)"
        );
    }

    #[test]
    fn debounce_stops_when_consumer_is_gone() {
        let (raw_tx, raw_rx) = std::sync::mpsc::channel::<PathBuf>();
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        raw_tx.send(PathBuf::from("/nonexistent/a.rs")).unwrap();
        // The sender stays alive: only the closed consumer can end the loop.
        debounce_paths(
            &raw_rx,
            &tx,
            Duration::from_millis(20),
            Duration::from_millis(100),
        );
        drop(raw_tx);
    }

    /// Events under a gitignored directory must be dropped before they are
    /// queued, while a real source edit still comes through.
    #[test]
    fn notify_watcher_drops_ignored_paths_before_debouncing() {
        let tmp = TempDir::new().unwrap();
        // Canonical path: FSEvents reports `/private/var/...` for `/var/...`.
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join(GITIGNORE_FILENAME), "DerivedData/\n").unwrap();
        std::fs::create_dir_all(root.join("DerivedData/Build")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();

        let (tx, mut rx) = mpsc::channel(64);
        let _watcher =
            start_notify_watcher(root.clone(), ext_set(), name_set(), Vec::new(), tx).unwrap();
        // Let the backend register the stream before writing.
        thread::sleep(Duration::from_millis(300));

        for i in 0..50 {
            std::fs::write(root.join(format!("DerivedData/Build/gen{i}.rs")), "x").unwrap();
        }
        std::fs::write(root.join("src/lib.rs"), "pub fn a() {}\n").unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen: Vec<PathBuf> = Vec::new();
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(b) => {
                    seen.extend(b.changed);
                    seen.extend(b.deleted);
                    if seen.iter().any(|p| p.ends_with("src/lib.rs")) {
                        break;
                    }
                }
                Err(_) => thread::sleep(Duration::from_millis(20)),
            }
        }
        assert!(
            seen.iter().any(|p| p.ends_with("src/lib.rs")),
            "the source edit must reach the indexer, saw {seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|p| p.components().any(|c| c.as_os_str() == "DerivedData")),
            "ignored paths must never be batched, saw {seen:?}"
        );
    }
}
