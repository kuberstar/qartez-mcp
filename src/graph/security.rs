// Rust guideline compliant 2026-04-15

//! Security vulnerability scanner for indexed codebases.
//!
//! Scans symbol bodies, names, and signatures against a configurable set of
//! rules (built-in OWASP top-10 patterns plus optional TOML overrides).
//! Findings are scored by `severity_weight * file_pagerank * (1 + is_exported)`
//! so vulnerabilities in high-impact, widely-imported files surface first.

use std::collections::HashMap;
use std::path::Path;

use regex::Regex;
use rusqlite::Connection;
use serde::Deserialize;

use crate::storage::models::SymbolRow;
use crate::storage::read;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Severity level for a security finding, ordered from least to most severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    /// Numeric weight used in risk-score computation.
    /// Critical (8) is 8x Low (1) to ensure high-severity findings in
    /// even low-PageRank files outrank low-severity findings in hot files.
    pub fn weight(self) -> f64 {
        match self {
            Self::Low => 1.0,
            Self::Medium => 2.0,
            Self::High => 4.0,
            Self::Critical => 8.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Critical => "Critical",
        }
    }
}

/// How the rule detects a vulnerability.
#[derive(Debug, Clone)]
pub enum SecurityPattern {
    /// Regex applied to the full source text of a symbol body.
    BodyRegex(String),
    /// Regex applied to the symbol name.
    SymbolName(String),
    /// Regex applied to the symbol signature.
    SignatureRegex(String),
}

/// A single security detection rule.
#[derive(Debug, Clone)]
pub struct SecurityRule {
    pub id: String,
    pub name: String,
    pub severity: Severity,
    pub category: String,
    pub pattern: SecurityPattern,
    pub description: String,
    /// When `Some`, the rule only fires for files in these languages.
    /// `None` means all languages.
    pub languages: Option<Vec<String>>,
}

/// One vulnerability finding produced by the scanner.
#[derive(Debug, Clone)]
pub struct Finding {
    pub rule_id: String,
    pub rule_name: String,
    pub severity: Severity,
    pub category: String,
    pub file_path: String,
    pub symbol_name: String,
    pub line_start: u32,
    pub line_end: u32,
    pub pagerank: f64,
    pub risk_score: f64,
    pub snippet: Option<String>,
    pub description: String,
}

/// Options controlling the scan scope and filters.
pub struct ScanOptions {
    pub include_tests: bool,
    pub category_filter: Option<String>,
    pub min_severity: Severity,
    pub file_path_filter: Option<String>,
    pub project_roots: Vec<std::path::PathBuf>,
}

// ---------------------------------------------------------------------------
// Built-in rules (universal OWASP / common-vulnerability patterns)
// ---------------------------------------------------------------------------

/// Returns the 13 built-in detection rules covering secrets, injection,
/// crypto, unsafe code, and information leaks.
pub fn builtin_rules() -> Vec<SecurityRule> {
    vec![
        SecurityRule {
            id: "SEC001".into(),
            name: "hardcoded-secret".into(),
            severity: Severity::Critical,
            category: "secrets".into(),
            pattern: SecurityPattern::BodyRegex(
                r#"(?i)(password|passwd|secret|api_key|token)\s*=\s*"[^"]{4,}""#.into(),
            ),
            description: "Hardcoded password or secret in source code.".into(),
            languages: None,
        },
        SecurityRule {
            id: "SEC002".into(),
            name: "hardcoded-private-key".into(),
            severity: Severity::Critical,
            category: "secrets".into(),
            pattern: SecurityPattern::BodyRegex(
                r"-----BEGIN (RSA |EC |DSA )?PRIVATE KEY-----".into(),
            ),
            description: "Private key embedded in source code.".into(),
            languages: None,
        },
        SecurityRule {
            id: "SEC003".into(),
            name: "sql-injection".into(),
            severity: Severity::High,
            category: "injection".into(),
            pattern: SecurityPattern::BodyRegex(
                r#"(?i)(format!\(|\.format\(|f\"|%).*(?:SELECT|INSERT|UPDATE|DELETE|DROP)"#.into(),
            ),
            description: "SQL query built with string interpolation.".into(),
            languages: None,
        },
        SecurityRule {
            id: "SEC004".into(),
            name: "command-injection".into(),
            severity: Severity::High,
            category: "injection".into(),
            pattern: SecurityPattern::BodyRegex(
                r"(?i)(Command::new|subprocess|os\.system|exec\(|eval\()".into(),
            ),
            description: "Shell command or eval with potential untrusted input.".into(),
            languages: None,
        },
        SecurityRule {
            id: "SEC005".into(),
            name: "path-traversal".into(),
            severity: Severity::Medium,
            category: "injection".into(),
            pattern: SecurityPattern::BodyRegex(r"\.\./".into()),
            description: "Path traversal pattern in file operations.".into(),
            languages: None,
        },
        SecurityRule {
            id: "SEC006".into(),
            name: "insecure-hash".into(),
            severity: Severity::Medium,
            category: "crypto".into(),
            pattern: SecurityPattern::BodyRegex(r"(?i)\b(md5|sha1)\b".into()),
            description: "Weak hash algorithm (MD5 or SHA-1) in use.".into(),
            languages: None,
        },
        SecurityRule {
            id: "SEC007".into(),
            name: "insecure-http".into(),
            severity: Severity::Low,
            category: "crypto".into(),
            pattern: SecurityPattern::BodyRegex(r"http://[a-zA-Z][a-zA-Z0-9.\-]+".into()),
            description: "Insecure HTTP URL (non-localhost).".into(),
            languages: None,
        },
        SecurityRule {
            id: "SEC008".into(),
            name: "unsafe-block".into(),
            severity: Severity::Medium,
            category: "unsafe".into(),
            pattern: SecurityPattern::BodyRegex(r"\bunsafe\b".into()),
            description: "Rust unsafe block requires manual memory safety review.".into(),
            languages: Some(vec!["rust".into()]),
        },
        SecurityRule {
            id: "SEC009".into(),
            name: "unwrap-in-exported".into(),
            severity: Severity::Low,
            category: "unsafe".into(),
            pattern: SecurityPattern::BodyRegex(r"\.unwrap\(\)".into()),
            description: "unwrap() in exported function may panic on unexpected input.".into(),
            languages: Some(vec!["rust".into()]),
        },
        SecurityRule {
            id: "SEC010".into(),
            name: "eval-usage".into(),
            severity: Severity::High,
            category: "injection".into(),
            pattern: SecurityPattern::BodyRegex(r"\beval\(".into()),
            description: "eval() executes arbitrary code.".into(),
            languages: Some(vec![
                "javascript".into(),
                "typescript".into(),
                "python".into(),
            ]),
        },
        SecurityRule {
            id: "SEC011".into(),
            name: "innerHTML-xss".into(),
            severity: Severity::High,
            category: "injection".into(),
            pattern: SecurityPattern::BodyRegex(r"(?i)(innerHTML|dangerouslySetInnerHTML)".into()),
            description: "DOM innerHTML or dangerouslySetInnerHTML enables XSS.".into(),
            languages: Some(vec!["javascript".into(), "typescript".into()]),
        },
        SecurityRule {
            id: "SEC012".into(),
            name: "debug-leak".into(),
            severity: Severity::Low,
            category: "info-leak".into(),
            pattern: SecurityPattern::BodyRegex(r"(dbg!\(|console\.log\(|print!\()".into()),
            description: "Debug logging in exported code may leak information.".into(),
            languages: None,
        },
        SecurityRule {
            id: "SEC013".into(),
            name: "todo-security".into(),
            severity: Severity::Medium,
            category: "review".into(),
            pattern: SecurityPattern::BodyRegex(
                r"(?i)(TODO|FIXME|HACK|XXX).*(security|auth|vuln|inject)".into(),
            ),
            description: "Security-related TODO/FIXME comment needs attention.".into(),
            languages: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// TOML config for custom rules / overrides (.qartez/security.toml)
// ---------------------------------------------------------------------------

/// Parsed form of `.qartez/security.toml`.
#[derive(Debug, Default, Deserialize)]
pub struct SecurityConfig {
    /// Built-in rule IDs to suppress (e.g. `["SEC009", "SEC012"]`).
    #[serde(default)]
    pub disable: Vec<String>,
    /// Additional user-defined rules.
    #[serde(default, rename = "rule")]
    pub rules: Vec<CustomRule>,
}

/// A user-defined rule from the TOML config.
#[derive(Debug, Deserialize)]
pub struct CustomRule {
    pub id: String,
    pub name: String,
    pub severity: String,
    pub category: String,
    /// Regex pattern applied to symbol bodies.
    pub pattern: String,
    pub description: String,
    #[serde(default)]
    pub languages: Option<Vec<String>>,
}

/// Read and parse `.qartez/security.toml`.
pub fn load_custom_config(path: &Path) -> Result<SecurityConfig, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    toml_edit::de::from_str(&text).map_err(|e| format!("Invalid TOML in {}: {e}", path.display()))
}

fn parse_severity(s: &str) -> Result<Severity, String> {
    match s.to_lowercase().as_str() {
        "low" => Ok(Severity::Low),
        "medium" => Ok(Severity::Medium),
        "high" => Ok(Severity::High),
        "critical" => Ok(Severity::Critical),
        other => Err(format!(
            "Unknown severity '{other}', expected: low, medium, high, critical"
        )),
    }
}

/// Remove disabled rules and append custom rules from config.
pub fn apply_config(rules: &mut Vec<SecurityRule>, config: &SecurityConfig) -> Result<(), String> {
    rules.retain(|r| !config.disable.contains(&r.id));

    for cr in &config.rules {
        let severity = parse_severity(&cr.severity)?;
        // Validate the regex upfront so we fail early with a clear message.
        // Cap compiled size to 1 MiB to prevent pathological backtracking
        // from user-supplied patterns in security.toml.
        regex::RegexBuilder::new(&cr.pattern)
            .size_limit(1 << 20)
            .build()
            .map_err(|e| format!("Invalid regex in rule {}: {e}", cr.id))?;
        rules.push(SecurityRule {
            id: cr.id.clone(),
            name: cr.name.clone(),
            severity,
            category: cr.category.clone(),
            pattern: SecurityPattern::BodyRegex(cr.pattern.clone()),
            description: cr.description.clone(),
            languages: cr.languages.clone(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

/// Risk score: `severity_weight * file_pagerank * (1 + is_exported)`.
///
/// Exported symbols double the score because they form the public API
/// surface and are reachable from more call sites.
pub fn compute_risk_score(severity: Severity, pagerank: f64, is_exported: bool) -> f64 {
    severity.weight() * pagerank * (1.0 + f64::from(u8::from(is_exported)))
}

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

/// Returns true for paths that look like test/spec files.
fn is_test_path(path: &str) -> bool {
    path.contains("/tests/")
        || path.contains("/test_")
        || path.contains("_test.")
        || path.contains("/spec/")
        || path.contains("_spec.")
        || path.contains(".test.")
        || path.contains(".spec.")
}

/// Compiled version of a [`SecurityRule`] with its pre-built regex.
struct CompiledRule<'a> {
    rule: &'a SecurityRule,
    regex: Regex,
}

/// Run the security scan against all indexed symbols.
///
/// Reads symbol source from disk (grouped by file for efficiency),
/// matches each rule's pattern, and scores findings by PageRank.
pub fn scan(conn: &Connection, rules: &[SecurityRule], opts: &ScanOptions) -> Vec<Finding> {
    let all_symbols = match read::get_all_symbols_with_path(conn) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let all_files = match read::get_all_files(conn) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let file_pagerank: HashMap<i64, f64> = all_files.iter().map(|f| (f.id, f.pagerank)).collect();
    let file_language: HashMap<i64, &str> = all_files
        .iter()
        .map(|f| (f.id, f.language.as_str()))
        .collect();

    // Pre-compile all regexes; skip rules with invalid patterns.
    let compiled: Vec<CompiledRule<'_>> = rules
        .iter()
        .filter_map(|rule| {
            let pat = match &rule.pattern {
                SecurityPattern::BodyRegex(p)
                | SecurityPattern::SymbolName(p)
                | SecurityPattern::SignatureRegex(p) => p,
            };
            regex::RegexBuilder::new(pat)
                .size_limit(1 << 20)
                .build()
                .ok()
                .map(|regex| CompiledRule { rule, regex })
        })
        .collect();

    // Group symbols by file path so we read each source file at most once.
    let mut by_file: HashMap<&str, Vec<&SymbolRow>> = HashMap::new();
    for (sym, path) in &all_symbols {
        if let Some(ref fp) = opts.file_path_filter
            && !path.contains(fp.as_str())
        {
            continue;
        }
        if !opts.include_tests && is_test_path(path) {
            continue;
        }
        by_file.entry(path.as_str()).or_default().push(sym);
    }

    let mut findings = Vec::new();

    for (rel_path, symbols) in &by_file {
        let abs = resolve_path(rel_path, &opts.project_roots);
        let file_text = match abs.and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(t) => t,
            None => continue,
        };
        let lines: Vec<&str> = file_text.lines().collect();

        for sym in symbols {
            let lang = file_language.get(&sym.file_id).copied().unwrap_or("");
            let pr = file_pagerank.get(&sym.file_id).copied().unwrap_or(0.0);

            let start = (sym.line_start as usize).saturating_sub(1);
            let end = (sym.line_end as usize).min(lines.len());
            if start >= lines.len() || start >= end {
                continue;
            }
            let body = lines[start..end].join("\n");

            for cr in &compiled {
                if let Some(ref langs) = cr.rule.languages
                    && !langs.iter().any(|l| l == lang)
                {
                    continue;
                }
                if let Some(ref cat) = opts.category_filter
                    && cr.rule.category != *cat
                {
                    continue;
                }
                if cr.rule.severity < opts.min_severity {
                    continue;
                }

                // SEC009/SEC012: only flag in exported symbols.
                if (cr.rule.id == "SEC009" || cr.rule.id == "SEC012") && !sym.is_exported {
                    continue;
                }

                let matched = match &cr.rule.pattern {
                    SecurityPattern::BodyRegex(_) => {
                        let body_match = cr.regex.is_match(&body);
                        // SEC007: exclude safe localhost/loopback URLs to
                        // reduce false positives.
                        if body_match && cr.rule.id == "SEC007" {
                            cr.regex.find_iter(&body).any(|m| {
                                let url = m.as_str();
                                !url.starts_with("http://localhost")
                                    && !url.starts_with("http://127.")
                                    && !url.starts_with("http://0.0.0.0")
                                    && !url.starts_with("http://[::1]")
                            })
                        } else {
                            body_match
                        }
                    }
                    SecurityPattern::SymbolName(_) => cr.regex.is_match(&sym.name),
                    SecurityPattern::SignatureRegex(_) => sym
                        .signature
                        .as_ref()
                        .is_some_and(|sig| cr.regex.is_match(sig)),
                };

                if !matched {
                    continue;
                }

                let snippet = if matches!(&cr.rule.pattern, SecurityPattern::BodyRegex(_)) {
                    body.lines().find(|line| cr.regex.is_match(line)).map(|l| {
                        let trimmed = l.trim();
                        if trimmed.len() > 120 {
                            // Truncate by char count to avoid panicking on multi-byte UTF-8.
                            format!("{}...", trimmed.chars().take(117).collect::<String>())
                        } else {
                            trimmed.to_string()
                        }
                    })
                } else {
                    None
                };

                findings.push(Finding {
                    rule_id: cr.rule.id.clone(),
                    rule_name: cr.rule.name.clone(),
                    severity: cr.rule.severity,
                    category: cr.rule.category.clone(),
                    file_path: (*rel_path).to_string(),
                    symbol_name: sym.name.clone(),
                    line_start: sym.line_start,
                    line_end: sym.line_end,
                    pagerank: pr,
                    risk_score: compute_risk_score(cr.rule.severity, pr, sym.is_exported),
                    snippet,
                    description: cr.rule.description.clone(),
                });
            }
        }
    }

    findings.sort_by(|a, b| {
        b.risk_score
            .partial_cmp(&a.risk_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    findings
}

/// Resolve a relative index path to an absolute path using the project roots.
///
/// Follows the same multi-root prefix resolution as
/// `rebuild_symbol_bodies_multi`: if the first path component matches a
/// known root directory name, join the remainder with that root.
fn resolve_path(rel_path: &str, roots: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    if roots.len() > 1 {
        let p = std::path::Path::new(rel_path);
        if let Some(std::path::Component::Normal(first)) = p.components().next() {
            let first_str = first.to_string_lossy();
            for root in roots {
                if let Some(name) = root.file_name()
                    && name.to_string_lossy() == *first_str
                {
                    let remainder: std::path::PathBuf = p.components().skip(1).collect();
                    let abs = root.join(remainder);
                    if abs.exists() {
                        return Some(abs);
                    }
                }
            }
        }
    }
    roots.first().map(|r| r.join(rel_path))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn check_regex(pattern: &str, input: &str, should_match: bool) {
        let re = Regex::new(pattern).expect("valid regex");
        assert_eq!(
            re.is_match(input),
            should_match,
            "pattern={pattern} input={input} expected={should_match}",
        );
    }

    fn pattern_str(rule: &SecurityRule) -> &str {
        match &rule.pattern {
            SecurityPattern::BodyRegex(p)
            | SecurityPattern::SymbolName(p)
            | SecurityPattern::SignatureRegex(p) => p,
        }
    }

    #[test]
    fn sec001_hardcoded_secret() {
        let rules = builtin_rules();
        let pat = pattern_str(rules.iter().find(|r| r.id == "SEC001").unwrap());
        check_regex(pat, r#"let password = "hunter2";"#, true);
        check_regex(pat, r#"let API_KEY = "abc123def";"#, true);
        check_regex(pat, r#"let password = "";"#, false);
        check_regex(pat, r#"let name = "Alice";"#, false);
    }

    #[test]
    fn sec002_private_key() {
        let rules = builtin_rules();
        let pat = pattern_str(rules.iter().find(|r| r.id == "SEC002").unwrap());
        check_regex(pat, "-----BEGIN RSA PRIVATE KEY-----", true);
        check_regex(pat, "-----BEGIN PRIVATE KEY-----", true);
        check_regex(pat, "-----BEGIN EC PRIVATE KEY-----", true);
        check_regex(pat, "-----BEGIN PUBLIC KEY-----", false);
    }

    #[test]
    fn sec003_sql_injection() {
        let rules = builtin_rules();
        let pat = pattern_str(rules.iter().find(|r| r.id == "SEC003").unwrap());
        check_regex(
            pat,
            r#"format!("SELECT * FROM users WHERE id = {}", id)"#,
            true,
        );
        check_regex(pat, r#"format!("DELETE FROM temp WHERE id = {}", x)"#, true);
    }

    #[test]
    fn sec007_insecure_http() {
        let rules = builtin_rules();
        let pat = pattern_str(rules.iter().find(|r| r.id == "SEC007").unwrap());
        check_regex(pat, "http://example.com/api", true);
        check_regex(pat, "https://example.com/api", false);
        // The regex itself matches localhost/127.x URLs, but the scanner's
        // post-filter (SEC007) excludes them at scan time.
        check_regex(pat, "http://localhost:3000", true);
    }

    #[test]
    fn sec008_rust_only() {
        let rules = builtin_rules();
        let rule = rules.iter().find(|r| r.id == "SEC008").unwrap();
        assert!(
            rule.languages
                .as_ref()
                .unwrap()
                .contains(&"rust".to_string())
        );
    }

    #[test]
    fn sec010_eval_languages() {
        let rules = builtin_rules();
        let rule = rules.iter().find(|r| r.id == "SEC010").unwrap();
        let langs = rule.languages.as_ref().unwrap();
        assert!(langs.contains(&"javascript".to_string()));
        assert!(langs.contains(&"python".to_string()));
        assert!(!langs.contains(&"rust".to_string()));
    }

    #[test]
    fn risk_score_exported_doubles() {
        let priv_score = compute_risk_score(Severity::High, 0.5, false);
        let pub_score = compute_risk_score(Severity::High, 0.5, true);
        assert!((pub_score - priv_score * 2.0).abs() < 1e-10);
    }

    #[test]
    fn risk_score_critical_beats_low() {
        let crit = compute_risk_score(Severity::Critical, 0.5, false);
        let low = compute_risk_score(Severity::Low, 0.5, false);
        assert!(crit > low);
    }

    #[test]
    fn test_path_detection() {
        assert!(is_test_path("src/tests/foo.rs"));
        assert!(is_test_path("src/test_helper.py"));
        assert!(is_test_path("foo_test.go"));
        assert!(is_test_path("foo.test.ts"));
        assert!(is_test_path("foo.spec.js"));
        assert!(!is_test_path("src/server/mod.rs"));
    }

    #[test]
    fn severity_ordering() {
        assert!(Severity::Critical > Severity::High);
        assert!(Severity::High > Severity::Medium);
        assert!(Severity::Medium > Severity::Low);
    }

    #[test]
    fn parse_toml_config() {
        let toml_str = r#"
disable = ["SEC009", "SEC012"]

[[rule]]
id = "CUSTOM001"
name = "aws-key"
severity = "critical"
category = "secrets"
pattern = "AKIA[0-9A-Z]{16}"
description = "AWS access key"
"#;
        let config: SecurityConfig = toml_edit::de::from_str(toml_str).unwrap();
        assert_eq!(config.disable, vec!["SEC009", "SEC012"]);
        assert_eq!(config.rules.len(), 1);
        assert_eq!(config.rules[0].id, "CUSTOM001");
    }

    #[test]
    fn apply_config_disables_and_adds() {
        let mut rules = builtin_rules();
        let initial = rules.len();
        let config = SecurityConfig {
            disable: vec!["SEC009".into(), "SEC012".into()],
            rules: vec![CustomRule {
                id: "CUSTOM001".into(),
                name: "test".into(),
                severity: "high".into(),
                category: "test".into(),
                pattern: "foobar".into(),
                description: "test rule".into(),
                languages: None,
            }],
        };
        apply_config(&mut rules, &config).unwrap();
        // Removed 2, added 1.
        assert_eq!(rules.len(), initial - 2 + 1);
        assert!(!rules.iter().any(|r| r.id == "SEC009"));
        assert!(rules.iter().any(|r| r.id == "CUSTOM001"));
    }

    #[test]
    fn apply_config_rejects_invalid_regex() {
        let mut rules = builtin_rules();
        let config = SecurityConfig {
            disable: vec![],
            rules: vec![CustomRule {
                id: "BAD".into(),
                name: "bad".into(),
                severity: "high".into(),
                category: "test".into(),
                pattern: "[invalid".into(),
                description: "bad regex".into(),
                languages: None,
            }],
        };
        assert!(apply_config(&mut rules, &config).is_err());
    }

    #[test]
    fn all_builtin_regexes_compile() {
        for rule in builtin_rules() {
            let pat = pattern_str(&rule);
            assert!(
                Regex::new(pat).is_ok(),
                "Rule {} has invalid regex: {pat}",
                rule.id
            );
        }
    }
}
