use std::path::Path;

use anyhow::{Result, anyhow};
use git2::Repository;
use tracing::debug;

use crate::index::parser::ParserPool;

/// Maximum number of commits to walk when computing a trend.
const MAX_COMMIT_LIMIT: u32 = 50;

/// A single data point in a complexity trend: one commit's snapshot of a symbol.
#[derive(Debug, Clone)]
pub struct TrendPoint {
    pub commit_sha: String,
    pub commit_summary: String,
    pub complexity: u32,
    pub line_count: u32,
}

/// Complexity trend for a single symbol across multiple commits.
#[derive(Debug, Clone)]
pub struct SymbolTrend {
    pub symbol_name: String,
    pub file_path: String,
    pub points: Vec<TrendPoint>,
}

/// Compute complexity trend for symbols in a file by walking git history.
///
/// For each of the last `limit` commits that touched `file_path`, extracts the
/// file content at that revision, parses it with tree-sitter, and records the
/// cyclomatic complexity of every symbol matching `symbol_filter` (or all
/// symbols when `None`).
///
/// Returns one `SymbolTrend` per distinct symbol name found. Points are ordered
/// oldest-first (chronological).
pub fn complexity_trend(
    root: &Path,
    file_path: &str,
    symbol_filter: Option<&str>,
    limit: u32,
) -> Result<Vec<SymbolTrend>> {
    let limit = limit.min(MAX_COMMIT_LIMIT);
    let repo = Repository::open(root)?;
    let head = repo.head().map_err(|e| anyhow!("cannot read HEAD: {e}"))?;
    let head_oid = head.target().ok_or_else(|| anyhow!("HEAD has no target"))?;

    let commits = commits_touching_file(&repo, head_oid, file_path, limit)?;
    if commits.is_empty() {
        return Ok(Vec::new());
    }

    let pool = ParserPool::new();

    // symbol_name -> Vec<TrendPoint>, preserving insertion order via a separate
    // key list so the output is deterministic.
    let mut trends: std::collections::HashMap<String, Vec<TrendPoint>> =
        std::collections::HashMap::new();
    let mut seen_names: Vec<String> = Vec::new();

    // Walk commits oldest-first so the trend reads chronologically.
    for (sha, summary) in commits.iter().rev() {
        let source = match file_content_at_revision(&repo, sha, file_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let fake_path = Path::new(file_path);
        let parse_result = match pool.parse_file(fake_path, &source) {
            Ok((pr, _lang)) => pr,
            Err(e) => {
                debug!(
                    file = file_path,
                    sha = sha.as_str(),
                    error = %e,
                    "skipping unparsable revision"
                );
                continue;
            }
        };

        for sym in &parse_result.symbols {
            let cc = match sym.complexity {
                Some(c) => c,
                None => continue,
            };

            if let Some(filter) = symbol_filter
                && sym.name != filter
            {
                continue;
            }

            let line_count = sym.line_end.saturating_sub(sym.line_start) + 1;

            let point = TrendPoint {
                commit_sha: short_sha(sha),
                commit_summary: summary.clone(),
                complexity: cc,
                line_count,
            };

            let entry = trends.entry(sym.name.clone()).or_default();
            if !seen_names.contains(&sym.name) {
                seen_names.push(sym.name.clone());
            }
            entry.push(point);
        }
    }

    let result = seen_names
        .into_iter()
        .filter_map(|name| {
            let points = trends.remove(&name)?;
            if points.len() < 2 {
                return None;
            }
            Some(SymbolTrend {
                symbol_name: name,
                file_path: file_path.to_string(),
                points,
            })
        })
        .collect();

    Ok(result)
}

/// Walk git history and return commits (newest-first) that touched `file_path`.
fn commits_touching_file(
    repo: &Repository,
    head_oid: git2::Oid,
    file_path: &str,
    limit: u32,
) -> Result<Vec<(String, String)>> {
    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(git2::Sort::TIME | git2::Sort::TOPOLOGICAL)?;
    revwalk.push(head_oid)?;

    let mut result = Vec::new();

    for oid_result in revwalk {
        if result.len() as u32 >= limit {
            break;
        }

        let oid = match oid_result {
            Ok(o) => o,
            Err(_) => continue,
        };

        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if !commit_touches_file(&commit, file_path) {
            continue;
        }

        let sha = oid.to_string();
        let summary = commit
            .summary()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect::<String>();

        result.push((sha, summary));
    }

    Ok(result)
}

/// Check whether a commit modified the given file path.
fn commit_touches_file(commit: &git2::Commit, file_path: &str) -> bool {
    let tree = match commit.tree() {
        Ok(t) => t,
        Err(_) => return false,
    };

    // File must exist in this commit's tree.
    if tree.get_path(Path::new(file_path)).is_err() {
        return false;
    }

    // For the initial commit (no parents), the file existing is enough.
    if commit.parent_count() == 0 {
        return true;
    }

    // Check if any parent has a different blob for this path.
    for i in 0..commit.parent_count() {
        let parent = match commit.parent(i) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let parent_tree = match parent.tree() {
            Ok(t) => t,
            Err(_) => return true,
        };

        let current_entry = tree.get_path(Path::new(file_path));
        let parent_entry = parent_tree.get_path(Path::new(file_path));

        match (current_entry, parent_entry) {
            (Ok(cur), Ok(par)) => {
                if cur.id() != par.id() {
                    return true;
                }
            }
            (Ok(_), Err(_)) => return true,
            _ => {}
        }
    }

    false
}

/// Extract file content from a specific git revision.
fn file_content_at_revision(repo: &Repository, sha: &str, file_path: &str) -> Result<Vec<u8>> {
    let oid = git2::Oid::from_str(sha)?;
    let commit = repo.find_commit(oid)?;
    let tree = commit.tree()?;
    let entry = tree
        .get_path(Path::new(file_path))
        .map_err(|e| anyhow!("file not found at {sha}: {e}"))?;
    let blob = repo
        .find_blob(entry.id())
        .map_err(|e| anyhow!("cannot read blob at {sha}: {e}"))?;
    Ok(blob.content().to_vec())
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use git2::{Repository, Signature};

    fn init_repo(dir: &Path) -> Repository {
        Repository::init(dir).expect("failed to init repo")
    }

    fn make_commit(repo: &Repository, dir: &Path, files: &[(&str, &str)], message: &str) {
        for &(name, content) in files {
            let full = dir.join(name);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).ok();
            }
            fs::write(&full, content).expect("failed to write file");
        }

        let mut index = repo.index().expect("failed to get index");
        for &(name, _) in files {
            index.add_path(Path::new(name)).expect("failed to add path");
        }
        index.write().expect("failed to write index");

        let tree_oid = index.write_tree().expect("failed to write tree");
        let tree = repo.find_tree(tree_oid).expect("failed to find tree");
        let sig = Signature::now("test", "test@test.com").expect("failed to create sig");

        let parents: Vec<git2::Commit<'_>> = match repo.head() {
            Ok(head) => vec![
                head.peel_to_commit()
                    .expect("failed to peel HEAD to commit"),
            ],
            Err(_) => vec![],
        };
        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();

        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .expect("failed to create commit");
    }

    #[test]
    fn trend_for_rust_function() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo(dir.path());

        // Commit 1: simple function, CC=1 (no branches).
        make_commit(
            &repo,
            dir.path(),
            &[("lib.rs", "pub fn greet() -> &'static str { \"hello\" }\n")],
            "v1: simple function",
        );

        // Commit 2: add an if branch, CC=2.
        make_commit(
            &repo,
            dir.path(),
            &[(
                "lib.rs",
                "pub fn greet(loud: bool) -> &'static str {\n    if loud { \"HELLO\" } else { \"hello\" }\n}\n",
            )],
            "v2: add branching",
        );

        // Commit 3: add a match with two arms, CC=4.
        make_commit(
            &repo,
            dir.path(),
            &[(
                "lib.rs",
                concat!(
                    "pub fn greet(mode: u8) -> &'static str {\n",
                    "    if mode == 0 { return \"silent\"; }\n",
                    "    match mode {\n",
                    "        1 => \"hello\",\n",
                    "        2 => \"HELLO\",\n",
                    "        _ => \"hey\",\n",
                    "    }\n",
                    "}\n",
                ),
            )],
            "v3: add match",
        );

        let trends = complexity_trend(dir.path(), "lib.rs", Some("greet"), 10).unwrap();
        assert_eq!(trends.len(), 1);

        let t = &trends[0];
        assert_eq!(t.symbol_name, "greet");
        assert_eq!(t.points.len(), 3);

        // Oldest first: complexity should increase.
        assert!(t.points[0].complexity <= t.points[1].complexity);
        assert!(t.points[1].complexity <= t.points[2].complexity);
    }

    #[test]
    fn trend_no_git_history() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No repo initialized.
        let result = complexity_trend(dir.path(), "foo.rs", None, 10);
        assert!(result.is_err());
    }

    #[test]
    fn trend_nonexistent_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo(dir.path());
        make_commit(&repo, dir.path(), &[("a.rs", "pub fn a() {}\n")], "init");

        let trends = complexity_trend(dir.path(), "nonexistent.rs", None, 10).unwrap();
        assert!(trends.is_empty());
    }

    #[test]
    fn trend_symbol_filter_excludes_others() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo(dir.path());

        make_commit(
            &repo,
            dir.path(),
            &[(
                "lib.rs",
                "pub fn foo() -> bool { true }\npub fn bar() -> bool { false }\n",
            )],
            "v1",
        );
        make_commit(
            &repo,
            dir.path(),
            &[(
                "lib.rs",
                "pub fn foo() -> bool { if true { true } else { false } }\npub fn bar() -> bool { false }\n",
            )],
            "v2",
        );

        let trends = complexity_trend(dir.path(), "lib.rs", Some("foo"), 10).unwrap();
        assert_eq!(trends.len(), 1);
        assert_eq!(trends[0].symbol_name, "foo");
    }

    #[test]
    fn trend_limit_caps_at_max() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo(dir.path());

        for i in 0..5 {
            make_commit(
                &repo,
                dir.path(),
                &[("lib.rs", &format!("pub fn f() {{ let _ = {i}; }}\n"))],
                &format!("commit {i}"),
            );
        }

        // Ask for 3, should get at most 3 commits.
        let trends = complexity_trend(dir.path(), "lib.rs", None, 3).unwrap();
        for t in &trends {
            assert!(t.points.len() <= 3);
        }
    }

    #[test]
    fn trend_single_commit_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo(dir.path());
        make_commit(
            &repo,
            dir.path(),
            &[("lib.rs", "pub fn f() { if true {} }\n")],
            "only commit",
        );

        // Need at least 2 data points for a meaningful trend.
        let trends = complexity_trend(dir.path(), "lib.rs", Some("f"), 10).unwrap();
        assert!(trends.is_empty());
    }
}
