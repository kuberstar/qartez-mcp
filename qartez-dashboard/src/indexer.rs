//! Incremental reindex coordinator.
//!
//! Subscribes to `Event::FileChanged` on the broadcast bus and runs the
//! injected `IncrementalIndexer` callback for each batch. A single in-flight
//! reindex is enforced by the worker loop: while one batch is processing,
//! every newer `FileChanged` event accumulates in the mpsc queue and is
//! drained as a single coalesced trailing pass when the previous run returns.
//!
//! The indexer call is injected as `Arc<dyn Fn>` so this crate does not need
//! to depend on the qartez-mcp library, which would create a workspace cycle:
//! `qartez-mcp -> qartez-dashboard -> qartez-mcp`. The qartez binary supplies
//! a closure that opens a rusqlite connection and calls
//! `qartez_mcp::index::incremental_index` on every invocation.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

use crate::state::{AppState, Event, ReindexPhase};

/// Outcome of one incremental indexer pass.
#[derive(Debug, Clone, Copy)]
pub struct IndexResult {
    /// Number of files re-ingested.
    pub changed: usize,
    /// Number of files removed from the index.
    pub deleted: usize,
}

/// Callback that runs an incremental reindex against the on-disk index.
///
/// The closure is invoked on a `spawn_blocking` thread, so it may perform
/// synchronous SQLite work without yielding the runtime. It receives the
/// project root and two slices of absolute paths: files that exist on disk
/// (treated as additions or modifications) and files that no longer exist
/// (treated as deletions).
pub type IncrementalIndexer =
    Arc<dyn Fn(&Path, &[PathBuf], &[PathBuf]) -> anyhow::Result<IndexResult> + Send + Sync>;

/// Spawn the indexer worker on the current tokio runtime.
///
/// The task lives until `state.shutdown()` is cancelled. It performs no work
/// until a `FileChanged` event is broadcast.
pub fn spawn(state: AppState, indexer: IncrementalIndexer) {
    let (tx, rx) = mpsc::unbounded_channel::<Vec<PathBuf>>();
    spawn_forwarder(state.clone(), tx);
    spawn_worker(state, rx, indexer);
}

fn spawn_forwarder(state: AppState, tx: mpsc::UnboundedSender<Vec<PathBuf>>) {
    let mut subscriber = state.subscribe();
    let shutdown = state.shutdown();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                event = subscriber.recv() => match event {
                    Ok(Event::FileChanged { paths }) => {
                        let buf: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
                        if tx.send(buf).is_err() {
                            break;
                        }
                    }
                    Ok(_) => continue,
                    Err(RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "indexer.forwarder.lagged");
                        continue;
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        }
    });
}

fn spawn_worker(
    state: AppState,
    mut rx: mpsc::UnboundedReceiver<Vec<PathBuf>>,
    indexer: IncrementalIndexer,
) {
    let events = state.events();
    let project_root = state.project_root().to_path_buf();
    let shutdown = state.shutdown();

    tokio::spawn(async move {
        loop {
            let first_batch = tokio::select! {
                biased;
                _ = shutdown.cancelled() => return,
                maybe = rx.recv() => match maybe {
                    Some(batch) => batch,
                    None => return,
                },
            };

            let mut pending: HashSet<PathBuf> = first_batch.into_iter().collect();
            while let Ok(more) = rx.try_recv() {
                pending.extend(more);
            }

            let (changed, deleted) = partition_paths(&project_root, pending);
            if changed.is_empty() && deleted.is_empty() {
                continue;
            }

            let _ = events.send(Event::ReindexProgress {
                phase: ReindexPhase::Start,
                percent: 0,
            });

            let indexer = Arc::clone(&indexer);
            let root = project_root.clone();
            let result =
                tokio::task::spawn_blocking(move || indexer(&root, &changed, &deleted)).await;

            match result {
                Ok(Ok(outcome)) => {
                    let _ = events.send(Event::ReindexProgress {
                        phase: ReindexPhase::Complete,
                        percent: 100,
                    });
                    let _ = events.send(Event::IndexUpdated {
                        changed: outcome.changed,
                        deleted: outcome.deleted,
                    });
                }
                Ok(Err(error)) => {
                    tracing::error!(?error, "indexer.reindex.failed");
                    let _ = events.send(Event::ReindexProgress {
                        phase: ReindexPhase::Complete,
                        percent: 100,
                    });
                }
                Err(error) => {
                    tracing::error!(?error, "indexer.spawn_blocking.join_failed");
                    let _ = events.send(Event::ReindexProgress {
                        phase: ReindexPhase::Complete,
                        percent: 100,
                    });
                }
            }
        }
    });
}

fn partition_paths(root: &Path, pending: HashSet<PathBuf>) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut changed = Vec::new();
    let mut deleted = Vec::new();
    for path in pending {
        if !path.starts_with(root) {
            continue;
        }
        if path.exists() {
            changed.push(path);
        } else {
            deleted.push(path);
        }
    }
    (changed, deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    use crate::state::AppState;

    #[tokio::test]
    async fn coalesce_collapses_bursts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let shutdown = CancellationToken::new();
        let state = AppState::new(root.clone(), "test".into(), shutdown.clone());

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_for_indexer = Arc::clone(&counter);
        let indexer: IncrementalIndexer = Arc::new(move |_root, changed, deleted| {
            counter_for_indexer.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(50));
            Ok(IndexResult {
                changed: changed.len(),
                deleted: deleted.len(),
            })
        });

        let mut events_rx = state.subscribe();

        spawn(state.clone(), indexer);

        for i in 0..6 {
            let path = root.join(format!("file_{i}.rs"));
            std::fs::write(&path, "// fixture").unwrap();
            let _ = state.events().send(Event::FileChanged {
                paths: vec![path.to_string_lossy().into_owned()],
            });
        }

        tokio::time::sleep(Duration::from_millis(300)).await;
        shutdown.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let runs = counter.load(Ordering::SeqCst);
        assert!(
            (1..=2).contains(&runs),
            "expected 1 or 2 reindex runs, got {runs}"
        );

        let mut saw_index_updated = false;
        while let Ok(event) = events_rx.try_recv() {
            if matches!(event, Event::IndexUpdated { .. }) {
                saw_index_updated = true;
                break;
            }
        }
        assert!(
            saw_index_updated,
            "expected at least one IndexUpdated event"
        );
    }

    #[tokio::test]
    async fn paths_outside_root_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let shutdown = CancellationToken::new();
        let state = AppState::new(root.clone(), "test".into(), shutdown.clone());

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_for_indexer = Arc::clone(&counter);
        let indexer: IncrementalIndexer = Arc::new(move |_root, _changed, _deleted| {
            counter_for_indexer.fetch_add(1, Ordering::SeqCst);
            Ok(IndexResult {
                changed: 0,
                deleted: 0,
            })
        });

        spawn(state.clone(), indexer);

        let _ = state.events().send(Event::FileChanged {
            paths: vec!["/totally/outside/the/root.rs".into()],
        });

        tokio::time::sleep(Duration::from_millis(150)).await;
        shutdown.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "indexer must not run when every path is outside the project root"
        );
    }
}
