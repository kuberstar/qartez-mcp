use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use ignore::gitignore::Gitignore;
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use rusqlite::Connection;
use tokio::sync::mpsc;

use crate::graph;
use crate::index;
use crate::index::languages;
use crate::lock::RepoLock;

const QARTEZIGNORE_FILENAME: &str = ".qartezignore";

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

fn load_qartezignore(root: &Path) -> Gitignore {
    let ignore_path = root.join(QARTEZIGNORE_FILENAME);
    if ignore_path.exists() {
        let (gi, err) = Gitignore::new(&ignore_path);
        if let Some(e) = err {
            tracing::warn!(path = %ignore_path.display(), error = %e, "partial parse of .qartezignore");
        }
        gi
    } else {
        Gitignore::empty()
    }
}

/// Hot-reload wrapper for `.qartezignore`. Holds the parsed matcher together
/// with the mtime that was observed when it was loaded, so the closure can
/// refresh the cache after the user edits the ignore file during a live
/// watcher session (rather than requiring a full restart).
struct QartezIgnoreCache {
    gi: Gitignore,
    mtime: Option<SystemTime>,
}

impl QartezIgnoreCache {
    fn new(root: &Path) -> Self {
        Self {
            gi: load_qartezignore(root),
            mtime: fs_mtime(&root.join(QARTEZIGNORE_FILENAME)),
        }
    }

    fn refresh_if_changed(&mut self, root: &Path) {
        let current = fs_mtime(&root.join(QARTEZIGNORE_FILENAME));
        if current != self.mtime {
            self.gi = load_qartezignore(root);
            self.mtime = current;
        }
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
                .filter(|p| {
                    !guard
                        .gi
                        .matched_path_or_any_parents(p, p.is_dir())
                        .is_ignore()
                })
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
            cache
                .gi
                .matched_path_or_any_parents(&target_before, false)
                .is_ignore(),
            "initial ignore pattern should block generated/"
        );
        let target_other = root.join("other/file.rs");
        assert!(
            !cache
                .gi
                .matched_path_or_any_parents(&target_other, false)
                .is_ignore(),
            "non-matching path should not be ignored initially"
        );

        // Sleep long enough that the filesystem reports a new mtime;
        // on fast SSDs the metadata resolution can be as coarse as 1s.
        thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&ignore_path, "other/\n").unwrap();

        cache.refresh_if_changed(root);
        assert!(
            cache
                .gi
                .matched_path_or_any_parents(&target_other, false)
                .is_ignore(),
            "cache must have reloaded and now ignore other/"
        );
        assert!(
            !cache
                .gi
                .matched_path_or_any_parents(&target_before, false)
                .is_ignore(),
            "old pattern (generated/) must be dropped after reload"
        );
    }

    #[test]
    fn qartezignore_cache_starts_empty_when_file_absent() {
        let tmp = TempDir::new().unwrap();
        let cache = QartezIgnoreCache::new(tmp.path());
        assert!(cache.mtime.is_none());
        let any_path = tmp.path().join("anything");
        assert!(
            !cache
                .gi
                .matched_path_or_any_parents(&any_path, false)
                .is_ignore(),
            "empty cache matches nothing"
        );
    }

    #[test]
    fn qartezignore_cache_picks_up_newly_created_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut cache = QartezIgnoreCache::new(root);
        let target = root.join("vendor/dep.rs");

        assert!(
            !cache
                .gi
                .matched_path_or_any_parents(&target, false)
                .is_ignore()
        );

        std::fs::write(root.join(QARTEZIGNORE_FILENAME), "vendor/\n").unwrap();
        cache.refresh_if_changed(root);

        assert!(
            cache
                .gi
                .matched_path_or_any_parents(&target, false)
                .is_ignore(),
            "cache must notice a freshly-written .qartezignore"
        );
    }
}
