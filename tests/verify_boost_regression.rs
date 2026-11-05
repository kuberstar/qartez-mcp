//! Regression verification for the /boost refactors.
//!
//! (1) Proves the hotspots/health N+1 -> bulk-group refactor produces
//!     byte-identical per-file avg_cc/max_cc as the old per-file query.
//! (2) Proves the serde-saphyr YAML backend preserves the
//!     read-modify-write YAML round-trip that setup.rs performs.

use std::collections::HashMap;

use rusqlite::Connection;

use qartez_mcp::storage::{models::SymbolInsert, read, schema, write};

fn setup() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    conn
}

fn sym(name: &str, start: u32, end: u32, cc: Option<u32>) -> SymbolInsert {
    SymbolInsert {
        name: name.to_string(),
        kind: "function".to_string(),
        line_start: start,
        line_end: end,
        signature: None,
        is_exported: true,
        shape_hash: None,
        parent_idx: None,
        unused_excluded: false,
        complexity: cc,
        owner_type: None,
    }
}

/// The refactor replaced an O(files) `get_symbols_for_file` fan-out with a
/// single `get_all_symbols_with_path` bulk load grouped by `file_id`. This
/// test computes avg_cc/max_cc BOTH ways on a fixture with multiple symbols
/// per file (including a None-complexity symbol and a file with zero scored
/// symbols) and asserts they are identical.
#[test]
fn bulk_group_matches_per_file_avg_and_max_cc() {
    let conn = setup();

    // File A: three functions, one with no complexity (must be skipped).
    let a = write::upsert_file(&conn, "src/a.rs", 1000, 100, "rust", 30).unwrap();
    write::insert_symbols(
        &conn,
        a,
        &[
            sym("a1", 1, 5, Some(3)),
            sym("a2", 7, 20, Some(11)),
            sym("a3", 22, 25, None), // no complexity -> excluded from both paths
        ],
    )
    .unwrap();

    // File B: single function.
    let b = write::upsert_file(&conn, "src/b.rs", 1000, 100, "rust", 10).unwrap();
    write::insert_symbols(&conn, b, &[sym("b1", 1, 40, Some(7))]).unwrap();

    // File C: only a None-complexity symbol -> yields no complexities at all.
    let c = write::upsert_file(&conn, "src/c.rs", 1000, 100, "rust", 5).unwrap();
    write::insert_symbols(&conn, c, &[sym("c1", 1, 3, None)]).unwrap();

    // --- OLD path: per-file query, filter Some(cc) ---
    let mut old: HashMap<i64, (f64, f64)> = HashMap::new();
    for file in read::get_all_files(&conn).unwrap() {
        let ccs: Vec<u32> = read::get_symbols_for_file(&conn, file.id)
            .unwrap()
            .into_iter()
            .filter_map(|s| s.complexity)
            .collect();
        if ccs.is_empty() {
            continue;
        }
        let avg = ccs.iter().copied().sum::<u32>() as f64 / ccs.len() as f64;
        let max = ccs.iter().copied().max().unwrap_or(1) as f64;
        old.insert(file.id, (avg, max));
    }

    // --- NEW path: bulk load, group by file_id ---
    let all_symbols = read::get_all_symbols_with_path(&conn).unwrap();
    let mut cc_by_file: HashMap<i64, Vec<u32>> = HashMap::new();
    for (s, _) in &all_symbols {
        if let Some(cc) = s.complexity {
            cc_by_file.entry(s.file_id).or_default().push(cc);
        }
    }
    let mut new: HashMap<i64, (f64, f64)> = HashMap::new();
    for (fid, ccs) in &cc_by_file {
        if ccs.is_empty() {
            continue;
        }
        let avg = ccs.iter().copied().sum::<u32>() as f64 / ccs.len() as f64;
        let max = ccs.iter().copied().max().unwrap_or(1) as f64;
        new.insert(*fid, (avg, max));
    }

    assert_eq!(old, new, "bulk group-by must equal per-file query");

    // Concrete expected values: A avg=(3+11)/2=7, max=11; B avg=7, max=7; C absent.
    assert_eq!(new.get(&a), Some(&(7.0, 11.0)));
    assert_eq!(new.get(&b), Some(&(7.0, 7.0)));
    assert_eq!(
        new.get(&c),
        None,
        "None-complexity-only file must be excluded"
    );
    assert_eq!(new.len(), 2);
}

/// Reproduces the exact read-modify-write that `install_continue` in
/// src/bin/setup.rs performs: parse an existing YAML doc into a
/// `serde_json::Value`, edit the `mcpServers` sequence, re-serialize, and
/// re-parse. Asserts structure, values and key order survive the
/// serde-saphyr round-trip.
#[test]
fn serde_saphyr_mcp_config_roundtrip() {
    let input = "name: Local Config\nversion: 0.0.1\nschema: v1\nmcpServers:\n  - name: other\n    command: /usr/bin/other\n    args: []\n";

    let mut data: serde_json::Value = serde_saphyr::from_str(input).unwrap();
    let servers = data["mcpServers"].as_array_mut().unwrap();
    servers.push(serde_json::json!({
        "name": "qartez",
        "command": "/opt/qartez",
        "args": [],
    }));

    let out = serde_saphyr::to_string(&data).unwrap();
    let reparsed: serde_json::Value = serde_saphyr::from_str(&out).unwrap();

    // Top-level scalars preserved; `0.0.1` stays a string, not a number.
    assert_eq!(reparsed["name"].as_str(), Some("Local Config"));
    assert_eq!(reparsed["version"].as_str(), Some("0.0.1"));
    assert_eq!(reparsed["schema"].as_str(), Some("v1"));

    // The user's key order survives the write-back.
    let keys: Vec<&str> = reparsed
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["name", "version", "schema", "mcpServers"]);

    // Both servers present, qartez appended with the right command.
    let servers = reparsed["mcpServers"].as_array().unwrap();
    assert_eq!(servers.len(), 2);
    assert_eq!(servers[0]["name"].as_str(), Some("other"));
    assert_eq!(servers[1]["name"].as_str(), Some("qartez"));
    assert_eq!(servers[1]["command"].as_str(), Some("/opt/qartez"));
    assert!(servers[1]["args"].as_array().unwrap().is_empty());
}

/// Reproduces the skill-frontmatter parse path in setup.rs: deserialize a
/// YAML frontmatter block into a typed struct.
#[test]
fn serde_saphyr_frontmatter_parse() {
    #[derive(serde::Deserialize)]
    struct Frontmatter {
        name: String,
        description: String,
    }

    let frontmatter =
        "name: qartez\ndescription: |\n  Semantic code intelligence.\n  Multi-line body.\n";
    let parsed: Frontmatter = serde_saphyr::from_str(frontmatter).unwrap();
    assert_eq!(parsed.name, "qartez");
    assert!(parsed.description.contains("Semantic code intelligence."));
    assert!(parsed.description.contains("Multi-line body."));

    // Untyped Value parse of the same block also succeeds (the fallback path).
    let val: serde_json::Value = serde_saphyr::from_str(frontmatter).unwrap();
    assert_eq!(val["name"].as_str(), Some("qartez"));
}
