// Rust guideline compliant 2026-04-13

//! Architecture boundary enforcement over the file-level import graph.
//!
//! Users declare path-scoped rules in `.qartez/boundaries.toml` - files
//! matching a `from` glob must not import files matching any `deny` glob,
//! with an optional `allow` override for cross-cutting concerns (shared
//! error types, logging, DTOs, etc.). The checker walks `edges`, resolves
//! each endpoint to its path, and emits one `Violation` per offending
//! edge.
//!
//! Rules are keyed on directory globs rather than Leiden cluster ids
//! because cluster ids are unstable across re-indexing: adding or
//! removing a single file may reshuffle the community assignment, which
//! would silently break rules keyed on cluster membership. Directory
//! layouts, by contrast, change rarely and on purpose.
//!
//! `suggest_boundaries` inspects the current Leiden clustering and
//! produces a starter config whose `from` prefixes match the dominant
//! directory of each non-misc cluster and whose `deny` lists include
//! every other cluster prefix that the current edge graph does not
//! already traverse - i.e. it freezes the existing architecture so the
//! user can relax the rules intentionally rather than accidentally
//! ratifying drift.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use globset::{Glob, GlobMatcher};
use serde::{Deserialize, Serialize};

use crate::error::{QartezError, Result};
use crate::graph::leiden::MISC_CLUSTER_ID;
use crate::storage::models::FileRow;

/// Parsed form of `.qartez/boundaries.toml`.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct BoundaryConfig {
    #[serde(default)]
    pub boundary: Vec<BoundaryRule>,
}

/// A single rule. `from`, `deny`, and `allow` are all glob patterns
/// matched against file paths relative to the project root.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BoundaryRule {
    pub from: String,
    pub deny: Vec<String>,
    #[serde(default)]
    pub allow: Vec<String>,
}

/// One offending edge reported by [`check_boundaries`]. `rule_index` is
/// the position of the triggering rule in `config.boundary`; callers use
/// it to group or summarize violations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub from_file: String,
    pub to_file: String,
    pub rule_index: usize,
    pub deny_pattern: String,
}

/// Read and parse a TOML config file at `path`.
pub fn load_config(path: &Path) -> Result<BoundaryConfig> {
    let text = std::fs::read_to_string(path)?;
    parse_config(&text, path)
}

/// Parse a TOML document into a [`BoundaryConfig`]. Validates every glob
/// pattern up front so misconfigurations fail at load time rather than
/// silently matching zero edges during the walk.
pub fn parse_config(text: &str, path: &Path) -> Result<BoundaryConfig> {
    let config: BoundaryConfig = toml_edit::de::from_str(text).map_err(|e| {
        QartezError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: {e}", path.display()),
        ))
    })?;
    for (idx, rule) in config.boundary.iter().enumerate() {
        validate_glob(&rule.from, path, idx, "from")?;
        for pattern in &rule.deny {
            validate_glob(pattern, path, idx, "deny")?;
        }
        for pattern in &rule.allow {
            validate_glob(pattern, path, idx, "allow")?;
        }
    }
    Ok(config)
}

fn validate_glob(pattern: &str, path: &Path, idx: usize, field: &str) -> Result<()> {
    Glob::new(pattern).map(|_| ()).map_err(|e| {
        QartezError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{}: rule #{idx} `{field}` pattern {pattern:?} is invalid: {e}",
                path.display()
            ),
        ))
    })
}

/// Walk `edges` and collect every edge that violates a rule. Rules are
/// evaluated in order; within a rule, `allow` overrides `deny`. Each
/// offending edge is reported exactly once, against the first rule that
/// catches it.
///
/// The returned list is sorted by `(rule_index, from_file, to_file)` so
/// re-running the checker on the same DB produces byte-identical output
/// - important for CI gating and for writing deterministic tests.
pub fn check_boundaries(
    config: &BoundaryConfig,
    files: &[FileRow],
    edges: &[(i64, i64)],
) -> Vec<Violation> {
    if config.boundary.is_empty() {
        return Vec::new();
    }
    let compiled: Vec<CompiledRule> = config
        .boundary
        .iter()
        .map(CompiledRule::from_rule)
        .collect();
    let id_to_path: HashMap<i64, &str> = files.iter().map(|f| (f.id, f.path.as_str())).collect();

    let mut out: Vec<Violation> = Vec::new();
    for &(from_id, to_id) in edges {
        let (Some(&from_path), Some(&to_path)) = (id_to_path.get(&from_id), id_to_path.get(&to_id))
        else {
            continue;
        };
        for (idx, rule) in compiled.iter().enumerate() {
            if !rule.from.is_match(from_path) {
                continue;
            }
            let Some(deny_pattern) = rule.first_deny_match(to_path) else {
                continue;
            };
            if rule.is_allowed(to_path) {
                continue;
            }
            out.push(Violation {
                from_file: from_path.to_string(),
                to_file: to_path.to_string(),
                rule_index: idx,
                deny_pattern,
            });
            break;
        }
    }

    out.sort_by(|a, b| {
        a.rule_index
            .cmp(&b.rule_index)
            .then_with(|| a.from_file.cmp(&b.from_file))
            .then_with(|| a.to_file.cmp(&b.to_file))
    });
    out
}

struct CompiledRule {
    from: GlobMatcher,
    denies: Vec<(String, GlobMatcher)>,
    allows: Vec<GlobMatcher>,
}

impl CompiledRule {
    fn from_rule(rule: &BoundaryRule) -> Self {
        Self {
            from: compile_glob(&rule.from),
            denies: rule
                .deny
                .iter()
                .map(|p| (p.clone(), compile_glob(p)))
                .collect(),
            allows: rule.allow.iter().map(|p| compile_glob(p)).collect(),
        }
    }

    fn first_deny_match(&self, path: &str) -> Option<String> {
        self.denies
            .iter()
            .find(|(_, m)| m.is_match(path))
            .map(|(p, _)| p.clone())
    }

    fn is_allowed(&self, path: &str) -> bool {
        self.allows.iter().any(|m| m.is_match(path))
    }
}

// Patterns that fail to compile are already rejected by `parse_config`,
// so the unwrap path here only fires when a caller constructs a
// `BoundaryConfig` in memory with a broken pattern. In that case the
// rule is degraded to "matches nothing" rather than panicking at query
// time - the user-supplied input path is parse_config, not this one.
fn compile_glob(pattern: &str) -> GlobMatcher {
    Glob::new(pattern)
        .map(|g| g.compile_matcher())
        .unwrap_or_else(|_| {
            Glob::new("\0__qartez_unreachable__\0")
                .expect("hard-coded literal glob must compile")
                .compile_matcher()
        })
}

/// Generate a starter config from the current Leiden clustering. One
/// rule is produced per non-misc cluster whose files share a
/// two-segment directory prefix; the rule's `deny` list contains every
/// other cluster prefix the current edge graph does not already
/// traverse. Clusters without a clean prefix are skipped to keep the
/// generated rules readable.
pub fn suggest_boundaries(
    files: &[FileRow],
    clusters: &[(i64, i64)],
    edges: &[(i64, i64)],
) -> BoundaryConfig {
    let cluster_map: HashMap<i64, i64> = clusters.iter().copied().collect();

    let mut groups: BTreeMap<i64, Vec<&FileRow>> = BTreeMap::new();
    for file in files {
        let cid = cluster_map
            .get(&file.id)
            .copied()
            .unwrap_or(MISC_CLUSTER_ID);
        if cid == MISC_CLUSTER_ID {
            continue;
        }
        groups.entry(cid).or_default().push(file);
    }

    let mut prefixes: BTreeMap<i64, String> = BTreeMap::new();
    for (&cid, members) in &groups {
        if let Some(prefix) = longest_common_dir(members)
            && prefix.split('/').count() >= 2
        {
            prefixes.insert(cid, prefix);
        }
    }

    let mut edge_counts: HashMap<(i64, i64), usize> = HashMap::new();
    for &(from, to) in edges {
        let fc = cluster_map.get(&from).copied().unwrap_or(MISC_CLUSTER_ID);
        let tc = cluster_map.get(&to).copied().unwrap_or(MISC_CLUSTER_ID);
        if fc == tc || fc == MISC_CLUSTER_ID || tc == MISC_CLUSTER_ID {
            continue;
        }
        *edge_counts.entry((fc, tc)).or_insert(0) += 1;
    }

    let cluster_ids: Vec<i64> = prefixes.keys().copied().collect();
    let mut rules: Vec<BoundaryRule> = Vec::new();
    for &from_cid in &cluster_ids {
        let from_prefix = &prefixes[&from_cid];
        let mut deny: Vec<String> = Vec::new();
        for &to_cid in &cluster_ids {
            if to_cid == from_cid {
                continue;
            }
            let count = edge_counts.get(&(from_cid, to_cid)).copied().unwrap_or(0);
            if count == 0 {
                deny.push(format!("{}/**", prefixes[&to_cid]));
            }
        }
        if deny.is_empty() {
            continue;
        }
        deny.sort();
        rules.push(BoundaryRule {
            from: format!("{from_prefix}/**"),
            deny,
            allow: Vec::new(),
        });
    }

    BoundaryConfig { boundary: rules }
}

/// Render a [`BoundaryConfig`] as TOML. Written by hand rather than via
/// `toml_edit::ser` so the `suggest_boundaries` output can include
/// heading comments and a stable rule ordering without pulling in an
/// extra serialization path.
pub fn render_config_toml(config: &BoundaryConfig) -> String {
    let mut out = String::new();
    out.push_str("# Qartez architecture boundaries.\n");
    out.push_str("# Files matching `from` must not import any file matching a `deny` pattern.\n");
    out.push_str("# Optional `allow` overrides deny for cross-cutting dependencies.\n\n");
    for rule in &config.boundary {
        out.push_str("[[boundary]]\n");
        out.push_str(&format!("from = {}\n", toml_string(&rule.from)));
        out.push_str("deny = [");
        let items: Vec<String> = rule.deny.iter().map(|d| toml_string(d)).collect();
        out.push_str(&items.join(", "));
        out.push_str("]\n");
        if !rule.allow.is_empty() {
            out.push_str("allow = [");
            let items: Vec<String> = rule.allow.iter().map(|a| toml_string(a)).collect();
            out.push_str(&items.join(", "));
            out.push_str("]\n");
        }
        out.push('\n');
    }
    out
}

fn toml_string(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn longest_common_dir(files: &[&FileRow]) -> Option<String> {
    let first = files.first()?;
    let first_parts: Vec<&str> = first.path.split('/').collect();
    if first_parts.len() <= 1 {
        return None;
    }
    let mut shared = first_parts.len() - 1;
    for file in files.iter().skip(1) {
        let parts: Vec<&str> = file.path.split('/').collect();
        let limit = shared.min(parts.len().saturating_sub(1));
        let mut matched = 0;
        while matched < limit && parts[matched] == first_parts[matched] {
            matched += 1;
        }
        shared = matched;
        if shared == 0 {
            return None;
        }
    }
    if shared == 0 {
        return None;
    }
    Some(first_parts[..shared].join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: i64, path: &str) -> FileRow {
        FileRow {
            id,
            path: path.to_string(),
            mtime_ns: 0,
            size_bytes: 0,
            language: "rust".to_string(),
            line_count: 0,
            pagerank: 0.0,
            indexed_at: 0,
            change_count: 0,
        }
    }

    #[test]
    fn parse_config_round_trips_basic_rules() {
        let text = r#"
[[boundary]]
from = "src/ui/**"
deny = ["src/db/**"]

[[boundary]]
from = "src/domain/**"
deny = ["src/ui/**"]
allow = ["src/shared/**"]
"#;
        let cfg = parse_config(text, Path::new("test.toml")).unwrap();
        assert_eq!(cfg.boundary.len(), 2);
        assert_eq!(cfg.boundary[0].from, "src/ui/**");
        assert_eq!(cfg.boundary[0].deny, vec!["src/db/**".to_string()]);
        assert!(cfg.boundary[0].allow.is_empty());
        assert_eq!(cfg.boundary[1].allow, vec!["src/shared/**".to_string()]);
    }

    #[test]
    fn parse_config_empty_document_returns_zero_rules() {
        let cfg = parse_config("", Path::new("test.toml")).unwrap();
        assert!(cfg.boundary.is_empty());
    }

    #[test]
    fn parse_config_rejects_invalid_glob() {
        let text = r#"
[[boundary]]
from = "src/ui/["
deny = ["src/db/**"]
"#;
        let err = parse_config(text, Path::new("test.toml")).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid"), "message was: {msg}");
    }

    #[test]
    fn check_returns_empty_when_no_rules() {
        let files = vec![file(1, "src/a.rs"), file(2, "src/b.rs")];
        let edges = vec![(1, 2)];
        let cfg = BoundaryConfig::default();
        assert!(check_boundaries(&cfg, &files, &edges).is_empty());
    }

    #[test]
    fn check_flags_deny_edge() {
        let files = vec![file(1, "src/ui/page.rs"), file(2, "src/db/table.rs")];
        let edges = vec![(1, 2)];
        let cfg = BoundaryConfig {
            boundary: vec![BoundaryRule {
                from: "src/ui/**".to_string(),
                deny: vec!["src/db/**".to_string()],
                allow: Vec::new(),
            }],
        };
        let violations = check_boundaries(&cfg, &files, &edges);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].from_file, "src/ui/page.rs");
        assert_eq!(violations[0].to_file, "src/db/table.rs");
        assert_eq!(violations[0].rule_index, 0);
        assert_eq!(violations[0].deny_pattern, "src/db/**");
    }

    #[test]
    fn check_ignores_edges_outside_from_pattern() {
        let files = vec![file(1, "src/util/log.rs"), file(2, "src/db/table.rs")];
        let edges = vec![(1, 2)];
        let cfg = BoundaryConfig {
            boundary: vec![BoundaryRule {
                from: "src/ui/**".to_string(),
                deny: vec!["src/db/**".to_string()],
                allow: Vec::new(),
            }],
        };
        assert!(check_boundaries(&cfg, &files, &edges).is_empty());
    }

    #[test]
    fn check_allow_overrides_deny() {
        let files = vec![
            file(1, "src/domain/order.rs"),
            file(2, "src/ui/widget.rs"),
            file(3, "src/shared/error.rs"),
        ];
        let edges = vec![(1, 2), (1, 3)];
        let cfg = BoundaryConfig {
            boundary: vec![BoundaryRule {
                from: "src/domain/**".to_string(),
                deny: vec!["src/ui/**".to_string(), "src/shared/**".to_string()],
                allow: vec!["src/shared/**".to_string()],
            }],
        };
        let violations = check_boundaries(&cfg, &files, &edges);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].to_file, "src/ui/widget.rs");
    }

    #[test]
    fn check_reports_each_edge_against_first_matching_rule_only() {
        let files = vec![file(1, "src/ui/a.rs"), file(2, "src/db/b.rs")];
        let edges = vec![(1, 2)];
        let cfg = BoundaryConfig {
            boundary: vec![
                BoundaryRule {
                    from: "src/ui/**".to_string(),
                    deny: vec!["src/db/**".to_string()],
                    allow: Vec::new(),
                },
                BoundaryRule {
                    from: "src/**".to_string(),
                    deny: vec!["src/db/**".to_string()],
                    allow: Vec::new(),
                },
            ],
        };
        let violations = check_boundaries(&cfg, &files, &edges);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule_index, 0);
    }

    #[test]
    fn check_is_deterministic() {
        let files = vec![
            file(1, "src/ui/a.rs"),
            file(2, "src/ui/b.rs"),
            file(3, "src/db/x.rs"),
            file(4, "src/db/y.rs"),
        ];
        let edges = vec![(2, 4), (1, 3), (2, 3), (1, 4)];
        let cfg = BoundaryConfig {
            boundary: vec![BoundaryRule {
                from: "src/ui/**".to_string(),
                deny: vec!["src/db/**".to_string()],
                allow: Vec::new(),
            }],
        };
        let a = check_boundaries(&cfg, &files, &edges);
        let b = check_boundaries(&cfg, &files, &edges);
        assert_eq!(a, b);
        assert_eq!(a.len(), 4);
        let sorted: Vec<(&str, &str)> = a
            .iter()
            .map(|v| (v.from_file.as_str(), v.to_file.as_str()))
            .collect();
        assert_eq!(
            sorted,
            vec![
                ("src/ui/a.rs", "src/db/x.rs"),
                ("src/ui/a.rs", "src/db/y.rs"),
                ("src/ui/b.rs", "src/db/x.rs"),
                ("src/ui/b.rs", "src/db/y.rs"),
            ]
        );
    }

    #[test]
    fn suggest_emits_rule_per_cluster_prefix() {
        let files = vec![
            file(1, "src/ui/page.rs"),
            file(2, "src/ui/widget.rs"),
            file(3, "src/db/table.rs"),
            file(4, "src/db/index.rs"),
        ];
        let clusters = vec![(1, 1), (2, 1), (3, 2), (4, 2)];
        let edges: Vec<(i64, i64)> = Vec::new();
        let cfg = suggest_boundaries(&files, &clusters, &edges);
        assert_eq!(cfg.boundary.len(), 2);
        let froms: Vec<&str> = cfg.boundary.iter().map(|r| r.from.as_str()).collect();
        assert!(froms.contains(&"src/ui/**"));
        assert!(froms.contains(&"src/db/**"));
        for rule in &cfg.boundary {
            assert!(!rule.deny.is_empty());
        }
    }

    #[test]
    fn suggest_respects_existing_edges() {
        let files = vec![
            file(1, "src/ui/page.rs"),
            file(2, "src/ui/widget.rs"),
            file(3, "src/domain/order.rs"),
            file(4, "src/domain/cart.rs"),
            file(5, "src/db/table.rs"),
            file(6, "src/db/index.rs"),
        ];
        let clusters = vec![(1, 1), (2, 1), (3, 2), (4, 2), (5, 3), (6, 3)];
        let edges = vec![(1, 3)];
        let cfg = suggest_boundaries(&files, &clusters, &edges);
        let ui_rule = cfg
            .boundary
            .iter()
            .find(|r| r.from == "src/ui/**")
            .expect("ui rule");
        assert!(
            !ui_rule.deny.contains(&"src/domain/**".to_string()),
            "existing edge ui -> domain should not be denied: {:?}",
            ui_rule.deny
        );
        assert!(
            ui_rule.deny.contains(&"src/db/**".to_string()),
            "db should still be denied: {:?}",
            ui_rule.deny
        );
    }

    #[test]
    fn suggest_skips_misc_cluster() {
        let files = vec![
            file(1, "src/ui/a.rs"),
            file(2, "src/ui/b.rs"),
            file(3, "src/junk/lone.rs"),
        ];
        let clusters = vec![(1, 1), (2, 1), (3, MISC_CLUSTER_ID)];
        let cfg = suggest_boundaries(&files, &clusters, &[]);
        // Only one cluster with a clean prefix; no other non-misc
        // cluster to deny, so the rule list is empty.
        assert!(cfg.boundary.is_empty());
    }

    #[test]
    fn render_config_toml_is_parseable() {
        let cfg = BoundaryConfig {
            boundary: vec![
                BoundaryRule {
                    from: "src/ui/**".to_string(),
                    deny: vec!["src/db/**".to_string(), "src/infra/**".to_string()],
                    allow: Vec::new(),
                },
                BoundaryRule {
                    from: "src/domain/**".to_string(),
                    deny: vec!["src/ui/**".to_string()],
                    allow: vec!["src/shared/**".to_string()],
                },
            ],
        };
        let text = render_config_toml(&cfg);
        let roundtrip = parse_config(&text, Path::new("synth.toml")).unwrap();
        assert_eq!(roundtrip.boundary.len(), 2);
        assert_eq!(roundtrip.boundary[0].from, "src/ui/**");
        assert_eq!(
            roundtrip.boundary[1].allow,
            vec!["src/shared/**".to_string()]
        );
    }

    #[test]
    fn load_config_reads_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("boundaries.toml");
        std::fs::write(
            &path,
            "[[boundary]]\nfrom = \"src/a/**\"\ndeny = [\"src/b/**\"]\n",
        )
        .unwrap();
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.boundary.len(), 1);
        assert_eq!(cfg.boundary[0].from, "src/a/**");
    }
}
