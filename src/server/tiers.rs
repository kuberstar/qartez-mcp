// Rust guideline compliant 2026-04-15

//! Progressive tool disclosure tiers and runtime state.
//!
//! When `QARTEZ_PROGRESSIVE=1` is set, the server starts with only the
//! **core** tier visible to MCP clients. Additional tiers are unlocked
//! on demand via the `qartez_tools` meta-tool, which sends a
//! `notifications/tools/list_changed` notification after each mutation.
//!
//! When the env var is absent (the default), all tools are visible from
//! the start, preserving backwards compatibility.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

/// Tool names that form the always-visible core tier.
///
/// These eight tools cover the navigate + safety workflow and are
/// sufficient for the vast majority of code-exploration sessions.
pub(super) const TIER_CORE: &[&str] = &[
    "qartez_map",
    "qartez_workspace",
    "qartez_find",
    "qartez_grep",
    "qartez_read",
    "qartez_outline",
    "qartez_impact",
    "qartez_deps",
    "qartez_stats",
];

/// Deep-analysis tools unlocked on demand.
pub(super) const TIER_ANALYSIS: &[&str] = &[
    "qartez_refs",
    "qartez_calls",
    "qartez_cochange",
    "qartez_context",
    "qartez_unused",
    "qartez_diff_impact",
    "qartez_hotspots",
    "qartez_clones",
    "qartez_smells",
    "qartez_boundaries",
    "qartez_hierarchy",
    "qartez_trend",
    "qartez_security",
    "qartez_semantic",
    "qartez_test_gaps",
    "qartez_knowledge",
];

/// Destructive refactoring tools unlocked on demand.
pub(super) const TIER_REFACTOR: &[&str] = &["qartez_rename", "qartez_move", "qartez_rename_file"];

/// Build and documentation meta-tools.
pub(super) const TIER_META: &[&str] = &["qartez_project", "qartez_wiki"];

/// The `qartez_tools` discovery tool is always visible regardless of mode.
pub(super) const META_TOOL_NAME: &str = "qartez_tools";

/// Maps a tier name string to its tool list.
pub(super) fn tier_tools(tier: &str) -> Option<&'static [&'static str]> {
    match tier {
        "core" => Some(TIER_CORE),
        "analysis" => Some(TIER_ANALYSIS),
        "refactor" => Some(TIER_REFACTOR),
        "meta" => Some(TIER_META),
        _ => None,
    }
}

/// All known tier names for enumeration.
pub(super) const ALL_TIER_NAMES: &[&str] = &["core", "analysis", "refactor", "meta"];

/// Shared mutable set of currently enabled tool names.
///
/// Wrapped in `Arc<RwLock<>>` because `QartezServer` is `Clone` and tool
/// handlers run concurrently. Reads (list_tools) take a shared lock;
/// writes (qartez_tools enable/disable) take an exclusive lock.
pub(super) type EnabledTools = Arc<RwLock<HashSet<String>>>;

/// Returns true when `QARTEZ_PROGRESSIVE=1` is set in the environment.
pub(super) fn is_progressive_mode() -> bool {
    std::env::var("QARTEZ_PROGRESSIVE")
        .ok()
        .is_some_and(|v| v == "1")
}

/// Build the initial set of enabled tool names.
///
/// In progressive mode, only core tools and `qartez_tools` are enabled.
/// Otherwise every tool is enabled.
pub(super) fn initial_enabled_tools(all_tool_names: &[String]) -> EnabledTools {
    let set = if is_progressive_mode() {
        let mut s: HashSet<String> = TIER_CORE.iter().map(|&n| n.to_owned()).collect();
        s.insert(META_TOOL_NAME.to_owned());
        s
    } else {
        let mut s: HashSet<String> = all_tool_names.iter().cloned().collect();
        s.insert(META_TOOL_NAME.to_owned());
        s
    };
    Arc::new(RwLock::new(set))
}
