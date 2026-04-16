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

/// Detect workspace member directories from workspace config files.
///
/// Checks for npm/yarn/pnpm (`package.json` `"workspaces"`), Cargo
/// (`Cargo.toml` `[workspace] members`), and Go (`go.work` `use`
/// directives). Returned paths are absolute and sorted.
fn detect_workspace_members(root: &Path) -> Vec<PathBuf> {
    let mut members = Vec::new();
    members.extend(detect_npm_workspace(root));
    members.extend(detect_cargo_workspace(root));
    members.extend(detect_go_workspace(root));
    members.sort();
    members.dedup();
    members
}

/// Parse `package.json` `"workspaces"` field and expand globs.
fn detect_npm_workspace(root: &Path) -> Vec<PathBuf> {
    let pkg_path = root.join("package.json");
    let content = match std::fs::read_to_string(&pkg_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    // "workspaces" can be an array or an object with a "packages" key
    let patterns: Vec<&str> = match &json["workspaces"] {
        serde_json::Value::Array(arr) => arr.iter().filter_map(|v| v.as_str()).collect(),
        serde_json::Value::Object(obj) => match obj.get("packages") {
            Some(serde_json::Value::Array(arr)) => arr.iter().filter_map(|v| v.as_str()).collect(),
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };

    expand_workspace_globs(root, &patterns)
}

/// Parse `Cargo.toml` `[workspace] members` and expand globs.
fn detect_cargo_workspace(root: &Path) -> Vec<PathBuf> {
    let cargo_path = root.join("Cargo.toml");
    let content = match std::fs::read_to_string(&cargo_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let doc: toml_edit::DocumentMut = match content.parse() {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let members = match doc
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
    {
        Some(arr) => arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>(),
        None => return Vec::new(),
    };

    expand_workspace_globs(root, &members)
}

/// Parse `go.work` `use` directives.
fn detect_go_workspace(root: &Path) -> Vec<PathBuf> {
    let go_work = root.join("go.work");
    let content = match std::fs::read_to_string(&go_work) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut results = Vec::new();
    let mut in_use_block = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "use (" {
            in_use_block = true;
            continue;
        }
        if in_use_block && trimmed == ")" {
            in_use_block = false;
            continue;
        }

        let dir = if in_use_block {
            trimmed.trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace())
        } else if let Some(rest) = trimmed.strip_prefix("use ") {
            rest.trim().trim_matches(|c: char| c == '"' || c == '\'')
        } else {
            continue;
        };

        if dir.is_empty() || dir.starts_with("//") {
            continue;
        }
        let abs = root.join(dir);
        if abs.is_dir() {
            results.push(abs);
        }
    }

    results
}

/// Expand workspace glob patterns (e.g. `"packages/*"`) relative to a root
/// directory. Splits each pattern into a literal parent and a glob tail,
/// walks the parent directory, and matches entries with `globset`. Only
/// returns directories that actually exist on disk.
fn expand_workspace_globs(root: &Path, patterns: &[&str]) -> Vec<PathBuf> {
    let mut results = Vec::new();
    for pattern in patterns {
        let pat_path = Path::new(pattern);

        // If the pattern has no glob characters, treat it as a literal path
        if !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[') {
            let candidate = root.join(pattern);
            if candidate.is_dir() {
                results.push(candidate);
            }
            continue;
        }

        // Split into literal parent dir and glob filename component.
        // e.g. "packages/*" -> parent="packages", glob_part="*"
        let (parent_rel, glob_part) = match pat_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => (
                p.to_string_lossy().to_string(),
                pat_path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default(),
            ),
            _ => (String::new(), pattern.to_string()),
        };

        let scan_dir = if parent_rel.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&parent_rel)
        };

        let Ok(entries) = std::fs::read_dir(&scan_dir) else {
            continue;
        };

        let Ok(matcher) = globset::GlobBuilder::new(&glob_part)
            .literal_separator(true)
            .build()
            .map(|g| g.compile_matcher())
        else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name();
            if matcher.is_match(name.to_string_lossy().as_ref()) {
                results.push(path);
            }
        }
    }
    results
}

/// For each root, check if it has a workspace config and expand its members
/// into additional roots. The original root is kept (it may contain shared
/// config, scripts, etc.), and members are appended after it. Duplicates
/// are removed.
fn expand_roots_with_workspaces(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut expanded = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in &roots {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
        if seen.insert(canonical.clone()) {
            expanded.push(root.clone());
        }
        for member in detect_workspace_members(root) {
            let member_canonical = member.canonicalize().unwrap_or_else(|_| member.clone());
            if seen.insert(member_canonical) {
                expanded.push(member);
            }
        }
    }
    expanded
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
                // No markers in cwd - check for child project roots (meta-directory)
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

        // Expand workspace members: if any root has a workspace config
        // (npm, Cargo, Go), add member directories as additional roots.
        let project_roots = expand_roots_with_workspaces(project_roots);

        let primary_root = project_roots[0].clone();

        // For multi-root (meta-directory), store the database in cwd
        let db_anchor = if project_roots.len() > 1 {
            &cwd
        } else {
            &primary_root
        };
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
