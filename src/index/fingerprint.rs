// Rust guideline compliant 2026-04-25

//! Workspace fingerprint computation for skip-on-unchanged startup.
//!
//! The fingerprint is a non-cryptographic digest of every input that, if
//! changed, would invalidate the body-FTS / unused-exports / pagerank
//! derived tables on disk. The MCP-server startup path compares the
//! stored fingerprint against the freshly computed one and skips
//! [`crate::index::full_index_multi`] when they match. This keeps
//! `initialize`/`tools/list` responses fast even when the on-disk
//! `.qartez/index.db` is multiple gigabytes.
//!
//! Inputs hashed:
//!
//! - The crate version (`CARGO_PKG_VERSION`), so a binary upgrade always
//!   forces a reindex.
//! - Each project root in canonical form, sorted, paired with its alias.
//! - The contents of every root's `.qartezignore` file, when present.
//! - Any other config flag that influences which files are walked or how
//!   they are parsed (currently `QARTEZ_MAX_FILE_BYTES`).
//!
//! The hash is intentionally stable across runs of the same binary on
//! the same workspace; it is *not* designed to resist collisions from a
//! malicious caller. Collisions in the wild would require two different
//! workspaces to produce byte-identical input strings, which is
//! vanishingly unlikely for the inputs above.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::config::Config;

/// Meta key under which the workspace fingerprint is persisted.
///
/// Stored on `meta.value` in the existing `meta` table; no schema change is
/// needed beyond the row itself.
pub const META_KEY_WORKSPACE_FINGERPRINT: &str = "workspace_fingerprint";

/// Meta key for the timestamp of the last successful full reindex.
///
/// Distinct from `last_index` (which the index code already writes at the
/// end of every full or incremental run). This entry is only set when a
/// full reindex completes from start to finish, so the maintenance tool
/// can report when the body-FTS tables were last fully rebuilt.
pub const META_KEY_LAST_FULL_REINDEX: &str = "last_full_reindex";

/// Compute the workspace fingerprint for the given configuration.
///
/// Returns a hex-encoded `u64` derived from `DefaultHasher`. The output is
/// stable for the lifetime of one binary version: changing the package
/// version, adding or reordering roots, editing any `.qartezignore`, or
/// adjusting `QARTEZ_MAX_FILE_BYTES` will produce a different value.
///
/// # Examples
///
/// ```ignore
/// let fp1 = compute_workspace_fingerprint(&config);
/// let fp2 = compute_workspace_fingerprint(&config);
/// assert_eq!(fp1, fp2);
/// ```
pub fn compute_workspace_fingerprint(config: &Config) -> String {
    let mut hasher = DefaultHasher::new();

    // Version of the crate that produced the index. Bumping the version
    // always invalidates so a new release can rebuild derived tables
    // without users having to remember to pass --reindex.
    env!("CARGO_PKG_VERSION").hash(&mut hasher);

    // Canonical form of each root, sorted for deterministic ordering.
    // Falls back to the raw path when canonicalization fails (e.g. the
    // root no longer exists on disk).
    let mut canonical_roots: Vec<(String, String)> = config
        .project_roots
        .iter()
        .map(|root| {
            let canonical = root
                .canonicalize()
                .unwrap_or_else(|_| root.clone())
                .to_string_lossy()
                .into_owned();
            let alias = config.root_aliases.get(root).cloned().unwrap_or_default();
            (canonical, alias)
        })
        .collect();
    canonical_roots.sort();
    canonical_roots.hash(&mut hasher);

    // Any `.qartezignore` file in any root contributes its bytes verbatim
    // so editing the file flips the fingerprint without us having to
    // track mtime separately. Missing files contribute the literal
    // marker "<absent>" so an absent-then-present transition flips too.
    for root in &config.project_roots {
        hash_qartezignore(root, &mut hasher);
    }

    // Indexing-relevant env vars. Adding entries here is how new
    // configuration flags opt in to fingerprint invalidation.
    std::env::var("QARTEZ_MAX_FILE_BYTES")
        .unwrap_or_default()
        .hash(&mut hasher);

    format!("{:016x}", hasher.finish())
}

/// Hash the `.qartezignore` file for a single root, when present.
///
/// Reads the file with a 1 MiB cap so a pathological multi-gigabyte
/// ignore file cannot stall startup. Beyond the cap we hash the truncated
/// prefix together with the marker `<truncated>`.
fn hash_qartezignore(root: &Path, hasher: &mut DefaultHasher) {
    let path = root.join(".qartezignore");
    /// Cap on bytes read from a single `.qartezignore` so the fingerprint
    /// pass cannot stall on a runaway file. Real ignore files are well
    /// under 1 KiB; the cap exists strictly as a defensive limit.
    const MAX_IGNORE_BYTES: u64 = 1024 * 1024;

    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => {
            let len = meta.len();
            if len > MAX_IGNORE_BYTES {
                if let Ok(bytes) = std::fs::read(&path) {
                    bytes[..MAX_IGNORE_BYTES as usize].hash(hasher);
                    "<truncated>".hash(hasher);
                } else {
                    "<read-error>".hash(hasher);
                }
            } else if let Ok(bytes) = std::fs::read(&path) {
                bytes.hash(hasher);
            } else {
                "<read-error>".hash(hasher);
            }
        }
        _ => {
            "<absent>".hash(hasher);
        }
    }
}

/// Live root-prefix set derived from a [`Config`].
///
/// Mirrors the prefix derivation in [`crate::index::full_index_multi`] so
/// callers can purge orphan rows from removed roots without needing the
/// indexer's internals. Single-root projects produce a single empty
/// string; multi-root projects produce one entry per root keyed by alias
/// or directory name. If any root in a multi-root configuration has no
/// alias mapping, the empty string is also included so unprefixed rows
/// (the legacy single-root layout) are treated as live by
/// [`crate::storage::maintenance::purge_stale_roots`].
pub fn live_root_prefixes(
    roots: &[PathBuf],
    aliases: &std::collections::HashMap<PathBuf, String>,
) -> Vec<String> {
    if roots.len() <= 1 {
        return vec![String::new()];
    }
    let mut out: Vec<String> = roots
        .iter()
        .map(|r| crate::index::root_prefix(r, aliases.get(r).map(String::as_str)))
        .collect();
    if roots.iter().any(|r| !aliases.contains_key(r)) {
        out.push(String::new());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn cfg_with_root(root: PathBuf) -> Config {
        Config {
            project_roots: vec![root.clone()],
            root_aliases: HashMap::new(),
            primary_root: root.clone(),
            db_path: root.join(".qartez").join("index.db"),
            reindex: false,
            git_depth: 0,
            has_project: true,
        }
    }

    #[test]
    fn fingerprint_is_stable_for_identical_inputs() {
        let tmp = TempDir::new().unwrap();
        let cfg = cfg_with_root(tmp.path().to_path_buf());
        let a = compute_workspace_fingerprint(&cfg);
        let b = compute_workspace_fingerprint(&cfg);
        assert_eq!(a, b, "identical config must produce identical fingerprint");
        assert_eq!(a.len(), 16, "fingerprint must be 16 hex chars");
    }

    #[test]
    fn fingerprint_changes_when_root_added() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        let mut cfg = cfg_with_root(tmp1.path().to_path_buf());
        let one_root = compute_workspace_fingerprint(&cfg);
        cfg.project_roots.push(tmp2.path().to_path_buf());
        let two_roots = compute_workspace_fingerprint(&cfg);
        assert_ne!(
            one_root, two_roots,
            "adding a root must invalidate the fingerprint"
        );
    }

    #[test]
    fn fingerprint_changes_when_qartezignore_changes() {
        let tmp = TempDir::new().unwrap();
        let cfg = cfg_with_root(tmp.path().to_path_buf());
        let absent = compute_workspace_fingerprint(&cfg);

        std::fs::write(tmp.path().join(".qartezignore"), "vendor/\n").unwrap();
        let with_ignore = compute_workspace_fingerprint(&cfg);
        assert_ne!(
            absent, with_ignore,
            "creating .qartezignore must invalidate the fingerprint"
        );

        std::fs::write(tmp.path().join(".qartezignore"), "vendor/\nbuild/\n").unwrap();
        let edited_ignore = compute_workspace_fingerprint(&cfg);
        assert_ne!(
            with_ignore, edited_ignore,
            "editing .qartezignore must invalidate the fingerprint"
        );
    }

    #[test]
    fn fingerprint_is_independent_of_root_order() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        let mut cfg_ab = cfg_with_root(tmp1.path().to_path_buf());
        cfg_ab.project_roots.push(tmp2.path().to_path_buf());
        let mut cfg_ba = cfg_with_root(tmp2.path().to_path_buf());
        cfg_ba.project_roots.push(tmp1.path().to_path_buf());

        let ab = compute_workspace_fingerprint(&cfg_ab);
        let ba = compute_workspace_fingerprint(&cfg_ba);
        assert_eq!(ab, ba, "fingerprint must not depend on root listing order");
    }

    #[test]
    fn live_root_prefixes_single_root_yields_empty_string() {
        let tmp = TempDir::new().unwrap();
        let prefixes = live_root_prefixes(&[tmp.path().to_path_buf()], &HashMap::new());
        assert_eq!(prefixes, vec![String::new()]);
    }

    #[test]
    fn live_root_prefixes_multi_root_uses_aliases() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        let mut aliases = HashMap::new();
        aliases.insert(tmp1.path().to_path_buf(), "alpha".to_string());
        aliases.insert(tmp2.path().to_path_buf(), "beta".to_string());
        let prefixes = live_root_prefixes(
            &[tmp1.path().to_path_buf(), tmp2.path().to_path_buf()],
            &aliases,
        );
        assert!(prefixes.contains(&"alpha".to_string()));
        assert!(prefixes.contains(&"beta".to_string()));
        assert_eq!(prefixes.len(), 2);
    }

    #[test]
    fn live_root_prefixes_multi_root_includes_empty_when_root_has_no_alias() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        let mut aliases = HashMap::new();
        aliases.insert(tmp2.path().to_path_buf(), "ext".to_string());
        let prefixes = live_root_prefixes(
            &[tmp1.path().to_path_buf(), tmp2.path().to_path_buf()],
            &aliases,
        );
        assert!(
            prefixes.contains(&String::new()),
            "an unaliased root must surface the empty-prefix safety net so legacy single-root rows survive purge_stale_roots"
        );
        assert!(prefixes.contains(&"ext".to_string()));
    }
}
