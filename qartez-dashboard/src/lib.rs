//! Qartez Dashboard: a local web UI for the Qartez code intelligence index.
//!
//! The dashboard is a background daemon that exposes an HTTP + WebSocket
//! server on `127.0.0.1`. It reads the `qartez-mcp` SQLite index from disk
//! and pushes incremental updates to the browser when files change.

use std::path::PathBuf;

use anyhow::Result;

pub mod api;
pub mod auth;
pub mod cli;
pub mod daemon;
pub mod discovery;
pub mod embed;
pub mod indexer;
pub mod paths;
pub mod server;
pub mod state;
pub mod watcher;
pub mod ws;

pub use cli::DashboardCommand;

/// Entry point dispatched from the `qartez dashboard <subcommand>` CLI route.
///
/// # Errors
/// Returns the error from the dispatched subcommand verbatim. Each subcommand
/// surfaces its own diagnostics via `tracing` before returning.
pub async fn run(
    command: DashboardCommand,
    project_root: Option<PathBuf>,
    indexer: indexer::IncrementalIndexer,
) -> Result<()> {
    match command {
        DashboardCommand::Start { foreground, port } => {
            daemon::start(project_root, foreground, port, indexer).await
        }
        DashboardCommand::Stop => daemon::stop().await,
        DashboardCommand::Status => daemon::status().await,
        DashboardCommand::Open => daemon::open().await,
    }
}
