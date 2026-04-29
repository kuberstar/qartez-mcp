use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use rusqlite::Connection;
use tokio::sync::mpsc;

use crate::graph;
use crate::index;
use crate::index::languages;
use crate::lock::RepoLock;

const QARTEZIGNORE_FILENAME: &str = ".qartezignore";
const GITIGNORE_FILENAME: &str = ".gitignore";

/// Debounce window: events arriving within this interval after the first
/// event in a batch are folded into the same re-index cycle.
const DEBOUNCE_MS: u64 = 500;

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

            // Debounce: drain any additional events that arrive within the window.
            while let Ok(Some(more)) =
                tokio::time::timeout(Duration::from_millis(DEBOUNCE_MS), rx.recv()).await
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
        graph::pagerank::compute_pagerank(&conn, &Default::default())?;
        graph::pagerank::compute_symbol_pagerank(&conn, &Default::default())?;
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
        }
    }

    fn refresh_if_changed(&mut self, root: &Path) {
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
    fn is_ignored(&self, path: &Path) -> bool {
        for component in path.components() {
            if let Component::Normal(name) = component
                && matches!(name.to_str(), Some(".git") | Some(".qartez"))
            {
                return true;
            }
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

fn start_notify_watcher(
    root: PathBuf,
    supported_ext: HashSet<&'static str>,
    supported_names: HashSet<&'static str>,
    supported_prefixes: Vec<&'static str>,
    tx: mpsc::Sender<WatchBatch>,
) -> anyhow::Result<RecommendedWatcher> {
    let ignore_cache = Arc::new(Mutex::new(QartezIgnoreCache::new(&root)));
    let ignore_root = root.clone();

    let mut watcher =
        notify::recommended_watcher(move |result: std::result::Result<Event, notify::Error>| {
            let event = match result {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("watch error: {e}");
                    return;
                }
            };

            let dominated = matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            );
            if !dominated {
                return;
            }

            let mut guard = match ignore_cache.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.refresh_if_changed(&ignore_root);

            let filtered: Vec<PathBuf> = event
                .paths
                .iter()
                .filter(|p| {
                    is_indexable_path(p, &supported_ext, &supported_names, &supported_prefixes)
                })
                .filter(|p| !guard.is_ignored(p))
                .cloned()
                .collect();
            drop(guard);

            if filtered.is_empty() {
                return;
            }

            // Translate rename events into a remove+create pair. On platforms
            // where `notify` emits `Modify(Name::Both)` the event carries both
            // the old and new path; on platforms that split into
            // `Modify(Name::From)` + `Modify(Name::To)` each side arrives as a
            // separate event. `RenameMode::Any` is the fallback used by some
            // backends - we split by existence on disk at observation time.
            let (changed, deleted) = match event.kind {
                EventKind::Remove(_) => (Vec::new(), filtered),
                EventKind::Modify(ModifyKind::Name(RenameMode::From)) => (Vec::new(), filtered),
                EventKind::Modify(ModifyKind::Name(RenameMode::To)) => (filtered, Vec::new()),
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if filtered.len() == 2 => {
                    // `notify` emits exactly two paths for a `Both` rename:
                    // `[from, to]`. Anything else is backend noise, not a
                    // real rename, so fall through to the existence-check
                    // branch instead of treating every non-first entry as
                    // a new destination.
                    let from = filtered[0].clone();
                    let to = filtered[1].clone();
                    (vec![to], vec![from])
                }
                EventKind::Modify(ModifyKind::Name(_)) => {
                    let mut changed = Vec::new();
                    let mut deleted = Vec::new();
                    for p in filtered {
                        if p.exists() {
                            changed.push(p);
                        } else {
                            deleted.push(p);
                        }
                    }
                    (changed, deleted)
                }
                _ => (filtered, Vec::new()),
            };

            if changed.is_empty() && deleted.is_empty() {
                return;
            }

            let _ = tx.blocking_send(WatchBatch { changed, deleted });
        })?;

    watcher.watch(&root, RecursiveMode::Recursive)?;
    Ok(watcher)
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
}
