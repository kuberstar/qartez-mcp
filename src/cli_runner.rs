// Rust guideline compliant 2026-04-16
//
// Standalone CLI execution layer. Translates clap subcommands into
// `QartezServer::call_tool_by_name` calls, runs synchronous indexing,
// and formats output for terminal use.

use std::io::{self, IsTerminal, Write};

use anyhow::{Context, Result};
use console::Style;
use serde_json::json;

use crate::cli::{Command, OutputFormat};
use crate::config::Config;
use crate::server::QartezServer;
use crate::{git, graph, index, storage};

/// Run a CLI subcommand end-to-end: index, dispatch, format, print.
///
/// Returns `Ok(())` on success, propagating indexing or tool errors.
pub fn run(config: &Config, command: &Command, format: OutputFormat) -> Result<()> {
    let conn = storage::open_db(&config.db_path)?;

    if config.has_project {
        eprintln!("Indexing...");
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
        eprintln!("Index ready.");
    }

    let server = QartezServer::with_roots(
        conn,
        config.primary_root.clone(),
        config.project_roots.clone(),
        config.root_aliases.clone(),
        config.git_depth,
    );

    let (tool_name, args) = build_tool_call(command);

    let output = server
        .call_tool_by_name(&tool_name, args)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let format = resolve_format(format);
    print_output(&tool_name, &output, format)?;

    Ok(())
}

/// Pick human format when stdout is a TTY, compact otherwise.
fn resolve_format(explicit: OutputFormat) -> OutputFormat {
    match explicit {
        // If the user did not override, auto-detect
        f @ (OutputFormat::Json | OutputFormat::Compact) => f,
        OutputFormat::Human => {
            if io::stdout().is_terminal() {
                OutputFormat::Human
            } else {
                OutputFormat::Compact
            }
        }
    }
}

/// Translate a CLI subcommand into (tool_name, json_args).
fn build_tool_call(command: &Command) -> (String, serde_json::Value) {
    match command {
        Command::Map {
            top_n,
            boost,
            all_files,
            by,
        } => {
            let mut args = json!({ "top_n": top_n });
            if !boost.is_empty() {
                args["boost_terms"] = json!(boost);
            }
            if *all_files {
                args["all_files"] = json!(true);
            }
            if let Some(by) = by {
                args["by"] = json!(by);
            }
            ("qartez_map".into(), args)
        }

        Command::Find { name, kind } => {
            let mut args = json!({ "name": name });
            if let Some(k) = kind {
                args["kind"] = json!(k);
            }
            ("qartez_find".into(), args)
        }

        Command::Grep {
            query,
            limit,
            bodies,
            regex,
        } => {
            let mut args = json!({ "query": query, "limit": limit });
            if *bodies {
                args["search_bodies"] = json!(true);
            }
            if *regex {
                args["regex"] = json!(true);
            }
            ("qartez_grep".into(), args)
        }

        Command::Read {
            name,
            file,
            start,
            end,
            context,
        } => {
            let mut args = json!({});
            if let Some(n) = name {
                args["symbol_name"] = json!(n);
            }
            if let Some(f) = file {
                args["file_path"] = json!(f);
            }
            if let Some(s) = start {
                args["start_line"] = json!(s);
            }
            if let Some(e) = end {
                args["end_line"] = json!(e);
            }
            if *context > 0 {
                args["context_lines"] = json!(context);
            }
            ("qartez_read".into(), args)
        }

        Command::Outline { file } => ("qartez_outline".into(), json!({ "file_path": file })),

        Command::Impact {
            file,
            include_tests,
        } => {
            let mut args = json!({ "file_path": file });
            if *include_tests {
                args["include_tests"] = json!(true);
            }
            ("qartez_impact".into(), args)
        }

        Command::Deps { file } => ("qartez_deps".into(), json!({ "file_path": file })),

        Command::Stats { file } => {
            let args = match file {
                Some(f) => json!({ "file_path": f }),
                None => json!({}),
            };
            ("qartez_stats".into(), args)
        }

        Command::Unused { limit } => ("qartez_unused".into(), json!({ "limit": limit })),

        Command::Refs { name } => ("qartez_refs".into(), json!({ "symbol": name })),

        Command::Hotspots => ("qartez_hotspots".into(), json!({})),

        Command::Calls {
            name,
            direction,
            depth,
        } => {
            let args = json!({
                "name": name,
                "direction": direction,
                "depth": depth,
            });
            ("qartez_calls".into(), args)
        }

        Command::Clones => ("qartez_clones".into(), json!({})),

        Command::Boundaries => ("qartez_boundaries".into(), json!({})),

        Command::Hierarchy { name } => ("qartez_hierarchy".into(), json!({ "symbol": name })),

        Command::Trend { file, name } => {
            let mut args = json!({ "file_path": file });
            if let Some(n) = name {
                args["symbol_name"] = json!(n);
            }
            ("qartez_trend".into(), args)
        }

        Command::Security => ("qartez_security".into(), json!({})),

        Command::Cochange { file } => ("qartez_cochange".into(), json!({ "file_path": file })),

        Command::Context { files, task } => {
            let mut args = json!({ "files": files });
            if let Some(t) = task {
                args["task"] = json!(t);
            }
            ("qartez_context".into(), args)
        }

        Command::Workspace {
            action,
            alias,
            path,
        } => {
            let args = json!({
                "action": action,
                "alias": alias,
                "path": path,
            });
            ("qartez_workspace".into(), args)
        }
    }
}

/// Format and print tool output based on the selected format.
fn print_output(tool_name: &str, output: &str, format: OutputFormat) -> Result<()> {
    let mut stdout = io::stdout().lock();
    match format {
        OutputFormat::Json => {
            let payload = json!({
                "tool": tool_name,
                "output": output,
            });
            writeln!(stdout, "{}", serde_json::to_string_pretty(&payload)?)
                .context("write to stdout")?;
        }
        OutputFormat::Human => {
            print_human(&mut stdout, output)?;
        }
        OutputFormat::Compact => {
            print_compact(&mut stdout, output)?;
        }
    }
    Ok(())
}

/// Human-readable output with light color highlights.
fn print_human(w: &mut impl Write, output: &str) -> Result<()> {
    let header_style = Style::new().bold().cyan();
    let separator_style = Style::new().dim();

    for line in output.lines() {
        if line.starts_with("# ") || line.starts_with("## ") {
            writeln!(w, "{}", header_style.apply_to(line))?;
        } else if line.starts_with("---") || line.starts_with("===") {
            writeln!(w, "{}", separator_style.apply_to(line))?;
        } else {
            writeln!(w, "{line}")?;
        }
    }
    Ok(())
}

/// Compact output: strip blank lines and markdown headers.
fn print_compact(w: &mut impl Write, output: &str) -> Result<()> {
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Strip leading markdown header markers
        let stripped = trimmed
            .strip_prefix("### ")
            .or_else(|| trimmed.strip_prefix("## "))
            .or_else(|| trimmed.strip_prefix("# "))
            .unwrap_or(trimmed);
        writeln!(w, "{stripped}")?;
    }
    Ok(())
}
