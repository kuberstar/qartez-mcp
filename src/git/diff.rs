use std::path::Path;

use anyhow::{Result, anyhow};
use git2::Repository;

/// Extract changed file paths between two git revisions.
///
/// Accepts standard git revspec syntax (`main..HEAD`, `HEAD~3..HEAD`) or a
/// single ref (`main`), which is interpreted as `<ref>..HEAD`.
pub fn changed_files_in_range(root: &Path, revspec: &str) -> Result<Vec<String>> {
    let repo = Repository::discover(root)?;

    let effective = if revspec.contains("..") {
        revspec.to_string()
    } else {
        format!("{revspec}..HEAD")
    };

    let parsed = repo.revparse(&effective)?;

    let from_obj = parsed
        .from()
        .ok_or_else(|| anyhow!("revspec '{effective}' resolved without a 'from' endpoint"))?;
    let to_obj = parsed
        .to()
        .ok_or_else(|| anyhow!("revspec '{effective}' resolved without a 'to' endpoint"))?;

    // WHY: `diff_tree_to_tree` reports the symmetric set of path deltas
    // regardless of direction, so `HEAD..HEAD~1` silently returned the
    // same file list as `HEAD~1..HEAD` and masked a real user mistake.
    // Reject reversed ranges (from is a descendant of to) with a hint
    // that names the forward form. We do not autocorrect - that would
    // hide the bug in the caller's tooling or typed revspec.
    if let (Ok(from_commit), Ok(to_commit)) = (from_obj.peel_to_commit(), to_obj.peel_to_commit())
        && from_commit.id() != to_commit.id()
        && repo.graph_descendant_of(from_commit.id(), to_commit.id())?
    {
        let (forward_from, forward_to) = match effective.split_once("..") {
            Some((a, b)) => (b.to_string(), a.to_string()),
            None => (to_commit.id().to_string(), from_commit.id().to_string()),
        };
        return Err(anyhow!(
            "range reversed: '{effective}' goes from descendant to ancestor. Did you mean '{forward_from}..{forward_to}' instead?"
        ));
    }

    let from_tree = from_obj.peel_to_tree()?;
    let to_tree = to_obj.peel_to_tree()?;

    let diff = repo.diff_tree_to_tree(Some(&from_tree), Some(&to_tree), None)?;

    let mut paths = Vec::new();
    for delta in diff.deltas() {
        if let Some(p) = delta.new_file().path().and_then(|p| p.to_str()) {
            paths.push(p.to_string());
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
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
    fn changed_files_between_commits() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let repo = init_repo(dir.path());
        make_commit(&repo, dir.path(), &[("a.txt", "hello")], "first");
        make_commit(
            &repo,
            dir.path(),
            &[("a.txt", "world"), ("b.txt", "new")],
            "second",
        );

        let files = changed_files_in_range(dir.path(), "HEAD~1..HEAD").unwrap();
        assert!(files.contains(&"a.txt".to_string()));
        assert!(files.contains(&"b.txt".to_string()));
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn single_ref_implies_to_head() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let repo = init_repo(dir.path());
        make_commit(&repo, dir.path(), &[("a.txt", "v1")], "first");
        make_commit(&repo, dir.path(), &[("b.txt", "v1")], "second");

        let with_range = changed_files_in_range(dir.path(), "HEAD~1..HEAD").unwrap();
        let with_single = changed_files_in_range(dir.path(), "HEAD~1").unwrap();
        assert_eq!(with_range, with_single);
    }

    #[test]
    fn no_changes_returns_empty() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let repo = init_repo(dir.path());
        make_commit(&repo, dir.path(), &[("a.txt", "v1")], "first");

        let files = changed_files_in_range(dir.path(), "HEAD..HEAD").unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn invalid_revspec_returns_error() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let _repo = init_repo(dir.path());
        let result = changed_files_in_range(dir.path(), "nonexistent..HEAD");
        assert!(result.is_err());
    }

    #[test]
    fn not_a_git_repo_returns_error() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let result = changed_files_in_range(dir.path(), "main..HEAD");
        assert!(result.is_err());
    }
}
