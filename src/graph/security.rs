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
    /// Map of canonical root path to the alias the user configured for it
    /// in `workspace.toml`. Without this, aliased roots in a multi-root
    /// project resolve to the wrong on-disk path when the first component
    /// of the indexed relative path is the alias rather than the directory
    /// name. Defaults to empty for callers that have no aliases.
    pub root_aliases: HashMap<std::path::PathBuf, String>,
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
            // Require both a string-formatter token AND a recognizable SQL
            // statement opener inside the formatted literal. The earlier
            // `(SELECT|...|DROP)` tail matched any case-insensitive substring
            // (`drop-shadow`, `Settings updated`, `selector:...`), drowning real
            // findings in CSS, log strings, and HashMap key formatting. The
            // new tail requires SQL syntax that does not appear in plain
            // English: `SELECT *|DISTINCT|TOP|<col>`, `INSERT INTO`,
            // `UPDATE x SET`, `DELETE FROM`, `DROP TABLE|INDEX|VIEW|...`.
            pattern: SecurityPattern::BodyRegex(
                r#"(?i)(?:format!\(|\.format\(|f")[^\n]*?\b(?:SELECT\s+(?:\*|DISTINCT|TOP|[`"\[]?\w)|INSERT\s+INTO\b|UPDATE\s+\w+\s+SET\b|DELETE\s+FROM\b|DROP\s+(?:TABLE|INDEX|VIEW|DATABASE|SCHEMA)\b)"#.into(),
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
///
/// A virtual leading `/` is prepended before substring matching so a
/// top-level `tests/foo.rs` is recognised the same way as `src/tests/foo.rs`.
/// Without that, the directory-prefixed patterns (`/tests/`, `/test_`,
/// `/spec/`) would only fire when the test directory had a parent.
fn is_test_path(path: &str) -> bool {
    let normalized = format!("/{}", path.trim_start_matches('/'));
    normalized.contains("/tests/")
        || normalized.contains("/test_")
        || normalized.contains("_test.")
        || normalized.contains("/spec/")
        || normalized.contains("_spec.")
        || normalized.contains(".test.")
        || normalized.contains(".spec.")
}

/// Find Rust `#[cfg(test)]` module blocks inside a source file. Returns
/// inclusive 1-based line ranges spanning each block, including the
/// attribute line itself.
///
/// Used to suppress security findings produced inside inline test modules
/// (e.g. `#[cfg(test)] mod tests { ... }` in production source files) when
/// the caller asked to skip tests but `is_test_path` did not match because
/// the host file lives outside the conventional test directories.
///
/// Parses with `tree-sitter-rust` so the result is robust against
/// multi-line strings, raw strings, byte strings, block comments, and
/// every other construct that defeats line-based brace counting. If the
/// parser fails to load (should not happen — the language is statically
/// linked) the function returns an empty vector, which means findings
/// inside test modules continue to surface as before; it never hides
/// findings outside test modules.
fn find_cfg_test_blocks(source: &str) -> Vec<(u32, u32)> {
    use tree_sitter::{Language, Parser};

    let mut parser = Parser::new();
    if parser
        .set_language(&Language::new(tree_sitter_rust::LANGUAGE))
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let bytes = source.as_bytes();
    let mut ranges = Vec::new();
    collect_cfg_test_mod_ranges(tree.root_node(), bytes, &mut ranges);
    ranges
}

/// Walk the AST collecting `(start_line, end_line)` 1-based ranges for
/// every `mod_item` whose preceding sibling chain includes an
/// `attribute_item` matching `#[cfg(test)]` or `#[cfg(any(test, ...))]`.
/// Recurses into non-matching children so nested test modules are found.
fn collect_cfg_test_mod_ranges(
    node: tree_sitter::Node<'_>,
    bytes: &[u8],
    ranges: &mut Vec<(u32, u32)>,
) {
    if node.kind() == "mod_item"
        && let Some(attr_row) = preceding_cfg_test_attr_row(node, bytes)
    {
        let start = (attr_row + 1) as u32;
        let end = (node.end_position().row + 1) as u32;
        ranges.push((start, end));
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_cfg_test_mod_ranges(child, bytes, ranges);
    }
}

/// Walk back through prior siblings of `node`, skipping comments and
/// non-cfg-test attributes. Return the topmost row that belongs to a
/// `cfg(test)` attribute when one exists.
fn preceding_cfg_test_attr_row(node: tree_sitter::Node<'_>, bytes: &[u8]) -> Option<usize> {
    let mut found_row = None;
    let mut sib = node.prev_sibling();
    while let Some(s) = sib {
        match s.kind() {
            "attribute_item" | "inner_attribute_item" => {
                let text = std::str::from_utf8(&bytes[s.byte_range()]).unwrap_or("");
                if attr_text_targets_test(text) {
                    found_row = Some(s.start_position().row);
                }
                sib = s.prev_sibling();
            }
            "line_comment" | "block_comment" => {
                sib = s.prev_sibling();
            }
            _ => break,
        }
    }
    found_row
}

/// True when an attribute textual form names `test` as a cfg target.
/// Covers `#[cfg(test)]`, `#[cfg(any(test, ...))]`, and the same forms
/// nested inside `cfg_attr` predicates.
fn attr_text_targets_test(text: &str) -> bool {
    let stripped: String = text.split_whitespace().collect();
    stripped.contains("cfg(test)")
        || stripped.contains("cfg(any(test")
        || stripped.contains(",test)")
        || stripped.contains(",test,")
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
        // Skip the file that defines the built-in detection rules: it has to
        // mention every regex literal (`Command::new`, `format!(`, `MD5`,
        // `unsafe`, `TODO security`, etc.) and would otherwise self-match
        // every body-regex rule, drowning the report in noise.
        if is_security_rule_definition_path(path) {
            continue;
        }
        by_file.entry(path.as_str()).or_default().push(sym);
    }

    let mut findings = Vec::new();

    for (rel_path, symbols) in &by_file {
        let abs = resolve_path(rel_path, &opts.project_roots, &opts.root_aliases);
        let file_text = match abs.and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(t) => t,
            None => continue,
        };
        let lines: Vec<&str> = file_text.lines().collect();

        // For Rust source files, locate inline `#[cfg(test)]` modules so
        // their symbols can be skipped when tests are excluded. The path
        // check (`is_test_path`) only catches files under conventional
        // test directories; inline test modules in production files
        // (e.g. `src/foo.rs` with `#[cfg(test)] mod tests {}`) need this.
        let file_lang = symbols
            .first()
            .map(|s| file_language.get(&s.file_id).copied().unwrap_or(""))
            .unwrap_or("");
        let cfg_test_ranges: Vec<(u32, u32)> = if !opts.include_tests && file_lang == "rust" {
            find_cfg_test_blocks(&file_text)
        } else {
            Vec::new()
        };

        for sym in symbols {
            if !cfg_test_ranges.is_empty()
                && cfg_test_ranges
                    .iter()
                    .any(|(s, e)| sym.line_start >= *s && sym.line_end <= *e)
            {
                continue;
            }

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
                        if body_match && cr.rule.id == "SEC007" {
                            cr.regex
                                .find_iter(&body)
                                .any(|m| !is_sec007_benign(m.as_str(), &body, m.start()))
                        } else if body_match && cr.rule.id == "SEC001" {
                            cr.regex
                                .find_iter(&body)
                                .any(|m| !is_sec001_env_indirection(m.as_str()))
                        } else if body_match && cr.rule.id == "SEC004" {
                            cr.regex
                                .find_iter(&body)
                                .any(|m| !is_sec004_static_command(m.as_str(), &body, m.start()))
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

/// SEC007 allowlist: returns true when an `http://` match should NOT be
/// reported. Covers loopback literals, single-label internal hostnames
/// (Docker/K8s service names, nginx upstreams), and XML-namespace URIs
/// that are identifiers rather than URLs ever fetched over the network.
fn is_sec007_benign(url: &str, body: &str, match_start: usize) -> bool {
    if url.starts_with("http://localhost")
        || url.starts_with("http://127.")
        || url.starts_with("http://0.0.0.0")
        || url.starts_with("http://[::1]")
    {
        return true;
    }
    // Single-label hostname: no dot in the host means it cannot resolve
    // on public DNS, so it's an internal service name (Docker Compose,
    // K8s service, nginx upstream). The SEC007 regex stops at `/`, `:`,
    // or `_`, so the remainder after `http://` is the bare host.
    let host = &url["http://".len()..];
    if !host.contains('.') {
        return true;
    }
    // Well-known XML namespace: `http://www.w3.org/*` identifies SVG,
    // XHTML, XSL, MathML etc. and is never fetched.
    if url.starts_with("http://www.w3.org") {
        return true;
    }
    // Any URL appearing inside an `xmlns=` or `xmlns:foo=` attribute
    // is a namespace identifier, not a network URL. Walk forward to the
    // nearest UTF-8 char boundary so non-ASCII bytes in the preceding
    // text (common in CSS/HTML) can't panic the slice.
    let mut prefix_start = match_start.saturating_sub(32);
    while prefix_start < match_start && !body.is_char_boundary(prefix_start) {
        prefix_start += 1;
    }
    let prefix = &body[prefix_start..match_start];
    if prefix.contains("xmlns=") || prefix.contains("xmlns:") {
        return true;
    }
    false
}

/// Returns `true` for the file that defines this scanner's built-in rules.
///
/// Every body-regex pattern is materialized as a string literal in
/// `builtin_rules()` here, so a body scan of this file would surface a
/// match for SEC001 (the `password|token` literal), SEC003 (`SELECT...DROP`),
/// SEC004 (`Command::new`), SEC006 (`md5|sha1`), SEC008 (`unsafe`), and
/// SEC013 (`TODO.*security`). None of those are real findings, they are
/// the rule definitions themselves. Skip the whole file rather than per-rule
/// to keep the exemption obvious from one place.
fn is_security_rule_definition_path(path: &str) -> bool {
    path.ends_with("graph/security.rs") || path.ends_with("graph\\security.rs")
}

/// SEC001 allowlist: returns `true` when the matched `name="value"` snippet
/// is just an environment-variable indirection rather than a hardcoded
/// secret. Catches shell `TOKEN="$GITHUB_TOKEN"`, Bash `${VAR}`, JS
/// `process.env.X`, Python `os.environ['X']`, and YAML `${{ secrets.X }}`.
fn is_sec001_env_indirection(snippet: &str) -> bool {
    let value = match snippet.split_once('=') {
        Some((_, after)) => after.trim().trim_matches(|c| c == '"' || c == '\''),
        None => return false,
    };
    let v = value.trim();
    if v.is_empty() {
        return false;
    }
    if v.starts_with('$') || v.starts_with("${") {
        return true;
    }
    if v.contains("process.env")
        || v.contains("os.environ")
        || v.contains("System.getenv")
        || v.contains("env::var")
        || v.contains("std::env::var")
        || v.contains("ENV[")
        || v.contains("getenv(")
        || v.contains("${{") && v.contains("secrets.")
    {
        return true;
    }
    false
}

/// SEC004 allowlist: returns `true` when the match is a `Command::new("LIT")`
/// invocation whose executable is a string literal AND the surrounding
/// `.args(...)` chain contains no `format!` / `&` interpolation. Static
/// commands like `Command::new("git").args(["rev-parse", "HEAD"])` cannot
/// inject arbitrary shells; only interpolated args are dangerous.
fn is_sec004_static_command(matched: &str, body: &str, match_start: usize) -> bool {
    if !matched.starts_with("Command::new") {
        return false;
    }
    let after = &body[match_start + matched.len()..];
    let exec_lit = match after.split_once(')') {
        Some((inside, _)) => inside.trim_start_matches('('),
        None => return false,
    };
    let exec = exec_lit.trim();
    let is_string_literal = exec.len() >= 2
        && (exec.starts_with('"') && exec.ends_with('"'))
        && !exec[1..exec.len() - 1].contains('{');
    if !is_string_literal {
        return false;
    }
    // Inspect the next ~512 bytes of method chain (typical builder length).
    // If the chain interpolates via `format!`, raw `&str` concat, or `+`,
    // treat it as dynamic. The 512-byte window is large enough to cover
    // the longest builder call we have in-tree (~300 bytes) but small
    // enough that a downstream `format!(` in unrelated code does not
    // poison the verdict.
    let tail_end = (after.len()).min(512);
    let tail = &after[..tail_end];
    if tail.contains("format!(")
        || tail.contains("&format!")
        || tail.contains(".to_string()")
        || tail.contains("String::from(")
    {
        return false;
    }
    true
}

/// Resolve a relative index path to an absolute path using the project roots.
///
/// Follows the same multi-root prefix resolution as
/// `rebuild_symbol_bodies_multi`: if the first path component matches a
/// known root alias or directory name, join the remainder with that root.
fn resolve_path(
    rel_path: &str,
    roots: &[std::path::PathBuf],
    aliases: &HashMap<std::path::PathBuf, String>,
) -> Option<std::path::PathBuf> {
    if roots.len() > 1 {
        let p = std::path::Path::new(rel_path);
        if let Some(std::path::Component::Normal(first)) = p.components().next() {
            let first_str = first.to_string_lossy();
            for root in roots {
                let matches = match aliases.get(root) {
                    // User-configured alias wins when present - the index stored
                    // paths as `<alias>/sub/file.rs`, not `<dir_name>/sub/...`.
                    Some(alias) => alias.as_str() == first_str.as_ref(),
                    None => root
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy() == first_str),
                };
                if matches {
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
mod resolve_path_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn resolves_single_root_without_prefix() {
        let roots = vec![PathBuf::from("/tmp/a")];
        let aliases = HashMap::new();
        let abs = resolve_path("src/lib.rs", &roots, &aliases);
        assert_eq!(abs, Some(PathBuf::from("/tmp/a/src/lib.rs")));
    }

    #[test]
    fn resolves_aliased_root_by_alias_name() {
        // The regression: the DB stored the file as `MyAlpha/src/lib.rs`
        // because the user configured an alias in workspace.toml. The old
        // `resolve_path` only matched `root.file_name()` (= "a"), so with
        // a non-matching first component it fell through to `roots[0]`
        // which pointed at the wrong directory.
        let mut tmp_a = std::env::temp_dir();
        tmp_a.push("qartez_resolve_a");
        let mut tmp_b = std::env::temp_dir();
        tmp_b.push("qartez_resolve_b");
        std::fs::create_dir_all(tmp_a.join("src")).unwrap();
        std::fs::create_dir_all(tmp_b.join("src")).unwrap();
        std::fs::write(tmp_b.join("src/hit.rs"), "fn hit() {}").unwrap();

        let roots = vec![tmp_a.clone(), tmp_b.clone()];
        let mut aliases = HashMap::new();
        aliases.insert(tmp_b.clone(), "MyAlpha".to_string());

        let resolved = resolve_path("MyAlpha/src/hit.rs", &roots, &aliases);
        assert_eq!(
            resolved,
            Some(tmp_b.join("src/hit.rs")),
            "aliased prefix must resolve against its mapped root"
        );

        // Clean up.
        let _ = std::fs::remove_dir_all(&tmp_a);
        let _ = std::fs::remove_dir_all(&tmp_b);
    }

    #[test]
    fn falls_back_to_first_root_when_prefix_unknown() {
        let roots = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")];
        let aliases = HashMap::new();
        let abs = resolve_path("unknown/src/lib.rs", &roots, &aliases);
        assert_eq!(abs, Some(PathBuf::from("/tmp/a/unknown/src/lib.rs")));
    }
}

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
        check_regex(pat, r#"format!("UPDATE users SET name = {}", n)"#, true);
        check_regex(pat, r#"format!("INSERT INTO users VALUES ({})", v)"#, true);
        check_regex(pat, r#"format!("DROP TABLE foo")"#, true);
    }

    #[test]
    fn sec003_does_not_flag_log_messages_or_css() {
        // The old regex matched any case-insensitive `update`/`select`/`drop`
        // substring, flagging `Settings updated`, `selector:{key}`, and CSS
        // `drop-shadow`. The new regex requires SQL syntax tokens.
        let rules = builtin_rules();
        let pat = pattern_str(rules.iter().find(|r| r.id == "SEC003").unwrap());
        check_regex(pat, r#"format!("Settings updated: {}", path)"#, false);
        check_regex(pat, r#"format!("selector:{key}={val}")"#, false);
        check_regex(
            pat,
            r#"format!("Tool list updated. {} tools", count)"#,
            false,
        );
        check_regex(
            pat,
            "0%, 100% { filter: drop-shadow(0 0 8px var(--glow)); }",
            false,
        );
        check_regex(
            pat,
            r#"format!("Qartez snippet updated in {}", target)"#,
            false,
        );
    }

    #[test]
    fn sec001_skips_env_indirection() {
        // The hardcoded-secret rule must not fire on shell/JS/Python
        // env-variable indirections - those fetch the secret at runtime,
        // they do not embed it in source.
        assert!(is_sec001_env_indirection(r#"GH_TOKEN="$GITHUB_TOKEN""#));
        assert!(is_sec001_env_indirection(r#"token="${env.GITHUB_TOKEN}""#));
        assert!(is_sec001_env_indirection(
            r#"api_key="process.env.OPENAI_KEY""#
        ));
        assert!(is_sec001_env_indirection(
            r#"password="os.environ['DB_PASS']""#
        ));
        // Real hardcoded secrets are still flagged.
        assert!(!is_sec001_env_indirection(r#"password="hunter2""#));
        assert!(!is_sec001_env_indirection(r#"api_key="sk-abc123def""#));
    }

    #[test]
    fn sec004_skips_static_command() {
        // Static `Command::new("git").args([...static literals...])` is safe.
        let body =
            r#"std::process::Command::new("git").args(["rev-parse", "--short", "HEAD"]).output()"#;
        let m = "Command::new";
        let start = body.find(m).unwrap();
        assert!(is_sec004_static_command(m, body, start));

        // Dynamic command construction via format! must still be flagged.
        let body = r#"Command::new("sh").arg(format!("echo {}", user_input)).output()"#;
        let start = body.find(m).unwrap();
        assert!(!is_sec004_static_command(m, body, start));

        // Variable executable (not a literal) is still suspicious.
        let body = r#"Command::new(&cmd[0]).args(args).output()"#;
        let start = body.find(m).unwrap();
        assert!(!is_sec004_static_command(m, body, start));
    }

    #[test]
    fn rule_definition_file_is_exempt() {
        // The file containing the rule literals would otherwise self-match
        // every body-regex rule (it has `Command::new`, `password`, `MD5`,
        // `unsafe`, `TODO security`, ...). The exemption skips it wholesale.
        assert!(is_security_rule_definition_path(
            "qartez-public/src/graph/security.rs"
        ));
        assert!(is_security_rule_definition_path(
            r"qartez-public\src\graph\security.rs"
        ));
        assert!(!is_security_rule_definition_path(
            "qartez-public/src/graph/boundaries.rs"
        ));
    }

    #[test]
    fn rule_exemption_does_not_match_other_security_files() {
        // A `*security*.rs` file in another module must still be scanned.
        // Only the canonical path `graph/security.rs` is exempt.
        assert!(!is_security_rule_definition_path(
            "src/server/tools/security.rs"
        ));
        assert!(!is_security_rule_definition_path("src/security.rs"));
        assert!(!is_security_rule_definition_path("graph/security_test.rs"));
        assert!(!is_security_rule_definition_path("user/code.rs"));
    }

    #[test]
    fn sec001_env_indirection_covers_more_languages() {
        // System.getenv (Java)
        assert!(is_sec001_env_indirection(
            r#"token="System.getenv(\"GH_TOKEN\")""#
        ));
        // env::var (Rust)
        assert!(is_sec001_env_indirection(
            r#"secret="env::var(\"SECRET\").unwrap()""#
        ));
        // std::env::var (Rust full path)
        assert!(is_sec001_env_indirection(
            r#"api_key="std::env::var(\"API_KEY\").ok()""#
        ));
        // Ruby ENV[]
        assert!(is_sec001_env_indirection(r#"token="ENV[\"GH_TOKEN\"]""#));
        // C getenv()
        assert!(is_sec001_env_indirection(r#"token="getenv(\"TOKEN\")""#));
        // GitHub Actions secrets context
        assert!(is_sec001_env_indirection(
            r#"token="${{ secrets.GH_TOKEN }}""#
        ));
    }

    #[test]
    fn sec001_does_not_skip_secrets_with_dollar_signs() {
        // A real password that happens to contain `$` mid-string must
        // still be flagged. Only LEADING `$`/`${` indicates an env ref.
        assert!(!is_sec001_env_indirection(r#"password="hunter$2""#));
        assert!(!is_sec001_env_indirection(r#"api_key="sk-$abc-123""#));
    }

    #[test]
    fn sec001_handles_single_quoted_values() {
        // Bash/JS single-quoted string: `token='$GITHUB_TOKEN'` must
        // still register as env indirection (the helper trims both
        // quote styles).
        assert!(is_sec001_env_indirection(r#"token='$GITHUB_TOKEN'"#));
        assert!(is_sec001_env_indirection(
            r#"api_key='process.env.OPENAI_KEY'"#
        ));
    }

    #[test]
    fn sec001_handles_no_equals_sign() {
        // The hardcoded-secret regex always captures `name = "value"`,
        // but a defensive helper should not panic if fed something
        // unusual. No `=` → cannot parse → not env indirection.
        assert!(!is_sec001_env_indirection("just some text"));
        assert!(!is_sec001_env_indirection(""));
    }

    #[test]
    fn sec004_static_command_handles_string_from() {
        // `String::from(format!(...))` and `arg.to_string()` are also
        // dynamic and must NOT be skipped.
        let m = "Command::new";
        let body = r#"Command::new("sh").arg(String::from(user_input)).output()"#;
        let start = body.find(m).unwrap();
        assert!(!is_sec004_static_command(m, body, start));

        let body = r#"Command::new("sh").arg(user_input.to_string()).output()"#;
        let start = body.find(m).unwrap();
        assert!(!is_sec004_static_command(m, body, start));

        let body = r#"Command::new("git").arg(&format!("--{flag}", flag = f)).output()"#;
        let start = body.find(m).unwrap();
        assert!(!is_sec004_static_command(m, body, start));
    }

    #[test]
    fn sec004_static_command_handles_curly_brace_in_literal() {
        // `Command::new("script-{version}")` interpolation in the exec
        // name itself is dynamic; must NOT be skipped.
        let m = "Command::new";
        let body = r#"Command::new("script-{version}").output()"#;
        let start = body.find(m).unwrap();
        assert!(!is_sec004_static_command(m, body, start));
    }

    #[test]
    fn sec004_static_command_long_chain_within_window() {
        // Builder chain inside the 512-byte inspection window: even a
        // long chain stays static if no interpolation is present.
        let m = "Command::new";
        let body = r#"Command::new("cargo").args(["build", "--release", "--all-features", "--target=x86_64-apple-darwin", "--manifest-path", "Cargo.toml"]).output()"#;
        let start = body.find(m).unwrap();
        assert!(is_sec004_static_command(m, body, start));
    }

    #[test]
    fn sec004_static_command_unrelated_format_outside_window_is_safe() {
        // A `format!(` further than 512 bytes after the Command::new
        // belongs to unrelated code and must not poison the verdict.
        let m = "Command::new";
        let padding = " ".repeat(600);
        let body = format!(
            r#"Command::new("git").arg("status").output();{padding}let s = format!("unrelated {{}}", x);"#
        );
        let start = body.find(m).unwrap();
        assert!(is_sec004_static_command(m, &body, start));
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
    fn sec007_benign_loopback() {
        // Loopback literals are filtered by the scan-time allowlist.
        let body = "let u = \"http://localhost:3000\";";
        let url = "http://localhost";
        let start = body.find(url).unwrap();
        assert!(is_sec007_benign(url, body, start));

        let body = "curl http://127.0.0.1";
        let url = "http://127.0.0.1";
        // The regex only captures up to the first digit-only segment, but
        // the literal prefix test covers the whole family.
        assert!(is_sec007_benign(url, body, body.find(url).unwrap()));
    }

    #[test]
    fn sec007_benign_single_label_host() {
        // Docker/K8s/nginx-upstream-style internal hostnames never
        // resolve publicly, so flagging them is a false positive.
        let body = "proxy_pass http://backend;";
        let url = "http://backend";
        let start = body.find(url).unwrap();
        assert!(is_sec007_benign(url, body, start));

        let body = "upstream: http://redis";
        let url = "http://redis";
        assert!(is_sec007_benign(url, body, body.find(url).unwrap()));
    }

    #[test]
    fn sec007_benign_w3c_namespace() {
        // W3C namespace IRIs are identifiers, not URLs ever fetched.
        let body = "<svg xmlns='http://www.w3.org/2000/svg'>";
        let url = "http://www.w3.org";
        let start = body.find(url).unwrap();
        assert!(is_sec007_benign(url, body, start));
    }

    #[test]
    fn sec007_benign_xmlns_context() {
        // Any `xmlns=`/`xmlns:foo=` context marks the URL as a namespace.
        let body = r#"<root xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">"#;
        let url = "http://schemas.xmlsoap.org";
        let start = body.find(url).unwrap();
        assert!(is_sec007_benign(url, body, start));
    }

    #[test]
    fn sec007_utf8_boundary_safe() {
        // Multi-byte chars in the preceding text must not panic the
        // char-boundary-snapped prefix slice.
        let body = "описание: fetch(\"http://example.com/api\")";
        let url = "http://example.com";
        let start = body.find(url).unwrap();
        assert!(!is_sec007_benign(url, body, start));
    }

    #[test]
    fn sec007_flags_real_external_http() {
        // Real external plaintext URLs must still be flagged.
        let body = "fetch(\"http://example.com/api\")";
        let url = "http://example.com";
        let start = body.find(url).unwrap();
        assert!(!is_sec007_benign(url, body, start));

        let body = "const api = \"http://api.vendor.io/v1\";";
        let url = "http://api.vendor.io";
        assert!(!is_sec007_benign(url, body, body.find(url).unwrap()));
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
        // With preceding directory.
        assert!(is_test_path("src/tests/foo.rs"));
        assert!(is_test_path("src/test_helper.py"));
        assert!(is_test_path("src/spec/foo.rb"));
        // Top-level test directories or test-prefixed files (this is the
        // CLI-from-project-root case that used to slip through).
        assert!(is_test_path("tests/foo.rs"));
        assert!(is_test_path("test_helper.py"));
        assert!(is_test_path("spec/foo.rb"));
        // Leading slash should be normalised away, not double-counted.
        assert!(is_test_path("/tests/foo.rs"));
        // Filename-suffix patterns work anywhere.
        assert!(is_test_path("foo_test.go"));
        assert!(is_test_path("foo.test.ts"));
        assert!(is_test_path("foo.spec.js"));
        // Negative cases.
        assert!(!is_test_path("src/server/mod.rs"));
        assert!(!is_test_path("attests/foo.rs"));
        assert!(!is_test_path("respec/foo.rb"));
        assert!(!is_test_path(""));
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

    #[test]
    fn cfg_test_block_basic() {
        let src = "fn foo() {}\n\
                   \n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                       #[test]\n\
                       fn t() {\n\
                           let p = \"../etc/passwd\";\n\
                       }\n\
                   }\n\
                   \n\
                   fn bar() {}\n";
        let blocks = find_cfg_test_blocks(src);
        assert_eq!(blocks.len(), 1);
        let (s, e) = blocks[0];
        assert_eq!(s, 3, "block starts at #[cfg(test)] line");
        assert_eq!(e, 9, "block ends at closing brace of mod tests");
    }

    #[test]
    fn cfg_test_block_named_module() {
        let src = "#[cfg(test)]\n\
                   mod safe_resolve_tests {\n\
                       fn helper() {}\n\
                   }\n";
        let blocks = find_cfg_test_blocks(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], (1, 4));
    }

    #[test]
    fn cfg_test_block_handles_braces_in_strings() {
        // Unbalanced `{` and `}` inside string literals must not throw the
        // brace counter off; otherwise post-block code could be wrongly
        // included in the test range and real findings hidden.
        let src = "#[cfg(test)]\n\
                   mod tests {\n\
                       fn t() {\n\
                           let a = \"{\";\n\
                           let b = \"}\";\n\
                       }\n\
                   }\n\
                   fn after() {}\n";
        let blocks = find_cfg_test_blocks(src);
        assert_eq!(blocks.len(), 1);
        let (_s, e) = blocks[0];
        assert_eq!(e, 7, "string-literal braces must be ignored");
    }

    #[test]
    fn cfg_test_block_ignores_non_module_attr() {
        let src = "#[cfg(test)]\n\
                   fn standalone_test() { let p = \"../foo\"; }\n";
        // Only `mod` declarations are recognised; standalone `#[cfg(test)]`
        // fns are out of scope for this filter.
        assert!(find_cfg_test_blocks(src).is_empty());
    }

    #[test]
    fn cfg_test_block_no_attr_returns_empty() {
        let src = "fn foo() {}\nmod tests {}\n";
        assert!(find_cfg_test_blocks(src).is_empty());
    }

    #[test]
    fn cfg_test_block_skips_line_comments() {
        let src = "#[cfg(test)]\n\
                   mod tests {\n\
                       // close brace } in comment must be ignored\n\
                       fn t() {}\n\
                   }\n";
        let blocks = find_cfg_test_blocks(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], (1, 5));
    }

    #[test]
    fn cfg_test_block_handles_lifetimes() {
        // A lifetime token `'a` must not be treated as a char literal,
        // which would otherwise consume bytes and skew brace counting.
        let src = "#[cfg(test)]\n\
                   mod tests {\n\
                       fn t<'a>(s: &'a str) {}\n\
                   }\n";
        let blocks = find_cfg_test_blocks(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], (1, 4));
    }

    #[test]
    fn cfg_test_block_pub_module() {
        let src = "#[cfg(test)]\n\
                   pub mod tests {\n\
                       fn t() {}\n\
                   }\n";
        assert_eq!(find_cfg_test_blocks(src).len(), 1);
    }

    #[test]
    fn cfg_test_block_multiple_in_one_file() {
        let src = "#[cfg(test)]\n\
                   mod first {\n    \
                       fn a() {}\n\
                   }\n\
                   \n\
                   fn between() {}\n\
                   \n\
                   #[cfg(test)]\n\
                   mod second {\n    \
                       fn b() {}\n\
                   }\n";
        let blocks = find_cfg_test_blocks(src);
        assert_eq!(blocks.len(), 2, "two #[cfg(test)] blocks expected");
        assert_eq!(blocks[0], (1, 4));
        assert_eq!(blocks[1], (8, 11));
    }

    #[test]
    fn cfg_test_block_deeply_nested_braces() {
        let src = "#[cfg(test)]\n\
                   mod tests {\n    \
                       fn t() {\n        \
                           let x = match 1 {\n            \
                               0 => { 0 }\n            \
                               _ => { let y = || { 1 }; y() }\n        \
                           };\n        \
                           let _ = x;\n    \
                       }\n\
                   }\n\
                   fn after() {}\n";
        let blocks = find_cfg_test_blocks(src);
        assert_eq!(blocks.len(), 1);
        let (s, e) = blocks[0];
        assert_eq!(s, 1);
        assert_eq!(e, 10, "must close at outer mod brace, not earlier");
    }

    #[test]
    fn cfg_test_block_real_index_mod_file() {
        // Cross-check against the real `src/index/mod.rs`: every test
        // function declared inside a `#[cfg(test)]` module must fall
        // inside one of the detected ranges. Line numbers are looked up
        // dynamically so the test is resilient to refactors that move
        // code around.
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = manifest.join("src").join("index").join("mod.rs");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let blocks = find_cfg_test_blocks(&text);
        assert!(
            !blocks.is_empty(),
            "expected at least one #[cfg(test)] block in index/mod.rs"
        );
        // Pick a few well-known test fns that have lived in the test mod
        // since the original SEC005 false-positive report.
        for name in [
            "test_resolve_import_parent_dir",
            "test_resolve_import_js_to_ts",
        ] {
            let line = lookup_fn_line(&text, name)
                .unwrap_or_else(|| panic!("{name} not found in index/mod.rs"));
            assert!(
                blocks.iter().any(|(s, e)| line >= *s && line <= *e),
                "{name} (line {line}) not covered by any block; got {blocks:?}"
            );
        }
    }

    #[test]
    fn cfg_test_block_real_server_mod_file() {
        // Cross-check against the real `src/server/mod.rs`. It carries
        // two #[cfg(test)] modules; verify all four functions that
        // originally produced the SEC005 false positives are filtered.
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = manifest.join("src").join("server").join("mod.rs");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let blocks = find_cfg_test_blocks(&text);
        assert!(
            blocks.len() >= 2,
            "expected at least 2 #[cfg(test)] blocks in server/mod.rs, got {blocks:?}"
        );
        for name in [
            "rejects_traversal_beyond_root",
            "rejects_sneaky_traversal",
            "allows_internal_parent_within_root",
            "rejects_single_parent_dir",
        ] {
            let line = lookup_fn_line(&text, name)
                .unwrap_or_else(|| panic!("{name} not found in server/mod.rs"));
            assert!(
                blocks.iter().any(|(s, e)| line >= *s && line <= *e),
                "{name} (line {line}) not covered by any block; got {blocks:?}"
            );
        }
    }

    /// Return the 1-based line number of `fn <name>` in `source`, or
    /// `None` if the function is missing.
    fn lookup_fn_line(source: &str, name: &str) -> Option<u32> {
        let needle = format!("fn {name}");
        source
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains(&needle))
            .map(|(i, _)| (i + 1) as u32)
    }

    #[test]
    fn cfg_test_block_does_not_eat_post_block_code() {
        // Ensure that real production code AFTER a test block is NOT
        // included in the block range, because that would mask real
        // vulnerabilities.
        let src = "#[cfg(test)]\n\
                   mod tests {\n    \
                       fn t() { let _ = \"../safe-in-test\"; }\n\
                   }\n\
                   \n\
                   pub fn risky() {\n    \
                       let p = \"../etc/passwd\";\n\
                   }\n";
        let blocks = find_cfg_test_blocks(src);
        assert_eq!(blocks.len(), 1);
        let (s, e) = blocks[0];
        // `pub fn risky` lives at line 6; range must not include it.
        assert!(s == 1 && e == 4, "block must end at line 4, got {blocks:?}");
        let in_block = |line: u32| blocks.iter().any(|(a, b)| line >= *a && line <= *b);
        assert!(!in_block(6), "production fn risky must not be in any block");
        assert!(
            !in_block(7),
            "production code line must not be in any block"
        );
    }
}
