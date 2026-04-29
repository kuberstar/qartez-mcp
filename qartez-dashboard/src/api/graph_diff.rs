//! `GET /api/graph-diff` - file-level structural delta between the
//! currently indexed graph and a historical git revision.
//!
//! Resolves `?against=<rev>` (default `HEAD~10`) via `git rev-parse`,
//! lists the files at that commit with `git ls-tree -r --name-only`, and
//! diffs that set against the paths present in
//! `<project_root>/.qartez/index.db`. The response holds the added /
//! removed path lists plus an `unchanged_count` so the dashboard can
//! render churn deltas without reissuing the query.
//!
//! The endpoint always returns 200. Failures (missing git binary, bad
//! rev, missing DB, panicked spawn-blocking task) collapse to an empty
//! diff with an `error` field describing the reason. This keeps the UI
//! widget live even when the project is not a git repo.
//!
//! `against` is sanitized before being passed to `git`. Anything outside
//! `[A-Za-z0-9_~^/.-]` (shell metacharacters, spaces, command
//! substitutions) is rejected with the `invalid against` error - the
//! handler still returns 200 so the dashboard does not crash on a typo.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use axum::Json;
use axum::extract::{Query, State};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Default revision compared against `HEAD` when the caller omits
/// `?against=`. Ten commits back roughly matches a sprint of activity on
/// medium-velocity repos and stays cheap to resolve.
const DEFAULT_AGAINST: &str = "HEAD~10";

/// Query string for `GET /api/graph-diff?against=<rev>`.
#[derive(Debug, Deserialize)]
pub struct GraphDiffQuery {
    /// Git revision to compare against, e.g. `HEAD~10`, `HEAD^`, or a
    /// 40-char SHA. Defaults to `HEAD~10`. Sanitized before being
    /// forwarded to `git rev-parse`.
    pub against: Option<String>,
}

/// Response body for `GET /api/graph-diff`. Always returned with
/// `200 OK`; failures populate `error` and clear `added` / `removed`.
#[derive(Debug, Serialize)]
pub struct GraphDiffResponse {
    /// Paths present in the current index but not at `against`.
    pub added: Vec<String>,
    /// Paths present at `against` but not in the current index.
    pub removed: Vec<String>,
    /// Number of paths in both sets. Cheaper to count once here than to
    /// have the UI compute the intersection.
    pub unchanged_count: i64,
    /// Echo of the requested rev, verbatim (after sanitation). The UI
    /// shows this in the "comparing against ..." label.
    pub against: String,
    /// Resolved 40-char SHA from `git rev-parse`. Empty string when the
    /// rev could not be resolved or the diff path failed.
    pub resolved_sha: String,
    /// Populated only when the diff failed; describes the reason. The
    /// HTTP status is still 200 so the dashboard widget can render the
    /// fallback view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Allowed character set for `against`. Covers refs (`HEAD`, `HEAD~10`,
/// `HEAD^`), SHAs (`a1b2c3d`), branch and tag names with slashes, and
/// the dot used in `..` ranges. Everything else is rejected to keep
/// shell metacharacters out of the `git` invocation.
fn is_valid_against(rev: &str) -> bool {
    if rev.is_empty() {
        return false;
    }
    rev.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '~' | '^' | '/' | '.' | '-'))
}

/// Handle `GET /api/graph-diff?against=<rev>`.
///
/// Always returns `200 OK`. Failure modes populate `error` in the body
/// rather than swapping the status code so the dashboard widget can
/// render a fallback view without juggling HTTP error handling.
pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<GraphDiffQuery>,
) -> Json<GraphDiffResponse> {
    let against = query.against.unwrap_or_else(|| DEFAULT_AGAINST.to_string());

    if !is_valid_against(&against) {
        return Json(GraphDiffResponse {
            added: Vec::new(),
            removed: Vec::new(),
            unchanged_count: 0,
            against,
            resolved_sha: String::new(),
            error: Some("invalid against".to_string()),
        });
    }

    let root = state.project_root().to_path_buf();
    let against_for_task = against.clone();

    let join =
        tokio::task::spawn_blocking(move || compute_graph_diff_at_root(&root, &against_for_task))
            .await;

    match join {
        Ok(response) => Json(response),
        Err(error) => {
            tracing::error!(?error, "graph_diff.join.failed");
            Json(GraphDiffResponse {
                added: Vec::new(),
                removed: Vec::new(),
                unchanged_count: 0,
                against,
                resolved_sha: String::new(),
                error: Some("join error".to_string()),
            })
        }
    }
}

fn compute_graph_diff_at_root(root: &Path, against: &str) -> GraphDiffResponse {
    let current = match load_indexed_paths(&default_db_path(root)) {
        Ok(set) => set,
        Err(error) => {
            tracing::debug!(?error, "graph_diff.index.unavailable");
            return GraphDiffResponse {
                added: Vec::new(),
                removed: Vec::new(),
                unchanged_count: 0,
                against: against.to_string(),
                resolved_sha: String::new(),
                error: Some("index db missing".to_string()),
            };
        }
    };

    #[expect(
        clippy::cast_possible_wrap,
        reason = "indexed path counts fit comfortably in i64"
    )]
    let current_count = current.len() as i64;

    let resolved_sha = match git_rev_parse(root, against) {
        Ok(sha) => sha,
        Err(reason) => {
            tracing::debug!(reason = %reason, "graph_diff.rev_parse.failed");
            return GraphDiffResponse {
                added: Vec::new(),
                removed: Vec::new(),
                unchanged_count: current_count,
                against: against.to_string(),
                resolved_sha: String::new(),
                error: Some(reason),
            };
        }
    };

    let past = match git_ls_tree(root, &resolved_sha) {
        Ok(set) => set,
        Err(reason) => {
            tracing::debug!(reason = %reason, "graph_diff.ls_tree.failed");
            return GraphDiffResponse {
                added: Vec::new(),
                removed: Vec::new(),
                unchanged_count: current_count,
                against: against.to_string(),
                resolved_sha,
                error: Some(reason),
            };
        }
    };

    let (added, removed, unchanged_count) = diff_path_sets(&current, &past);

    GraphDiffResponse {
        added,
        removed,
        unchanged_count,
        against: against.to_string(),
        resolved_sha,
        error: None,
    }
}

/// Compute the structural diff between two path sets.
///
/// Factored out as a pure helper so unit tests can validate the diff
/// math without spawning `git`. `added` is `current - past`, `removed`
/// is `past - current`, both sorted ascending so the output is
/// deterministic. `unchanged_count` is the size of the intersection.
pub(crate) fn diff_path_sets(
    current: &HashSet<String>,
    past: &HashSet<String>,
) -> (Vec<String>, Vec<String>, i64) {
    let mut added: Vec<String> = current.difference(past).cloned().collect();
    added.sort();
    let mut removed: Vec<String> = past.difference(current).cloned().collect();
    removed.sort();
    #[expect(
        clippy::cast_possible_wrap,
        reason = "intersection size fits in i64 for any realistic repo"
    )]
    let unchanged = current.intersection(past).count() as i64;
    (added, removed, unchanged)
}

fn load_indexed_paths(db_path: &Path) -> anyhow::Result<HashSet<String>> {
    if !db_path.exists() {
        anyhow::bail!("index db missing");
    }
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare("SELECT path FROM files")?;
    let rows = stmt.query_map(params![], |r| r.get::<_, String>(0))?;
    let mut out = HashSet::new();
    for row in rows {
        out.insert(row?);
    }
    Ok(out)
}

fn git_rev_parse(root: &Path, rev: &str) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg(rev)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| format!("git binary missing: {error}"))?;
    if !output.status.success() {
        return Err(format!("rev not resolved: {rev}"));
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        return Err(format!("rev not resolved: {rev}"));
    }
    Ok(sha)
}

fn git_ls_tree(root: &Path, sha: &str) -> Result<HashSet<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("ls-tree")
        .arg("-r")
        .arg("--name-only")
        .arg(sha)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| format!("git binary missing: {error}"))?;
    if !output.status.success() {
        return Err(format!("ls-tree failed for {sha}"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = HashSet::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            out.insert(trimmed.to_string());
        }
    }
    Ok(out)
}

fn default_db_path(root: &Path) -> PathBuf {
    root.join(".qartez").join("index.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_against_param_default_to_head_minus_10() {
        // Sanitizer accepts the default and other common refs.
        for valid in [
            "HEAD",
            "HEAD~5",
            "HEAD~10",
            "HEAD^",
            "HEAD^^",
            "abc123def",
            "main",
            "feature/foo",
            "v1.2.3",
            "release-2026",
        ] {
            assert!(is_valid_against(valid), "expected `{valid}` to be accepted");
        }

        // Sanitizer rejects shell metacharacters and spaces.
        for invalid in [
            "",
            "HEAD; rm -rf /",
            "foo bar",
            "$(echo x)",
            "`whoami`",
            "HEAD|cat",
            "HEAD&&ls",
            "rev>file",
        ] {
            assert!(
                !is_valid_against(invalid),
                "expected `{invalid}` to be rejected"
            );
        }

        // Default constant is itself a valid value.
        assert!(is_valid_against(DEFAULT_AGAINST));
    }

    #[test]
    fn compute_diff_no_db_returns_zero_unchanged() {
        let tmp = TempDir::new().expect("create tempdir");
        let response = compute_graph_diff_at_root(tmp.path(), "HEAD~10");

        assert!(response.added.is_empty());
        assert!(response.removed.is_empty());
        assert_eq!(response.unchanged_count, 0);
        assert_eq!(response.against, "HEAD~10");
        assert_eq!(response.resolved_sha, "");
        assert_eq!(response.error.as_deref(), Some("index db missing"));
    }

    #[test]
    fn compute_diff_with_two_path_sets() {
        let mut current = HashSet::new();
        current.insert("src/a.rs".to_string());
        current.insert("src/b.rs".to_string());
        current.insert("src/new.rs".to_string());

        let mut past = HashSet::new();
        past.insert("src/a.rs".to_string());
        past.insert("src/b.rs".to_string());
        past.insert("src/gone.rs".to_string());

        let (added, removed, unchanged) = diff_path_sets(&current, &past);

        assert_eq!(added, vec!["src/new.rs".to_string()]);
        assert_eq!(removed, vec!["src/gone.rs".to_string()]);
        assert_eq!(unchanged, 2);
    }

    #[test]
    fn diff_path_sets_handles_disjoint_and_empty_inputs() {
        let empty: HashSet<String> = HashSet::new();
        let mut only_current = HashSet::new();
        only_current.insert("only.rs".to_string());

        let (added, removed, unchanged) = diff_path_sets(&only_current, &empty);
        assert_eq!(added, vec!["only.rs".to_string()]);
        assert!(removed.is_empty());
        assert_eq!(unchanged, 0);

        let (added, removed, unchanged) = diff_path_sets(&empty, &only_current);
        assert!(added.is_empty());
        assert_eq!(removed, vec!["only.rs".to_string()]);
        assert_eq!(unchanged, 0);

        let (added, removed, unchanged) = diff_path_sets(&empty, &empty);
        assert!(added.is_empty());
        assert!(removed.is_empty());
        assert_eq!(unchanged, 0);
    }
}

// Rust guideline compliant 2026-04-26
