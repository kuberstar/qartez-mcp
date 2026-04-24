use std::collections::HashSet;
use std::fs;
use std::path::Path;

use rusqlite::Connection;
use tempfile::TempDir;

use qartez_mcp::graph::{blast, pagerank};
use qartez_mcp::index;
use qartez_mcp::storage::{models::SymbolInsert, read, schema, write};
use qartez_mcp::toolchain;

fn setup() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    conn
}

fn insert_file(conn: &Connection, path: &str) -> i64 {
    write::upsert_file(conn, path, 1000, 100, "rust", 10).unwrap()
}

// ---------------------------------------------------------------------------
// 1. End-to-end indexing: Rust
// ---------------------------------------------------------------------------

#[test]
fn test_index_rust_file() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub fn hello() -> &'static str { \"world\" }\n\
         pub struct Config { pub name: String }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let files = read::get_all_files(&conn).unwrap();
    assert_eq!(files.len(), 1);

    let symbols = read::get_symbols_for_file(&conn, files[0].id).unwrap();
    assert!(
        symbols.len() >= 2,
        "expected >=2 symbols, got {}",
        symbols.len()
    );

    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"hello"), "missing symbol 'hello'");
    assert!(names.contains(&"Config"), "missing symbol 'Config'");

    let hello = symbols.iter().find(|s| s.name == "hello").unwrap();
    assert!(hello.is_exported, "pub fn should be exported");
}

// ---------------------------------------------------------------------------
// 1. End-to-end indexing: TypeScript
// ---------------------------------------------------------------------------

#[test]
fn test_index_typescript_file_with_imports_exports() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("utils.ts"),
        "export function add(a: number, b: number): number { return a + b; }\n\
         export const PI = 3.14;\n",
    )
    .unwrap();

    fs::write(
        src.join("app.ts"),
        "import { add } from './utils';\n\
         export class App {\n\
             run() { console.log(add(1, 2)); }\n\
         }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let file_count = read::get_file_count(&conn).unwrap();
    assert_eq!(file_count, 2);

    let sym_count = read::get_symbol_count(&conn).unwrap();
    assert!(
        sym_count >= 3,
        "expected >=3 symbols (add, PI, App), got {sym_count}"
    );

    let edges = read::get_all_edges(&conn).unwrap();
    assert!(
        !edges.is_empty(),
        "TS import should create at least one edge"
    );
}

// ---------------------------------------------------------------------------
// 1. End-to-end indexing: Python
// ---------------------------------------------------------------------------

#[test]
fn test_index_python_file() {
    let dir = TempDir::new().unwrap();
    let pkg = dir.path().join("pkg");
    fs::create_dir_all(&pkg).unwrap();

    fs::write(
        pkg.join("models.py"),
        "class User:\n    def __init__(self, name: str):\n        self.name = name\n\n\
         def greet(user: User) -> str:\n    return f'Hello, {user.name}'\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let files = read::get_all_files(&conn).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].language, "python");

    let symbols = read::get_symbols_for_file(&conn, files[0].id).unwrap();
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"User"), "missing class 'User'");
    assert!(names.contains(&"greet"), "missing function 'greet'");
}

// ---------------------------------------------------------------------------
// 1. End-to-end indexing: Go
// ---------------------------------------------------------------------------

#[test]
fn test_index_go_file() {
    let dir = TempDir::new().unwrap();
    let pkg = dir.path().join("cmd");
    fs::create_dir_all(&pkg).unwrap();

    fs::write(
        pkg.join("main.go"),
        "package main\n\n\
         type Config struct {\n    Name string\n}\n\n\
         func NewConfig(name string) *Config {\n    return &Config{Name: name}\n}\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let files = read::get_all_files(&conn).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].language, "go");

    let symbols = read::get_symbols_for_file(&conn, files[0].id).unwrap();
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"Config"), "missing struct 'Config'");
    assert!(names.contains(&"NewConfig"), "missing function 'NewConfig'");
}

// ---------------------------------------------------------------------------
// 1. End-to-end indexing: C
// ---------------------------------------------------------------------------

#[test]
fn test_index_c_file() {
    let dir = TempDir::new().unwrap();

    fs::write(
        dir.path().join("main.c"),
        "#include <stdio.h>\n\n\
         struct Point {\n    int x;\n    int y;\n};\n\n\
         int add(int a, int b) {\n    return a + b;\n}\n\n\
         int main() {\n    printf(\"%d\\n\", add(1, 2));\n    return 0;\n}\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let files = read::get_all_files(&conn).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].language, "c");

    let symbols = read::get_symbols_for_file(&conn, files[0].id).unwrap();
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"add"), "missing function 'add'");
    assert!(names.contains(&"main"), "missing function 'main'");
}

// ---------------------------------------------------------------------------
// 1. End-to-end indexing: Java
// ---------------------------------------------------------------------------

#[test]
fn test_index_java_file() {
    let dir = TempDir::new().unwrap();

    fs::write(
        dir.path().join("App.java"),
        "public class App {\n\
             public static void main(String[] args) {\n\
                 System.out.println(\"Hello\");\n\
             }\n\
             \n\
             public int add(int a, int b) {\n\
                 return a + b;\n\
             }\n\
         }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let files = read::get_all_files(&conn).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].language, "java");

    let symbols = read::get_symbols_for_file(&conn, files[0].id).unwrap();
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"App"), "missing class 'App'");
}

// ---------------------------------------------------------------------------
// 1. End-to-end indexing: Multi-file project with cross-file imports
// ---------------------------------------------------------------------------

#[test]
fn test_index_multi_file_project_creates_edges() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("utils.ts"),
        "export function helper() { return 42; }\n",
    )
    .unwrap();

    fs::write(
        src.join("service.ts"),
        "import { helper } from './utils';\n\
         export function run() { return helper(); }\n",
    )
    .unwrap();

    fs::write(src.join("index.ts"), "export { run } from './service';\n").unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    assert_eq!(read::get_file_count(&conn).unwrap(), 3);

    let edges = read::get_all_edges(&conn).unwrap();
    assert!(
        edges.len() >= 2,
        "expected >=2 import edges, got {}",
        edges.len()
    );
}

// ---------------------------------------------------------------------------
// 2. Incremental indexing: skip unchanged
// ---------------------------------------------------------------------------

#[test]
fn test_incremental_skip_unchanged() {
    let dir = TempDir::new().unwrap();

    fs::write(dir.path().join("main.ts"), "export function main() {}\n").unwrap();
    fs::write(
        dir.path().join("helper.ts"),
        "export function helper() {}\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();
    assert_eq!(read::get_file_count(&conn).unwrap(), 2);

    let first_indexed_at: i64 = conn
        .query_row(
            "SELECT indexed_at FROM files WHERE path LIKE '%main.ts'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(1100));

    fs::write(
        dir.path().join("helper.ts"),
        "export function helper() { return 1; }\n",
    )
    .unwrap();

    index::full_index(&conn, dir.path(), false).unwrap();

    let reindexed_at: i64 = conn
        .query_row(
            "SELECT indexed_at FROM files WHERE path LIKE '%main.ts'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    assert_eq!(
        first_indexed_at, reindexed_at,
        "unchanged file should retain its indexed_at timestamp"
    );

    assert_eq!(read::get_file_count(&conn).unwrap(), 2);
}

// ---------------------------------------------------------------------------
// 2. Incremental indexing: detect deleted files
// ---------------------------------------------------------------------------

#[test]
fn test_incremental_detect_deleted() {
    let dir = TempDir::new().unwrap();

    fs::write(dir.path().join("a.ts"), "export const A = 1;\n").unwrap();
    fs::write(dir.path().join("b.ts"), "export const B = 2;\n").unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();
    assert_eq!(read::get_file_count(&conn).unwrap(), 2);

    fs::remove_file(dir.path().join("b.ts")).unwrap();

    index::full_index(&conn, dir.path(), true).unwrap();

    let files = read::get_all_files(&conn).unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.iter().any(|p| p.contains("a.ts")));
}

// ---------------------------------------------------------------------------
// 3. PageRank computation
// ---------------------------------------------------------------------------

#[test]
fn test_pagerank_highly_imported_file_ranks_higher() {
    let conn = setup();
    let core = insert_file(&conn, "src/core.rs");
    let a = insert_file(&conn, "src/a.rs");
    let b = insert_file(&conn, "src/b.rs");
    let c = insert_file(&conn, "src/c.rs");
    let leaf = insert_file(&conn, "src/leaf.rs");

    write::insert_edge(&conn, a, core, "import", None).unwrap();
    write::insert_edge(&conn, b, core, "import", None).unwrap();
    write::insert_edge(&conn, c, core, "import", None).unwrap();
    write::insert_edge(&conn, leaf, a, "import", None).unwrap();

    pagerank::compute_pagerank(&conn, &pagerank::PageRankConfig::default()).unwrap();

    let ranked = read::get_files_ranked(&conn, 10).unwrap();
    assert_eq!(ranked[0].path, "src/core.rs", "core.rs should rank first");
    assert!(
        ranked[0].pagerank > ranked.last().unwrap().pagerank,
        "top file should have higher rank than the last"
    );
}

#[test]
fn test_pagerank_ranks_sum_to_one() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");
    let c = insert_file(&conn, "c.rs");
    write::insert_edge(&conn, a, b, "import", None).unwrap();
    write::insert_edge(&conn, b, c, "import", None).unwrap();
    write::insert_edge(&conn, c, a, "import", None).unwrap();

    pagerank::compute_pagerank(&conn, &pagerank::PageRankConfig::default()).unwrap();

    let files = read::get_all_files(&conn).unwrap();
    let total: f64 = files.iter().map(|f| f.pagerank).sum();
    assert!(
        (total - 1.0).abs() < 0.01,
        "ranks should sum to ~1.0, got {total}"
    );
}

// ---------------------------------------------------------------------------
// 3. Blast radius
// ---------------------------------------------------------------------------

#[test]
fn test_blast_radius_transitive() {
    let conn = setup();
    let a = insert_file(&conn, "src/a.rs");
    let b = insert_file(&conn, "src/b.rs");
    let c = insert_file(&conn, "src/c.rs");

    write::insert_edge(&conn, a, b, "import", None).unwrap();
    write::insert_edge(&conn, b, c, "import", None).unwrap();

    let result = blast::blast_radius_for_file(&conn, c).unwrap();
    assert!(result.direct_importers.contains(&b));
    assert_eq!(
        result.transitive_count, 2,
        "C should be depended on by A and B"
    );
    assert!(result.transitive_importers.contains(&a));
    assert!(result.transitive_importers.contains(&b));
}

#[test]
fn test_blast_radius_no_importers() {
    let conn = setup();
    let a = insert_file(&conn, "src/a.rs");
    let b = insert_file(&conn, "src/b.rs");

    write::insert_edge(&conn, a, b, "import", None).unwrap();

    let result = blast::blast_radius_for_file(&conn, a).unwrap();
    assert!(result.direct_importers.is_empty());
    assert_eq!(result.transitive_count, 0);
}

#[test]
fn test_blast_radius_diamond() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");
    let c = insert_file(&conn, "c.rs");
    let d = insert_file(&conn, "d.rs");

    write::insert_edge(&conn, a, b, "import", None).unwrap();
    write::insert_edge(&conn, a, c, "import", None).unwrap();
    write::insert_edge(&conn, b, d, "import", None).unwrap();
    write::insert_edge(&conn, c, d, "import", None).unwrap();

    let result = blast::blast_radius_for_file(&conn, d).unwrap();
    assert_eq!(
        result.transitive_count, 3,
        "D should be depended on by A, B, C"
    );
}

// ---------------------------------------------------------------------------
// 4. Symbol reference tests
// ---------------------------------------------------------------------------

#[test]
fn test_get_symbol_references() {
    // Rewritten for the symbol-level refs implementation: the importer
    // side now needs a concrete referring symbol + an entry in the
    // `symbol_refs` table. A bare file-level import edge no longer
    // produces a reference, which is intentional - it was the proxy
    // behaviour the rewrite set out to kill.
    let conn = setup();
    let lib = insert_file(&conn, "src/lib.rs");
    let consumer = insert_file(&conn, "src/consumer.rs");

    let config_ids = write::insert_symbols(
        &conn,
        lib,
        &[SymbolInsert {
            name: "Config".to_string(),
            kind: "struct".to_string(),
            line_start: 1,
            line_end: 5,
            signature: Some("pub struct Config".to_string()),
            is_exported: true,
            shape_hash: None,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        }],
    )
    .unwrap();
    let caller_ids = write::insert_symbols(
        &conn,
        consumer,
        &[SymbolInsert {
            name: "use_config".to_string(),
            kind: "function".to_string(),
            line_start: 1,
            line_end: 3,
            signature: Some("fn use_config() -> Config".to_string()),
            is_exported: false,
            shape_hash: None,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        }],
    )
    .unwrap();
    write::insert_symbol_refs(&conn, &[(caller_ids[0], config_ids[0], "type")]).unwrap();

    let refs = read::get_symbol_references(&conn, "Config").unwrap();
    assert_eq!(refs.len(), 1);

    let (sym, def_file, importers) = &refs[0];
    assert_eq!(sym.name, "Config");
    assert_eq!(def_file.path, "src/lib.rs");
    assert_eq!(importers.len(), 1);
    assert_eq!(importers[0].1.path, "src/consumer.rs");
    // Synthetic edge kind set by the new read::get_symbol_references path.
    assert_eq!(importers[0].0.kind, "symbol_ref");
}

#[test]
fn test_get_symbol_references_no_match() {
    let conn = setup();
    let refs = read::get_symbol_references(&conn, "NonExistent").unwrap();
    assert!(refs.is_empty());
}

// ---------------------------------------------------------------------------
// 5. Toolchain detection
// ---------------------------------------------------------------------------

#[test]
fn test_detect_rust_toolchain() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

    let tc = toolchain::detect_toolchain(dir.path()).unwrap();
    assert_eq!(tc.name, "rust");
    assert_eq!(tc.build_tool, "cargo");
    assert_eq!(tc.test_cmd, vec!["cargo", "test"]);
    assert!(tc.lint_cmd.is_some());
}

#[test]
fn test_detect_go_toolchain() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("go.mod"), "module example.com/test").unwrap();

    let tc = toolchain::detect_toolchain(dir.path()).unwrap();
    assert_eq!(tc.name, "go");
    assert_eq!(tc.build_tool, "go");
}

#[test]
fn test_detect_node_npm_toolchain() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("package.json"),
        r#"{"scripts":{"test":"jest","build":"tsc"}}"#,
    )
    .unwrap();

    let tc = toolchain::detect_toolchain(dir.path()).unwrap();
    assert_eq!(tc.name, "node");
    assert_eq!(tc.build_tool, "npm");
}

#[test]
fn test_detect_node_bun_toolchain() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("package.json"), "{}").unwrap();
    fs::write(dir.path().join("bun.lockb"), "").unwrap();

    let tc = toolchain::detect_toolchain(dir.path()).unwrap();
    assert_eq!(tc.name, "node");
    assert_eq!(tc.build_tool, "bun");
}

#[test]
fn test_detect_node_yarn_toolchain() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("package.json"), "{}").unwrap();
    fs::write(dir.path().join("yarn.lock"), "").unwrap();

    let tc = toolchain::detect_toolchain(dir.path()).unwrap();
    assert_eq!(tc.name, "node");
    assert_eq!(tc.build_tool, "yarn");
}

#[test]
fn test_detect_python_toolchain() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("pyproject.toml"), "[project]").unwrap();

    let tc = toolchain::detect_toolchain(dir.path()).unwrap();
    assert_eq!(tc.name, "python");
    assert_eq!(tc.build_tool, "pip");
}

#[test]
fn test_detect_ruby_toolchain() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("Gemfile"), "source 'https://rubygems.org'").unwrap();

    let tc = toolchain::detect_toolchain(dir.path()).unwrap();
    assert_eq!(tc.name, "ruby");
    assert_eq!(tc.build_tool, "bundle");
}

#[test]
fn test_detect_make_toolchain() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("Makefile"), "build:\n\techo build").unwrap();

    let tc = toolchain::detect_toolchain(dir.path()).unwrap();
    assert_eq!(tc.name, "make");
    assert_eq!(tc.build_tool, "make");
}

#[test]
fn test_detect_no_toolchain() {
    let dir = TempDir::new().unwrap();
    let tc = toolchain::detect_toolchain(dir.path());
    assert!(tc.is_none());
}

// ---------------------------------------------------------------------------
// 6. FTS search tests
// ---------------------------------------------------------------------------

#[test]
fn test_fts_search_across_languages() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("config.rs"),
        "pub struct AppConfig {\n    pub debug: bool,\n}\n",
    )
    .unwrap();

    fs::write(
        src.join("config.ts"),
        "export interface AppConfig {\n    debug: boolean;\n}\n",
    )
    .unwrap();

    fs::write(
        src.join("config.py"),
        "class AppConfig:\n    def __init__(self):\n        self.debug = False\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let results = read::search_symbols_fts(&conn, "AppConfig", 20).unwrap();
    assert!(
        results.len() >= 3,
        "expected FTS hits in at least 3 languages, got {}",
        results.len()
    );

    let file_paths: HashSet<&str> = results.iter().map(|(_, p)| p.as_str()).collect();
    assert!(file_paths.iter().any(|p| p.contains("config.rs")));
    assert!(file_paths.iter().any(|p| p.contains("config.ts")));
    assert!(file_paths.iter().any(|p| p.contains("config.py")));
}

#[test]
fn test_fts_prefix_search() {
    let conn = setup();
    let f = insert_file(&conn, "src/db.rs");
    write::insert_symbols(
        &conn,
        f,
        &[
            SymbolInsert {
                name: "DatabasePool".to_string(),
                kind: "struct".to_string(),
                line_start: 1,
                line_end: 5,
                signature: None,
                is_exported: true,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            },
            SymbolInsert {
                name: "DatabaseConfig".to_string(),
                kind: "struct".to_string(),
                line_start: 7,
                line_end: 10,
                signature: None,
                is_exported: true,
                shape_hash: None,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            },
        ],
    )
    .unwrap();
    write::sync_fts(&conn).unwrap();

    let results = read::search_symbols_fts(&conn, "Database*", 10).unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn test_fts_no_results() {
    let conn = setup();
    let f = insert_file(&conn, "src/lib.rs");
    write::insert_symbols(
        &conn,
        f,
        &[SymbolInsert {
            name: "hello".to_string(),
            kind: "function".to_string(),
            line_start: 1,
            line_end: 3,
            signature: None,
            is_exported: true,
            shape_hash: None,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        }],
    )
    .unwrap();
    write::sync_fts(&conn).unwrap();

    let results = read::search_symbols_fts(&conn, "zzz_no_match", 10).unwrap();
    assert!(results.is_empty());
}

// ---------------------------------------------------------------------------
// 7. Co-change analysis tests (with git)
// ---------------------------------------------------------------------------

fn init_git_repo(dir: &Path) -> git2::Repository {
    git2::Repository::init(dir).unwrap()
}

fn make_commit(repo: &git2::Repository, dir: &Path, files: &[&str], message: &str) {
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    let mut index = repo.index().unwrap();

    for file in files {
        let file_path = dir.join(file);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let existing = fs::read_to_string(&file_path).unwrap_or_default();
        fs::write(&file_path, format!("{existing}\n// edit")).unwrap();
        index.add_path(Path::new(file)).unwrap();
    }

    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();

    let parent = repo
        .head()
        .ok()
        .and_then(|h| h.target().and_then(|oid| repo.find_commit(oid).ok()));

    match parent {
        Some(p) => {
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&p])
                .unwrap();
        }
        None => {
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[])
                .unwrap();
        }
    }
}

#[test]
fn test_cochange_with_git_history() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let repo = init_git_repo(root);
    let conn = setup();

    make_commit(&repo, root, &["src/a.rs", "src/b.rs"], "commit 1");
    make_commit(&repo, root, &["src/a.rs", "src/b.rs"], "commit 2");
    make_commit(&repo, root, &["src/a.rs", "src/c.rs"], "commit 3");

    write::upsert_file(&conn, "src/a.rs", 1000, 100, "rust", 10).unwrap();
    write::upsert_file(&conn, "src/b.rs", 1000, 100, "rust", 10).unwrap();
    write::upsert_file(&conn, "src/c.rs", 1000, 100, "rust", 10).unwrap();

    use qartez_mcp::git::cochange::{CoChangeConfig, analyze_cochanges};
    analyze_cochanges(&conn, root, &CoChangeConfig::default()).unwrap();

    let file_a = read::get_file_by_path(&conn, "src/a.rs").unwrap().unwrap();
    let cochanges = read::get_cochanges(&conn, file_a.id, 10).unwrap();
    assert!(!cochanges.is_empty(), "should have co-change partners");

    let partner_paths: Vec<&str> = cochanges.iter().map(|(_, f)| f.path.as_str()).collect();
    assert!(
        partner_paths.contains(&"src/b.rs"),
        "a.rs should co-change with b.rs"
    );
}

#[test]
fn test_cochange_non_git_dir_succeeds() {
    let dir = TempDir::new().unwrap();
    let conn = setup();

    use qartez_mcp::git::cochange::{CoChangeConfig, analyze_cochanges};
    let result = analyze_cochanges(&conn, dir.path(), &CoChangeConfig::default());
    assert!(result.is_ok(), "non-git dir should not fail");
}

// ---------------------------------------------------------------------------
// 8. Unused exports detection
// ---------------------------------------------------------------------------

#[test]
fn test_unused_exported_symbols() {
    let conn = setup();
    let lib = insert_file(&conn, "src/lib.rs");
    let consumer = insert_file(&conn, "src/consumer.rs");
    let orphan = insert_file(&conn, "src/orphan.rs");

    write::insert_symbols(
        &conn,
        lib,
        &[SymbolInsert {
            name: "used_fn".to_string(),
            kind: "function".to_string(),
            line_start: 1,
            line_end: 5,
            signature: None,
            is_exported: true,
            shape_hash: None,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        }],
    )
    .unwrap();
    write::insert_symbols(
        &conn,
        orphan,
        &[SymbolInsert {
            name: "orphan_fn".to_string(),
            kind: "function".to_string(),
            line_start: 1,
            line_end: 5,
            signature: None,
            is_exported: true,
            shape_hash: None,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        }],
    )
    .unwrap();

    write::insert_edge(&conn, consumer, lib, "import", None).unwrap();

    let unused = read::get_unused_exports_page(&conn, i64::MAX, 0).unwrap();
    let unused_names: Vec<&str> = unused.iter().map(|(s, _)| s.name.as_str()).collect();
    assert!(
        unused_names.contains(&"orphan_fn"),
        "orphan_fn should be unused"
    );
    assert!(
        !unused_names.contains(&"used_fn"),
        "used_fn should NOT be unused"
    );
}

// ---------------------------------------------------------------------------
// 9. Edge and file CRUD tests
// ---------------------------------------------------------------------------

#[test]
fn test_upsert_file_updates_existing() {
    let conn = setup();
    let id1 = write::upsert_file(&conn, "src/a.rs", 1000, 100, "rust", 10).unwrap();
    let id2 = write::upsert_file(&conn, "src/a.rs", 2000, 200, "rust", 20).unwrap();
    assert_eq!(id1, id2, "upsert should return same ID");

    let file = read::get_file_by_id(&conn, id1).unwrap().unwrap();
    assert_eq!(file.size_bytes, 200);
    assert_eq!(file.line_count, 20);
}

#[test]
fn test_delete_file_cascades_symbols_and_edges() {
    let conn = setup();
    let f1 = insert_file(&conn, "src/a.rs");
    let f2 = insert_file(&conn, "src/b.rs");

    write::insert_symbols(
        &conn,
        f1,
        &[SymbolInsert {
            name: "foo".to_string(),
            kind: "function".to_string(),
            line_start: 1,
            line_end: 5,
            signature: None,
            is_exported: false,
            shape_hash: None,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        }],
    )
    .unwrap();
    write::insert_edge(&conn, f1, f2, "import", None).unwrap();

    write::delete_file_data(&conn, f1).unwrap();

    assert_eq!(read::get_symbol_count(&conn).unwrap(), 0);
    assert!(read::get_all_edges(&conn).unwrap().is_empty());
}

#[test]
fn test_duplicate_edge_is_ignored() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");

    write::insert_edge(&conn, a, b, "import", Some("crate::b")).unwrap();
    write::insert_edge(&conn, a, b, "import", Some("crate::b")).unwrap();

    let edges = read::get_all_edges(&conn).unwrap();
    assert_eq!(edges.len(), 1);
}

// ---------------------------------------------------------------------------
// 10. Meta key/value storage
// ---------------------------------------------------------------------------

#[test]
fn test_meta_set_and_get() {
    let conn = setup();
    write::set_meta(&conn, "version", "1").unwrap();
    assert_eq!(
        read::get_meta(&conn, "version").unwrap(),
        Some("1".to_string())
    );

    write::set_meta(&conn, "version", "2").unwrap();
    assert_eq!(
        read::get_meta(&conn, "version").unwrap(),
        Some("2".to_string())
    );
}

#[test]
fn test_meta_missing_key() {
    let conn = setup();
    assert!(read::get_meta(&conn, "nonexistent").unwrap().is_none());
}

// ---------------------------------------------------------------------------
// 11. Full index sets last_index meta
// ---------------------------------------------------------------------------

#[test]
fn test_full_index_sets_last_index_meta() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("main.ts"), "export function main() {}\n").unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let last = read::get_meta(&conn, "last_index").unwrap();
    assert!(
        last.is_some(),
        "last_index meta should be set after indexing"
    );
    let ts: u64 = last.unwrap().parse().unwrap();
    assert!(ts > 0, "timestamp should be positive");
}

// ---------------------------------------------------------------------------
// 12. FTS sync after indexing
// ---------------------------------------------------------------------------

#[test]
fn test_fts_synced_after_full_index() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("lib.rs"), "pub fn unique_symbol_xyz() {}\n").unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let results = read::search_symbols_fts(&conn, "unique_symbol_xyz", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0.name, "unique_symbol_xyz");
}

// ---------------------------------------------------------------------------
// 13. Find symbol by name
// ---------------------------------------------------------------------------

#[test]
fn test_find_symbol_by_name_across_files() {
    let conn = setup();
    let f1 = insert_file(&conn, "src/a.rs");
    let f2 = insert_file(&conn, "src/b.rs");

    write::insert_symbols(
        &conn,
        f1,
        &[SymbolInsert {
            name: "process".to_string(),
            kind: "function".to_string(),
            line_start: 1,
            line_end: 5,
            signature: Some("pub fn process()".to_string()),
            is_exported: true,
            shape_hash: None,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        }],
    )
    .unwrap();
    write::insert_symbols(
        &conn,
        f2,
        &[SymbolInsert {
            name: "process".to_string(),
            kind: "function".to_string(),
            line_start: 1,
            line_end: 3,
            signature: Some("fn process()".to_string()),
            is_exported: false,
            shape_hash: None,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        }],
    )
    .unwrap();

    let results = read::find_symbol_by_name(&conn, "process").unwrap();
    assert_eq!(results.len(), 2);

    let paths: HashSet<&str> = results.iter().map(|(_, f)| f.path.as_str()).collect();
    assert!(paths.contains("src/a.rs"));
    assert!(paths.contains("src/b.rs"));
}

// ---------------------------------------------------------------------------
// 14. Stale files detection
// ---------------------------------------------------------------------------

#[test]
fn test_stale_files() {
    let conn = setup();
    let indexed = insert_file(&conn, "src/indexed.rs");
    insert_file(&conn, "src/stale.rs");

    write::insert_symbols(
        &conn,
        indexed,
        &[SymbolInsert {
            name: "main".to_string(),
            kind: "function".to_string(),
            line_start: 1,
            line_end: 5,
            signature: None,
            is_exported: false,
            shape_hash: None,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        }],
    )
    .unwrap();

    let stale = read::get_stale_files(&conn).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].path, "src/stale.rs");
}

// ---------------------------------------------------------------------------
// 15. get_or_create_file
// ---------------------------------------------------------------------------

#[test]
fn test_get_or_create_file() {
    let conn = setup();

    let id1 = write::get_or_create_file(&conn, "src/new.rs").unwrap();
    assert!(id1 > 0);

    let id2 = write::get_or_create_file(&conn, "src/new.rs").unwrap();
    assert_eq!(id1, id2, "should return existing file id");

    let file = read::get_file_by_path(&conn, "src/new.rs")
        .unwrap()
        .unwrap();
    assert_eq!(file.language, "rust");
}

// ---------------------------------------------------------------------------
// 16. Cochange upsert counting
// ---------------------------------------------------------------------------

#[test]
fn test_cochange_increments_count() {
    let conn = setup();
    let a = insert_file(&conn, "a.rs");
    let b = insert_file(&conn, "b.rs");

    write::upsert_cochange(&conn, a, b).unwrap();
    write::upsert_cochange(&conn, a, b).unwrap();
    write::upsert_cochange(&conn, a, b).unwrap();

    let cochanges = read::get_cochanges(&conn, a, 10).unwrap();
    assert_eq!(cochanges.len(), 1);
    assert_eq!(cochanges[0].0.count, 3);
}

// ---------------------------------------------------------------------------
// 17. Schema idempotent creation
// ---------------------------------------------------------------------------

#[test]
fn test_schema_idempotent() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    schema::create_schema(&conn).unwrap();

    let count = read::get_file_count(&conn).unwrap();
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// 18. Edge queries: from and to
// ---------------------------------------------------------------------------

#[test]
fn test_get_edges_to_and_from() {
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
}

// ---------------------------------------------------------------------------
// 19. Run command
// ---------------------------------------------------------------------------

#[test]
fn test_run_command_success() {
    let dir = TempDir::new().unwrap();
    let cmd = vec!["echo".to_string(), "hello world".to_string()];
    let (code, output) = toolchain::run_command(dir.path(), &cmd, None, 10).unwrap();
    assert_eq!(code, 0);
    assert!(output.contains("hello world"));
}

#[test]
fn test_run_command_with_filter() {
    let dir = TempDir::new().unwrap();
    let cmd = vec!["echo".to_string()];
    let (code, output) = toolchain::run_command(dir.path(), &cmd, Some("filtered"), 10).unwrap();
    assert_eq!(code, 0);
    assert!(output.contains("filtered"));
}

#[test]
fn test_run_command_nonexistent_fails() {
    let dir = TempDir::new().unwrap();
    let cmd = vec!["nonexistent_binary_xyz".to_string()];
    let result = toolchain::run_command(dir.path(), &cmd, None, 10);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// 20. End-to-end: index + pagerank + blast radius
// ---------------------------------------------------------------------------

#[test]
fn test_end_to_end_index_pagerank_blast() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("core.ts"),
        "export function coreUtil() { return 42; }\n",
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
    index::full_index(&conn, dir.path(), false).unwrap();

    assert_eq!(read::get_file_count(&conn).unwrap(), 3);
    assert!(read::get_all_edges(&conn).unwrap().len() >= 2);

    pagerank::compute_pagerank(&conn, &pagerank::PageRankConfig::default()).unwrap();

    let ranked = read::get_files_ranked(&conn, 10).unwrap();
    assert!(ranked[0].pagerank > 0.0);

    let core_file = read::get_file_by_path(&conn, "src/core.ts")
        .unwrap()
        .unwrap();
    let result = blast::blast_radius_for_file(&conn, core_file.id).unwrap();
    assert!(
        result.transitive_count >= 2,
        "core.ts should be depended on by service and handler"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_call_tool_by_name_dispatches_to_qartez_stats() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub fn hi() -> i32 { 1 }\n").unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name("qartez_stats", json!({}))
        .expect("qartez_stats dispatch");
    assert!(
        result.contains("files="),
        "expected stats header, got: {result}"
    );

    let find = server
        .call_tool_by_name("qartez_find", json!({ "name": "hi" }))
        .expect("qartez_find dispatch");
    assert!(find.contains("hi"), "expected 'hi' in result, got: {find}");

    let err = server
        .call_tool_by_name("nonexistent_tool", json!({}))
        .unwrap_err();
    assert!(
        err.contains("unknown tool"),
        "expected unknown-tool error, got: {err}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_qartez_read_file_path_alone_reads_whole_file() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    let lib_contents =
        "// header comment\nuse std::io;\n\npub fn one() -> i32 { 1 }\npub fn two() -> i32 { 2 }\n";
    fs::write(src.join("lib.rs"), lib_contents).unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);

    // file_path alone returns the whole file.
    let whole = server
        .call_tool_by_name("qartez_read", json!({ "file_path": "src/lib.rs" }))
        .expect("qartez_read file_path alone should succeed");
    assert!(
        whole.contains("header comment"),
        "whole-file read should include the header comment, got: {whole}"
    );
    assert!(
        whole.contains("pub fn one"),
        "whole-file read should include first symbol, got: {whole}"
    );
    assert!(
        whole.contains("pub fn two"),
        "whole-file read should include second symbol, got: {whole}"
    );
    assert!(
        whole.starts_with("src/lib.rs L1-5"),
        "expected header 'src/lib.rs L1-5', got: {whole}"
    );

    // start_line + limit still pages correctly.
    let sliced = server
        .call_tool_by_name(
            "qartez_read",
            json!({ "file_path": "src/lib.rs", "start_line": 2, "limit": 2 }),
        )
        .expect("qartez_read with limit should succeed");
    assert!(
        sliced.contains("use std::io"),
        "slice starting at line 2 should include `use std::io`, got: {sliced}"
    );
    assert!(
        !sliced.contains("pub fn two"),
        "2-line slice from line 2 must not contain line 5, got: {sliced}"
    );

    // max_bytes cap yields a truncation marker rather than unbounded output.
    let truncated = server
        .call_tool_by_name(
            "qartez_read",
            json!({ "file_path": "src/lib.rs", "max_bytes": 40 }),
        )
        .expect("qartez_read with tiny cap should succeed");
    assert!(
        truncated.contains("truncated"),
        "tiny max_bytes should trigger truncation marker, got: {truncated}"
    );

    // start_line beyond EOF is a clear error, not silent empty output.
    let oob = server
        .call_tool_by_name(
            "qartez_read",
            json!({ "file_path": "src/lib.rs", "start_line": 999 }),
        )
        .unwrap_err();
    assert!(
        oob.contains("exceeds file length"),
        "out-of-range start_line should error, got: {oob}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_qartez_read_split_preserves_all_modes() {
    // Behavioral parity test for the qartez_read refactor: every mode the
    // monolithic version supported must still produce equivalent output
    // through the new dispatcher + helpers.
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("a.rs"),
        "// header\npub fn alpha() -> i32 { 1 }\npub fn beta() -> i32 { 2 }\n",
    )
    .unwrap();
    fs::write(
        src.join("b.rs"),
        "pub fn gamma() -> i32 { 3 }\npub fn delta() -> i32 { 4 }\n",
    )
    .unwrap();
    fs::write(
        src.join("dup.rs"),
        "pub fn alpha() -> i32 { 99 }\n", // shadows alpha from a.rs
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);

    // 1. Single-symbol read by symbol_name.
    let single = server
        .call_tool_by_name("qartez_read", json!({ "symbol_name": "beta" }))
        .expect("single-symbol read should succeed");
    assert!(single.contains("beta"), "single read missing symbol header");
    assert!(single.contains("pub fn beta"), "single read missing body");

    // 2. Batch read via `symbols=[...]`.
    let batch = server
        .call_tool_by_name(
            "qartez_read",
            json!({ "symbols": ["alpha", "gamma", "delta"] }),
        )
        .expect("batch read should succeed");
    assert!(batch.contains("alpha"), "batch missing alpha");
    assert!(batch.contains("gamma"), "batch missing gamma");
    assert!(batch.contains("delta"), "batch missing delta");

    // 3. Batch with one missing - partial-hit returns output + footer.
    let partial = server
        .call_tool_by_name(
            "qartez_read",
            json!({ "symbols": ["alpha", "no_such_symbol"] }),
        )
        .expect("partial batch should not error");
    assert!(partial.contains("alpha"), "partial missing the hit");
    assert!(
        partial.contains("not found") && partial.contains("no_such_symbol"),
        "partial batch must surface the missing name in footer"
    );

    // 4. file_path filter disambiguates between two `alpha` definitions.
    let filtered = server
        .call_tool_by_name(
            "qartez_read",
            json!({ "symbol_name": "alpha", "file_path": "dup.rs" }),
        )
        .expect("filtered single-symbol read should succeed");
    assert!(
        filtered.contains("dup.rs") && filtered.contains("99"),
        "file_path filter must pick the dup.rs definition (= 99), got: {filtered}"
    );
    assert!(
        !filtered.contains("a.rs"),
        "filter must exclude a.rs alpha, got: {filtered}"
    );

    // 5. All-missing batch returns Err (no partial hits to fall back on).
    let err = server
        .call_tool_by_name("qartez_read", json!({ "symbols": ["nope1", "nope2"] }))
        .unwrap_err();
    assert!(
        err.contains("No symbol found") || err.contains("not found"),
        "all-missing batch should error, got: {err}"
    );

    // 6. Missing both symbols and file_path is a clear error.
    let err = server
        .call_tool_by_name("qartez_read", json!({}))
        .unwrap_err();
    assert!(
        err.contains("required") || err.contains("Either"),
        "empty params must error, got: {err}"
    );

    // 7. Empty symbols list is treated as missing.
    let err = server
        .call_tool_by_name("qartez_read", json!({ "symbols": [] }))
        .unwrap_err();
    assert!(
        err.contains("required") || err.contains("Either"),
        "empty symbols list must error, got: {err}"
    );

    // 8. Symbols list with only empty strings is also missing.
    let err = server
        .call_tool_by_name("qartez_read", json!({ "symbols": ["", ""] }))
        .unwrap_err();
    assert!(
        err.contains("required") || err.contains("non-empty"),
        "all-empty symbols list must error, got: {err}"
    );

    // 9. context_lines expands the slice on the start side.
    let with_ctx = server
        .call_tool_by_name(
            "qartez_read",
            json!({ "symbol_name": "alpha", "file_path": "a.rs", "context_lines": 1 }),
        )
        .expect("context_lines read should succeed");
    assert!(
        with_ctx.contains("// header"),
        "context_lines=1 must include the line above alpha (// header), got: {with_ctx}"
    );

    // 10. Tiny max_bytes triggers a truncation marker for batch mode.
    let trunc = server
        .call_tool_by_name(
            "qartez_read",
            json!({ "symbols": ["alpha", "beta", "gamma", "delta"], "max_bytes": 100 }),
        )
        .expect("truncated batch should not error");
    assert!(
        trunc.contains("truncated") || trunc.contains("skipped"),
        "tiny max_bytes must trigger truncation marker for batch, got: {trunc}"
    );
}

// ---------------------------------------------------------------------------
// qartez-guard PreToolUse hook - end-to-end
// ---------------------------------------------------------------------------

mod guard_binary {
    use std::io::Write;
    use std::process::{Command, Stdio};

    use qartez_mcp::graph::pagerank;
    use qartez_mcp::guard;
    use qartez_mcp::index;
    use qartez_mcp::storage;
    use rusqlite::Connection;
    use tempfile::TempDir;

    /// Build a tiny indexed project: `hub.rs` imported twice so it has a
    /// non-zero blast radius. Returns (project_dir, db_path, rel_hub_path).
    fn indexed_project() -> (TempDir, std::path::PathBuf, String) {
        let dir = TempDir::new().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir src");
        std::fs::write(src.join("hub.rs"), "pub fn shared() -> u32 { 42 }\n").expect("write hub");
        std::fs::write(
            src.join("a.rs"),
            "use crate::hub::shared;\npub fn a() -> u32 { shared() }\n",
        )
        .expect("write a");
        std::fs::write(
            src.join("b.rs"),
            "use crate::hub::shared;\npub fn b() -> u32 { shared() + 1 }\n",
        )
        .expect("write b");
        std::fs::write(src.join("lib.rs"), "pub mod a;\npub mod b;\npub mod hub;\n")
            .expect("write lib");
        // Cargo.toml so detect_project_root can find it if the guard walks
        // upward from the file_path.
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname=\"fx\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
        )
        .expect("write Cargo.toml");

        let db_path = dir.path().join(".qartez").join("index.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).expect("mkdir .qartez");
        let conn: Connection = storage::open_db(&db_path).expect("open db");
        index::full_index(&conn, dir.path(), false).expect("index");
        pagerank::compute_pagerank(&conn, &Default::default()).expect("pagerank");
        drop(conn);

        (dir, db_path, "src/hub.rs".to_string())
    }

    fn run_guard(project_dir: &std::path::Path, payload: &str, extra_args: &[&str]) -> String {
        let exe = env!("CARGO_BIN_EXE_qartez-guard");
        let mut cmd = Command::new(exe);
        cmd.arg("--project-root")
            .arg(project_dir)
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove("QARTEZ_GUARD_DISABLE")
            .env_remove("QARTEZ_GUARD_PAGERANK_MIN")
            .env_remove("QARTEZ_GUARD_BLAST_MIN")
            .env_remove("QARTEZ_GUARD_ACK_TTL_SECS");
        let mut child = cmd.spawn().expect("spawn qartez-guard");
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(payload.as_bytes())
            .expect("write stdin");
        let out = child.wait_with_output().expect("wait guard");
        assert!(
            out.status.success(),
            "qartez-guard must exit 0 (fail-open): status={:?} stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8 stdout")
    }

    #[test]
    fn denies_hot_file_without_ack() {
        let (dir, _db, hub) = indexed_project();
        let abs = dir.path().join(&hub);
        let payload = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": abs},
            "cwd": dir.path(),
        })
        .to_string();
        // Thresholds set low so a 2-importer fixture trips the guard
        // deterministically, independent of actual PageRank values.
        let out = run_guard(
            dir.path(),
            &payload,
            &["--pagerank-min", "0", "--blast-min", "1"],
        );
        assert!(
            out.contains("permissionDecision"),
            "expected deny JSON, got: {out}"
        );
        assert!(out.contains("deny"));
        assert!(out.contains("qartez_impact"));
        assert!(out.contains(&hub));
    }

    #[test]
    fn allows_hot_file_after_ack() {
        let (dir, _db, hub) = indexed_project();
        guard::touch_ack(dir.path(), &hub);

        let abs = dir.path().join(&hub);
        let payload = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": abs},
            "cwd": dir.path(),
        })
        .to_string();
        let out = run_guard(
            dir.path(),
            &payload,
            &["--pagerank-min", "0", "--blast-min", "1"],
        );
        assert!(
            out.trim().is_empty(),
            "expected empty (allow) stdout after ack, got: {out}"
        );
    }

    #[test]
    fn allows_non_edit_tools() {
        let (dir, _db, hub) = indexed_project();
        let abs = dir.path().join(&hub);
        let payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": format!("ls {}", abs.display())},
            "cwd": dir.path(),
        })
        .to_string();
        let out = run_guard(
            dir.path(),
            &payload,
            &["--pagerank-min", "0", "--blast-min", "1"],
        );
        assert!(out.trim().is_empty(), "Bash tool must not be guarded");
    }

    #[test]
    fn allows_unindexed_file() {
        let (dir, _db, _hub) = indexed_project();
        let abs = dir.path().join("src/new_file.rs");
        let payload = serde_json::json!({
            "tool_name": "Write",
            "tool_input": {"file_path": abs},
            "cwd": dir.path(),
        })
        .to_string();
        let out = run_guard(
            dir.path(),
            &payload,
            &["--pagerank-min", "0", "--blast-min", "1"],
        );
        assert!(
            out.trim().is_empty(),
            "creating a new file (not in index) must not be blocked"
        );
    }
}

// ---------------------------------------------------------------------------
// Type hierarchy: Rust trait impls
// ---------------------------------------------------------------------------

#[test]
fn test_type_hierarchy_rust_trait_impl() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("lib.rs"),
        "\
pub trait Greet { fn greet(&self); }
pub struct Alice;
pub struct Bob;
impl Greet for Alice { fn greet(&self) {} }
impl Greet for Bob { fn greet(&self) {} }
",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let subs = read::get_subtypes(&conn, "Greet").unwrap();
    let sub_names: HashSet<&str> = subs.iter().map(|(h, _)| h.sub_name.as_str()).collect();
    assert!(
        sub_names.contains("Alice"),
        "Alice should implement Greet, got: {sub_names:?}"
    );
    assert!(
        sub_names.contains("Bob"),
        "Bob should implement Greet, got: {sub_names:?}"
    );
    assert_eq!(subs.len(), 2);

    let supers = read::get_supertypes(&conn, "Alice").unwrap();
    assert_eq!(supers.len(), 1);
    assert_eq!(supers[0].0.super_name, "Greet");
    assert_eq!(supers[0].0.kind, "implements");
}

#[test]
fn test_type_hierarchy_rust_multiple_traits() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("lib.rs"),
        "\
pub trait Read { fn read(&self); }
pub trait Write { fn write(&self); }
pub struct File;
impl Read for File { fn read(&self) {} }
impl Write for File { fn write(&self) {} }
",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let supers = read::get_supertypes(&conn, "File").unwrap();
    let super_names: HashSet<&str> = supers.iter().map(|(h, _)| h.super_name.as_str()).collect();
    assert_eq!(super_names.len(), 2, "File should implement Read + Write");
    assert!(super_names.contains("Read"));
    assert!(super_names.contains("Write"));
}

#[test]
fn test_type_hierarchy_inherent_impl_no_relation() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("lib.rs"),
        "\
pub struct Foo;
impl Foo { pub fn new() -> Self { Foo } }
",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let subs = read::get_subtypes(&conn, "Foo").unwrap();
    assert!(
        subs.is_empty(),
        "inherent impl should not create type hierarchy rows"
    );
}

#[test]
fn test_type_hierarchy_insert_and_cascade() {
    let conn = setup();
    let file_id = insert_file(&conn, "src/example.rs");

    write::insert_type_relations(
        &conn,
        file_id,
        &[
            ("Alice".into(), "Greet".into(), "implements".into(), 10),
            ("Bob".into(), "Greet".into(), "implements".into(), 20),
        ],
    )
    .unwrap();

    let subs = read::get_subtypes(&conn, "Greet").unwrap();
    assert_eq!(subs.len(), 2);

    // Delete the file; cascade should remove type_hierarchy rows
    write::delete_file_data(&conn, file_id).unwrap();
    let subs_after = read::get_subtypes(&conn, "Greet").unwrap();
    assert!(
        subs_after.is_empty(),
        "cascade delete should remove type_hierarchy rows"
    );
}

#[test]
fn test_type_hierarchy_incremental_reindex() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("lib.rs"),
        "\
pub trait T { fn t(&self); }
pub struct A;
impl T for A { fn t(&self) {} }
",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let subs = read::get_subtypes(&conn, "T").unwrap();
    assert_eq!(subs.len(), 1);

    // Add a second implementor and re-index incrementally
    fs::write(
        dir.path().join("lib.rs"),
        "\
pub trait T { fn t(&self); }
pub struct A;
pub struct B;
impl T for A { fn t(&self) {} }
impl T for B { fn t(&self) {} }
",
    )
    .unwrap();

    index::incremental_index(&conn, dir.path(), &[dir.path().join("lib.rs")], &[]).unwrap();

    let subs = read::get_subtypes(&conn, "T").unwrap();
    assert_eq!(
        subs.len(),
        2,
        "incremental reindex should pick up new implementor"
    );
}

// ---------------------------------------------------------------------------
// Security scanner
// ---------------------------------------------------------------------------

#[cfg(feature = "benchmark")]
#[test]
fn test_security_scan_detects_hardcoded_secret() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("config.rs"),
        "pub fn load_config() {\n    let password = \"hunter2\";\n}\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name("qartez_security", json!({}))
        .expect("qartez_security dispatch");

    assert!(
        result.contains("hardcoded-secret"),
        "expected hardcoded-secret finding, got: {result}"
    );
    assert!(
        result.contains("Security Scan"),
        "expected scan header, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_security_scan_empty_when_clean() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub fn safe_fn() -> i32 { 42 }\n").unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name("qartez_security", json!({}))
        .expect("qartez_security dispatch");

    assert!(
        result.contains("No security findings"),
        "clean code should have no findings, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_security_scan_severity_filter() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    // unwrap() is Low severity, hardcoded secret is Critical.
    fs::write(
        src.join("lib.rs"),
        "pub fn risky() {\n    let x = Some(1).unwrap();\n    let password = \"s3cret!!\";\n}\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);

    // With severity=critical, only the hardcoded secret should appear.
    let result = server
        .call_tool_by_name("qartez_security", json!({"severity": "critical"}))
        .expect("qartez_security dispatch");

    assert!(
        result.contains("hardcoded-secret"),
        "critical filter should include SEC001, got: {result}"
    );
    assert!(
        !result.contains("unwrap-in-exported"),
        "critical filter should exclude Low-severity unwrap, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_security_scan_skips_inline_cfg_test_modules() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    // `prod_path` is a real SEC005 hit; `test_resolve_traversal` lives
    // inside `#[cfg(test)] mod tests {}` and must not surface as a
    // finding when include_tests is left at its default of false.
    fs::write(
        src.join("lib.rs"),
        "pub fn prod_path() {\n    \
             let p = \"../etc/passwd\";\n    \
             let _ = std::fs::read(p);\n\
         }\n\
         \n\
         #[cfg(test)]\n\
         mod tests {\n    \
             #[test]\n    \
             fn test_resolve_traversal() {\n        \
                 let p = \"../../../sneaky\";\n        \
                 assert!(p.contains(\"..\"));\n    \
             }\n\
         }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name("qartez_security", json!({"category": "injection"}))
        .expect("qartez_security dispatch");

    assert!(
        result.contains("prod_path"),
        "production SEC005 must surface, got: {result}"
    );
    // Symbol names get truncated in the rendered table, so the negative
    // assertion has to look at the snippet text, which is verbatim and
    // unique to the inline test fixture.
    assert!(
        !result.contains("../../../sneaky"),
        "inline #[cfg(test)] symbol must be skipped, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_security_scan_include_tests_surfaces_inline_test_symbols() {
    // When include_tests=true, the cfg(test) filter must be a no-op:
    // both production and inline-test SEC005 hits must appear.
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub fn prod_path() {\n    let p = \"../etc/passwd\";\n    let _ = std::fs::read(p);\n}\n\
         \n\
         #[cfg(test)]\n\
         mod tests {\n    \
             #[test]\n    \
             fn test_resolve_traversal() {\n        \
                 let p = \"../../../sneaky\";\n        \
                 assert!(p.contains(\"..\"));\n    \
             }\n\
         }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name(
            "qartez_security",
            json!({"category": "injection", "include_tests": true}),
        )
        .expect("qartez_security dispatch");

    assert!(
        result.contains("prod_path"),
        "production SEC005 must surface with include_tests=true, got: {result}"
    );
    // Symbol names are truncated in the table; check the snippet text
    // unique to the inline test fixture.
    assert!(
        result.contains("../../../sneaky"),
        "inline test SEC005 must surface with include_tests=true, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_security_scan_preserves_findings_outside_test_block() {
    // SEC005 violations BEFORE and AFTER an inline #[cfg(test)] mod must
    // both surface; only the inline test fixture should be filtered.
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub fn before_block() {\n    let _ = \"../before/path\";\n}\n\
         \n\
         #[cfg(test)]\n\
         mod tests {\n    \
             #[test]\n    \
             fn inside_test() {\n        \
                 let _ = \"../../inside/test\";\n    \
             }\n\
         }\n\
         \n\
         pub fn after_block() {\n    let _ = \"../after/path\";\n}\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name("qartez_security", json!({"category": "injection"}))
        .expect("qartez_security dispatch");

    assert!(
        result.contains("before_block"),
        "SEC005 before test mod must surface, got: {result}"
    );
    assert!(
        result.contains("after_block"),
        "SEC005 after test mod must surface, got: {result}"
    );
    // Snippet text is the most reliable check (symbol names can be
    // truncated in the rendered table).
    assert!(
        result.contains("../before/path"),
        "SEC005 snippet before test mod must appear, got: {result}"
    );
    assert!(
        result.contains("../after/path"),
        "SEC005 snippet after test mod must appear, got: {result}"
    );
    assert!(
        !result.contains("../../inside/test"),
        "SEC005 snippet inside test mod must be filtered, got: {result}"
    );
}

#[test]
fn test_security_scan_sec007_allowlist_end_to_end() {
    // End-to-end test for SEC007 allowlist behaviour. The scanner must:
    // - skip `xmlns="http://www.w3.org/2000/svg"` inside CSS data-URIs,
    // - skip `proxy_pass http://backend` (single-label internal host),
    // - still flag real public plaintext URLs.
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    // Public external URL — must be flagged.
    fs::write(
        src.join("real_external.rs"),
        "pub fn leak_plaintext() {\n    \
         let api = \"http://api.vendor.io/v1/metrics\";\n    \
         let _ = api;\n\
         }\n",
    )
    .unwrap();

    // xmlns SVG namespace in data-URI — must NOT be flagged.
    fs::write(
        src.join("svg_css.rs"),
        "pub fn svg_bg() -> &'static str {\n    \
         \"background: url(\\\"data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'/>\\\")\"\n\
         }\n",
    )
    .unwrap();

    // nginx-style internal upstream — must NOT be flagged.
    fs::write(
        src.join("nginx_conf.rs"),
        "pub fn nginx_snippet() -> &'static str {\n    \
         \"location / {\\n    proxy_pass http://backend;\\n}\"\n\
         }\n",
    )
    .unwrap();

    // xmlns SOAP namespace (generic xmlns context, not w3.org) — must NOT be flagged.
    fs::write(
        src.join("soap_envelope.rs"),
        "pub fn soap_envelope() -> &'static str {\n    \
         \"<env:Envelope xmlns:env=\\\"http://schemas.xmlsoap.org/soap/envelope/\\\"/>\"\n\
         }\n",
    )
    .unwrap();

    // Localhost — must NOT be flagged (pre-existing allowlist behaviour).
    fs::write(
        src.join("local_dev.rs"),
        "pub fn dev_url() -> &'static str {\n    \
         \"http://localhost:8080/health\"\n\
         }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name(
            "qartez_security",
            json!({ "category": "crypto", "min_severity": "low" }),
        )
        .expect("qartez_security dispatch");

    // Real public URL must surface.
    assert!(
        result.contains("real_external.rs"),
        "expected real external URL to be flagged, got: {result}"
    );

    // None of the four benign cases should appear as findings.
    for benign in [
        "svg_css.rs",
        "nginx_conf.rs",
        "soap_envelope.rs",
        "local_dev.rs",
    ] {
        assert!(
            !result.contains(benign),
            "SEC007 false positive in {benign}, got: {result}"
        );
    }

    // Sanity-check on the specific allowlisted URL strings.
    assert!(
        !result.contains("http://www.w3.org"),
        "w3.org namespace must not appear in findings: {result}"
    );
    assert!(
        !result.contains("http://backend"),
        "single-label host must not appear in findings: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_security_scan_noise_reduction_end_to_end() {
    // End-to-end coverage for the four noise-reduction changes shipped in
    // the noise-reduction patch: SEC003 SQL syntax tightening, SEC001
    // env-indirection skip, SEC004 static-literal Command skip, and the
    // graph/security.rs self-scan exemption. Build a fixture project with
    // ALL the historical false positives next to a curated set of true
    // positives, scan it, and assert FPs are gone while TPs survive.
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    let graph = src.join("graph");
    fs::create_dir_all(&graph).unwrap();

    // ---- TRUE POSITIVES (must surface) ----

    // Real SQL injection via format!.
    fs::write(
        src.join("real_sql.rs"),
        "pub fn user_lookup(id: i64) {\n    \
         let q = format!(\"SELECT * FROM users WHERE id = {}\", id);\n    \
         let _ = q;\n\
         }\n",
    )
    .unwrap();

    // Real hardcoded secret (literal value, not env indirection).
    fs::write(
        src.join("real_secret.rs"),
        "pub fn load() {\n    \
         let password = \"hunter2-real-prod-secret\";\n    \
         let _ = password;\n\
         }\n",
    )
    .unwrap();

    // Real dynamic Command (interpolated arg via format!).
    fs::write(
        src.join("real_cmd.rs"),
        "pub fn unsafe_exec(user: &str) {\n    \
         let _ = std::process::Command::new(\"sh\")\n        \
                 .arg(format!(\"echo {}\", user))\n        \
                 .output();\n\
         }\n",
    )
    .unwrap();

    // ---- FALSE POSITIVES (must NOT surface) ----

    // SEC003: format! with substring "updated" (matched 'UPDATE' under old regex).
    fs::write(
        src.join("fp_log_update.rs"),
        "pub fn log_settings_update(path: &str) {\n    \
         let msg = format!(\"Settings updated: {}\", path);\n    \
         let _ = msg;\n\
         }\n",
    )
    .unwrap();

    // SEC003: format! with "selector:" (matched 'SELECT' under old regex).
    fs::write(
        src.join("fp_selector_key.rs"),
        "pub fn build_selector(key: &str, val: &str) {\n    \
         let s = format!(\"selector:{}={}\", key, val);\n    \
         let _ = s;\n\
         }\n",
    )
    .unwrap();

    // SEC001: env-variable indirections (no hardcoded value).
    fs::write(
        src.join("fp_env_token.rs"),
        "pub fn env_indirection() {\n    \
         let token = \"${env.GITHUB_TOKEN}\";\n    \
         let api_key = \"process.env.OPENAI_KEY\";\n    \
         let _ = (token, api_key);\n\
         }\n",
    )
    .unwrap();

    // SEC004: static Command::new with all-literal args.
    fs::write(
        src.join("fp_static_git.rs"),
        "pub fn git_sha() {\n    \
         let _ = std::process::Command::new(\"git\")\n        \
                 .args([\"rev-parse\", \"--short\", \"HEAD\"])\n        \
                 .output();\n\
         }\n",
    )
    .unwrap();

    // graph/security.rs exemption: drop the same regex literal here that
    // would self-match every body-regex rule. The scanner must skip this
    // file entirely.
    fs::write(
        graph.join("security.rs"),
        "pub fn fake_rule_table() {\n    \
         let _sec001 = r#\"(?i)(password|token|secret|api_key)\\s*=\\s*\"[^\"]{4,}\"\"#;\n    \
         let _sec003 = r#\"(?i)(format!\\(|\\.format\\(|f\")[^\"\\n]*?\\b(?:SELECT|INSERT|UPDATE|DELETE|DROP)\"#;\n    \
         let _sec004 = r#\"(?i)(Command::new|subprocess|os\\.system|exec\\()\"#;\n    \
         let _ = (_sec001, _sec003, _sec004);\n\
         }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name(
            "qartez_security",
            json!({ "min_severity": "low", "limit": 200 }),
        )
        .expect("qartez_security dispatch");

    // ---- TRUE POSITIVES must surface ----
    assert!(
        result.contains("real_sql.rs"),
        "real SQL injection must still be flagged, got:\n{result}"
    );
    assert!(
        result.contains("real_secret.rs"),
        "real hardcoded secret must still be flagged, got:\n{result}"
    );
    assert!(
        result.contains("real_cmd.rs"),
        "dynamic Command::new with format! must still be flagged, got:\n{result}"
    );

    // ---- FALSE POSITIVES must NOT surface ----
    for fp in [
        "fp_log_update.rs",
        "fp_selector_key.rs",
        "fp_env_token.rs",
        "fp_static_git.rs",
    ] {
        assert!(
            !result.contains(fp),
            "false positive resurfaced in {fp}, got:\n{result}"
        );
    }

    // graph/security.rs exemption: even though the file contains every
    // body-regex literal, the scanner must skip it wholesale.
    assert!(
        !result.contains("graph/security.rs"),
        "graph/security.rs self-scan exemption broken, got:\n{result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_security_scan_line_points_at_match_not_symbol_header() {
    // Regression: previously `Finding.line_start` was always the enclosing
    // symbol's first line (the `fn` header), while `snippet` was the
    // match line. The table row and the snippet pointed at different
    // places, so the finding could not be navigated from the report. The
    // fix computes the real match line by counting newlines within the
    // symbol body and surfaces it as both `line_start` and `line_end`.
    //
    // Fixture: a 10-line function where the only Command::new is on line
    // 8. The enclosing fn header is on line 2, so an old-behavior report
    // would print `Line 2` with the snippet showing L8. The new report
    // must print `Line 8`.
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    // Craft the file so the fn spans lines 1-10 and the Command::new call
    // lands on line 8. Each `\n` advances one line; the first line is 1.
    let body = "// Leading comment\n\
                pub fn run_something(exec_path: &str) {\n    \
                let mut acc = Vec::new();\n    \
                acc.push(1);\n    \
                acc.push(2);\n    \
                acc.push(3);\n    \
                // next line spawns the process\n    \
                let _ = std::process::Command::new(exec_path)\n        \
                .arg(\"--help\")\n        \
                .output();\n\
                }\n";
    fs::write(src.join("run.rs"), body).unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name(
            "qartez_security",
            json!({ "min_severity": "high", "limit": 50 }),
        )
        .expect("qartez_security dispatch");

    assert!(
        result.contains("command-injection"),
        "expected SEC004 finding, got:\n{result}"
    );

    // Verify the Line column shows 8 (the Command::new match line) rather
    // than 2 (the fn header line). The concise table row ends with the
    // line number as its last column.
    let has_line_8 = result
        .lines()
        .any(|l| l.contains("command-injection") && l.trim_end().ends_with(" 8"));
    assert!(
        has_line_8,
        "Line column must point at the Command::new match line (8), got:\n{result}"
    );

    // Verify the snippet block cites `run.rs:8`, not `run.rs:2`.
    assert!(
        result.contains("run.rs:8"),
        "snippet header must cite the match line (run.rs:8), got:\n{result}"
    );
    assert!(
        !result.contains("run.rs:2"),
        "snippet header must NOT cite the fn header line (run.rs:2), got:\n{result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_security_scan_line_points_at_real_match_when_allowlisted_precedes() {
    // Edge case for the line-at-match fix: a function body contains TWO
    // `Command::new` occurrences. The earlier one is `Command::new("git")`
    // - a static literal on a non-shell binary, which the SEC004
    // allowlist correctly suppresses. The later one is
    // `Command::new(dyn_path)` - a dynamic, flagged finding.
    //
    // The finding-existence check iterates match positions and asks "is
    // there any non-static match?". The per-line snippet/line extractor
    // must also skip allowlisted matches; otherwise the report surfaces
    // the `git` literal's line while claiming there's an injection, which
    // is misdirection worse than the original fn-header bug. The
    // finding's `line_start` and snippet must both point at the dynamic
    // call, not the earlier allowlisted one.
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    // Line layout:
    //   1: pub fn spawn_stuff(dyn_path: &str) {
    //   2:     // static literal - allowlisted
    //   3:     let _ = std::process::Command::new("git").arg("log").output();
    //   4:     let mut acc = Vec::new();
    //   5:     acc.push(1);
    //   6:     acc.push(2);
    //   7:     // dynamic - REAL finding
    //   8:     let _ = std::process::Command::new(dyn_path).arg("--help").output();
    //   9: }
    let body = "pub fn spawn_stuff(dyn_path: &str) {\n    \
                // static literal - allowlisted\n    \
                let _ = std::process::Command::new(\"git\").arg(\"log\").output();\n    \
                let mut acc = Vec::new();\n    \
                acc.push(1);\n    \
                acc.push(2);\n    \
                // dynamic - REAL finding\n    \
                let _ = std::process::Command::new(dyn_path).arg(\"--help\").output();\n\
                }\n";
    fs::write(src.join("run.rs"), body).unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name(
            "qartez_security",
            json!({ "min_severity": "high", "limit": 50 }),
        )
        .expect("qartez_security dispatch");

    assert!(
        result.contains("command-injection"),
        "expected SEC004 finding for dyn_path, got:\n{result}"
    );

    // The snippet should cite the dynamic call's line (8), NOT the
    // allowlisted static-git line (3).
    assert!(
        result.contains("run.rs:8"),
        "snippet should cite the dynamic Command::new line (8), got:\n{result}"
    );
    assert!(
        !result.contains("run.rs:3"),
        "snippet must not cite the allowlisted static-git line (3), got:\n{result}"
    );

    // Line column must be 8, not 3 or 1.
    let has_line_8 = result
        .lines()
        .any(|l| l.contains("command-injection") && l.trim_end().ends_with(" 8"));
    assert!(
        has_line_8,
        "Line column must be 8 (the real dyn_path call), got:\n{result}"
    );

    // Snippet text must contain `dyn_path`, not `\"git\"` literal.
    assert!(
        result.contains("dyn_path"),
        "snippet text should be the dyn_path line, got:\n{result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_security_scan_line_at_match_snippet_truncation_over_120_chars() {
    // The snippet extractor truncates lines longer than 120 chars to
    // `{first 117 chars}...`. Verify:
    //   * truncation happens with `...` suffix
    //   * the reported line still points at the correct match line
    //     (not the enclosing fn header)
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    // Fn header at L1, throwaway line at L2, SEC001 literal >120 chars
    // on L3, trailer on L4-L5.
    let long_secret_line = format!(
        "    let api_key = \"{}\";",
        "x".repeat(180) // ensures trimmed line exceeds 120 chars
    );
    let body = format!(
        "pub fn load_creds() {{\n    \
         let _padding = 1;\n\
         {long_secret_line}\n    \
         let _ = api_key;\n\
         }}\n"
    );
    fs::write(src.join("creds.rs"), &body).unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name(
            "qartez_security",
            json!({ "min_severity": "low", "limit": 50 }),
        )
        .expect("qartez_security dispatch");

    assert!(
        result.contains("hardcoded-secret"),
        "expected SEC001 finding, got:\n{result}"
    );
    // Snippet must be truncated with `...`.
    assert!(
        result.contains("..."),
        "snippet should be truncated with ellipsis, got:\n{result}"
    );
    // Untruncated 180-char run must not appear.
    assert!(
        !result.contains(&"x".repeat(180)),
        "untruncated 180-char snippet must not appear in output"
    );
    // Line column must be 3 (the api_key assignment), not 1 (fn header).
    assert!(
        result.contains("creds.rs:3"),
        "snippet header should cite creds.rs:3, got:\n{result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_security_scan_line_at_match_last_line_of_symbol() {
    // A match on the symbol's very last statement line must be reported
    // with the correct line number (not saturating back to the header).
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    // Fn spans L1-L5; dynamic Command::new on a real shell is on L4.
    let body = "pub fn tail_match(x: &str) {\n    \
                let a = 1;\n    \
                let b = 2;\n    \
                let _ = std::process::Command::new(\"sh\").arg(\"-c\").arg(format!(\"echo {}\", x)).output();\n\
                }\n";
    fs::write(src.join("tail.rs"), body).unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name(
            "qartez_security",
            json!({ "min_severity": "high", "limit": 50 }),
        )
        .expect("qartez_security dispatch");

    assert!(
        result.contains("command-injection"),
        "dynamic sh + format! must be flagged, got:\n{result}"
    );
    assert!(
        result.contains("tail.rs:4"),
        "snippet header should cite line 4, got:\n{result}"
    );
}

// ---------------------------------------------------------------------------
// qartez_smells: end-to-end integration tests
// ---------------------------------------------------------------------------

/// Build a test DB with symbols designed to trigger each smell category.
/// Returns (server, temp_dir) - temp_dir must live as long as server.
#[cfg(feature = "benchmark")]
fn smells_test_fixture() -> (qartez_mcp::server::QartezServer, TempDir) {
    use qartez_mcp::server::QartezServer;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    // File A: contains a god function and a normal function
    let file_a = insert_file(&conn, "src/engine.rs");
    let _syms_a = write::insert_symbols(
        &conn,
        file_a,
        &[
            // God function: CC=20, 100 lines
            SymbolInsert {
                name: "process_everything".into(),
                kind: "function".into(),
                line_start: 1,
                line_end: 100,
                signature: Some("fn process_everything(data: Vec<u8>)".into()),
                is_exported: true,
                complexity: Some(20),
                owner_type: None,
                ..Default::default()
            },
            // Normal function: CC=3, 10 lines
            SymbolInsert {
                name: "small_helper".into(),
                kind: "function".into(),
                line_start: 102,
                line_end: 112,
                signature: Some("fn small_helper(x: i32)".into()),
                is_exported: false,
                complexity: Some(3),
                owner_type: None,
                ..Default::default()
            },
            // Long param list: 7 parameters
            SymbolInsert {
                name: "build_config".into(),
                kind: "function".into(),
                line_start: 114,
                line_end: 130,
                signature: Some(
                    "fn build_config(a: i32, b: String, c: bool, d: f64, e: Vec<u8>, f: Option<String>, g: HashMap<String, i32>)"
                        .into(),
                ),
                is_exported: true,
                complexity: Some(2),
                owner_type: None,
                ..Default::default()
            },
            // Exactly 5 params (at threshold) - should appear with default min_params=5
            SymbolInsert {
                name: "at_threshold".into(),
                kind: "function".into(),
                line_start: 132,
                line_end: 140,
                signature: Some("fn at_threshold(a: i32, b: i32, c: i32, d: i32, e: i32)".into()),
                is_exported: false,
                complexity: Some(1),
                owner_type: None,
                ..Default::default()
            },
            // 4 params (below threshold)
            SymbolInsert {
                name: "below_threshold".into(),
                kind: "function".into(),
                line_start: 142,
                line_end: 150,
                signature: Some("fn below_threshold(a: i32, b: i32, c: i32, d: i32)".into()),
                is_exported: false,
                complexity: Some(1),
                owner_type: None,
                ..Default::default()
            },
            // Method with &self and 5 non-self params
            SymbolInsert {
                name: "method_long".into(),
                kind: "method".into(),
                line_start: 152,
                line_end: 160,
                signature: Some("fn method_long(&self, a: i32, b: i32, c: i32, d: i32, e: i32)".into()),
                is_exported: false,
                complexity: Some(1),
                owner_type: Some("Engine".into()),
                ..Default::default()
            },
        ],
    )
    .unwrap();

    // File B: contains methods for feature envy testing
    let file_b = insert_file(&conn, "src/adapter.rs");
    let syms_b = write::insert_symbols(
        &conn,
        file_b,
        &[
            // Method on Adapter that mostly calls Engine methods (feature envy)
            SymbolInsert {
                name: "do_adaptation".into(),
                kind: "method".into(),
                line_start: 1,
                line_end: 30,
                signature: Some("fn do_adaptation(&self)".into()),
                is_exported: true,
                complexity: Some(5),
                owner_type: Some("Adapter".into()),
                ..Default::default()
            },
            // Target of calls: Engine method
            SymbolInsert {
                name: "engine_step_one".into(),
                kind: "method".into(),
                line_start: 32,
                line_end: 40,
                signature: Some("fn engine_step_one(&self)".into()),
                is_exported: true,
                complexity: Some(2),
                owner_type: Some("Engine".into()),
                ..Default::default()
            },
            SymbolInsert {
                name: "engine_step_two".into(),
                kind: "method".into(),
                line_start: 42,
                line_end: 50,
                signature: Some("fn engine_step_two(&self)".into()),
                is_exported: true,
                complexity: Some(2),
                owner_type: Some("Engine".into()),
                ..Default::default()
            },
            SymbolInsert {
                name: "engine_step_three".into(),
                kind: "method".into(),
                line_start: 52,
                line_end: 60,
                signature: Some("fn engine_step_three(&self)".into()),
                is_exported: true,
                complexity: Some(2),
                owner_type: Some("Engine".into()),
                ..Default::default()
            },
            // Own-type call target
            SymbolInsert {
                name: "adapter_helper".into(),
                kind: "method".into(),
                line_start: 62,
                line_end: 70,
                signature: Some("fn adapter_helper(&self)".into()),
                is_exported: false,
                complexity: Some(1),
                owner_type: Some("Adapter".into()),
                ..Default::default()
            },
        ],
    )
    .unwrap();

    // Insert symbol_refs: do_adaptation calls 3 Engine methods and 1 Adapter method
    // That's 3 external to 1 own = ratio 3.0 (above default 2.0 threshold)
    let do_adaptation_id = syms_b[0];
    let engine_step_one_id = syms_b[1];
    let engine_step_two_id = syms_b[2];
    let engine_step_three_id = syms_b[3];
    let adapter_helper_id = syms_b[4];

    write::insert_symbol_refs(
        &conn,
        &[
            (do_adaptation_id, engine_step_one_id, "call"),
            (do_adaptation_id, engine_step_two_id, "call"),
            (do_adaptation_id, engine_step_three_id, "call"),
            (do_adaptation_id, adapter_helper_id, "call"),
        ],
    )
    .unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);
    (server, dir)
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_detects_god_functions() {
    use serde_json::json;
    let (server, _dir) = smells_test_fixture();

    let result = server
        .call_tool_by_name("qartez_smells", json!({"kind": "god_function"}))
        .expect("qartez_smells should succeed");

    assert!(
        result.contains("process_everything"),
        "should detect god function, got: {result}"
    );
    assert!(
        !result.contains("small_helper"),
        "should not flag low-complexity function, got: {result}"
    );
    assert!(
        result.contains("CC"),
        "should show complexity info, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_detects_long_param_lists() {
    use serde_json::json;
    let (server, _dir) = smells_test_fixture();

    let result = server
        .call_tool_by_name("qartez_smells", json!({"kind": "long_params"}))
        .expect("qartez_smells should succeed");

    assert!(
        result.contains("build_config"),
        "should detect 7-param function, got: {result}"
    );
    assert!(
        result.contains("at_threshold"),
        "should detect function at exactly 5 params, got: {result}"
    );
    assert!(
        result.contains("method_long"),
        "should detect method with 5 non-self params, got: {result}"
    );
    assert!(
        !result.contains("below_threshold"),
        "should not flag 4-param function, got: {result}"
    );
    assert!(
        !result.contains("small_helper"),
        "should not flag 1-param function, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_detects_feature_envy() {
    use serde_json::json;
    let (server, _dir) = smells_test_fixture();

    let result = server
        .call_tool_by_name("qartez_smells", json!({"kind": "feature_envy"}))
        .expect("qartez_smells should succeed");

    assert!(
        result.contains("do_adaptation"),
        "should detect feature envy (3 Engine calls vs 1 own), got: {result}"
    );
    assert!(
        result.contains("Adapter"),
        "should show own type, got: {result}"
    );
    assert!(
        result.contains("Engine"),
        "should show envied type, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_all_kinds_combined() {
    use serde_json::json;
    let (server, _dir) = smells_test_fixture();

    // No kind filter - all three categories
    let result = server
        .call_tool_by_name("qartez_smells", json!({}))
        .expect("qartez_smells should succeed");

    assert!(
        result.contains("God Functions"),
        "should have god function section, got: {result}"
    );
    assert!(
        result.contains("Long Parameter Lists"),
        "should have long params section, got: {result}"
    );
    assert!(
        result.contains("Feature Envy"),
        "should have feature envy section, got: {result}"
    );
    assert!(
        result.contains("Code Smells"),
        "should have summary header, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_custom_thresholds() {
    use serde_json::json;
    let (server, _dir) = smells_test_fixture();

    // Raise thresholds so nothing triggers
    let result = server
        .call_tool_by_name(
            "qartez_smells",
            json!({"min_complexity": 50, "min_lines": 200, "min_params": 20, "envy_ratio": 100.0}),
        )
        .expect("qartez_smells should succeed");

    assert!(
        result.contains("No code smells detected"),
        "should report no smells with very high thresholds, got: {result}"
    );

    // Lower god function thresholds to catch small_helper too
    let result2 = server
        .call_tool_by_name(
            "qartez_smells",
            json!({"kind": "god_function", "min_complexity": 1, "min_lines": 1}),
        )
        .expect("qartez_smells should succeed");

    assert!(
        result2.contains("small_helper"),
        "lowered thresholds should catch small_helper, got: {result2}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_concise_format() {
    use serde_json::json;
    let (server, _dir) = smells_test_fixture();

    let result = server
        .call_tool_by_name("qartez_smells", json!({"format": "concise"}))
        .expect("qartez_smells should succeed");

    // Concise format should not have markdown table delimiters
    assert!(
        !result.contains("|---"),
        "concise format should not have table separators, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_file_path_scoping() {
    use serde_json::json;
    let (server, _dir) = smells_test_fixture();

    // Scope to engine.rs only - should see god function but not feature envy
    let result = server
        .call_tool_by_name("qartez_smells", json!({"file_path": "src/engine.rs"}))
        .expect("qartez_smells should succeed");

    assert!(
        result.contains("process_everything"),
        "should find god function in engine.rs, got: {result}"
    );
    assert!(
        !result.contains("do_adaptation"),
        "should not find adapter.rs symbols, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_feature_envy_ignores_associated_function_calls() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    // Target type `Bar` with only an associated function `new` (no self param).
    let file_bar = insert_file(&conn, "src/bar.rs");
    let bar_syms = write::insert_symbols(
        &conn,
        file_bar,
        &[SymbolInsert {
            name: "new".into(),
            kind: "method".into(),
            line_start: 1,
            line_end: 5,
            signature: Some("fn new(name: &str) -> Bar".into()),
            is_exported: true,
            complexity: Some(1),
            owner_type: Some("Bar".into()),
            ..Default::default()
        }],
    )
    .unwrap();
    let bar_new_id = bar_syms[0];

    // Calling method on `Foo` that constructs Bars via `Bar::new` 5 times.
    let file_foo = insert_file(&conn, "src/foo.rs");
    let foo_syms = write::insert_symbols(
        &conn,
        file_foo,
        &[
            SymbolInsert {
                name: "build_many".into(),
                kind: "method".into(),
                line_start: 1,
                line_end: 20,
                signature: Some("fn build_many(&self)".into()),
                is_exported: true,
                complexity: Some(3),
                owner_type: Some("Foo".into()),
                ..Default::default()
            },
            SymbolInsert {
                name: "foo_helper".into(),
                kind: "method".into(),
                line_start: 22,
                line_end: 25,
                signature: Some("fn foo_helper(&self)".into()),
                is_exported: false,
                complexity: Some(1),
                owner_type: Some("Foo".into()),
                ..Default::default()
            },
        ],
    )
    .unwrap();
    let build_many_id = foo_syms[0];
    let foo_helper_id = foo_syms[1];

    // build_many "calls" Bar::new 5 times and Foo's own helper once. Without
    // the associated-function filter, ratio would be 5.0 and `build_many`
    // would be flagged; with the filter those calls are skipped.
    // Note: symbol_refs UNIQUE(from,to,kind) collapses duplicates, but a
    // single associated-function call is enough to prove it gets filtered.
    write::insert_symbol_refs(
        &conn,
        &[
            (build_many_id, bar_new_id, "call"),
            (build_many_id, foo_helper_id, "call"),
        ],
    )
    .unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);
    let result = server
        .call_tool_by_name("qartez_smells", json!({"kind": "feature_envy"}))
        .expect("qartez_smells should succeed");

    assert!(
        !result.contains("build_many"),
        "associated-function calls (Bar::new) should not trigger feature envy, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_feature_envy_still_flags_instance_method_calls() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    // Target type `Bar` exposes three &self methods.
    let file_bar = insert_file(&conn, "src/bar.rs");
    let bar_syms = write::insert_symbols(
        &conn,
        file_bar,
        &[
            SymbolInsert {
                name: "do_a".into(),
                kind: "method".into(),
                line_start: 1,
                line_end: 3,
                signature: Some("fn do_a(&self)".into()),
                is_exported: true,
                complexity: Some(1),
                owner_type: Some("Bar".into()),
                ..Default::default()
            },
            SymbolInsert {
                name: "do_b".into(),
                kind: "method".into(),
                line_start: 5,
                line_end: 7,
                signature: Some("fn do_b(&self)".into()),
                is_exported: true,
                complexity: Some(1),
                owner_type: Some("Bar".into()),
                ..Default::default()
            },
            SymbolInsert {
                name: "do_c".into(),
                kind: "method".into(),
                line_start: 9,
                line_end: 11,
                signature: Some("fn do_c(&self)".into()),
                is_exported: true,
                complexity: Some(1),
                owner_type: Some("Bar".into()),
                ..Default::default()
            },
        ],
    )
    .unwrap();

    // Method `Foo::envy_bar` calls all three `Bar` &self methods, no own calls.
    let file_foo = insert_file(&conn, "src/foo.rs");
    let foo_syms = write::insert_symbols(
        &conn,
        file_foo,
        &[SymbolInsert {
            name: "envy_bar".into(),
            kind: "method".into(),
            line_start: 1,
            line_end: 20,
            signature: Some("fn envy_bar(&self, b: &Bar)".into()),
            is_exported: true,
            complexity: Some(2),
            owner_type: Some("Foo".into()),
            ..Default::default()
        }],
    )
    .unwrap();
    let envy_bar_id = foo_syms[0];

    write::insert_symbol_refs(
        &conn,
        &[
            (envy_bar_id, bar_syms[0], "call"),
            (envy_bar_id, bar_syms[1], "call"),
            (envy_bar_id, bar_syms[2], "call"),
        ],
    )
    .unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);
    let result = server
        .call_tool_by_name("qartez_smells", json!({"kind": "feature_envy"}))
        .expect("qartez_smells should succeed");

    assert!(
        result.contains("envy_bar"),
        "3 &self-method calls on a foreign type should still flag envy, got: {result}"
    );
    assert!(
        result.contains("Bar"),
        "envied type should appear in output, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_invalid_file_path_returns_error() {
    use serde_json::json;
    let (server, _dir) = smells_test_fixture();

    let result =
        server.call_tool_by_name("qartez_smells", json!({"file_path": "nonexistent/file.rs"}));

    assert!(result.is_err(), "should error for missing file");
    assert!(
        result.unwrap_err().contains("not found"),
        "error should mention file not found"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_header_counts_are_consistent() {
    use serde_json::json;
    let (server, _dir) = smells_test_fixture();

    // With a very small limit, the header should still show total found
    let result = server
        .call_tool_by_name("qartez_smells", json!({"limit": 2}))
        .expect("qartez_smells should succeed");

    // Header should show total found count, not truncated count
    assert!(
        result.contains("found:"),
        "should have summary with total, got: {result}"
    );
    // With limit=2 and multiple categories, some get truncated
    if result.contains("Showing") {
        assert!(
            result.contains("of"),
            "truncation message should show X of Y, got: {result}"
        );
    }
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_feature_envy_works_when_file_scoped() {
    use serde_json::json;
    let (server, _dir) = smells_test_fixture();

    // Scope to adapter.rs - should still detect feature envy because
    // the owner_lookup resolves Engine symbols from the full DB
    let result = server
        .call_tool_by_name(
            "qartez_smells",
            json!({"kind": "feature_envy", "file_path": "src/adapter.rs"}),
        )
        .expect("qartez_smells should succeed");

    assert!(
        result.contains("do_adaptation"),
        "file-scoped envy should still detect cross-file references, got: {result}"
    );
    assert!(
        result.contains("Engine"),
        "should show envied type even when scoped, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_empty_db_returns_no_smells() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    let result = server
        .call_tool_by_name("qartez_smells", json!({}))
        .expect("should succeed on empty DB");

    assert!(
        result.contains("No code smells detected"),
        "empty DB should report no smells, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn smells_string_coercion_for_numeric_params() {
    use serde_json::json;
    let (server, _dir) = smells_test_fixture();

    // MCP clients sometimes send numbers as strings
    let result = server
        .call_tool_by_name(
            "qartez_smells",
            json!({"min_complexity": "15", "min_lines": "50", "min_params": "5", "envy_ratio": "2.0", "limit": "10"}),
        )
        .expect("string-coerced params should work");

    assert!(
        result.contains("Code Smells"),
        "should work with string params, got: {result}"
    );
}

// ---- qartez_test_gaps tests ----

#[cfg(feature = "benchmark")]
fn test_gaps_fixture() -> (qartez_mcp::server::QartezServer, TempDir) {
    use qartez_mcp::server::QartezServer;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    // Source file A: has a test file importing it
    let file_a = insert_file(&conn, "src/core.rs");
    write::insert_symbols(
        &conn,
        file_a,
        &[SymbolInsert {
            name: "process".into(),
            kind: "function".into(),
            line_start: 1,
            line_end: 20,
            signature: Some("fn process(data: Vec<u8>)".into()),
            is_exported: true,
            complexity: Some(8),
            owner_type: None,
            ..Default::default()
        }],
    )
    .unwrap();

    // Source file B: no test imports it (gap)
    let file_b = insert_file(&conn, "src/utils.rs");
    write::insert_symbols(
        &conn,
        file_b,
        &[SymbolInsert {
            name: "helper".into(),
            kind: "function".into(),
            line_start: 1,
            line_end: 10,
            signature: Some("fn helper(x: i32) -> i32".into()),
            is_exported: true,
            complexity: Some(3),
            owner_type: None,
            ..Default::default()
        }],
    )
    .unwrap();

    // Source file C: also no test (another gap, higher complexity)
    let file_c = insert_file(&conn, "src/engine.rs");
    write::insert_symbols(
        &conn,
        file_c,
        &[SymbolInsert {
            name: "run_engine".into(),
            kind: "function".into(),
            line_start: 1,
            line_end: 80,
            signature: Some("fn run_engine()".into()),
            is_exported: true,
            complexity: Some(18),
            owner_type: None,
            ..Default::default()
        }],
    )
    .unwrap();

    // Test file that imports file_a
    let test_file = insert_file(&conn, "tests/test_core.rs");
    write::insert_symbols(
        &conn,
        test_file,
        &[SymbolInsert {
            name: "test_process".into(),
            kind: "function".into(),
            line_start: 1,
            line_end: 10,
            signature: Some("fn test_process()".into()),
            is_exported: false,
            complexity: Some(1),
            owner_type: None,
            ..Default::default()
        }],
    )
    .unwrap();

    // Edge: test file imports source file A
    write::insert_edge(&conn, test_file, file_a, "import", None).unwrap();
    // Edge: engine imports utils (for blast radius)
    write::insert_edge(&conn, file_c, file_b, "import", None).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);
    (server, dir)
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_map_mode_shows_coverage() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "map"}))
        .expect("map mode should succeed");

    assert!(
        result.contains("Test-to-source mapping"),
        "should show mapping header, got: {result}"
    );
    assert!(
        result.contains("src/core.rs"),
        "should show covered source file, got: {result}"
    );
    assert!(
        result.contains("tests/test_core.rs"),
        "should show test file, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_map_mode_file_scoped() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({"mode": "map", "file_path": "src/core.rs"}),
        )
        .expect("file-scoped map should succeed");

    assert!(
        result.contains("tests/test_core.rs"),
        "should show test file for core.rs, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_map_mode_uncovered_file() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({"mode": "map", "file_path": "src/utils.rs"}),
        )
        .expect("uncovered file map should succeed");

    assert!(
        result.contains("no test files importing it"),
        "should indicate no coverage, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_gaps_mode_finds_untested() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("gaps mode should succeed");

    assert!(
        result.contains("Test coverage gaps"),
        "should show gaps header, got: {result}"
    );
    assert!(
        result.contains("src/utils.rs"),
        "should flag utils.rs as untested, got: {result}"
    );
    assert!(
        result.contains("src/engine.rs"),
        "should flag engine.rs as untested, got: {result}"
    );
    assert!(
        !result.contains("src/core.rs"),
        "core.rs has test coverage, should not appear in gaps, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_gaps_mode_default_is_gaps() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({}))
        .expect("default mode should succeed");

    assert!(
        result.contains("Test coverage gaps"),
        "default mode should be gaps, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_gaps_mode_concise() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({"mode": "gaps", "format": "concise"}),
        )
        .expect("concise gaps should succeed");

    assert!(
        result.contains("PR="),
        "concise format should show PR= notation, got: {result}"
    );
    assert!(
        result.contains("score="),
        "concise format should show score=, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_gaps_mode_pagerank_filter() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    // Set a very high min_pagerank that filters out everything
    let result = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({"mode": "gaps", "min_pagerank": 100.0}),
        )
        .expect("pagerank filter should succeed");

    assert!(
        result.contains("No untested source files found"),
        "high pagerank filter should exclude all, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_invalid_mode() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server.call_tool_by_name("qartez_test_gaps", json!({"mode": "invalid"}));

    assert!(result.is_err(), "invalid mode should return error");
    let err = result.unwrap_err();
    assert!(
        err.contains("Unknown mode"),
        "should mention unknown mode, got: {err}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_suggest_mode_requires_base() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server.call_tool_by_name("qartez_test_gaps", json!({"mode": "suggest"}));

    assert!(result.is_err(), "suggest without base should return error");
    let err = result.unwrap_err();
    assert!(
        err.contains("base"),
        "error should mention base param, got: {err}"
    );
}

// ---- Edge-case tests ----

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_empty_db() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    // Map mode on empty DB
    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "map"}))
        .expect("map on empty DB should succeed");
    assert!(
        result.contains("0/0 source files"),
        "empty DB map should show 0/0, got: {result}"
    );

    // Gaps mode on empty DB
    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("gaps on empty DB should succeed");
    assert!(
        result.contains("No untested source files"),
        "empty DB gaps should show no gaps, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_all_files_covered() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    let src = insert_file(&conn, "src/lib.rs");
    let test = insert_file(&conn, "tests/test_lib.rs");
    write::insert_edge(&conn, test, src, "import", None).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("should succeed");
    assert!(
        result.contains("No untested source files"),
        "all covered - should show no gaps, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_inline_cfg_test_not_flagged_as_gap() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    // Write a real Rust file with an inline `#[cfg(test)]` module to the temp
    // project root. No external test file imports it, but its inline tests
    // should still mark it as covered.
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/inlined.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }\n\
         #[cfg(test)]\n\
         mod tests {\n\
             use super::*;\n\
             #[test]\n\
             fn it_works() { assert_eq!(add(2, 2), 4); }\n\
         }\n",
    )
    .unwrap();

    insert_file(&conn, "src/inlined.rs");

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("should succeed");
    assert!(
        result.contains("No untested source files"),
        "file with inline `#[cfg(test)]` mod must not be flagged as gap, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_inline_test_attr_only_not_flagged() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    // File uses bare `#[test]` without a `#[cfg(test)]` wrapper (legal but rare).
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/bare.rs"),
        "pub fn id(x: i32) -> i32 { x }\n\
         #[test]\n\
         fn check() { assert_eq!(id(1), 1); }\n",
    )
    .unwrap();

    insert_file(&conn, "src/bare.rs");
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("should succeed");
    assert!(
        result.contains("No untested source files"),
        "bare `#[test]` must mark file as covered, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_tokio_test_attr_not_flagged() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/async_file.rs"),
        "pub async fn work() -> u32 { 42 }\n\
         #[tokio::test]\n\
         async fn check() { assert_eq!(work().await, 42); }\n",
    )
    .unwrap();

    insert_file(&conn, "src/async_file.rs");
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("should succeed");
    assert!(
        result.contains("No untested source files"),
        "`#[tokio::test]` must mark file as covered, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_missing_source_file_still_flagged() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    // File row is indexed but no physical file on disk - read_to_string fails,
    // helper returns false, file should remain flagged (no silent masking).
    insert_file(&conn, "src/phantom.rs");
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("should succeed");
    assert!(
        result.contains("src/phantom.rs"),
        "missing file on disk must still be flagged, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_non_rust_file_with_test_in_name_still_flagged() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    // A Python file (not in a `tests/` directory) - helper must short-circuit
    // on extension, so this gets flagged regardless of contents.
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/script.py"),
        "# [test] not a rust attr\n#[cfg(test)] also ignored\n",
    )
    .unwrap();

    insert_file(&conn, "src/script.py");
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("should succeed");
    assert!(
        result.contains("src/script.py"),
        "non-Rust file with test-looking text must still be flagged, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_file_without_inline_tests_still_flagged() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/plain.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
    )
    .unwrap();

    insert_file(&conn, "src/plain.rs");

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("should succeed");
    assert!(
        result.contains("src/plain.rs"),
        "file without any tests must still be flagged as gap, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_test_file_not_counted_as_gap() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    // Only a test file, no source files
    let _test = insert_file(&conn, "tests/test_only.rs");

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("should succeed");
    assert!(
        result.contains("No untested source files"),
        "test-only DB should show no gaps, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_self_referencing_edge_ignored() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    let src = insert_file(&conn, "src/self_ref.rs");
    // Self-referencing edge should not count as test coverage
    write::insert_edge(&conn, src, src, "import", None).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("should succeed");
    assert!(
        result.contains("src/self_ref.rs"),
        "self-ref file should appear as gap, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_map_mode_test_file_with_no_source_imports() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    // Test file that imports nothing
    let _test = insert_file(&conn, "tests/orphan_test.rs");
    let _src = insert_file(&conn, "src/lonely.rs");

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    // Map for the test file
    let result = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({"mode": "map", "file_path": "tests/orphan_test.rs"}),
        )
        .expect("should succeed");
    assert!(
        result.contains("no indexed source imports"),
        "orphan test should have no source imports, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_map_mode_include_symbols() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({"mode": "map", "file_path": "src/core.rs", "include_symbols": true}),
        )
        .expect("include_symbols should succeed");

    assert!(
        result.contains("process"),
        "should list exported symbols, got: {result}"
    );
    assert!(
        result.contains("exported symbols"),
        "should have exported symbols section, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_limit_parameter() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps", "limit": 1}))
        .expect("limit=1 should succeed");

    // Should have exactly 1 row in the table (plus header)
    let data_rows = result.lines().filter(|l| l.starts_with("| src/")).count();
    assert_eq!(
        data_rows, 1,
        "limit=1 should show exactly 1 gap, got {data_rows} in: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_gaps_ranking_order() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("gaps should succeed");

    // engine.rs (CC=18) should rank higher than utils.rs (CC=3)
    let engine_pos = result.find("engine.rs").expect("engine should appear");
    let utils_pos = result.find("utils.rs").expect("utils should appear");
    assert!(
        engine_pos < utils_pos,
        "engine.rs (higher complexity) should rank before utils.rs, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_string_coercion_for_numeric_params() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    // MCP clients sometimes send numbers as strings
    let result = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({"mode": "gaps", "limit": "5", "min_pagerank": "0.0"}),
        )
        .expect("string-coerced params should work");

    assert!(
        result.contains("Test coverage gaps"),
        "should work with string params, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_multiple_tests_for_one_source() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();

    let src = insert_file(&conn, "src/core.rs");
    let test_a = insert_file(&conn, "tests/test_core_a.rs");
    let test_b = insert_file(&conn, "tests/test_core_b.rs");
    write::insert_edge(&conn, test_a, src, "import", None).unwrap();
    write::insert_edge(&conn, test_b, src, "import", None).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);

    // Map for the source file should list both tests
    let result = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({"mode": "map", "file_path": "src/core.rs"}),
        )
        .expect("should succeed");

    assert!(
        result.contains("2 test file(s)"),
        "should show 2 test files, got: {result}"
    );
    assert!(
        result.contains("test_core_a.rs"),
        "should list test A, got: {result}"
    );
    assert!(
        result.contains("test_core_b.rs"),
        "should list test B, got: {result}"
    );

    // Gaps should be empty since src/core.rs is covered
    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("should succeed");
    assert!(
        result.contains("No untested source files"),
        "covered file should not appear in gaps, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_map_full_shows_correct_counts() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "map"}))
        .expect("map should succeed");

    // Fixture: 3 source files, 1 test file, 1 source covered
    assert!(
        result.contains("1/3 source files covered by 1 test files"),
        "should show correct coverage fraction, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_map_concise_format() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({"mode": "map", "format": "concise"}),
        )
        .expect("concise map should succeed");

    // Concise format should NOT have the "- file (N tests)" with sub-items
    assert!(
        !result.contains("    -"),
        "concise format should not have indented test list, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_test_files_excluded_from_gaps() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    let result = server
        .call_tool_by_name("qartez_test_gaps", json!({"mode": "gaps"}))
        .expect("should succeed");

    assert!(
        !result.contains("tests/test_core.rs"),
        "test files should never appear in gaps, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_gaps_params_null_and_extra_fields() {
    use serde_json::json;
    let (server, _dir) = test_gaps_fixture();

    // All-null params (should use defaults)
    let result = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({"mode": null, "limit": null, "format": null, "min_pagerank": null}),
        )
        .expect("null params should use defaults");
    assert!(
        result.contains("Test coverage gaps"),
        "null mode should default to gaps, got: {result}"
    );

    // Extra unknown fields should be silently ignored (serde default behavior)
    let result = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({"mode": "gaps", "unknown_field": "ignored", "another": 42}),
        )
        .expect("unknown fields should be ignored");
    assert!(
        result.contains("Test coverage gaps"),
        "should work with extra fields, got: {result}"
    );

    // include_symbols as string "true" should work via flexible::bool_opt
    let result = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({"mode": "map", "file_path": "src/core.rs", "include_symbols": "true"}),
        )
        .expect("string bool should work");
    assert!(
        result.contains("exported symbols"),
        "string 'true' should enable include_symbols, got: {result}"
    );
}

// ---- qartez_knowledge integration tests ----

#[cfg(feature = "benchmark")]
#[test]
fn knowledge_no_git_depth_returns_error() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let conn = setup();
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 0);
    let result = server.call_tool_by_name("qartez_knowledge", json!({}));
    assert!(result.is_err(), "should error when git_depth is 0");
    assert!(
        result.unwrap_err().contains("git history"),
        "error should mention git history"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn knowledge_file_level_via_call_tool_by_name() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    // Set up a git repo with multiple authors
    let repo = git2::Repository::init(dir.path()).unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();
    fs::write(src.join("lib.rs"), "pub fn foo() {}\npub fn bar() {}\n").unwrap();

    let mut index = repo.index().unwrap();
    index.add_path(Path::new("src/main.rs")).unwrap();
    index.add_path(Path::new("src/lib.rs")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("Alice", "alice@test.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    // Index files in DB
    let conn = setup();
    write::upsert_file(&conn, "src/main.rs", 0, 50, "rust", 3).unwrap();
    write::upsert_file(&conn, "src/lib.rs", 0, 40, "rust", 2).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 100);

    // Default params (file level, detailed)
    let result = server
        .call_tool_by_name("qartez_knowledge", json!({}))
        .expect("should succeed");
    assert!(
        result.contains("Bus Factor"),
        "should have header, got: {result}"
    );
    assert!(
        result.contains("src/main.rs"),
        "should list files, got: {result}"
    );
    assert!(
        result.contains("Alice"),
        "should show author name, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn knowledge_module_level_via_call_tool_by_name() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.rs"), "fn a() {}\n").unwrap();

    let mut index = repo.index().unwrap();
    index.add_path(Path::new("src/a.rs")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("Bob", "bob@test.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    let conn = setup();
    write::upsert_file(&conn, "src/a.rs", 0, 10, "rust", 1).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 100);

    let result = server
        .call_tool_by_name("qartez_knowledge", json!({"level": "module"}))
        .expect("should succeed");
    assert!(
        result.contains("module level"),
        "should have module header, got: {result}"
    );
    assert!(result.contains("src"), "should list module, got: {result}");
}

#[cfg(feature = "benchmark")]
#[test]
fn knowledge_concise_format_via_call_tool_by_name() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    fs::write(dir.path().join("f.rs"), "fn f() {}\n").unwrap();

    let mut index = repo.index().unwrap();
    index.add_path(Path::new("f.rs")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("Carol", "carol@test.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    let conn = setup();
    write::upsert_file(&conn, "f.rs", 0, 10, "rust", 1).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 100);

    let result = server
        .call_tool_by_name("qartez_knowledge", json!({"format": "concise"}))
        .expect("should succeed");
    assert!(
        !result.contains("+----"),
        "concise should not contain table borders, got: {result}"
    );
    assert!(result.contains("f.rs"), "should list file, got: {result}");
}

#[cfg(feature = "benchmark")]
#[test]
fn knowledge_file_path_filter() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let src = dir.path().join("src");
    let tests_dir = dir.path().join("tests");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&tests_dir).unwrap();
    fs::write(src.join("lib.rs"), "pub fn f() {}\n").unwrap();
    fs::write(tests_dir.join("t.rs"), "fn test() {}\n").unwrap();

    let mut index = repo.index().unwrap();
    index.add_path(Path::new("src/lib.rs")).unwrap();
    index.add_path(Path::new("tests/t.rs")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("Dev", "dev@test.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    let conn = setup();
    write::upsert_file(&conn, "src/lib.rs", 0, 14, "rust", 1).unwrap();
    write::upsert_file(&conn, "tests/t.rs", 0, 14, "rust", 1).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 100);

    // Filter to src/ only
    let result = server
        .call_tool_by_name("qartez_knowledge", json!({"file_path": "src/"}))
        .expect("should succeed");
    assert!(
        result.contains("src/lib.rs"),
        "should include src file, got: {result}"
    );
    assert!(
        !result.contains("tests/t.rs"),
        "should exclude test file, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn knowledge_author_filter_via_call_tool_by_name() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();

    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.rs")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("SpecificPerson", "sp@test.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    let conn = setup();
    write::upsert_file(&conn, "a.rs", 0, 10, "rust", 1).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 100);

    // Filter to existing author
    let result = server
        .call_tool_by_name("qartez_knowledge", json!({"author": "specific"}))
        .expect("should succeed with matching author");
    assert!(
        result.contains("a.rs"),
        "should find file by author, got: {result}"
    );

    // Filter to non-existent author
    let result = server
        .call_tool_by_name("qartez_knowledge", json!({"author": "NonExistentPerson"}))
        .expect("should succeed even with no matches");
    assert!(
        result.contains("No blame data"),
        "should report no data for unknown author, got: {result}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn knowledge_string_coercion_for_limit() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();

    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.rs")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("Dev", "dev@test.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    let conn = setup();
    write::upsert_file(&conn, "a.rs", 0, 10, "rust", 1).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 100);

    // MCP clients sometimes send numbers as strings
    let result = server
        .call_tool_by_name("qartez_knowledge", json!({"limit": "5"}))
        .expect("string-coerced limit should work");
    assert!(
        result.contains("Bus Factor"),
        "should work with string limit, got: {result}"
    );
}

#[test]
fn test_call_tool_by_name_deps_mermaid() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("main.rs"),
        "mod lib;\nfn main() { lib::hello(); }\n",
    )
    .unwrap();
    fs::write(src.join("lib.rs"), "pub fn hello() { println!(\"hi\"); }\n").unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name(
            "qartez_deps",
            json!({"file_path": "src/main.rs", "format": "mermaid"}),
        )
        .expect("qartez_deps mermaid via dispatcher");
    assert!(
        result.starts_with("graph LR\n"),
        "should start with graph direction, got: {result}"
    );
    assert!(
        !result.contains("```"),
        "raw mermaid output, no markdown fences, got: {result}"
    );
}

#[test]
fn test_mermaid_format_fallback_on_unsupported_tool() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.rs"), "fn main() { }\n").unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);

    // qartez_outline rejects format=mermaid with a clear validation error
    let err = server
        .call_tool_by_name(
            "qartez_outline",
            json!({"file_path": "src/main.rs", "format": "mermaid"}),
        )
        .expect_err("mermaid on outline must return an error");
    assert!(
        err.contains("format=mermaid is not supported for qartez_outline"),
        "unexpected error message: {err}"
    );

    // qartez_grep rejects format=mermaid with a clear validation error
    let err = server
        .call_tool_by_name("qartez_grep", json!({"query": "main", "format": "mermaid"}))
        .expect_err("mermaid on grep must return an error");
    assert!(
        err.contains("format=mermaid is not supported for qartez_grep"),
        "unexpected error message: {err}"
    );
}

#[test]
fn test_call_tool_by_name_calls_mermaid() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("main.rs"),
        "fn helper() { }\nfn main() { helper(); }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);
    let result = server
        .call_tool_by_name("qartez_calls", json!({"name": "main", "format": "mermaid"}))
        .expect("qartez_calls mermaid via dispatcher");
    assert!(
        result.starts_with("graph TD\n"),
        "should start with graph direction, got: {result}"
    );
    assert!(
        !result.contains("```"),
        "raw mermaid output, no markdown fences, got: {result}"
    );
    assert!(
        result.contains("main"),
        "should contain target symbol, got: {result}"
    );
}

// ---------------------------------------------------------------------------
// Audit follow-up: incremental index must preserve cross-file symbol_refs
// ---------------------------------------------------------------------------
//
// Reproduces the regression fixed by `snapshot_cross_file_refs` /
// `restore_cross_file_refs` in `index::mod.rs`. Before the fix, when file
// B was re-ingested via the watcher path, SQLite CASCADEd every
// symbol_refs row pointing at B's old symbols (including those whose
// `from_symbol_id` lived in unchanged file A). Because the subsequent
// `resolve_symbol_references` pass only iterated re-parsed files, A's
// call-graph edges to B were silently dropped.
#[test]
fn test_incremental_preserves_cross_file_symbol_refs() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    // File B exports `helper`.
    fs::write(root.join("helper.rs"), "pub fn helper() { let _ = 1; }\n").unwrap();

    // File A imports and calls `helper`.
    fs::write(
        root.join("caller.rs"),
        "use crate::helper::helper;\n\npub fn caller() {\n    helper();\n}\n",
    )
    .unwrap();

    // Minimal Cargo-like skeleton so the Rust import resolver finds both.
    fs::write(root.join("lib.rs"), "pub mod helper;\npub mod caller;\n").unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();

    let refs_before = read::get_all_symbol_refs(&conn).unwrap();
    let ref_count_before = refs_before.len();
    assert!(
        ref_count_before > 0,
        "full index must have resolved at least one cross-file ref, got 0"
    );

    // Trigger the watcher path on `helper.rs` - this is the scenario the
    // audit flagged. The body changes but the `helper` symbol name/kind
    // stays the same, so restoration should succeed.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(root.join("helper.rs"), "pub fn helper() { let _ = 2; }\n").unwrap();

    index::incremental_index(&conn, root, &[root.join("helper.rs")], &[]).unwrap();

    let refs_after = read::get_all_symbol_refs(&conn).unwrap();
    assert!(
        refs_after.len() >= ref_count_before,
        "incremental re-index must not lose cross-file refs: before={} after={}",
        ref_count_before,
        refs_after.len()
    );
}

// ---------------------------------------------------------------------------
// Audit follow-up: a rename between re-ingests drops the (now stale) ref
// ---------------------------------------------------------------------------
//
// Guards the conservative half of the snapshot/restore logic: when the
// target symbol genuinely disappears (renamed / removed), the preserved
// ref must NOT be relinked to a randomly-chosen neighbor. Ambiguous
// matches are dropped, not guessed.
#[test]
fn test_incremental_drops_ref_when_target_symbol_renamed() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    fs::write(root.join("helper.rs"), "pub fn helper() { let _ = 1; }\n").unwrap();
    fs::write(
        root.join("caller.rs"),
        "use crate::helper::helper;\npub fn caller() { helper(); }\n",
    )
    .unwrap();
    fs::write(root.join("lib.rs"), "pub mod helper;\npub mod caller;\n").unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let before = read::get_all_symbol_refs(&conn).unwrap().len();
    assert!(before > 0);

    std::thread::sleep(std::time::Duration::from_millis(1100));
    // Rename `helper` → `renamed_helper` so the preserved ref has no
    // candidate with the same (name, kind) after re-ingest.
    fs::write(
        root.join("helper.rs"),
        "pub fn renamed_helper() { let _ = 2; }\n",
    )
    .unwrap();

    index::incremental_index(&conn, root, &[root.join("helper.rs")], &[]).unwrap();

    // Ref to `helper` from `caller` is gone. We do not assert strict
    // equality with `before - 1` because the resolver may also emit
    // brand-new refs (e.g. the `caller.rs` import edge resolution re-runs
    // on the unchanged caller when its import target gets remapped). The
    // invariant is: no ref targets the old `helper` symbol id that has
    // just been wiped.
    let orphan_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbol_refs sr
             LEFT JOIN symbols s ON sr.to_symbol_id = s.id
             WHERE s.id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        orphan_count, 0,
        "no symbol_refs may dangle after incremental re-index"
    );
}

// ---------------------------------------------------------------------------
// Audit follow-up: qartez_move preserves source file trailing newline (#6)
// ---------------------------------------------------------------------------
#[test]
fn test_qartez_move_preserves_trailing_newline_in_source() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    // Two functions; we move `b` out. The source file ends with `\n`
    // - the POSIX convention that git-diff depends on.
    let original = "pub fn a() { let _ = 1; }\n\npub fn b() { let _ = 2; }\n";
    fs::write(src.join("lib.rs"), original).unwrap();
    fs::write(src.join("other.rs"), "pub fn x() {}\n").unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let _ = server
        .call_tool_by_name(
            "qartez_move",
            json!({
                "symbol": "b",
                "to_file": "src/other.rs",
                "apply": true,
            }),
        )
        .expect("qartez_move must succeed");

    let new_source = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert!(
        new_source.ends_with('\n'),
        "source file must keep POSIX trailing newline after move, got: {new_source:?}"
    );
    assert!(
        !new_source.contains("pub fn b"),
        "`b` must have been removed, got: {new_source:?}"
    );
    assert!(
        new_source.contains("pub fn a"),
        "`a` must still be present, got: {new_source:?}"
    );
}

// ---------------------------------------------------------------------------
// Audit follow-up: qartez_move includes glob importers (#7)
// ---------------------------------------------------------------------------
#[test]
fn test_qartez_move_rewrites_glob_importer() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(src.join("helper.rs"), "pub fn helper() {}\n").unwrap();
    // Glob import - the old `spec.contains(symbol)` filter dropped this
    // importer because the specifier "helper::*" does not contain the
    // symbol name `helper` as a word boundary match in the intended way.
    fs::write(
        src.join("caller.rs"),
        "use crate::helper::*;\npub fn caller() {}\n",
    )
    .unwrap();
    fs::write(src.join("lib.rs"), "pub mod helper;\npub mod caller;\n").unwrap();
    // Pre-create the target so the move writes a valid destination.
    fs::write(src.join("moved.rs"), "pub fn anchor() {}\n").unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    // Preview path: the glob importer must be listed.
    let preview = server
        .call_tool_by_name(
            "qartez_move",
            json!({
                "symbol": "helper",
                "to_file": "src/moved.rs",
            }),
        )
        .expect("preview must succeed");
    assert!(
        preview.contains("caller.rs"),
        "glob importer caller.rs must appear in preview, got: {preview}"
    );
}

// ---------------------------------------------------------------------------
// Regression: qartez_move preserves intentional blank-line separators
// ---------------------------------------------------------------------------
#[test]
fn test_qartez_move_preserves_blank_line_separators() {
    // Before the fix, qartez_move globally collapsed every run of three
    // or more consecutive newlines down to two, silently destroying
    // intentional multi-line separators between symbol groups - far
    // beyond the extraction site. Verify that a two-blank separator
    // between unrelated items survives a move operation.
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    // `a` and `X` are separated by a two-blank (3-newline) separator;
    // `X` and `b` by another. Only `b` is being moved, so the first
    // separator must be untouched by the operation.
    let original = "pub fn a() {}\n\n\npub const X: i32 = 1;\n\n\npub fn b() {}\n";
    fs::write(src.join("lib.rs"), original).unwrap();
    fs::write(src.join("other.rs"), "pub fn anchor() {}\n").unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let _ = server
        .call_tool_by_name(
            "qartez_move",
            json!({
                "symbol": "b",
                "to_file": "src/other.rs",
                "apply": true,
            }),
        )
        .expect("qartez_move must succeed");

    let new_source = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert!(
        new_source.contains("pub fn a() {}\n\n\npub const X: i32 = 1;"),
        "two-blank separator between `a` and `X` must survive the move, got: {new_source:?}"
    );
    assert!(
        !new_source.contains("pub fn b"),
        "symbol `b` must be extracted, got: {new_source:?}"
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn test_qartez_move_split_preserves_all_modes() {
    // Behavioral parity test for the qartez_move refactor: every code
    // path the monolithic version supported must still produce the same
    // result through the new validate_source + extract_lines +
    // gather_importers + format_move_preview + write_atomic +
    // rewrite_importers helpers.
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    // 1. Multi-match disambiguation: two `dup` symbols in different files.
    fs::write(src.join("a.rs"), "pub fn dup() -> i32 { 1 }\n").unwrap();
    fs::write(src.join("b.rs"), "pub fn dup() -> i32 { 2 }\n").unwrap();
    fs::write(src.join("target.rs"), "pub fn anchor() {}\n").unwrap();
    // 2. Kind disambiguation: same name as fn AND const.
    fs::write(
        src.join("kinds.rs"),
        "pub fn shared() {}\npub const shared: i32 = 0;\n",
    )
    .unwrap();
    // 3. Conflict: target file already has same-name same-kind symbol.
    fs::write(src.join("conflict.rs"), "pub fn already_here() {}\n").unwrap();
    fs::write(
        src.join("conflict_target.rs"),
        "pub fn already_here() {}\n", // would collide
    )
    .unwrap();
    // 4. Apply path with importer rewrite.
    fs::write(src.join("util.rs"), "pub fn helper() -> i32 { 42 }\n").unwrap();
    fs::write(
        src.join("consumer.rs"),
        "use crate::util::helper;\npub fn use_it() -> i32 { helper() }\n",
    )
    .unwrap();
    fs::write(src.join("util_dest.rs"), "pub fn anchor2() {}\n").unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub mod a;\npub mod b;\npub mod target;\npub mod kinds;\n\
         pub mod conflict;\npub mod conflict_target;\npub mod util;\n\
         pub mod consumer;\npub mod util_dest;\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    // 1. Multi-match must error with disambiguation hint.
    let err = server
        .call_tool_by_name(
            "qartez_move",
            json!({ "symbol": "dup", "to_file": "src/target.rs" }),
        )
        .unwrap_err();
    assert!(
        err.contains("Multiple definitions") && err.contains("a.rs") && err.contains("b.rs"),
        "multi-match must list both locations, got: {err}"
    );

    // 2. Kind disambiguation: pass kind=function to pick exactly one.
    let preview_fn = server
        .call_tool_by_name(
            "qartez_move",
            json!({
                "symbol": "shared",
                "kind": "function",
                "to_file": "src/target.rs",
            }),
        )
        .expect("kind-narrowed preview must succeed");
    assert!(
        preview_fn.contains("Preview") && preview_fn.contains("kinds.rs"),
        "kind=function must produce a preview from kinds.rs, got: {preview_fn}"
    );

    // 2b. Bad kind = clear error listing available kinds.
    let err = server
        .call_tool_by_name(
            "qartez_move",
            json!({
                "symbol": "shared",
                "kind": "trait",
                "to_file": "src/target.rs",
            }),
        )
        .unwrap_err();
    assert!(
        err.contains("Available kinds"),
        "bad kind must report available kinds, got: {err}"
    );

    // 3. Same-name in source AND target is also caught - the multi-match
    //    error (which fires from find_symbol_by_name returning >1 row)
    //    is the primary protection. The conflict check inside
    //    validate_source is a defensive second line for cases where the
    //    target's symbols are present but the multi-match path is bypassed
    //    (e.g., kind filter narrows source to 1 while target retains its
    //    own match in get_symbols_for_file).
    let err = server
        .call_tool_by_name(
            "qartez_move",
            json!({
                "symbol": "already_here",
                "to_file": "src/conflict_target.rs",
            }),
        )
        .unwrap_err();
    assert!(
        err.contains("Multiple definitions")
            || err.contains("already defines")
            || err.contains("Refusing to overwrite"),
        "name collision must be caught (multi-match OR conflict), got: {err}"
    );

    // 4. Apply path rewrites importer + creates target body + truncates source.
    let result = server
        .call_tool_by_name(
            "qartez_move",
            json!({
                "symbol": "helper",
                "to_file": "src/util_dest.rs",
                "apply": true,
            }),
        )
        .expect("apply must succeed");
    assert!(
        result.contains("Moved") && result.contains("importer(s) rewritten"),
        "apply must report move + importer count, got: {result}"
    );

    // Source file must no longer contain `helper`.
    let src_content = fs::read_to_string(src.join("util.rs")).unwrap();
    assert!(
        !src_content.contains("pub fn helper"),
        "source must be drained, got: {src_content:?}"
    );
    // Target file must contain `helper` body.
    let tgt_content = fs::read_to_string(src.join("util_dest.rs")).unwrap();
    assert!(
        tgt_content.contains("pub fn helper"),
        "target must include the helper body, got: {tgt_content:?}"
    );
    // Importer must be rewritten from `crate::util::helper` to
    // `crate::util_dest::helper`.
    let consumer = fs::read_to_string(src.join("consumer.rs")).unwrap();
    assert!(
        consumer.contains("util_dest::helper"),
        "consumer import must be rewritten, got: {consumer:?}"
    );
    assert!(
        !consumer.contains("util::helper"),
        "old import path must be gone, got: {consumer:?}"
    );

    // 5. Symbol that does not exist returns a clean error.
    let err = server
        .call_tool_by_name(
            "qartez_move",
            json!({ "symbol": "no_such_symbol_anywhere", "to_file": "src/target.rs" }),
        )
        .unwrap_err();
    assert!(
        err.contains("No symbol found"),
        "missing symbol must error, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Regression: qartez_rename_file rewrites `use crate::<old>` importers
// ---------------------------------------------------------------------------
#[test]
fn test_qartez_rename_file_updates_crate_imports() {
    // Renaming a root-level Rust file must rewrite every
    // `use crate::<old>::...;` importer to the new module path. Before
    // the fix, the rename-pair generator only emitted the internal
    // `src::<old>` stem - a form that never appears in real Rust code -
    // so every `crate::`-relative importer was silently left dangling.
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(src.join("lib.rs"), "pub mod foo;\npub mod caller;\n").unwrap();
    fs::write(src.join("foo.rs"), "pub fn do_work() {}\n").unwrap();
    fs::write(
        src.join("caller.rs"),
        "use crate::foo::do_work;\n\npub fn entry() {\n    do_work();\n}\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let result = server
        .call_tool_by_name(
            "qartez_rename_file",
            json!({
                "from": "src/foo.rs",
                "to": "src/baz.rs",
                "apply": true,
            }),
        )
        .expect("qartez_rename_file must succeed");
    assert!(
        result.contains("renamed"),
        "expected rename confirmation: {result}"
    );

    let caller = fs::read_to_string(src.join("caller.rs")).unwrap();
    assert!(
        caller.contains("use crate::baz::do_work;"),
        "`use crate::<old>` must be rewritten to `crate::<new>`, got: {caller:?}"
    );
    assert!(
        !caller.contains("use crate::foo"),
        "old crate-relative import must be gone, got: {caller:?}"
    );

    let lib = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert!(
        lib.contains("pub mod baz;"),
        "parent `mod foo;` declaration must be rewritten, got: {lib:?}"
    );
}

// ---------------------------------------------------------------------------
// qartez_replace_symbol / qartez_insert_before_symbol / qartez_insert_after_symbol / qartez_safe_delete
// ---------------------------------------------------------------------------

#[test]
fn test_qartez_replace_symbol_rewrites_definition() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    let original = "pub fn a() {\n    let _ = 1;\n}\n\npub fn b() {\n    let _ = 2;\n}\n";
    fs::write(src.join("lib.rs"), original).unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let new_body = "pub fn b() {\n    let _ = 42;\n    let _ = 43;\n}";
    let result = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "b",
                "new_code": new_body,
                "apply": true,
            }),
        )
        .expect("qartez_replace_symbol must succeed");
    assert!(
        result.starts_with("Replaced 'b'"),
        "expected replace confirmation, got: {result}"
    );

    let updated = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert!(
        updated.contains("let _ = 42;"),
        "new body must be present: {updated}"
    );
    assert!(
        updated.contains("let _ = 43;"),
        "new body second line must be present: {updated}"
    );
    assert!(
        !updated.contains("let _ = 2;"),
        "old body must be gone: {updated}"
    );
    assert!(
        updated.contains("pub fn a()"),
        "unrelated symbol must stay: {updated}"
    );
    assert!(
        updated.ends_with('\n'),
        "trailing newline must be preserved: {updated:?}"
    );
}

#[test]
fn test_qartez_replace_symbol_preview_does_not_modify() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    let original = "pub fn a() {\n    let _ = 1;\n}\n";
    fs::write(src.join("lib.rs"), original).unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let result = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "a",
                "new_code": "pub fn a() { let _ = 99; }",
            }),
        )
        .expect("qartez_replace_symbol preview must succeed");
    assert!(
        result.contains("Preview"),
        "preview header must be present: {result}"
    );

    let untouched = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert_eq!(
        untouched, original,
        "preview must not write to disk: {untouched:?}"
    );
}

#[test]
fn test_qartez_insert_before_symbol_splices_at_anchor() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    let original = "pub fn a() {\n    let _ = 1;\n}\n\npub fn b() {\n    let _ = 2;\n}\n";
    fs::write(src.join("lib.rs"), original).unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let _ = server
        .call_tool_by_name(
            "qartez_insert_before_symbol",
            json!({
                "symbol": "b",
                "new_code": "pub fn helper() {}\n",
                "apply": true,
            }),
        )
        .expect("qartez_insert_before_symbol must succeed");

    let updated = fs::read_to_string(src.join("lib.rs")).unwrap();
    let helper_idx = updated
        .find("pub fn helper()")
        .expect("helper must be inserted");
    let b_idx = updated.find("pub fn b()").expect("b must still exist");
    assert!(
        helper_idx < b_idx,
        "helper must appear before anchor `b`, got: {updated}"
    );
    assert!(
        updated.contains("pub fn a()"),
        "unrelated symbol must stay: {updated}"
    );
}

#[test]
fn test_qartez_insert_after_symbol_splices_at_anchor() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    let original = "pub fn a() {\n    let _ = 1;\n}\n\npub fn b() {\n    let _ = 2;\n}\n";
    fs::write(src.join("lib.rs"), original).unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let _ = server
        .call_tool_by_name(
            "qartez_insert_after_symbol",
            json!({
                "symbol": "a",
                "new_code": "pub fn helper() {}",
                "apply": true,
            }),
        )
        .expect("qartez_insert_after_symbol must succeed");

    let updated = fs::read_to_string(src.join("lib.rs")).unwrap();
    let a_idx = updated.find("pub fn a()").expect("a must still exist");
    let helper_idx = updated
        .find("pub fn helper()")
        .expect("helper must be inserted");
    let b_idx = updated.find("pub fn b()").expect("b must still exist");
    assert!(
        a_idx < helper_idx && helper_idx < b_idx,
        "helper must appear between a and b, got: {updated}"
    );
    assert!(
        updated.ends_with('\n'),
        "trailing newline must be preserved: {updated:?}"
    );
}

#[test]
fn test_qartez_safe_delete_refuses_when_importer_exists() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(src.join("lib.rs"), "pub mod helper;\npub mod caller;\n").unwrap();
    fs::write(src.join("helper.rs"), "pub fn helper() {}\n").unwrap();
    fs::write(
        src.join("caller.rs"),
        "use crate::helper::helper;\npub fn entry() { helper(); }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let err = server
        .call_tool_by_name(
            "qartez_safe_delete",
            json!({
                "symbol": "helper",
                "apply": true,
            }),
        )
        .expect_err("delete must refuse without force");
    assert!(
        err.contains("Refusing to delete"),
        "expected refusal message, got: {err}"
    );
    // File must be untouched.
    let untouched = fs::read_to_string(src.join("helper.rs")).unwrap();
    assert!(
        untouched.contains("pub fn helper()"),
        "symbol must still be present after refused delete: {untouched}"
    );
}

#[test]
fn test_qartez_safe_delete_removes_symbol_with_force() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub fn keep() {}\n\npub fn drop_me() { let _ = 1; }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let result = server
        .call_tool_by_name(
            "qartez_safe_delete",
            json!({
                "symbol": "drop_me",
                "apply": true,
            }),
        )
        .expect("qartez_safe_delete must succeed when no importers");
    assert!(
        result.starts_with("Deleted 'drop_me'"),
        "expected delete confirmation: {result}"
    );

    let updated = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert!(
        !updated.contains("drop_me"),
        "symbol must be removed: {updated:?}"
    );
    assert!(
        updated.contains("pub fn keep()"),
        "unrelated symbol must stay: {updated:?}"
    );
    assert!(
        updated.ends_with('\n'),
        "trailing newline must be preserved: {updated:?}"
    );
}

#[test]
fn test_qartez_safe_delete_preview_lists_importers() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub mod h;\npub mod c;\n").unwrap();
    fs::write(src.join("h.rs"), "pub fn target() {}\n").unwrap();
    fs::write(
        src.join("c.rs"),
        "use crate::h::target;\npub fn entry() { target(); }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let preview = server
        .call_tool_by_name(
            "qartez_safe_delete",
            json!({
                "symbol": "target",
            }),
        )
        .expect("preview must succeed");
    assert!(
        preview.contains("Preview: delete 'target'"),
        "expected preview header: {preview}"
    );
    assert!(
        preview.contains("src/c.rs"),
        "importer must be listed in preview: {preview}"
    );

    // File must be untouched.
    let untouched = fs::read_to_string(src.join("h.rs")).unwrap();
    assert!(
        untouched.contains("pub fn target()"),
        "preview must not write to disk: {untouched}"
    );
}

// ---------------------------------------------------------------------------
// Edge cases for the new refactor tools
// ---------------------------------------------------------------------------

#[test]
fn test_qartez_replace_symbol_first_symbol_in_file() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    let original = "pub fn first() {}\n\npub fn second() {}\n";
    fs::write(src.join("lib.rs"), original).unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "first",
                "new_code": "pub fn first() { let _ = 99; }",
                "apply": true,
            }),
        )
        .expect("replace first symbol must succeed");

    let updated = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert!(
        updated.starts_with("pub fn first() { let _ = 99; }"),
        "replacement must start the file: {updated:?}"
    );
    assert!(
        updated.contains("pub fn second()"),
        "later symbols preserved: {updated:?}"
    );
}

#[test]
fn test_qartez_replace_symbol_last_symbol_no_trailing_newline() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    // No trailing newline - regression case for naive `lines().join("\n")`.
    let original = "pub fn first() {}\n\npub fn last() { 1 }";
    fs::write(src.join("lib.rs"), original).unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "last",
                "new_code": "pub fn last() { 2 }",
                "apply": true,
            }),
        )
        .expect("replace last symbol must succeed");

    let updated = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert!(
        !updated.ends_with('\n'),
        "missing trailing newline must not be invented: {updated:?}"
    );
    assert!(
        updated.contains("pub fn last() { 2 }"),
        "new code must be in file: {updated:?}"
    );
}

#[test]
fn test_qartez_replace_symbol_empty_new_code_refused() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    let original = "pub fn a() {}\n";
    fs::write(src.join("lib.rs"), original).unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let err = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "a",
                "new_code": "",
                "apply": true,
            }),
        )
        .expect_err("empty new_code must error");
    assert!(
        err.contains("Empty `new_code`"),
        "expected empty-code error: {err}"
    );
    // File must be untouched.
    let untouched = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert_eq!(untouched, original, "file must be unchanged: {untouched:?}");
}

#[test]
fn test_qartez_replace_symbol_ambiguous_errors_cleanly() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    fs::write(src.join("a.rs"), "pub fn dup() {}\n").unwrap();
    fs::write(src.join("b.rs"), "pub fn dup() {}\n").unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let err = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "dup",
                "new_code": "pub fn dup() { 1 }",
                "apply": true,
            }),
        )
        .expect_err("ambiguous symbol must error");
    assert!(
        err.contains("Multiple definitions"),
        "expected ambiguity error: {err}"
    );

    // Now disambiguate via file_path.
    let ok = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "dup",
                "new_code": "pub fn dup() { 2 }",
                "file_path": "src/a.rs",
                "apply": true,
            }),
        )
        .expect("file_path disambiguation must succeed");
    assert!(ok.contains("Replaced"), "expected apply confirmation: {ok}");

    let a_src = fs::read_to_string(src.join("a.rs")).unwrap();
    let b_src = fs::read_to_string(src.join("b.rs")).unwrap();
    assert!(
        a_src.contains("pub fn dup() { 2 }"),
        "a.rs updated: {a_src}"
    );
    assert!(
        !b_src.contains("pub fn dup() { 2 }"),
        "b.rs untouched: {b_src}"
    );
}

#[test]
fn test_qartez_replace_symbol_nonexistent_symbol_errors() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub fn a() {}\n").unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let err = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "does_not_exist",
                "new_code": "fn whatever() {}",
                "apply": true,
            }),
        )
        .expect_err("nonexistent symbol must error");
    assert!(err.contains("No symbol found"), "expected not-found: {err}");
}

#[test]
fn test_qartez_replace_symbol_path_traversal_blocked() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub fn a() {}\n").unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    // Invalid `file_path` - traversal attempt. Should not resolve to any
    // indexed file (index only holds paths relative to root).
    let err = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "a",
                "new_code": "pub fn a() { 1 }",
                "file_path": "../../../etc/passwd",
                "apply": true,
            }),
        )
        .expect_err("traversal path must not match any indexed symbol");
    assert!(
        err.contains("No symbol 'a' in file") || err.contains("No symbol found"),
        "expected filter-miss error for traversal path, got: {err}"
    );
}

#[test]
fn test_qartez_insert_preview_does_not_modify() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    let original = "pub fn anchor() {}\n";
    fs::write(src.join("lib.rs"), original).unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let preview = server
        .call_tool_by_name(
            "qartez_insert_after_symbol",
            json!({
                "symbol": "anchor",
                "new_code": "pub fn new_helper() {}",
            }),
        )
        .expect("preview must succeed");
    assert!(
        preview.contains("Preview"),
        "expected preview header: {preview}"
    );

    let untouched = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert_eq!(untouched, original, "preview must not write to disk");
}

#[test]
fn test_qartez_insert_empty_new_code_is_rejected() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    let original = "pub fn anchor() {}\n";
    fs::write(src.join("lib.rs"), original).unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let err = server
        .call_tool_by_name(
            "qartez_insert_after_symbol",
            json!({
                "symbol": "anchor",
                "new_code": "",
                "apply": true,
            }),
        )
        .expect_err("empty insert must be rejected to mirror qartez_replace_symbol");
    assert!(
        err.contains("Empty `new_code`"),
        "empty insert must surface an explicit error: {err}"
    );

    let untouched = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert_eq!(untouched, original, "rejected insert must not change file");
}

#[test]
fn test_qartez_insert_after_at_end_of_file() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    let original = "pub fn only() {}\n";
    fs::write(src.join("lib.rs"), original).unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    server
        .call_tool_by_name(
            "qartez_insert_after_symbol",
            json!({
                "symbol": "only",
                "new_code": "pub fn appended() {}",
                "apply": true,
            }),
        )
        .expect("insert after last symbol must succeed");

    let updated = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert!(
        updated.contains("pub fn only()") && updated.contains("pub fn appended()"),
        "both symbols must be present: {updated}"
    );
    assert!(
        updated.find("pub fn only()").unwrap() < updated.find("pub fn appended()").unwrap(),
        "new symbol must follow anchor: {updated}"
    );
    assert!(updated.ends_with('\n'), "trailing newline preserved");
}

#[test]
fn test_qartez_insert_before_at_start_of_file() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    let original = "pub fn first() {}\n";
    fs::write(src.join("lib.rs"), original).unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    server
        .call_tool_by_name(
            "qartez_insert_before_symbol",
            json!({
                "symbol": "first",
                "new_code": "// prelude comment",
                "apply": true,
            }),
        )
        .expect("insert before first symbol must succeed");

    let updated = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert!(
        updated.starts_with("// prelude comment"),
        "insert-before must land at file start: {updated:?}"
    );
}

#[test]
fn test_qartez_insert_preserves_unicode_content() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    // Rust allows non-ASCII string literals; tree-sitter tolerates them.
    let original = "pub fn greet() -> &'static str { \"Привет мир\" }\n";
    fs::write(src.join("lib.rs"), original).unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    server
        .call_tool_by_name(
            "qartez_insert_after_symbol",
            json!({
                "symbol": "greet",
                "new_code": "pub fn hello() -> &'static str { \"日本語\" }",
                "apply": true,
            }),
        )
        .expect("unicode content must not break insert");

    let updated = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert!(
        updated.contains("Привет мир") && updated.contains("日本語"),
        "both unicode strings must survive: {updated}"
    );
}

#[test]
fn test_qartez_safe_delete_nonexistent_symbol_errors() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub fn a() {}\n").unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let err = server
        .call_tool_by_name(
            "qartez_safe_delete",
            json!({
                "symbol": "missing",
                "apply": true,
            }),
        )
        .expect_err("nonexistent symbol must error");
    assert!(err.contains("No symbol found"), "expected not-found: {err}");
}

#[test]
fn test_qartez_safe_delete_force_drops_symbol_with_importers() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(src.join("lib.rs"), "pub mod helper;\npub mod caller;\n").unwrap();
    fs::write(src.join("helper.rs"), "pub fn helper() {}\n").unwrap();
    fs::write(
        src.join("caller.rs"),
        "use crate::helper::helper;\npub fn entry() { helper(); }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let result = server
        .call_tool_by_name(
            "qartez_safe_delete",
            json!({
                "symbol": "helper",
                "apply": true,
                "force": true,
            }),
        )
        .expect("force=true must succeed");
    assert!(result.contains("Deleted"), "expected delete: {result}");
    assert!(
        result.contains("WARNING"),
        "must warn about dangling imports: {result}"
    );

    let helper_src = fs::read_to_string(src.join("helper.rs")).unwrap();
    assert!(
        !helper_src.contains("pub fn helper()"),
        "symbol must be removed: {helper_src:?}"
    );
    // Caller is left dangling per the tool's contract.
    let caller_src = fs::read_to_string(src.join("caller.rs")).unwrap();
    assert!(
        caller_src.contains("use crate::helper::helper;"),
        "tool must NOT auto-fix importers (that is the caller's job): {caller_src}"
    );
}

#[test]
fn test_qartez_replace_symbol_preview_kind_disambiguates() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    // Free function `pub fn foo()` and impl method `Foo::foo()` share a name
    // but have different kinds (`function` vs `method`).
    fs::write(
        src.join("lib.rs"),
        "pub struct Foo;\npub fn foo() {}\nimpl Foo {\n    pub fn foo() {}\n}\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = QartezServer::new(conn, root.to_path_buf(), 300);

    let preview = server
        .call_tool_by_name(
            "qartez_replace_symbol",
            json!({
                "symbol": "foo",
                "kind": "function",
                "new_code": "pub fn foo() { let _ = 1; }",
            }),
        )
        .expect("kind filter must resolve to free function");
    assert!(
        preview.contains("Preview: replace 'foo'"),
        "preview must identify target: {preview}"
    );
}

#[test]
fn test_write_atomic_tmp_path_unique_across_concurrent_writes() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;
    use std::sync::Arc;
    use std::thread;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    for i in 0..8 {
        fs::write(
            src.join(format!("f{i}.rs")),
            format!("pub fn g{i}() {{}}\n"),
        )
        .unwrap();
    }
    let mut lib = String::from("pub mod f0;\npub mod f1;\npub mod f2;\npub mod f3;\n");
    lib.push_str("pub mod f4;\npub mod f5;\npub mod f6;\npub mod f7;\n");
    fs::write(src.join("lib.rs"), &lib).unwrap();

    let conn = setup();
    index::full_index(&conn, root, false).unwrap();
    let server = Arc::new(QartezServer::new(conn, root.to_path_buf(), 300));

    let mut handles = Vec::new();
    for i in 0..8 {
        let server = Arc::clone(&server);
        handles.push(thread::spawn(move || {
            server.call_tool_by_name(
                "qartez_replace_symbol",
                json!({
                    "symbol": format!("g{i}"),
                    "new_code": format!("pub fn g{i}() {{ let _ = {i}; }}"),
                    "apply": true,
                }),
            )
        }));
    }

    for (i, h) in handles.into_iter().enumerate() {
        let r = h.join().unwrap();
        assert!(r.is_ok(), "worker {i} must succeed, got: {:?}", r.err());
    }

    for i in 0..8 {
        let content = fs::read_to_string(src.join(format!("f{i}.rs"))).unwrap();
        assert!(
            content.contains(&format!("let _ = {i};")),
            "file f{i}.rs must contain its replacement, got: {content:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// End-to-end: qartez_health + qartez_refactor_plan on a real indexed crate.
//
// Exercises the full path: tree-sitter parse -> complexity + signature -> DB
// -> MCP dispatch -> tool impl -> output. The fixture-only tests in
// quality_tests.rs hand-craft CC numbers; this test lets the real indexer
// compute them, which is what catches any drift between what the tool expects
// to see in the DB and what the indexer actually writes.
// ---------------------------------------------------------------------------

#[test]
fn test_qartez_health_end_to_end_on_real_index() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    // A genuinely complex function: many independent branches, real body >= 50 lines.
    let mut god_body = String::from("pub fn classify(n: i32) -> &'static str {\n");
    for i in 0..25 {
        god_body.push_str(&format!(
            "    if n == {i} {{ return \"v{i}\"; }}\n    else if n < -{i} {{ return \"nv{i}\"; }}\n"
        ));
    }
    god_body.push_str("    \"unknown\"\n}\n");
    fs::write(src.join("classify.rs"), &god_body).unwrap();

    // Benign file so the "no findings" branch is exercised against a realistic repo.
    fs::write(
        src.join("simple.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);

    // Health report: classify must surface as a smell (real CC, real body).
    let health = server
        .call_tool_by_name(
            "qartez_health",
            json!({
                "max_health": 10.0,
                "min_complexity": 10,
                "min_lines": 20,
            }),
        )
        .expect("qartez_health must run against a real index");
    assert!(
        health.contains("classify"),
        "real-index health must surface classify(): {health}"
    );
    assert!(
        health.contains("src/classify.rs"),
        "path must be rendered: {health}"
    );
    // Must be in a severity bucket (Medium is expected given low churn).
    assert!(
        health.contains("Medium") || health.contains("Critical") || health.contains("High"),
        "health must emit a severity bucket, got: {health}"
    );
}

#[test]
fn test_qartez_refactor_plan_end_to_end_on_real_index() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    // One god function (high real CC + long body)
    let mut god_body = String::from("pub fn dispatch(kind: u32, payload: &str) -> u32 {\n");
    for i in 0..20 {
        god_body.push_str(&format!(
            "    if kind == {i} {{\n        if payload.starts_with(\"a{i}\") {{ return {i} + 1; }}\n        else if payload.starts_with(\"b{i}\") {{ return {i} + 2; }}\n        return {i};\n    }}\n"
        ));
    }
    god_body.push_str("    0\n}\n");

    // One long-params function in the same file
    god_body.push_str(
        "pub fn build(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32, g: i32) -> i32 { a + b + c + d + e + f + g }\n",
    );
    fs::write(src.join("dispatch.rs"), &god_body).unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();

    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);

    let plan = server
        .call_tool_by_name(
            "qartez_refactor_plan",
            json!({
                "file_path": "src/dispatch.rs",
                "min_complexity": 10,
                "min_lines": 20,
                "min_params": 5,
            }),
        )
        .expect("qartez_refactor_plan must run against a real index");

    assert!(plan.contains("Refactor Plan"), "header missing: {plan}");
    assert!(
        plan.contains("dispatch"),
        "god function must appear as a step: {plan}"
    );
    assert!(
        plan.contains("build"),
        "long-params fn must appear as a step: {plan}"
    );
    assert!(
        plan.contains("Extract Method"),
        "god step must propose Extract Method: {plan}"
    );
    assert!(
        plan.contains("Introduce Parameter Object"),
        "long-params step must propose IPO: {plan}"
    );
    // The CC impact range must render for the god step.
    assert!(
        plan.contains("-") && plan.contains("CC"),
        "CC impact range missing: {plan}"
    );
}

#[test]
fn test_qartez_health_no_findings_against_clean_repo() {
    use qartez_mcp::server::QartezServer;
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    // Two tiny functions, no smells possible.
    fs::write(
        src.join("lib.rs"),
        "pub fn one() -> i32 { 1 }\npub fn two() -> i32 { 2 }\n",
    )
    .unwrap();

    let conn = setup();
    index::full_index(&conn, dir.path(), false).unwrap();
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);

    let out = server
        .call_tool_by_name("qartez_health", json!({ "max_health": 5.0 }))
        .expect("must not error on a clean repo");
    assert!(
        out.contains("No unhealthy files") || out.contains("Showing 0"),
        "clean repo must report no findings, got: {out}"
    );
}
