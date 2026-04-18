#![allow(unused_imports)]

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};

use super::super::QartezServer;
use super::super::helpers::{self, *};
use super::super::params::*;
use super::super::tiers;
use super::super::treesitter::*;

use crate::graph::blast;
use crate::guard;
use crate::storage::read;
use crate::storage::read::sanitize_fts_query;
use crate::toolchain;

#[tool_router(router = qartez_security_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_security",
        description = "Scan indexed code for security vulnerability patterns (OWASP top-10, hardcoded secrets, injection, unsafe code). Findings are scored by severity x PageRank so vulnerabilities in high-impact files surface first. Supports custom rules via `.qartez/security.toml`.",
        annotations(
            title = "Security Scanner",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_security(
        &self,
        Parameters(params): Parameters<SoulSecurityParams>,
    ) -> Result<String, String> {
        use crate::graph::security::{
            ScanOptions, Severity, apply_config, builtin_rules, load_custom_config, scan,
        };

        let concise = is_concise(&params.format);
        let limit = params.limit.unwrap_or(50) as usize;
        let offset = params.offset.unwrap_or(0) as usize;
        let include_tests = params.include_tests.unwrap_or(false);

        let min_severity = match params.severity.as_deref() {
            Some("critical") => Severity::Critical,
            Some("high") => Severity::High,
            Some("medium") => Severity::Medium,
            Some("low") | None => Severity::Low,
            Some(other) => {
                return Err(format!(
                    "Unknown severity '{other}'. Use: low, medium, high, critical"
                ));
            }
        };

        let mut rules = builtin_rules();

        // Load custom config if available.
        let config_rel = params
            .config_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(".qartez/security.toml");
        let config_abs = self.safe_resolve(config_rel)?;
        if config_abs.exists() {
            let config = load_custom_config(&config_abs)?;
            apply_config(&mut rules, &config)?;
        }

        let conn = self.db.lock().map_err(|e| format!("DB lock error: {e}"))?;

        let opts = ScanOptions {
            include_tests,
            category_filter: params.category.clone(),
            min_severity,
            // Windows callers may pass either separator; DB keys are forward-slash.
            file_path_filter: params
                .file_path
                .as_ref()
                .map(|s| crate::index::to_forward_slash(s.clone())),
            project_roots: self
                .project_roots
                .read()
                .map_err(|e| e.to_string())?
                .clone(),
        };

        let findings = scan(&conn, &rules, &opts);
        drop(conn);

        if findings.is_empty() {
            return Ok(
                "No security findings. All scanned symbols passed the active rule set.".to_string(),
            );
        }

        let total = findings.len();
        let unique_files: HashSet<&str> = findings.iter().map(|f| f.file_path.as_str()).collect();
        let file_count = unique_files.len();

        let page: Vec<_> = findings.into_iter().skip(offset).take(limit).collect();

        let mut out = String::new();
        out.push_str(&format!(
            "# Security Scan: {total} finding(s) across {file_count} file(s)\n\n",
        ));

        if concise {
            out.push_str("# risk severity rule file symbol line\n");
            for (i, f) in page.iter().enumerate() {
                out.push_str(&format!(
                    "{} {:.4} {} {} {} {} {}\n",
                    offset + i + 1,
                    f.risk_score,
                    f.severity.label(),
                    f.rule_name,
                    f.file_path,
                    f.symbol_name,
                    f.line_start,
                ));
            }
        } else {
            out.push_str("  # | Risk   | Sev      | Rule              | File                          | Symbol          | Line\n");
            out.push_str("----+--------+----------+-------------------+-------------------------------+-----------------+-----\n");
            for (i, f) in page.iter().enumerate() {
                out.push_str(&format!(
                    "{:>3} | {:>6.4} | {:<8} | {:<17} | {:<29} | {:<15} | {}\n",
                    offset + i + 1,
                    f.risk_score,
                    f.severity.label(),
                    truncate_path(&f.rule_name, 17),
                    truncate_path(&f.file_path, 29),
                    truncate_path(&f.symbol_name, 15),
                    f.line_start,
                ));
            }

            // Append snippets for detailed mode.
            let with_snippets: Vec<_> = page
                .iter()
                .enumerate()
                .filter_map(|(i, f)| f.snippet.as_ref().map(|s| (i, f, s)))
                .collect();
            if !with_snippets.is_empty() {
                out.push_str("\n## Snippets\n\n");
                for (i, f, snippet) in with_snippets {
                    out.push_str(&format!(
                        "  #{} [{}] {}:{} -- {}\n    {}\n",
                        offset + i + 1,
                        f.rule_id,
                        f.file_path,
                        f.line_start,
                        f.description,
                        snippet,
                    ));
                }
            }
        }

        if total > offset + limit {
            out.push_str(&format!(
                "\nShowing {}-{} of {}. Use offset={} to see more.\n",
                offset + 1,
                offset + page.len(),
                total,
                offset + limit,
            ));
        }

        Ok(out)
    }
}
