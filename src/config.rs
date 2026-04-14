use std::path::{Path, PathBuf};

use crate::cli::Cli;
use crate::error::Result;

pub struct Config {
    pub project_roots: Vec<PathBuf>,
    pub primary_root: PathBuf,
    pub db_path: PathBuf,
    pub reindex: bool,
    pub git_depth: u32,
    pub has_project: bool,
}

const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
];

fn detect_project_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        for marker in PROJECT_MARKERS {
            if current.join(marker).exists() {
                return Some(current);
            }
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Scan immediate children of `dir` for project markers (e.g. `.git`).
/// Handles the meta-directory pattern where a folder groups multiple repos.
fn detect_child_project_roots(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut roots: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let has_marker = PROJECT_MARKERS.iter().any(|m| path.join(m).exists());
            has_marker.then_some(path)
        })
        .collect();

    roots.sort();
    roots
}

fn is_home_dir(path: &Path) -> bool {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .is_some_and(|home| path == home)
}

impl Config {
    pub fn from_cli(cli: &Cli) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let (project_roots, has_project) = if cli.root.is_empty() {
            let cwd_is_project =
                !is_home_dir(&cwd) && PROJECT_MARKERS.iter().any(|m| cwd.join(m).exists());
            if cwd_is_project {
                (vec![cwd.clone()], true)
            } else {
                // No markers in cwd — check for child project roots (meta-directory)
                let children = detect_child_project_roots(&cwd);
                if !children.is_empty() {
                    (children, true)
                } else {
                    // Walk up, but reject home directory to avoid indexing ~
                    match detect_project_root(&cwd) {
                        Some(root) if !is_home_dir(&root) => (vec![root], true),
                        _ => (vec![cwd.clone()], false),
                    }
                }
            }
        } else {
            (cli.root.clone(), true)
        };

        let primary_root = project_roots[0].clone();

        // For multi-root (meta-directory), store the database in cwd
        let db_anchor = if project_roots.len() > 1 { &cwd } else { &primary_root };
        let db_path = match &cli.db_path {
            Some(p) => p.clone(),
            None => db_anchor.join(".qartez").join("index.db"),
        };

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        Ok(Config {
            project_roots,
            primary_root,
            db_path,
            reindex: cli.reindex,
            git_depth: cli.git_depth,
            has_project,
        })
    }
}
