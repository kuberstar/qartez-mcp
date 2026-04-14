use std::sync::Arc;

use clap::Parser;
use rmcp::ServiceExt;
use qartez_mcp::{cli, config, git, graph, index, server, storage, watch};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(&cli.log_level)
        .init();

    let config = config::Config::from_cli(&cli)?;

    tracing::info!(
        "qartez-mcp starting, {} root(s), primary: {}",
        config.project_roots.len(),
        config.primary_root.display()
    );

    let conn = storage::open_db(&config.db_path)?;

    tracing::info!("Database ready at {}", config.db_path.display());

    if config.has_project {
        for root in &config.project_roots {
            tracing::info!("Indexing root: {}", root.display());
            index::full_index(&conn, root, config.reindex)?;
        }
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

        if let Some(wiki_path) = cli.wiki.as_ref() {
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
        }
    } else {
        tracing::info!("No project detected, starting MCP server with empty index");
    }

    let server = server::QartezServer::new(conn, config.primary_root, config.git_depth);

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
