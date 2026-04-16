use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use clap::{CommandFactory, Parser};
use qartez_mcp::{cli, cli_runner, config, git, graph, index, server, storage, watch};
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

    if config.has_project {
        if let Some(wiki_path) = cli.wiki.as_ref() {
            // Wiki generation depends on a fresh index + pagerank + co-change,
            // and the CLI caller is explicitly waiting for the output file.
            // Keep this path synchronous.
            index::full_index_multi(&conn, &config.project_roots, config.reindex)?;
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
            let db_path = config.db_path.clone();
            let project_roots = config.project_roots.clone();
            let primary_root = config.primary_root.clone();
            let reindex = config.reindex;
            let git_depth = config.git_depth;
            tokio::task::spawn_blocking(move || {
                let conn = match storage::open_db(&db_path) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("background indexer: open_db failed: {e}");
                        return;
                    }
                };
                if let Err(e) = index::full_index_multi(&conn, &project_roots, reindex) {
                    tracing::error!("background indexer: full_index_multi failed: {e}");
                    return;
                }
                if let Err(e) = graph::pagerank::compute_pagerank(&conn, &Default::default()) {
                    tracing::error!("background indexer: pagerank failed: {e}");
                }
                if let Err(e) = graph::pagerank::compute_symbol_pagerank(&conn, &Default::default())
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
                tracing::info!(
                    "Background index complete for {} root(s)",
                    project_roots.len()
                );
            });
        }
    } else {
        tracing::info!("No project detected, starting MCP server with empty index");
    }

    let server = server::QartezServer::with_roots(
        conn,
        config.primary_root,
        config.project_roots.clone(),
        config.git_depth,
    );

    if !cli.no_watch && config.has_project {
        let db = server.db_arc();
        for root in config.project_roots.iter() {
            let watcher = watch::Watcher::new(Arc::clone(&db), root.clone());
            let root_display = root.display().to_string();
            tokio::spawn(async move {
                if let Err(e) = watcher.run().await {
                    tracing::error!("watcher error for {}: {e}", root_display);
                }
            });
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
        .and_then(|p| p.parent().map(|d| d.join("qartez-setup")))
        .filter(|p| p.is_file())
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

fn update_cache_is_fresh() -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let cache = PathBuf::from(home)
        .join(".qartez")
        .join("last-update-check");
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
