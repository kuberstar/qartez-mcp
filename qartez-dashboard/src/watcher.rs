//! Notify-based file watcher.
//!
//! M1 scope: spawn `notify-debouncer-full` on the project root, log debounced
//! events, and broadcast `Event::FileChanged` to subscribed WebSocket clients.
//! Incremental reindex pipeline lands in M3 (`indexer::Indexer`).

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, new_debouncer};
use tokio::sync::mpsc;

use crate::state::{AppState, Event};

/// Inner debounce window. Coalesces duplicate save events from editors that
/// emit "modify metadata" + "modify data" + "create" + "rename" in quick
/// succession (every editor on macOS, basically).
const DEBOUNCE_MS: u64 = 50;

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

fn run_blocking(root: &Path, tx: mpsc::Sender<Vec<PathBuf>>) -> Result<()> {
    let filter = IgnoreFilter::for_root(root);
    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<DebounceEventResult>();

    let mut debouncer = new_debouncer(
        Duration::from_millis(DEBOUNCE_MS),
        None,
        move |res: DebounceEventResult| {
            let _ = raw_tx.send(res);
        },
    )
    .context("creating debouncer")?;

    debouncer
        .watch(root, RecursiveMode::Recursive)
        .with_context(|| format!("watching {}", root.display()))?;

    tracing::info!(root = %root.display(), "watcher.started");

    while let Ok(result) = raw_rx.recv() {
        match result {
            Ok(events) => {
                let mut paths: Vec<PathBuf> = events
                    .into_iter()
                    .flat_map(|e| e.event.paths)
                    .filter(|p| !filter.is_ignored(p))
                    .collect();
                paths.sort();
                paths.dedup();
                if paths.is_empty() {
                    continue;
                }
                if tx.blocking_send(paths).is_err() {
                    break;
                }
            }
            Err(errors) => {
                for error in errors {
                    tracing::warn!(%error, "watcher.event.error");
                }
            }
        }
    }

    Ok(())
}

/// Path filter for the watcher, mirroring the gitignore sources consumed by
/// `walker::walk_source_files` so the initial scan and the live event stream
/// stay consistent.
struct IgnoreFilter {
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
        // `Gitignore::global()` reads `$XDG_CONFIG_HOME/git/ignore` when
        // present; we keep it as a second matcher so XDG-only setups still
        // work without an explicit `core.excludesfile`.
        let (global, _) = Gitignore::global();
        Self { local, global }
    }

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
