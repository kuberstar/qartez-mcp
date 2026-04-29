//! `GET /api/graph` - architecture map node + link payload for the M4
//! force-directed graph view.
//!
//! Reads `<project_root>/.qartez/index.db` and returns the top N files by
//! PageRank along with the subset of `edges` rows where both endpoints are
//! still in the kept set. Field names on links use d3-force conventions
//! (`source` / `target`) so the browser can pass the response straight into
//! a simulation without remapping.
//!
//! When the index DB is missing the handler returns an empty graph rather
//! than 404, matching the graceful behavior of `/api/project`. The caller
//! is expected to render an "indexing in progress" placeholder rather than
//! treat absence as an error.
//!
//! `truncated` is true when `total_files > limit`. The UI uses this to show
//! a "showing top N of M" affordance.
//!
//! When the caller sets `?with_cochanges=true`, the response also includes
//! co-change pairs from the `co_changes` table, restricted to file pairs
//! where both endpoints are in the kept node set and `count >= 2`. The
//! field is always present (empty `Vec` by default) so the response shape
//! is stable regardless of the flag.
//!
//! M6 (Signal density): every node carries a `hot_score` in `[0, 1]` and
//! an optional `cluster_id`. `hot_score` is computed as `pagerank * (1 +
//! ln(1 + max_complexity)) * (1 + ln(1 + change_count))` and then min-max
//! normalized across the response so the UI can color-code without
//! re-scanning. `cluster_id` is read from `file_clusters` via LEFT JOIN
//! and is `null` for files that were never assigned a cluster.
//!
//! M6 also introduces `?neighbors_of=PATH` which returns the 1-hop
//! neighborhood (importers, importees, co-change partners) of a single
//! file, capped at 100 nodes total. Used by the dashboard's "focus on
//! file" affordance.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Default node count when the caller omits `?limit=`. Chosen to render
/// fluidly in d3-force on commodity hardware while still showing enough
/// of the project to be useful.
const DEFAULT_LIMIT: i64 = 200;

/// Hard ceiling on `?limit=`. Above this the simulation gets sluggish in
/// the browser and the SQL `IN (...)` placeholder list grows large enough
/// to bump against SQLite's default expression depth.
const MAX_LIMIT: i64 = 1000;

/// Maximum node count returned by `?neighbors_of=PATH` (target file plus
/// up to 99 highest-PageRank neighbors). Chosen so the focused view stays
/// legible in the UI even when a hub file has hundreds of importers.
const SUBGRAPH_NODE_CAP: i64 = 100;

/// Query string for `GET /api/graph?limit=N&with_cochanges=true&neighbors_of=PATH`.
#[derive(Debug, Deserialize)]
pub struct GraphQuery {
    /// Maximum number of nodes to return. Clamped to `[1, MAX_LIMIT]`;
    /// invalid values fall back to `DEFAULT_LIMIT`. Ignored when
    /// `neighbors_of` is set (the subgraph cap takes over).
    pub limit: Option<i64>,
    /// When true, populate `cochanges` in the response with co-change pairs
    /// from the `co_changes` table. Defaults to false to keep the default
    /// payload small for callers that only need the structural graph.
    pub with_cochanges: Option<bool>,
    /// When set, restrict the response to the 1-hop neighborhood of the
    /// file at this exact path (importers, importees, co-change partners
    /// with `count >= 2`). Capped at `SUBGRAPH_NODE_CAP` total nodes by
    /// PageRank. An unknown path returns an empty graph (not a 404).
    pub neighbors_of: Option<String>,
}

/// One node in the architecture map. Field names are emitted verbatim;
/// the UI sorts by `pagerank` and labels by `path`.
#[derive(Debug, Serialize)]
pub struct GraphNode {
    pub id: i64,
    pub path: String,
    pub language: String,
    pub pagerank: f64,
    pub loc: i64,
    /// M6 signal density: `pagerank * (1 + ln(1 + max_cc)) * (1 + ln(1 +
    /// change_count))` min-max normalized across the response. Always in
    /// `[0, 1]`. When the response has only one node, or all raw scores
    /// are identical, every value is `0.0`.
    pub hot_score: f64,
    /// Cluster id from `file_clusters`, or `null` when the file has no
    /// cluster assignment (or the `file_clusters` table is missing).
    pub cluster_id: Option<i64>,
}

/// One directed edge in the graph. `source` and `target` are file IDs
/// matching `GraphNode::id`. The names follow d3-force's expected schema.
#[derive(Debug, Serialize)]
pub struct GraphLink {
    pub source: i64,
    pub target: i64,
    pub kind: String,
}

/// One co-change pair: two files that historically changed together in
/// git, with the number of shared commits. The pair is emitted with
/// `source` = `min(file_a, file_b)` and `target` = `max(file_a, file_b)`
/// so the frontend can dedupe by `(source, target)` regardless of how
/// `co_changes` stored the canonical orientation.
#[derive(Debug, Serialize)]
pub struct CoChange {
    pub source: i64,
    pub target: i64,
    pub count: i64,
}

/// Response body for `GET /api/graph`.
#[derive(Debug, Serialize)]
pub struct GraphResponse {
    pub nodes: Vec<GraphNode>,
    pub links: Vec<GraphLink>,
    pub truncated: bool,
    pub cochanges: Vec<CoChange>,
}

/// JSON error envelope returned on 500.
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: &'static str,
}

/// Raw row read from the `files` (and optionally `file_clusters`) join
/// before normalization. Carries the inputs needed to compute `hot_score`
/// without making a second pass over the result set.
struct RawNode {
    id: i64,
    path: String,
    language: String,
    pagerank: f64,
    loc: i64,
    change_count: i64,
    cluster_id: Option<i64>,
}

/// Handle `GET /api/graph?limit=N`.
///
/// # Errors
///
/// - `500 Internal Server Error` when the spawn-blocking task panics or
///   SQLite returns an error.
///
/// A missing index DB is not an error; the handler returns 200 with an
/// empty graph so the UI can show a placeholder.
pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<GraphQuery>,
) -> Result<Json<GraphResponse>, (StatusCode, Json<ApiError>)> {
    let limit = clamp_limit(query.limit);
    let with_cochanges = query.with_cochanges.unwrap_or(false);
    let neighbors_of = query.neighbors_of.clone();
    let root = state.project_root().to_path_buf();

    let result = tokio::task::spawn_blocking(move || match neighbors_of {
        Some(path) => compute_graph_neighbors_at_root(&root, &path, with_cochanges),
        None => compute_graph_at_root(&root, limit, with_cochanges),
    })
    .await
    .map_err(|error| {
        tracing::error!(?error, "graph.join.failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "join error",
            }),
        )
    })?;

    match result {
        Ok(Some(graph)) => Ok(Json(graph)),
        Ok(None) => Ok(Json(GraphResponse {
            nodes: Vec::new(),
            links: Vec::new(),
            truncated: false,
            cochanges: Vec::new(),
        })),
        Err(error) => {
            tracing::error!(?error, "graph.query.failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError { error: "internal" }),
            ))
        }
    }
}

fn clamp_limit(requested: Option<i64>) -> i64 {
    match requested {
        Some(value) if (1..=MAX_LIMIT).contains(&value) => value,
        _ => DEFAULT_LIMIT,
    }
}

fn compute_graph_at_root(
    root: &Path,
    limit: i64,
    with_cochanges: bool,
) -> anyhow::Result<Option<GraphResponse>> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(&db_path)?;
    Ok(Some(compute_graph(&conn, limit, with_cochanges)?))
}

fn compute_graph_neighbors_at_root(
    root: &Path,
    path: &str,
    with_cochanges: bool,
) -> anyhow::Result<Option<GraphResponse>> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(&db_path)?;
    Ok(Some(compute_graph_neighbors(&conn, path, with_cochanges)?))
}

/// Compute the graph payload from an open SQLite connection.
///
/// Factored out so unit tests can drive it against an in-memory DB
/// without spinning up the HTTP layer.
pub(crate) fn compute_graph(
    conn: &Connection,
    limit: i64,
    with_cochanges: bool,
) -> anyhow::Result<GraphResponse> {
    let raw_nodes = load_nodes(conn, limit)?;
    let total_files = load_total_files(conn)?;
    let kept_ids: HashSet<i64> = raw_nodes.iter().map(|n| n.id).collect();
    let max_cc = load_max_cc(conn)?;
    let nodes = finalize_nodes(raw_nodes, &max_cc);
    let links = load_links(conn, &kept_ids)?;
    let cochanges = if with_cochanges {
        load_cochanges(conn, &kept_ids)?
    } else {
        Vec::new()
    };
    let truncated = i64::try_from(nodes.len()).unwrap_or(i64::MAX) < total_files;

    Ok(GraphResponse {
        nodes,
        links,
        truncated,
        cochanges,
    })
}

/// Compute the 1-hop neighborhood of `path`. Returns an empty response
/// when the file is unknown so the UI can surface "no such file" without
/// the handler returning 404.
pub(crate) fn compute_graph_neighbors(
    conn: &Connection,
    path: &str,
    with_cochanges: bool,
) -> anyhow::Result<GraphResponse> {
    let target_id: Option<i64> = conn
        .query_row("SELECT id FROM files WHERE path = ?1", params![path], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(target_id) = target_id else {
        return Ok(GraphResponse {
            nodes: Vec::new(),
            links: Vec::new(),
            truncated: false,
            cochanges: Vec::new(),
        });
    };

    let neighbor_ids = load_neighbor_ids(conn, target_id)?;
    let available_neighbors = neighbor_ids.len();

    let mut wanted_ids: HashSet<i64> = neighbor_ids.iter().copied().collect();
    wanted_ids.insert(target_id);

    let raw_nodes = load_nodes_by_ids(conn, &wanted_ids, SUBGRAPH_NODE_CAP)?;
    let kept_ids: HashSet<i64> = raw_nodes.iter().map(|n| n.id).collect();

    let max_cc = load_max_cc(conn)?;
    let nodes = finalize_nodes(raw_nodes, &max_cc);
    let links = load_links(conn, &kept_ids)?;
    let cochanges = if with_cochanges {
        load_cochanges(conn, &kept_ids)?
    } else {
        Vec::new()
    };

    let cap = usize::try_from(SUBGRAPH_NODE_CAP).unwrap_or(usize::MAX);
    let truncated = available_neighbors + 1 > cap;

    Ok(GraphResponse {
        nodes,
        links,
        truncated,
        cochanges,
    })
}

fn load_nodes(conn: &Connection, limit: i64) -> anyhow::Result<Vec<RawNode>> {
    let has_clusters = file_clusters_exists(conn)?;
    let sql = if has_clusters {
        "SELECT f.id, f.path, f.language, f.pagerank, f.line_count, f.change_count, fc.cluster_id
         FROM files f
         LEFT JOIN file_clusters fc ON fc.file_id = f.id
         ORDER BY f.pagerank DESC, f.id ASC
         LIMIT ?1"
    } else {
        "SELECT f.id, f.path, f.language, f.pagerank, f.line_count, f.change_count, NULL
         FROM files f
         ORDER BY f.pagerank DESC, f.id ASC
         LIMIT ?1"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![limit], |r| {
        Ok(RawNode {
            id: r.get(0)?,
            path: r.get(1)?,
            language: r.get(2)?,
            pagerank: r.get(3)?,
            loc: r.get(4)?,
            change_count: r.get(5)?,
            cluster_id: r.get::<_, Option<i64>>(6)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn load_nodes_by_ids(
    conn: &Connection,
    wanted_ids: &HashSet<i64>,
    cap: i64,
) -> anyhow::Result<Vec<RawNode>> {
    if wanted_ids.is_empty() {
        return Ok(Vec::new());
    }
    let has_clusters = file_clusters_exists(conn)?;
    let ids: Vec<i64> = wanted_ids.iter().copied().collect();
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = if has_clusters {
        format!(
            "SELECT f.id, f.path, f.language, f.pagerank, f.line_count, f.change_count, fc.cluster_id
             FROM files f
             LEFT JOIN file_clusters fc ON fc.file_id = f.id
             WHERE f.id IN ({placeholders})
             ORDER BY f.pagerank DESC, f.id ASC
             LIMIT ?"
        )
    } else {
        format!(
            "SELECT f.id, f.path, f.language, f.pagerank, f.line_count, f.change_count, NULL
             FROM files f
             WHERE f.id IN ({placeholders})
             ORDER BY f.pagerank DESC, f.id ASC
             LIMIT ?"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let mut bind: Vec<i64> = ids;
    bind.push(cap);
    let rows = stmt.query_map(params_from_iter(bind.iter()), |r| {
        Ok(RawNode {
            id: r.get(0)?,
            path: r.get(1)?,
            language: r.get(2)?,
            pagerank: r.get(3)?,
            loc: r.get(4)?,
            change_count: r.get(5)?,
            cluster_id: r.get::<_, Option<i64>>(6)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Read every `(file_id, MAX(complexity))` pair from `symbols` in one
/// query so `hot_score` computation does not issue a query per node.
///
/// Returns an empty map and logs once when the `complexity` column is
/// missing on `symbols` (very old DBs predate that migration). Callers
/// treat missing rows as `max_cc == 0`.
fn load_max_cc(conn: &Connection) -> anyhow::Result<HashMap<i64, i64>> {
    if !column_exists(conn, "symbols", "complexity")? {
        tracing::warn!("graph.max_cc.complexity_column_missing");
        return Ok(HashMap::new());
    }
    let mut stmt = conn.prepare(
        "SELECT file_id, MAX(complexity) FROM symbols
         WHERE complexity IS NOT NULL
         GROUP BY file_id",
    )?;
    let rows = stmt.query_map([], |r| {
        let file_id: i64 = r.get(0)?;
        let max_cc: Option<i64> = r.get(1)?;
        Ok((file_id, max_cc.unwrap_or(0)))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (file_id, max_cc) = row?;
        out.insert(file_id, max_cc);
    }
    Ok(out)
}

/// Convert `RawNode`s into `GraphNode`s with `hot_score` min-max
/// normalized across the input slice. When all raw scores collapse to
/// the same value (or the slice has zero/one node), every `hot_score`
/// becomes `0.0` because a flat heatmap conveys nothing.
fn finalize_nodes(raw: Vec<RawNode>, max_cc: &HashMap<i64, i64>) -> Vec<GraphNode> {
    if raw.is_empty() {
        return Vec::new();
    }

    let raws: Vec<f64> = raw
        .iter()
        .map(|n| {
            let cc = max_cc.get(&n.id).copied().unwrap_or(0);
            #[expect(
                clippy::cast_precision_loss,
                reason = "cc and change_count are small integers, used inside ln_1p"
            )]
            let cc_term = (cc as f64).ln_1p();
            #[expect(
                clippy::cast_precision_loss,
                reason = "change_count is a small integer, used inside ln_1p"
            )]
            let cc_change = (n.change_count as f64).ln_1p();
            n.pagerank * (1.0 + cc_term) * (1.0 + cc_change)
        })
        .collect();

    let min = raws.iter().copied().fold(f64::INFINITY, f64::min);
    let max = raws.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = max - min;
    let normalize = |raw_value: f64| -> f64 {
        if span <= f64::EPSILON {
            0.0
        } else {
            ((raw_value - min) / span).clamp(0.0, 1.0)
        }
    };

    raw.into_iter()
        .zip(raws)
        .map(|(node, raw_value)| GraphNode {
            id: node.id,
            path: node.path,
            language: node.language,
            pagerank: node.pagerank,
            loc: node.loc,
            hot_score: normalize(raw_value),
            cluster_id: node.cluster_id,
        })
        .collect()
}

fn load_total_files(conn: &Connection) -> anyhow::Result<i64> {
    let count: Option<i64> = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .optional()?;
    Ok(count.unwrap_or(0))
}

fn load_links(conn: &Connection, kept_ids: &HashSet<i64>) -> anyhow::Result<Vec<GraphLink>> {
    if kept_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = kept_ids.iter().copied().collect();
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT from_file, to_file, kind
         FROM edges
         WHERE from_file IN ({placeholders})
           AND to_file   IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let combined: Vec<i64> = ids.iter().chain(ids.iter()).copied().collect();
    let rows = stmt.query_map(params_from_iter(combined.iter()), |r| {
        Ok(GraphLink {
            source: r.get(0)?,
            target: r.get(1)?,
            kind: r.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Load co-change pairs restricted to the kept node set.
///
/// Returns an empty vector when:
/// - `kept_ids` is empty (nothing to filter against),
/// - the `co_changes` table does not exist (older index DBs predate this
///   table; the dashboard must not 500 in that case).
///
/// Pairs are emitted with `source` = `min(file_a, file_b)` and `target` =
/// `max(file_a, file_b)` so the frontend can dedupe on the canonical
/// orientation regardless of how the row was stored.
///
/// Only pairs with `count >= 2` are returned. A single shared commit is
/// noisy at scale and tends to drown the structural graph; the threshold
/// matches the behavior of the `qartez_cochange` MCP tool.
fn load_cochanges(conn: &Connection, kept_ids: &HashSet<i64>) -> anyhow::Result<Vec<CoChange>> {
    if kept_ids.is_empty() {
        return Ok(Vec::new());
    }

    if !table_exists(conn, "co_changes")? {
        tracing::debug!("graph.cochanges.table_missing");
        return Ok(Vec::new());
    }

    let ids: Vec<i64> = kept_ids.iter().copied().collect();
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT file_a, file_b, count
         FROM co_changes
         WHERE file_a IN ({placeholders})
           AND file_b IN ({placeholders})
           AND count >= 2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let combined: Vec<i64> = ids.iter().chain(ids.iter()).copied().collect();
    let rows = stmt.query_map(params_from_iter(combined.iter()), |r| {
        let file_a: i64 = r.get(0)?;
        let file_b: i64 = r.get(1)?;
        let count: i64 = r.get(2)?;
        Ok(CoChange {
            source: file_a.min(file_b),
            target: file_a.max(file_b),
            count,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Collect the IDs of every file that imports, is imported by, or
/// historically co-changes (count >= 2) with the target. Excludes the
/// target itself - callers add it back explicitly.
fn load_neighbor_ids(conn: &Connection, target_id: i64) -> anyhow::Result<Vec<i64>> {
    let mut neighbors: HashSet<i64> = HashSet::new();

    let mut stmt = conn.prepare("SELECT to_file FROM edges WHERE from_file = ?1")?;
    let rows = stmt.query_map(params![target_id], |r| r.get::<_, i64>(0))?;
    for row in rows {
        neighbors.insert(row?);
    }

    let mut stmt = conn.prepare("SELECT from_file FROM edges WHERE to_file = ?1")?;
    let rows = stmt.query_map(params![target_id], |r| r.get::<_, i64>(0))?;
    for row in rows {
        neighbors.insert(row?);
    }

    if table_exists(conn, "co_changes")? {
        let mut stmt =
            conn.prepare("SELECT file_b FROM co_changes WHERE file_a = ?1 AND count >= 2")?;
        let rows = stmt.query_map(params![target_id], |r| r.get::<_, i64>(0))?;
        for row in rows {
            neighbors.insert(row?);
        }

        let mut stmt =
            conn.prepare("SELECT file_a FROM co_changes WHERE file_b = ?1 AND count >= 2")?;
        let rows = stmt.query_map(params![target_id], |r| r.get::<_, i64>(0))?;
        for row in rows {
            neighbors.insert(row?);
        }
    }

    neighbors.remove(&target_id);
    Ok(neighbors.into_iter().collect())
}

fn table_exists(conn: &Connection, table: &str) -> anyhow::Result<bool> {
    let exists: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
            params![table],
            |r| r.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

fn file_clusters_exists(conn: &Connection) -> anyhow::Result<bool> {
    table_exists(conn, "file_clusters")
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn default_db_path(root: &Path) -> PathBuf {
    root.join(".qartez").join("index.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Minimal subset of the `qartez-mcp` schema needed to drive the
    /// graph queries. Mirrors `qartez-public/src/storage/schema.rs` for
    /// the tables we read; intentionally omits FTS, indexes, and
    /// unrelated tables to keep the fixture readable.
    const TEST_SCHEMA: &str = "
        CREATE TABLE files (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            path         TEXT    NOT NULL UNIQUE,
            mtime_ns     INTEGER NOT NULL,
            size_bytes   INTEGER NOT NULL,
            language     TEXT    NOT NULL,
            line_count   INTEGER NOT NULL,
            pagerank     REAL    NOT NULL DEFAULT 0.0,
            indexed_at   INTEGER NOT NULL,
            change_count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE edges (
            id        INTEGER PRIMARY KEY AUTOINCREMENT,
            from_file INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            to_file   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            kind      TEXT    NOT NULL DEFAULT 'import',
            specifier TEXT,
            UNIQUE(from_file, to_file, kind)
        );
        CREATE TABLE co_changes (
            id     INTEGER PRIMARY KEY AUTOINCREMENT,
            file_a INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            file_b INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            count  INTEGER NOT NULL DEFAULT 1,
            UNIQUE(file_a, file_b)
        );
        CREATE TABLE symbols (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            name       TEXT    NOT NULL,
            kind       TEXT    NOT NULL,
            line_start INTEGER NOT NULL,
            line_end   INTEGER NOT NULL,
            complexity INTEGER
        );
        CREATE TABLE file_clusters (
            file_id     INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
            cluster_id  INTEGER NOT NULL,
            computed_at INTEGER NOT NULL DEFAULT 0
        );
    ";

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(TEST_SCHEMA).expect("create schema");
        conn
    }

    fn insert_file(conn: &Connection, id: i64, path: &str, pagerank: f64, line_count: i64) {
        insert_file_full(conn, id, path, pagerank, line_count, 0);
    }

    fn insert_file_full(
        conn: &Connection,
        id: i64,
        path: &str,
        pagerank: f64,
        line_count: i64,
        change_count: i64,
    ) {
        conn.execute(
            "INSERT INTO files
             (id, path, mtime_ns, size_bytes, language, line_count, pagerank, indexed_at, change_count)
             VALUES (?1, ?2, 0, 0, 'rust', ?3, ?4, 0, ?5)",
            params![id, path, line_count, pagerank, change_count],
        )
        .expect("insert file");
    }

    fn insert_edge(conn: &Connection, from_file: i64, to_file: i64) {
        conn.execute(
            "INSERT INTO edges (from_file, to_file, kind, specifier)
             VALUES (?1, ?2, 'import', NULL)",
            params![from_file, to_file],
        )
        .expect("insert edge");
    }

    fn insert_cochange(conn: &Connection, file_a: i64, file_b: i64, count: i64) {
        conn.execute(
            "INSERT INTO co_changes (file_a, file_b, count) VALUES (?1, ?2, ?3)",
            params![file_a, file_b, count],
        )
        .expect("insert cochange");
    }

    fn insert_symbol(conn: &Connection, file_id: i64, complexity: Option<i64>) {
        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, line_start, line_end, complexity)
             VALUES (?1, 'sym', 'function', 1, 2, ?2)",
            params![file_id, complexity],
        )
        .expect("insert symbol");
    }

    fn insert_cluster(conn: &Connection, file_id: i64, cluster_id: i64) {
        conn.execute(
            "INSERT INTO file_clusters (file_id, cluster_id, computed_at) VALUES (?1, ?2, 0)",
            params![file_id, cluster_id],
        )
        .expect("insert cluster");
    }

    #[test]
    fn empty_db_returns_empty_graph() {
        let conn = fresh_db();
        let graph = compute_graph(&conn, 10, false).expect("query ok");
        assert!(graph.nodes.is_empty());
        assert!(graph.links.is_empty());
        assert!(!graph.truncated);
        assert!(graph.cochanges.is_empty());
    }

    #[test]
    fn orders_nodes_by_pagerank_desc_and_keeps_link_integrity() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs", 0.5, 100);
        insert_file(&conn, 2, "src/main.rs", 0.3, 50);
        insert_file(&conn, 3, "src/util.rs", 0.1, 25);
        insert_edge(&conn, 1, 2);
        insert_edge(&conn, 2, 3);

        let graph = compute_graph(&conn, 10, false).expect("query ok");

        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.nodes[0].id, 1);
        assert_eq!(graph.nodes[0].pagerank, 0.5);
        assert_eq!(graph.nodes[0].loc, 100);
        assert_eq!(graph.nodes[1].id, 2);
        assert_eq!(graph.nodes[2].id, 3);

        assert_eq!(graph.links.len(), 2);
        let ids: HashSet<(i64, i64)> = graph.links.iter().map(|l| (l.source, l.target)).collect();
        assert!(ids.contains(&(1, 2)));
        assert!(ids.contains(&(2, 3)));
        for link in &graph.links {
            assert_eq!(link.kind, "import");
        }
        assert!(!graph.truncated);
        assert!(graph.cochanges.is_empty());
    }

    #[test]
    fn truncates_low_pagerank_nodes_and_drops_their_edges() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs", 0.5, 100);
        insert_file(&conn, 2, "src/main.rs", 0.3, 50);
        insert_file(&conn, 3, "src/util.rs", 0.1, 25);
        insert_edge(&conn, 1, 2);
        insert_edge(&conn, 2, 3);
        insert_edge(&conn, 3, 1);

        let graph = compute_graph(&conn, 2, false).expect("query ok");

        assert_eq!(graph.nodes.len(), 2);
        let kept: HashSet<i64> = graph.nodes.iter().map(|n| n.id).collect();
        assert!(kept.contains(&1));
        assert!(kept.contains(&2));
        assert!(!kept.contains(&3));

        assert_eq!(graph.links.len(), 1);
        assert_eq!(graph.links[0].source, 1);
        assert_eq!(graph.links[0].target, 2);

        assert!(graph.truncated);
        assert!(graph.cochanges.is_empty());
    }

    #[test]
    fn cochanges_are_empty_when_flag_off() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs", 0.5, 100);
        insert_file(&conn, 2, "src/main.rs", 0.3, 50);
        insert_cochange(&conn, 1, 2, 5);

        let graph = compute_graph(&conn, 10, false).expect("query ok");

        assert_eq!(graph.nodes.len(), 2);
        assert!(
            graph.cochanges.is_empty(),
            "cochanges must stay empty when the flag is off"
        );
    }

    #[test]
    fn cochanges_are_returned_with_canonical_orientation_and_min_count() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs", 0.5, 100);
        insert_file(&conn, 2, "src/main.rs", 0.3, 50);
        insert_file(&conn, 3, "src/util.rs", 0.1, 25);
        insert_cochange(&conn, 1, 2, 5);
        insert_cochange(&conn, 2, 3, 1);
        insert_cochange(&conn, 1, 3, 3);

        let graph = compute_graph(&conn, 10, true).expect("query ok");

        assert_eq!(
            graph.cochanges.len(),
            2,
            "count<2 pairs must be filtered out"
        );
        for pair in &graph.cochanges {
            assert!(
                pair.source < pair.target,
                "expected canonical (min, max) orientation, got ({}, {})",
                pair.source,
                pair.target
            );
        }
        let pairs: HashSet<(i64, i64, i64)> = graph
            .cochanges
            .iter()
            .map(|c| (c.source, c.target, c.count))
            .collect();
        assert!(pairs.contains(&(1, 2, 5)));
        assert!(pairs.contains(&(1, 3, 3)));
    }

    #[test]
    fn hot_score_is_normalized_across_response() {
        let conn = fresh_db();
        insert_file_full(&conn, 1, "src/lib.rs", 0.9, 100, 10);
        insert_file_full(&conn, 2, "src/main.rs", 0.5, 50, 4);
        insert_file_full(&conn, 3, "src/util.rs", 0.1, 25, 1);
        insert_symbol(&conn, 1, Some(20));
        insert_symbol(&conn, 2, Some(8));
        insert_symbol(&conn, 3, Some(2));

        let graph = compute_graph(&conn, 10, false).expect("query ok");
        assert_eq!(graph.nodes.len(), 3);

        for node in &graph.nodes {
            assert!(node.hot_score.is_finite(), "hot_score must be finite");
            assert!(
                (0.0..=1.0).contains(&node.hot_score),
                "hot_score {} must be within [0, 1]",
                node.hot_score
            );
        }

        let min = graph
            .nodes
            .iter()
            .map(|n| n.hot_score)
            .fold(f64::INFINITY, f64::min);
        let max = graph
            .nodes
            .iter()
            .map(|n| n.hot_score)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (min - 0.0).abs() < f64::EPSILON,
            "min hot_score must be 0.0, was {min}"
        );
        assert!(
            (max - 1.0).abs() < f64::EPSILON,
            "max hot_score must be 1.0, was {max}"
        );
    }

    #[test]
    fn cluster_id_is_null_for_unclustered_files() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs", 0.5, 100);
        insert_file(&conn, 2, "src/main.rs", 0.3, 50);
        insert_file(&conn, 3, "src/util.rs", 0.1, 25);
        insert_cluster(&conn, 1, 7);
        insert_cluster(&conn, 3, 7);

        let graph = compute_graph(&conn, 10, false).expect("query ok");
        assert_eq!(graph.nodes.len(), 3);

        let by_id: HashMap<i64, &GraphNode> = graph.nodes.iter().map(|n| (n.id, n)).collect();
        assert_eq!(by_id[&1].cluster_id, Some(7));
        assert_eq!(by_id[&2].cluster_id, None);
        assert_eq!(by_id[&3].cluster_id, Some(7));

        let unclustered = graph
            .nodes
            .iter()
            .filter(|n| n.cluster_id.is_none())
            .count();
        assert_eq!(unclustered, 1);
    }

    #[test]
    fn neighbors_of_returns_one_hop_subgraph() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/target.rs", 0.9, 100);
        for i in 0..5 {
            let id = 100 + i;
            insert_file(&conn, id, &format!("src/importer_{i}.rs"), 0.5, 50);
            insert_edge(&conn, id, 1);
        }
        for i in 0..5 {
            let id = 200 + i;
            insert_file(&conn, id, &format!("src/importee_{i}.rs"), 0.4, 40);
            insert_edge(&conn, 1, id);
        }
        for i in 0..3 {
            let id = 300 + i;
            insert_file(&conn, id, &format!("src/cochange_{i}.rs"), 0.3, 30);
            insert_cochange(&conn, 1, id, 5);
        }
        insert_file(&conn, 999, "src/unrelated.rs", 0.8, 80);

        let graph =
            compute_graph_neighbors(&conn, "src/target.rs", true).expect("neighbor query ok");

        let kept: HashSet<i64> = graph.nodes.iter().map(|n| n.id).collect();
        assert!(kept.contains(&1), "target must be in result");
        for i in 0..5 {
            assert!(kept.contains(&(100 + i)), "importer {i} must be in result");
            assert!(kept.contains(&(200 + i)), "importee {i} must be in result");
        }
        for i in 0..3 {
            assert!(
                kept.contains(&(300 + i)),
                "co-change partner {i} must be in result"
            );
        }
        assert!(
            !kept.contains(&999),
            "unrelated file must be absent from the response"
        );
        assert!(graph.nodes.len() <= 14);
        assert!(!graph.truncated, "14 neighbors fit under the cap");
    }

    #[test]
    fn neighbors_of_caps_at_subgraph_node_cap() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/hub.rs", 0.99, 100);
        for i in 0_i32..200 {
            let id = i64::from(100 + i);
            insert_file(&conn, id, &format!("src/imp_{i}.rs"), f64::from(i), 10);
            insert_edge(&conn, id, 1);
        }

        let graph = compute_graph_neighbors(&conn, "src/hub.rs", false).expect("neighbor query ok");

        let cap = usize::try_from(SUBGRAPH_NODE_CAP).expect("cap fits in usize");
        assert_eq!(
            graph.nodes.len(),
            cap,
            "must cap at SUBGRAPH_NODE_CAP nodes"
        );
        assert!(
            graph.truncated,
            "200 neighbors must mark response truncated"
        );
    }

    #[test]
    fn neighbors_of_unknown_path_returns_empty_graph() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs", 0.5, 100);

        let graph =
            compute_graph_neighbors(&conn, "src/does-not-exist.rs", true).expect("query ok");
        assert!(graph.nodes.is_empty());
        assert!(graph.links.is_empty());
        assert!(graph.cochanges.is_empty());
        assert!(!graph.truncated);
    }
}
