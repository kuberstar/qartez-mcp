//! `qartez-guard` - PreToolUse hook binary for Claude Code.
//!
//! Reads a Claude Code PreToolUse hook payload on stdin. If the target file
//! is load-bearing (PageRank or transitive blast radius above the configured
//! threshold) and no recent `qartez_impact` acknowledgment exists, prints a
//! `permissionDecision: "deny"` JSON envelope to stdout so Claude is forced
//! to run `qartez_impact` first. Otherwise exits 0 silently.
//!
//! The hook is intentionally fail-open: any unexpected condition (missing
//! index, unparseable stdin, file outside the project, read-only DB error)
//! short-circuits to "allow" so the guard can never wedge an edit session.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use rusqlite::{Connection, OpenFlags};

use qartez_mcp::graph::blast;
use qartez_mcp::guard::{self, FileFacts, GuardConfig, HookInput};
use qartez_mcp::storage::read;

#[derive(Parser, Debug)]
#[command(
    name = "qartez-guard",
    about = "Pre-tool-use modification guard for qartez-mcp"
)]
struct Cli {
    /// Override the qartez SQLite database path. Defaults to
    /// `<project_root>/.qartez/index.db`, discovered by walking up from cwd.
    #[arg(long)]
    db: Option<PathBuf>,

    /// Override the project root used for ack files and path relativization.
    /// Defaults to `$CLAUDE_PROJECT_DIR` or auto-detection.
    #[arg(long)]
    project_root: Option<PathBuf>,

    /// PageRank threshold (env: `QARTEZ_GUARD_PAGERANK_MIN`).
    #[arg(long)]
    pagerank_min: Option<f64>,

    /// Blast radius threshold (env: `QARTEZ_GUARD_BLAST_MIN`).
    #[arg(long)]
    blast_min: Option<i64>,

    /// Ack TTL in seconds (env: `QARTEZ_GUARD_ACK_TTL_SECS`).
    #[arg(long)]
    ack_ttl_secs: Option<u64>,
}

fn main() -> ExitCode {
    // Fail-open wrapper: run() only returns Err for truly unexpected states;
    // the caller's intent is that any guard failure must not block Edit.
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("qartez-guard: {e:#}");
            ExitCode::SUCCESS
        }
    }
}

fn run() -> anyhow::Result<()> {
    if GuardConfig::is_disabled_by_env() {
        return Ok(());
    }

    let cli = Cli::parse();
    let cfg = merge_config(&cli);

    let mut stdin_buf = String::new();
    std::io::stdin().read_to_string(&mut stdin_buf)?;
    let hook: HookInput = match serde_json::from_str(&stdin_buf) {
        Ok(h) => h,
        Err(_) => return Ok(()),
    };

    if !matches!(
        hook.tool_name.as_str(),
        "Edit" | "Write" | "MultiEdit" | "replace" | "write_file"
    ) {
        return Ok(());
    }
    let hook_cwd = hook.cwd.clone();
    let Some(file_path_raw) = hook.tool_input.file_path else {
        return Ok(());
    };
    let file_path = PathBuf::from(&file_path_raw);

    let project_root = match cli.project_root.clone() {
        Some(r) => r,
        None => match resolve_project_root(hook_cwd.as_deref(), &file_path) {
            Some(r) => r,
            None => return Ok(()),
        },
    };
    let db_path = cli
        .db
        .clone()
        .unwrap_or_else(|| project_root.join(".qartez").join("index.db"));
    if !db_path.is_file() {
        return Ok(());
    }

    let Some(rel_path) = guard::relativize_file_path(&project_root, &file_path) else {
        return Ok(());
    };

    let conn = match open_read_only(&db_path) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    let Some(file_row) = read::get_file_by_path(&conn, &rel_path).ok().flatten() else {
        return Ok(());
    };

    let blast_count = blast::blast_radius_for_file(&conn, file_row.id)
        .map(|r| r.transitive_count as i64)
        .unwrap_or(0);

    // Top hot symbols inside this file - powers the enriched deny message
    // so Claude sees exactly which symbols the guard thinks are load-bearing
    // before it decides to call `qartez_impact`. Fail-open: an error here must
    // never block the edit, so swallow and continue with an empty list.
    let hot_symbols: Vec<(String, f64)> = read::get_symbols_ranked_for_file(&conn, file_row.id, 3)
        .map(|rows| {
            rows.into_iter()
                .filter(|s| s.pagerank > 0.0)
                .map(|s| (s.name, s.pagerank))
                .collect()
        })
        .unwrap_or_default();

    let facts = FileFacts {
        rel_path: rel_path.clone(),
        pagerank: file_row.pagerank,
        blast_radius: blast_count,
        hot_symbols,
    };
    let ack_fresh = guard::ack_is_fresh(&project_root, &rel_path, cfg.ack_ttl_secs);
    let decision = guard::evaluate(&facts, &cfg, ack_fresh);

    if let Some(json) = guard::render_stdout(&decision, hook.hook_event_name.as_deref()) {
        println!("{json}");
    }
    Ok(())
}

fn merge_config(cli: &Cli) -> GuardConfig {
    let mut cfg = GuardConfig::from_env();
    if let Some(v) = cli.pagerank_min
        && v.is_finite()
        && v >= 0.0
    {
        cfg.pagerank_min = v;
    }
    if let Some(v) = cli.blast_min
        && v >= 0
    {
        cfg.blast_min = v;
    }
    if let Some(v) = cli.ack_ttl_secs {
        cfg.ack_ttl_secs = v;
    }
    cfg
}

fn resolve_project_root(hook_cwd: Option<&str>, file_path: &std::path::Path) -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("CLAUDE_PROJECT_DIR")
        && !explicit.is_empty()
    {
        let candidate = PathBuf::from(explicit);
        if candidate.join(".qartez").join("index.db").is_file() {
            return Some(candidate);
        }
    }
    if let Some(cwd) = hook_cwd
        && let Some(root) = guard::find_project_root(std::path::Path::new(cwd))
    {
        return Some(root);
    }
    if let Some(parent) = file_path.parent()
        && let Some(root) = guard::find_project_root(parent)
    {
        return Some(root);
    }
    std::env::current_dir()
        .ok()
        .and_then(|d| guard::find_project_root(&d))
}

fn open_read_only(db_path: &std::path::Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // busy_timeout still works on read-only connections and protects against
    // concurrent writers holding a WAL lock briefly during indexing.
    conn.busy_timeout(std::time::Duration::from_millis(500))?;
    Ok(conn)
}
