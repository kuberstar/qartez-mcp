use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceAction {
    Add,
    Remove,
}

/// Output format for CLI commands.
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum OutputFormat {
    /// Colored, human-readable output (default for TTY)
    #[default]
    Human,
    /// Raw JSON for piping and scripting
    Json,
    /// Concise text, good for CI logs
    Compact,
}

#[derive(Parser)]
#[command(
    name = "qartez",
    about = "Code intelligence toolkit - CLI and MCP server",
    long_about = "Qartez provides code intelligence via tree-sitter indexing, PageRank ranking,\n\
        blast-radius analysis, and more.\n\n\
        Run without a subcommand to start the MCP server.\n\
        Run with a subcommand to use the CLI directly.",
    version
)]
pub struct Cli {
    /// Project root(s). Can be specified multiple times for monorepo support.
    #[arg(long, global = true)]
    pub root: Vec<PathBuf>,

    /// Force full re-index
    #[arg(long, global = true)]
    pub reindex: bool,

    /// Max git commits to analyze for co-changes
    #[arg(long, default_value = "300", global = true)]
    pub git_depth: u32,

    /// Database path override
    #[arg(long, global = true)]
    pub db_path: Option<PathBuf>,

    /// Disable the automatic file watcher (watcher is enabled by default
    /// when a project is detected).
    #[arg(long)]
    pub no_watch: bool,

    /// Log level
    #[arg(long, default_value = "info", global = true)]
    pub log_level: String,

    /// Generate an architecture wiki after indexing and write it to this path
    /// (relative to the project root). Leave unset to skip generation.
    #[arg(long)]
    pub wiki: Option<PathBuf>,

    /// Leiden resolution parameter. Larger values produce more, smaller
    /// clusters; smaller values merge clusters.
    #[arg(long, default_value = "1.0")]
    pub leiden_resolution: f64,

    /// Output format (human, json, compact). Defaults to human for TTY.
    #[arg(long, value_enum, global = true)]
    pub format: Option<OutputFormat>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Project skeleton ranked by importance (PageRank)
    Map {
        /// Number of top files to show
        #[arg(long, default_value = "20")]
        top_n: u32,
        /// Boost files containing these terms
        #[arg(long)]
        boost: Vec<String>,
        /// Show all files (ignores --top-n)
        #[arg(long)]
        all_files: bool,
        /// Ranking axis: files or symbols
        #[arg(long)]
        by: Option<String>,
    },

    /// Jump to a symbol definition by name
    Find {
        /// Symbol name to look up
        name: String,
        /// Filter by symbol kind (function, struct, class, etc.)
        #[arg(long)]
        kind: Option<String>,
    },

    /// Search indexed symbols by name
    Grep {
        /// Search query (supports prefix* matching)
        query: String,
        /// Max results
        #[arg(long, default_value = "20")]
        limit: u32,
        /// Search function bodies instead of symbol names
        #[arg(long)]
        bodies: bool,
        /// Interpret query as regex
        #[arg(long)]
        regex: bool,
    },

    /// Read symbol source code with line numbers
    Read {
        /// Symbol name to read (omit to read a file range)
        name: Option<String>,
        /// File path (disambiguates symbol or reads raw range)
        #[arg(long)]
        file: Option<String>,
        /// Start line (for file range reads)
        #[arg(long)]
        start: Option<u32>,
        /// End line (for file range reads)
        #[arg(long)]
        end: Option<u32>,
        /// Context lines before the symbol
        #[arg(long, default_value = "0")]
        context: u32,
    },

    /// File symbol table (table of contents)
    Outline {
        /// File path
        file: String,
    },

    /// Blast radius analysis before editing
    Impact {
        /// File path to analyze
        file: String,
        /// Include test files in blast radius
        #[arg(long)]
        include_tests: bool,
    },

    /// File dependency graph
    Deps {
        /// File path
        file: String,
    },

    /// Project or per-file metrics
    Stats {
        /// Optional file path (omit for project-wide stats)
        file: Option<String>,
    },

    /// Dead exports and unreferenced symbols
    Unused {
        /// Max results to return
        #[arg(long, default_value = "50")]
        limit: u32,
    },

    /// All usages of a symbol across the codebase
    Refs {
        /// Symbol name
        name: String,
    },

    /// Complexity x coupling x churn ranking
    Hotspots,

    /// Call hierarchy (callers and callees)
    Calls {
        /// Symbol name
        name: String,
        /// Direction: callers, callees, or both
        #[arg(long, default_value = "both")]
        direction: String,
        /// Max depth
        #[arg(long, default_value = "2")]
        depth: u32,
    },

    /// Duplicate code detection via AST hashing
    Clones {
        /// Minimum number of source lines for a symbol to be considered
        /// (default: 8). Lower values surface smaller near-duplicates.
        #[arg(long)]
        min_lines: Option<u32>,
        /// Max number of clone groups to return (default: 20). Groups
        /// are sorted by size (most duplicates first).
        #[arg(long)]
        limit: Option<u32>,
        /// Page offset into the sorted group list (default: 0).
        #[arg(long)]
        offset: Option<u32>,
        /// Include clones that live in test files (default: excluded).
        #[arg(long)]
        include_tests: bool,
    },

    /// Architecture boundary rule violations
    Boundaries,

    /// Type/trait inheritance hierarchy
    Hierarchy {
        /// Type or trait name
        name: String,
    },

    /// Symbol complexity trend over git history
    Trend {
        /// File path to analyze
        file: String,
        /// Optional symbol name to filter to one function
        #[arg(long)]
        name: Option<String>,
    },

    /// Security analysis
    Security {
        /// Minimum severity threshold: low (default), medium, high, critical.
        #[arg(long)]
        severity: Option<String>,
        /// Filter by vulnerability category: secrets, injection, crypto,
        /// unsafe, info-leak, review.
        #[arg(long)]
        category: Option<String>,
        /// Scan only files whose path contains this substring.
        #[arg(long)]
        file: Option<String>,
        /// Include test/spec files in the scan (default: excluded).
        #[arg(long)]
        include_tests: bool,
        /// Max number of findings to return (default: 50). Sorted by risk.
        #[arg(long)]
        limit: Option<u32>,
        /// Skip the first N findings (for pagination).
        #[arg(long)]
        offset: Option<u32>,
        /// Path to a custom rules TOML, relative to the project root.
        #[arg(long)]
        config_path: Option<String>,
    },

    /// Files that historically change together
    Cochange {
        /// File path
        file: String,
    },

    /// Related files for a task (files you plan to modify)
    Context {
        /// File paths to analyze context for
        files: Vec<String>,
        /// Optional task description to help prioritize
        #[arg(long)]
        task: Option<String>,
    },

    /// Manage project domains (workspaces) dynamically
    Workspace {
        action: WorkspaceAction,
        /// The alias (domain name) for the project
        alias: String,
        /// The path to the project directory (required for 'add')
        path: Option<String>,
    },

    /// Local web dashboard (Project Pulse, live impact preview)
    Dashboard {
        #[command(subcommand)]
        action: qartez_dashboard::DashboardCommand,
    },
}
