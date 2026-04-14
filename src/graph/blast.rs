use std::collections::{HashMap, HashSet, VecDeque};

use rusqlite::Connection;

use crate::error::Result;
use crate::storage::read::get_all_edges;

#[allow(dead_code)]
pub struct BlastResult {
    pub file_id: i64,
    pub direct_importers: Vec<i64>,
    pub transitive_count: usize,
    pub transitive_importers: Vec<i64>,
}

/// For each file, count how many files transitively depend on it.
pub fn compute_blast_radius(conn: &Connection) -> Result<HashMap<i64, i64>> {
    let edges = get_all_edges(conn)?;
    Ok(blast_radius_raw(&edges))
}

/// Compute blast radius on abstract edge data without DB access.
fn blast_radius_raw(edges: &[(i64, i64)]) -> HashMap<i64, i64> {
    let reverse = build_reverse_adjacency(edges);

    let all_nodes: HashSet<i64> = edges.iter().flat_map(|&(a, b)| [a, b]).collect();

    let mut result = HashMap::new();
    for &node in &all_nodes {
        let reachable = bfs_reachable(&reverse, node);
        result.insert(node, reachable.len() as i64);
    }
    result
}

/// Get blast radius for a specific file.
pub fn blast_radius_for_file(conn: &Connection, file_id: i64) -> Result<BlastResult> {
    let edges = get_all_edges(conn)?;
    Ok(blast_for_node(file_id, &edges))
}

/// Compute blast radius for a specific set of files in one pass.
///
/// Cheaper than `compute_blast_radius` when only a handful of files need a
/// lookup: loads the edge list once, builds the reverse adjacency once, then
/// runs a BFS per requested node instead of per node in the whole graph.
/// Used by tools that already know which files matched a query (qartez_find,
/// qartez_read, qartez_deps).
pub fn blast_radius_for_files(conn: &Connection, file_ids: &[i64]) -> Result<HashMap<i64, i64>> {
    if file_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let edges = get_all_edges(conn)?;
    let reverse = build_reverse_adjacency(&edges);
    let mut out: HashMap<i64, i64> = HashMap::with_capacity(file_ids.len());
    for &id in file_ids {
        if out.contains_key(&id) {
            continue;
        }
        let reached = bfs_reachable(&reverse, id);
        out.insert(id, reached.len() as i64);
    }
    Ok(out)
}

fn blast_for_node(file_id: i64, edges: &[(i64, i64)]) -> BlastResult {
    let reverse = build_reverse_adjacency(edges);

    let direct_importers: Vec<i64> = reverse.get(&file_id).cloned().unwrap_or_default();

    let transitive_importers: Vec<i64> = bfs_reachable(&reverse, file_id).into_iter().collect();

    BlastResult {
        file_id,
        direct_importers,
        transitive_count: transitive_importers.len(),
        transitive_importers,
    }
}

/// Build reverse adjacency: if A imports B (edge A->B), then reverse[B] contains A.
fn build_reverse_adjacency(edges: &[(i64, i64)]) -> HashMap<i64, Vec<i64>> {
    let mut reverse: HashMap<i64, Vec<i64>> = HashMap::new();
    for &(from, to) in edges {
        if from == to {
            continue;
        }
        reverse.entry(to).or_default().push(from);
    }
    reverse
}

/// BFS from `start` on the given adjacency, returning all reachable nodes excluding `start`.
fn bfs_reachable(adjacency: &HashMap<i64, Vec<i64>>, start: i64) -> Vec<i64> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    if let Some(neighbors) = adjacency.get(&start) {
        for &n in neighbors {
            if visited.insert(n) {
                queue.push_back(n);
            }
        }
    }

    while let Some(current) = queue.pop_front() {
        if let Some(neighbors) = adjacency.get(&current) {
            for &n in neighbors {
                if n != start && visited.insert(n) {
                    queue.push_back(n);
                }
            }
        }
    }

    let mut result: Vec<i64> = visited.into_iter().collect();
    result.sort_unstable();
    result
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
    fn test_linear_chain() {
        // A -> B -> C means A imports B, B imports C
        // blast(C) should include both A and B
        let edges = vec![(1, 2), (2, 3)];
        let radii = blast_radius_raw(&edges);

        assert_eq!(
            radii[&3], 2,
            "C should be transitively depended on by A and B"
        );
        assert_eq!(radii[&2], 1, "B should be depended on by A only");
        assert_eq!(radii[&1], 0, "A has no importers");
    }

    #[test]
    fn test_star_pattern() {
        // A -> C, B -> C
        let edges = vec![(1, 3), (2, 3)];
        let radii = blast_radius_raw(&edges);

        assert_eq!(radii[&3], 2, "C is imported by A and B");
        assert_eq!(radii[&1], 0, "A has no importers");
        assert_eq!(radii[&2], 0, "B has no importers");
    }

    #[test]
    fn test_no_importers() {
        let edges = vec![(1, 2)];
        let radii = blast_radius_raw(&edges);

        assert_eq!(radii[&2], 1, "B is imported by A");
        assert_eq!(radii[&1], 0, "A has no importers");
    }

    #[test]
    fn test_empty_graph() {
        let edges: Vec<(i64, i64)> = vec![];
        let radii = blast_radius_raw(&edges);
        assert!(radii.is_empty());
    }

    #[test]
    fn test_diamond_graph() {
        // A -> B, A -> C, B -> D, C -> D
        let edges = vec![(1, 2), (1, 3), (2, 4), (3, 4)];
        let radii = blast_radius_raw(&edges);

        assert_eq!(radii[&4], 3, "D is transitively depended on by A, B, and C");
        assert_eq!(radii[&2], 1, "B is depended on by A");
        assert_eq!(radii[&3], 1, "C is depended on by A");
        assert_eq!(radii[&1], 0, "A has no importers");
    }

    #[test]
    fn test_blast_for_file_with_db() {
        let conn = setup();
        let a = insert_file(&conn, "src/a.rs");
        let b = insert_file(&conn, "src/b.rs");
        let c = insert_file(&conn, "src/c.rs");
        write::insert_edge(&conn, a, b, "import", None).unwrap();
        write::insert_edge(&conn, b, c, "import", None).unwrap();

        let result = blast_radius_for_file(&conn, c).unwrap();
        assert_eq!(result.file_id, c);
        assert_eq!(result.transitive_count, 2);
        assert!(result.direct_importers.contains(&b));

        let mut transitive = result.transitive_importers.clone();
        transitive.sort();
        assert!(transitive.contains(&a));
        assert!(transitive.contains(&b));
    }

    #[test]
    fn test_blast_for_file_no_importers() {
        let conn = setup();
        let a = insert_file(&conn, "src/a.rs");
        let b = insert_file(&conn, "src/b.rs");
        write::insert_edge(&conn, a, b, "import", None).unwrap();

        let result = blast_radius_for_file(&conn, a).unwrap();
        assert_eq!(result.transitive_count, 0);
        assert!(result.direct_importers.is_empty());
        assert!(result.transitive_importers.is_empty());
    }

    #[test]
    fn test_compute_blast_radius_with_db() {
        let conn = setup();
        let a = insert_file(&conn, "src/a.rs");
        let b = insert_file(&conn, "src/b.rs");
        let c = insert_file(&conn, "src/c.rs");
        write::insert_edge(&conn, a, c, "import", None).unwrap();
        write::insert_edge(&conn, b, c, "import", None).unwrap();

        let radii = compute_blast_radius(&conn).unwrap();
        assert_eq!(radii[&c], 2);
    }
}
