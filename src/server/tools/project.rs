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
        // Reject `timeout=0` up front, regardless of `action`. The
        // schema documents `timeout=0` as a hard reject because a
        // zero-second budget would expire before any toolchain could
        // start. Previously the check lived inside the run-the-command
        // branch, so `action=info, timeout=0` was silently accepted
        // and the contract diverged from its documentation.
        if let Some(0) = params.timeout {
            return Err(
                "timeout must be >= 1 (a 0-second budget would expire before the command could start). Use a positive integer in seconds, capped at 600.".into(),
            );
        }
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
            // Identify the "primary" toolchain: the one closest to the
            // project root. Root-level toolchains carry `subdir == None`,
            // so we prefer the first such entry; if every detection
            // came from a subdirectory (no root-level manifest), the
            // first entry in detection order is treated as primary.
            // The dispatcher already prefers the toolchain that
            // actually defines the requested command via `pick_by`,
            // but the info output needs to surface that disambiguation
            // explicitly so a caller staring at three Cargo blocks can
            // see which one drives `qartez_project action=test`.
            let primary_index = all_toolchains
                .iter()
                .position(|tc| tc.subdir.is_none())
                .unwrap_or(0);
            let ambiguous = all_toolchains.len() > 1;
            let mut out = String::new();
            if ambiguous {
                out.push_str(&format!(
                    "# Note: {} toolchains detected; the first match per command wins. The closest-to-root toolchain is marked [primary] below.\n\n",
                    all_toolchains.len(),
                ));
            }
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
                let primary_tag = if ambiguous && i == primary_index {
                    " [primary]"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "# Project toolchain: {}{}{}{}\n\n",
                    tc.name, subdir_tag, primary_tag, marker,
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

        if all_toolchains.is_empty() {
            return Err(
                "No recognized toolchain found. Looked for: Cargo.toml, package.json, go.mod, pyproject.toml, setup.py, pubspec.yaml, Gemfile, Makefile, pom.xml, build.gradle(.kts), build.sbt".to_string()
            );
        }

        // Per-action toolchain pick: route to the first detected
        // toolchain that actually defines the requested command,
        // rather than always picking `[0]`. A monorepo root with a
        // bare `Makefile` plus a `qartez-public/Cargo.toml` subdir
        // otherwise reported "No test command configured for make
        // toolchain" because the pruned `make` entry sorted first
        // and its `test_cmd` was emptied by the Makefile-target
        // prune. The subdir Cargo toolchain (which DOES define
        // `cargo test`) was never consulted. `action=info` still
        // lists every detected toolchain unchanged.
        let pick_by = |needs: fn(&toolchain::DetectedToolchain) -> bool,
                       label: &str|
         -> Result<&toolchain::DetectedToolchain, String> {
            all_toolchains
                    .iter()
                    .find(|tc| needs(tc))
                    .ok_or_else(|| {
                        let head = all_toolchains
                            .first()
                            .map(|t| t.name.as_str())
                            .unwrap_or("<none>");
                        format!(
                            "No {label} command configured across the detected toolchains (primary: {head}, total: {}). Run `qartez_project action=info` to see what each toolchain supports.",
                            all_toolchains.len(),
                        )
                    })
        };

        if action == ProjectAction::Run {
            // `run` without a filter defaults to `build`. The previous
            // default of `test` surprised callers expecting the bare
            // "compile everything" verb; `qartez_project action=test`
            // remains the explicit verb for running tests.
            let subcommand = params.filter.as_deref().unwrap_or("build");
            let tc = match subcommand {
                "test" => pick_by(|tc| !tc.test_cmd.is_empty(), "test")?,
                "build" => pick_by(|tc| !tc.build_cmd.is_empty(), "build")?,
                "lint" => pick_by(|tc| tc.lint_cmd.is_some(), "lint")?,
                "typecheck" => pick_by(|tc| tc.typecheck_cmd.is_some(), "typecheck")?,
                other => {
                    return Err(format!(
                        "Unknown run subcommand '{other}'. Supported: test, build, lint, typecheck",
                    ));
                }
            };
            let resolved: &Vec<String> = match subcommand {
                "test" => &tc.test_cmd,
                "build" => &tc.build_cmd,
                "lint" => tc.lint_cmd.as_ref().expect("pick guaranteed presence"),
                "typecheck" => tc.typecheck_cmd.as_ref().expect("pick guaranteed presence"),
                _ => unreachable!(),
            };
            let subdir_tag = tc
                .subdir
                .as_deref()
                .map(|s| format!(" (subdir: {s}/)"))
                .unwrap_or_default();
            return Ok(format!(
                "# {toolchain}{subdir_tag} {sub} (dry-run - command not executed)\n$ {cmd}\n",
                toolchain = tc.name,
                sub = subcommand,
                cmd = resolved.join(" "),
            ));
        }

        let (tc, cmd, action_label): (&toolchain::DetectedToolchain, &Vec<String>, &'static str) =
            match action {
                ProjectAction::Test => {
                    let tc = pick_by(|tc| !tc.test_cmd.is_empty(), "test")?;
                    (tc, &tc.test_cmd, "TEST")
                }
                ProjectAction::Build => {
                    let tc = pick_by(|tc| !tc.build_cmd.is_empty(), "build")?;
                    (tc, &tc.build_cmd, "BUILD")
                }
                ProjectAction::Lint => {
                    let tc = pick_by(|tc| tc.lint_cmd.is_some(), "lint")?;
                    let cmd = tc.lint_cmd.as_ref().expect("pick guaranteed presence");
                    (tc, cmd, "LINT")
                }
                ProjectAction::Typecheck => {
                    let tc = pick_by(|tc| tc.typecheck_cmd.is_some(), "typecheck")?;
                    let cmd = tc.typecheck_cmd.as_ref().expect("pick guaranteed presence");
                    (tc, cmd, "TYPECHECK")
                }
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
        // Reject shell-injection metacharacters even though the filter
        // is passed through `std::process::Command::arg` (which avoids
        // a shell). Downstream build tools may still re-shell-parse
        // their arguments (e.g. Make recipes, `cargo test -- <arg>`
        // into a runner that splits on whitespace), and a lone quote
        // or subshell char in the filter leaks straight through. Keep
        // the allowed set to identifiers, path-like tokens, and a few
        // structured separators.
        if let Some(f) = filter {
            const FORBIDDEN: &[char] = &['\'', '"', '`', ';', '|', '&', '$', '<', '>', '\\', '\n'];
            if let Some(bad) = FORBIDDEN.iter().find(|c| f.contains(**c)) {
                return Err(format!(
                    "Filter contains unsupported character '{bad}': filter may contain only alphanumerics, '-', '_', '.', '/', ':', '=', '@', '+', and whitespace. Got: {f}",
                ));
            }
        }

        // Pre-flight test filter against the index: when the caller
        // asks for `action=test filter=<pattern>` and zero indexed
        // function names match `<pattern>` as a substring, the
        // toolchain-driven test runner would still kick off a full
        // build before discovering that no test matches and exiting
        // with `0 tests run`. That wastes minutes of CPU and burns
        // the user's timeout budget. Refuse up-front when we can
        // prove no candidate test exists, but stay conservative:
        // only short-circuit when the index has at least one function
        // symbol overall (so empty / pre-index sessions still fall
        // through to the toolchain).
        if matches!(action, ProjectAction::Test)
            && let Some(f) = filter
            && !f.is_empty()
            && let Ok(conn) = self.db.lock()
        {
            const ANY_FUNCTION_QUERY: &str =
                "SELECT 1 FROM symbols WHERE kind = 'function' LIMIT 1";
            let any_function: bool = conn
                .query_row(ANY_FUNCTION_QUERY, [], |_| Ok(()))
                .map(|()| true)
                .unwrap_or(false);
            if any_function {
                let needle = f.to_ascii_lowercase();
                const MATCH_QUERY: &str = "SELECT 1 FROM symbols \
                     WHERE kind IN ('function', 'method') \
                     AND instr(LOWER(name), ?1) > 0 \
                     LIMIT 1";
                let has_match: bool = conn
                    .query_row(MATCH_QUERY, rusqlite::params![needle], |_| Ok(()))
                    .map(|()| true)
                    .unwrap_or(false);
                if !has_match {
                    return Err(format!(
                        "No indexed function or method name contains '{f}'. Refusing to run `{}{}` because it would compile the project before discovering no test matches the filter. Use `qartez_grep query='{f}*'` (with regex=false for FTS prefix) to find the actual test name first.",
                        cmd.join(" "),
                        filter.map(|x| format!(" {x}")).unwrap_or_default(),
                    ));
                }
            }
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
