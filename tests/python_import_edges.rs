//! Regression coverage for the Python analog of the empty-edges bug: a full
//! reindex of a Python codebase produced zero import edges because absolute
//! imports (`from pkg.mod import x`, `import pkg.mod`) were never resolved -
//! only dot-relative imports were. These tests pin that absolute imports now
//! populate the edges table for both flat and `src/` layouts, so the
//! graph-tier tools (qartez_impact, qartez_deps, ...) work on Python projects.

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

/// Flat layout: package at the repo root, absolute imports `chitta.<mod>`.
#[test]
fn python_absolute_imports_populate_edges_flat_layout() {
    let dir = TempDir::new().unwrap();
    let pkg = dir.path().join("chitta");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("__init__.py"), "").unwrap();
    fs::write(pkg.join("config.py"), "settings = {}\n").unwrap();
    fs::write(pkg.join("db.py"), "def connect():\n    return None\n").unwrap();
    fs::write(
        pkg.join("main.py"),
        "from chitta.config import settings\nimport chitta.db\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let edges = read::get_all_edges(&conn).unwrap();
    assert!(
        edges.len() >= 2,
        "expected main.py->config.py and main.py->db.py, got {}: {edges:?}",
        edges.len(),
    );

    let config = read::get_file_by_path(&conn, "chitta/config.py")
        .unwrap()
        .expect("chitta/config.py indexed");
    let importers: Vec<i64> = read::get_edges_to(&conn, config.id)
        .unwrap()
        .iter()
        .map(|e| e.from_file)
        .collect();
    assert_eq!(
        importers.len(),
        1,
        "chitta/config.py should be imported by main.py, got from_file ids {importers:?}",
    );
}

/// src-layout: package under `src/`, resolved via the discovered `src` import
/// root rather than the repo root.
#[test]
fn python_absolute_imports_populate_edges_src_layout() {
    let dir = TempDir::new().unwrap();
    let pkg = dir.path().join("src").join("chitta");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("__init__.py"), "").unwrap();
    fs::write(pkg.join("models.py"), "class User:\n    pass\n").unwrap();
    fs::write(
        pkg.join("service.py"),
        "from chitta.models import User\n\n\ndef make():\n    return User()\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let models = read::get_file_by_path(&conn, "src/chitta/models.py")
        .unwrap()
        .expect("src/chitta/models.py indexed");
    let importers: Vec<i64> = read::get_edges_to(&conn, models.id)
        .unwrap()
        .iter()
        .map(|e| e.from_file)
        .collect();
    assert_eq!(
        importers.len(),
        1,
        "src/chitta/models.py should be imported by service.py, got from_file ids {importers:?}",
    );
}
