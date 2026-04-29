//! `GET /api/symbol-graph` - top symbols by PageRank with their `symbol_refs`
//! call edges, formatted for the d3-force symbol-level overlay.
//!
//! Reads `<project_root>/.qartez/index.db` and returns the top N symbols
//! ordered by `symbols.pagerank DESC` (ties broken by `symbols.id ASC`),
//! along with the subset of `symbol_refs` rows where both endpoints are
//! still in the kept set. Field names on links use d3-force conventions
//! (`source` / `target`) so the browser can pass the response straight
//! into a simulation without remapping.
//!
//! `color_by` is an advisory presentation hint (`file` or `kind`); the
//! backend returns the same payload either way and the UI picks the
//! coloring strategy. Unknown values silently fall back to `file` rather
//! than 400 - this is a hint, not a contract.
//!
//! When `?neighbors_of=N` is set, the response is restricted to the
//! 1-hop call neighborhood of the symbol with id `N` (its callers and
//! callees) plus the target itself, capped at `SUBGRAPH_NODE_CAP`
//! nodes. An unknown id yields an empty graph rather than 404 to keep
//! the UI placeholder logic simple.
//!
//! When the index DB is missing the handler returns an empty graph rather
//! than 404, matching `/api/graph` and `/api/project`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Default node count when the caller omits `?limit=`. Smaller than the
/// file-graph default because symbol counts are roughly an order of
/// magnitude larger and the simulation must stay interactive.
const DEFAULT_LIMIT: i64 = 200;

/// Hard ceiling on `?limit=`. Above this the d3-force simulation gets
/// sluggish in the browser and the SQL `IN (...)` placeholder list grows
/// large enough to start straining SQLite's expression depth.
const MAX_LIMIT: i64 = 500;

/// Hard ceiling on the number of nodes returned by a `?neighbors_of=`
/// subgraph query. Mirrors the file-graph cap and keeps the d3-force
/// simulation responsive when a hub symbol has hundreds of callers.
const SUBGRAPH_NODE_CAP: i64 = 100;

/// Query string for `GET /api/symbol-graph?limit=N&color_by=file|kind&neighbors_of=ID`.
#[derive(Debug, Deserialize)]
pub struct SymbolGraphQuery {
    /// Maximum number of nodes to return. Clamped to `[1, MAX_LIMIT]`;
    /// invalid values fall back to `DEFAULT_LIMIT`.
    pub limit: Option<i64>,
    /// Presentation hint for the UI. Accepted values are `file` and
    /// `kind`; anything else silently falls back to `file`. The backend
    /// returns the same payload either way.
    pub color_by: Option<String>,
    /// When set, restrict the response to the 1-hop call neighborhood of
    /// the symbol with this id (its callers + callees + the target),
    /// capped at `SUBGRAPH_NODE_CAP` nodes. Unknown ids yield an empty
    /// graph rather than 404.
    pub neighbors_of: Option<i64>,
}

/// One node in the symbol graph. Field names are emitted verbatim and
/// the UI uses `pagerank` for sizing and either `file_id` or `kind` for
/// coloring (per the `color_by` query hint).
#[derive(Debug, Serialize)]
pub struct SymbolGraphNode {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub file_id: i64,
    pub file_path: String,
    pub pagerank: f64,
    /// Cyclomatic complexity from `symbols.complexity`. `None` when the
    /// row has no value (the indexer leaves it null for some kinds).
    pub complexity: Option<i64>,
}

/// One directed call edge. `source` and `target` are symbol IDs matching
/// `SymbolGraphNode::id`; the names follow d3-force's expected schema.
#[derive(Debug, Serialize)]
pub struct SymbolGraphLink {
    pub source: i64,
    pub target: i64,
    pub kind: String,
}

/// Response body for `GET /api/symbol-graph`.
#[derive(Debug, Serialize)]
pub struct SymbolGraphResponse {
    pub nodes: Vec<SymbolGraphNode>,
    pub links: Vec<SymbolGraphLink>,
    pub truncated: bool,
}

/// JSON error envelope returned on 500.
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: &'static str,
}

/// Raw row read from the `symbols` join before any post-processing.
struct RawNode {
    id: i64,
    name: String,
    kind: String,
    file_id: i64,
    file_path: String,
    pagerank: f64,
    complexity: Option<i64>,
}

/// Handle `GET /api/symbol-graph?limit=N&color_by=file|kind`.
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
    Query(query): Query<SymbolGraphQuery>,
) -> Result<Json<SymbolGraphResponse>, (StatusCode, Json<ApiError>)> {
    let limit = clamp_limit(query.limit);
    let _color_by = normalize_color_by(query.color_by.as_deref());
    let neighbors_of = query.neighbors_of;
    let root = state.project_root().to_path_buf();

    let result = tokio::task::spawn_blocking(move || match neighbors_of {
        Some(target_id) => compute_symbol_graph_neighbors_at_root(&root, target_id),
        None => compute_symbol_graph_at_root(&root, limit),
    })
    .await
    .map_err(|error| {
        tracing::error!(?error, "symbol_graph.join.failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "join error",
            }),
        )
    })?;

    match result {
        Ok(Some(graph)) => Ok(Json(graph)),
        Ok(None) => Ok(Json(SymbolGraphResponse {
            nodes: Vec::new(),
            links: Vec::new(),
            truncated: false,
        })),
        Err(error) => {
            tracing::error!(?error, "symbol_graph.query.failed");
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

fn normalize_color_by(requested: Option<&str>) -> &'static str {
    match requested {
        Some("kind") => "kind",
        _ => "file",
    }
}

fn compute_symbol_graph_at_root(
    root: &Path,
    limit: i64,
) -> anyhow::Result<Option<SymbolGraphResponse>> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(&db_path)?;
    Ok(Some(compute_symbol_graph(&conn, limit)?))
}

fn compute_symbol_graph_neighbors_at_root(
    root: &Path,
    target_id: i64,
) -> anyhow::Result<Option<SymbolGraphResponse>> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(&db_path)?;
    Ok(Some(compute_symbol_graph_neighbors(&conn, target_id)?))
}

/// Compute the symbol-graph payload from an open SQLite connection.
///
/// Factored out so unit tests can drive it against an in-memory DB
/// without spinning up the HTTP layer.
pub(crate) fn compute_symbol_graph(
    conn: &Connection,
    limit: i64,
) -> anyhow::Result<SymbolGraphResponse> {
    let raw_nodes = load_nodes(conn, limit)?;
    let total_symbols = load_total_symbols(conn)?;
    let kept_ids: HashSet<i64> = raw_nodes.iter().map(|n| n.id).collect();
    let nodes = finalize_nodes(raw_nodes);
    let links = load_links(conn, &kept_ids)?;
    let truncated = i64::try_from(nodes.len()).unwrap_or(i64::MAX) < total_symbols;

    Ok(SymbolGraphResponse {
        nodes,
        links,
        truncated,
    })
}

/// Compute the 1-hop call neighborhood of `target_id`. Returns an empty
/// response when the symbol is unknown so the UI can surface "no such
/// symbol" without the handler returning 404.
pub(crate) fn compute_symbol_graph_neighbors(
    conn: &Connection,
    target_id: i64,
) -> anyhow::Result<SymbolGraphResponse> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT id FROM symbols WHERE id = ?1",
            params![target_id],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Ok(SymbolGraphResponse {
            nodes: Vec::new(),
            links: Vec::new(),
            truncated: false,
        });
    }

    let neighbor_ids = load_symbol_neighbor_ids(conn, target_id)?;
    let available_neighbors = neighbor_ids.len();

    let mut wanted_ids: HashSet<i64> = neighbor_ids.into_iter().collect();
    wanted_ids.insert(target_id);

    let raw_nodes = load_symbol_nodes_by_ids(conn, &wanted_ids, SUBGRAPH_NODE_CAP)?;
    let kept_ids: HashSet<i64> = raw_nodes.iter().map(|n| n.id).collect();
    let nodes = finalize_nodes(raw_nodes);
    let links = load_links(conn, &kept_ids)?;

    let cap = usize::try_from(SUBGRAPH_NODE_CAP).unwrap_or(usize::MAX);
    let truncated = available_neighbors + 1 > cap;

    Ok(SymbolGraphResponse {
        nodes,
        links,
        truncated,
    })
}

/// Collect the union of caller and callee symbol ids for `target_id`.
/// The target itself is removed so the caller can decide whether to
/// include it in the kept set.
fn load_symbol_neighbor_ids(conn: &Connection, target_id: i64) -> anyhow::Result<Vec<i64>> {
    let mut neighbors: HashSet<i64> = HashSet::new();

    let mut stmt = conn.prepare(
        "SELECT from_symbol_id FROM symbol_refs
         WHERE to_symbol_id = ?1 AND kind = 'call'",
    )?;
    let rows = stmt.query_map(params![target_id], |r| r.get::<_, i64>(0))?;
    for row in rows {
        neighbors.insert(row?);
    }

    let mut stmt = conn.prepare(
        "SELECT to_symbol_id FROM symbol_refs
         WHERE from_symbol_id = ?1 AND kind = 'call'",
    )?;
    let rows = stmt.query_map(params![target_id], |r| r.get::<_, i64>(0))?;
    for row in rows {
        neighbors.insert(row?);
    }

    neighbors.remove(&target_id);
    Ok(neighbors.into_iter().collect())
}

fn load_symbol_nodes_by_ids(
    conn: &Connection,
    wanted_ids: &HashSet<i64>,
    cap: i64,
) -> anyhow::Result<Vec<RawNode>> {
    if wanted_ids.is_empty() {
        return Ok(Vec::new());
    }
    let has_complexity = column_exists(conn, "symbols", "complexity")?;
    let ids: Vec<i64> = wanted_ids.iter().copied().collect();
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = if has_complexity {
        format!(
            "SELECT s.id, s.name, s.kind, s.file_id, f.path, s.pagerank, s.complexity
             FROM symbols s
             JOIN files f ON f.id = s.file_id
             WHERE s.id IN ({placeholders})
             ORDER BY s.pagerank DESC, s.id ASC
             LIMIT ?"
        )
    } else {
        format!(
            "SELECT s.id, s.name, s.kind, s.file_id, f.path, s.pagerank, NULL
             FROM symbols s
             JOIN files f ON f.id = s.file_id
             WHERE s.id IN ({placeholders})
             ORDER BY s.pagerank DESC, s.id ASC
             LIMIT ?"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let mut bind: Vec<i64> = ids;
    bind.push(cap);
    let rows = stmt.query_map(params_from_iter(bind.iter()), |r| {
        Ok(RawNode {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            file_id: r.get(3)?,
            file_path: r.get(4)?,
            pagerank: r.get(5)?,
            complexity: r.get::<_, Option<i64>>(6)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn load_nodes(conn: &Connection, limit: i64) -> anyhow::Result<Vec<RawNode>> {
    let has_complexity = column_exists(conn, "symbols", "complexity")?;
    let sql = if has_complexity {
        "SELECT s.id, s.name, s.kind, s.file_id, f.path, s.pagerank, s.complexity
         FROM symbols s
         JOIN files f ON f.id = s.file_id
         ORDER BY s.pagerank DESC, s.id ASC
         LIMIT ?1"
    } else {
        "SELECT s.id, s.name, s.kind, s.file_id, f.path, s.pagerank, NULL
         FROM symbols s
         JOIN files f ON f.id = s.file_id
         ORDER BY s.pagerank DESC, s.id ASC
         LIMIT ?1"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![limit], |r| {
        Ok(RawNode {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            file_id: r.get(3)?,
            file_path: r.get(4)?,
            pagerank: r.get(5)?,
            complexity: r.get::<_, Option<i64>>(6)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn load_total_symbols(conn: &Connection) -> anyhow::Result<i64> {
    let count: Option<i64> = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
        .optional()?;
    Ok(count.unwrap_or(0))
}

fn load_links(conn: &Connection, kept_ids: &HashSet<i64>) -> anyhow::Result<Vec<SymbolGraphLink>> {
    if kept_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = kept_ids.iter().copied().collect();
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT from_symbol_id, to_symbol_id, kind
         FROM symbol_refs
         WHERE from_symbol_id IN ({placeholders})
           AND to_symbol_id   IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let combined: Vec<i64> = ids.iter().chain(ids.iter()).copied().collect();
    let rows = stmt.query_map(params_from_iter(combined.iter()), |r| {
        Ok(SymbolGraphLink {
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

fn finalize_nodes(raw: Vec<RawNode>) -> Vec<SymbolGraphNode> {
    raw.into_iter()
        .map(|node| SymbolGraphNode {
            id: node.id,
            name: node.name,
            kind: node.kind,
            file_id: node.file_id,
            file_path: node.file_path,
            pagerank: node.pagerank,
            complexity: node.complexity,
        })
        .collect()
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
    /// symbol-graph queries. Mirrors `qartez-public/src/storage/schema.rs`
    /// for the tables we read; intentionally omits FTS, indexes, and
    /// unrelated tables to keep the fixture readable.
    const TEST_SCHEMA: &str = "
        CREATE TABLE files (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            path       TEXT    NOT NULL UNIQUE,
            mtime_ns   INTEGER NOT NULL,
            size_bytes INTEGER NOT NULL,
            language   TEXT    NOT NULL,
            line_count INTEGER NOT NULL,
            pagerank   REAL    NOT NULL DEFAULT 0.0,
            indexed_at INTEGER NOT NULL
        );
        CREATE TABLE symbols (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            name        TEXT    NOT NULL,
            kind        TEXT    NOT NULL,
            line_start  INTEGER NOT NULL,
            line_end    INTEGER NOT NULL,
            signature   TEXT,
            is_exported INTEGER NOT NULL DEFAULT 0,
            pagerank    REAL    NOT NULL DEFAULT 0.0,
            complexity  INTEGER
        );
        CREATE TABLE symbol_refs (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            from_symbol_id  INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
            to_symbol_id    INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
            kind            TEXT    NOT NULL DEFAULT 'call',
            UNIQUE(from_symbol_id, to_symbol_id, kind)
        );
    ";

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(TEST_SCHEMA).expect("create schema");
        conn
    }

    fn insert_file(conn: &Connection, id: i64, path: &str) {
        conn.execute(
            "INSERT INTO files (id, path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES (?1, ?2, 0, 0, 'rust', 100, 0)",
            params![id, path],
        )
        .expect("insert file");
    }

    fn insert_symbol(
        conn: &Connection,
        id: i64,
        file_id: i64,
        name: &str,
        pagerank: f64,
        complexity: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO symbols
             (id, file_id, name, kind, line_start, line_end, pagerank, complexity)
             VALUES (?1, ?2, ?3, 'function', 1, 2, ?4, ?5)",
            params![id, file_id, name, pagerank, complexity],
        )
        .expect("insert symbol");
    }

    fn insert_ref(conn: &Connection, from: i64, to: i64) {
        conn.execute(
            "INSERT INTO symbol_refs (from_symbol_id, to_symbol_id, kind)
             VALUES (?1, ?2, 'call')",
            params![from, to],
        )
        .expect("insert symbol_ref");
    }

    #[test]
    fn empty_db_returns_empty_symbol_graph() {
        let conn = fresh_db();
        let graph = compute_symbol_graph(&conn, 10).expect("query ok");
        assert!(graph.nodes.is_empty());
        assert!(graph.links.is_empty());
        assert!(!graph.truncated);
    }

    #[test]
    fn orders_symbols_by_pagerank_desc() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs");
        insert_symbol(&conn, 1, 1, "low", 0.1, Some(2));
        insert_symbol(&conn, 2, 1, "high", 0.9, Some(8));
        insert_symbol(&conn, 3, 1, "mid", 0.5, Some(4));

        let graph = compute_symbol_graph(&conn, 10).expect("query ok");

        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.nodes[0].name, "high");
        assert_eq!(graph.nodes[0].pagerank, 0.9);
        assert_eq!(graph.nodes[1].name, "mid");
        assert_eq!(graph.nodes[2].name, "low");
        assert!(!graph.truncated);
    }

    #[test]
    fn truncates_to_limit_and_drops_links_with_dropped_endpoints() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs");
        insert_symbol(&conn, 1, 1, "alpha", 0.9, None);
        insert_symbol(&conn, 2, 1, "beta", 0.5, None);
        insert_symbol(&conn, 3, 1, "gamma", 0.1, None);
        insert_ref(&conn, 1, 2);
        insert_ref(&conn, 2, 3);
        insert_ref(&conn, 3, 1);

        let graph = compute_symbol_graph(&conn, 2).expect("query ok");

        assert_eq!(graph.nodes.len(), 2);
        let kept: HashSet<i64> = graph.nodes.iter().map(|n| n.id).collect();
        assert!(kept.contains(&1));
        assert!(kept.contains(&2));
        assert!(!kept.contains(&3));

        assert_eq!(graph.links.len(), 1);
        assert_eq!(graph.links[0].source, 1);
        assert_eq!(graph.links[0].target, 2);
        assert!(graph.truncated);
    }

    #[test]
    fn links_filter_to_kept_set() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs");
        insert_symbol(&conn, 1, 1, "a", 0.9, None);
        insert_symbol(&conn, 2, 1, "b", 0.8, None);
        insert_symbol(&conn, 3, 1, "c", 0.7, None);
        insert_ref(&conn, 1, 2);
        insert_ref(&conn, 2, 3);
        insert_ref(&conn, 1, 3);

        let graph = compute_symbol_graph(&conn, 10).expect("query ok");

        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.links.len(), 3);
        for link in &graph.links {
            assert_eq!(link.kind, "call");
        }
    }

    #[test]
    fn nullable_complexity_serializes_as_none() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs");
        insert_symbol(&conn, 1, 1, "with_cc", 0.9, Some(7));
        insert_symbol(&conn, 2, 1, "no_cc", 0.5, None);

        let graph = compute_symbol_graph(&conn, 10).expect("query ok");

        assert_eq!(graph.nodes.len(), 2);
        let with_cc = graph
            .nodes
            .iter()
            .find(|n| n.name == "with_cc")
            .expect("with_cc present");
        assert_eq!(with_cc.complexity, Some(7));
        let no_cc = graph
            .nodes
            .iter()
            .find(|n| n.name == "no_cc")
            .expect("no_cc present");
        assert_eq!(no_cc.complexity, None);

        let json = serde_json::to_string(&graph).expect("serialize ok");
        assert!(json.contains("\"complexity\":null"));
        assert!(json.contains("\"complexity\":7"));
    }

    #[test]
    fn neighbors_of_returns_one_hop_subgraph() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs");
        insert_symbol(&conn, 1, 1, "a", 0.9, None);
        insert_symbol(&conn, 2, 1, "b", 0.8, None);
        insert_symbol(&conn, 3, 1, "c", 0.7, None);
        insert_symbol(&conn, 4, 1, "d", 0.6, None);
        insert_ref(&conn, 1, 2);
        insert_ref(&conn, 2, 3);
        insert_ref(&conn, 3, 4);

        let graph = compute_symbol_graph_neighbors(&conn, 2).expect("neighbor query ok");

        let kept: HashSet<i64> = graph.nodes.iter().map(|n| n.id).collect();
        assert!(kept.contains(&1), "caller a must be present");
        assert!(kept.contains(&2), "target b must be present");
        assert!(kept.contains(&3), "callee c must be present");
        assert!(!kept.contains(&4), "two-hop d must be absent");
        assert_eq!(graph.nodes.len(), 3);
        assert!(!graph.truncated);
    }

    #[test]
    fn neighbors_of_caps_at_subgraph_node_cap() {
        let conn = fresh_db();
        insert_file(&conn, 1, "src/lib.rs");
        insert_symbol(&conn, 1, 1, "target", 0.99, None);
        for i in 0_i32..200 {
            let id = i64::from(100 + i);
            insert_symbol(&conn, id, 1, &format!("callee_{i}"), f64::from(i), None);
            insert_ref(&conn, 1, id);
        }

        let graph = compute_symbol_graph_neighbors(&conn, 1).expect("neighbor query ok");

        let cap = usize::try_from(SUBGRAPH_NODE_CAP).expect("cap fits in usize");
        assert!(
            graph.nodes.len() <= cap,
            "must cap at SUBGRAPH_NODE_CAP nodes, got {}",
            graph.nodes.len()
        );
        assert!(graph.truncated, "200 callees must mark response truncated");
    }
}

// Rust guideline compliant 2026-04-26
