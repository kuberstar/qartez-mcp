//! Simulates the non-MCP workflow: `Glob`, `Grep`, `Read`, and external
//! commands that a Claude Code agent would shell out to when the Qartez
//! MCP server is unavailable.
//!
//! The output format is chosen to match Claude Code's tool output as
//! closely as possible so that token counts are a faithful comparison:
//!
//! - `Glob`  => `{rel_path}\n` per matching file
//! - `Grep` (files mode)   => `{rel_path}\n` per matching file
//! - `Grep` (content mode) => `{rel_path}:{lineno}:{line}\n` per match
//! - `Read` => `{lineno:>6}\t{line}\n` per line (Claude Code's `cat -n`
//!   style)
//! - `GitLog` => raw `git log` stdout

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use ignore::WalkBuilder;
use regex::Regex;

use super::scenarios::SimStep;

#[derive(Debug)]
pub enum SimError {
    Regex(String),
    Io(String),
    Git(String),
}

impl fmt::Display for SimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Regex(s) => write!(f, "regex compile error: {s}"),
            Self::Io(s) => write!(f, "io error: {s}"),
            Self::Git(s) => write!(f, "git error: {s}"),
        }
    }
}

impl std::error::Error for SimError {}

/// Extra filters applied on top of an individual step's `ext_filter`.
///
/// The scenario machinery builds one [`Options`] per run from the active
/// language profile, then threads it through every `SimStep`. Steps that
/// walk the file system (Glob/Grep) respect both the step's own
/// extension filter and these profile-level exclude globs so that e.g.
/// TypeScript runs do not mine `node_modules/**`.
#[derive(Debug, Clone, Default)]
pub struct Options<'a> {
    pub exclude_globs: &'a [&'a str],
}

/// Run the non-MCP step sequence with no profile-level excludes. Used
/// by existing call sites that do not thread a profile through (e.g.
/// warm-up loops that care only about side-effects).
pub fn run(project_root: &Path, steps: &[SimStep]) -> Result<String, SimError> {
    run_with(project_root, steps, &Options::default())
}

pub fn run_with(
    project_root: &Path,
    steps: &[SimStep],
    opts: &Options<'_>,
) -> Result<String, SimError> {
    let mut out = String::new();
    for step in steps {
        execute_step(project_root, step, opts, &mut out)?;
    }
    Ok(out)
}

fn execute_step(
    root: &Path,
    step: &SimStep,
    opts: &Options<'_>,
    out: &mut String,
) -> Result<(), SimError> {
    match step {
        SimStep::Glob { ext_filter } => {
            for path in walk_files(root, ext_filter.as_deref(), opts.exclude_globs) {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push_str(&rel.display().to_string());
                    out.push('\n');
                }
            }
            Ok(())
        }
        SimStep::GrepFiles { regex, ext_filter } => {
            let re = Regex::new(regex).map_err(|e| SimError::Regex(e.to_string()))?;
            for path in walk_files(root, ext_filter.as_deref(), opts.exclude_globs) {
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                if re.is_match(&text)
                    && let Ok(rel) = path.strip_prefix(root)
                {
                    out.push_str(&rel.display().to_string());
                    out.push('\n');
                }
            }
            Ok(())
        }
        SimStep::GrepContent { regex, ext_filter } => {
            let re = Regex::new(regex).map_err(|e| SimError::Regex(e.to_string()))?;
            for path in walk_files(root, ext_filter.as_deref(), opts.exclude_globs) {
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                for (idx, line) in text.lines().enumerate() {
                    if re.is_match(line) {
                        out.push_str(&rel);
                        out.push(':');
                        out.push_str(&(idx + 1).to_string());
                        out.push(':');
                        out.push_str(line);
                        out.push('\n');
                    }
                }
            }
            Ok(())
        }
        SimStep::Read { path, range } => {
            let abs = root.join(path);
            let text = fs::read_to_string(&abs)
                .map_err(|e| SimError::Io(format!("{}: {e}", abs.display())))?;
            let (start, end) = range.unwrap_or((1, usize::MAX));
            for (idx, line) in text.lines().enumerate() {
                let line_no = idx + 1;
                if line_no < start {
                    continue;
                }
                if line_no > end {
                    break;
                }
                // Matches Claude Code's Read output: right-aligned
                // 6-digit line number, tab separator, line text.
                // Byte-identical to what the non-MCP agent would
                // actually see.
                out.push_str(&format!("{line_no:>6}\t{line}\n"));
            }
            Ok(())
        }
        SimStep::GitLog { file, limit } => {
            let output = Command::new("git")
                .current_dir(root)
                .arg("log")
                .arg("--name-only")
                .arg("--pretty=format:---%n")
                .arg(format!("-n{limit}"))
                .arg("--")
                .arg(file)
                .output()
                .map_err(|e| SimError::Git(e.to_string()))?;
            if !output.status.success() {
                return Err(SimError::Git(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ));
            }
            out.push_str(&String::from_utf8_lossy(&output.stdout));
            out.push('\n');
            Ok(())
        }
        SimStep::BashOutput { bytes } => {
            // Representative padding used when the non-MCP equivalent
            // would need a multi-step workflow whose full byte cost
            // cannot be reproduced in-process (e.g. git history mining
            // for co-change inference). The scenario annotates what the
            // bytes stand for; this keeps the accounting honest without
            // fabricating output.
            out.reserve(*bytes);
            for _ in 0..*bytes {
                out.push(' ');
            }
            out.push('\n');
            Ok(())
        }
        SimStep::ImpactBfs {
            seed,
            depth,
            ext_filter,
        } => impact_bfs(
            root,
            seed,
            *depth,
            ext_filter.as_deref(),
            opts.exclude_globs,
            out,
        ),
        SimStep::GitCoChange {
            target_file,
            limit,
            top_n,
        } => git_cochange(root, target_file, *limit, *top_n, out),
    }
}

/// Seeded BFS over crate-level imports. Starts from `seed` (typically
/// the module path of the target file), greps `use crate::{seed}` to
/// find direct importers, then for each importer derives its own crate
/// stem and greps for that, recursing up to `depth` levels. Result bytes
/// represent what a non-MCP agent would actually see while chasing
/// transitive blast radius.
fn impact_bfs(
    root: &Path,
    seed: &str,
    depth: u32,
    ext_filter: Option<&[String]>,
    exclude_globs: &[&str],
    out: &mut String,
) -> Result<(), SimError> {
    let files = walk_files(root, ext_filter, exclude_globs);
    let mut contents: HashMap<PathBuf, String> = HashMap::new();
    for f in &files {
        if let Ok(t) = fs::read_to_string(f) {
            contents.insert(f.clone(), t);
        }
    }

    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: Vec<(String, u32)> = vec![(seed.to_string(), 0)];
    while let Some((stem, level)) = queue.pop() {
        if !visited.insert(stem.clone()) {
            continue;
        }
        let needle = format!("use crate::{stem}");
        let re = Regex::new(&regex::escape(&needle)).map_err(|e| SimError::Regex(e.to_string()))?;
        let mut next_stems: Vec<String> = Vec::new();
        for (path, text) in &contents {
            if !re.is_match(text) {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string();
            for (idx, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    out.push_str(&rel);
                    out.push(':');
                    out.push_str(&(idx + 1).to_string());
                    out.push(':');
                    out.push_str(line);
                    out.push('\n');
                }
            }
            if level + 1 < depth
                && let Some(s) = path_to_crate_stem(&rel)
            {
                next_stems.push(s);
            }
        }
        for s in next_stems {
            queue.push((s, level + 1));
        }
    }
    Ok(())
}

fn path_to_crate_stem(rel_path: &str) -> Option<String> {
    let without_prefix = rel_path.strip_prefix("src/").unwrap_or(rel_path);
    let base = without_prefix
        .rsplit_once('.')
        .map(|(b, _)| b)
        .unwrap_or(without_prefix);
    let stem = base.replace('/', "::");
    let stem = stem
        .strip_suffix("::mod")
        .map(str::to_string)
        .unwrap_or(stem);
    if stem.is_empty() { None } else { Some(stem) }
}

/// Faithful sim of co-change analysis for `target_file`:
///   1. `git log --name-only --pretty=format:%H -n{limit}`
///   2. Group filenames by commit hash
///   3. For each commit containing `target_file`, increment pair counts
///      for every other file touched
///   4. Print top `top_n` partners descending
///
/// Output is sized to represent what a non-MCP agent would actually see:
/// the raw `git log` stream plus the aggregated top-N rows.
fn git_cochange(
    root: &Path,
    target_file: &str,
    limit: u32,
    top_n: u32,
    out: &mut String,
) -> Result<(), SimError> {
    let output = Command::new("git")
        .current_dir(root)
        .arg("log")
        .arg("--name-only")
        .arg("--pretty=format:%H")
        .arg(format!("-n{limit}"))
        .output()
        .map_err(|e| SimError::Git(e.to_string()))?;
    if !output.status.success() {
        return Err(SimError::Git(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    out.push_str(&raw);
    out.push('\n');

    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut current_files: Vec<String> = Vec::new();
    let mut in_commit = false;
    for line in raw.lines() {
        if line.len() == 40 && line.chars().all(|c| c.is_ascii_hexdigit()) {
            if in_commit && current_files.iter().any(|f| f == target_file) {
                for f in &current_files {
                    if f != target_file {
                        *counts.entry(f.clone()).or_insert(0) += 1;
                    }
                }
            }
            current_files.clear();
            in_commit = true;
        } else if !line.is_empty() {
            current_files.push(line.to_string());
        }
    }
    if in_commit && current_files.iter().any(|f| f == target_file) {
        for f in &current_files {
            if f != target_file {
                *counts.entry(f.clone()).or_insert(0) += 1;
            }
        }
    }

    let mut pairs: Vec<(String, u32)> = counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs.truncate(top_n as usize);

    out.push_str(&format!("# Co-change pairs for {target_file}:\n"));
    for (path, count) in &pairs {
        out.push_str(&format!("  {path}: {count}\n"));
    }
    Ok(())
}

/// Lists files under `root` subject to a per-step extension filter and a
/// profile-level exclude-globs list. An `ext_filter` of `None` accepts
/// any extension; an empty slice behaves identically to `None` so
/// callers that build the filter from
/// [`super::profiles::LanguageProfile::extensions`] do not have to
/// special-case zero-extension profiles.
fn walk_files(root: &Path, ext_filter: Option<&[String]>, exclude_globs: &[&str]) -> Vec<PathBuf> {
    let compiled_excludes: Vec<glob::Pattern> = exclude_globs
        .iter()
        .filter_map(|g| glob::Pattern::new(g).ok())
        .collect();

    WalkBuilder::new(root)
        .standard_filters(true)
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter(|entry| match ext_filter {
            None | Some([]) => true,
            Some(exts) => entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|ext| exts.iter().any(|want| want == ext))
                .unwrap_or(false),
        })
        .filter(|entry| {
            if compiled_excludes.is_empty() {
                return true;
            }
            let Ok(rel) = entry.path().strip_prefix(root) else {
                return true;
            };
            let rel_str = rel.to_string_lossy();
            !compiled_excludes.iter().any(|p| p.matches(&rel_str))
        })
        .map(|entry| entry.into_path())
        .collect()
}
