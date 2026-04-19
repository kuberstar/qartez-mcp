// Rust guideline compliant 2026-04-15

//! Per-file tree-sitter parse cache, keyed by relative path. Each entry is
//! mtime-stamped so a stale file triggers reparse on next access.
//!
//! Why: `qartez_rename` and `qartez_calls` both walk tree-sitter ASTs of large
//! files (e.g. `src/server/mod.rs`, ~2300 lines of Rust). A cold parse of
//! that file runs in the 3-6 ms range, and walking it for call names is
//! another ~0.5 ms. On repeated invocations (benchmark warmup + measured
//! runs, multi-file renames that revisit the definition file, depth-2 call
//! hierarchies), those costs dominate. Caching source / tree / call sites
//! turns the steady-state cost into a HashMap lookup plus a shallow clone
//! of `Arc` handles.
//!
//! Fields are populated lazily: a caller that only needs the raw source
//! (e.g. the text prefilter in `qartez_calls`) does not pay the parse cost,
//! and a caller that needs pre-extracted call sites gets them walked once
//! per file lifetime.

use std::collections::HashMap;
use std::sync::Arc;

use super::treesitter::{IdentMap, collect_call_names, collect_identifiers_grouped};
use crate::index::languages;

#[derive(Default)]
pub(super) struct ParseCache {
    pub entries: HashMap<String, ParseEntry>,
}

#[derive(Default)]
pub(super) struct ParseEntry {
    pub mtime_ns: i64,
    pub source: Option<Arc<String>>,
    pub tree: Option<Arc<tree_sitter::Tree>>,
    pub calls: Option<Arc<Vec<(String, usize)>>>,
    /// Full name->occurrences map built from a single AST walk. Used by
    /// `qartez_rename` to skip the per-name walk on repeat invocations.
    pub idents: Option<Arc<IdentMap>>,
}

impl super::QartezServer {
    /// Read a file's mtime in nanoseconds, or `None` if the file is missing
    /// or the filesystem does not expose a modification time.
    pub(super) fn file_mtime_ns(&self, rel_path: &str) -> Option<i64> {
        let abs_path = self.safe_resolve(rel_path).ok()?;
        std::fs::metadata(&abs_path)
            .ok()?
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| i64::try_from(d.as_nanos()).unwrap_or(0))
    }

    /// Return the cached source for `rel_path`, reading and caching it on
    /// the first miss. Returns `None` only when the file cannot be read.
    ///
    /// Used by the `qartez_calls` callers-loop prefilter: reading is cheap,
    /// parsing is not, so we check whether the identifier is textually
    /// present before committing to a parse.
    pub(super) fn cached_source(&self, rel_path: &str) -> Option<Arc<String>> {
        let mtime_ns = self.file_mtime_ns(rel_path)?;
        if let Ok(cache) = self.parse_cache.lock()
            && let Some(entry) = cache.entries.get(rel_path)
            && entry.mtime_ns == mtime_ns
            && let Some(src) = &entry.source
        {
            return Some(src.clone());
        }

        let abs_path = self.safe_resolve(rel_path).ok()?;
        let raw = std::fs::read_to_string(&abs_path).ok()?;
        let arc = Arc::new(raw);

        if let Ok(mut cache) = self.parse_cache.lock() {
            let entry = cache.entries.entry(rel_path.to_string()).or_default();
            if entry.mtime_ns != mtime_ns {
                *entry = ParseEntry::default();
                entry.mtime_ns = mtime_ns;
            }
            entry.source = Some(arc.clone());
        }
        Some(arc)
    }

    /// Return a parsed tree-sitter tree for `rel_path`, along with its
    /// source bytes. Parses on first miss, returns from cache on subsequent
    /// calls with matching mtime.
    ///
    /// Returns `None` when the file cannot be read, the extension has no
    /// language support, or the parse itself fails. Callers must fall back
    /// to non-AST paths in those cases.
    pub(super) fn cached_tree(
        &self,
        rel_path: &str,
    ) -> Option<(Arc<String>, Arc<tree_sitter::Tree>)> {
        let mtime_ns = self.file_mtime_ns(rel_path)?;
        if let Ok(cache) = self.parse_cache.lock()
            && let Some(entry) = cache.entries.get(rel_path)
            && entry.mtime_ns == mtime_ns
            && let (Some(src), Some(tree)) = (&entry.source, &entry.tree)
        {
            return Some((src.clone(), tree.clone()));
        }

        let source_arc = self.cached_source(rel_path)?;
        let ext = rel_path.rsplit('.').next().unwrap_or("");
        let lang_support = languages::get_language_for_ext(ext)?;
        let ts_lang = lang_support.tree_sitter_language(ext);
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).ok()?;
        let tree = parser.parse(source_arc.as_bytes(), None)?;
        let tree_arc = Arc::new(tree);

        if let Ok(mut cache) = self.parse_cache.lock() {
            let entry = cache.entries.entry(rel_path.to_string()).or_default();
            if entry.mtime_ns != mtime_ns {
                *entry = ParseEntry::default();
                entry.mtime_ns = mtime_ns;
                entry.source = Some(source_arc.clone());
            }
            entry.tree = Some(tree_arc.clone());
        }
        Some((source_arc, tree_arc))
    }

    /// Return the per-file identifier map for `rel_path`, keyed by
    /// identifier text. Walks the cached tree exactly once per file
    /// mtime; subsequent lookups (any name, any tool) are just a
    /// `HashMap::get`.
    ///
    /// Returns `None` when the file cannot be parsed (unsupported
    /// language or read error). Callers must fall back to a word-boundary
    /// scan in that case.
    pub(super) fn cached_idents(&self, rel_path: &str) -> Option<Arc<IdentMap>> {
        let mtime_ns = self.file_mtime_ns(rel_path)?;
        if let Ok(cache) = self.parse_cache.lock()
            && let Some(entry) = cache.entries.get(rel_path)
            && entry.mtime_ns == mtime_ns
            && let Some(idents) = &entry.idents
        {
            return Some(idents.clone());
        }

        let (source_arc, tree_arc) = self.cached_tree(rel_path)?;
        let mut map: IdentMap = HashMap::new();
        collect_identifiers_grouped(&mut tree_arc.walk(), source_arc.as_bytes(), &mut map);
        let arc = Arc::new(map);

        if let Ok(mut cache) = self.parse_cache.lock()
            && let Some(entry) = cache.entries.get_mut(rel_path)
        {
            entry.idents = Some(arc.clone());
        }
        Some(arc)
    }

    /// Return the pre-extracted call sites for `rel_path`. Walks the cached
    /// tree on first miss, returns the cached vector on subsequent calls.
    /// Returns an empty vector when the file cannot be parsed.
    pub(super) fn cached_calls(&self, rel_path: &str) -> Arc<Vec<(String, usize)>> {
        if let Some(mtime_ns) = self.file_mtime_ns(rel_path)
            && let Ok(cache) = self.parse_cache.lock()
            && let Some(entry) = cache.entries.get(rel_path)
            && entry.mtime_ns == mtime_ns
            && let Some(calls) = &entry.calls
        {
            return calls.clone();
        }

        let Some((source_arc, tree_arc)) = self.cached_tree(rel_path) else {
            return Arc::new(Vec::new());
        };
        let mut results = Vec::new();
        collect_call_names(&mut tree_arc.walk(), source_arc.as_bytes(), &mut results);
        let arc = Arc::new(results);

        if let Ok(mut cache) = self.parse_cache.lock()
            && let Some(entry) = cache.entries.get_mut(rel_path)
        {
            entry.calls = Some(arc.clone());
        }
        arc
    }
}
