// Rust guideline compliant 2026-04-12

//! Deterministic community detection over the import graph.
//!
//! Implements the classical Louvain modularity-maximization algorithm
//! [Blondel et al. 2008] with a connectedness refinement pass borrowed from
//! the Leiden algorithm [Traag et al. 2019]. The refinement pass splits any
//! community with disconnected sub-components into separate clusters, which
//! is Louvain's well-known failure mode that Leiden was invented to fix.
//!
//! Determinism: all iteration happens in sorted-node / sorted-community
//! order, ties break on the smallest community id, and no rng is consulted.
//! Running the same input twice must produce byte-identical output — the
//! auto-wiki feature depends on this so re-indexing does not churn every PR.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::error::Result;
use crate::storage::read::{get_all_edges, get_all_files};
use crate::storage::write::{clear_file_clusters, upsert_file_cluster};

/// Reserved cluster id for files that fell below `min_cluster_size` and
/// were merged into the catch-all bucket. Numeric `0` is never assigned
/// by the optimization loop itself, so callers can rely on it as a
/// sentinel without walking the whole cluster set.
pub const MISC_CLUSTER_ID: i64 = 0;

pub struct LeidenConfig {
    /// Resolution parameter γ for the modularity objective. Larger values
    /// produce more, smaller clusters; smaller values merge clusters.
    pub resolution: f64,
    /// Clusters with fewer than this many files are folded into the
    /// [`MISC_CLUSTER_ID`] bucket after optimization.
    pub min_cluster_size: u32,
    /// Maximum local-move passes before the algorithm gives up on improving
    /// modularity. In practice convergence is reached in ≤10 passes.
    pub max_iterations: u32,
    /// Minimum modularity improvement per pass; lower deltas stop the loop.
    pub epsilon: f64,
}

impl Default for LeidenConfig {
    fn default() -> Self {
        Self {
            resolution: 1.0,
            min_cluster_size: 3,
            max_iterations: 50,
            epsilon: 1e-6,
        }
    }
}

#[allow(
    dead_code,
    reason = "fields are consumed by tests and future callers that surface cluster counts"
)]
pub struct ClusterReport {
    pub assignments: HashMap<i64, i64>,
    pub modularity: f64,
    pub cluster_count: usize,
    pub misc_count: usize,
}

/// Run modularity-maximizing community detection over `(nodes, edges)`.
///
/// `nodes` lists the node ids in the graph (may include isolated nodes).
/// `edges` is a directed edge list — each pair contributes weight 1 to the
/// undirected projection, and parallel directed edges accumulate. Returns a
/// per-node community assignment (densely numbered from 1) and the final
/// modularity score on the weighted undirected graph.
///
/// The result is deterministic: same input, same output.
pub fn leiden_raw(
    nodes: &[i64],
    edges: &[(i64, i64)],
    config: &LeidenConfig,
) -> (HashMap<i64, i64>, f64) {
    let mut sorted_nodes: Vec<i64> = nodes.to_vec();
    sorted_nodes.sort_unstable();
    sorted_nodes.dedup();

    if sorted_nodes.is_empty() {
        return (HashMap::new(), 0.0);
    }

    let n = sorted_nodes.len();
    let node_to_idx: HashMap<i64, usize> = sorted_nodes
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i))
        .collect();

    let mut weight_map: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for &(src, dst) in edges {
        let (Some(&si), Some(&di)) = (node_to_idx.get(&src), node_to_idx.get(&dst)) else {
            continue;
        };
        if si == di {
            continue;
        }
        let key = if si < di { (si, di) } else { (di, si) };
        *weight_map.entry(key).or_insert(0.0) += 1.0;
    }

    let mut adjacency: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    for (&(a, b), &w) in &weight_map {
        adjacency[a].push((b, w));
        adjacency[b].push((a, w));
    }
    for neighbors in &mut adjacency {
        neighbors.sort_unstable_by_key(|(idx, _)| *idx);
    }

    let degrees: Vec<f64> = adjacency
        .iter()
        .map(|row| row.iter().map(|(_, w)| *w).sum())
        .collect();
    let total_weight: f64 = degrees.iter().sum::<f64>() / 2.0;

    let mut community: Vec<usize> = (0..n).collect();
    let mut community_degree: Vec<f64> = degrees.clone();

    if total_weight > 0.0 {
        louvain_local_move(
            &adjacency,
            &degrees,
            &mut community,
            &mut community_degree,
            total_weight,
            config,
        );
    }

    refine_connected_components(&adjacency, &mut community);

    let modularity = compute_modularity(
        &adjacency,
        &degrees,
        &community,
        total_weight,
        config.resolution,
    );

    let mut idx_to_dense: HashMap<usize, i64> = HashMap::new();
    let mut next_dense: i64 = 1;
    let mut assignments: HashMap<i64, i64> = HashMap::new();
    for (idx, &node_id) in sorted_nodes.iter().enumerate() {
        let raw = community[idx];
        let dense = *idx_to_dense.entry(raw).or_insert_with(|| {
            let id = next_dense;
            next_dense += 1;
            id
        });
        assignments.insert(node_id, dense);
    }

    (assignments, modularity)
}

fn louvain_local_move(
    adjacency: &[Vec<(usize, f64)>],
    degrees: &[f64],
    community: &mut [usize],
    community_degree: &mut [f64],
    total_weight: f64,
    config: &LeidenConfig,
) {
    let n = adjacency.len();
    let two_m = 2.0 * total_weight;
    let gamma = config.resolution;

    let mut prev_modularity = compute_modularity(adjacency, degrees, community, total_weight, gamma);
    for _ in 0..config.max_iterations {
        let mut moved = false;
        for node in 0..n {
            let current = community[node];
            let node_degree = degrees[node];

            let mut neighbor_weights: BTreeMap<usize, f64> = BTreeMap::new();
            for &(neighbor, weight) in &adjacency[node] {
                if neighbor == node {
                    continue;
                }
                *neighbor_weights.entry(community[neighbor]).or_insert(0.0) += weight;
            }

            let k_in_current = neighbor_weights.get(&current).copied().unwrap_or(0.0);
            community_degree[current] = (community_degree[current] - node_degree).max(0.0);

            let mut best_community = current;
            let mut best_gain = 0.0_f64;
            for (&target, &k_in_target) in &neighbor_weights {
                if target == current {
                    continue;
                }
                let gain = k_in_target - gamma * node_degree * community_degree[target] / two_m;
                if gain > best_gain {
                    best_gain = gain;
                    best_community = target;
                }
            }

            let stay_gain = k_in_current - gamma * node_degree * community_degree[current] / two_m;
            if best_gain <= stay_gain {
                best_community = current;
            }

            community_degree[best_community] += node_degree;
            if best_community != current {
                community[node] = best_community;
                moved = true;
            }
        }

        if !moved {
            break;
        }
        let cur_modularity = compute_modularity(adjacency, degrees, community, total_weight, gamma);
        if cur_modularity.is_nan() || (cur_modularity - prev_modularity).abs() < config.epsilon {
            break;
        }
        prev_modularity = cur_modularity;
    }
}

fn refine_connected_components(adjacency: &[Vec<(usize, f64)>], community: &mut [usize]) {
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (idx, &cid) in community.iter().enumerate() {
        groups.entry(cid).or_default().push(idx);
    }

    let mut max_id: usize = community.iter().copied().max().unwrap_or(0);
    for (_cid, members) in groups {
        if members.len() < 2 {
            continue;
        }
        let member_set: std::collections::BTreeSet<usize> = members.iter().copied().collect();
        let mut visited: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
        let mut components: Vec<Vec<usize>> = Vec::new();

        for &start in &members {
            if visited.contains(&start) {
                continue;
            }
            let mut queue: VecDeque<usize> = VecDeque::new();
            queue.push_back(start);
            visited.insert(start);
            let mut comp: Vec<usize> = Vec::new();
            while let Some(v) = queue.pop_front() {
                comp.push(v);
                for &(neighbor, _) in &adjacency[v] {
                    if !member_set.contains(&neighbor) {
                        continue;
                    }
                    if visited.insert(neighbor) {
                        queue.push_back(neighbor);
                    }
                }
            }
            comp.sort_unstable();
            components.push(comp);
        }

        if components.len() <= 1 {
            continue;
        }
        components.sort_by_key(|c| c.first().copied().unwrap_or(usize::MAX));
        for comp in components.iter().skip(1) {
            max_id += 1;
            for &node in comp {
                community[node] = max_id;
            }
        }
    }
}

fn compute_modularity(
    adjacency: &[Vec<(usize, f64)>],
    degrees: &[f64],
    community: &[usize],
    total_weight: f64,
    resolution: f64,
) -> f64 {
    if total_weight <= 0.0 {
        return 0.0;
    }
    let two_m = 2.0 * total_weight;
    let n = adjacency.len();
    let mut q = 0.0_f64;

    for u in 0..n {
        for &(v, weight) in &adjacency[u] {
            if community[u] != community[v] {
                continue;
            }
            q += weight - resolution * degrees[u] * degrees[v] / two_m;
        }
    }
    q / two_m
}

/// Load the edge graph, run Leiden, and persist the result into
/// `file_clusters`. Clusters smaller than `config.min_cluster_size` get
/// folded into [`MISC_CLUSTER_ID`]. Returns a report suitable for logging.
pub fn compute_clusters(conn: &Connection, config: &LeidenConfig) -> Result<ClusterReport> {
    let edges = get_all_edges(conn)?;
    let files = get_all_files(conn)?;
    let nodes: Vec<i64> = files.iter().map(|f| f.id).collect();

    let (raw, modularity) = leiden_raw(&nodes, &edges, config);

    let mut counts: HashMap<i64, usize> = HashMap::new();
    for &cid in raw.values() {
        *counts.entry(cid).or_insert(0) += 1;
    }

    let min_size = config.min_cluster_size as usize;
    let mut remap: HashMap<i64, i64> = HashMap::new();
    let mut next: i64 = 1;
    let mut sorted_ids: Vec<i64> = counts.keys().copied().collect();
    sorted_ids.sort_unstable();
    for cid in sorted_ids {
        let size = counts[&cid];
        if size < min_size {
            remap.insert(cid, MISC_CLUSTER_ID);
        } else {
            remap.insert(cid, next);
            next += 1;
        }
    }

    let mut final_assignments: HashMap<i64, i64> = HashMap::new();
    for (file_id, raw_cid) in raw {
        final_assignments.insert(file_id, *remap.get(&raw_cid).unwrap_or(&MISC_CLUSTER_ID));
    }

    let computed_at: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let tx = conn.unchecked_transaction()?;
    clear_file_clusters(&tx)?;
    for (&file_id, &cluster_id) in &final_assignments {
        upsert_file_cluster(&tx, file_id, cluster_id, computed_at)?;
    }
    tx.commit()?;

    let cluster_count = (next - 1) as usize;
    let misc_count = final_assignments
        .values()
        .filter(|&&c| c == MISC_CLUSTER_ID)
        .count();

    Ok(ClusterReport {
        assignments: final_assignments,
        modularity,
        cluster_count,
        misc_count,
    })
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
    fn empty_graph_has_no_assignments() {
        let (map, q) = leiden_raw(&[], &[], &LeidenConfig::default());
        assert!(map.is_empty());
        assert_eq!(q, 0.0);
    }

    #[test]
    fn isolated_nodes_each_get_own_cluster() {
        let nodes = vec![1, 2, 3];
        let (map, _) = leiden_raw(&nodes, &[], &LeidenConfig::default());
        let unique: std::collections::BTreeSet<i64> = map.values().copied().collect();
        assert_eq!(unique.len(), 3);
    }

    #[test]
    fn two_disconnected_triangles_split_into_two_clusters() {
        let nodes = vec![1, 2, 3, 4, 5, 6];
        let edges = vec![(1, 2), (2, 3), (3, 1), (4, 5), (5, 6), (6, 4)];
        let (map, modularity) = leiden_raw(&nodes, &edges, &LeidenConfig::default());
        let unique: std::collections::BTreeSet<i64> = map.values().copied().collect();
        assert_eq!(
            unique.len(),
            2,
            "two disconnected triangles should produce two clusters"
        );
        assert!(map[&1] == map[&2] && map[&2] == map[&3]);
        assert!(map[&4] == map[&5] && map[&5] == map[&6]);
        assert!(map[&1] != map[&4]);
        assert!(modularity > 0.0);
    }

    #[test]
    fn barbell_graph_splits_the_triangles() {
        let nodes = vec![1, 2, 3, 4, 5, 6];
        let edges = vec![(1, 2), (2, 3), (3, 1), (4, 5), (5, 6), (6, 4), (3, 4)];
        let (map, _) = leiden_raw(&nodes, &edges, &LeidenConfig::default());
        let unique: std::collections::BTreeSet<i64> = map.values().copied().collect();
        assert!(
            unique.len() >= 2,
            "barbell graph should produce at least two clusters, got {}",
            unique.len()
        );
    }

    #[test]
    fn determinism_same_input_same_output() {
        let nodes: Vec<i64> = (1..=20).collect();
        let edges: Vec<(i64, i64)> = vec![
            (1, 2),
            (2, 3),
            (3, 4),
            (4, 5),
            (5, 1),
            (6, 7),
            (7, 8),
            (8, 9),
            (9, 10),
            (10, 6),
            (11, 12),
            (12, 13),
            (13, 14),
            (14, 11),
            (5, 6),
            (10, 11),
        ];
        let config = LeidenConfig::default();
        let (a, qa) = leiden_raw(&nodes, &edges, &config);
        let (b, qb) = leiden_raw(&nodes, &edges, &config);
        assert_eq!(a, b);
        assert!((qa - qb).abs() < f64::EPSILON);
    }

    #[test]
    fn single_chain_collapses_to_few_clusters() {
        let nodes: Vec<i64> = (1..=8).collect();
        let edges: Vec<(i64, i64)> = (1..8).map(|i| (i, i + 1)).collect();
        let (_, modularity) = leiden_raw(&nodes, &edges, &LeidenConfig::default());
        assert!(modularity >= 0.0);
    }

    #[test]
    fn compute_clusters_writes_to_db() {
        let conn = setup();
        let f1 = insert_file(&conn, "src/a.rs");
        let f2 = insert_file(&conn, "src/b.rs");
        let f3 = insert_file(&conn, "src/c.rs");
        let f4 = insert_file(&conn, "src/d.rs");
        let f5 = insert_file(&conn, "src/e.rs");
        let f6 = insert_file(&conn, "src/f.rs");
        write::insert_edge(&conn, f1, f2, "import", None).unwrap();
        write::insert_edge(&conn, f2, f3, "import", None).unwrap();
        write::insert_edge(&conn, f3, f1, "import", None).unwrap();
        write::insert_edge(&conn, f4, f5, "import", None).unwrap();
        write::insert_edge(&conn, f5, f6, "import", None).unwrap();
        write::insert_edge(&conn, f6, f4, "import", None).unwrap();

        let report = compute_clusters(&conn, &LeidenConfig::default()).unwrap();
        assert_eq!(report.assignments.len(), 6);
        assert!(report.cluster_count >= 2);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM file_clusters", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 6);
    }

    #[test]
    fn compute_clusters_merges_small_into_misc() {
        let conn = setup();
        let big1 = insert_file(&conn, "src/big1.rs");
        let big2 = insert_file(&conn, "src/big2.rs");
        let big3 = insert_file(&conn, "src/big3.rs");
        let big4 = insert_file(&conn, "src/big4.rs");
        let small1 = insert_file(&conn, "src/small1.rs");
        let small2 = insert_file(&conn, "src/small2.rs");

        write::insert_edge(&conn, big1, big2, "import", None).unwrap();
        write::insert_edge(&conn, big2, big3, "import", None).unwrap();
        write::insert_edge(&conn, big3, big4, "import", None).unwrap();
        write::insert_edge(&conn, big4, big1, "import", None).unwrap();
        write::insert_edge(&conn, small1, small2, "import", None).unwrap();

        let config = LeidenConfig {
            min_cluster_size: 3,
            ..Default::default()
        };
        let report = compute_clusters(&conn, &config).unwrap();
        assert!(report.misc_count > 0, "small cluster should fold into misc");
    }
}
