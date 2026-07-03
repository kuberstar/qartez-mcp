// Rust guideline compliant 2026-07-03

#![allow(unused_imports)]

use std::collections::{HashMap, HashSet, VecDeque};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};

use schemars::JsonSchema;
use serde::Deserialize;

use super::super::QartezServer;
use super::super::helpers::{self, *};

use crate::storage::read;

/// Default BFS depth ceiling when the caller does not pass `max_depth`.
const DEFAULT_MAX_DEPTH: usize = 25;
/// Hard ceiling on BFS depth. The symbol-reference graph can be very wide, so
/// even an explicit request is clamped to keep the walk bounded.
const MAX_MAX_DEPTH: usize = 50;
/// Default output token budget when the caller does not pass `token_budget`.
const DEFAULT_TOKEN_BUDGET: usize = 4000;

/// Tolerant deserializers so MCP clients that stringify numeric arguments
/// (e.g. `{"max_depth":"5"}`) deserialize the same as native JSON numbers.
/// Mirrors the `flexible` helpers in `server/params.rs`, duplicated here
/// because that module keeps them private.
mod flexible {
    use serde::{Deserialize, Deserializer, de::Error};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum U32OrStr {
        Num(u32),
        Str(String),
    }

    pub(super) fn u32_opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
        match Option::<U32OrStr>::deserialize(d)? {
            None => Ok(None),
            Some(U32OrStr::Num(n)) => Ok(Some(n)),
            Some(U32OrStr::Str(s)) => s.parse::<u32>().map(Some).map_err(D::Error::custom),
        }
    }
}

/// Parameters for the `qartez_path` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub(in crate::server) struct SoulPathParams {
    #[schemars(
        description = "Name of the origin symbol. Accepts aliases `source` and `symbol`. The path is searched forward along call/reference edges from here."
    )]
    #[serde(alias = "source", alias = "symbol")]
    pub from: String,
    #[schemars(
        description = "Name of the destination symbol. Accepts alias `target`. The shortest forward path that reaches this symbol is returned."
    )]
    #[serde(alias = "target")]
    pub to: String,
    #[schemars(
        description = "Edge kind to traverse: 'call' or 'type'. Omit to traverse both (the default, matching the full symbol-reference graph)."
    )]
    pub kind: Option<String>,
    #[schemars(
        description = "Disambiguate the origin by file when `from` resolves to multiple definitions. Relative path."
    )]
    #[serde(alias = "from_path")]
    pub from_file: Option<String>,
    #[schemars(
        description = "Disambiguate the destination by file when `to` resolves to multiple definitions. Relative path."
    )]
    #[serde(alias = "to_path")]
    pub to_file: Option<String>,
    #[schemars(
        description = "Maximum number of hops to search (default: 25, max: 50). Values above the cap are clamped."
    )]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub max_depth: Option<u32>,
    #[schemars(description = "Approximate token budget for output (default: 4000)")]
    #[serde(default, deserialize_with = "flexible::u32_opt")]
    pub token_budget: Option<u32>,
}

/// Metadata for one node on a resolved path, keyed by symbol id.
struct NodeInfo {
    name: String,
    kind: String,
    line: u32,
    path: String,
}

#[tool_router(router = qartez_path_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_path",
        description = "Find the shortest call/reference path between two symbols. Runs a forward BFS over the persisted symbol-reference graph from `from` to `to`, returning the ordered symbol/file chain plus a count of alternative shortest paths. Filter edges by `kind` ('call' or 'type'); both are traversed by default.",
        annotations(
            title = "Symbol Path",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_path(
        &self,
        Parameters(params): Parameters<SoulPathParams>,
    ) -> Result<String, String> {
        let from_name = params.from.trim();
        let to_name = params.to.trim();
        if from_name.is_empty() || to_name.is_empty() {
            return Err("both `from` and `to` are required and must be non-empty".to_string());
        }

        let kind_filter = params
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(k) = kind_filter {
            if !matches!(k, "call" | "type") {
                return Err(format!(
                    "unknown kind '{k}'. Valid edge kinds: 'call', 'type' (omit to traverse both)."
                ));
            }
        }

        let token_budget = params.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET as u32) as usize;
        let requested_depth = params
            .max_depth
            .map(|d| d as usize)
            .unwrap_or(DEFAULT_MAX_DEPTH);
        let max_depth = requested_depth.clamp(1, MAX_MAX_DEPTH);
        let depth_was_clamped = requested_depth > MAX_MAX_DEPTH;

        // Lock 1: resolve both endpoints and load the edge list. The whole
        // symbol-reference graph is pulled once so the BFS below runs
        // entirely in memory without re-taking the mutex per hop.
        let (from_syms, to_syms, edges) = {
            let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
            let from_syms = read::find_symbol_by_name(&conn, from_name)
                .map_err(|e| format!("DB error: {e}"))?;
            let to_syms =
                read::find_symbol_by_name(&conn, to_name).map_err(|e| format!("DB error: {e}"))?;
            let edges = if let Some(k) = kind_filter {
                let mut stmt = conn
                    .prepare("SELECT from_symbol_id, to_symbol_id FROM symbol_refs WHERE kind = ?1")
                    .map_err(|e| format!("DB error: {e}"))?;
                let rows = stmt
                    .query_map([k], |row| Ok((row.get(0)?, row.get(1)?)))
                    .map_err(|e| format!("DB error: {e}"))?;
                let mut v: Vec<(i64, i64)> = Vec::new();
                for row in rows {
                    v.push(row.map_err(|e| format!("DB error: {e}"))?);
                }
                v
            } else {
                // Default: the full symbol-reference graph (call + type edges).
                read::get_all_symbol_refs(&conn).map_err(|e| format!("DB error: {e}"))?
            };
            (from_syms, to_syms, edges)
        };

        if from_syms.is_empty() {
            return Err(format!("No symbol found with name '{from_name}'"));
        }
        if to_syms.is_empty() {
            return Err(format!("No symbol found with name '{to_name}'"));
        }

        // Optional endpoint disambiguation by file. Applied after the
        // existence check so a bogus file filter reports "excluded" rather
        // than masquerading as "no symbol".
        let from_syms = filter_by_file(from_syms, params.from_file.as_deref());
        let to_syms = filter_by_file(to_syms, params.to_file.as_deref());
        if from_syms.is_empty() {
            return Err(format!(
                "'{from_name}' has no definition in file_path={:?}. Drop `from_file` to search every candidate.",
                params.from_file,
            ));
        }
        if to_syms.is_empty() {
            return Err(format!(
                "'{to_name}' has no definition in file_path={:?}. Drop `to_file` to search every candidate.",
                params.to_file,
            ));
        }

        let sources: HashSet<i64> = from_syms.iter().map(|(s, _)| s.id).collect();
        let targets: HashSet<i64> = to_syms.iter().map(|(s, _)| s.id).collect();

        let kind_label = kind_filter.unwrap_or("call+type");
        let mut out = format!("qartez_path: {from_name} -> {to_name} (kind={kind_label})\n");
        if from_syms.len() > 1 || to_syms.len() > 1 {
            out.push_str(&format!(
                "  (note: from resolved to {} candidate(s), to resolved to {} candidate(s); searching across all of them - pass `from_file`/`to_file` to pin one.)\n",
                from_syms.len(),
                to_syms.len(),
            ));
        }

        // Same-symbol short-circuit: `from` and `to` name (partially) the
        // same definition. The path has length zero; no graph walk needed.
        if sources.iter().any(|s| targets.contains(s)) {
            out.push_str(
                "\nsame symbol: from and to resolve to the same definition; path length is 0.\n",
            );
            return Ok(out);
        }

        // Build the forward adjacency list once from the flat edge list.
        let mut graph: HashMap<i64, Vec<i64>> = HashMap::new();
        for (from_id, to_id) in &edges {
            graph.entry(*from_id).or_default().push(*to_id);
        }

        // Multi-source / multi-target BFS. `dist` is the shortest hop count
        // from the source set, `npaths` the number of distinct shortest
        // paths reaching a node, and `pred` one concrete predecessor used to
        // reconstruct a single path for display.
        let mut dist: HashMap<i64, usize> = HashMap::new();
        let mut npaths: HashMap<i64, u64> = HashMap::new();
        let mut pred: HashMap<i64, i64> = HashMap::new();
        let mut queue: VecDeque<i64> = VecDeque::new();
        for &s in &sources {
            dist.insert(s, 0);
            npaths.insert(s, 1);
            queue.push_back(s);
        }
        while let Some(u) = queue.pop_front() {
            let du = dist[&u];
            if du >= max_depth {
                continue;
            }
            let paths_u = npaths[&u];
            if let Some(neighbors) = graph.get(&u) {
                for &v in neighbors {
                    match dist.get(&v).copied() {
                        None => {
                            dist.insert(v, du + 1);
                            npaths.insert(v, paths_u);
                            pred.insert(v, u);
                            queue.push_back(v);
                        }
                        Some(dv) if dv == du + 1 => {
                            // Another shortest path of equal length reaches v.
                            *npaths.entry(v).or_insert(0) += paths_u;
                        }
                        _ => {}
                    }
                }
            }
        }

        // Pick the closest reachable target; break ties by symbol id for
        // deterministic output.
        let best_target = targets
            .iter()
            .filter_map(|t| dist.get(t).map(|d| (*d, *t)))
            .min_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)))
            .map(|(_, t)| t);

        let Some(target) = best_target else {
            out.push_str(&format!(
                "\nNo path found from '{from_name}' to '{to_name}' within max_depth={max_depth} (kind={kind_label}). The graph is directed: try swapping `from`/`to` or widening `max_depth`.\n",
            ));
            if depth_was_clamped {
                out.push_str(&format!(
                    "!warning: max_depth={requested_depth} was clamped to {MAX_MAX_DEPTH} (server-side hard cap).\n",
                ));
            }
            return Ok(out);
        };

        let best_dist = dist[&target];

        // Reconstruct one shortest path by walking predecessors back to a
        // source, then reversing into forward (from -> to) order.
        let mut chain: Vec<i64> = vec![target];
        let mut cursor = target;
        while let Some(&p) = pred.get(&cursor) {
            chain.push(p);
            cursor = p;
        }
        chain.reverse();

        // Count alternative shortest paths across every target that sits at
        // the same minimal distance (not just the one we render).
        let total_shortest: u64 = targets
            .iter()
            .filter(|t| dist.get(*t) == Some(&best_dist))
            .map(|t| npaths.get(t).copied().unwrap_or(1))
            .sum();
        let alternatives = total_shortest.saturating_sub(1);

        // Lock 2: resolve the node ids on the path to name/kind/line/file.
        let node_info = self.load_node_info(&chain)?;

        out.push_str(&format!("\nshortest path: {best_dist} hop(s)\n"));
        let mut rows: Vec<String> = Vec::with_capacity(chain.len());
        for (idx, id) in chain.iter().enumerate() {
            let row = match node_info.get(id) {
                Some(info) => format!(
                    "  {}. {} ({}) @ {}:L{}\n",
                    idx + 1,
                    info.name,
                    info.kind,
                    info.path,
                    info.line,
                ),
                None => format!("  {}. <symbol #{id}>\n", idx + 1),
            };
            rows.push(row);
        }
        helpers::budget_render(&mut out, &rows, token_budget);

        out.push_str(&format!("\nalternative shortest paths: {alternatives}\n"));
        if depth_was_clamped {
            out.push_str(&format!(
                "!warning: max_depth={requested_depth} was clamped to {MAX_MAX_DEPTH} (server-side hard cap).\n",
            ));
        }
        Ok(out)
    }
}

impl QartezServer {
    /// Resolve a set of symbol ids to their name/kind/line/file for rendering.
    fn load_node_info(&self, ids: &[i64]) -> Result<HashMap<i64, NodeInfo>, String> {
        let mut info: HashMap<i64, NodeInfo> = HashMap::new();
        if ids.is_empty() {
            return Ok(info);
        }
        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT s.id, s.name, s.kind, s.line_start, f.path
             FROM symbols s JOIN files f ON s.file_id = f.id
             WHERE s.id IN ({placeholders})",
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("DB error: {e}"))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    NodeInfo {
                        name: row.get::<_, String>(1)?,
                        kind: row.get::<_, String>(2)?,
                        line: row.get::<_, u32>(3)?,
                        path: row.get::<_, String>(4)?,
                    },
                ))
            })
            .map_err(|e| format!("DB error: {e}"))?;
        for row in rows {
            let (id, node) = row.map_err(|e| format!("DB error: {e}"))?;
            info.insert(id, node);
        }
        Ok(info)
    }
}

/// Keep only candidate definitions whose file matches `file`, or all of them
/// when no file filter is supplied.
fn filter_by_file(
    candidates: Vec<(
        crate::storage::models::SymbolRow,
        crate::storage::models::FileRow,
    )>,
    file: Option<&str>,
) -> Vec<(
    crate::storage::models::SymbolRow,
    crate::storage::models::FileRow,
)> {
    match file.map(str::trim).filter(|s| !s.is_empty()) {
        Some(f) => candidates
            .into_iter()
            .filter(|(_, fr)| fr.path == f)
            .collect(),
        None => candidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn insert_file(conn: &Connection, path: &str) -> i64 {
        conn.execute(
            "INSERT INTO files (path, mtime_ns, size_bytes, language, line_count, indexed_at)
             VALUES (?1, 0, 0, 'rust', 100, 0)",
            [path],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_symbol(conn: &Connection, file_id: i64, name: &str, line: u32) -> i64 {
        conn.execute(
            "INSERT INTO symbols (file_id, name, kind, line_start, line_end)
             VALUES (?1, ?2, 'function', ?3, ?3)",
            rusqlite::params![file_id, name, line],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_ref(conn: &Connection, from_id: i64, to_id: i64, kind: &str) {
        conn.execute(
            "INSERT INTO symbol_refs (from_symbol_id, to_symbol_id, kind) VALUES (?1, ?2, ?3)",
            rusqlite::params![from_id, to_id, kind],
        )
        .unwrap();
    }

    /// Build a server whose graph is `foo -> bar -> baz` (calls), plus an
    /// isolated `qux` with no edges.
    fn server_with_chain() -> QartezServer {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::schema::create_schema(&conn).unwrap();
        let file_id = insert_file(&conn, "a.rs");
        let foo = insert_symbol(&conn, file_id, "foo", 1);
        let bar = insert_symbol(&conn, file_id, "bar", 10);
        let baz = insert_symbol(&conn, file_id, "baz", 20);
        let _qux = insert_symbol(&conn, file_id, "qux", 30);
        insert_ref(&conn, foo, bar, "call");
        insert_ref(&conn, bar, baz, "call");
        QartezServer::new(conn, std::path::PathBuf::from("/tmp/test"), 0)
    }

    fn params(from: &str, to: &str) -> SoulPathParams {
        SoulPathParams {
            from: from.to_string(),
            to: to.to_string(),
            kind: None,
            from_file: None,
            to_file: None,
            max_depth: None,
            token_budget: None,
        }
    }

    #[test]
    fn finds_shortest_path_between_connected_symbols() {
        let server = server_with_chain();
        let out = server
            .qartez_path(Parameters(params("foo", "baz")))
            .unwrap();
        assert!(out.contains("shortest path: 2 hop(s)"), "output: {out}");
        assert!(out.contains("1. foo"), "output: {out}");
        assert!(out.contains("2. bar"), "output: {out}");
        assert!(out.contains("3. baz"), "output: {out}");
        assert!(
            out.contains("alternative shortest paths: 0"),
            "output: {out}"
        );
    }

    #[test]
    fn reports_no_path_when_unreachable() {
        let server = server_with_chain();
        // The graph is directed: baz has no forward edge back to foo.
        let out = server
            .qartez_path(Parameters(params("baz", "foo")))
            .unwrap();
        assert!(out.contains("No path found"), "output: {out}");

        // Fully disconnected target.
        let out2 = server
            .qartez_path(Parameters(params("foo", "qux")))
            .unwrap();
        assert!(out2.contains("No path found"), "output: {out2}");
    }

    #[test]
    fn same_symbol_is_zero_length() {
        let server = server_with_chain();
        let out = server
            .qartez_path(Parameters(params("foo", "foo")))
            .unwrap();
        assert!(out.contains("path length is 0"), "output: {out}");
    }

    #[test]
    fn missing_symbol_errors() {
        let server = server_with_chain();
        let err = server
            .qartez_path(Parameters(params("nope", "baz")))
            .unwrap_err();
        assert!(
            err.contains("No symbol found with name 'nope'"),
            "err: {err}"
        );
    }

    #[test]
    fn kind_filter_excludes_other_edges() {
        let server = server_with_chain();
        // Only 'type' edges exist for this pair? No - all edges are 'call',
        // so filtering to 'type' should find no path.
        let mut p = params("foo", "baz");
        p.kind = Some("type".to_string());
        let out = server.qartez_path(Parameters(p)).unwrap();
        assert!(out.contains("No path found"), "output: {out}");
    }
}
