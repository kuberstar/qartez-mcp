use std::collections::HashSet;
use std::fs;

use rusqlite::Connection;
use tempfile::TempDir;

use qartez_mcp::git::cochange::{CoChangeConfig, analyze_cochanges};
use qartez_mcp::graph::{blast, leiden, pagerank, wiki};
use qartez_mcp::index;
use qartez_mcp::storage::{models::SymbolInsert, read, schema, write};

fn setup() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    conn
}

fn insert_file(conn: &Connection, path: &str) -> i64 {
    write::upsert_file(conn, path, 1000, 100, "rust", 10).unwrap()
}

fn sym(name: &str, kind: &str, start: u32, end: u32, exported: bool) -> SymbolInsert {
    SymbolInsert {
        name: name.to_string(),
        kind: kind.to_string(),
        line_start: start,
        line_end: end,
        signature: None,
        is_exported: exported,
        shape_hash: None,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    }
}

// ---------------------------------------------------------------------------
// 1. Unused exports: fallback query missing symbol_refs check
//
// populate_unused_exports checks BOTH file-level edges AND symbol_refs.
// count_unused_exports fallback also checks both.
// But get_unused_exports_page fallback only checks file-level edges,
// MISSING the symbol_refs condition.
// This test exposes the inconsistency.
// ---------------------------------------------------------------------------

#[test]
fn test_unused_exports_fallback_must_respect_symbol_refs() {
    let conn = setup();
    let lib = insert_file(&conn, "src/lib.rs");
    let consumer = insert_file(&conn, "src/consumer.rs");

    let lib_ids =
        write::insert_symbols(&conn, lib, &[sym("Config", "struct", 1, 5, true)]).unwrap();

    let consumer_ids = write::insert_symbols(
        &conn,
        consumer,
        &[sym("use_config", "function", 1, 3, false)],
    )
    .unwrap();

    // Symbol-level reference but NO file-level import edge.
    write::insert_symbol_refs(&conn, &[(consumer_ids[0], lib_ids[0], "type")]).unwrap();

    // Do NOT call populate_unused_exports — force the fallback path in both
    // count_unused_exports and get_unused_exports_page.

    let count = read::count_unused_exports(&conn).unwrap();
    let page = read::get_unused_exports_page(&conn, i64::MAX, 0).unwrap();

    // count_unused_exports correctly excludes Config (it checks symbol_refs).
    // get_unused_exports_page fallback is missing the symbol_refs check,
    // so it incorrectly reports Config as unused.
    assert_eq!(
        count as usize,
        page.len(),
        "count_unused_exports ({count}) disagrees with get_unused_exports_page ({}) — \
         the fallback query in get_unused_exports_page is missing the symbol_refs check",
        page.len()
    );
}

// ---------------------------------------------------------------------------
// 2. Unused exports: materialized vs fallback consistency
// ---------------------------------------------------------------------------

#[test]
fn test_unused_exports_materialized_equals_fallback() {
    let conn = setup();
    let lib = insert_file(&conn, "src/lib.rs");
    let orphan = insert_file(&conn, "src/orphan.rs");

    write::insert_symbols(
        &conn,
        lib,
        &[
            sym("UsedStruct", "struct", 1, 5, true),
            sym("private_fn", "function", 7, 10, false),
        ],
    )
    .unwrap();
    write::insert_symbols(&conn, orphan, &[sym("OrphanFn", "function", 1, 5, true)]).unwrap();

    // File-level edge: someone imports lib, so its exports are "used" at
    // the file level.
    let consumer = insert_file(&conn, "src/consumer.rs");
    write::insert_edge(&conn, consumer, lib, "import", None).unwrap();

    // Fallback path (materialized table empty).
    let fallback = read::get_unused_exports_page(&conn, i64::MAX, 0).unwrap();
    let fallback_names: Vec<&str> = fallback.iter().map(|(s, _)| s.name.as_str()).collect();

    // Materialized path.
    write::populate_unused_exports(&conn).unwrap();
    let materialized = read::get_unused_exports_page(&conn, i64::MAX, 0).unwrap();
    let mat_names: Vec<&str> = materialized.iter().map(|(s, _)| s.name.as_str()).collect();

    assert_eq!(
        fallback_names, mat_names,
        "fallback ({fallback_names:?}) and materialized ({mat_names:?}) must agree"
    );
}

// ---------------------------------------------------------------------------
// 3. Unused exports: unused_excluded flag respected
// ---------------------------------------------------------------------------

#[test]
fn test_unused_excluded_symbols_not_reported() {
    let conn = setup();
    let f = insert_file(&conn, "src/lib.rs");

    write::insert_symbols(
        &conn,
        f,
        &[
            SymbolInsert {
                name: "trait_impl".into(),
                kind: "function".into(),
                line_start: 1,
                line_end: 5,
                signature: None,
                is_exported: true,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: true,
                complexity: None,
                owner_type: None,
            },
            sym("normal_export", "function", 7, 10, true),
        ],
    )
    .unwrap();

    write::populate_unused_exports(&conn).unwrap();

    let unused = read::get_unused_exports_page(&conn, i64::MAX, 0).unwrap();
    let names: Vec<&str> = unused.iter().map(|(s, _)| s.name.as_str()).collect();

    assert!(
        !names.contains(&"trait_impl"),
        "unused_excluded symbols must not appear in unused exports"
    );
    assert!(
        names.contains(&"normal_export"),
        "normal exported symbols should appear"
    );
}

// ---------------------------------------------------------------------------
// 4. Unused exports: count matches page length after materialization
// ---------------------------------------------------------------------------

#[test]
fn test_unused_exports_count_matches_page_after_materialize() {
    let conn = setup();
    let f1 = insert_file(&conn, "src/a.rs");
    let f2 = insert_file(&conn, "src/b.rs");
    insert_file(&conn, "src/c.rs");

    write::insert_symbols(&conn, f1, &[sym("alpha", "function", 1, 5, true)]).unwrap();
    write::insert_symbols(
        &conn,
        f2,
        &[
            sym("beta", "function", 1, 5, true),
            sym("gamma", "struct", 7, 10, true),
        ],
    )
    .unwrap();

    write::populate_unused_exports(&conn).unwrap();

    let count = read::count_unused_exports(&conn).unwrap();
    let page = read::get_unused_exports_page(&conn, i64::MAX, 0).unwrap();

    assert_eq!(
        count as usize,
        page.len(),
        "count ({count}) must equal page length ({})",
        page.len()
    );
}

// ---------------------------------------------------------------------------
// 5. FTS sanitization edge cases
// ---------------------------------------------------------------------------

#[test]
fn test_sanitize_fts_empty_string() {
    let result = read::sanitize_fts_query("");
    // Empty input is not "plain" so it gets quoted.
    assert_eq!(result, "\"\"");
}

#[test]
fn test_sanitize_fts_special_chars_quoted() {
    let result = read::sanitize_fts_query("foo-bar");
    assert!(
        result.starts_with('"') && result.ends_with('"'),
        "special chars should be quoted: {result}"
    );
    assert_eq!(result, "\"foo-bar\"");
}

#[test]
fn test_sanitize_fts_double_quotes_escaped() {
    let result = read::sanitize_fts_query("foo\"bar");
    assert_eq!(result, "\"foo\"\"bar\"");
}

#[test]
fn test_sanitize_fts_plain_passthrough() {
    assert_eq!(read::sanitize_fts_query("hello"), "hello");
    assert_eq!(read::sanitize_fts_query("Hello_World"), "Hello_World");
    assert_eq!(read::sanitize_fts_query("prefix*"), "prefix*");
}

#[test]
fn test_sanitize_fts_all_special() {
    let result = read::sanitize_fts_query("@#$%");
    assert!(
        result.starts_with('"'),
        "all-special chars should be quoted"
    );
}

// ---------------------------------------------------------------------------
// 6. FTS search: empty query and unsanitized query
// ---------------------------------------------------------------------------

#[test]
fn test_fts_search_empty_query_does_not_panic() {
    let conn = setup();
    let f = insert_file(&conn, "src/lib.rs");
    write::insert_symbols(&conn, f, &[sym("hello", "function", 1, 3, true)]).unwrap();
    write::sync_fts(&conn).unwrap();

    // Empty string sanitized to "" — should not panic, may return empty.
    let sanitized = read::sanitize_fts_query("");
    let result = read::search_symbols_fts(&conn, &sanitized, 10);
    // The important thing is it doesn't panic.
    assert!(result.is_ok() || result.is_err());
}

// ---------------------------------------------------------------------------
// 7. insert_symbols: forward parent_idx silently becomes NULL
// ---------------------------------------------------------------------------

#[test]
fn test_insert_symbols_forward_parent_idx_drops_silently() {
    let conn = setup();
    let f = insert_file(&conn, "src/lib.rs");

    write::insert_symbols(
        &conn,
        f,
        &[
            SymbolInsert {
                name: "child".into(),
                kind: "field".into(),
                line_start: 2,
                line_end: 2,
                signature: None,
                is_exported: false,
                shape_hash: None,
                parent_idx: Some(1), // forward reference — not yet inserted
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            },
            sym("parent_struct", "struct", 1, 10, true),
        ],
    )
    .unwrap();

    let symbols = read::get_symbols_for_file(&conn, f).unwrap();
    let child = symbols.iter().find(|s| s.name == "child").unwrap();

    // Forward parent_idx was silently converted to NULL.
    assert!(
        child.parent_id.is_none(),
        "forward parent_idx silently dropped to NULL (data loss)"
    );
}

#[test]
fn test_insert_symbols_backward_parent_idx_resolves() {
    let conn = setup();
    let f = insert_file(&conn, "src/lib.rs");

    let ids = write::insert_symbols(
        &conn,
        f,
        &[
            sym("MyStruct", "struct", 1, 10, true),
            SymbolInsert {
                name: "my_field".into(),
                kind: "field".into(),
                line_start: 2,
                line_end: 2,
                signature: None,
                is_exported: false,
                shape_hash: None,
                parent_idx: Some(0),
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            },
        ],
    )
    .unwrap();

    let symbols = read::get_symbols_for_file(&conn, f).unwrap();
    let field = symbols.iter().find(|s| s.name == "my_field").unwrap();

    assert_eq!(
        field.parent_id,
        Some(ids[0]),
        "backward parent_idx should resolve to parent's DB id"
    );
}

#[test]
fn test_insert_symbols_out_of_bounds_parent_idx() {
    let conn = setup();
    let f = insert_file(&conn, "src/lib.rs");

    // parent_idx=99 is way out of bounds.
    write::insert_symbols(
        &conn,
        f,
        &[SymbolInsert {
            name: "orphan".into(),
            kind: "field".into(),
            line_start: 1,
            line_end: 1,
            signature: None,
            is_exported: false,
            shape_hash: None,
            parent_idx: Some(99),
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        }],
    )
    .unwrap();

    let symbols = read::get_symbols_for_file(&conn, f).unwrap();
    assert!(
        symbols[0].parent_id.is_none(),
        "out-of-bounds parent_idx silently becomes NULL"
    );
}

// ---------------------------------------------------------------------------
// 8. Cochange: ordering, self-reference, batch increment
// ---------------------------------------------------------------------------

#[test]
fn test_cochange_canonical_ordering() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");

    // Insert in both orders — should merge into one canonical pair.
    write::upsert_cochange(&conn, a, b).unwrap();
    write::upsert_cochange(&conn, b, a).unwrap();

    let cochanges_a = read::get_cochanges(&conn, a, 10).unwrap();
    assert_eq!(cochanges_a.len(), 1);
    assert_eq!(cochanges_a[0].0.count, 2);
}

#[test]
fn test_cochange_n_batch_increment() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");

    write::upsert_cochange_n(&conn, a, b, 5).unwrap();
    write::upsert_cochange_n(&conn, b, a, 3).unwrap();

    let cochanges = read::get_cochanges(&conn, a, 10).unwrap();
    assert_eq!(cochanges.len(), 1);
    assert_eq!(cochanges[0].0.count, 8, "5 + 3 = 8");
}

#[test]
fn test_cochange_self_reference() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");

    write::upsert_cochange(&conn, a, a).unwrap();

    let cochanges = read::get_cochanges(&conn, a, 10).unwrap();
    // A self-referencing co-change is stored — the CASE in the query will
    // join on the same file. This documents the current behavior.
    assert_eq!(cochanges.len(), 1);
}

// ---------------------------------------------------------------------------
// 9. PageRank: edge cases
// ---------------------------------------------------------------------------

#[test]
fn test_pagerank_single_node_no_edges() {
    let ranks = pagerank::pagerank_raw(&[1], &[], &pagerank::PageRankConfig::default());
    assert_eq!(ranks.len(), 1);
    let rank = ranks[&1];
    assert!(
        (rank - 1.0).abs() < 0.01,
        "single node should have rank ~1.0, got {rank}"
    );
}

#[test]
fn test_pagerank_self_loop_ignored() {
    let ranks = pagerank::pagerank_raw(
        &[1, 2],
        &[(1, 1), (1, 2)],
        &pagerank::PageRankConfig::default(),
    );
    assert!(
        ranks[&2] > ranks[&1],
        "node 2 (imported) should rank higher; got n1={}, n2={}",
        ranks[&1],
        ranks[&2]
    );
}

#[test]
fn test_pagerank_duplicate_edges_deduped() {
    let ranks_single = pagerank::pagerank_raw(
        &[1, 2, 3],
        &[(1, 3), (2, 3)],
        &pagerank::PageRankConfig::default(),
    );
    let ranks_duped = pagerank::pagerank_raw(
        &[1, 2, 3],
        &[(1, 3), (2, 3), (1, 3), (1, 3)],
        &pagerank::PageRankConfig::default(),
    );
    assert!(
        (ranks_single[&3] - ranks_duped[&3]).abs() < 0.001,
        "duplicates should be deduped: single={}, duped={}",
        ranks_single[&3],
        ranks_duped[&3]
    );
}

#[test]
fn test_pagerank_all_dangling_equal_rank() {
    let ranks = pagerank::pagerank_raw(&[1, 2, 3], &[], &pagerank::PageRankConfig::default());
    let expected = 1.0 / 3.0;
    for &id in &[1, 2, 3] {
        assert!(
            (ranks[&id] - expected).abs() < 0.01,
            "node {id}: expected ~{expected}, got {}",
            ranks[&id]
        );
    }
}

#[test]
fn test_pagerank_ranks_sum_to_one_complex_graph() {
    let ranks = pagerank::pagerank_raw(
        &[1, 2, 3, 4, 5],
        &[(1, 2), (1, 3), (2, 4), (3, 4), (4, 5), (5, 1)],
        &pagerank::PageRankConfig::default(),
    );
    let total: f64 = ranks.values().sum();
    assert!(
        (total - 1.0).abs() < 0.01,
        "ranks should sum to ~1.0, got {total}"
    );
}

#[test]
fn test_pagerank_nodes_not_in_edges_still_get_rank() {
    // Node 99 has no edges at all but is in the node list.
    let ranks =
        pagerank::pagerank_raw(&[1, 2, 99], &[(1, 2)], &pagerank::PageRankConfig::default());
    assert!(ranks.contains_key(&99), "isolated node should have a rank");
    assert!(ranks[&99] > 0.0, "isolated node should have positive rank");
}

#[test]
fn test_pagerank_edges_with_unknown_nodes_ignored() {
    // Edge references node 999 which is not in the node list.
    let ranks = pagerank::pagerank_raw(
        &[1, 2],
        &[(1, 2), (1, 999)],
        &pagerank::PageRankConfig::default(),
    );
    assert_eq!(ranks.len(), 2, "unknown nodes should not appear");
    assert!(!ranks.contains_key(&999));
}

// ---------------------------------------------------------------------------
// 10. PageRank: convergence with different configs
// ---------------------------------------------------------------------------

#[test]
fn test_pagerank_zero_iterations() {
    let ranks = pagerank::pagerank_raw(
        &[1, 2],
        &[(1, 2)],
        &pagerank::PageRankConfig {
            damping: 0.85,
            iterations: 0,
            epsilon: 1e-6,
        },
    );
    // Zero iterations => initial distribution: 1/n for each.
    let expected = 0.5;
    assert!(
        (ranks[&1] - expected).abs() < 0.01,
        "0 iterations should return initial distribution"
    );
}

// ---------------------------------------------------------------------------
// 11. Blast radius: cycles, isolated nodes, self-loops
// ---------------------------------------------------------------------------

#[test]
fn test_blast_radius_cycle() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");
    let c = insert_file(&conn, "c.rs");

    write::insert_edge(&conn, a, b, "import", None).unwrap();
    write::insert_edge(&conn, b, c, "import", None).unwrap();
    write::insert_edge(&conn, c, a, "import", None).unwrap();

    let result = blast::blast_radius_for_file(&conn, a).unwrap();
    assert_eq!(
        result.transitive_count, 2,
        "in a 3-node cycle, each node is depended on by the other 2"
    );
}

#[test]
fn test_blast_radius_isolated_node() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    insert_file(&conn, "b.rs");

    let result = blast::blast_radius_for_file(&conn, a).unwrap();
    assert_eq!(result.transitive_count, 0);
    assert!(result.direct_importers.is_empty());
}

#[test]
fn test_blast_radius_self_import() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");

    write::insert_edge(&conn, a, a, "import", None).unwrap(); // self-loop
    write::insert_edge(&conn, b, a, "import", None).unwrap();

    let result = blast::blast_radius_for_file(&conn, a).unwrap();
    // Self-loop should not count as a dependency on itself.
    assert_eq!(result.transitive_count, 1);
    assert!(result.transitive_importers.contains(&b));
}

// ---------------------------------------------------------------------------
// 12. Leiden clustering: edge cases
// ---------------------------------------------------------------------------

#[test]
fn test_leiden_empty_graph() {
    let (assignments, modularity) = leiden::leiden_raw(&[], &[], &leiden::LeidenConfig::default());
    assert!(assignments.is_empty());
    assert_eq!(modularity, 0.0);
}

#[test]
fn test_leiden_single_node() {
    let (assignments, _) = leiden::leiden_raw(&[1], &[], &leiden::LeidenConfig::default());
    assert_eq!(assignments.len(), 1);
    assert!(assignments.contains_key(&1));
}

#[test]
fn test_leiden_two_disconnected_cliques() {
    // Clique 1: 1-2-3, Clique 2: 4-5-6
    let nodes: Vec<i64> = (1..=6).collect();
    let edges = vec![
        (1, 2),
        (2, 3),
        (1, 3), // clique 1
        (4, 5),
        (5, 6),
        (4, 6), // clique 2
    ];
    let (assignments, modularity) =
        leiden::leiden_raw(&nodes, &edges, &leiden::LeidenConfig::default());

    assert_eq!(assignments.len(), 6);
    // Nodes within each clique should share a cluster.
    assert_eq!(assignments[&1], assignments[&2]);
    assert_eq!(assignments[&2], assignments[&3]);
    assert_eq!(assignments[&4], assignments[&5]);
    assert_eq!(assignments[&5], assignments[&6]);
    // The two cliques should be in different clusters.
    assert_ne!(assignments[&1], assignments[&4]);
    assert!(
        modularity > 0.0,
        "non-trivial graph should have positive modularity"
    );
}

#[test]
fn test_leiden_deterministic() {
    let nodes: Vec<i64> = (1..=8).collect();
    let edges = vec![
        (1, 2),
        (2, 3),
        (1, 3),
        (4, 5),
        (5, 6),
        (4, 6),
        (7, 8),
        (3, 4), // bridge
    ];

    let (a1, _) = leiden::leiden_raw(&nodes, &edges, &leiden::LeidenConfig::default());
    let (a2, _) = leiden::leiden_raw(&nodes, &edges, &leiden::LeidenConfig::default());

    assert_eq!(a1, a2, "identical input should produce identical output");
}

#[test]
fn test_leiden_compute_clusters_writes_to_db() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");
    let c = insert_file(&conn, "c.rs");
    write::insert_edge(&conn, a, b, "import", None).unwrap();
    write::insert_edge(&conn, b, c, "import", None).unwrap();

    let report = leiden::compute_clusters(&conn, &leiden::LeidenConfig::default()).unwrap();
    assert!(!report.assignments.is_empty());

    let cluster_count = read::get_file_clusters_count(&conn).unwrap();
    assert!(cluster_count > 0, "clusters should be persisted to DB");
}

// ---------------------------------------------------------------------------
// 13. Symbol references: multiple referrers, no match
// ---------------------------------------------------------------------------

#[test]
fn test_symbol_refs_multiple_referrers() {
    let conn = setup();
    let lib = insert_file(&conn, "src/lib.rs");
    let a = insert_file(&conn, "src/a.rs");
    let b = insert_file(&conn, "src/b.rs");

    let lib_ids =
        write::insert_symbols(&conn, lib, &[sym("SharedType", "struct", 1, 10, true)]).unwrap();
    let a_ids = write::insert_symbols(&conn, a, &[sym("fn_a", "function", 1, 5, false)]).unwrap();
    let b_ids = write::insert_symbols(&conn, b, &[sym("fn_b", "function", 1, 5, false)]).unwrap();

    write::insert_symbol_refs(
        &conn,
        &[
            (a_ids[0], lib_ids[0], "type"),
            (b_ids[0], lib_ids[0], "call"),
        ],
    )
    .unwrap();

    let refs = read::get_symbol_references(&conn, "SharedType").unwrap();
    assert_eq!(refs.len(), 1, "one definition of SharedType");

    let (_, _, importers) = &refs[0];
    assert_eq!(importers.len(), 2, "two files reference SharedType");

    let importer_paths: HashSet<&str> = importers.iter().map(|(_, f)| f.path.as_str()).collect();
    assert!(importer_paths.contains("src/a.rs"));
    assert!(importer_paths.contains("src/b.rs"));
}

#[test]
fn test_symbol_refs_no_match() {
    let conn = setup();
    let refs = read::get_symbol_references(&conn, "NonExistent").unwrap();
    assert!(refs.is_empty());
}

// ---------------------------------------------------------------------------
// 14. Language stats aggregation
// ---------------------------------------------------------------------------

#[test]
fn test_language_stats_aggregation() {
    let conn = setup();
    write::upsert_file(&conn, "a.rs", 1000, 500, "rust", 50).unwrap();
    write::upsert_file(&conn, "b.rs", 1000, 300, "rust", 30).unwrap();
    write::upsert_file(&conn, "c.ts", 1000, 200, "typescript", 20).unwrap();

    let stats = read::get_language_stats(&conn).unwrap();
    assert!(stats.len() >= 2);

    let rust_stat = stats.iter().find(|s| s.language == "rust").unwrap();
    assert_eq!(rust_stat.file_count, 2);
    assert_eq!(rust_stat.line_count, 80);
    assert_eq!(rust_stat.byte_count, 800);
}

#[test]
fn test_language_stats_empty_db() {
    let conn = setup();
    let stats = read::get_language_stats(&conn).unwrap();
    assert!(stats.is_empty());
}

// ---------------------------------------------------------------------------
// 15. Symbol ranked queries
// ---------------------------------------------------------------------------

#[test]
fn test_symbols_ranked_for_file_order() {
    let conn = setup();
    let f = insert_file(&conn, "src/lib.rs");

    let ids = write::insert_symbols(
        &conn,
        f,
        &[
            sym("low_rank", "function", 1, 5, true),
            sym("high_rank", "function", 7, 10, true),
        ],
    )
    .unwrap();

    write::update_symbol_pagerank(&conn, ids[0], 0.1).unwrap();
    write::update_symbol_pagerank(&conn, ids[1], 0.9).unwrap();

    let ranked = read::get_symbols_ranked_for_file(&conn, f, 10).unwrap();
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].name, "high_rank");
    assert_eq!(ranked[1].name, "low_rank");
}

#[test]
fn test_symbols_ranked_global() {
    let conn = setup();
    let f1 = insert_file(&conn, "a.rs");
    let f2 = insert_file(&conn, "b.rs");

    let ids1 = write::insert_symbols(&conn, f1, &[sym("top", "struct", 1, 5, true)]).unwrap();
    let ids2 = write::insert_symbols(&conn, f2, &[sym("bottom", "function", 1, 3, true)]).unwrap();

    write::update_symbol_pagerank(&conn, ids1[0], 0.8).unwrap();
    write::update_symbol_pagerank(&conn, ids2[0], 0.2).unwrap();

    let ranked = read::get_symbols_ranked(&conn, 10).unwrap();
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].0.name, "top");
    assert_eq!(ranked[1].0.name, "bottom");
}

// ---------------------------------------------------------------------------
// 16. Most imported files
// ---------------------------------------------------------------------------

#[test]
fn test_most_imported_files_ordering() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");
    let c = insert_file(&conn, "c.rs");
    let d = insert_file(&conn, "d.rs");

    write::insert_edge(&conn, a, c, "import", None).unwrap();
    write::insert_edge(&conn, b, c, "import", None).unwrap();
    write::insert_edge(&conn, a, d, "import", None).unwrap();

    let most = read::get_most_imported_files(&conn, 10).unwrap();
    assert!(most.len() >= 2);
    assert_eq!(most[0].0.path, "c.rs");
    assert_eq!(most[0].1, 2);
}

// ---------------------------------------------------------------------------
// 17. File CRUD
// ---------------------------------------------------------------------------

#[test]
fn test_get_file_by_path_not_found() {
    let conn = setup();
    assert!(read::get_file_by_path(&conn, "nope.rs").unwrap().is_none());
}

#[test]
fn test_get_file_by_id_not_found() {
    let conn = setup();
    assert!(read::get_file_by_id(&conn, 99999).unwrap().is_none());
}

#[test]
fn test_upsert_file_preserves_id_on_update() {
    let conn = setup();
    let id1 = write::upsert_file(&conn, "a.rs", 1000, 100, "rust", 10).unwrap();
    let id2 = write::upsert_file(&conn, "a.rs", 2000, 200, "rust", 20).unwrap();
    assert_eq!(id1, id2);

    let file = read::get_file_by_id(&conn, id1).unwrap().unwrap();
    assert_eq!(file.size_bytes, 200);
    assert_eq!(file.line_count, 20);
    assert_eq!(file.mtime_ns, 2000);
}

// ---------------------------------------------------------------------------
// 18. Edge queries
// ---------------------------------------------------------------------------

#[test]
fn test_edge_count() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");
    let c = insert_file(&conn, "c.rs");

    write::insert_edge(&conn, a, b, "import", None).unwrap();
    write::insert_edge(&conn, a, c, "import", None).unwrap();

    assert_eq!(read::get_edge_count(&conn).unwrap(), 2);
}

#[test]
fn test_edges_from_and_to() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");
    let c = insert_file(&conn, "c.rs");

    write::insert_edge(&conn, a, c, "import", Some("crate::c")).unwrap();
    write::insert_edge(&conn, b, c, "import", Some("crate::c")).unwrap();

    let to_c = read::get_edges_to(&conn, c).unwrap();
    assert_eq!(to_c.len(), 2);

    let from_a = read::get_edges_from(&conn, a).unwrap();
    assert_eq!(from_a.len(), 1);
    assert_eq!(from_a[0].to_file, c);
    assert_eq!(from_a[0].specifier.as_deref(), Some("crate::c"));
}

// ---------------------------------------------------------------------------
// 19. Delete file cascades
// ---------------------------------------------------------------------------

#[test]
fn test_delete_file_cascades_symbol_refs() {
    let conn = setup();
    let f1 = insert_file(&conn, "a.rs");
    let f2 = insert_file(&conn, "b.rs");

    let ids1 = write::insert_symbols(&conn, f1, &[sym("target", "struct", 1, 5, true)]).unwrap();
    let ids2 = write::insert_symbols(&conn, f2, &[sym("caller", "function", 1, 3, false)]).unwrap();

    write::insert_symbol_refs(&conn, &[(ids2[0], ids1[0], "call")]).unwrap();
    write::insert_edge(&conn, f2, f1, "import", None).unwrap();

    write::delete_file_data(&conn, f1).unwrap();

    // Symbols, edges, and symbol_refs should all be gone.
    assert_eq!(read::get_symbols_for_file(&conn, f1).unwrap().len(), 0);
    assert!(read::get_all_edges(&conn).unwrap().is_empty());
    assert_eq!(read::get_all_symbol_refs(&conn).unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// 20. Stale files detection
// ---------------------------------------------------------------------------

#[test]
fn test_stale_files_detection() {
    let conn = setup();
    let indexed = insert_file(&conn, "indexed.rs");
    insert_file(&conn, "stale.rs");

    write::insert_symbols(&conn, indexed, &[sym("main", "function", 1, 5, false)]).unwrap();

    let stale = read::get_stale_files(&conn).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].path, "stale.rs");
}

// ---------------------------------------------------------------------------
// 21. FTS body search (requires disk files)
// ---------------------------------------------------------------------------

#[test]
fn test_rebuild_symbol_bodies_and_search() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub fn unique_marker_xyz() {\n    let x = 42;\n    println!(\"hello\");\n}\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    write::rebuild_symbol_bodies(&conn, dir.path()).unwrap();

    let results = read::search_symbol_bodies_fts(&conn, "unique_marker_xyz", 10).unwrap();
    assert!(
        !results.is_empty(),
        "body search should find symbols containing the marker"
    );
    assert_eq!(results[0].0.name, "unique_marker_xyz");
}

// ---------------------------------------------------------------------------
// 22. Wiki rendering (end-to-end)
// ---------------------------------------------------------------------------

#[test]
fn test_wiki_render_with_clusters() {
    let conn = setup();
    let a = insert_file(&conn, "src/server/handler.rs");
    let b = insert_file(&conn, "src/server/router.rs");
    let c = insert_file(&conn, "src/storage/db.rs");

    write::insert_symbols(&conn, a, &[sym("handle", "function", 1, 10, true)]).unwrap();
    write::insert_symbols(&conn, b, &[sym("route", "function", 1, 5, true)]).unwrap();
    write::insert_symbols(&conn, c, &[sym("query", "function", 1, 5, true)]).unwrap();

    write::insert_edge(&conn, a, b, "import", None).unwrap();
    write::insert_edge(&conn, a, c, "import", None).unwrap();

    pagerank::compute_pagerank(&conn, &pagerank::PageRankConfig::default()).unwrap();

    let config = wiki::WikiConfig {
        project_name: "TestProject".to_string(),
        recompute: true,
        ..Default::default()
    };
    let (output, modularity) = wiki::render_wiki(&conn, &config).unwrap();

    assert!(output.contains("TestProject"));
    assert!(output.contains("Table of contents"));
    assert!(modularity.is_some());
}

#[test]
fn test_wiki_render_empty_db() {
    let conn = setup();

    let config = wiki::WikiConfig {
        project_name: "Empty".to_string(),
        recompute: true,
        ..Default::default()
    };
    let (output, _) = wiki::render_wiki(&conn, &config).unwrap();
    assert!(output.contains("Empty"));
}

// ---------------------------------------------------------------------------
// 23. End-to-end: index → pagerank → blast → unused → wiki
// ---------------------------------------------------------------------------

#[test]
fn test_end_to_end_pipeline() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("core.ts"),
        "export function coreUtil() { return 42; }\n\
         export function coreHelper() { return 0; }\n",
    )
    .unwrap();
    fs::write(
        src.join("service.ts"),
        "import { coreUtil } from './core';\n\
         export function serve() { return coreUtil(); }\n",
    )
    .unwrap();
    fs::write(
        src.join("handler.ts"),
        "import { serve } from './service';\n\
         export function handle() { serve(); }\n",
    )
    .unwrap();

    let conn = setup();

    // 1. Index
    index::full_index(&conn, dir.path(), false).unwrap();
    assert_eq!(read::get_file_count(&conn).unwrap(), 3);

    // 2. PageRank
    pagerank::compute_pagerank(&conn, &pagerank::PageRankConfig::default()).unwrap();
    let ranked = read::get_files_ranked(&conn, 10).unwrap();
    assert!(ranked[0].pagerank > 0.0);

    // 3. Blast radius
    let core = read::get_file_by_path(&conn, "src/core.ts")
        .unwrap()
        .unwrap();
    let blast_result = blast::blast_radius_for_file(&conn, core.id).unwrap();
    assert!(
        blast_result.transitive_count >= 2,
        "core.ts should have blast radius >= 2"
    );

    // 4. Unused exports
    write::populate_unused_exports(&conn).unwrap();
    let count = read::count_unused_exports(&conn).unwrap();
    // coreHelper is exported but not imported by anyone.
    assert!(
        count >= 1,
        "should have at least one unused export (coreHelper)"
    );

    // 5. Wiki
    let config = wiki::WikiConfig {
        project_name: "E2E".to_string(),
        recompute: true,
        ..Default::default()
    };
    let (wiki, _) = wiki::render_wiki(&conn, &config).unwrap();
    assert!(wiki.contains("E2E"));
}

// ---------------------------------------------------------------------------
// 24. File clusters
// ---------------------------------------------------------------------------

#[test]
fn test_file_clusters_crud() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");

    write::upsert_file_cluster(&conn, a, 1, 1000).unwrap();
    write::upsert_file_cluster(&conn, b, 2, 1000).unwrap();

    let clusters = read::get_all_file_clusters(&conn).unwrap();
    assert_eq!(clusters.len(), 2);

    let count = read::get_file_clusters_count(&conn).unwrap();
    assert_eq!(count, 2);

    write::clear_file_clusters(&conn).unwrap();
    let after = read::get_file_clusters_count(&conn).unwrap();
    assert_eq!(after, 0);
}

// ---------------------------------------------------------------------------
// 25. Meta key/value
// ---------------------------------------------------------------------------

#[test]
fn test_meta_overwrite() {
    let conn = setup();
    write::set_meta(&conn, "key", "v1").unwrap();
    write::set_meta(&conn, "key", "v2").unwrap();
    assert_eq!(
        read::get_meta(&conn, "key").unwrap(),
        Some("v2".to_string())
    );
}

#[test]
fn test_meta_multiple_keys() {
    let conn = setup();
    write::set_meta(&conn, "a", "1").unwrap();
    write::set_meta(&conn, "b", "2").unwrap();

    assert_eq!(read::get_meta(&conn, "a").unwrap(), Some("1".to_string()));
    assert_eq!(read::get_meta(&conn, "b").unwrap(), Some("2".to_string()));
    assert!(read::get_meta(&conn, "c").unwrap().is_none());
}

// ---------------------------------------------------------------------------
// 26. find_symbol_by_name: duplicate names across files
// ---------------------------------------------------------------------------

#[test]
fn test_find_symbol_by_name_returns_all_definitions() {
    let conn = setup();
    let f1 = insert_file(&conn, "src/a.rs");
    let f2 = insert_file(&conn, "src/b.rs");
    let f3 = insert_file(&conn, "src/c.rs");

    write::insert_symbols(&conn, f1, &[sym("process", "function", 1, 5, true)]).unwrap();
    write::insert_symbols(&conn, f2, &[sym("process", "function", 1, 3, false)]).unwrap();
    write::insert_symbols(&conn, f3, &[sym("unrelated", "function", 1, 3, true)]).unwrap();

    let results = read::find_symbol_by_name(&conn, "process").unwrap();
    assert_eq!(results.len(), 2);

    let paths: HashSet<&str> = results.iter().map(|(_, f)| f.path.as_str()).collect();
    assert!(paths.contains("src/a.rs"));
    assert!(paths.contains("src/b.rs"));
}

// ---------------------------------------------------------------------------
// 27. get_all_symbols_with_path
// ---------------------------------------------------------------------------

#[test]
fn test_all_symbols_with_path() {
    let conn = setup();
    let f1 = insert_file(&conn, "src/a.rs");
    let f2 = insert_file(&conn, "src/b.rs");

    write::insert_symbols(&conn, f1, &[sym("foo", "function", 1, 5, true)]).unwrap();
    write::insert_symbols(&conn, f2, &[sym("bar", "struct", 1, 3, true)]).unwrap();

    let all = read::get_all_symbols_with_path(&conn).unwrap();
    assert_eq!(all.len(), 2);

    let paths: HashSet<&str> = all.iter().map(|(_, p)| p.as_str()).collect();
    assert!(paths.contains("src/a.rs"));
    assert!(paths.contains("src/b.rs"));
}

// ---------------------------------------------------------------------------
// 28. Pagination in unused exports
// ---------------------------------------------------------------------------

#[test]
fn test_unused_exports_pagination() {
    let conn = setup();
    for i in 0..5 {
        let f = insert_file(&conn, &format!("src/f{i}.rs"));
        write::insert_symbols(
            &conn,
            f,
            &[sym(&format!("sym_{i}"), "function", 1, 5, true)],
        )
        .unwrap();
    }

    write::populate_unused_exports(&conn).unwrap();

    let page1 = read::get_unused_exports_page(&conn, 2, 0).unwrap();
    let page2 = read::get_unused_exports_page(&conn, 2, 2).unwrap();
    let page3 = read::get_unused_exports_page(&conn, 2, 4).unwrap();

    assert_eq!(page1.len(), 2);
    assert_eq!(page2.len(), 2);
    assert_eq!(page3.len(), 1);

    // No overlap between pages.
    let names1: HashSet<&str> = page1.iter().map(|(s, _)| s.name.as_str()).collect();
    let names2: HashSet<&str> = page2.iter().map(|(s, _)| s.name.as_str()).collect();
    assert!(names1.is_disjoint(&names2), "pages should not overlap");
}

// ---------------------------------------------------------------------------
// 29. FTS search_file_ids_by_fts
// ---------------------------------------------------------------------------

#[test]
fn test_search_file_ids_by_fts() {
    let conn = setup();
    let f = insert_file(&conn, "src/db.rs");
    write::insert_symbols(
        &conn,
        f,
        &[
            sym("DatabasePool", "struct", 1, 5, true),
            sym("DatabaseConfig", "struct", 7, 10, true),
        ],
    )
    .unwrap();
    write::sync_fts(&conn).unwrap();

    let ids = read::search_file_ids_by_fts(&conn, "Database*").unwrap();
    assert!(!ids.is_empty());
    assert!(ids.contains(&f));
}

// ---------------------------------------------------------------------------
// 30. End-to-end indexing across multiple languages
// ---------------------------------------------------------------------------

#[test]
fn test_index_mixed_languages() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(src.join("lib.rs"), "pub fn rust_fn() -> i32 { 1 }\n").unwrap();
    fs::write(
        src.join("app.ts"),
        "export function tsFn(): number { return 1; }\n",
    )
    .unwrap();
    fs::write(src.join("utils.py"), "def py_fn():\n    return 1\n").unwrap();
    fs::write(
        src.join("main.go"),
        "package main\n\nfunc GoFn() int {\n    return 1\n}\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let stats = read::get_language_stats(&conn).unwrap();
    let languages: HashSet<&str> = stats.iter().map(|s| s.language.as_str()).collect();

    assert!(languages.contains("rust"), "should detect Rust");
    assert!(languages.contains("typescript"), "should detect TypeScript");
    assert!(languages.contains("python"), "should detect Python");
    assert!(languages.contains("go"), "should detect Go");
}

// ---------------------------------------------------------------------------
// 31. Schema is idempotent
// ---------------------------------------------------------------------------

#[test]
fn test_schema_double_create_idempotent() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    schema::create_schema(&conn).unwrap();

    let f = write::upsert_file(&conn, "a.rs", 1000, 100, "rust", 10).unwrap();
    assert!(f > 0);
}

// ---------------------------------------------------------------------------
// 32. get_or_create_file: idempotent
// ---------------------------------------------------------------------------

#[test]
fn test_get_or_create_file_idempotent() {
    let conn = setup();

    let id1 = write::get_or_create_file(&conn, "src/new.rs").unwrap();
    let id2 = write::get_or_create_file(&conn, "src/new.rs").unwrap();
    assert_eq!(id1, id2);

    let file = read::get_file_by_path(&conn, "src/new.rs")
        .unwrap()
        .unwrap();
    assert_eq!(file.language, "rust");
}

// ---------------------------------------------------------------------------
// 33. Blast radius for multiple files
// ---------------------------------------------------------------------------

#[test]
fn test_blast_radius_for_files_batch() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");
    let c = insert_file(&conn, "c.rs");

    write::insert_edge(&conn, a, c, "import", None).unwrap();
    write::insert_edge(&conn, b, c, "import", None).unwrap();

    let radii = blast::blast_radius_for_files(&conn, &[a, b, c]).unwrap();
    assert_eq!(radii[&c], 2);
    assert_eq!(radii[&a], 0);
    assert_eq!(radii[&b], 0);
}

#[test]
fn test_blast_radius_for_files_empty() {
    let conn = setup();
    let radii = blast::blast_radius_for_files(&conn, &[]).unwrap();
    assert!(radii.is_empty());
}

// ---------------------------------------------------------------------------
// 34. Symbol pagerank computation
// ---------------------------------------------------------------------------

#[test]
fn test_symbol_pagerank_computation() {
    let conn = setup();
    let f1 = insert_file(&conn, "a.rs");
    let f2 = insert_file(&conn, "b.rs");

    let ids1 = write::insert_symbols(&conn, f1, &[sym("callee", "function", 1, 5, true)]).unwrap();
    let ids2 = write::insert_symbols(
        &conn,
        f2,
        &[
            sym("caller1", "function", 1, 3, false),
            sym("caller2", "function", 5, 8, false),
        ],
    )
    .unwrap();

    write::insert_symbol_refs(
        &conn,
        &[(ids2[0], ids1[0], "call"), (ids2[1], ids1[0], "call")],
    )
    .unwrap();

    pagerank::compute_symbol_pagerank(&conn, &pagerank::PageRankConfig::default()).unwrap();

    let all = read::get_all_symbols(&conn).unwrap();
    let callee = all.iter().find(|s| s.name == "callee").unwrap();
    let caller1 = all.iter().find(|s| s.name == "caller1").unwrap();

    assert!(
        callee.pagerank > caller1.pagerank,
        "callee (referenced 2x) should rank higher than caller1"
    );
}

// ---------------------------------------------------------------------------
// 35. Unused exports: symbol referenced via symbol_refs should NOT be unused
// ---------------------------------------------------------------------------

#[test]
fn test_populate_unused_exports_excludes_symbol_refs() {
    let conn = setup();
    let lib = insert_file(&conn, "src/lib.rs");
    let consumer = insert_file(&conn, "src/consumer.rs");

    let lib_ids = write::insert_symbols(
        &conn,
        lib,
        &[
            sym("Referenced", "struct", 1, 5, true),
            sym("Orphan", "function", 7, 10, true),
        ],
    )
    .unwrap();

    let consumer_ids =
        write::insert_symbols(&conn, consumer, &[sym("user", "function", 1, 3, false)]).unwrap();

    // Symbol-level ref on Referenced, no ref on Orphan.
    write::insert_symbol_refs(&conn, &[(consumer_ids[0], lib_ids[0], "type")]).unwrap();

    write::populate_unused_exports(&conn).unwrap();

    let unused = read::get_unused_exports_page(&conn, i64::MAX, 0).unwrap();
    let names: Vec<&str> = unused.iter().map(|(s, _)| s.name.as_str()).collect();

    assert!(
        !names.contains(&"Referenced"),
        "symbol with symbol_refs should NOT be unused"
    );
    assert!(
        names.contains(&"Orphan"),
        "symbol with no refs should be unused"
    );
}

// ---------------------------------------------------------------------------
// 36. Sync FTS after symbol insertion
// ---------------------------------------------------------------------------

#[test]
fn test_sync_fts_after_symbol_changes() {
    let conn = setup();
    let f = insert_file(&conn, "src/lib.rs");

    write::insert_symbols(
        &conn,
        f,
        &[
            sym("AlphaSymbol", "struct", 1, 5, true),
            sym("BetaSymbol", "function", 7, 10, true),
        ],
    )
    .unwrap();
    write::sync_fts(&conn).unwrap();

    let alpha = read::search_symbols_fts(&conn, "AlphaSymbol", 10).unwrap();
    assert_eq!(alpha.len(), 1);

    let beta = read::search_symbols_fts(&conn, "BetaSymbol", 10).unwrap();
    assert_eq!(beta.len(), 1);

    // Prefix search.
    let prefix = read::search_symbols_fts(&conn, "Alpha*", 10).unwrap();
    assert_eq!(prefix.len(), 1);
}

// ---------------------------------------------------------------------------
// 37. Duplicate edge insertion is idempotent
// ---------------------------------------------------------------------------

#[test]
fn test_duplicate_edge_is_idempotent() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");

    write::insert_edge(&conn, a, b, "import", Some("crate::b")).unwrap();
    write::insert_edge(&conn, a, b, "import", Some("crate::b")).unwrap();
    write::insert_edge(&conn, a, b, "import", Some("crate::b")).unwrap();

    assert_eq!(read::get_all_edges(&conn).unwrap().len(), 1);
    assert_eq!(read::get_edge_count(&conn).unwrap(), 1);
}

// ===========================================================================
// 38. rebuild_symbol_bodies: single-line symbol
// ===========================================================================

#[test]
fn test_rebuild_bodies_single_line_symbol() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub const X: i32 = 42;\n").unwrap();

    let conn = setup();
    let f = write::upsert_file(&conn, "src/lib.rs", 1000, 100, "rust", 1).unwrap();
    write::insert_symbols(&conn, f, &[sym("X", "constant", 1, 1, true)]).unwrap();

    write::rebuild_symbol_bodies(&conn, dir.path()).unwrap();

    let results = read::search_symbol_bodies_fts(&conn, "X", 10).unwrap();
    assert!(
        !results.is_empty(),
        "single-line symbol body should be indexed"
    );
    assert!(
        results[0].0.name == "X",
        "body search should return the single-line constant"
    );
}

// ===========================================================================
// 39. rebuild_symbol_bodies: line range past end of file
// ===========================================================================

#[test]
fn test_rebuild_bodies_line_range_past_eof() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("short.rs"), "line1\nline2\n").unwrap();

    let conn = setup();
    let f = write::upsert_file(&conn, "src/short.rs", 1000, 100, "rust", 2).unwrap();
    // Symbol claims to span lines 1-100, but file only has 2 lines.
    write::insert_symbols(&conn, f, &[sym("overflow", "function", 1, 100, true)]).unwrap();

    write::rebuild_symbol_bodies(&conn, dir.path()).unwrap();

    let results = read::search_symbol_bodies_fts(&conn, "line1", 10).unwrap();
    assert!(
        !results.is_empty(),
        "symbol with line range past EOF should still index available lines"
    );
}

// ===========================================================================
// 40. rebuild_symbol_bodies: file missing from disk
// ===========================================================================

#[test]
fn test_rebuild_bodies_missing_file_skipped() {
    let dir = TempDir::new().unwrap();

    let conn = setup();
    let f = write::upsert_file(&conn, "src/gone.rs", 1000, 100, "rust", 10).unwrap();
    write::insert_symbols(&conn, f, &[sym("ghost", "function", 1, 5, true)]).unwrap();

    // File does not exist on disk — should not panic.
    write::rebuild_symbol_bodies(&conn, dir.path()).unwrap();

    let results = read::search_symbol_bodies_fts(&conn, "ghost", 10).unwrap();
    assert!(
        results.is_empty(),
        "missing file should be silently skipped"
    );
}

// ===========================================================================
// 41. rebuild_symbol_bodies: empty file
// ===========================================================================

#[test]
fn test_rebuild_bodies_empty_file() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("empty.rs"), "").unwrap();

    let conn = setup();
    let f = write::upsert_file(&conn, "src/empty.rs", 1000, 0, "rust", 0).unwrap();
    write::insert_symbols(&conn, f, &[sym("phantom", "function", 1, 1, true)]).unwrap();

    write::rebuild_symbol_bodies(&conn, dir.path()).unwrap();

    let results = read::search_symbol_bodies_fts(&conn, "phantom", 10).unwrap();
    assert!(results.is_empty(), "empty file has no lines to extract");
}

// ===========================================================================
// 42. rebuild_symbol_bodies: multi-byte UTF-8 content
// ===========================================================================

#[test]
fn test_rebuild_bodies_multibyte_utf8() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("unicode.rs"),
        "// 日本語コメント\npub fn grüße() -> &'static str { \"héllo\" }\n",
    )
    .unwrap();

    let conn = setup();
    let f = write::upsert_file(&conn, "src/unicode.rs", 1000, 200, "rust", 2).unwrap();
    write::insert_symbols(&conn, f, &[sym("grüße", "function", 2, 2, true)]).unwrap();

    write::rebuild_symbol_bodies(&conn, dir.path()).unwrap();

    // The function body should be indexed correctly despite multi-byte chars.
    let results = read::search_symbol_bodies_fts(&conn, "héllo", 10).unwrap();
    assert!(
        !results.is_empty(),
        "multi-byte UTF-8 content should be searchable in body FTS"
    );
}

// ===========================================================================
// 43. rebuild_symbol_bodies: line_start=0 treated same as line_start=1
// ===========================================================================

#[test]
fn test_rebuild_bodies_line_start_zero() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("zero.rs"), "first_line\nsecond_line\n").unwrap();

    let conn = setup();
    let f = write::upsert_file(&conn, "src/zero.rs", 1000, 100, "rust", 2).unwrap();
    // line_start=0 is invalid (1-indexed), but saturating_sub(1) maps it to 0.
    write::insert_symbols(&conn, f, &[sym("zero_start", "function", 0, 1, true)]).unwrap();

    write::rebuild_symbol_bodies(&conn, dir.path()).unwrap();

    let results = read::search_symbol_bodies_fts(&conn, "first_line", 10).unwrap();
    assert!(
        !results.is_empty(),
        "line_start=0 should map to the first line via saturating_sub"
    );
}

// ===========================================================================
// 44. rebuild_symbol_bodies: file without trailing newline
// ===========================================================================

#[test]
fn test_rebuild_bodies_no_trailing_newline() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("noeol.rs"), "fn no_newline() {}").unwrap(); // no \n

    let conn = setup();
    let f = write::upsert_file(&conn, "src/noeol.rs", 1000, 100, "rust", 1).unwrap();
    write::insert_symbols(&conn, f, &[sym("no_newline", "function", 1, 1, true)]).unwrap();

    write::rebuild_symbol_bodies(&conn, dir.path()).unwrap();

    let results = read::search_symbol_bodies_fts(&conn, "no_newline", 10).unwrap();
    assert!(
        !results.is_empty(),
        "file without trailing newline should still have its body indexed"
    );
}

// ===========================================================================
// 45. find_file_paths_by_body_text: basic matching
// ===========================================================================

#[test]
fn test_find_file_paths_by_body_text() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("db.rs"),
        "pub fn unique_database_marker() { todo!() }\n",
    )
    .unwrap();
    fs::write(src.join("web.rs"), "pub fn handle_request() { todo!() }\n").unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();
    write::rebuild_symbol_bodies(&conn, dir.path()).unwrap();

    let paths = read::find_file_paths_by_body_text(&conn, "unique_database_marker").unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], "src/db.rs");
}

// ===========================================================================
// 46. find_file_paths_by_body_text: no match
// ===========================================================================

#[test]
fn test_find_file_paths_by_body_text_no_match() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub fn hello() {}\n").unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();
    write::rebuild_symbol_bodies(&conn, dir.path()).unwrap();

    let paths = read::find_file_paths_by_body_text(&conn, "nonexistent_token_xyz").unwrap();
    assert!(paths.is_empty());
}

// ===========================================================================
// 47. populate_unused_exports is idempotent
// ===========================================================================

#[test]
fn test_populate_unused_exports_idempotent() {
    let conn = setup();
    let f1 = insert_file(&conn, "src/a.rs");
    let f2 = insert_file(&conn, "src/b.rs");

    write::insert_symbols(&conn, f1, &[sym("used", "function", 1, 5, true)]).unwrap();
    write::insert_symbols(&conn, f2, &[sym("unused", "function", 1, 5, true)]).unwrap();
    write::insert_edge(&conn, f2, f1, "import", None).unwrap();

    write::populate_unused_exports(&conn).unwrap();
    let first = read::get_unused_exports_page(&conn, i64::MAX, 0).unwrap();

    write::populate_unused_exports(&conn).unwrap();
    let second = read::get_unused_exports_page(&conn, i64::MAX, 0).unwrap();

    let first_names: Vec<&str> = first.iter().map(|(s, _)| s.name.as_str()).collect();
    let second_names: Vec<&str> = second.iter().map(|(s, _)| s.name.as_str()).collect();
    assert_eq!(
        first_names, second_names,
        "calling populate_unused_exports twice must yield identical results"
    );
}

// ===========================================================================
// 48. compute_blast_radius (whole-graph) vs blast_radius_for_file consistency
// ===========================================================================

#[test]
fn test_blast_radius_whole_graph_vs_per_file() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");
    let c = insert_file(&conn, "c.rs");
    let d = insert_file(&conn, "d.rs");

    write::insert_edge(&conn, a, b, "import", None).unwrap();
    write::insert_edge(&conn, b, c, "import", None).unwrap();
    write::insert_edge(&conn, a, c, "import", None).unwrap();
    write::insert_edge(&conn, d, c, "import", None).unwrap();

    let whole = blast::compute_blast_radius(&conn).unwrap();

    for &file_id in &[a, b, c, d] {
        let per_file = blast::blast_radius_for_file(&conn, file_id).unwrap();
        let whole_count = whole.get(&file_id).copied().unwrap_or(0);
        assert_eq!(
            per_file.transitive_count as i64, whole_count,
            "blast radius mismatch for file {file_id}: per_file={}, whole_graph={whole_count}",
            per_file.transitive_count
        );
    }
}

// ===========================================================================
// 49. blast_radius_for_files: diamond dependency
// ===========================================================================

#[test]
fn test_blast_radius_diamond_dependency() {
    let conn = setup();
    let top = insert_file(&conn, "top.rs");
    let left = insert_file(&conn, "left.rs");
    let right = insert_file(&conn, "right.rs");
    let bottom = insert_file(&conn, "bottom.rs");

    // Diamond: top → left → bottom, top → right → bottom
    write::insert_edge(&conn, top, left, "import", None).unwrap();
    write::insert_edge(&conn, top, right, "import", None).unwrap();
    write::insert_edge(&conn, left, bottom, "import", None).unwrap();
    write::insert_edge(&conn, right, bottom, "import", None).unwrap();

    let result = blast::blast_radius_for_file(&conn, bottom).unwrap();
    assert_eq!(
        result.transitive_count, 3,
        "bottom of diamond should have blast radius 3 (left, right, top)"
    );
    assert_eq!(
        result.direct_importers.len(),
        2,
        "left and right import bottom directly"
    );
}

// ===========================================================================
// 50. analyze_cochanges: file count filtering
// ===========================================================================

#[test]
fn test_analyze_cochanges_file_count_filtering() {
    let dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();

    // Helper to create commits with specific file sets.
    let make_commit = |repo: &git2::Repository, files: &[&str], msg: &str| {
        for f in files {
            let path = dir.path().join(f);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, format!("content of {f}")).unwrap();
        }
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let parents: Vec<git2::Commit> = match repo.head() {
            Ok(head) => vec![head.peel_to_commit().unwrap()],
            Err(_) => vec![],
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parent_refs)
            .unwrap();
    };

    // Commit 1: 2 files (within default min=2, max=20).
    make_commit(&repo, &["a.rs", "b.rs"], "two files");
    // Commit 2: 1 file (below min_files=2 → should be filtered).
    make_commit(&repo, &["c.rs"], "one file");

    let conn = setup();
    // Pre-register files as `full_index` would: co-change no longer creates
    // phantom rows, so paths must already exist in `files` to participate.
    write::get_or_create_file(&conn, "a.rs").unwrap();
    write::get_or_create_file(&conn, "b.rs").unwrap();
    write::get_or_create_file(&conn, "c.rs").unwrap();
    let config = CoChangeConfig {
        commit_limit: 100,
        min_files: 2,
        max_files: 5,
    };
    analyze_cochanges(&conn, dir.path(), &config).unwrap();

    // Only the 2-file commit qualifies; the single-file commit is filtered.
    let a = read::get_file_by_path(&conn, "a.rs").unwrap();
    let b = read::get_file_by_path(&conn, "b.rs").unwrap();
    let c = read::get_file_by_path(&conn, "c.rs").unwrap();

    assert!(a.is_some(), "a.rs should exist from the qualifying commit");
    assert!(b.is_some(), "b.rs should exist from the qualifying commit");
    // c.rs may or may not exist as a file row — but it should NOT have cochanges.
    if let Some(c_row) = c {
        let cochanges = read::get_cochanges(&conn, c_row.id, 10).unwrap();
        assert!(
            cochanges.is_empty(),
            "single-file commit should not produce cochanges"
        );
    }

    let a_cochanges = read::get_cochanges(&conn, a.unwrap().id, 10).unwrap();
    assert_eq!(
        a_cochanges.len(),
        1,
        "a.rs and b.rs should be co-change partners"
    );
}

// ===========================================================================
// 51. analyze_cochanges: dotfile filtering
// ===========================================================================

#[test]
fn test_analyze_cochanges_dotfile_filtered() {
    let dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();

    let make_commit = |repo: &git2::Repository, files: &[&str], msg: &str| {
        for f in files {
            let path = dir.path().join(f);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, format!("content of {f}")).unwrap();
        }
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let parents: Vec<git2::Commit> = match repo.head() {
            Ok(head) => vec![head.peel_to_commit().unwrap()],
            Err(_) => vec![],
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parent_refs)
            .unwrap();
    };

    // Commit with a dotfile (.gitignore) and two normal files.
    // The dotfile should be filtered, leaving 2 normal files as a co-change pair.
    make_commit(
        &repo,
        &["src/main.rs", "src/lib.rs", ".gitignore"],
        "with dotfile",
    );

    let conn = setup();
    analyze_cochanges(&conn, dir.path(), &CoChangeConfig::default()).unwrap();

    // .gitignore should not appear in any co-change pair.
    let gitignore = read::get_file_by_path(&conn, ".gitignore").unwrap();
    assert!(
        gitignore.is_none(),
        "dotfile should not be registered as a co-change partner"
    );
}

// ===========================================================================
// 52. analyze_cochanges: subdirectory dotfile passes through
// ===========================================================================

#[test]
fn test_analyze_cochanges_subdirectory_dotfile_not_filtered() {
    let dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();

    let make_commit = |repo: &git2::Repository, files: &[&str], msg: &str| {
        for f in files {
            let path = dir.path().join(f);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, format!("content of {f}")).unwrap();
        }
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let parents: Vec<git2::Commit> = match repo.head() {
            Ok(head) => vec![head.peel_to_commit().unwrap()],
            Err(_) => vec![],
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parent_refs)
            .unwrap();
    };

    // ".github/ci.yml" — the filename is "ci.yml" (not dotfile), but
    // the DIRECTORY starts with dot. Only the filename is checked.
    make_commit(&repo, &[".github/ci.yml", "src/main.rs"], "github ci");

    let conn = setup();
    // Pre-register files as `full_index` would — co-change now skips paths
    // that aren't in the files table.
    write::get_or_create_file(&conn, ".github/ci.yml").unwrap();
    write::get_or_create_file(&conn, "src/main.rs").unwrap();
    analyze_cochanges(&conn, dir.path(), &CoChangeConfig::default()).unwrap();

    // .github/ci.yml should NOT be filtered because only the filename
    // component is checked for leading dot.
    let ci = read::get_file_by_path(&conn, ".github/ci.yml").unwrap();
    assert!(
        ci.is_some(),
        "file in dot-directory should pass through because only filename is checked"
    );
}

// ===========================================================================
// 53. Leiden: complete graph → single cluster
// ===========================================================================

#[test]
fn test_leiden_complete_graph() {
    let nodes: Vec<i64> = (1..=5).collect();
    let mut edges = Vec::new();
    for i in 1..=5i64 {
        for j in (i + 1)..=5 {
            edges.push((i, j));
        }
    }

    let (assignments, _modularity) =
        leiden::leiden_raw(&nodes, &edges, &leiden::LeidenConfig::default());

    assert_eq!(assignments.len(), 5);
    // In a complete graph, all nodes should end up in a small number of clusters.
    let unique: HashSet<i64> = assignments.values().copied().collect();
    // For a 5-clique, Louvain typically places everything in one cluster.
    assert!(
        unique.len() <= 2,
        "complete graph should have very few clusters, got {}",
        unique.len()
    );
}

// ===========================================================================
// 54. Leiden: star topology
// ===========================================================================

#[test]
fn test_leiden_star_topology() {
    // Hub node 1 connected to leaves 2,3,4,5. No leaf-to-leaf edges.
    let nodes: Vec<i64> = (1..=5).collect();
    let edges = vec![(1, 2), (1, 3), (1, 4), (1, 5)];

    let (assignments, _) = leiden::leiden_raw(&nodes, &edges, &leiden::LeidenConfig::default());

    assert_eq!(assignments.len(), 5);
    // Star graph — Louvain tends to put everything in one cluster.
    // At minimum, check it doesn't panic and produces valid output.
    let unique_clusters: HashSet<i64> = assignments.values().copied().collect();
    assert!(!unique_clusters.is_empty());
}

// ===========================================================================
// 55. Leiden: self-loops only
// ===========================================================================

#[test]
fn test_leiden_self_loops_only() {
    let nodes: Vec<i64> = (1..=3).collect();
    let edges = vec![(1, 1), (2, 2), (3, 3)];

    let (assignments, _) = leiden::leiden_raw(&nodes, &edges, &leiden::LeidenConfig::default());

    assert_eq!(assignments.len(), 3);
    // Self-loops are filtered out in adjacency, so this is like no edges.
    // Each node should be in its own cluster.
    let unique: HashSet<i64> = assignments.values().copied().collect();
    assert_eq!(
        unique.len(),
        3,
        "self-loops-only graph: each node in its own cluster"
    );
}

// ===========================================================================
// 56. Leiden: modularity is non-negative for non-trivial partitions
// ===========================================================================

#[test]
fn test_leiden_modularity_non_negative() {
    let nodes: Vec<i64> = (1..=10).collect();
    let edges = vec![
        (1, 2),
        (2, 3),
        (1, 3),
        (4, 5),
        (5, 6),
        (4, 6),
        (7, 8),
        (8, 9),
        (7, 9),
        (3, 4),
        (6, 7), // bridges
    ];

    let (_, modularity) = leiden::leiden_raw(&nodes, &edges, &leiden::LeidenConfig::default());

    assert!(
        modularity >= 0.0,
        "modularity should be non-negative for community structure, got {modularity}"
    );
}

// ===========================================================================
// 57. Wiki: single-file cluster label
// ===========================================================================

#[test]
fn test_wiki_single_file_cluster() {
    let conn = setup();
    let f = insert_file(&conn, "src/main.rs");
    write::insert_symbols(&conn, f, &[sym("main", "function", 1, 5, true)]).unwrap();

    pagerank::compute_pagerank(&conn, &pagerank::PageRankConfig::default()).unwrap();

    let config = wiki::WikiConfig {
        project_name: "SingleFile".to_string(),
        recompute: true,
        ..Default::default()
    };
    let (output, _) = wiki::render_wiki(&conn, &config).unwrap();

    assert!(output.contains("SingleFile"));
    assert!(
        output.contains("main"),
        "wiki should mention the main function"
    );
}

// ===========================================================================
// 58. Wiki: all files in same directory
// ===========================================================================

#[test]
fn test_wiki_same_directory_files() {
    let conn = setup();
    for name in &["a.rs", "b.rs", "c.rs"] {
        let path = format!("src/storage/{name}");
        let f = insert_file(&conn, &path);
        write::insert_symbols(&conn, f, &[sym(name, "struct", 1, 5, true)]).unwrap();
    }

    pagerank::compute_pagerank(&conn, &pagerank::PageRankConfig::default()).unwrap();

    let config = wiki::WikiConfig {
        project_name: "SameDir".to_string(),
        recompute: true,
        ..Default::default()
    };
    let (output, _) = wiki::render_wiki(&conn, &config).unwrap();

    assert!(output.contains("SameDir"));
    // All files share the "src/storage" prefix — the cluster label should
    // reflect that.
    assert!(
        output.contains("src/storage") || output.contains("storage"),
        "cluster label should reflect the shared directory"
    );
}

// ===========================================================================
// 59. Wiki: no edges, no clusters → misc bucket
// ===========================================================================

#[test]
fn test_wiki_no_edges_misc_bucket() {
    let conn = setup();
    let f1 = insert_file(&conn, "alpha.rs");
    let f2 = insert_file(&conn, "beta.rs");

    write::insert_symbols(&conn, f1, &[sym("alpha", "function", 1, 3, true)]).unwrap();
    write::insert_symbols(&conn, f2, &[sym("beta", "function", 1, 3, true)]).unwrap();

    pagerank::compute_pagerank(&conn, &pagerank::PageRankConfig::default()).unwrap();

    let config = wiki::WikiConfig {
        project_name: "NoEdges".to_string(),
        recompute: true,
        ..Default::default()
    };
    let (output, _) = wiki::render_wiki(&conn, &config).unwrap();

    assert!(output.contains("NoEdges"));
}

// ===========================================================================
// 60. FTS: special character queries don't panic
// ===========================================================================

#[test]
fn test_fts_special_characters_no_panic() {
    let conn = setup();
    let f = insert_file(&conn, "src/lib.rs");
    write::insert_symbols(&conn, f, &[sym("normal", "function", 1, 3, true)]).unwrap();
    write::sync_fts(&conn).unwrap();

    // These should all be safe due to sanitize_fts_query.
    for query in &[
        "OR", "AND", "NOT", "NEAR", "\"", "'", "()", "a OR b", "a*b", "near/2",
    ] {
        let sanitized = read::sanitize_fts_query(query);
        let result = read::search_symbols_fts(&conn, &sanitized, 10);
        assert!(
            result.is_ok(),
            "FTS query {query:?} (sanitized: {sanitized:?}) should not panic"
        );
    }

    // Verify FTS5 operators are properly quoted.
    assert_eq!(read::sanitize_fts_query("OR"), "\"OR\"");
    assert_eq!(read::sanitize_fts_query("AND"), "\"AND\"");
    assert_eq!(read::sanitize_fts_query("NOT"), "\"NOT\"");
    assert_eq!(read::sanitize_fts_query("NEAR"), "\"NEAR\"");
    // Case-insensitive: FTS5 treats operators case-insensitively.
    assert_eq!(read::sanitize_fts_query("or"), "\"or\"");
    assert_eq!(read::sanitize_fts_query("And"), "\"And\"");
}

// ===========================================================================
// 61. FTS: body search with prefix
// ===========================================================================

#[test]
fn test_fts_body_search_prefix() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub fn configuration_loader() { todo!() }\npub fn configure_server() { todo!() }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();
    write::rebuild_symbol_bodies(&conn, dir.path()).unwrap();

    let results = read::search_symbol_bodies_fts(&conn, "configur*", 10).unwrap();
    assert!(
        results.len() >= 2,
        "prefix search 'configur*' should match both symbols, got {}",
        results.len()
    );
}

// ===========================================================================
// 62. search_file_ids_by_fts: empty database
// ===========================================================================

#[test]
fn test_search_file_ids_by_fts_empty_db() {
    let conn = setup();
    write::sync_fts(&conn).unwrap();

    let ids = read::search_file_ids_by_fts(&conn, "anything").unwrap();
    assert!(ids.is_empty());
}

// ===========================================================================
// 63. File cluster reassignment
// ===========================================================================

#[test]
fn test_file_cluster_reassignment() {
    let conn = setup();
    let f = insert_file(&conn, "a.rs");

    write::upsert_file_cluster(&conn, f, 1, 1000).unwrap();
    let clusters = read::get_all_file_clusters(&conn).unwrap();
    assert_eq!(clusters[0].1, 1); // (file_id, cluster_id)

    // Reassign to a different cluster.
    write::upsert_file_cluster(&conn, f, 2, 2000).unwrap();
    let clusters = read::get_all_file_clusters(&conn).unwrap();
    assert_eq!(clusters.len(), 1, "should update, not duplicate");
    assert_eq!(clusters[0].1, 2);
}

// ===========================================================================
// 64. PageRank: damping=0 gives uniform distribution
// ===========================================================================

#[test]
fn test_pagerank_damping_zero_uniform() {
    let ranks = pagerank::pagerank_raw(
        &[1, 2, 3],
        &[(1, 2), (2, 3)],
        &pagerank::PageRankConfig {
            damping: 0.0,
            iterations: 100,
            epsilon: 1e-10,
        },
    );

    let expected = 1.0 / 3.0;
    for &id in &[1, 2, 3] {
        assert!(
            (ranks[&id] - expected).abs() < 0.01,
            "damping=0 should give uniform rank; node {id} got {}",
            ranks[&id]
        );
    }
}

// ===========================================================================
// 65. PageRank: damping=1 (no teleportation)
// ===========================================================================

#[test]
fn test_pagerank_damping_one() {
    let ranks = pagerank::pagerank_raw(
        &[1, 2, 3],
        &[(1, 2), (2, 3), (3, 1)],
        &pagerank::PageRankConfig {
            damping: 1.0,
            iterations: 200,
            epsilon: 1e-10,
        },
    );

    // In a 3-node cycle with damping=1, all nodes should have equal rank.
    let total: f64 = ranks.values().sum();
    assert!(
        (total - 1.0).abs() < 0.01,
        "ranks should still sum to ~1.0, got {total}"
    );
}

// ===========================================================================
// 66. PageRank: large fan-in node
// ===========================================================================

#[test]
fn test_pagerank_large_fan_in() {
    let mut nodes: Vec<i64> = (1..=100).collect();
    let hub = 100;
    nodes.push(hub);
    let edges: Vec<(i64, i64)> = (1..100).map(|i| (i, hub)).collect();

    let ranks = pagerank::pagerank_raw(&nodes, &edges, &pagerank::PageRankConfig::default());

    // Hub should have the highest rank.
    let hub_rank = ranks[&hub];
    for i in 1..100 {
        assert!(
            hub_rank > ranks[&i],
            "hub should outrank node {i}: hub={hub_rank}, node={}",
            ranks[&i]
        );
    }
}

// ===========================================================================
// 67. Blast radius: long chain
// ===========================================================================

#[test]
fn test_blast_radius_long_chain() {
    let conn = setup();
    let mut ids = Vec::new();
    for i in 0..10 {
        ids.push(insert_file(&conn, &format!("f{i}.rs")));
    }
    // Chain: f0 → f1 → f2 → ... → f9
    for i in 0..9 {
        write::insert_edge(&conn, ids[i], ids[i + 1], "import", None).unwrap();
    }

    // f9 (end of chain) is depended on transitively by all 9 predecessors.
    let result = blast::blast_radius_for_file(&conn, ids[9]).unwrap();
    assert_eq!(result.transitive_count, 9);
    assert_eq!(
        result.direct_importers.len(),
        1,
        "only f8 imports f9 directly"
    );

    // f0 (start of chain) has no dependents.
    let result0 = blast::blast_radius_for_file(&conn, ids[0]).unwrap();
    assert_eq!(result0.transitive_count, 0);
}

// ===========================================================================
// 68. Symbol PageRank: no refs → all equal
// ===========================================================================

#[test]
fn test_symbol_pagerank_no_refs_equal() {
    let conn = setup();
    let f = insert_file(&conn, "a.rs");
    let ids = write::insert_symbols(
        &conn,
        f,
        &[
            sym("alpha", "function", 1, 3, true),
            sym("beta", "function", 5, 7, true),
            sym("gamma", "function", 9, 11, true),
        ],
    )
    .unwrap();

    // No symbol refs at all.
    pagerank::compute_symbol_pagerank(&conn, &pagerank::PageRankConfig::default()).unwrap();

    let all = read::get_all_symbols(&conn).unwrap();
    let ranks: Vec<f64> = ids
        .iter()
        .map(|id| all.iter().find(|s| s.id == *id).unwrap().pagerank)
        .collect();

    // All should have approximately equal rank.
    assert!(
        (ranks[0] - ranks[1]).abs() < 0.01,
        "no refs: all symbols should have equal rank"
    );
    assert!(
        (ranks[1] - ranks[2]).abs() < 0.01,
        "no refs: all symbols should have equal rank"
    );
}

// ===========================================================================
// 69. Cochange: canonical ordering determinism
// ===========================================================================

#[test]
fn test_cochange_canonical_ordering_deterministic() {
    let conn = setup();
    let a = insert_file(&conn, "alpha.rs");
    let b = insert_file(&conn, "beta.rs");
    let c = insert_file(&conn, "gamma.rs");

    // Insert pairs in different orders.
    write::upsert_cochange(&conn, c, a).unwrap();
    write::upsert_cochange(&conn, a, c).unwrap();
    write::upsert_cochange(&conn, b, a).unwrap();
    write::upsert_cochange(&conn, a, b).unwrap();

    let a_cochanges = read::get_cochanges(&conn, a, 10).unwrap();
    assert_eq!(a_cochanges.len(), 2, "a should have 2 co-change partners");

    // Both should have count=2 regardless of insertion order.
    for (row, _) in &a_cochanges {
        assert_eq!(row.count, 2, "each pair inserted twice should have count=2");
    }
}

// ===========================================================================
// 70. insert_symbols: many children with same parent
// ===========================================================================

#[test]
fn test_insert_symbols_many_children_same_parent() {
    let conn = setup();
    let f = insert_file(&conn, "src/lib.rs");

    let mut batch = vec![sym("MyEnum", "enum", 1, 20, true)];
    for i in 0..10 {
        batch.push(SymbolInsert {
            name: format!("Variant{i}"),
            kind: "variant".into(),
            line_start: 2 + i as u32,
            line_end: 2 + i as u32,
            signature: None,
            is_exported: false,
            shape_hash: None,
            parent_idx: Some(0), // all point to MyEnum
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        });
    }

    let ids = write::insert_symbols(&conn, f, &batch).unwrap();
    assert_eq!(ids.len(), 11);

    let symbols = read::get_symbols_for_file(&conn, f).unwrap();
    let parent_id = ids[0];
    for child in symbols.iter().filter(|s| s.name.starts_with("Variant")) {
        assert_eq!(
            child.parent_id,
            Some(parent_id),
            "all variants should point to MyEnum"
        );
    }
}

// ===========================================================================
// 71. delete_file_data + re-insert: IDs are fresh
// ===========================================================================

#[test]
fn test_delete_and_reinsert_fresh_ids() {
    let conn = setup();
    let f = insert_file(&conn, "a.rs");
    let old_ids = write::insert_symbols(&conn, f, &[sym("old", "function", 1, 3, true)]).unwrap();

    write::delete_file_data(&conn, f).unwrap();

    let f2 = insert_file(&conn, "a.rs");
    let new_ids = write::insert_symbols(&conn, f2, &[sym("new", "function", 1, 3, true)]).unwrap();

    assert_ne!(
        old_ids[0], new_ids[0],
        "re-inserted symbol should get a new ID"
    );
}

// ===========================================================================
// 72. Reindex unchanged file is skipped
// ===========================================================================

#[test]
fn test_reindex_unchanged_file_skipped() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub fn stable() {}\n").unwrap();

    let conn = setup();

    index::full_index(&conn, dir.path(), false).unwrap();
    let count1 = read::get_symbol_count(&conn).unwrap();

    // Re-index without changes — symbols should not be duplicated.
    index::full_index(&conn, dir.path(), false).unwrap();
    let count2 = read::get_symbol_count(&conn).unwrap();

    assert_eq!(
        count1, count2,
        "re-indexing unchanged file should not duplicate symbols"
    );
}

// ===========================================================================
// 73. Force reindex processes all files
// ===========================================================================

#[test]
fn test_force_reindex() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub fn original() {}\n").unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    // Change the file content.
    fs::write(src.join("lib.rs"), "pub fn replaced() {}\n").unwrap();

    // Force reindex.
    index::full_index(&conn, dir.path(), true).unwrap();

    let all = read::get_all_symbols(&conn).unwrap();
    let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"replaced"),
        "force reindex should pick up new symbol"
    );
    assert!(
        !names.contains(&"original"),
        "old symbol should be gone after force reindex"
    );
}

// ===========================================================================
// 74. Compute clusters writes to DB and is re-readable
// ===========================================================================

#[test]
fn test_compute_clusters_round_trip() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");
    let c = insert_file(&conn, "c.rs");
    let d = insert_file(&conn, "d.rs");

    write::insert_edge(&conn, a, b, "import", None).unwrap();
    write::insert_edge(&conn, c, d, "import", None).unwrap();

    let report = leiden::compute_clusters(&conn, &leiden::LeidenConfig::default()).unwrap();
    assert!(!report.assignments.is_empty());

    // Read back from DB.
    let clusters = read::get_all_file_clusters(&conn).unwrap();
    let cluster_file_ids: HashSet<i64> = clusters.iter().map(|c| c.0).collect();

    for &file_id in &[a, b, c, d] {
        assert!(
            cluster_file_ids.contains(&file_id),
            "file {file_id} should have a cluster assignment in DB"
        );
    }
}

// ===========================================================================
// 75. get_symbols_for_file: ordering by line_start
// ===========================================================================

#[test]
fn test_symbols_for_file_ordered_by_line() {
    let conn = setup();
    let f = insert_file(&conn, "src/lib.rs");

    // Insert out of order.
    write::insert_symbols(
        &conn,
        f,
        &[
            sym("third", "function", 20, 25, true),
            sym("first", "function", 1, 5, true),
            sym("second", "function", 10, 15, true),
        ],
    )
    .unwrap();

    let symbols = read::get_symbols_for_file(&conn, f).unwrap();
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

    // The DB returns them in insertion order or by ID — verify the function
    // provides consistent results.
    assert_eq!(symbols.len(), 3);
    // At minimum check all 3 are present.
    assert!(names.contains(&"first"));
    assert!(names.contains(&"second"));
    assert!(names.contains(&"third"));
}

// ===========================================================================
// Architecture boundary enforcement (qartez_boundaries)
// ===========================================================================

#[test]
fn test_boundaries_end_to_end_catches_denied_edge() {
    use qartez_mcp::graph::boundaries::{check_boundaries, parse_config};
    use std::path::Path;

    let conn = setup();
    let ui = insert_file(&conn, "src/ui/page.rs");
    let db = insert_file(&conn, "src/db/table.rs");
    let shared = insert_file(&conn, "src/shared/error.rs");

    write::insert_edge(&conn, ui, db, "import", None).unwrap();
    write::insert_edge(&conn, ui, shared, "import", None).unwrap();

    let cfg_text = r#"
[[boundary]]
from = "src/ui/**"
deny = ["src/db/**", "src/shared/**"]
allow = ["src/shared/**"]
"#;
    let config = parse_config(cfg_text, Path::new("test.toml")).unwrap();
    let files = read::get_all_files(&conn).unwrap();
    let edges = read::get_all_edges(&conn).unwrap();
    let violations = check_boundaries(&config, &files, &edges);

    assert_eq!(violations.len(), 1, "only ui->db should violate");
    assert_eq!(violations[0].from_file, "src/ui/page.rs");
    assert_eq!(violations[0].to_file, "src/db/table.rs");
    assert_eq!(violations[0].deny_pattern, "src/db/**");
}

#[test]
fn test_boundaries_load_config_from_file() {
    use qartez_mcp::graph::boundaries::{check_boundaries, load_config};

    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("boundaries.toml");
    fs::write(
        &config_path,
        "[[boundary]]\nfrom = \"src/a/**\"\ndeny = [\"src/b/**\"]\n",
    )
    .unwrap();

    let config = load_config(&config_path).unwrap();
    assert_eq!(config.boundary.len(), 1);

    let conn = setup();
    let a = insert_file(&conn, "src/a/lib.rs");
    let b = insert_file(&conn, "src/b/lib.rs");
    write::insert_edge(&conn, a, b, "import", None).unwrap();

    let files = read::get_all_files(&conn).unwrap();
    let edges = read::get_all_edges(&conn).unwrap();
    let violations = check_boundaries(&config, &files, &edges);
    assert_eq!(violations.len(), 1);
}

#[test]
fn test_boundaries_suggest_from_clusters() {
    use qartez_mcp::graph::boundaries::suggest_boundaries;

    let conn = setup();
    let f_ui1 = insert_file(&conn, "src/ui/a.rs");
    let f_ui2 = insert_file(&conn, "src/ui/b.rs");
    let f_ui3 = insert_file(&conn, "src/ui/c.rs");
    let f_db1 = insert_file(&conn, "src/db/x.rs");
    let f_db2 = insert_file(&conn, "src/db/y.rs");
    let f_db3 = insert_file(&conn, "src/db/z.rs");

    write::insert_edge(&conn, f_ui1, f_ui2, "import", None).unwrap();
    write::insert_edge(&conn, f_ui2, f_ui3, "import", None).unwrap();
    write::insert_edge(&conn, f_ui3, f_ui1, "import", None).unwrap();
    write::insert_edge(&conn, f_db1, f_db2, "import", None).unwrap();
    write::insert_edge(&conn, f_db2, f_db3, "import", None).unwrap();
    write::insert_edge(&conn, f_db3, f_db1, "import", None).unwrap();

    leiden::compute_clusters(&conn, &leiden::LeidenConfig::default()).unwrap();

    let files = read::get_all_files(&conn).unwrap();
    let clusters = read::get_all_file_clusters(&conn).unwrap();
    let edges = read::get_all_edges(&conn).unwrap();
    let cfg = suggest_boundaries(&files, &clusters, &edges);

    assert_eq!(cfg.boundary.len(), 2, "two cluster prefixes → two rules");
    let froms: HashSet<String> = cfg.boundary.iter().map(|r| r.from.clone()).collect();
    assert!(froms.contains("src/ui/**"));
    assert!(froms.contains("src/db/**"));
    for rule in &cfg.boundary {
        assert!(
            !rule.deny.is_empty(),
            "each rule should deny the other cluster"
        );
    }
}

#[test]
fn test_wiki_renders_violation_marker_when_config_passed() {
    use qartez_mcp::graph::boundaries::{check_boundaries, parse_config};
    use qartez_mcp::graph::wiki::{WikiConfig, render_wiki};
    use std::path::Path;

    let conn = setup();
    let f_a = write::upsert_file(&conn, "src/auth/login.rs", 1000, 100, "rust", 10).unwrap();
    let f_b = write::upsert_file(&conn, "src/auth/token.rs", 1000, 100, "rust", 10).unwrap();
    let f_c = write::upsert_file(&conn, "src/auth/session.rs", 1000, 100, "rust", 10).unwrap();
    let f_d = write::upsert_file(&conn, "src/db/blob.rs", 1000, 100, "rust", 10).unwrap();
    let f_e = write::upsert_file(&conn, "src/db/index.rs", 1000, 100, "rust", 10).unwrap();
    let f_f = write::upsert_file(&conn, "src/db/cache.rs", 1000, 100, "rust", 10).unwrap();

    write::insert_edge(&conn, f_a, f_b, "import", None).unwrap();
    write::insert_edge(&conn, f_b, f_c, "import", None).unwrap();
    write::insert_edge(&conn, f_c, f_a, "import", None).unwrap();
    write::insert_edge(&conn, f_d, f_e, "import", None).unwrap();
    write::insert_edge(&conn, f_e, f_f, "import", None).unwrap();
    write::insert_edge(&conn, f_f, f_d, "import", None).unwrap();
    write::insert_edge(&conn, f_a, f_d, "import", None).unwrap();

    let cfg_text = r#"
[[boundary]]
from = "src/auth/**"
deny = ["src/db/**"]
"#;
    let config = parse_config(cfg_text, Path::new("test.toml")).unwrap();
    let files = read::get_all_files(&conn).unwrap();
    let edges = read::get_all_edges(&conn).unwrap();
    let violations = check_boundaries(&config, &files, &edges);
    assert!(
        !violations.is_empty(),
        "seed edge src/auth/login -> src/db/blob must violate"
    );

    let wiki_cfg = WikiConfig {
        project_name: "test".to_string(),
        recompute: true,
        boundary_violations: Some(violations),
        ..Default::default()
    };
    let (markdown, _) = render_wiki(&conn, &wiki_cfg).unwrap();
    assert!(
        markdown.contains("[VIOLATION]"),
        "wiki should flag denied inter-cluster edge: {markdown}"
    );
}

#[test]
fn test_wiki_no_marker_without_config() {
    use qartez_mcp::graph::wiki::{WikiConfig, render_wiki};

    let conn = setup();
    let f_a = write::upsert_file(&conn, "src/auth/login.rs", 1000, 100, "rust", 10).unwrap();
    let f_b = write::upsert_file(&conn, "src/auth/token.rs", 1000, 100, "rust", 10).unwrap();
    let f_c = write::upsert_file(&conn, "src/auth/session.rs", 1000, 100, "rust", 10).unwrap();
    let f_d = write::upsert_file(&conn, "src/db/blob.rs", 1000, 100, "rust", 10).unwrap();
    let f_e = write::upsert_file(&conn, "src/db/index.rs", 1000, 100, "rust", 10).unwrap();
    let f_f = write::upsert_file(&conn, "src/db/cache.rs", 1000, 100, "rust", 10).unwrap();

    write::insert_edge(&conn, f_a, f_b, "import", None).unwrap();
    write::insert_edge(&conn, f_b, f_c, "import", None).unwrap();
    write::insert_edge(&conn, f_c, f_a, "import", None).unwrap();
    write::insert_edge(&conn, f_d, f_e, "import", None).unwrap();
    write::insert_edge(&conn, f_e, f_f, "import", None).unwrap();
    write::insert_edge(&conn, f_f, f_d, "import", None).unwrap();
    write::insert_edge(&conn, f_a, f_d, "import", None).unwrap();

    let wiki_cfg = WikiConfig {
        project_name: "test".to_string(),
        recompute: true,
        ..Default::default()
    };
    let (markdown, _) = render_wiki(&conn, &wiki_cfg).unwrap();
    assert!(
        !markdown.contains("[VIOLATION]"),
        "wiki must not flag anything without a boundary config"
    );
}

// ===========================================================================
// Benchmark-only tests (behind feature flag)
// ===========================================================================

#[cfg(feature = "benchmark")]
mod benchmark_tests {
    use qartez_mcp::benchmark::{judge, set_compare, tokenize};

    // -----------------------------------------------------------------------
    // 76. set_compare: both outputs empty → precision=1.0, recall=1.0
    // -----------------------------------------------------------------------

    #[test]
    fn test_compare_both_empty() {
        let scores = set_compare::compare("qartez_find", "", "").unwrap();
        assert_eq!(scores.mcp_items, 0);
        assert_eq!(scores.non_mcp_items, 0);
        assert_eq!(scores.intersection, 0);
        // Debatable: both empty returns "perfect" scores.
        assert_eq!(
            scores.precision, 1.0,
            "empty mcp → precision=1.0 by convention"
        );
        assert_eq!(
            scores.recall, 1.0,
            "empty non-mcp → recall=1.0 by convention"
        );
    }

    // -----------------------------------------------------------------------
    // 77. set_compare: only mcp has items → recall=1.0, precision=0.0
    // -----------------------------------------------------------------------

    #[test]
    fn test_compare_mcp_only() {
        // Parser expects `+ name kind` or `- name kind` format.
        let scores = set_compare::compare("qartez_find", "+ foo function", "").unwrap();
        assert!(scores.mcp_items > 0, "mcp should parse at least 1 item");
        assert_eq!(scores.non_mcp_items, 0);
        assert_eq!(scores.recall, 1.0, "empty reference → recall=1.0");
        assert_eq!(scores.precision, 0.0, "no intersection → precision=0.0");
    }

    // -----------------------------------------------------------------------
    // 78. set_compare: only non-mcp has items → precision=1.0, recall=0.0
    // -----------------------------------------------------------------------

    #[test]
    fn test_compare_non_mcp_only() {
        let scores = set_compare::compare("qartez_find", "", "+ bar struct").unwrap();
        assert_eq!(scores.mcp_items, 0);
        assert!(
            scores.non_mcp_items > 0,
            "non-mcp should parse at least 1 item"
        );
        assert_eq!(scores.precision, 1.0, "empty mcp → precision=1.0");
        assert_eq!(scores.recall, 0.0, "no intersection → recall=0.0");
    }

    // -----------------------------------------------------------------------
    // 79. set_compare: identical outputs → perfect scores
    // -----------------------------------------------------------------------

    #[test]
    fn test_compare_identical() {
        let output = "+ foo function\n+ bar struct";
        let scores = set_compare::compare("qartez_find", output, output).unwrap();
        assert_eq!(scores.precision, 1.0);
        assert_eq!(scores.recall, 1.0);
        assert!(scores.mcp_only.is_empty());
        assert!(scores.non_mcp_only.is_empty());
    }

    // -----------------------------------------------------------------------
    // 80. set_compare: excluded tool returns None
    // -----------------------------------------------------------------------

    #[test]
    fn test_compare_excluded_tool() {
        let result = set_compare::compare("qartez_stats", "anything", "anything");
        assert!(result.is_none(), "excluded tool should return None");
    }

    // -----------------------------------------------------------------------
    // 81. set_compare: completely disjoint outputs
    // -----------------------------------------------------------------------

    #[test]
    fn test_compare_disjoint() {
        let mcp = "+ alpha function";
        let non_mcp = "+ beta function";
        let scores = set_compare::compare("qartez_find", mcp, non_mcp).unwrap();
        assert_eq!(scores.intersection, 0);
        assert_eq!(scores.precision, 0.0);
        assert_eq!(scores.recall, 0.0);
    }

    // -----------------------------------------------------------------------
    // 82. cohens_weighted_kappa: perfect agreement
    // -----------------------------------------------------------------------

    #[test]
    fn test_kappa_perfect_agreement() {
        let pairs: Vec<(u8, u8)> = vec![(0, 0), (1, 1), (2, 2), (0, 0), (1, 1)];
        let kappa = judge::cohens_weighted_kappa(&pairs, 3);
        assert!(
            (kappa - 1.0).abs() < 0.001,
            "perfect agreement should give kappa=1.0, got {kappa}"
        );
    }

    // -----------------------------------------------------------------------
    // 83. cohens_weighted_kappa: total disagreement
    // -----------------------------------------------------------------------

    #[test]
    fn test_kappa_total_disagreement() {
        // All pairs are maximally disagreeing: (0, k-1).
        let pairs: Vec<(u8, u8)> = vec![(0, 2), (2, 0), (0, 2), (2, 0)];
        let kappa = judge::cohens_weighted_kappa(&pairs, 3);
        assert!(
            kappa < 0.0,
            "total disagreement should give negative kappa, got {kappa}"
        );
    }

    // -----------------------------------------------------------------------
    // 84. cohens_weighted_kappa: k=2 boundary
    // -----------------------------------------------------------------------

    #[test]
    fn test_kappa_k2() {
        let pairs: Vec<(u8, u8)> = vec![(0, 0), (1, 1), (0, 1), (1, 0)];
        let kappa = judge::cohens_weighted_kappa(&pairs, 2);
        // With k=2, denom = 1, so weight(0,1) = 1 - 1/1 = 0.
        // This means disagreements have zero weight — kappa should still
        // be computable without NaN.
        assert!(!kappa.is_nan(), "k=2 should not produce NaN");
    }

    // -----------------------------------------------------------------------
    // 85. cohens_weighted_kappa: all same value → 1.0 (p_e ≈ 1 guard)
    // -----------------------------------------------------------------------

    #[test]
    fn test_kappa_all_same_value() {
        let pairs: Vec<(u8, u8)> = vec![(0, 0), (0, 0), (0, 0)];
        let kappa = judge::cohens_weighted_kappa(&pairs, 3);
        assert_eq!(
            kappa, 1.0,
            "all same value should hit the p_e≈1 guard and return 1.0"
        );
    }

    // -----------------------------------------------------------------------
    // 86. cohens_weighted_kappa: too few pairs → NaN
    // -----------------------------------------------------------------------

    #[test]
    fn test_kappa_too_few_pairs() {
        let kappa = judge::cohens_weighted_kappa(&[(0, 0)], 3);
        assert!(kappa.is_nan(), "fewer than 2 pairs should return NaN");
    }

    // -----------------------------------------------------------------------
    // 87. cohens_weighted_kappa: value out of range → NaN
    // -----------------------------------------------------------------------

    #[test]
    fn test_kappa_value_out_of_range() {
        let kappa = judge::cohens_weighted_kappa(&[(0, 5), (1, 1)], 3);
        assert!(kappa.is_nan(), "value >= k should return NaN");
    }

    // -----------------------------------------------------------------------
    // 88. krippendorff_alpha: perfect agreement (different values across units)
    // -----------------------------------------------------------------------

    #[test]
    fn test_krippendorff_perfect_agreement() {
        let units = vec![
            vec![Some(1.0), Some(1.0)],
            vec![Some(2.0), Some(2.0)],
            vec![Some(3.0), Some(3.0)],
        ];
        let alpha = judge::krippendorff_alpha_interval(&units).unwrap();
        assert!(
            (alpha - 1.0).abs() < 0.001,
            "perfect agreement should give alpha=1.0, got {alpha}"
        );
    }

    // -----------------------------------------------------------------------
    // 89. krippendorff_alpha: all same value everywhere → None (d_e=0)
    // -----------------------------------------------------------------------

    #[test]
    fn test_krippendorff_zero_variance() {
        let units = vec![vec![Some(5.0), Some(5.0)], vec![Some(5.0), Some(5.0)]];
        let alpha = judge::krippendorff_alpha_interval(&units);
        assert!(
            alpha.is_none(),
            "zero variance in pool should return None, got {alpha:?}"
        );
    }

    // -----------------------------------------------------------------------
    // 90. krippendorff_alpha: pool too small → None
    // -----------------------------------------------------------------------

    #[test]
    fn test_krippendorff_pool_too_small() {
        let units = vec![vec![Some(1.0), Some(2.0)]];
        let alpha = judge::krippendorff_alpha_interval(&units);
        assert!(
            alpha.is_none(),
            "pool < 3 should return None, got {alpha:?}"
        );
    }

    // -----------------------------------------------------------------------
    // 91. krippendorff_alpha: missing data (Some/None mix)
    // -----------------------------------------------------------------------

    #[test]
    fn test_krippendorff_missing_data() {
        let units = vec![
            vec![Some(1.0), Some(1.0), None],
            vec![Some(2.0), None, Some(2.0)],
            vec![None, Some(3.0), Some(3.0)],
        ];
        let alpha = judge::krippendorff_alpha_interval(&units);
        // Should compute successfully with present values only.
        assert!(alpha.is_some(), "missing data should be handled gracefully");
        let alpha = alpha.unwrap();
        assert!(
            (alpha - 1.0).abs() < 0.001,
            "agreement on present values should give alpha near 1.0, got {alpha}"
        );
    }

    // -----------------------------------------------------------------------
    // 92. krippendorff_alpha: complete disagreement
    // -----------------------------------------------------------------------

    #[test]
    fn test_krippendorff_disagreement() {
        let units = vec![
            vec![Some(0.0), Some(100.0)],
            vec![Some(100.0), Some(0.0)],
            vec![Some(0.0), Some(100.0)],
        ];
        let alpha = judge::krippendorff_alpha_interval(&units);
        assert!(alpha.is_some());
        let alpha = alpha.unwrap();
        assert!(
            alpha < 0.5,
            "strong disagreement should give low alpha, got {alpha}"
        );
    }

    // -----------------------------------------------------------------------
    // 93. krippendorff_alpha: all units have only one rater → None
    // -----------------------------------------------------------------------

    #[test]
    fn test_krippendorff_single_rater_per_unit() {
        let units = vec![
            vec![Some(1.0), None],
            vec![None, Some(2.0)],
            vec![Some(3.0), None],
        ];
        let alpha = judge::krippendorff_alpha_interval(&units);
        // Each unit has n_u < 2, so d_o_den = 0. Should return None.
        assert!(
            alpha.is_none(),
            "single rater per unit should return None, got {alpha:?}"
        );
    }

    // -----------------------------------------------------------------------
    // 94. count_tokens: empty string
    // -----------------------------------------------------------------------

    #[test]
    fn test_count_tokens_empty() {
        assert_eq!(tokenize::count_tokens(""), 0);
    }

    // -----------------------------------------------------------------------
    // 95. count_tokens: known string
    // -----------------------------------------------------------------------

    #[test]
    fn test_count_tokens_known() {
        let count = tokenize::count_tokens("hello world");
        assert!(count > 0, "non-empty string should have > 0 tokens");
        assert!(count < 10, "2-word string should have fewer than 10 tokens");
    }

    // -----------------------------------------------------------------------
    // 96. naive_count: edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_naive_count_edge_cases() {
        assert_eq!(tokenize::naive_count(""), 0);
        assert_eq!(tokenize::naive_count("ab"), 0); // 2/4 = 0 (integer division)
        assert_eq!(tokenize::naive_count("abcd"), 1);
        assert_eq!(tokenize::naive_count("abcdefgh"), 2);
    }

    // -----------------------------------------------------------------------
    // 97. set_compare: generic tool uses parse_generic_identifiers
    // -----------------------------------------------------------------------

    #[test]
    fn test_compare_generic_tool() {
        // A tool name not in the known list → falls through to parse_generic_identifiers.
        let scores = set_compare::compare("qartez_map", "foo bar baz", "foo bar baz");
        assert!(scores.is_some());
        let scores = scores.unwrap();
        assert_eq!(scores.precision, 1.0);
        assert_eq!(scores.recall, 1.0);
    }
}
