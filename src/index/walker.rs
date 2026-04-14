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
            && supported_ext.contains(ext) {
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
