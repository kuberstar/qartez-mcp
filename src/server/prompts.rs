// Rust guideline compliant 2026-04-12

//! Canned MCP workflow prompts.
//!
//! Each prompt is a slash-command recipe the assistant can invoke (e.g.
//! `/qartez_review`). The handler does not touch the database or execute any
//! Qartez tool itself — it returns a deterministic user-message payload
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
             1. Call `qartez_impact` with `file_path=\"{target}\"` — this returns direct importers, transitive dependents, and co-change partners (the full blast radius).\n\
             2. Call `qartez_outline` with `file_path=\"{target}\"` so you can reason about the changed symbols one by one.\n\
             3. For every symbol that was renamed, removed, or had its signature changed, call `qartez_refs` with `symbol=\"<name>\"` to find lingering usages elsewhere.\n\
             4. Call `qartez_cochange` with `file_path=\"{target}\"` and flag any historical sibling files that were NOT touched in this change — those are suspicious omissions.\n\
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
        let top_n = if top_n_raw.is_empty() { "15" } else { top_n_raw };
        let text = format!(
            "Give a one-minute architecture overview of this codebase using Qartez.\n\
             \n\
             Run these tools in order:\n\
             \n\
             1. Call `qartez_map` with `top_n={top_n}` and `format=\"concise\"` — these are the files that matter most by PageRank.\n\
             2. Call `qartez_stats` (no arguments) for the language / LOC / symbol-count breakdown.\n\
             3. Call `qartez_project` with `action=\"info\"` to surface the detected build tool and test / lint / typecheck commands.\n\
             \n\
             Then write a narrative overview in this shape:\n\
             - what kind of project this is (language mix, primary toolchain)\n\
             - the 3–5 most central files and what they own (cite PageRank)\n\
             - the dependency spine: which files fan into which\n\
             - where to start reading if you had to understand it in 30 minutes\n\
             \n\
             Keep it tight — one screenful, no speculation beyond what the tools report."
        );
        GetPromptResult::new(user_text(text)).with_description(
            "Qartez architecture overview workflow".to_string(),
        )
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
             - callers — the most likely entry points for the failing path\n\
             - callees — the downstream functions to audit\n\
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
                    "1. Call `qartez_map` with `boost_terms=[\"{a}\"]`, `top_n=15`, and `format=\"detailed\"` — this surfaces the PageRank-ranked files biased toward `{a}`."
                ),
                format!("focused on `{a}`"),
            ),
            None => (
                "1. Call `qartez_map` with `top_n=15` and `format=\"detailed\"` — this surfaces the PageRank-ranked core files."
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
        let (file_list, plan) = if files.is_empty() {
            (
                "(no files supplied — ask the user or read `git diff --name-only`)".to_string(),
                "1. If the caller did not provide any files, run `git diff --name-only` via Bash and feed the result back through this prompt.".to_string(),
            )
        } else {
            let listed = files
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n");
            let steps = files
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    format!(
                        "{step}. Call `qartez_impact` with `file_path=\"{f}\"` AND `qartez_cochange` with `file_path=\"{f}\"` to collect direct importers plus historical co-change partners.",
                        step = i + 1,
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            (listed, steps)
        };
        let final_step = files.len() + 1;
        let text = format!(
            "Pre-merge safety check for this change set using Qartez.\n\
             \n\
             Changed files:\n\
             {file_list}\n\
             \n\
             Run these tools in order:\n\
             \n\
             {plan}\n\
             {final_step}. Call `qartez_unused` (no arguments) to surface any exports that became dead after the change.\n\
             \n\
             Then return a merge-risk report:\n\
             - total blast radius (union of direct importers across the changed files)\n\
             - co-change partners that were NOT touched — suspicious omissions worth a sanity-check\n\
             - exports that are now unused — candidates for deletion or a follow-up PR\n\
             - a final ship / hold recommendation with a one-line justification"
        );
        GetPromptResult::new(user_text(text))
            .with_description("Qartez pre-merge safety-check workflow".to_string())
    }
}
