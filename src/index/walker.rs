use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use super::languages;

pub fn walk_source_files(root: &Path) -> Vec<PathBuf> {
    let supported_ext: HashSet<&str> = languages::supported_extensions().into_iter().collect();
    let supported_names: HashSet<&str> = languages::supported_filenames().into_iter().collect();
    let supported_prefixes: Vec<&str> = languages::supported_prefixes();

    let mut files = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".qartezignore")
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!(error = %e, "walker: skipping entry");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();

        // Extension-based match (existing behavior)
        if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && supported_ext.contains(ext)
        {
            files.push(path.to_path_buf());
            continue;
        }

        // Filename-based match for extensionless files (Dockerfile, Makefile, etc.)
        if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
            if supported_names.contains(filename) {
                files.push(path.to_path_buf());
                continue;
            }
            // Prefix match (e.g. "Dockerfile.prod")
            if supported_prefixes.iter().any(|p| filename.starts_with(p)) {
                files.push(path.to_path_buf());
            }
        }
    }

    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn relative_paths(root: &Path, files: &[PathBuf]) -> Vec<String> {
        let mut out: Vec<String> = files
            .iter()
            .filter_map(|p| p.strip_prefix(root).ok())
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn test_qartezignore_excludes_directories() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("vendor/lib")).unwrap();
        fs::create_dir_all(root.join("generated")).unwrap();

        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("vendor/lib/dep.rs"), "pub fn dep() {}").unwrap();
        fs::write(root.join("generated/api.rs"), "pub struct Api;").unwrap();

        // Without .qartezignore all three files should appear
        let files = walk_source_files(root);
        let rel = relative_paths(root, &files);
        assert!(rel.contains(&"src/main.rs".to_string()));
        assert!(rel.contains(&"vendor/lib/dep.rs".to_string()));
        assert!(rel.contains(&"generated/api.rs".to_string()));

        // With .qartezignore vendor/ and generated/ are excluded
        fs::write(root.join(".qartezignore"), "vendor/\ngenerated/\n").unwrap();

        let files = walk_source_files(root);
        let rel = relative_paths(root, &files);
        assert!(rel.contains(&"src/main.rs".to_string()));
        assert!(!rel.contains(&"vendor/lib/dep.rs".to_string()));
        assert!(!rel.contains(&"generated/api.rs".to_string()));
    }

    #[test]
    fn test_qartezignore_supports_negation() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();

        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("src/generated.rs"), "pub struct Gen;").unwrap();
        fs::write(root.join("src/manual.rs"), "pub fn manual() {}").unwrap();

        // Exclude all .rs except generated.rs via negation
        fs::write(root.join(".qartezignore"), "src/*.rs\n!src/generated.rs\n").unwrap();

        let files = walk_source_files(root);
        let rel = relative_paths(root, &files);
        assert!(rel.contains(&"src/generated.rs".to_string()));
        assert!(!rel.contains(&"src/main.rs".to_string()));
        assert!(!rel.contains(&"src/manual.rs".to_string()));
    }

    #[test]
    fn test_qartezignore_glob_patterns() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests/snapshots")).unwrap();

        fs::write(root.join("src/lib.rs"), "pub fn lib() {}").unwrap();
        fs::write(root.join("tests/snapshots/snap.rs"), "fn snap() {}").unwrap();

        // Exclude with glob pattern
        fs::write(root.join(".qartezignore"), "**/snapshots/\n").unwrap();

        let files = walk_source_files(root);
        let rel = relative_paths(root, &files);
        assert!(rel.contains(&"src/lib.rs".to_string()));
        assert!(!rel.contains(&"tests/snapshots/snap.rs".to_string()));
    }

    #[test]
    fn test_no_qartezignore_indexes_everything() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("vendor")).unwrap();

        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("vendor/dep.rs"), "pub fn dep() {}").unwrap();

        // No .qartezignore file exists
        let files = walk_source_files(root);
        let rel = relative_paths(root, &files);
        assert!(rel.contains(&"src/main.rs".to_string()));
        assert!(rel.contains(&"vendor/dep.rs".to_string()));
    }
}
