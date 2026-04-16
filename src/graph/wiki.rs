// Rust guideline compliant 2026-04-12

//! Auto-generated architecture wiki.
//!
//! Rendering is a pure function of the cluster assignment, the file table,
//! and the symbol table: feed it a fixed DB and you always get the same
//! markdown back. That keeps the feature cheap to test and lets callers
//! re-run it as a post-indexing step without touching network or ffi.

use std::collections::{BTreeMap, HashMap};

use rusqlite::Connection;

use crate::error::Result;
use crate::graph::boundaries::Violation;
use crate::graph::leiden::{ClusterReport, LeidenConfig, MISC_CLUSTER_ID, compute_clusters};
use crate::storage::models::FileRow;
use crate::storage::read::{
    get_all_edges, get_all_file_clusters, get_all_files, get_edge_count, get_file_clusters_count,
};

pub struct WikiConfig {
    pub project_name: String,
    pub max_files_per_section: usize,
    pub max_top_symbols: usize,
    pub recompute: bool,
    pub leiden: LeidenConfig,
    /// Boundary violations pre-computed by the caller (typically via
    /// `boundaries::check_boundaries`). When non-empty, `render_wiki`
    /// appends a `[VIOLATION]` marker next to each inter-cluster edge
    /// that crosses a denied path pair. `None` keeps the markdown
    /// byte-identical to the pre-boundary output so callers that do
    /// not use architecture rules are unaffected.
    pub boundary_violations: Option<Vec<Violation>>,
}

impl Default for WikiConfig {
    fn default() -> Self {
        Self {
            project_name: "this codebase".to_string(),
            max_files_per_section: 20,
            max_top_symbols: 5,
            recompute: false,
            leiden: LeidenConfig::default(),
            boundary_violations: None,
        }
    }
}

struct ClusterView {
    id: i64,
    label: String,
    files: Vec<FileRow>,
    pagerank_sum: f64,
    top_symbols: Vec<(String, String, u32)>,
}

/// Build the architecture wiki. If no cluster assignment is stored or
/// `config.recompute` is set, run Leiden first; otherwise read the existing
/// `file_clusters` rows.
pub fn render_wiki(conn: &Connection, config: &WikiConfig) -> Result<(String, Option<f64>)> {
    let mut modularity: Option<f64> = None;
    let should_recompute = config.recompute || get_file_clusters_count(conn)? == 0;
    if should_recompute {
        let report: ClusterReport = compute_clusters(conn, &config.leiden)?;
        modularity = Some(report.modularity);
    }

    let raw: Vec<(i64, i64)> = get_all_file_clusters(conn)?;
    let files: Vec<FileRow> = get_all_files(conn)?;
    let edges: Vec<(i64, i64)> = get_all_edges(conn)?;
    let total_edges = get_edge_count(conn)?;

    let file_to_cluster: HashMap<i64, i64> = raw.iter().copied().collect();

    let mut buckets: BTreeMap<i64, Vec<FileRow>> = BTreeMap::new();
    for file in &files {
        let cid = file_to_cluster
            .get(&file.id)
            .copied()
            .unwrap_or(MISC_CLUSTER_ID);
        buckets.entry(cid).or_default().push(file.clone());
    }
    for rows in buckets.values_mut() {
        rows.sort_by(|a, b| {
            b.pagerank
                .partial_cmp(&a.pagerank)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.path.cmp(&b.path))
        });
    }

    let mut views: Vec<ClusterView> = Vec::new();
    for (&cid, rows) in &buckets {
        let pagerank_sum: f64 = rows.iter().map(|f| f.pagerank).sum();
        let label = cluster_label(cid, rows);
        let top_symbols = collect_top_symbols(conn, rows, config.max_top_symbols)?;
        views.push(ClusterView {
            id: cid,
            label,
            files: rows.clone(),
            pagerank_sum,
            top_symbols,
        });
    }
    views.sort_by(|a, b| {
        b.pagerank_sum
            .partial_cmp(&a.pagerank_sum)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    disambiguate_labels(&mut views);

    let mut inter: HashMap<(i64, i64), usize> = HashMap::new();
    for (from, to) in edges {
        let fc = file_to_cluster
            .get(&from)
            .copied()
            .unwrap_or(MISC_CLUSTER_ID);
        let tc = file_to_cluster.get(&to).copied().unwrap_or(MISC_CLUSTER_ID);
        if fc == tc {
            continue;
        }
        *inter.entry((fc, tc)).or_insert(0) += 1;
    }

    let path_to_file: HashMap<&str, i64> = files.iter().map(|f| (f.path.as_str(), f.id)).collect();
    let mut violation_pairs: std::collections::HashSet<(i64, i64)> =
        std::collections::HashSet::new();
    if let Some(violations) = config.boundary_violations.as_ref() {
        for v in violations {
            let (Some(&fid), Some(&tid)) = (
                path_to_file.get(v.from_file.as_str()),
                path_to_file.get(v.to_file.as_str()),
            ) else {
                continue;
            };
            let fc = file_to_cluster
                .get(&fid)
                .copied()
                .unwrap_or(MISC_CLUSTER_ID);
            let tc = file_to_cluster
                .get(&tid)
                .copied()
                .unwrap_or(MISC_CLUSTER_ID);
            if fc == tc {
                continue;
            }
            violation_pairs.insert((fc, tc));
        }
    }

    let mut out = String::new();
    out.push_str(&format!("# Architecture of {}\n\n", config.project_name));
    let date = current_date();
    let modularity_line = modularity
        .map(|q| format!(", modularity {q:.2}"))
        .unwrap_or_default();
    out.push_str(&format!(
        "Generated by Qartez on {} from {} files and {} import edges, partitioned into {} clusters by a deterministic Louvain pass with Leiden-style connectedness refinement (resolution {:.2}{modularity_line}).\n\n",
        date,
        files.len(),
        total_edges,
        views.len(),
        config.leiden.resolution,
    ));

    out.push_str("## Table of contents\n\n");
    for (i, view) in views.iter().enumerate() {
        out.push_str(&format!(
            "{}. [{}](#{}) - {} files, PageRank sum {:.3}\n",
            i + 1,
            view.label,
            anchor(&view.label),
            view.files.len(),
            view.pagerank_sum,
        ));
    }
    out.push_str("\n---\n\n");

    for view in &views {
        out.push_str(&format!("## {}\n\n", view.label));

        if view.top_symbols.is_empty() {
            out.push_str("**Top symbols:** _(no indexed symbols)_\n\n");
        } else {
            out.push_str("**Top symbols:** ");
            let rendered: Vec<String> = view
                .top_symbols
                .iter()
                .map(|(name, path, line)| format!("`{name}` ({path}:L{line})"))
                .collect();
            out.push_str(&rendered.join(", "));
            out.push_str("\n\n");
        }

        out.push_str("**Files:**\n");
        let cap = config.max_files_per_section.min(view.files.len());
        for file in view.files.iter().take(cap) {
            out.push_str(&format!(
                "- `{}` (PageRank {:.3})\n",
                file.path, file.pagerank,
            ));
        }
        if view.files.len() > cap {
            out.push_str(&format!("- ... and {} more\n", view.files.len() - cap));
        }
        out.push('\n');

        let mut outgoing: Vec<(i64, usize)> = inter
            .iter()
            .filter_map(|(&(from, to), &count)| {
                if from == view.id {
                    Some((to, count))
                } else {
                    None
                }
            })
            .collect();
        outgoing.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut incoming: Vec<(i64, usize)> = inter
            .iter()
            .filter_map(|(&(from, to), &count)| {
                if to == view.id {
                    Some((from, count))
                } else {
                    None
                }
            })
            .collect();
        incoming.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let label_for = |cid: i64| -> String {
            views
                .iter()
                .find(|v| v.id == cid)
                .map(|v| v.label.clone())
                .unwrap_or_else(|| format!("cluster_{cid}"))
        };

        if !outgoing.is_empty() {
            let list: Vec<String> = outgoing
                .iter()
                .map(|(cid, n)| {
                    let marker = if violation_pairs.contains(&(view.id, *cid)) {
                        " [VIOLATION]"
                    } else {
                        ""
                    };
                    format!("{} ({n} edges){marker}", label_for(*cid))
                })
                .collect();
            out.push_str(&format!("**Imports from:** {}\n", list.join(", ")));
        }
        if !incoming.is_empty() {
            let list: Vec<String> = incoming
                .iter()
                .map(|(cid, n)| {
                    let marker = if violation_pairs.contains(&(*cid, view.id)) {
                        " [VIOLATION]"
                    } else {
                        ""
                    };
                    format!("{} ({n} edges){marker}", label_for(*cid))
                })
                .collect();
            out.push_str(&format!("**Imported by:** {}\n", list.join(", ")));
        }
        if outgoing.is_empty() && incoming.is_empty() {
            out.push_str("_(no inter-cluster edges)_\n");
        }

        out.push_str("\n---\n\n");
    }

    Ok((out, modularity))
}

fn collect_top_symbols(
    conn: &Connection,
    files: &[FileRow],
    limit: usize,
) -> Result<Vec<(String, String, u32)>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: String = (0..files.len())
        .map(|i| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT name, file_id, line_start FROM symbols \
         WHERE file_id IN ({placeholders}) AND is_exported = 1"
    );

    let file_map: HashMap<i64, &FileRow> = files.iter().map(|f| (f.id, f)).collect();
    let params: Vec<&dyn rusqlite::types::ToSql> = files
        .iter()
        .map(|f| &f.id as &dyn rusqlite::types::ToSql)
        .collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, u32>(2)?,
        ))
    })?;

    let mut all: Vec<(String, String, u32, f64)> = Vec::new();
    for row in rows {
        let (name, file_id, line_start) = row?;
        if let Some(file) = file_map.get(&file_id) {
            all.push((name, file.path.clone(), line_start, file.pagerank));
        }
    }

    all.sort_by(|a, b| {
        b.3.partial_cmp(&a.3)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    all.truncate(limit);
    Ok(all.into_iter().map(|(n, p, l, _)| (n, p, l)).collect())
}

/// Derive a human-readable label for a cluster. The misc bucket is always
/// "misc"; otherwise the heuristic is, in order:
///
/// 1. Longest common path prefix of depth ≥ 2 (so `src/graph` wins).
/// 2. Most common second-level directory - when files straddle `src/`
///    (e.g. a cluster containing `src/error.rs`, `src/storage/*`,
///    `src/graph/*`), this picks `src/storage` because that is the modal
///    subdirectory and a reader recognises it faster than the top file
///    stem.
/// 3. Stem of the top-PageRank file as a last resort.
///
/// All three branches are deterministic and run offline.
fn cluster_label(cid: i64, files: &[FileRow]) -> String {
    if cid == MISC_CLUSTER_ID {
        return "misc".to_string();
    }
    if files.is_empty() {
        return format!("cluster_{cid}");
    }
    if let Some(prefix) = longest_common_dir(files)
        && prefix.split('/').count() >= 2
    {
        return prefix;
    }
    if let Some(modal) = modal_subdirectory(files) {
        return modal;
    }
    match top_file_by_pagerank(files) {
        Some(top) => path_stem(&top.path),
        None => format!("cluster_{cid}"),
    }
}

/// Count the files inside each two-segment directory prefix and return
/// the prefix with the most files, provided that prefix covers at least
/// 30% of the cluster's files. Used when the longest common prefix is
/// only the crate root, so a cluster spanning `src/storage/*`, `src/graph/*`,
/// `src/error.rs` still gets labelled after one of its dominant
/// subdirectories (tie-broken alphabetically) instead of the top-PageRank
/// file stem, which is almost always less informative.
fn modal_subdirectory(files: &[FileRow]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for file in files {
        let parts: Vec<&str> = file.path.split('/').collect();
        if parts.len() < 3 {
            continue;
        }
        let prefix = format!("{}/{}", parts[0], parts[1]);
        *counts.entry(prefix).or_insert(0) += 1;
    }
    // Sort by (count desc, label asc) so ties break alphabetically and
    // the pick is deterministic across re-runs.
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let (label, count) = ranked.into_iter().next()?;
    if count * 10 >= files.len() * 3 {
        Some(label)
    } else {
        None
    }
}

/// Make every label unique by suffixing collisions with the first
/// distinguishing file stem, and if that still duplicates, the cluster
/// id. Runs after sorting so the dominant cluster keeps the short name.
fn disambiguate_labels(views: &mut [ClusterView]) {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for view in views.iter_mut() {
        let base = view.label.clone();
        let count = seen.entry(base.clone()).or_insert(0);
        if *count == 0 {
            *count += 1;
            continue;
        }
        *count += 1;
        let candidate = pick_distinguishing_suffix(&base, view)
            .unwrap_or_else(|| format!("{}-{}", base, view.id));
        let existing = seen.get(&candidate).copied().unwrap_or(0);
        if existing == 0 {
            seen.insert(candidate.clone(), 1);
            view.label = candidate;
        } else {
            let fallback = format!("{}-{}", base, view.id);
            seen.insert(fallback.clone(), 1);
            view.label = fallback;
        }
    }
}

/// Pick a short, stable suffix to append to a label that already exists.
/// Walks the cluster's files in PageRank order and returns the first
/// stem that is not identical to the base label's tail. For example a
/// `src/benchmark` cluster whose top file is `mod.rs` falls through to
/// `report.rs` and yields `src/benchmark-report`. Returns None when every
/// file stem duplicates the base; the caller then falls back to the
/// cluster id.
fn pick_distinguishing_suffix(base: &str, view: &ClusterView) -> Option<String> {
    let base_tail = base.rsplit_once('/').map(|(_, t)| t).unwrap_or(base);
    let mut ranked: Vec<&FileRow> = view.files.iter().collect();
    ranked.sort_by(|a, b| {
        b.pagerank
            .partial_cmp(&a.pagerank)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    for file in ranked {
        let stem = path_stem(&file.path);
        if stem.is_empty() {
            continue;
        }
        if stem == base || stem == base_tail {
            continue;
        }
        return Some(format!("{base}-{stem}"));
    }
    None
}

fn top_file_by_pagerank(files: &[FileRow]) -> Option<&FileRow> {
    files.iter().max_by(|a, b| {
        a.pagerank
            .partial_cmp(&b.pagerank)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.path.cmp(&a.path))
    })
}

fn longest_common_dir(files: &[FileRow]) -> Option<String> {
    let first = files.first()?;
    let first_parts: Vec<&str> = first.path.split('/').collect();
    if first_parts.len() <= 1 {
        return None;
    }
    let mut shared: usize = first_parts.len() - 1;
    for file in files.iter().skip(1) {
        let parts: Vec<&str> = file.path.split('/').collect();
        let limit = shared.min(parts.len().saturating_sub(1));
        let mut matched = 0;
        while matched < limit && parts[matched] == first_parts[matched] {
            matched += 1;
        }
        shared = matched;
        if shared == 0 {
            return None;
        }
    }
    if shared == 0 {
        return None;
    }
    Some(first_parts[..shared].join("/"))
}

fn path_stem(path: &str) -> String {
    let without_ext = path.rsplit_once('.').map(|(b, _)| b).unwrap_or(path);
    let trimmed = without_ext.trim_end_matches("/mod");
    trimmed
        .rsplit_once('/')
        .map(|(_, name)| name.to_string())
        .unwrap_or_else(|| trimmed.to_string())
}

fn anchor(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == ' ' || ch == '-' || ch == '_' || ch == '/' {
            out.push('-');
        }
    }
    out
}

fn current_date() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_ymd(now)
}

fn format_ymd(epoch_secs: i64) -> String {
    // Conway's "Doomsday"-free rendering: convert UTC epoch to YYYY-MM-DD
    // without pulling a full date crate. The calendar is cheap because we
    // only need days, not hours - the wiki header is static metadata.
    let days = epoch_secs.div_euclid(86_400);
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}")
}

fn days_to_ymd(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::SymbolInsert;
    use crate::storage::schema::create_schema;
    use crate::storage::write;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        create_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn format_ymd_known_dates() {
        assert_eq!(format_ymd(0), "1970-01-01");
        assert_eq!(format_ymd(1_776_038_400), "2026-04-13");
        assert_eq!(format_ymd(1_582_934_400), "2020-02-29");
    }

    #[test]
    fn anchor_sanitizes_label() {
        assert_eq!(anchor("src/graph"), "src-graph");
        assert_eq!(anchor("Indexer_01"), "indexer-01");
        assert_eq!(anchor("misc"), "misc");
    }

    #[test]
    fn path_stem_strips_extension_and_mod() {
        assert_eq!(path_stem("src/graph/pagerank.rs"), "pagerank");
        assert_eq!(path_stem("src/graph/mod.rs"), "graph");
        assert_eq!(path_stem("lib.rs"), "lib");
    }

    #[test]
    fn longest_common_dir_finds_prefix() {
        let files = vec![
            make_file("src/graph/mod.rs"),
            make_file("src/graph/pagerank.rs"),
            make_file("src/graph/blast.rs"),
        ];
        assert_eq!(longest_common_dir(&files), Some("src/graph".to_string()));
    }

    #[test]
    fn longest_common_dir_none_when_disjoint() {
        let files = vec![make_file("src/alpha.rs"), make_file("other/beta.rs")];
        assert_eq!(longest_common_dir(&files), None);
    }

    #[test]
    fn modal_subdirectory_picks_the_plurality_prefix() {
        let files = vec![
            make_file("src/storage/read.rs"),
            make_file("src/storage/write.rs"),
            make_file("src/storage/mod.rs"),
            make_file("src/graph/blast.rs"),
            make_file("src/error.rs"),
        ];
        assert_eq!(modal_subdirectory(&files), Some("src/storage".to_string()));
    }

    #[test]
    fn modal_subdirectory_breaks_ties_alphabetically() {
        let files = vec![
            make_file("src/storage/a.rs"),
            make_file("src/storage/b.rs"),
            make_file("src/graph/c.rs"),
            make_file("src/graph/d.rs"),
        ];
        assert_eq!(modal_subdirectory(&files), Some("src/graph".to_string()));
    }

    #[test]
    fn modal_subdirectory_returns_none_when_all_singletons() {
        let files = vec![
            make_file("src/a/x.rs"),
            make_file("src/b/y.rs"),
            make_file("src/c/z.rs"),
            make_file("src/d/w.rs"),
        ];
        assert_eq!(modal_subdirectory(&files), None);
    }

    fn make_file(path: &str) -> FileRow {
        FileRow {
            id: 0,
            path: path.to_string(),
            mtime_ns: 0,
            size_bytes: 0,
            language: "rust".to_string(),
            line_count: 0,
            pagerank: 0.0,
            indexed_at: 0,
            change_count: 0,
        }
    }

    #[test]
    fn render_wiki_includes_clusters_and_inter_cluster_edges() {
        let conn = setup();
        let f_a = write::upsert_file(&conn, "src/auth/login.rs", 1000, 100, "rust", 10).unwrap();
        let f_b = write::upsert_file(&conn, "src/auth/token.rs", 1000, 100, "rust", 10).unwrap();
        let f_c = write::upsert_file(&conn, "src/auth/session.rs", 1000, 100, "rust", 10).unwrap();
        let f_d = write::upsert_file(&conn, "src/storage/blob.rs", 1000, 100, "rust", 10).unwrap();
        let f_e = write::upsert_file(&conn, "src/storage/index.rs", 1000, 100, "rust", 10).unwrap();
        let f_f = write::upsert_file(&conn, "src/storage/cache.rs", 1000, 100, "rust", 10).unwrap();

        write::update_pagerank(&conn, f_a, 0.3).unwrap();
        write::update_pagerank(&conn, f_b, 0.2).unwrap();
        write::update_pagerank(&conn, f_c, 0.1).unwrap();
        write::update_pagerank(&conn, f_d, 0.25).unwrap();
        write::update_pagerank(&conn, f_e, 0.1).unwrap();
        write::update_pagerank(&conn, f_f, 0.05).unwrap();

        write::insert_edge(&conn, f_a, f_b, "import", None).unwrap();
        write::insert_edge(&conn, f_b, f_c, "import", None).unwrap();
        write::insert_edge(&conn, f_c, f_a, "import", None).unwrap();
        write::insert_edge(&conn, f_d, f_e, "import", None).unwrap();
        write::insert_edge(&conn, f_e, f_f, "import", None).unwrap();
        write::insert_edge(&conn, f_f, f_d, "import", None).unwrap();
        write::insert_edge(&conn, f_a, f_d, "import", None).unwrap();

        for (id, name) in [
            (f_a, "login_handler"),
            (f_b, "issue_token"),
            (f_d, "put_blob"),
            (f_e, "index_entry"),
        ] {
            write::insert_symbols(
                &conn,
                id,
                &[SymbolInsert {
                    name: name.to_string(),
                    kind: "function".to_string(),
                    line_start: 1,
                    line_end: 5,
                    signature: None,
                    is_exported: true,
                    shape_hash: None,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                }],
            )
            .unwrap();
        }

        let config = WikiConfig {
            project_name: "test-project".to_string(),
            recompute: true,
            ..Default::default()
        };
        let (md, modularity) = render_wiki(&conn, &config).unwrap();
        assert!(md.contains("# Architecture of test-project"));
        assert!(md.contains("## Table of contents"));
        assert!(md.contains("src/auth"));
        assert!(md.contains("src/storage"));
        assert!(md.contains("Imports from") || md.contains("Imported by"));
        assert!(modularity.is_some());
        assert!(md.contains("PageRank"));
    }
}
