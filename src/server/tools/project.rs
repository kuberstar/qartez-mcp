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

#[tool_router(router = qartez_project_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_project",
        description = "Run project commands (test, build, lint, typecheck) with auto-detected toolchain (Cargo, npm/bun/yarn/pnpm, Go, Python, Dart/Flutter, Maven, Gradle, sbt, Ruby, Make). Use action='info' to see detected commands. Use filter for targeted runs (e.g., test name).",
        annotations(
            title = "Run Project Command",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub(in crate::server) fn qartez_project(
        &self,
        Parameters(params): Parameters<SoulProjectParams>,
    ) -> Result<String, String> {
        let mut all_toolchains = toolchain::detect_all_toolchains(&self.project_root);
        // Monorepo fallback: when the repository root has its own
        // (possibly generic) Makefile but per-crate manifests live under
        // first-level subdirectories, surface each subdir toolchain too.
        // Without this, a layout like `qartez-public/Cargo.toml` next to
        // a top-level Makefile reported `Test: (not configured)` because
        // the detector only looked at the root. Subdir entries carry a
        // `subdir` tag so the info output names them explicitly.
        const MAX_SUBDIRS_SCANNED: usize = 24;
        let subdir_tcs =
            toolchain::detect_subdir_toolchains(&self.project_root, MAX_SUBDIRS_SCANNED);
        all_toolchains.extend(subdir_tcs);
        let action = params.action.unwrap_or_default();

        // The generic `make` toolchain assumes `test`, `build`, and `lint`
        // targets exist, but many Makefiles only define a handful of
        // custom targets (e.g. `release`, `import-pr`). Parse the actual
        // Makefile once up-front and drop commands whose target is not
        // present; the info output also surfaces the detected target
        // list so callers can see what is available.
        let makefile_targets = read_makefile_targets(&self.project_root);
        let all_toolchains: Vec<toolchain::DetectedToolchain> = all_toolchains
            .into_iter()
            .map(|tc| {
                if tc.name == "make"
                    && let Some(ref targets) = makefile_targets
                {
                    prune_make_toolchain(tc, targets)
                } else {
                    tc
                }
            })
            .collect();

        if action == ProjectAction::Info {
            if all_toolchains.is_empty() {
                return Err("No recognized toolchain found. Looked for: Cargo.toml, package.json, go.mod, pyproject.toml, setup.py, pubspec.yaml, Gemfile, Makefile, pom.xml, build.gradle(.kts), build.sbt".to_string());
            }
            let mut out = String::new();
            for (i, tc) in all_toolchains.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                let available = toolchain::binary_available(&tc.build_tool);
                let marker = if available {
                    ""
                } else {
                    " (not found on PATH)"
                };
                let subdir_tag = tc
                    .subdir
                    .as_deref()
                    .map(|s| format!(" (subdir: {s}/)"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "# Project toolchain: {}{}{}\n\n",
                    tc.name, subdir_tag, marker,
                ));
                out.push_str(&format!("Build tool: {}\n", tc.build_tool));
                if tc.test_cmd.is_empty() {
                    out.push_str("Test:       (not configured)\n");
                } else {
                    out.push_str(&format!("Test:       {}\n", tc.test_cmd.join(" ")));
                }
                if tc.build_cmd.is_empty() {
                    out.push_str("Build:      (not configured)\n");
                } else {
                    out.push_str(&format!("Build:      {}\n", tc.build_cmd.join(" ")));
                }
                if let Some(ref lint) = tc.lint_cmd {
                    out.push_str(&format!("Lint:       {}\n", lint.join(" ")));
                }
                if let Some(ref typecheck) = tc.typecheck_cmd {
                    out.push_str(&format!("Typecheck:  {}\n", typecheck.join(" ")));
                }
                if tc.name == "make"
                    && let Some(ref targets) = makefile_targets
                {
                    if targets.is_empty() {
                        out.push_str("Makefile targets: (none detected)\n");
                    } else {
                        out.push_str(&format!("Makefile targets: {}\n", targets.join(", ")));
                    }
                }
            }
            return Ok(out);
        }

        let tc = all_toolchains.into_iter().next().ok_or_else(|| {
            "No recognized toolchain found. Looked for: Cargo.toml, package.json, go.mod, pyproject.toml, setup.py, pubspec.yaml, Gemfile, Makefile, pom.xml, build.gradle(.kts), build.sbt".to_string()
        })?;

        if action == ProjectAction::Run {
            let subcommand = params.filter.as_deref().unwrap_or("test");
            let resolved: &Vec<String> = match subcommand {
                "test" => {
                    if tc.test_cmd.is_empty() {
                        return Err(format!(
                            "No test command configured for {} toolchain",
                            tc.name
                        ));
                    }
                    &tc.test_cmd
                }
                "build" => {
                    if tc.build_cmd.is_empty() {
                        return Err(format!(
                            "No build command configured for {} toolchain",
                            tc.name
                        ));
                    }
                    &tc.build_cmd
                }
                "lint" => tc.lint_cmd.as_ref().ok_or_else(|| {
                    format!("No lint command configured for {} toolchain", tc.name)
                })?,
                "typecheck" => tc.typecheck_cmd.as_ref().ok_or_else(|| {
                    format!("No typecheck command configured for {} toolchain", tc.name)
                })?,
                other => {
                    return Err(format!(
                        "Unknown run subcommand '{other}'. Supported: test, build, lint, typecheck",
                    ));
                }
            };
            return Ok(format!(
                "# {toolchain} {sub} (dry-run - command not executed)\n$ {cmd}\n",
                toolchain = tc.name,
                sub = subcommand,
                cmd = resolved.join(" "),
            ));
        }

        let (cmd, action_label): (&Vec<String>, &'static str) = match action {
            ProjectAction::Test => {
                if tc.test_cmd.is_empty() {
                    return Err(format!(
                        "No test command configured for {} toolchain",
                        tc.name
                    ));
                }
                (&tc.test_cmd, "TEST")
            }
            ProjectAction::Build => {
                if tc.build_cmd.is_empty() {
                    return Err(format!(
                        "No build command configured for {} toolchain",
                        tc.name
                    ));
                }
                (&tc.build_cmd, "BUILD")
            }
            ProjectAction::Lint => (
                tc.lint_cmd.as_ref().ok_or_else(|| {
                    format!("No lint command configured for {} toolchain", tc.name)
                })?,
                "LINT",
            ),
            ProjectAction::Typecheck => (
                tc.typecheck_cmd.as_ref().ok_or_else(|| {
                    format!("No typecheck command configured for {} toolchain", tc.name)
                })?,
                "TYPECHECK",
            ),
            ProjectAction::Info | ProjectAction::Run => {
                // Handled by the early-return branches above.
                unreachable!()
            }
        };

        let timeout = params.timeout.unwrap_or(60).min(600);
        let filter = params.filter.as_deref();
        if let Some(f) = filter
            && f.starts_with('-')
        {
            return Err(format!("Filter must not start with '-': {f}"));
        }

        // Run inside the toolchain's subdirectory when it came from the
        // monorepo fallback. A `cargo build` aimed at a workspace
        // manifest sitting under `qartez-public/` would otherwise be
        // executed from the repo root and pick up the wrong (or
        // missing) Cargo.toml.
        let run_root = match tc.subdir.as_deref() {
            Some(sd) => self.project_root.join(sd),
            None => self.project_root.clone(),
        };
        let (exit_code, output) = toolchain::run_command(&run_root, cmd, filter, timeout)?;

        let status = if exit_code == 0 { "SUCCESS" } else { "FAILED" };
        let mut out = format!(
            "# {} {} (exit code: {})\n$ {}{}\n\n",
            action_label,
            status,
            exit_code,
            cmd.join(" "),
            filter.map(|f| format!(" {f}")).unwrap_or_default(),
        );
        out.push_str(&output);
        Ok(out)
    }
}

/// Parse the top-level `Makefile` at `project_root` and return the set of
/// rule target names it defines. Returns `None` when no Makefile is
/// present (so the caller can leave the default toolchain untouched) and
/// `Some(vec)` otherwise.
///
/// Target detection is deliberately conservative:
/// - only lines that start at column zero are considered,
/// - lines inside recipe bodies (which begin with a tab) are skipped,
/// - `.PHONY:` / `.SUFFIXES:` / other `.FOO` directives are skipped,
/// - lines beginning with `#` are treated as comments,
/// - pattern rules containing `%` are skipped (they are not callable by name),
/// - any line whose first colon-delimited prefix is a valid rule name
///   (`[A-Za-z0-9_./-]+`) is treated as a target.
///
/// Multiple targets on the same line (`foo bar: deps`) are all recorded.
fn read_makefile_targets(project_root: &std::path::Path) -> Option<Vec<String>> {
    let mf = project_root.join("Makefile");
    let src = std::fs::read_to_string(&mf).ok()?;
    let mut targets = Vec::<String>::new();
    let mut seen = std::collections::HashSet::<String>::new();
    for raw in src.lines() {
        let trimmed_start = raw.trim_start();
        if trimmed_start.is_empty() {
            continue;
        }
        // Recipe bodies are indented with a tab; anything starting with
        // whitespace that differs from the raw line is a continuation or
        // recipe line, not a rule header.
        if raw.starts_with([' ', '\t']) {
            continue;
        }
        if trimmed_start.starts_with('#') {
            continue;
        }
        // Only rule lines - the colon must appear before any `=` that
        // would make this a variable assignment.
        let colon = match trimmed_start.find(':') {
            Some(i) => i,
            None => continue,
        };
        if let Some(eq) = trimmed_start.find('=')
            && eq < colon
        {
            continue;
        }
        let lhs = trimmed_start[..colon].trim();
        if lhs.is_empty() {
            continue;
        }
        for name in lhs.split_whitespace() {
            if name.starts_with('.') {
                continue;
            }
            if name.contains('%') {
                continue;
            }
            if !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
            {
                continue;
            }
            if seen.insert(name.to_string()) {
                targets.push(name.to_string());
            }
        }
    }
    Some(targets)
}

/// Keep only the Make subcommands whose target is present in `targets`.
/// The detector in `toolchain::detect_all_toolchains` emits static
/// `make test` / `make build` / `make lint` commands; those are correct
/// for projects that follow the convention and misleading for the many
/// that do not. This prunes the generic defaults to match reality.
fn prune_make_toolchain(
    mut tc: toolchain::DetectedToolchain,
    targets: &[String],
) -> toolchain::DetectedToolchain {
    let has = |name: &str| targets.iter().any(|t| t == name);
    if !has("test") {
        tc.test_cmd = Vec::new();
    }
    if !has("build") {
        tc.build_cmd = Vec::new();
    }
    if tc.lint_cmd.as_ref().is_some_and(|_| !has("lint")) {
        tc.lint_cmd = None;
    }
    tc
}
