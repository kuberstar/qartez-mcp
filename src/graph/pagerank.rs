use std::collections::HashMap;

use rusqlite::Connection;

use crate::error::Result;
use crate::storage::read::{get_all_edges, get_all_files, get_all_symbol_refs, get_all_symbols};
use crate::storage::verify_foreign_keys;
use crate::storage::write::{update_pagerank, update_symbol_pagerank};

pub struct PageRankConfig {
    pub damping: f64,
    pub iterations: u32,
    pub epsilon: f64,
}

impl Default for PageRankConfig {
    fn default() -> Self {
        Self {
            damping: 0.85,
            iterations: 20,
            epsilon: 0.00001,
        }
    }
}

/// Compute PageRank and write results to the `files.pagerank` column.
///
/// Uses previously stored ranks as a warm-start so incremental re-indexes
/// converge in 1-3 iterations instead of ~15-20 from a cold start.
/// Only writes back ranks that actually changed, skipping no-op UPDATEs.
pub fn compute_pagerank(conn: &Connection, config: &PageRankConfig) -> Result<()> {
    let edges = get_all_edges(conn)?;
    let files = get_all_files(conn)?;

    let prev_ranks: HashMap<i64, f64> = files.iter().map(|f| (f.id, f.pagerank)).collect();

    let mut nodes: Vec<i64> = files.iter().map(|f| f.id).collect();
    for &(src, dst) in &edges {
        nodes.push(src);
        nodes.push(dst);
    }
    nodes.sort_unstable();
    nodes.dedup();

    let ranks = pagerank_inner(&nodes, &edges, config, &prev_ranks);

    // unchecked_transaction skips per-row FK enforcement for bulk-write
    // performance. PRAGMA foreign_key_check after commit catches violations.
    let tx = conn.unchecked_transaction()?;
    for (&node_id, &rank) in &ranks {
        let changed = prev_ranks
            .get(&node_id)
            .is_none_or(|&prev| (rank - prev).abs() >= config.epsilon);
        if changed {
            update_pagerank(&tx, node_id, rank)?;
        }
    }
    tx.commit()?;
    verify_foreign_keys(conn)?;

    Ok(())
}

/// Compute PageRank over the symbol-level graph and write results to the
/// `symbols.pagerank` column. Mirrors `compute_pagerank` but operates on
/// the `symbol_refs` edge list instead of file-level imports. Every symbol
/// participates as a node even if it has no incoming or outgoing edges,
/// giving unreferenced symbols a base rank of ~1/N so downstream consumers
/// can sort the whole symbol table without holes.
///
/// Uses warm-start and write-skip optimizations (see `compute_pagerank`).
pub fn compute_symbol_pagerank(conn: &Connection, config: &PageRankConfig) -> Result<()> {
    let edges = get_all_symbol_refs(conn)?;
    let symbols = get_all_symbols(conn)?;

    let prev_ranks: HashMap<i64, f64> = symbols.iter().map(|s| (s.id, s.pagerank)).collect();

    let mut nodes: Vec<i64> = symbols.iter().map(|s| s.id).collect();
    for &(src, dst) in &edges {
        nodes.push(src);
        nodes.push(dst);
    }
    nodes.sort_unstable();
    nodes.dedup();

    let ranks = pagerank_inner(&nodes, &edges, config, &prev_ranks);

    let tx = conn.unchecked_transaction()?;
    for (&node_id, &rank) in &ranks {
        let changed = prev_ranks
            .get(&node_id)
            .is_none_or(|&prev| (rank - prev).abs() >= config.epsilon);
        if changed {
            update_symbol_pagerank(&tx, node_id, rank)?;
        }
    }
    tx.commit()?;
    verify_foreign_keys(conn)?;

    Ok(())
}

/// Compute PageRank on an abstract graph without DB side effects.
pub fn pagerank_raw(
    nodes: &[i64],
    edges: &[(i64, i64)],
    config: &PageRankConfig,
) -> HashMap<i64, f64> {
    pagerank_inner(nodes, edges, config, &HashMap::new())
}

/// Core PageRank iteration loop. When `prev_ranks` is non-empty, uses those
/// values as the starting point (warm-start) so that small graph changes
/// converge in 1-3 iterations via the epsilon threshold. When empty, falls
/// back to uniform 1/N initialization (cold start).
fn pagerank_inner(
    nodes: &[i64],
    edges: &[(i64, i64)],
    config: &PageRankConfig,
    prev_ranks: &HashMap<i64, f64>,
) -> HashMap<i64, f64> {
    let n = nodes.len();
    if n == 0 {
        return HashMap::new();
    }

    let node_to_idx: HashMap<i64, usize> =
        nodes.iter().enumerate().map(|(i, &id)| (id, i)).collect();

    let mut outgoing: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut incoming: Vec<Vec<usize>> = vec![Vec::new(); n];

    for &(src, dst) in edges {
        if src == dst {
            continue;
        }
        if let (Some(&si), Some(&di)) = (node_to_idx.get(&src), node_to_idx.get(&dst)) {
            outgoing[si].push(di);
            incoming[di].push(si);
        }
    }
    for list in &mut outgoing {
        list.sort_unstable();
        list.dedup();
    }
    for list in &mut incoming {
        list.sort_unstable();
        list.dedup();
    }

    let uniform = 1.0 / n as f64;
    // A valid PageRank distribution sums to ~1.0. When all stored ranks are
    // 0.0 (first computation, before any ranks exist in the DB) the sum is 0
    // and warm-starting would break convergence. Fall back to uniform in that
    // case; the threshold of 0.5 catches partial-zero states too.
    let prev_sum: f64 = if prev_ranks.is_empty() {
        0.0
    } else {
        nodes.iter().filter_map(|id| prev_ranks.get(id)).sum()
    };
    let have_warm_start = prev_sum > 0.5;
    let mut ranks: Vec<f64> = if have_warm_start {
        nodes
            .iter()
            .map(|id| {
                prev_ranks
                    .get(id)
                    .copied()
                    .filter(|&r| r > 0.0)
                    .unwrap_or(uniform)
            })
            .collect()
    } else {
        vec![uniform; n]
    };
    let mut new_ranks = vec![0.0; n];
    let base = (1.0 - config.damping) / n as f64;

    for _ in 0..config.iterations {
        let leaked: f64 = (0..n)
            .filter(|&i| outgoing[i].is_empty())
            .map(|i| ranks[i])
            .sum();

        let leaked_share = config.damping * leaked / n as f64;

        for i in 0..n {
            let inbound_sum: f64 = incoming[i]
                .iter()
                .map(|&src| ranks[src] / outgoing[src].len() as f64)
                .sum();
            new_ranks[i] = base + config.damping * inbound_sum + leaked_share;
        }

        let delta: f64 = (0..n).map(|i| (new_ranks[i] - ranks[i]).abs()).sum();
        std::mem::swap(&mut ranks, &mut new_ranks);

        if delta < config.epsilon {
            break;
        }
    }

    nodes
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, ranks[i]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::create_schema;
    use crate::storage::write;
    use rusqlite::Connection;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        create_schema(&conn).unwrap();
        conn
    }

    fn insert_file(conn: &Connection, path: &str) -> i64 {
        write::upsert_file(conn, path, 1000, 100, "rust", 10).unwrap()
    }

    #[test]
    fn test_empty_graph() {
        let result = pagerank_raw(&[], &[], &PageRankConfig::default());
        assert!(result.is_empty());
    }

    #[test]
    fn test_triangle_equal_rank() {
        let nodes = vec![1, 2, 3];
        let edges = vec![(1, 2), (2, 3), (3, 1)];
        let ranks = pagerank_raw(&nodes, &edges, &PageRankConfig::default());

        let r1 = ranks[&1];
        let r2 = ranks[&2];
        let r3 = ranks[&3];

        assert!(
            (r1 - r2).abs() < 0.001,
            "nodes in a triangle should have roughly equal rank"
        );
        assert!(
            (r2 - r3).abs() < 0.001,
            "nodes in a triangle should have roughly equal rank"
        );
        assert!(
            (r1 - 1.0 / 3.0).abs() < 0.01,
            "each node should be near 1/3"
        );
    }

    #[test]
    fn test_star_graph_hub_highest() {
        let nodes = vec![1, 2, 3, 4, 5];
        // Leaves 2,3,4,5 all point to hub 1
        let edges = vec![(2, 1), (3, 1), (4, 1), (5, 1)];
        let ranks = pagerank_raw(&nodes, &edges, &PageRankConfig::default());

        let hub_rank = ranks[&1];
        for &leaf in &[2, 3, 4, 5] {
            assert!(
                hub_rank > ranks[&leaf],
                "hub should have higher rank than leaf {leaf}"
            );
        }
    }

    #[test]
    fn test_disconnected_nodes_get_base_rank() {
        let nodes = vec![1, 2, 3];
        let edges: Vec<(i64, i64)> = vec![];
        let config = PageRankConfig::default();
        let ranks = pagerank_raw(&nodes, &edges, &config);

        let expected = 1.0 / 3.0;
        for &id in &nodes {
            assert!(
                (ranks[&id] - expected).abs() < 0.001,
                "disconnected node {id} should have rank ~{expected}, got {}",
                ranks[&id]
            );
        }
    }

    #[test]
    fn test_convergence_simple_graph() {
        let nodes = vec![1, 2];
        let edges = vec![(1, 2), (2, 1)];
        let config = PageRankConfig {
            damping: 0.85,
            iterations: 1000,
            epsilon: 1e-10,
        };
        let ranks = pagerank_raw(&nodes, &edges, &config);

        assert!(
            (ranks[&1] - ranks[&2]).abs() < 1e-10,
            "symmetric graph should converge to equal ranks"
        );
    }

    #[test]
    fn test_dangling_nodes() {
        // Node 3 has no outgoing edges (dangling)
        let nodes = vec![1, 2, 3];
        let edges = vec![(1, 2), (2, 3)];
        let ranks = pagerank_raw(&nodes, &edges, &PageRankConfig::default());

        // All nodes should have positive rank
        for &id in &nodes {
            assert!(ranks[&id] > 0.0, "node {id} should have positive rank");
        }

        // Node 3 receives rank from 2, and redistributes its dangling rank
        assert!(
            ranks[&3] > ranks[&1],
            "node 3 (sink) should have more rank than node 1 (source)"
        );
    }

    #[test]
    fn test_ranks_sum_to_one() {
        let nodes = vec![1, 2, 3, 4];
        let edges = vec![(1, 2), (2, 3), (3, 4), (4, 1), (1, 3)];
        let ranks = pagerank_raw(&nodes, &edges, &PageRankConfig::default());

        let total: f64 = ranks.values().sum();
        assert!(
            (total - 1.0).abs() < 0.001,
            "ranks should sum to ~1.0, got {total}"
        );
    }

    #[test]
    fn test_compute_pagerank_with_db() {
        let conn = setup();
        let f1 = insert_file(&conn, "src/a.rs");
        let f2 = insert_file(&conn, "src/b.rs");
        let f3 = insert_file(&conn, "src/c.rs");
        write::insert_edge(&conn, f1, f2, "import", None).unwrap();
        write::insert_edge(&conn, f2, f3, "import", None).unwrap();

        compute_pagerank(&conn, &PageRankConfig::default()).unwrap();

        let files = crate::storage::read::get_files_ranked(&conn, 10).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files[0].pagerank > 0.0);
    }

    #[test]
    fn test_compute_pagerank_empty_db() {
        let conn = setup();
        compute_pagerank(&conn, &PageRankConfig::default()).unwrap();
    }

    #[test]
    fn test_compute_symbol_pagerank_star_graph() {
        // Five symbols in two files: `hub` is called by four leaves.
        // `hub` should end up with a higher rank than any individual leaf.
        let conn = setup();
        let file_id = write::upsert_file(&conn, "src/lib.rs", 0, 0, "rust", 0).unwrap();
        let inserts = vec![
            crate::storage::models::SymbolInsert {
                name: "hub".to_string(),
                kind: "function".to_string(),
                line_start: 1,
                line_end: 2,
                signature: None,
                is_exported: true,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            },
            crate::storage::models::SymbolInsert {
                name: "leaf_a".to_string(),
                kind: "function".to_string(),
                line_start: 3,
                line_end: 4,
                signature: None,
                is_exported: false,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            },
            crate::storage::models::SymbolInsert {
                name: "leaf_b".to_string(),
                kind: "function".to_string(),
                line_start: 5,
                line_end: 6,
                signature: None,
                is_exported: false,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            },
            crate::storage::models::SymbolInsert {
                name: "leaf_c".to_string(),
                kind: "function".to_string(),
                line_start: 7,
                line_end: 8,
                signature: None,
                is_exported: false,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            },
            crate::storage::models::SymbolInsert {
                name: "leaf_d".to_string(),
                kind: "function".to_string(),
                line_start: 9,
                line_end: 10,
                signature: None,
                is_exported: false,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            },
        ];
        let ids = write::insert_symbols(&conn, file_id, &inserts).unwrap();
        let (hub, leaves) = (ids[0], &ids[1..]);
        let edges: Vec<(i64, i64, &str)> = leaves.iter().map(|&leaf| (leaf, hub, "call")).collect();
        write::insert_symbol_refs(&conn, &edges).unwrap();

        compute_symbol_pagerank(&conn, &PageRankConfig::default()).unwrap();

        let hub_rank: f64 = conn
            .query_row("SELECT pagerank FROM symbols WHERE id = ?1", [hub], |r| {
                r.get(0)
            })
            .unwrap();
        for &leaf in leaves {
            let leaf_rank: f64 = conn
                .query_row("SELECT pagerank FROM symbols WHERE id = ?1", [leaf], |r| {
                    r.get(0)
                })
                .unwrap();
            assert!(
                hub_rank > leaf_rank,
                "hub should outrank leaf {leaf} (got hub={hub_rank}, leaf={leaf_rank})"
            );
        }
    }

    #[test]
    fn test_compute_symbol_pagerank_empty_db() {
        let conn = setup();
        compute_symbol_pagerank(&conn, &PageRankConfig::default()).unwrap();
    }

    #[test]
    fn test_warm_start_converges_in_one_iteration() {
        let nodes = vec![1, 2, 3];
        let edges = vec![(1, 2), (2, 3), (3, 1)];
        let config = PageRankConfig {
            iterations: 1,
            ..Default::default()
        };
        let cold = pagerank_raw(&nodes, &edges, &PageRankConfig::default());
        let warm = pagerank_inner(&nodes, &edges, &config, &cold);
        for &id in &nodes {
            assert!(
                (cold[&id] - warm[&id]).abs() < 0.001,
                "warm-start with converged input should match cold result for node {id}"
            );
        }
    }

    #[test]
    fn test_warm_start_falls_back_on_zero_ranks() {
        let nodes = vec![1, 2, 3];
        let edges = vec![(1, 2), (2, 3), (3, 1)];
        let zeros: HashMap<i64, f64> = nodes.iter().map(|&id| (id, 0.0)).collect();
        let cold = pagerank_raw(&nodes, &edges, &PageRankConfig::default());
        let from_zeros = pagerank_inner(&nodes, &edges, &PageRankConfig::default(), &zeros);
        for &id in &nodes {
            assert!(
                (cold[&id] - from_zeros[&id]).abs() < 0.001,
                "zero prev_ranks should produce the same result as cold start for node {id}"
            );
        }
    }

    #[test]
    fn test_warm_start_handles_new_node() {
        let nodes = vec![1, 2, 3];
        let edges = vec![(1, 2), (2, 3), (3, 1)];
        let cold = pagerank_raw(&nodes, &edges, &PageRankConfig::default());
        // Node 4 is new and has no previous rank.
        let nodes_extended = vec![1, 2, 3, 4];
        let edges_extended = vec![(1, 2), (2, 3), (3, 1), (3, 4)];
        let warm = pagerank_inner(
            &nodes_extended,
            &edges_extended,
            &PageRankConfig::default(),
            &cold,
        );
        let total: f64 = warm.values().sum();
        assert!(
            (total - 1.0).abs() < 0.01,
            "ranks should still sum to ~1.0 after adding a new node, got {total}"
        );
        assert!(warm[&4] > 0.0, "new node should have positive rank");
    }

    #[test]
    fn test_compute_pagerank_warm_start_idempotent() {
        let conn = setup();
        let f1 = insert_file(&conn, "src/a.rs");
        let f2 = insert_file(&conn, "src/b.rs");
        write::insert_edge(&conn, f1, f2, "import", None).unwrap();

        compute_pagerank(&conn, &PageRankConfig::default()).unwrap();
        let first = crate::storage::read::get_all_files(&conn).unwrap();
        let r1: Vec<f64> = first.iter().map(|f| f.pagerank).collect();

        // Second call uses warm-start from stored ranks.
        compute_pagerank(&conn, &PageRankConfig::default()).unwrap();
        let second = crate::storage::read::get_all_files(&conn).unwrap();
        let r2: Vec<f64> = second.iter().map(|f| f.pagerank).collect();

        for (a, b) in r1.iter().zip(r2.iter()) {
            assert!(
                (a - b).abs() < 0.0001,
                "re-running PageRank on unchanged graph should produce same ranks: {a} vs {b}"
            );
        }
    }
}
