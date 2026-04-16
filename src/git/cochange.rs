use std::collections::HashMap;
use std::path::Path;

use git2::Repository;
use rusqlite::Connection;
use tracing::{debug, info, warn};

use crate::error::Result;
use crate::storage::read::get_file_by_path;
use crate::storage::write::upsert_cochange_n;

pub struct CoChangeConfig {
    pub commit_limit: u32,
    pub min_files: usize,
    pub max_files: usize,
}

impl Default for CoChangeConfig {
    fn default() -> Self {
        Self {
            commit_limit: 300,
            min_files: 2,
            max_files: 20,
        }
    }
}

/// Analyze git history and populate the co_changes table.
///
/// Walks up to `config.commit_limit` commits from HEAD, extracts the set of
/// changed files per commit, and for every qualifying commit (between
/// `min_files` and `max_files` changed files) upserts a co-change count for
/// each unique pair.
pub fn analyze_cochanges(conn: &Connection, root: &Path, config: &CoChangeConfig) -> Result<()> {
    let repo = match Repository::open(root) {
        Ok(r) => r,
        Err(e) => {
            info!("Skipping co-change analysis: not a git repo ({})", e);
            return Ok(());
        }
    };

    let head = match repo.head() {
        Ok(h) => h,
        Err(e) => {
            info!("Skipping co-change analysis: cannot read HEAD ({})", e);
            return Ok(());
        }
    };

    let head_oid = match head.target() {
        Some(oid) => oid,
        None => {
            info!("Skipping co-change analysis: HEAD has no target");
            return Ok(());
        }
    };

    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(git2::Sort::TIME)?;
    revwalk.push(head_oid)?;

    let mut pair_counts: HashMap<(String, String), u32> = HashMap::new();
    let mut file_change_counts: HashMap<String, u32> = HashMap::new();
    let mut processed = 0u32;

    for oid_result in revwalk {
        if processed >= config.commit_limit {
            break;
        }

        let oid = match oid_result {
            Ok(o) => o,
            Err(e) => {
                warn!("Skipping unreadable commit during revwalk: {}", e);
                continue;
            }
        };

        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(e) => {
                warn!("Cannot read commit {}: {}", oid, e);
                continue;
            }
        };

        let files = changed_files_in_commit(&repo, &commit);

        // Per-file change count: every commit counts, regardless of size.
        for f in &files {
            *file_change_counts.entry(f.clone()).or_insert(0) += 1;
        }

        if files.len() < config.min_files || files.len() > config.max_files {
            processed += 1;
            continue;
        }

        for i in 0..files.len() {
            for j in (i + 1)..files.len() {
                let (a, b) = if files[i] < files[j] {
                    (&files[i], &files[j])
                } else {
                    (&files[j], &files[i])
                };
                *pair_counts.entry((a.clone(), b.clone())).or_insert(0) += 1;
            }
        }

        processed += 1;
    }

    debug!(
        "Co-change analysis: {} commits processed, {} unique pairs, {} files with changes",
        processed,
        pair_counts.len(),
        file_change_counts.len()
    );

    let tx = conn.unchecked_transaction()?;
    let mut written = 0u32;
    let mut skipped = 0u32;
    for ((path_a, path_b), count) in &pair_counts {
        // Only record co-changes between files that currently exist on disk
        // (i.e. were inserted by full_index). Paths seen only in git history
        // — renames, deletes, moves — are dropped rather than resurrected as
        // phantom rows with zero size/mtime.
        let (Some(a), Some(b)) = (
            get_file_by_path(&tx, path_a)?,
            get_file_by_path(&tx, path_b)?,
        ) else {
            skipped += 1;
            continue;
        };
        upsert_cochange_n(&tx, a.id, b.id, *count)?;
        written += 1;
    }

    // Write per-file change counts into the files table. Historical-only
    // paths are dropped here for the same reason.
    for (path, count) in &file_change_counts {
        if let Some(file) = get_file_by_path(&tx, path)? {
            tx.execute(
                "UPDATE files SET change_count = ?1 WHERE id = ?2",
                rusqlite::params![*count as i64, file.id],
            )?;
        }
    }

    tx.commit()?;
    crate::storage::verify_foreign_keys(conn)?;

    info!(
        "Co-change analysis complete: {} pairs written ({} skipped: historical-only paths), {} file change counts written",
        written,
        skipped,
        file_change_counts.len()
    );
    Ok(())
}

fn changed_files_in_commit(repo: &Repository, commit: &git2::Commit) -> Vec<String> {
    let commit_tree = match commit.tree() {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let parent_tree = if commit.parent_count() > 0 {
        commit.parent(0).ok().and_then(|p| p.tree().ok())
    } else {
        None
    };

    let diff = match repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&commit_tree), None) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let mut paths = Vec::new();
    for delta in diff.deltas() {
        if let Some(p) = delta.new_file().path().and_then(|p| p.to_str()) {
            if let Some(name) = Path::new(p).file_name().and_then(|n| n.to_str())
                && name.starts_with('.')
            {
                continue;
            }
            paths.push(p.to_string());
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage;
    use git2::Signature;
    use std::fs;
    use tempfile::TempDir;

    fn setup_db() -> Connection {
        storage::open_in_memory().unwrap()
    }

    fn init_repo(dir: &Path) -> Repository {
        Repository::init(dir).unwrap()
    }

    fn make_commit(repo: &Repository, dir: &Path, files: &[&str], message: &str) {
        let sig = Signature::now("Test", "test@example.com").unwrap();
        let mut index = repo.index().unwrap();

        for file in files {
            let file_path = dir.join(file);
            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            let existing = fs::read_to_string(&file_path).unwrap_or_default();
            fs::write(&file_path, format!("{}\n// edit", existing)).unwrap();
            index.add_path(Path::new(file)).unwrap();
        }

        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();

        let parent = repo
            .head()
            .ok()
            .and_then(|h| h.target().and_then(|oid| repo.find_commit(oid).ok()));

        match parent {
            Some(p) => {
                repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&p])
                    .unwrap();
            }
            None => {
                repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[])
                    .unwrap();
            }
        }
    }

    fn register_files(conn: &Connection, paths: &[&str]) {
        for path in paths {
            storage::write::upsert_file(conn, path, 1000, 100, "rust", 10).unwrap();
        }
    }

    #[test]
    fn test_basic_cochange_pairs() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        let conn = setup_db();

        make_commit(
            &repo,
            dir,
            &["src/a.rs", "src/b.rs", "src/c.rs"],
            "commit 1",
        );
        make_commit(&repo, dir, &["src/a.rs", "src/b.rs"], "commit 2");

        register_files(&conn, &["src/a.rs", "src/b.rs", "src/c.rs"]);

        analyze_cochanges(&conn, dir, &CoChangeConfig::default()).unwrap();

        let cochanges = storage::read::get_cochanges(&conn, 1, 10).unwrap();
        assert!(!cochanges.is_empty());

        let ab_count: i64 = conn
            .query_row(
                "SELECT count FROM co_changes WHERE file_a = ?1 AND file_b = ?2",
                rusqlite::params![
                    storage::read::get_file_by_path(&conn, "src/a.rs")
                        .unwrap()
                        .unwrap()
                        .id,
                    storage::read::get_file_by_path(&conn, "src/b.rs")
                        .unwrap()
                        .unwrap()
                        .id,
                ],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ab_count, 2);
    }

    #[test]
    fn test_non_git_directory_does_not_fail() {
        let tmp = TempDir::new().unwrap();
        let conn = setup_db();

        let result = analyze_cochanges(&conn, tmp.path(), &CoChangeConfig::default());
        assert!(result.is_ok());
    }

    #[test]
    fn test_min_files_filter() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        let conn = setup_db();

        make_commit(&repo, dir, &["only_one.rs"], "single file commit");
        make_commit(&repo, dir, &["src/a.rs", "src/b.rs"], "two file commit");

        register_files(&conn, &["only_one.rs", "src/a.rs", "src/b.rs"]);

        analyze_cochanges(&conn, dir, &CoChangeConfig::default()).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM co_changes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_max_files_filter() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        let conn = setup_db();

        let many_files: Vec<String> = (0..25).map(|i| format!("file_{}.rs", i)).collect();
        let many_refs: Vec<&str> = many_files.iter().map(|s| s.as_str()).collect();
        make_commit(&repo, dir, &many_refs, "large commit");

        make_commit(&repo, dir, &["src/x.rs", "src/y.rs"], "small commit");

        let mut all_files = many_files.clone();
        all_files.push("src/x.rs".to_string());
        all_files.push("src/y.rs".to_string());
        let all_refs: Vec<&str> = all_files.iter().map(|s| s.as_str()).collect();
        register_files(&conn, &all_refs);

        analyze_cochanges(&conn, dir, &CoChangeConfig::default()).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM co_changes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_empty_repo() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let _repo = init_repo(dir);
        let conn = setup_db();

        let result = analyze_cochanges(&conn, dir, &CoChangeConfig::default());
        assert!(result.is_ok());
    }

    #[test]
    fn test_unregistered_paths_are_skipped() {
        // Files that appear only in git history — never indexed from disk —
        // must not be resurrected as phantom rows. Co-change pairs touching
        // such paths are simply dropped.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let repo = init_repo(dir);
        let conn = setup_db();

        make_commit(
            &repo,
            dir,
            &["src/a.rs", "src/b.rs", "deploy.yaml"],
            "commit with non-indexed file",
        );

        // Only a.rs and b.rs are registered (simulating files indexed from disk).
        // deploy.yaml is seen in git history but not on disk.
        register_files(&conn, &["src/a.rs", "src/b.rs"]);

        analyze_cochanges(&conn, dir, &CoChangeConfig::default()).unwrap();

        // Only the (a, b) pair should be recorded. (a, deploy) and (b, deploy)
        // are skipped because deploy.yaml is not in the files table.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM co_changes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // deploy.yaml must NOT have been inserted.
        assert!(
            storage::read::get_file_by_path(&conn, "deploy.yaml")
                .unwrap()
                .is_none(),
            "historical-only paths must not be resurrected as phantom rows"
        );
    }
}
