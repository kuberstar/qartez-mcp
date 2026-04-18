// Rust guideline compliant 2026-04-16

//! Git-blame-based authorship analysis: single-author files, knowledge silos,
//! and bus factor per module.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use git2::{BlameOptions, Repository};
use tracing::{debug, info};

/// Per-file authorship breakdown derived from `git blame`.
pub struct FileAuthorship {
    pub path: String,
    /// Authors sorted by line count descending.
    pub authors: Vec<(String, u32)>,
    pub total_lines: u32,
    /// Minimum authors to cover >50% of lines.
    pub bus_factor: u32,
}

/// Per-module (directory) knowledge summary.
pub struct ModuleKnowledge {
    pub module: String,
    pub file_count: u32,
    pub total_lines: u32,
    /// Minimum authors to cover >50% of aggregated lines.
    pub bus_factor: u32,
    /// Top authors sorted by line count descending.
    pub top_authors: Vec<(String, u32)>,
    /// Files where a single person owns 100% of blame lines.
    pub single_author_files: u32,
}

/// Run `git blame` on each file and aggregate per-author line counts.
///
/// `file_paths` are relative to `root`. Files that cannot be blamed
/// (binary, untracked, missing history) are silently skipped.
pub fn analyze_knowledge(
    root: &Path,
    file_paths: &[String],
    author_filter: Option<&str>,
) -> Result<Vec<FileAuthorship>> {
    let repo = Repository::discover(root)?;
    let mailmap = repo.mailmap().ok();

    let mut results = Vec::new();
    let mut blame_opts = BlameOptions::new();

    for path in file_paths {
        let blame = match repo.blame_file(Path::new(path), Some(&mut blame_opts)) {
            Ok(b) => b,
            Err(e) => {
                debug!(path = path.as_str(), error = %e, "skipping unblameable file");
                continue;
            }
        };

        let mut author_lines: HashMap<String, u32> = HashMap::new();

        for i in 0..blame.len() {
            let Some(hunk) = blame.get_index(i) else {
                continue;
            };

            let sig = hunk.final_signature();
            let raw_name = sig.name().unwrap_or("unknown").to_string();

            // Apply mailmap for author deduplication.
            let author_name = if let Some(ref mm) = mailmap {
                mm.resolve_signature(&sig)
                    .ok()
                    .and_then(|resolved| resolved.name().map(String::from))
                    .unwrap_or(raw_name)
            } else {
                raw_name
            };

            let lines = hunk.lines_in_hunk() as u32;
            *author_lines.entry(author_name).or_insert(0) += lines;
        }

        if author_lines.is_empty() {
            continue;
        }

        let total_lines: u32 = author_lines.values().sum();

        let mut authors: Vec<(String, u32)> = author_lines.into_iter().collect();
        authors.sort_by(|a, b| b.1.cmp(&a.1));

        if let Some(filter) = author_filter {
            let filter_lower = filter.to_lowercase();
            if !authors
                .iter()
                .any(|(name, _)| name.to_lowercase().contains(&filter_lower))
            {
                continue;
            }
        }

        let bus_factor = compute_bus_factor(&authors, total_lines);

        results.push(FileAuthorship {
            path: path.clone(),
            authors,
            total_lines,
            bus_factor,
        });
    }

    info!(
        analyzed = results.len(),
        total = file_paths.len(),
        "knowledge analysis complete"
    );

    Ok(results)
}

/// Minimum number of authors whose combined line count exceeds 50% of total.
///
/// A bus factor of 1 means a single person owns the majority of the code.
fn compute_bus_factor(authors: &[(String, u32)], total_lines: u32) -> u32 {
    if authors.is_empty() || total_lines == 0 {
        return 0;
    }
    // 50% threshold, rounded up so a tie counts as covered.
    let threshold = total_lines.div_ceil(2);
    let mut accumulated = 0u32;
    for (i, (_, lines)) in authors.iter().enumerate() {
        accumulated += lines;
        if accumulated >= threshold {
            return (i + 1) as u32;
        }
    }
    authors.len() as u32
}

/// Roll up per-file authorship into per-module (directory) summaries.
pub fn rollup_modules(files: &[FileAuthorship]) -> Vec<ModuleKnowledge> {
    let mut module_map: HashMap<String, Vec<&FileAuthorship>> = HashMap::new();

    for f in files {
        let dir = Path::new(&f.path)
            .parent()
            .map(|p| {
                let s = p.to_string_lossy().to_string();
                if s.is_empty() { ".".to_string() } else { s }
            })
            .unwrap_or_else(|| ".".to_string());
        module_map.entry(dir).or_default().push(f);
    }

    let mut modules: Vec<ModuleKnowledge> = module_map
        .into_iter()
        .map(|(module, file_list)| {
            let file_count = file_list.len() as u32;
            let total_lines: u32 = file_list.iter().map(|f| f.total_lines).sum();
            let single_author_files =
                file_list.iter().filter(|f| f.authors.len() == 1).count() as u32;

            let mut combined: HashMap<String, u32> = HashMap::new();
            for f in &file_list {
                for (author, lines) in &f.authors {
                    *combined.entry(author.clone()).or_insert(0) += lines;
                }
            }
            let mut top_authors: Vec<(String, u32)> = combined.into_iter().collect();
            top_authors.sort_by(|a, b| b.1.cmp(&a.1));
            let bus_factor = compute_bus_factor(&top_authors, total_lines);

            ModuleKnowledge {
                module,
                file_count,
                total_lines,
                bus_factor,
                top_authors,
                single_author_files,
            }
        })
        .collect();

    // Lowest bus factor first (most risky), break ties by largest module.
    modules.sort_by(|a, b| {
        a.bus_factor
            .cmp(&b.bus_factor)
            .then(b.total_lines.cmp(&a.total_lines))
    });
    modules
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

    #[derive(Debug, Clone, Copy)]
    struct Author<'a> {
        name: &'a str,
        email: &'a str,
    }

    fn make_commit_as(
        repo: &Repository,
        dir: &Path,
        files: &[(&str, &str)],
        message: &str,
        author: Author<'_>,
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
        let sig = Signature::now(author.name, author.email).expect("failed to create sig");
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
    fn bus_factor_single_author() {
        let authors = vec![("Alice".to_string(), 100)];
        assert_eq!(compute_bus_factor(&authors, 100), 1);
    }

    #[test]
    fn bus_factor_two_equal_authors() {
        let authors = vec![("Alice".to_string(), 50), ("Bob".to_string(), 50)];
        assert_eq!(compute_bus_factor(&authors, 100), 1);
    }

    #[test]
    fn bus_factor_three_authors_spread() {
        // 40 + 35 + 25 = 100. First author alone covers 40%, not >50%.
        // First two cover 75% > 50%.
        let authors = vec![
            ("Alice".to_string(), 40),
            ("Bob".to_string(), 35),
            ("Carol".to_string(), 25),
        ];
        assert_eq!(compute_bus_factor(&authors, 100), 2);
    }

    #[test]
    fn bus_factor_empty() {
        assert_eq!(compute_bus_factor(&[], 0), 0);
    }

    #[test]
    fn analyze_single_author_repo() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let repo = init_repo(dir.path());
        make_commit(
            &repo,
            dir.path(),
            &[("src/main.rs", "fn main() {\n    println!(\"hello\");\n}\n")],
            "initial",
        );

        let result = analyze_knowledge(dir.path(), &["src/main.rs".into()], None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].bus_factor, 1);
        assert_eq!(result[0].authors.len(), 1);
        assert_eq!(result[0].authors[0].0, "test");
    }

    #[test]
    fn analyze_two_authors() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let repo = init_repo(dir.path());
        make_commit_as(
            &repo,
            dir.path(),
            &[("lib.rs", "line1\nline2\nline3\n")],
            "alice commit",
            Author {
                name: "Alice",
                email: "alice@test.com",
            },
        );
        make_commit_as(
            &repo,
            dir.path(),
            &[("lib.rs", "line1\nline2\nline3\nline4\nline5\nline6\n")],
            "bob commit",
            Author {
                name: "Bob",
                email: "bob@test.com",
            },
        );

        let result = analyze_knowledge(dir.path(), &["lib.rs".into()], None).unwrap();
        assert_eq!(result.len(), 1);
        assert!(!result[0].authors.is_empty());
    }

    #[test]
    fn author_filter_excludes_unmatched() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let repo = init_repo(dir.path());
        make_commit_as(
            &repo,
            dir.path(),
            &[("a.rs", "fn a() {}\n")],
            "alice only",
            Author {
                name: "Alice",
                email: "alice@test.com",
            },
        );

        let result = analyze_knowledge(dir.path(), &["a.rs".into()], Some("NonExistent")).unwrap();
        assert!(result.is_empty(), "should filter out non-matching authors");
    }

    #[test]
    fn rollup_aggregates_by_directory() {
        let files = vec![
            FileAuthorship {
                path: "src/a.rs".into(),
                authors: vec![("Alice".into(), 80), ("Bob".into(), 20)],
                total_lines: 100,
                bus_factor: 1,
            },
            FileAuthorship {
                path: "src/b.rs".into(),
                authors: vec![("Alice".into(), 50)],
                total_lines: 50,
                bus_factor: 1,
            },
            FileAuthorship {
                path: "tests/t.rs".into(),
                authors: vec![("Bob".into(), 30)],
                total_lines: 30,
                bus_factor: 1,
            },
        ];

        let modules = rollup_modules(&files);
        assert_eq!(modules.len(), 2);

        let src = modules.iter().find(|m| m.module == "src").unwrap();
        assert_eq!(src.file_count, 2);
        assert_eq!(src.total_lines, 150);
        assert_eq!(src.single_author_files, 1);

        let tests = modules.iter().find(|m| m.module == "tests").unwrap();
        assert_eq!(tests.file_count, 1);
        assert_eq!(tests.bus_factor, 1);
    }

    #[test]
    fn analyze_empty_file_list() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let _repo = init_repo(dir.path());
        let result = analyze_knowledge(dir.path(), &[], None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn analyze_nonexistent_file_skipped() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let repo = init_repo(dir.path());
        make_commit(&repo, dir.path(), &[("exists.rs", "fn f() {}\n")], "init");
        let result = analyze_knowledge(
            dir.path(),
            &["exists.rs".into(), "does_not_exist.rs".into()],
            None,
        )
        .unwrap();
        assert_eq!(result.len(), 1, "should only return the existing file");
        assert_eq!(result[0].path, "exists.rs");
    }

    #[test]
    fn analyze_non_git_directory_returns_error() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let result = analyze_knowledge(dir.path(), &["any.rs".into()], None);
        assert!(result.is_err(), "should fail on non-git directory");
    }

    #[test]
    fn author_filter_case_insensitive() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let repo = init_repo(dir.path());
        make_commit_as(
            &repo,
            dir.path(),
            &[("a.rs", "fn a() {}\n")],
            "commit",
            Author {
                name: "Alice Smith",
                email: "alice@test.com",
            },
        );
        // Filter with different case should still match
        let result = analyze_knowledge(dir.path(), &["a.rs".into()], Some("ALICE")).unwrap();
        assert_eq!(result.len(), 1, "case-insensitive filter should match");

        let result = analyze_knowledge(dir.path(), &["a.rs".into()], Some("alice smith")).unwrap();
        assert_eq!(result.len(), 1, "full name match should work");
    }

    #[test]
    fn bus_factor_dominant_author() {
        // One author owns 90%, others own 10% split across 10 people.
        let mut authors = vec![("Dominant".to_string(), 90)];
        for i in 0..10 {
            authors.push((format!("Minor{i}"), 1));
        }
        assert_eq!(compute_bus_factor(&authors, 100), 1);
    }

    #[test]
    fn bus_factor_perfectly_spread() {
        // 5 authors, 20 lines each = 100 total.
        // First author: 20/100 = 20% < 50%. First two: 40% < 50%. First three: 60% > 50%.
        let authors: Vec<(String, u32)> = (0..5).map(|i| (format!("Author{i}"), 20)).collect();
        assert_eq!(compute_bus_factor(&authors, 100), 3);
    }

    #[test]
    fn rollup_root_level_files() {
        // Files without a parent directory should map to "."
        let files = vec![FileAuthorship {
            path: "main.rs".into(),
            authors: vec![("Alice".into(), 10)],
            total_lines: 10,
            bus_factor: 1,
        }];
        let modules = rollup_modules(&files);
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].module, ".");
    }

    #[test]
    fn rollup_empty_input() {
        let modules = rollup_modules(&[]);
        assert!(modules.is_empty());
    }

    #[test]
    fn rollup_sorting_lowest_bus_factor_first() {
        let files = vec![
            FileAuthorship {
                path: "safe/a.rs".into(),
                authors: vec![("A".into(), 30), ("B".into(), 30), ("C".into(), 40)],
                total_lines: 100,
                bus_factor: 2,
            },
            FileAuthorship {
                path: "risky/b.rs".into(),
                authors: vec![("Solo".into(), 200)],
                total_lines: 200,
                bus_factor: 1,
            },
        ];
        let modules = rollup_modules(&files);
        assert_eq!(
            modules[0].module, "risky",
            "lowest bus factor should be first"
        );
        assert_eq!(modules[1].module, "safe");
    }

    #[test]
    fn analyze_total_lines_matches_file_content() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let repo = init_repo(dir.path());
        let content = "line1\nline2\nline3\nline4\nline5\n";
        make_commit(&repo, dir.path(), &[("five_lines.rs", content)], "init");
        let result = analyze_knowledge(dir.path(), &["five_lines.rs".into()], None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].total_lines, 5,
            "total_lines should match actual line count"
        );
    }

    #[test]
    fn analyze_multiple_files_independent() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let repo = init_repo(dir.path());
        make_commit_as(
            &repo,
            dir.path(),
            &[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\nfn b2() {}\n")],
            "init",
            Author {
                name: "Alice",
                email: "alice@test.com",
            },
        );
        let result = analyze_knowledge(dir.path(), &["a.rs".into(), "b.rs".into()], None).unwrap();
        assert_eq!(result.len(), 2, "should return both files");
        let a = result.iter().find(|f| f.path == "a.rs").unwrap();
        let b = result.iter().find(|f| f.path == "b.rs").unwrap();
        assert_eq!(a.total_lines, 1);
        assert_eq!(b.total_lines, 2);
    }
}
