use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "qartez-mcp", about = "MCP server for codebase intelligence")]
pub struct Cli {
    /// Project root(s). Can be specified multiple times for monorepo support.
    #[arg(long)]
    pub root: Vec<PathBuf>,

    /// Force full re-index
    #[arg(long)]
    pub reindex: bool,

    /// Max git commits to analyze for co-changes
    #[arg(long, default_value = "300")]
    pub git_depth: u32,

    /// Database path override
    #[arg(long)]
    pub db_path: Option<PathBuf>,

    /// Disable the automatic file watcher (watcher is enabled by default
    /// when a project is detected).
    #[arg(long)]
    pub no_watch: bool,

    /// Log level
    #[arg(long, default_value = "info")]
    pub log_level: String,

    /// Generate an architecture wiki after indexing and write it to this path
    /// (relative to the project root). Leave unset to skip generation.
    #[arg(long)]
    pub wiki: Option<PathBuf>,

    /// Leiden resolution parameter. Larger values produce more, smaller
    /// clusters; smaller values merge clusters.
    #[arg(long, default_value = "1.0")]
    pub leiden_resolution: f64,
}
