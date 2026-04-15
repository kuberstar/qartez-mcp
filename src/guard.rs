//! Modification guard: core logic for the `qartez-guard` PreToolUse hook.
//!
//! Given a `(project_root, file_path, PageRank, blast_radius)` tuple and a
//! threshold config, decides whether an `Edit`/`Write`/`MultiEdit` call should
//! be denied and `qartez_impact` required first.
//!
//! Also owns the filesystem-based acknowledgment protocol: when `qartez_impact`
//! runs on a file it touches `<project_root>/.qartez/acks/<hash>`, and the
//! guard allows subsequent edits on that file within the TTL window.
//
// This module is compiled into two separate trees: the `qartez-mcp` server
// binary uses only `touch_ack`, while the `qartez-guard` hook binary uses
// everything else. Silencing `dead_code` at the module level keeps the
// server-tree build warning-free without fragmenting the module behind cfg
// flags.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

/// Default PageRank threshold above which edits require a prior `qartez_impact`.
///
/// 0.05 roughly corresponds to the top ~5% of files in a medium-sized project
/// (qartez-mcp itself has ~6 files above this line out of ~99). Tuned to fire
/// only on load-bearing hubs, not on every file.
pub const DEFAULT_PAGERANK_MIN: f64 = 0.05;

/// Default transitive blast radius threshold (number of files that would be
/// affected by a breaking change). Above this, `qartez_impact` is required.
pub const DEFAULT_BLAST_MIN: i64 = 10;

/// How long an acknowledgment (a prior `qartez_impact` call) remains valid.
/// 10 minutes matches typical Claude Code conversation windows — long enough
/// to stay out of the way, short enough that acks don't leak across sessions.
pub const DEFAULT_ACK_TTL_SECS: u64 = 600;

#[derive(Debug, Clone, Copy)]
pub struct GuardConfig {
    pub pagerank_min: f64,
    pub blast_min: i64,
    pub ack_ttl_secs: u64,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            pagerank_min: DEFAULT_PAGERANK_MIN,
            blast_min: DEFAULT_BLAST_MIN,
            ack_ttl_secs: DEFAULT_ACK_TTL_SECS,
        }
    }
}

impl GuardConfig {
    /// Load thresholds from `QARTEZ_GUARD_*` env vars, falling back to
    /// defaults. Invalid values are ignored (default is used) so a broken
    /// env var cannot break every edit in the session.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("QARTEZ_GUARD_PAGERANK_MIN")
            && let Ok(parsed) = v.parse::<f64>()
            && parsed.is_finite()
            && parsed >= 0.0
        {
            cfg.pagerank_min = parsed;
        }
        if let Ok(v) = std::env::var("QARTEZ_GUARD_BLAST_MIN")
            && let Ok(parsed) = v.parse::<i64>()
            && parsed >= 0
        {
            cfg.blast_min = parsed;
        }
        if let Ok(v) = std::env::var("QARTEZ_GUARD_ACK_TTL_SECS")
            && let Ok(parsed) = v.parse::<u64>()
        {
            cfg.ack_ttl_secs = parsed;
        }
        cfg
    }

    pub fn is_disabled_by_env() -> bool {
        std::env::var("QARTEZ_GUARD_DISABLE")
            .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
            .unwrap_or(false)
    }
}

/// Shape of the tool-use hook payload (compatible with Claude Code and Gemini CLI).
/// Only the fields the guard actually uses are deserialized — unknown fields
/// are ignored so future CLI releases adding keys don't break us.
#[derive(Debug, Deserialize)]
pub struct HookInput {
    pub tool_name: String,
    pub tool_input: ToolInput,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub hook_event_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ToolInput {
    pub file_path: Option<String>,
}

/// Result of evaluating a hook payload against the index.
#[derive(Debug, Clone)]
pub enum GuardDecision {
    /// Let the edit proceed. The guard emits an empty response and exits 0.
    Allow,
    /// Block the edit and surface `reason` to the AI via the CLI's hook contract.
    Deny { reason: String },
}

/// Per-file facts the guard needs to decide. Pulled from the qartez DB.
#[derive(Debug, Clone)]
pub struct FileFacts {
    pub rel_path: String,
    pub pagerank: f64,
    pub blast_radius: i64,
    /// Top-ranked symbols inside this file, in descending order of their
    /// symbol-level PageRank. Populated by the guard binary from
    /// `read::get_symbols_ranked_for_file`; empty on legacy DBs that
    /// predate symbol PageRank. Used purely to enrich the deny message —
    /// does not affect the Allow/Deny decision.
    pub hot_symbols: Vec<(String, f64)>,
}

/// Core decision function — pure, no I/O, so it can be unit-tested without
/// touching a SQLite database or stdin.
///
/// Returns `Deny` iff the file is "hot" (PageRank or blast radius above the
/// configured threshold) AND there is no fresh acknowledgment recorded.
pub fn evaluate(facts: &FileFacts, cfg: &GuardConfig, ack_fresh: bool) -> GuardDecision {
    let hot_pagerank = facts.pagerank >= cfg.pagerank_min;
    let hot_blast = facts.blast_radius >= cfg.blast_min;
    if !hot_pagerank && !hot_blast {
        return GuardDecision::Allow;
    }
    if ack_fresh {
        return GuardDecision::Allow;
    }
    GuardDecision::Deny {
        reason: format_deny_reason(facts, cfg, hot_pagerank, hot_blast),
    }
}

fn format_deny_reason(
    facts: &FileFacts,
    cfg: &GuardConfig,
    hot_pagerank: bool,
    hot_blast: bool,
) -> String {
    let mut triggers: Vec<String> = Vec::new();
    if hot_pagerank {
        triggers.push(format!(
            "PageRank {:.4} >= {:.4}",
            facts.pagerank, cfg.pagerank_min
        ));
    }
    if hot_blast {
        triggers.push(format!(
            "blast radius {} >= {}",
            facts.blast_radius, cfg.blast_min
        ));
    }
    let mut reason = format!(
        "STOP: `{}` is load-bearing ({}). Call `qartez_impact` with file_path=\"{}\" FIRST to review direct and transitive importers, then retry the edit. Opt out for this project: `QARTEZ_GUARD_DISABLE=1`.",
        facts.rel_path,
        triggers.join(", "),
        facts.rel_path,
    );
    if !facts.hot_symbols.is_empty() {
        // Keep it terse — one comma-separated line that Claude can scan at
        // a glance before deciding to call qartez_impact.
        let parts: Vec<String> = facts
            .hot_symbols
            .iter()
            .map(|(name, rank)| format!("{} (pr={:.3})", name, rank))
            .collect();
        reason.push_str(" Hot symbols in this file: ");
        reason.push_str(&parts.join(", "));
        reason.push('.');
    }
    reason
}

/// Render a `GuardDecision` as the exact JSON the CLI expects on stdout.
/// Handles both Claude Code (PreToolUse) and Gemini CLI (BeforeTool).
/// `Allow` produces no output; `Deny` produces the appropriate hook envelope.
pub fn render_stdout(decision: &GuardDecision, event_name: Option<&str>) -> Option<String> {
    match decision {
        GuardDecision::Allow => None,
        GuardDecision::Deny { reason } => {
            if event_name == Some("BeforeTool") {
                // Gemini CLI format
                let envelope = serde_json::json!({
                    "decision": "deny",
                    "reason": reason,
                });
                Some(envelope.to_string())
            } else {
                // Claude Code format (default)
                let envelope = serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": reason,
                    }
                });
                Some(envelope.to_string())
            }
        }
    }
}

/// Walk up from `start` until a directory containing `.qartez/index.db` is
/// found. Returns `None` if no qartez-indexed project is an ancestor.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join(".qartez").join("index.db").is_file() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Convert an absolute tool file path into the project-relative path qartez
/// stores in `files.path`. Returns `None` if the path is outside the project.
pub fn relativize_file_path(project_root: &Path, file_path: &Path) -> Option<String> {
    let canonical_root = project_root.canonicalize().ok()?;
    let canonical_file = file_path
        .canonicalize()
        .ok()
        .unwrap_or_else(|| file_path.to_path_buf());
    canonical_file
        .strip_prefix(&canonical_root)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Compute the acknowledgment file path for a given project-relative file.
/// Uses a FNV-1a hash for stability across Rust compiler versions.
pub fn ack_path(project_root: &Path, rel_path: &str) -> PathBuf {
    let digest = format!("{:016x}", fnv1a_64(rel_path.as_bytes()));
    project_root.join(".qartez").join("acks").join(digest)
}

/// FNV-1a 64-bit hash, stable across Rust versions (unlike `DefaultHasher`).
fn fnv1a_64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Called by `qartez_impact` when Claude successfully reviews a file. Writes
/// (or touches) the ack file so subsequent edits within `ack_ttl_secs` are
/// let through. Failures are swallowed — ack is an optimisation, not a
/// correctness guarantee.
pub fn touch_ack(project_root: &Path, rel_path: &str) {
    let path = ack_path(project_root, rel_path);
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = std::fs::write(&path, ts.to_string());
}

/// Returns `true` iff an ack file exists for `rel_path` and its mtime is
/// within the TTL window. Missing file, unreadable metadata, or future mtime
/// all count as "not fresh".
pub fn ack_is_fresh(project_root: &Path, rel_path: &str, ttl_secs: u64) -> bool {
    let path = ack_path(project_root, rel_path);
    let Ok(meta) = std::fs::metadata(&path) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    let Ok(elapsed) = SystemTime::now().duration_since(mtime) else {
        return false;
    };
    elapsed < Duration::from_secs(ttl_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    fn facts(rel: &str, pr: f64, blast: i64) -> FileFacts {
        FileFacts {
            rel_path: rel.to_string(),
            pagerank: pr,
            blast_radius: blast,
            hot_symbols: Vec::new(),
        }
    }

    fn facts_with_hot(rel: &str, pr: f64, blast: i64, hot: &[(&str, f64)]) -> FileFacts {
        FileFacts {
            rel_path: rel.to_string(),
            pagerank: pr,
            blast_radius: blast,
            hot_symbols: hot.iter().map(|(n, r)| ((*n).to_string(), *r)).collect(),
        }
    }

    #[test]
    fn cold_file_is_allowed() {
        let cfg = GuardConfig::default();
        let decision = evaluate(&facts("src/x.rs", 0.001, 2), &cfg, false);
        assert!(matches!(decision, GuardDecision::Allow));
    }

    #[test]
    fn hot_pagerank_is_denied_without_ack() {
        let cfg = GuardConfig::default();
        let decision = evaluate(&facts("src/hub.rs", 0.09, 2), &cfg, false);
        match decision {
            GuardDecision::Deny { reason } => {
                assert!(reason.contains("PageRank"));
                assert!(reason.contains("src/hub.rs"));
                assert!(reason.contains("qartez_impact"));
            }
            GuardDecision::Allow => panic!("expected deny"),
        }
    }

    #[test]
    fn hot_blast_is_denied_without_ack() {
        let cfg = GuardConfig::default();
        let decision = evaluate(&facts("src/core.rs", 0.001, 30), &cfg, false);
        match decision {
            GuardDecision::Deny { reason } => {
                assert!(reason.contains("blast radius 30"));
            }
            GuardDecision::Allow => panic!("expected deny"),
        }
    }

    #[test]
    fn ack_flips_deny_to_allow() {
        let cfg = GuardConfig::default();
        let decision = evaluate(&facts("src/hub.rs", 0.09, 30), &cfg, true);
        assert!(matches!(decision, GuardDecision::Allow));
    }

    #[test]
    fn equal_to_threshold_counts_as_hot() {
        let cfg = GuardConfig::default();
        let decision = evaluate(&facts("src/edge.rs", DEFAULT_PAGERANK_MIN, 0), &cfg, false);
        assert!(matches!(decision, GuardDecision::Deny { .. }));
    }

    #[test]
    fn custom_thresholds_respected() {
        let cfg = GuardConfig {
            pagerank_min: 0.5,
            blast_min: 1000,
            ack_ttl_secs: 600,
        };
        let decision = evaluate(&facts("src/hub.rs", 0.09, 30), &cfg, false);
        assert!(matches!(decision, GuardDecision::Allow));
    }

    #[test]
    fn deny_message_includes_hot_symbols_when_present() {
        let cfg = GuardConfig::default();
        let f = facts_with_hot(
            "src/hub.rs",
            0.09,
            30,
            &[("parse_tags", 0.034), ("extract_imports", 0.021)],
        );
        match evaluate(&f, &cfg, false) {
            GuardDecision::Deny { reason } => {
                assert!(
                    reason.contains("parse_tags (pr=0.034)"),
                    "deny reason should include top hot symbol: {reason}"
                );
                assert!(
                    reason.contains("extract_imports (pr=0.021)"),
                    "deny reason should include second hot symbol: {reason}"
                );
                assert!(
                    reason.contains("Hot symbols in this file:"),
                    "deny reason should label the hot symbols section: {reason}"
                );
            }
            GuardDecision::Allow => panic!("expected deny"),
        }
    }

    #[test]
    fn deny_message_omits_hot_symbols_line_when_empty() {
        let cfg = GuardConfig::default();
        match evaluate(&facts("src/hub.rs", 0.09, 2), &cfg, false) {
            GuardDecision::Deny { reason } => {
                assert!(
                    !reason.contains("Hot symbols in this file:"),
                    "deny reason must not mention hot symbols when empty: {reason}"
                );
            }
            GuardDecision::Allow => panic!("expected deny"),
        }
    }

    #[test]
    fn render_stdout_allow_is_empty() {
        assert!(render_stdout(&GuardDecision::Allow, None).is_none());
    }

    #[test]
    fn render_stdout_deny_is_valid_json_claude() {
        let out = render_stdout(
            &GuardDecision::Deny {
                reason: "test".to_string(),
            },
            Some("PreToolUse"),
        )
        .expect("expected some JSON for deny");
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(parsed["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            parsed["hookSpecificOutput"]["permissionDecisionReason"],
            "test"
        );
    }

    #[test]
    fn render_stdout_deny_is_valid_json_gemini() {
        let out = render_stdout(
            &GuardDecision::Deny {
                reason: "test".to_string(),
            },
            Some("BeforeTool"),
        )
        .expect("expected some JSON for deny");
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(parsed["decision"], "deny");
        assert_eq!(parsed["reason"], "test");
    }

    #[test]
    fn parse_hook_input_minimal() {
        let raw = r#"{
            "tool_name": "Edit",
            "tool_input": { "file_path": "/abs/path/foo.rs" },
            "cwd": "/abs/path"
        }"#;
        let parsed: HookInput = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.tool_name, "Edit");
        assert_eq!(
            parsed.tool_input.file_path.as_deref(),
            Some("/abs/path/foo.rs")
        );
    }

    #[test]
    fn parse_hook_input_ignores_unknown() {
        let raw = r#"{
            "tool_name": "Write",
            "tool_input": { "file_path": "/x", "content": "...", "newKey": 42 },
            "session_id": "abc",
            "hook_event_name": "PreToolUse"
        }"#;
        let parsed: HookInput = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.tool_name, "Write");
    }

    #[test]
    fn env_config_clamps_bad_values() {
        // SAFETY: integration tests don't run in parallel by default here,
        // and these keys are namespaced.
        unsafe {
            std::env::set_var("QARTEZ_GUARD_PAGERANK_MIN", "not-a-number");
            std::env::set_var("QARTEZ_GUARD_BLAST_MIN", "-5");
        }
        let cfg = GuardConfig::from_env();
        assert_eq!(cfg.pagerank_min, DEFAULT_PAGERANK_MIN);
        assert_eq!(cfg.blast_min, DEFAULT_BLAST_MIN);
        unsafe {
            std::env::remove_var("QARTEZ_GUARD_PAGERANK_MIN");
            std::env::remove_var("QARTEZ_GUARD_BLAST_MIN");
        }
    }

    #[test]
    fn ack_roundtrip_under_ttl() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let rel = "src/core.rs";
        assert!(!ack_is_fresh(root, rel, 60));
        touch_ack(root, rel);
        assert!(ack_is_fresh(root, rel, 60));
    }

    #[test]
    fn ack_expires_past_ttl() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let rel = "src/core.rs";
        touch_ack(root, rel);
        sleep(Duration::from_millis(50));
        // TTL of 0 means "never fresh".
        assert!(!ack_is_fresh(root, rel, 0));
    }

    #[test]
    fn ack_paths_are_stable_per_relpath() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let p1 = ack_path(root, "src/foo.rs");
        let p2 = ack_path(root, "src/foo.rs");
        let p3 = ack_path(root, "src/bar.rs");
        assert_eq!(p1, p2);
        assert_ne!(p1, p3);
    }
}
