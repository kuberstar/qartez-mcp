//! Symbol-scoped `git blame`.
//!
//! Resolves to a symbol's line range and blames only those lines, so the
//! caller learns who last touched a specific function or type instead of a
//! whole file. Backed by git2 with mailmap support for author de-duplication.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use git2::{BlameOptions, Repository};
use tracing::debug;

/// One contiguous blame hunk within the requested line range.
#[derive(Debug, Clone)]
pub struct BlameLine {
    /// Abbreviated (7-char) id of the final commit for these lines.
    pub commit: String,
    /// Author name, mailmap-resolved.
    pub author: String,
    /// Commit time (Unix seconds); orders aggregate output by recency.
    pub time: i64,
    /// First line of the hunk (1-based, within the file).
    pub line_start: u32,
    /// Number of lines in the hunk.
    pub line_count: u32,
}

/// Per-author rollup of a symbol's blame, ordered by line count descending.
#[derive(Debug, Clone)]
pub struct AuthorBlame {
    pub author: String,
    pub lines: u32,
    /// Abbreviated id of this author's most RECENT touch in range.
    pub latest_commit: String,
    /// Commit time of `latest_commit` (Unix seconds).
    pub latest_time: i64,
}

/// Length of an abbreviated git object id. Seven hex chars is git's
/// conventional short form and is unambiguous for all but the largest repos.
const SHORT_SHA_LEN: usize = 7;

/// Blame the inclusive 1-based line range `[line_start, line_end]` of
/// `rel_path` and return one entry per blame hunk, in line order.
///
/// Returns an empty vector when the file has no blameable history (untracked,
/// binary, or outside the repo) rather than erroring, so a missing symbol's
/// history degrades gracefully.
pub fn symbol_blame(
    root: &Path,
    rel_path: &str,
    line_start: u32,
    line_end: u32,
) -> Result<Vec<BlameLine>> {
    let repo = Repository::discover(root)?;
    let mailmap = repo.mailmap().ok();

    let mut opts = BlameOptions::new();
    // Scope the blame to the symbol's lines rather than the whole file - this
    // is the point of the tool and keeps blaming a 20-line function in a
    // 5000-line file cheap.
    opts.min_line(line_start as usize);
    opts.max_line(line_end as usize);

    let blame = match repo.blame_file(Path::new(rel_path), Some(&mut opts)) {
        Ok(b) => b,
        Err(e) => {
            debug!(path = rel_path, error = %e, "skipping unblameable file");
            return Ok(Vec::new());
        }
    };

    let mut lines = Vec::new();
    for i in 0..blame.len() {
        let Some(hunk) = blame.get_index(i) else {
            continue;
        };
        // git2 0.21 returns Option here: the signature pointer is null when the
        // commit metadata is unavailable, so absent means "unknown author".
        let sig = hunk.final_signature();
        let raw_name = sig
            .as_ref()
            .and_then(|s| s.name().ok())
            .unwrap_or("unknown")
            .to_string();
        // Collapse author aliases through .mailmap when the repo has one.
        let author = match (&mailmap, &sig) {
            (Some(mm), Some(s)) => mm
                .resolve_signature(s)
                .ok()
                .and_then(|resolved| resolved.name().ok().map(String::from))
                .unwrap_or(raw_name),
            _ => raw_name,
        };
        let full = hunk.final_commit_id().to_string();
        let commit = full
            .get(..SHORT_SHA_LEN)
            .unwrap_or(full.as_str())
            .to_string();
        lines.push(BlameLine {
            commit,
            author,
            time: sig.as_ref().map_or(0, |s| s.when().seconds()),
            line_start: hunk.final_start_line() as u32,
            line_count: hunk.lines_in_hunk() as u32,
        });
    }
    Ok(lines)
}

/// Roll a hunk list up by author. Each author's `latest_commit` is the commit
/// with the greatest commit time among their hunks (not the first hunk's
/// commit), so the column genuinely reflects their most recent touch.
pub fn aggregate_by_author(hunks: &[BlameLine]) -> Vec<AuthorBlame> {
    let mut by_author: HashMap<&str, AuthorBlame> = HashMap::new();
    for h in hunks {
        let entry = by_author
            .entry(h.author.as_str())
            .or_insert_with(|| AuthorBlame {
                author: h.author.clone(),
                lines: 0,
                latest_commit: h.commit.clone(),
                latest_time: i64::MIN,
            });
        entry.lines += h.line_count;
        if h.time > entry.latest_time {
            entry.latest_time = h.time;
            entry.latest_commit = h.commit.clone();
        }
    }
    let mut out: Vec<AuthorBlame> = by_author.into_values().collect();
    // Most-lines first; tie-break by name for deterministic output.
    out.sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.author.cmp(&b.author)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use std::fs;
    use tempfile::TempDir;

    fn commit_file(repo: &Repository, root: &Path, rel: &str, content: &str, author: &str) {
        fs::write(root.join(rel), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(rel)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = Signature::now(author, &format!("{author}@example.com")).unwrap();
        let parent = repo
            .head()
            .ok()
            .and_then(|h| h.target())
            .and_then(|oid| repo.find_commit(oid).ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, "change", &tree, &parents)
            .unwrap();
    }

    #[test]
    fn symbol_blame_scopes_to_requested_lines() {
        let dir = TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file(
            &repo,
            dir.path(),
            "lib.rs",
            "fn a() {}\nfn b() {}\nfn c() {}\n",
            "Alice",
        );

        let hunks = symbol_blame(dir.path(), "lib.rs", 2, 2).unwrap();
        assert!(!hunks.is_empty(), "expected a blame hunk for line 2");
        assert!(hunks.iter().all(|h| h.author == "Alice"));
        assert!(hunks.iter().all(|h| h.commit.len() <= SHORT_SHA_LEN));
    }

    #[test]
    fn symbol_blame_untracked_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        Repository::init(dir.path()).unwrap();
        // Never committed -> no history -> empty, not an error.
        let hunks = symbol_blame(dir.path(), "ghost.rs", 1, 1).unwrap();
        assert!(hunks.is_empty());
    }

    #[test]
    fn aggregate_picks_latest_commit_by_time() {
        let hunks = vec![
            BlameLine {
                commit: "aaaaaaa".into(),
                author: "Alice".into(),
                time: 100,
                line_start: 1,
                line_count: 3,
            },
            BlameLine {
                commit: "bbbbbbb".into(),
                author: "Alice".into(),
                time: 200,
                line_start: 4,
                line_count: 2,
            },
            BlameLine {
                commit: "ccccccc".into(),
                author: "Bob".into(),
                time: 150,
                line_start: 6,
                line_count: 1,
            },
        ];

        let agg = aggregate_by_author(&hunks);
        assert_eq!(agg.len(), 2);
        // Alice owns the most lines (5) and her latest touch is the time=200
        // commit, not the first hunk encountered.
        assert_eq!(agg[0].author, "Alice");
        assert_eq!(agg[0].lines, 5);
        assert_eq!(agg[0].latest_commit, "bbbbbbb");
        assert_eq!(agg[1].author, "Bob");
        assert_eq!(agg[1].lines, 1);
    }
}
