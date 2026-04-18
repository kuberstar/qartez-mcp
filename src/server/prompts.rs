// Rust guideline compliant 2026-04-12

//! Canned MCP workflow prompts.
//!
//! Each prompt is a slash-command recipe the assistant can invoke (e.g.
//! `/qartez_review`). The handler does not touch the database or execute any
//! Qartez tool itself - it returns a deterministic user-message payload
//! that tells the assistant which Qartez tools to call, in what order, and
//! how to interpret the output. That keeps prompts declarative, cheap to
//! test, and free of runtime side effects.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{GetPromptResult, PromptMessage, PromptMessageRole};
use rmcp::{prompt, prompt_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::QartezServer;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SoulReviewArgs {
    /// Changed file path (or comma-separated list) to review.
    pub target: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SoulArchitectureArgs {
    /// How many top PageRank-ranked files to surface (default: 15).
    pub top_n: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SoulDebugArgs {
    /// Symbol or file path you are investigating.
    pub target: String,
    /// Optional file path to disambiguate when multiple definitions share the same name.
    #[serde(default)]
    pub file_path: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SoulOnboardArgs {
    /// Subsystem keyword to focus on (e.g., "auth", "billing", "indexing").
    pub area: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SoulPreMergeArgs {
    /// Comma-separated list of changed file paths (as printed by `git diff --name-only`).
    pub files: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SoulArchReviewArgs {
    /// Optional area to focus on (e.g., "auth", "data pipeline", "external integrations").
    pub focus: Option<String>,
}

fn user_text(text: String) -> Vec<PromptMessage> {
    vec![PromptMessage::new_text(PromptMessageRole::User, text)]
}

/// Split a comma- or whitespace-separated list of file paths, keeping only
/// the non-empty entries in their original order.
fn split_files(raw: &str) -> Vec<String> {
    raw.split([',', '\n'])
        .flat_map(|chunk| chunk.split_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[prompt_router(router = "prompt_router", vis = "pub(crate)")]
impl QartezServer {
    /// Code review workflow: blast radius, outline, references, co-change partners.
    #[prompt(
        name = "qartez_review",
        description = "Review a changed file using Qartez: blast radius, symbol outline, dangling references, and co-change partners. Argument: `target` (file path)."
    )]
    pub fn qartez_review_prompt(
        &self,
        Parameters(args): Parameters<SoulReviewArgs>,
    ) -> GetPromptResult {
        let target = args.target.trim();
        let text = format!(
            "Review `{target}` with the Qartez MCP workflow.\n\
             \n\
             Run these tools in order and summarise the findings:\n\
             \n\
             1. Call `qartez_impact` with `file_path=\"{target}\"` - this returns direct importers, transitive dependents, and co-change partners (the full blast radius).\n\
             2. Call `qartez_outline` with `file_path=\"{target}\"` so you can reason about the changed symbols one by one.\n\
             3. For every symbol that was renamed, removed, or had its signature changed, call `qartez_refs` with `symbol=\"<name>\"` to find lingering usages elsewhere.\n\
             4. Call `qartez_cochange` with `file_path=\"{target}\"` and flag any historical sibling files that were NOT touched in this change - those are suspicious omissions.\n\
             \n\
             Finish with a concise review checklist:\n\
             - files to verify (direct importers, PageRank-ordered)\n\
             - suspicious co-change partners that were not updated\n\
             - references that still match old names\n\
             - any newly-unused exports (optionally: `qartez_unused`)"
        );
        GetPromptResult::new(user_text(text))
            .with_description(format!("Qartez review workflow for {target}"))
    }

    /// One-minute architecture overview grounded in PageRank-ranked files.
    #[prompt(
        name = "qartez_architecture",
        description = "Produce a one-minute codebase architecture overview grounded in PageRank, language stats, and detected toolchain. Optional argument: `top_n`."
    )]
    pub fn qartez_architecture_prompt(
        &self,
        Parameters(args): Parameters<SoulArchitectureArgs>,
    ) -> GetPromptResult {
        let top_n_raw = args.top_n.as_deref().unwrap_or("15").trim();
        let top_n = if top_n_raw.is_empty() {
            "15"
        } else {
            top_n_raw
        };
        let text = format!(
            "Give a one-minute architecture overview of this codebase using Qartez.\n\
             \n\
             Run these tools in order:\n\
             \n\
             1. Call `qartez_map` with `top_n={top_n}` and `format=\"concise\"` - these are the files that matter most by PageRank.\n\
             2. Call `qartez_stats` (no arguments) for the language / LOC / symbol-count breakdown.\n\
             3. Call `qartez_project` with `action=\"info\"` to surface the detected build tool and test / lint / typecheck commands.\n\
             \n\
             Then write a narrative overview in this shape:\n\
             - what kind of project this is (language mix, primary toolchain)\n\
             - the 3–5 most central files and what they own (cite PageRank)\n\
             - the dependency spine: which files fan into which\n\
             - where to start reading if you had to understand it in 30 minutes\n\
             \n\
             Keep it tight - one screenful, no speculation beyond what the tools report."
        );
        GetPromptResult::new(user_text(text))
            .with_description("Qartez architecture overview workflow".to_string())
    }

    /// Debugging workflow: locate, read, and trace a suspect symbol.
    #[prompt(
        name = "qartez_debug",
        description = "Debugging workflow for a suspect symbol or file: definition, body, call hierarchy, and all references. Argument: `target` (symbol name or file path), optional `file_path`."
    )]
    pub fn qartez_debug_prompt(
        &self,
        Parameters(args): Parameters<SoulDebugArgs>,
    ) -> GetPromptResult {
        let target = args.target.trim();
        let file_hint = args
            .file_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let step2 = match file_hint {
            Some(fp) => format!(
                "2. Call `qartez_read` with `symbol_name=\"{target}\"` and `file_path=\"{fp}\"` to dump its source with line numbers."
            ),
            None => format!(
                "2. Call `qartez_read` with `symbol_name=\"{target}\"` to dump its source with line numbers. If multiple matches come back, pick the one the user means and re-run with the disambiguating `file_path`."
            ),
        };
        let text = format!(
            "Debug `{target}` using the Qartez MCP workflow.\n\
             \n\
             Run these tools in order:\n\
             \n\
             1. Call `qartez_find` with `name=\"{target}\"` to locate the definition(s). Note the file, line range, and whether multiple symbols share this name.\n\
             {step2}\n\
             3. Call `qartez_calls` with `name=\"{target}\"` and `direction=\"both\"` (depth 1) to see who calls it and what it calls.\n\
             4. Call `qartez_refs` with `symbol=\"{target}\"` to find every file that imports or uses it.\n\
             \n\
             Then return a focused debug summary:\n\
             - definition location (file + line range)\n\
             - what the function does (one-line distillation of the body)\n\
             - callers - the most likely entry points for the failing path\n\
             - callees - the downstream functions to audit\n\
             - any reference sites that look inconsistent with the current signature"
        );
        GetPromptResult::new(user_text(text))
            .with_description(format!("Qartez debug workflow for {target}"))
    }

    /// Onboarding workflow: the five files to read first, in order, for a given area.
    #[prompt(
        name = "qartez_onboard",
        description = "New-developer onboarding for a subsystem. Returns the five files to read first with motivation for each. Optional argument: `area`."
    )]
    pub fn qartez_onboard_prompt(
        &self,
        Parameters(args): Parameters<SoulOnboardArgs>,
    ) -> GetPromptResult {
        let area = args
            .area
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let (step1, scope_note) = match area {
            Some(a) => (
                format!(
                    "1. Call `qartez_map` with `boost_terms=[\"{a}\"]`, `top_n=15`, and `format=\"detailed\"` - this surfaces the PageRank-ranked files biased toward `{a}`."
                ),
                format!("focused on `{a}`"),
            ),
            None => (
                "1. Call `qartez_map` with `top_n=15` and `format=\"detailed\"` - this surfaces the PageRank-ranked core files."
                    .to_string(),
                "of the whole codebase".to_string(),
            ),
        };
        let text = format!(
            "Produce an onboarding reading list {scope_note} using Qartez.\n\
             \n\
             Run these tools in order:\n\
             \n\
             {step1}\n\
             2. Pick the top 3 files from step 1 and call `qartez_context` with `files=[<those 3 paths>]` to see which related files round out the picture.\n\
             3. Call `qartez_outline` on the most-imported file from step 1 to preview its symbols before the reader opens it.\n\
             \n\
             Then return an ordered reading list:\n\
             - exactly five files, ranked by where a new contributor should start\n\
             - for each file: one sentence explaining why it matters and which symbols to read first\n\
             - a final sentence tying them together into a mental model"
        );
        GetPromptResult::new(user_text(text))
            .with_description("Qartez onboarding workflow".to_string())
    }

    /// Pre-merge safety check: blast radius, co-change gaps, and newly unused exports.
    #[prompt(
        name = "qartez_pre_merge",
        description = "Pre-merge safety check across the changed files: blast radius, untouched co-change partners, and newly unused exports. Argument: `files` (comma-separated list)."
    )]
    pub fn qartez_pre_merge_prompt(
        &self,
        Parameters(args): Parameters<SoulPreMergeArgs>,
    ) -> GetPromptResult {
        let files = split_files(&args.files);
        let text = if files.is_empty() {
            "Pre-merge safety check using Qartez.\n\
             \n\
             No files were specified. Run these steps:\n\
             \n\
             1. Call `qartez_diff_impact` with `base=\"main\"` (or the appropriate target branch) \
             to get a unified impact report: changed files, union blast radius, convergence points, \
             and co-change omissions in one call.\n\
             2. Call `qartez_unused` (no arguments) to surface any exports that became dead after the change.\n\
             \n\
             Then return a merge-risk report:\n\
             - total blast radius (union of direct importers across the changed files)\n\
             - co-change partners that were NOT touched - suspicious omissions worth a sanity-check\n\
             - exports that are now unused - candidates for deletion or a follow-up PR\n\
             - a final ship / hold recommendation with a one-line justification"
                .to_string()
        } else {
            let file_list = files
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "Pre-merge safety check for this change set using Qartez.\n\
                 \n\
                 Changed files:\n\
                 {file_list}\n\
                 \n\
                 Run these steps:\n\
                 \n\
                 1. Call `qartez_diff_impact` with `base=\"main\"` (or the appropriate target branch). \
                 This single call covers blast radius, convergence points, and co-change omissions \
                 for all changed files.\n\
                 2. Call `qartez_unused` (no arguments) to surface any exports that became dead after the change.\n\
                 \n\
                 Then return a merge-risk report:\n\
                 - total blast radius (union of direct importers across the changed files)\n\
                 - co-change partners that were NOT touched - suspicious omissions worth a sanity-check\n\
                 - exports that are now unused - candidates for deletion or a follow-up PR\n\
                 - a final ship / hold recommendation with a one-line justification"
            )
        };
        GetPromptResult::new(user_text(text))
            .with_description("Qartez pre-merge safety-check workflow".to_string())
    }

    /// Architecture risk audit: structural risks that emerge from module relationships.
    #[prompt(
        name = "qartez_arch_review",
        description = "Architecture risk audit: single points of failure, unguarded entry points, abstraction leaks, silent degradation, and coupling hotspots. Optional argument: `focus` (area keyword)."
    )]
    pub fn qartez_arch_review_prompt(
        &self,
        Parameters(args): Parameters<SoulArchReviewArgs>,
    ) -> GetPromptResult {
        let focus = args
            .focus
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let (map_step, focus_note) = match focus {
            Some(f) => (
                format!(
                    "1. Call `qartez_map` with `boost_terms=[\"{f}\"]`, `top_n=20`, and `format=\"detailed\"` \
                     to get the PageRank-ranked project skeleton biased toward `{f}`."
                ),
                format!(" focused on `{f}`"),
            ),
            None => (
                "1. Call `qartez_map` with `top_n=20` and `format=\"detailed\"` \
                 to get the PageRank-ranked project skeleton."
                    .to_string(),
                String::new(),
            ),
        };

        let text = format!(
            "Perform an architecture risk audit of this codebase{focus_note} using Qartez.\n\
             \n\
             Run these tools in order. Steps 1-3 can run in parallel, then 4-6 sequentially:\n\
             \n\
             {map_step}\n\
             2. Call `qartez_stats` (no arguments) for language mix, LOC, and symbol count.\n\
             3. Call `qartez_security` (no arguments) for known security surface findings.\n\
             4. Call `qartez_hotspots` with `top_n=10` to find where complexity, coupling, and churn concentrate.\n\
             5. Call `qartez_boundaries` with `suggest=true` to see the Leiden-derived module clusters \
                and where the natural architectural seams are. If this returns a \"no cluster \
                assignment\" message, run `qartez_wiki` first to generate the clusters, then \
                retry `qartez_boundaries`.\n\
             6. For the top 3 hotspot files from step 4, call `qartez_deps` with `file_path=<file>` \
                to see their dependency fan-in and fan-out.\n\
             7. Identify any files that look like network-facing entry points (HTTP handlers, server \
                startup, route registration, CLI commands that start servers). For each, call \
                `qartez_calls` with `name=<entry_symbol>` and `direction=\"callees\"`, `depth=3` \
                to trace the call chain inward from the external surface.\n\
             \n\
             With all that data, analyze the codebase for these architectural risks:\n\
             \n\
             **Single points of failure** - High-PageRank modules with many dependents but no \
             redundancy, fallback, or retry visible in their call chain. Ask: if this module \
             fails or is unreachable, what degrades?\n\
             \n\
             **Unguarded entry points** - Symbols reachable from network-facing handlers where \
             the call chain between handler and business logic has no authentication, authorization, \
             or input validation step visible.\n\
             \n\
             **Abstraction leaks** - Protocols, traits, or interfaces where some concrete \
             implementations skip methods, return stubs, or silently no-op. Look for abstract \
             definitions whose call chains reach a concrete impl that does less than callers expect.\n\
             \n\
             **Silent degradation** - Error handling that swallows failures and returns defaults \
             instead of propagating. Especially dangerous on external boundaries (API calls, \
             database queries, network I/O) where a silent fallback hides an outage.\n\
             \n\
             **Coupling hotspots** - Modules that appear in multiple Leiden communities, or that \
             have high fan-in from unrelated clusters. These are architectural bottlenecks that \
             make changes risky and hard to test in isolation.\n\
             \n\
             **Missing resilience** - External API calls (HTTP clients, database connections, \
             third-party SDKs) with no retry, backoff, timeout, or circuit-breaking visible \
             in the call chain.\n\
             \n\
             Return a structured report:\n\
             \n\
             ## Architecture Risk Review\n\
             \n\
             ### Scope\n\
             State what was analyzed: entry points identified, module count, language mix.\n\
             \n\
             ### Findings\n\
             \n\
             | # | Risk | Severity | Evidence | Recommendation |\n\
             |---|------|----------|----------|----------------|\n\
             \n\
             For each finding:\n\
             - **Risk**: one-line description of the structural problem\n\
             - **Severity**: high / medium / low\n\
             - **Evidence**: which Qartez tool surfaced it, with the specific file and symbol\n\
             - **Recommendation**: concrete next step (not \"consider\" or \"evaluate\" - say what to do)\n\
             \n\
             ### Health summary\n\
             Two to three sentences: overall architectural health, the single highest-priority \
             risk, and whether the codebase is in good shape for its current scale.\n\
             \n\
             Only report findings you have evidence for from the tool output. Do not speculate \
             about risks the tools did not surface. If an area looks clean, say so briefly and \
             move on."
        );
        GetPromptResult::new(user_text(text))
            .with_description("Qartez architecture risk audit workflow".to_string())
    }
}
