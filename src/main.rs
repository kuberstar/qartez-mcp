use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::SystemTime;

use clap::{CommandFactory, Parser};
use qartez_mcp::{cli, cli_runner, config, git, graph, index, lock, server, storage};
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(&cli.log_level)
        .init();

    schedule_update_check();

    let config = config::Config::from_cli(&cli)?;

    // CLI subcommand path: index synchronously, run tool, exit.
    if let Some(ref command) = cli.command {
        let format = cli.format.unwrap_or_default();
        return cli_runner::run(&config, command, format);
    }

    // No subcommand + interactive terminal: show help instead of
    // silently blocking on stdin waiting for MCP JSON-RPC.
    if std::io::stdin().is_terminal() {
        cli::Cli::command().print_help()?;
        println!();
        return Ok(());
    }

    // MCP server path below (stdin is piped by an MCP client).
    tracing::info!(
        "qartez-mcp starting, {} root(s), primary: {}",
        config.project_roots.len(),
        config.primary_root.display()
    );

    let conn = storage::open_db(&config.db_path)?;

    tracing::info!("Database ready at {}", config.db_path.display());

    // Cheap two-stat startup telemetry: surfaces multi-GiB DB and WAL
    // sizes so operators see the bloat without inspecting the file by
    // hand. Threshold breach is logged at WARN with a hint pointing at
    // the qartez_maintenance tool.
    {
        let telemetry = storage::maintenance::startup_telemetry(&config.db_path);
        if telemetry.contains("[DB > ") || telemetry.contains("[WAL > ") {
            tracing::warn!("{telemetry}");
        } else {
            tracing::info!("{telemetry}");
        }
    }

    if config.has_project {
        if let Some(wiki_path) = cli.wiki.as_ref() {
            // Wiki generation depends on a fresh index + pagerank + co-change,
            // and the CLI caller is explicitly waiting for the output file.
            // Keep this path synchronous.
            //
            // Hold the cross-process lock for the entire write-heavy block so
            // a concurrent qartez-mcp on the same repo cannot race against
            // our index, pagerank, or cochange writes. The lock is dropped
            // at the end of the block before the read-only wiki render.
            let qartez_dir = lock_dir_for(&config.db_path);
            let _index_lock = lock::RepoLock::acquire(&qartez_dir).map_err(|e| {
                tracing::error!("could not acquire index lock for wiki path: {e}");
                anyhow::anyhow!("{e}")
            })?;
            index::full_index_multi(
                &conn,
                &config.project_roots,
                &config.root_aliases,
                config.reindex,
            )?;
            graph::pagerank::compute_pagerank(&conn, &Default::default())?;
            graph::pagerank::compute_symbol_pagerank(&conn, &Default::default())?;
            git::cochange::analyze_cochanges(
                &conn,
                &config.primary_root,
                &git::cochange::CoChangeConfig {
                    commit_limit: config.git_depth,
                    ..Default::default()
                },
            )?;
            tracing::info!("Index complete for {} root(s)", config.project_roots.len());

            let project_name = config
                .primary_root
                .canonicalize()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .or_else(|| {
                    config
                        .primary_root
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "project".to_string());
            let wiki_cfg = graph::wiki::WikiConfig {
                project_name,
                recompute: true,
                leiden: graph::leiden::LeidenConfig {
                    resolution: cli.leiden_resolution,
                    ..Default::default()
                },
                ..Default::default()
            };
            let (markdown, modularity) = graph::wiki::render_wiki(&conn, &wiki_cfg)?;
            let abs = config.primary_root.join(wiki_path);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&abs, &markdown)?;
            tracing::info!(
                "Wrote {} bytes to {} (modularity {:.2})",
                markdown.len(),
                abs.display(),
                modularity.unwrap_or(0.0),
            );
        } else {
            // MCP-server path: spawn indexing on a dedicated connection so
            // `server.serve()` below can answer `initialize`/`list_tools`
            // immediately. Tool calls issued before the background task
            // finishes see whatever the DB carried over from the previous
            // run (empty on first-ever start).
            //
            // Workspace fingerprint short-circuit: when the stored
            // fingerprint matches the freshly-computed one and the
            // caller did not pass `--reindex`, we skip the full reindex
            // entirely. The watcher still incrementally re-indexes any
            // file that changed since the last run, so up-to-date
            // semantics are preserved without a multi-minute DB rewrite
            // on startup.
            let db_path = config.db_path.clone();
            let project_roots = config.project_roots.clone();
            let root_aliases = config.root_aliases.clone();
            let primary_root = config.primary_root.clone();
            let reindex = config.reindex;
            let git_depth = config.git_depth;
            let new_fingerprint = index::fingerprint::compute_workspace_fingerprint(&config);
            let stored_fingerprint =
                storage::read::get_meta(&conn, index::fingerprint::META_KEY_WORKSPACE_FINGERPRINT)
                    .unwrap_or(None);
            let fingerprint_matches =
                !reindex && stored_fingerprint.as_deref() == Some(new_fingerprint.as_str());
            if fingerprint_matches {
                tracing::info!(
                    "workspace fingerprint matches; skipping full reindex (use --reindex to force)"
                );
            }
            tokio::task::spawn_blocking(move || {
                // Acquire the cross-process lock first so a sibling qartez
                // process indexing the same repo cannot race against our
                // writes. We open the connection only after the lock is
                // held; this keeps WAL contention out of the SQLite layer.
                // MCP serving (read-only) is unaffected because the server
                // owns its own connection and never participates in this
                // critical section.
                let qartez_dir = lock_dir_for(&db_path);
                let _index_lock = match lock::RepoLock::acquire(&qartez_dir) {
                    Ok(g) => g,
                    Err(e) => {
                        tracing::warn!("background indexer: skipping index this run, {e}");
                        return;
                    }
                };
                // Defer per-root WAL checkpoints inside `full_index_root` and
                // `incremental_index_with_prefix` so the cold-start path
                // completes faster; one trailing checkpoint runs at the end.
                // SAFETY: set_var requires a single-threaded process to be
                // safe across all callers. The MCP-server startup path runs
                // this once before any tool dispatch, so there is no
                // concurrent reader at this point.
                #[allow(unsafe_code)]
                unsafe {
                    std::env::set_var("QARTEZ_DEFER_COMPACTION", "1");
                }
                let conn = match storage::open_db(&db_path) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("background indexer: open_db failed: {e}");
                        return;
                    }
                };
                if !fingerprint_matches
                    && let Err(e) =
                        index::full_index_multi(&conn, &project_roots, &root_aliases, reindex)
                {
                    tracing::error!("background indexer: full_index_multi failed: {e}");
                    return;
                }
                if !fingerprint_matches {
                    if let Err(e) = graph::pagerank::compute_pagerank(&conn, &Default::default()) {
                        tracing::error!("background indexer: pagerank failed: {e}");
                    }
                    if let Err(e) =
                        graph::pagerank::compute_symbol_pagerank(&conn, &Default::default())
                    {
                        tracing::error!("background indexer: symbol_pagerank failed: {e}");
                    }
                    if let Err(e) = git::cochange::analyze_cochanges(
                        &conn,
                        &primary_root,
                        &git::cochange::CoChangeConfig {
                            commit_limit: git_depth,
                            ..Default::default()
                        },
                    ) {
                        tracing::error!("background indexer: cochange failed: {e}");
                    }
                    // Mark the fingerprint and the full-reindex
                    // timestamp atomically. A failure to write the
                    // fingerprint here only costs the next run a
                    // redundant reindex, never a stale result.
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .to_string();
                    if let Err(e) = crate::storage::write::set_meta(
                        &conn,
                        index::fingerprint::META_KEY_WORKSPACE_FINGERPRINT,
                        &new_fingerprint,
                    ) {
                        tracing::warn!("failed to persist workspace fingerprint: {e}");
                    }
                    if let Err(e) = crate::storage::write::set_meta(
                        &conn,
                        index::fingerprint::META_KEY_LAST_FULL_REINDEX,
                        &now,
                    ) {
                        tracing::warn!("failed to persist last_full_reindex: {e}");
                    }
                }
                // Single deferred checkpoint after all heavy writes
                // settled. Best-effort: SQLite's auto-checkpoint will
                // catch up on the next run if this fails.
                if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
                    tracing::debug!("post-index WAL checkpoint failed (non-fatal): {e}");
                }
                tracing::info!(
                    "Background index complete for {} root(s)",
                    project_roots.len()
                );
            });
        }
    } else {
        tracing::info!("No project detected, starting MCP server with empty index");
    }

    // Tag every initial root with its origin so `qartez_list_roots`
    // can render `cli` for command-line roots and `config` for the
    // ones reattached from `.qartez/workspace.toml`.
    let mut root_sources: std::collections::HashMap<std::path::PathBuf, server::RootSource> =
        std::collections::HashMap::new();
    let primary_canonical = config.primary_root.clone();
    for root in config.project_roots.iter() {
        let source = if config.root_aliases.contains_key(root) && root != &primary_canonical {
            server::RootSource::WorkspaceToml
        } else {
            server::RootSource::CliArg
        };
        root_sources.insert(root.clone(), source);
    }

    let watch_enabled = !cli.no_watch && config.has_project;
    let server_lock_dir = if config.has_project {
        Some(lock_dir_for(&config.db_path))
    } else {
        None
    };

    let server = server::QartezServer::with_roots_and_sources(
        conn,
        primary_canonical,
        config.project_roots.clone(),
        config.root_aliases.clone(),
        root_sources,
        config.git_depth,
        watch_enabled,
        server_lock_dir,
    );

    if watch_enabled {
        // Multi-root indexing keys rows with a per-root prefix so sibling
        // roots don't collide on `files.path`. The watcher must mirror
        // that, otherwise the first save orphans the original prefixed row.
        let multi_root = config.project_roots.len() > 1;
        for root in config.project_roots.iter() {
            let prefix = if multi_root {
                index::root_prefix(root, config.root_aliases.get(root).map(|s| s.as_str()))
            } else {
                String::new()
            };
            if let Err(e) = server.attach_watcher(root.clone(), prefix) {
                tracing::error!("failed to attach watcher for {}: {e}", root.display());
            }
        }
    }

    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let service = server.serve(transport).await?;

    service.waiting().await?;

    Ok(())
}

// Fire-and-forget background update check: spawns qartez-setup with
// --update-background. The setup binary handles the GitHub API call,
// version compare, and (if newer) re-runs install.sh from source.
//
// The TTL gate exists in two places on purpose:
//   1. Here, to avoid the process-spawn cost on every Claude Code start
//      when the cache is fresh (~5–20ms per spawn).
//   2. Inside qartez-setup itself, as the source of truth - protects
//      against concurrent qartez-mcp starts racing into the network.
//
// Skipped entirely when QARTEZ_NO_AUTO_UPDATE is set (any value).
fn schedule_update_check() {
    if std::env::var_os("QARTEZ_NO_AUTO_UPDATE").is_some() {
        return;
    }

    if update_cache_is_fresh() {
        return;
    }

    let Some(setup) = std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.parent().map(|d| {
                let bare = d.join("qartez-setup");
                if bare.is_file() {
                    Some(bare)
                } else if cfg!(windows) {
                    // On Windows, try with .exe suffix
                    let with_exe = d.join("qartez-setup.exe");
                    if with_exe.is_file() {
                        Some(with_exe)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
        })
        .flatten()
    else {
        return;
    };

    tokio::spawn(async move {
        let _ = tokio::process::Command::new(setup)
            .arg("--update-background")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    });
}

/// Cross-platform home directory lookup.
/// Checks HOME (Unix), USERPROFILE (Windows), HOMEDRIVE+HOMEPATH (Windows),
/// then falls back to current directory.
fn cross_platform_home() -> Option<PathBuf> {
    // Try HOME (Unix)
    if let Some(home) = std::env::var_os("HOME") {
        return Some(PathBuf::from(home));
    }
    // Try USERPROFILE (Windows)
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return Some(PathBuf::from(profile));
    }
    // Try HOMEDRIVE+HOMEPATH (Windows fallback)
    if let (Some(drive), Some(path)) = (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH"))
    {
        let mut combined = PathBuf::from(drive);
        combined.push(path);
        if combined.is_dir() {
            return Some(combined);
        }
    }
    None
}

/// Resolve the directory that hosts the cross-process index lock. The lock
/// file lives next to `index.db` so that workspace mode (`.qartez/index.db`
/// in the meta-directory) and single-root mode (`<root>/.qartez/index.db`)
/// both place the lock alongside the database it protects.
fn lock_dir_for(db_path: &std::path::Path) -> PathBuf {
    db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn update_cache_is_fresh() -> bool {
    let Some(home) = cross_platform_home() else {
        return false;
    };
    let cache = home.join(".qartez").join("last-update-check");
    let Ok(meta) = std::fs::metadata(&cache) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age.as_secs() < 24 * 60 * 60)
        .unwrap_or(false)
}
