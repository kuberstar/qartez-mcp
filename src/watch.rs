use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ignore::gitignore::Gitignore;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use rusqlite::Connection;
use tokio::sync::mpsc;

use crate::graph;
use crate::index;
use crate::index::languages;

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
}

impl Watcher {
    pub fn new(db: Arc<Mutex<Connection>>, project_root: PathBuf) -> Self {
        Self { db, project_root }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let supported: HashSet<&str> = languages::supported_extensions().into_iter().collect();

        let (tx, mut rx) = mpsc::channel::<WatchBatch>(64);

        let project_root = self.project_root.clone();
        let _watcher = start_notify_watcher(project_root.clone(), supported, tx)?;

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
        let conn = self.db.lock().expect("watcher db mutex poisoned");
        index::incremental_index(&conn, &self.project_root, changed, deleted)?;
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

fn start_notify_watcher(
    root: PathBuf,
    supported: HashSet<&'static str>,
    tx: mpsc::Sender<WatchBatch>,
) -> anyhow::Result<RecommendedWatcher> {
    let qartezignore = load_qartezignore(&root);

    let mut watcher =
        notify::recommended_watcher(move |result: std::result::Result<Event, notify::Error>| {
            let event = match result {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("watch error: {e}");
                    return;
                }
            };

            let is_remove = matches!(event.kind, EventKind::Remove(_));
            let dominated = matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            );
            if !dominated {
                return;
            }

            let paths: Vec<PathBuf> = event
                .paths
                .into_iter()
                .filter(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|ext| supported.contains(ext))
                })
                .filter(|p| {
                    !qartezignore
                        .matched_path_or_any_parents(p, p.is_dir())
                        .is_ignore()
                })
                .collect();

            if paths.is_empty() {
                return;
            }

            let batch = if is_remove {
                WatchBatch {
                    changed: Vec::new(),
                    deleted: paths,
                }
            } else {
                WatchBatch {
                    changed: paths,
                    deleted: Vec::new(),
                }
            };

            let _ = tx.blocking_send(batch);
        })?;

    watcher.watch(&root, RecursiveMode::Recursive)?;
    Ok(watcher)
}
