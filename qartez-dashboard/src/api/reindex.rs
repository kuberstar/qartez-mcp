//! `POST /api/reindex` - manually trigger a project-wide reindex.
//!
//! The watcher already drives an incremental reindex when files change
//! on disk, but the dashboard exposes this manual trigger so the user
//! can force a refresh without touching files (for example after a
//! `git checkout` that bypasses the OS file-event stream).
//!
//! The handler is non-blocking: it stamps an in-progress sentinel on
//! `AppState::reindex_started_at`, spawns a tokio task that walks the
//! project root and emits a `FileChanged` batch for every supported
//! source file, and returns 200 immediately. The existing indexer
//! worker on the broadcast bus consumes the batch and emits the usual
//! `ReindexProgress` and `IndexUpdated` events.
//!
//! Concurrency: while a reindex is in flight, subsequent calls return
//! 200 with `in_progress: true` and the original `started_at` timestamp
//! so the UI can render a single shared progress bar instead of
//! enqueueing duplicate scans.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use ignore::WalkBuilder;
use serde::Serialize;

use crate::state::{AppState, Event, ReindexPhase};

/// Response body for `POST /api/reindex`.
#[derive(Debug, Serialize)]
pub struct ReindexAck {
    /// Always `true` when the handler returns 200.
    pub ok: bool,
    /// `true` when a previous reindex is still running and this call
    /// piggy-backed on it; `false` when this call started a new run.
    pub in_progress: bool,
    /// Unix epoch milliseconds at which the active reindex started.
    pub started_at: u64,
}

/// Handle `POST /api/reindex`.
///
/// Returns 200 immediately. The reindex itself runs on a tokio task
/// that emits `Event::ReindexProgress` and `Event::FileChanged` over
/// the broadcast bus; the existing indexer worker handles the rest.
pub async fn handler(State(state): State<AppState>) -> (StatusCode, Json<ReindexAck>) {
    let now_ms = current_unix_ms();
    let (was_idle, started_at) = match try_start_reindex(state.reindex_started_at(), now_ms) {
        Ok(decision) => decision,
        Err(error) => {
            tracing::error!(?error, "reindex.lock.poisoned");
            return (
                StatusCode::OK,
                Json(ReindexAck {
                    ok: false,
                    in_progress: true,
                    started_at: now_ms,
                }),
            );
        }
    };

    if was_idle {
        spawn_reindex_worker(state.clone());
    }

    (
        StatusCode::OK,
        Json(ReindexAck {
            ok: true,
            in_progress: !was_idle,
            started_at,
        }),
    )
}

/// Atomically claim the reindex slot. Returns `(was_idle, started_at)`:
///
/// - `(true, now_ms)`  => the slot was free and is now claimed for `now_ms`.
/// - `(false, ts)`     => a reindex was already running, started at `ts`.
///
/// Factored out so unit tests can drive the state-machine logic
/// without spinning up a tokio runtime or touching the filesystem.
pub(crate) fn try_start_reindex(
    slot: &Mutex<Option<u64>>,
    now_ms: u64,
) -> Result<(bool, u64), &'static str> {
    let mut guard = slot.lock().map_err(|_| "reindex_started_at poisoned")?;
    match *guard {
        Some(existing) => Ok((false, existing)),
        None => {
            *guard = Some(now_ms);
            Ok((true, now_ms))
        }
    }
}

/// Reset the sentinel back to idle. Held in its own helper so the
/// guard struct in `spawn_reindex_worker` stays trivial.
fn clear_reindex_slot(slot: &Mutex<Option<u64>>) {
    if let Ok(mut guard) = slot.lock() {
        *guard = None;
    }
}

/// RAII guard that clears the reindex slot when dropped, so a panic in
/// the walk or send path does not strand the daemon in
/// `in_progress: true` forever.
struct ReindexSlotGuard {
    state: AppState,
}

impl Drop for ReindexSlotGuard {
    fn drop(&mut self) {
        clear_reindex_slot(self.state.reindex_started_at());
    }
}

fn spawn_reindex_worker(state: AppState) {
    tokio::spawn(async move {
        let _guard = ReindexSlotGuard {
            state: state.clone(),
        };

        let events = state.events();
        let _ = events.send(Event::ReindexProgress {
            phase: ReindexPhase::Start,
            percent: 0,
        });

        let root = state.project_root().to_path_buf();
        let walk = tokio::task::spawn_blocking(move || collect_source_files(&root)).await;

        let paths = match walk {
            Ok(paths) => paths,
            Err(error) => {
                tracing::error!(?error, "reindex.walk.join_failed");
                let _ = events.send(Event::ReindexProgress {
                    phase: ReindexPhase::Complete,
                    percent: 100,
                });
                return;
            }
        };

        if paths.is_empty() {
            let _ = events.send(Event::ReindexProgress {
                phase: ReindexPhase::Complete,
                percent: 100,
            });
            return;
        }

        let _ = events.send(Event::FileChanged { paths });
    });
}

/// Walk the project root and emit every non-ignored file path. The downstream
/// `IncrementalIndexer` callback applies the language registry (37+ extensions
/// plus extensionless filenames like `Dockerfile`, `Makefile`, `CMakeLists.txt`
/// and prefix matches like `Dockerfile.prod`), so filtering by extension here
/// would silently drop files this dashboard binary cannot enumerate without a
/// circular workspace dependency on `qartez-mcp`. The indexer skips unknown
/// files cheaply, so the only cost of forwarding extra paths is broadcast-bus
/// payload size, which is bounded by the `.gitignore`-honoring walk.
fn collect_source_files(root: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".qartezignore")
        .filter_entry(|entry| {
            entry
                .file_name()
                .to_str()
                .map(|n| !matches!(n, ".git" | ".qartez" | "node_modules" | "target"))
                .unwrap_or(true)
        })
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(error) => {
                tracing::debug!(%error, "reindex.walk.skip");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        paths.push(entry.path().to_string_lossy().into_owned());
    }
    paths
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_when_idle() {
        let slot: Mutex<Option<u64>> = Mutex::new(None);
        let (was_idle, ts) = try_start_reindex(&slot, 12345).expect("lock ok");
        assert!(was_idle, "fresh slot must start a new reindex");
        assert_eq!(ts, 12345);
        let observed = *slot.lock().expect("lock ok");
        assert_eq!(observed, Some(12345), "slot must hold the new timestamp");
    }

    #[test]
    fn returns_existing_when_in_progress() {
        let slot: Mutex<Option<u64>> = Mutex::new(Some(123456));
        let (was_idle, ts) = try_start_reindex(&slot, 999_999).expect("lock ok");
        assert!(!was_idle, "occupied slot must report not-idle");
        assert_eq!(ts, 123456, "must echo the original start timestamp");
        let observed = *slot.lock().expect("lock ok");
        assert_eq!(
            observed,
            Some(123456),
            "slot must keep the original timestamp untouched"
        );
    }
}

// Rust guideline compliant 2026-04-26
