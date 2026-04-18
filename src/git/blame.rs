//! Symbol-level git blame: who last touched a function and what was their
//! commit message?

use std::path::Path;

use anyhow::Result;
use git2::{BlameOptions, Repository};
use tracing::debug;

use crate::str_utils::floor_char_boundary;

pub struct BlameHunk {
    pub line_start: u32,
    pub line_end: u32,
    pub author: String,
    pub email: String,
    pub date: i64,
    pub commit_sha: String,
    pub commit_summary: String,
    pub lines: u32,
}

/// Run `git blame` scoped to `[sym_start, sym_end]` (1-based, inclusive).
///
/// Returns one `BlameHunk` per contiguous blame region within the range,
/// sorted by line_start ascending.
pub fn symbol_blame(
    root: &Path,
    file_path: &str,
    sym_start: u32,
    sym_end: u32,
) -> Result<Vec<BlameHunk>> {
    let repo = Repository::discover(root)?;
    let mailmap = repo.mailmap().ok();

    let mut opts = BlameOptions::new();
    opts.min_line(sym_start as usize);
    opts.max_line(sym_end as usize);
    opts.track_copies_same_commit_moves(true);
    opts.track_copies_same_commit_moves(true);

    let blame = repo
        .blame_file(Path::new(file_path), Some(&mut opts))
        .map_err(|e| {
            debug!(path = file_path, error = %e, "blame failed");
            anyhow::anyhow!("git blame failed for {file_path}: {e}")
        })?;

    let mut hunks = Vec::new();

    for i in 0..blame.len() {
        let Some(hunk) = blame.get_index(i) else {
            continue;
        };

        let sig = hunk.final_signature();
        let raw_name = sig.name().unwrap_or("unknown").to_string();
        let raw_email = sig.email().unwrap_or("").to_string();

        let (author, email) = if let Some(ref mm) = mailmap {
            let resolved = mm.resolve_signature(&sig).ok();
            (
                resolved
                    .as_ref()
                    .and_then(|r| r.name().map(String::from))
                    .unwrap_or(raw_name),
                resolved
                    .as_ref()
                    .and_then(|r| r.email().map(String::from))
                    .unwrap_or(raw_email),
            )
        } else {
            (raw_name, raw_email)
        };

        let oid = hunk.final_commit_id();
        let short_sha = &format!("{oid}")[..7];
        let (sha, summary, epoch) = match repo.find_commit(oid) {
            Ok(commit) => {
                let msg = commit.summary().unwrap_or("").to_string();
                let truncated = if msg.len() > 72 {
                    format!("{}...", &msg[..floor_char_boundary(&msg, 69)])
                } else {
                    msg
                };
                (short_sha.to_string(), truncated, commit.time().seconds())
            }
            Err(_) => (short_sha.to_string(), String::new(), 0),
        };

        let hunk_start = hunk.final_start_line() as u32;
        let hunk_lines = hunk.lines_in_hunk() as u32;
        let hunk_end = hunk_start + hunk_lines.saturating_sub(1);

        hunks.push(BlameHunk {
            line_start: hunk_start,
            line_end: hunk_end,
            author,
            email,
            date: epoch,
            commit_sha: sha,
            commit_summary: summary,
            lines: hunk_lines,
        });
    }

    Ok(hunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use git2::{Repository, Signature};

    fn init_repo(dir: &Path) -> Repository {
        Repository::init(dir).expect("failed to init repo")
    }

    fn make_commit_as(
        repo: &Repository,
        dir: &Path,
        files: &[(&str, &str)],
        message: &str,
        author: &str,
        email: &str,
    ) {
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
        let sig = Signature::now(author, email).expect("failed to create sig");
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
    fn blame_single_author() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());

        let content = "line1\nline2\nline3\nline4\nline5\n";
        make_commit_as(
            &repo,
            dir.path(),
            &[("test.txt", content)],
            "initial commit",
            "Alice",
            "alice@test.com",
        );

        let hunks = symbol_blame(dir.path(), "test.txt", 1, 5).unwrap();
        assert!(!hunks.is_empty());
        assert_eq!(hunks[0].author, "Alice");
        let total: u32 = hunks.iter().map(|h| h.lines).sum();
        assert_eq!(total, 5);
    }

    #[test]
    fn blame_multi_author() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());

        make_commit_as(
            &repo,
            dir.path(),
            &[("test.txt", "line1\nline2\nline3\nline4\nline5\n")],
            "initial",
            "Alice",
            "alice@test.com",
        );

        make_commit_as(
            &repo,
            dir.path(),
            &[("test.txt", "line1\nline2\nchanged3\nline4\nline5\n")],
            "fix line 3",
            "Bob",
            "bob@test.com",
        );

        let hunks = symbol_blame(dir.path(), "test.txt", 1, 5).unwrap();
        let authors: Vec<&str> = hunks.iter().map(|h| h.author.as_str()).collect();
        assert!(authors.contains(&"Alice"));
        assert!(authors.contains(&"Bob"));
    }

    #[test]
    fn blame_line_range_filters() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());

        let content = "header\nfn foo() {\n  body1\n  body2\n}\nfn bar() {\n  other\n}\n";
        make_commit_as(
            &repo,
            dir.path(),
            &[("test.rs", content)],
            "initial",
            "Alice",
            "alice@test.com",
        );

        // Blame only lines 2-5 (the foo function)
        let hunks = symbol_blame(dir.path(), "test.rs", 2, 5).unwrap();
        let total: u32 = hunks.iter().map(|h| h.lines).sum();
        assert_eq!(total, 4);
    }

    #[test]
    fn blame_nonexistent_file() {
        let dir = tempfile::tempdir().unwrap();
        let _repo = init_repo(dir.path());
        let result = symbol_blame(dir.path(), "nope.txt", 1, 10);
        assert!(result.is_err());
    }
}
