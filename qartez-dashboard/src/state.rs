//! Shared application state passed to axum handlers and background tasks.
//!
//! Held inside an `Arc` and cloned cheaply into request handlers. The
//! broadcast sender is the WebSocket fan-out hub: every file-change event,
//! reindex progress update, and project event flows through it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// WebSocket fan-out capacity. Slow clients see `RecvError::Lagged` and
/// the per-connection task continues from the latest message rather than
/// disconnecting, so a small buffer is fine.
const BROADCAST_CAPACITY: usize = 256;

/// Cloneable handle to the daemon's runtime state.
#[derive(Debug, Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    pub project_root: PathBuf,
    pub auth_token: String,
    pub events: broadcast::Sender<Event>,
    pub shutdown: CancellationToken,
    /// Sentinel for the `POST /api/reindex` endpoint. `Some(ts)` means a
    /// manual reindex is currently in progress and `ts` is the unix epoch
    /// in milliseconds at which it started. Cleared back to `None` when
    /// the spawned worker finishes (success or failure).
    pub reindex_started_at: Mutex<Option<u64>>,
}

impl AppState {
    pub fn new(project_root: PathBuf, auth_token: String, shutdown: CancellationToken) -> Self {
        let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                project_root,
                auth_token,
                events: tx,
                shutdown,
                reindex_started_at: Mutex::new(None),
            }),
        }
    }

    pub fn project_root(&self) -> &std::path::Path {
        &self.inner.project_root
    }

    pub fn auth_token(&self) -> &str {
        &self.inner.auth_token
    }

    pub fn events(&self) -> broadcast::Sender<Event> {
        self.inner.events.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn shutdown(&self) -> CancellationToken {
        self.inner.shutdown.clone()
    }

    /// Access the manual-reindex sentinel. `Some(ts)` => reindex in
    /// progress, started at `ts` (unix epoch ms); `None` => idle.
    pub fn reindex_started_at(&self) -> &Mutex<Option<u64>> {
        &self.inner.reindex_started_at
    }
}

/// JSON envelope pushed over `/ws` to subscribed browser tabs.
///
/// Adding new variants is forward-compatible: old clients ignore unknown types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    /// Heartbeat sent periodically to keep proxies and the client store
    /// from declaring the socket dead. No semantic meaning beyond liveness.
    Ping { ts_ms: u64 },

    /// One or more files changed on disk. Emitted by the watcher after
    /// debouncing.
    FileChanged { paths: Vec<String> },

    /// The index has been refreshed for the current project.
    IndexUpdated { changed: usize, deleted: usize },

    /// Reindex in progress. Emitted at the start of a batch and on completion.
    ReindexProgress { phase: ReindexPhase, percent: u8 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReindexPhase {
    Start,
    Indexing,
    Complete,
}
