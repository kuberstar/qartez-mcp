//! Regression coverage for issue #36: a full reindex of a C codebase left the
//! `edges` table empty, degrading every graph-tier tool (`qartez_impact`,
//! `qartez_deps`, `qartez_diff_impact`, `qartez_hotspots`, `qartez_health`).
//!
//! Root cause: C/C++ quoted includes (`#include "db.h"`) are bare or
//! include-root-relative file references, not dot-anchored module specifiers,
//! so the generic JS/TS-oriented resolver rejected them before any edge was
//! written. These tests pin the three include layouts a C resolver must
//! handle: same directory, project-root relative, and `-I include` style.

use std::fs;

use rusqlite::Connection;
use tempfile::TempDir;

use qartez_mcp::index;
use qartez_mcp::storage::{read, schema};

fn setup() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    conn
}

/// Mirrors the reporter's layout: headers under `include/`, sources under
/// `src/`, every source pulling shared headers via `#include "db.h"` as the
/// build system's `-I include` flag would resolve them.
#[test]
fn c_include_graph_populates_edges() {
    let dir = TempDir::new().unwrap();
    let include = dir.path().join("include");
    let src_db = dir.path().join("src").join("db");
    fs::create_dir_all(&include).unwrap();
    fs::create_dir_all(&src_db).unwrap();

    fs::write(include.join("db.h"), "int db_open(const char *path);\n").unwrap();
    fs::write(include.join("config.h"), "int config_load(void);\n").unwrap();

    // `-I include` style: bare specifier resolved against the include root.
    fs::write(
        src_db.join("db.c"),
        "#include \"db.h\"\n#include \"config.h\"\nint db_open(const char *p){return 0;}\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("src").join("main.c"),
        "#include \"db.h\"\nint main(void){return db_open(\"x\");}\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let edges = read::get_all_edges(&conn).unwrap();
    assert!(
        edges.len() >= 3,
        "expected >=3 include edges (db.c->db.h, db.c->config.h, main.c->db.h), got {}: {edges:?}",
        edges.len(),
    );

    // db.h is the hub: included by both db.c and main.c. This is exactly the
    // signal `qartez_impact`/`qartez_stats` read out of the edges table.
    let db_h = read::get_file_by_path(&conn, "include/db.h")
        .unwrap()
        .expect("include/db.h indexed");
    let importer_ids: Vec<i64> = read::get_edges_to(&conn, db_h.id)
        .unwrap()
        .iter()
        .map(|e| e.from_file)
        .collect();
    assert_eq!(
        importer_ids.len(),
        2,
        "include/db.h should be imported by db.c and main.c, got from_file ids {importer_ids:?}",
    );
}

/// Same-directory include: `#include "util.h"` next to `util.c`. This is the
/// quoted-include case a C compiler resolves first, before any `-I` path.
#[test]
fn c_same_directory_include_resolves() {
    let dir = TempDir::new().unwrap();
    let util = dir.path().join("src").join("util");
    fs::create_dir_all(&util).unwrap();

    fs::write(util.join("util.h"), "int clamp(int x);\n").unwrap();
    fs::write(
        util.join("util.c"),
        "#include \"util.h\"\nint clamp(int x){return x;}\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let util_h = read::get_file_by_path(&conn, "src/util/util.h")
        .unwrap()
        .expect("src/util/util.h indexed");
    let importer_ids: Vec<i64> = read::get_edges_to(&conn, util_h.id)
        .unwrap()
        .iter()
        .map(|e| e.from_file)
        .collect();
    assert_eq!(
        importer_ids.len(),
        1,
        "src/util/util.h should be imported by util.c, got from_file ids {importer_ids:?}",
    );
}
