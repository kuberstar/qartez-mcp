// Rust guideline compliant 2026-04-22
//
// End-to-end regression for the five-bug batch tracked in the
// `session-186d06c7` fix-stack:
//
//   D1. qartez_test_gaps `mode=gaps` must respect `file_path` scope.
//   D3. qartez_security must keep production findings from a file that
//       also contains a `#[cfg(test)]` block (suppress INSIDE the block
//       only, not the whole file).
//   D4. qartez_trend must apply `symbol_name` pre-limit so a filtered
//       call returns exactly one trend entry.
//   D6. qartez_diff_impact must be side-effect-free when `ack=false`:
//       no `.qartez/acks/` entries are written and the report omits the
//       "Guard ACK written" footer.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use qartez_mcp::graph::security::{self, ScanOptions, Severity};
use qartez_mcp::index;
use qartez_mcp::server::QartezServer;
use qartez_mcp::storage::schema;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn setup_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    schema::create_schema(&conn).unwrap();
    conn
}

fn build_and_index(dir: &Path) -> QartezServer {
    fs::create_dir_all(dir.join(".git")).unwrap();
    let conn = setup_db();
    index::full_index(&conn, dir, false).unwrap();
    QartezServer::new(conn, dir.to_path_buf(), 0)
}

fn write_cargo_manifest(dir: &Path) {
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// D1: test_gaps respects file_path in gaps mode
// ---------------------------------------------------------------------------

fn write_gaps_scope_fixture(dir: &Path) {
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    let a = src.join("a");
    let b = src.join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();

    // src/a/only_in_a.rs is untested and lives in directory A.
    fs::write(a.join("only_in_a.rs"), "pub fn untested_a() -> u32 { 1 }\n").unwrap();
    // src/b/only_in_b.rs is untested and lives in directory B.
    fs::write(b.join("only_in_b.rs"), "pub fn untested_b() -> u32 { 2 }\n").unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub mod a { pub mod only_in_a; }\npub mod b { pub mod only_in_b; }\n",
    )
    .unwrap();
}

#[test]
fn test_gaps_respects_file_path_scope_to_directory() {
    let dir = TempDir::new().unwrap();
    write_cargo_manifest(dir.path());
    write_gaps_scope_fixture(dir.path());
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({
                "mode": "gaps",
                "file_path": "src/a",
                "limit": 200,
                "format": "concise",
            }),
        )
        .expect("qartez_test_gaps gaps mode with file_path scope should succeed");

    assert!(
        out.contains("src/a/only_in_a.rs"),
        "scoped gaps must include src/a/only_in_a.rs: {out}"
    );
    assert!(
        !out.contains("src/b/only_in_b.rs"),
        "scoped gaps must NOT include src/b/only_in_b.rs when file_path='src/a': {out}"
    );
}

#[test]
fn test_gaps_respects_file_path_scope_to_single_file() {
    let dir = TempDir::new().unwrap();
    write_cargo_manifest(dir.path());
    write_gaps_scope_fixture(dir.path());
    let server = build_and_index(dir.path());

    let out = server
        .call_tool_by_name(
            "qartez_test_gaps",
            json!({
                "mode": "gaps",
                "file_path": "src/b/only_in_b.rs",
                "limit": 200,
                "format": "concise",
            }),
        )
        .expect("qartez_test_gaps gaps mode with exact file scope should succeed");

    assert!(
        out.contains("src/b/only_in_b.rs"),
        "exact-file gaps must include the scoped file: {out}"
    );
    assert!(
        !out.contains("src/a/only_in_a.rs"),
        "exact-file gaps must NOT include files outside the scope: {out}"
    );
}

// ---------------------------------------------------------------------------
// D3: security scan preserves production findings even when the file
//     contains a #[cfg(test)] block
// ---------------------------------------------------------------------------

// Production symbols carry SEC006 (md5) and SEC008 (unsafe). The
// `#[cfg(test)]` block carries a SEC006 hit that must be suppressed by
// default. The fixture proves the suppression is scoped to the
// `#[cfg(test)]` mod AND does not bail on the whole file.
const MIXED_PROD_AND_TEST_FIXTURE: &str = r#"pub fn use_md5_in_prod() -> &'static str {
    // MD5 token triggers SEC006.
    "md5-digest"
}

pub unsafe fn raw_access() {
    // Unsafe block triggers SEC008 in production code.
    let _ptr: *const u8 = std::ptr::null();
}

#[cfg(test)]
mod tests {
    #[test]
    fn uses_md5_inside_tests() {
        // SEC006 inside #[cfg(test)] must be suppressed on default scans.
        let _fake_hash = "md5-digest-in-test";
    }
}
"#;

#[test]
fn security_keeps_production_findings_when_cfg_test_present() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("prod_mixed.rs"), MIXED_PROD_AND_TEST_FIXTURE).unwrap();

    let conn = setup_db();
    index::full_index(&conn, dir.path(), false).unwrap();

    let rules = security::builtin_rules();
    let opts = ScanOptions {
        include_tests: false,
        category_filter: None,
        min_severity: Severity::Low,
        file_path_filter: None,
        project_roots: vec![dir.path().to_path_buf()],
        root_aliases: HashMap::new(),
    };
    let findings = security::scan(&conn, &rules, &opts);

    let prod_md5 = findings
        .iter()
        .any(|f| f.rule_id == "SEC006" && f.symbol_name == "use_md5_in_prod");
    let prod_unsafe = findings
        .iter()
        .any(|f| f.rule_id == "SEC008" && f.symbol_name == "raw_access");
    let test_md5 = findings
        .iter()
        .any(|f| f.rule_id == "SEC006" && f.symbol_name == "uses_md5_inside_tests");

    assert!(
        prod_md5,
        "production SEC006 in use_md5_in_prod must survive when file also has a #[cfg(test)] block; \
         got findings: {:?}",
        findings
            .iter()
            .map(|f| (&f.rule_id, &f.symbol_name))
            .collect::<Vec<_>>(),
    );
    assert!(
        prod_unsafe,
        "production SEC008 (unsafe) in raw_access must survive the cfg-test suppression; \
         got findings: {:?}",
        findings
            .iter()
            .map(|f| (&f.rule_id, &f.symbol_name))
            .collect::<Vec<_>>(),
    );
    assert!(
        !test_md5,
        "SEC006 inside #[cfg(test)] mod must be suppressed by default; got findings: {:?}",
        findings
            .iter()
            .map(|f| (&f.rule_id, &f.symbol_name))
            .collect::<Vec<_>>(),
    );
}

// ---------------------------------------------------------------------------
// D4: trend applies symbol_name filter BEFORE the limit
// ---------------------------------------------------------------------------

fn git_run(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .expect("git must be installed for this regression test");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn init_git_with_two_commits(dir: &Path, file_rel: &str, v1: &str, v2: &str) {
    git_run(dir, &["init", "-q"]);
    git_run(dir, &["config", "user.email", "test@example.com"]);
    git_run(dir, &["config", "user.name", "Test"]);
    git_run(dir, &["config", "commit.gpgsign", "false"]);

    fs::write(dir.join(file_rel), v1).unwrap();
    git_run(dir, &["add", "."]);
    git_run(dir, &["commit", "-q", "-m", "v1"]);

    fs::write(dir.join(file_rel), v2).unwrap();
    git_run(dir, &["add", "."]);
    git_run(dir, &["commit", "-q", "-m", "v2"]);
}

const TREND_MULTI_SYMBOL_V1: &str = r#"pub fn simple_alpha(x: u32) -> u32 { x + 1 }
pub fn simple_beta(x: u32) -> u32 { x + 2 }
pub fn simple_gamma(x: u32) -> u32 { x + 3 }
"#;

const TREND_MULTI_SYMBOL_V2: &str = r#"pub fn simple_alpha(x: u32) -> u32 {
    if x > 0 { x + 1 } else { 0 }
}
pub fn simple_beta(x: u32) -> u32 {
    if x > 10 { x + 2 } else if x > 5 { x + 3 } else { x }
}
pub fn simple_gamma(x: u32) -> u32 {
    match x { 0 => 1, 1 => 2, 2 => 3, _ => x + 3 }
}
"#;

#[test]
fn trend_symbol_name_filters_before_limit() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    write_cargo_manifest(dir.path());
    fs::write(src.join("lib.rs"), "pub mod mathy;\n").unwrap();
    init_git_with_two_commits(
        dir.path(),
        "src/mathy.rs",
        TREND_MULTI_SYMBOL_V1,
        TREND_MULTI_SYMBOL_V2,
    );

    let conn = setup_db();
    index::full_index(&conn, dir.path(), false).unwrap();
    // git_depth must be non-zero for qartez_trend to accept the call.
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);

    let out = server
        .call_tool_by_name(
            "qartez_trend",
            json!({
                "file_path": "src/mathy.rs",
                "symbol_name": "simple_beta",
                "limit": 10,
                "format": "detailed",
            }),
        )
        .expect("qartez_trend with symbol_name should succeed");

    assert!(
        out.contains("simple_beta"),
        "trend must surface the requested symbol: {out}"
    );
    assert!(
        !out.contains("simple_alpha"),
        "trend must NOT include unrelated symbols when symbol_name is set: {out}"
    );
    assert!(
        !out.contains("simple_gamma"),
        "trend must NOT include unrelated symbols when symbol_name is set: {out}"
    );
}

#[test]
fn trend_limit_clamps_to_fifty_server_side() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    write_cargo_manifest(dir.path());
    fs::write(src.join("lib.rs"), "pub mod mathy;\n").unwrap();
    init_git_with_two_commits(
        dir.path(),
        "src/mathy.rs",
        TREND_MULTI_SYMBOL_V1,
        TREND_MULTI_SYMBOL_V2,
    );

    let conn = setup_db();
    index::full_index(&conn, dir.path(), false).unwrap();
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);

    // Far above the documented 50-commit cap - must not error, must not
    // produce a runaway output.
    let out = server
        .call_tool_by_name(
            "qartez_trend",
            json!({
                "file_path": "src/mathy.rs",
                "limit": 5_000,
                "format": "detailed",
            }),
        )
        .expect("qartez_trend with oversized limit should succeed after clamping");

    assert!(
        !out.is_empty(),
        "qartez_trend must still return output after clamp"
    );
}

// ---------------------------------------------------------------------------
// D6: diff_impact is side-effect-free when ack=false
// ---------------------------------------------------------------------------

const DIFF_IMPACT_V1: &str = "pub fn changed() -> u32 { 1 }\n";
const DIFF_IMPACT_V2: &str = "pub fn changed() -> u32 { 2 }\n";

fn setup_diff_impact_project() -> TempDir {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    write_cargo_manifest(dir.path());
    fs::write(src.join("lib.rs"), "pub mod touched;\n").unwrap();

    init_git_with_two_commits(dir.path(), "src/touched.rs", DIFF_IMPACT_V1, DIFF_IMPACT_V2);
    dir
}

#[test]
fn diff_impact_has_no_side_effects_when_ack_false() {
    let dir = setup_diff_impact_project();
    let conn = setup_db();
    index::full_index(&conn, dir.path(), false).unwrap();
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);

    let acks_dir = dir.path().join(".qartez").join("acks");
    assert!(
        !acks_dir.exists(),
        "acks dir must not exist before the call; present at: {}",
        acks_dir.display(),
    );

    let out = server
        .call_tool_by_name(
            "qartez_diff_impact",
            json!({ "base": "HEAD~1..HEAD", "format": "detailed" }),
        )
        .expect("qartez_diff_impact should succeed");

    assert!(
        out.contains("src/touched.rs"),
        "report must still surface the changed file: {out}"
    );
    assert!(
        !out.contains("Guard ACK written"),
        "default read call must not emit the Guard ACK footer: {out}"
    );
    assert!(
        !acks_dir.exists() || fs::read_dir(&acks_dir).map(|r| r.count()).unwrap_or(0) == 0,
        "default read call must not create any .qartez/acks/ entries; dir={}",
        acks_dir.display(),
    );
}

#[test]
fn diff_impact_writes_acks_when_ack_true() {
    let dir = setup_diff_impact_project();
    let conn = setup_db();
    index::full_index(&conn, dir.path(), false).unwrap();
    let server = QartezServer::new(conn, dir.path().to_path_buf(), 300);

    let _ = server
        .call_tool_by_name(
            "qartez_diff_impact",
            json!({ "base": "HEAD~1..HEAD", "ack": true, "format": "detailed" }),
        )
        .expect("qartez_diff_impact with ack=true should succeed");

    let acks_dir = dir.path().join(".qartez").join("acks");
    assert!(
        acks_dir.exists(),
        "ack=true must create the .qartez/acks/ directory"
    );
    let entries = fs::read_dir(&acks_dir).unwrap().count();
    assert!(
        entries > 0,
        "ack=true must write at least one ack entry, got {entries}"
    );
}
