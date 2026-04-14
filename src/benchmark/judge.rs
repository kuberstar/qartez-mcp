//! LLM-judge that shells out to `claude -p` (the Claude Code CLI) to
//! score each scenario's MCP vs non-MCP output on five axes:
//! correctness, completeness, usability, groundedness, and
//! conciseness. Uses the user's
//! installed Claude Code session for authentication, so no
//! `ANTHROPIC_API_KEY` is required — under a Max subscription the
//! marginal cost per score is effectively zero and the model can be
//! the strongest one available (`claude-opus-4-6`).
//!
//! # Why subprocess, not an SDK crate
//!
//! The benchmark harness is a small self-contained Rust binary with
//! no async runtime. Shelling out to the Claude Code CLI keeps the
//! dependency surface minimal (`std::process::Command` plus
//! `serde_json`) and piggybacks on whatever auth the user already has
//! configured, including Bedrock / Vertex / apiKeyHelper. The
//! tradeoff is a hard dependency on `claude` being on `PATH` at
//! `--judge` time; the rest of the binary runs without it.
//!
//! # Determinism
//!
//! Opus at temperature zero is close to deterministic across runs
//! but not strictly so — expect ±1 point drift per axis. Do not use
//! quality scores as a strict CI regression gate; treat them as a
//! qualitative snapshot refreshed on demand.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::report::ScenarioReport;

/// Default Claude Code model used by the judge. Opus 4.6 is picked
/// because any Max subscription can use it for free and it gives the
/// strongest judgment; callers can override via `--judge-model`.
pub const DEFAULT_JUDGE_MODEL: &str = "claude-opus-4-6";

/// Maximum body length per side in the judge prompt. MCP responses
/// are typically under 2 KB; non-MCP sim output can reach 50 KB+ of
/// glob listings. 4 KB keeps enough signal for quality assessment
/// while halving per-call input tokens vs the previous 8 KB cap.
const MAX_BODY_BYTES: usize = 4 * 1024;

fn truncate_for_judge(s: &str) -> String {
    if s.len() <= MAX_BODY_BYTES {
        return s.to_string();
    }
    let mut cap = MAX_BODY_BYTES;
    while !s.is_char_boundary(cap) && cap > 0 {
        cap -= 1;
    }
    format!("{}\n[...truncated {} bytes]", &s[..cap], s.len() - cap)
}

/// Strips one surrounding layer of Markdown fence if Opus emitted one
/// despite the prompt telling it not to. Handles both ```json and
/// plain ``` variants.
fn strip_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let without_prefix = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    without_prefix
        .strip_suffix("```")
        .unwrap_or(without_prefix)
        .trim()
}

/// Grading order in the judge prompt. The protocol judges every scenario
/// twice — once with MCP as ANSWER A and once with non-MCP as ANSWER A —
/// then un-swaps the per-axis scores back to the `(mcp, non_mcp)` orientation
/// and averages them. This directly counteracts position bias. The enum is
/// exported so the ensemble wrapper can drive both positions itself when
/// calling `build_prompt` for the primary and secondary models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Position {
    McpFirst,
    NonMcpFirst,
}

/// Per-side quality scores on a 0-10 scale across five axes. Each axis
/// is restricted to the anchor set `{0, 3, 5, 7, 10}` via the JSON
/// schema in [`JUDGE_JSON_SCHEMA`]; callers that construct the type
/// manually are expected to stay inside that set (violation is a
/// programming bug — [`parse_judge_response`] is the enforcement point).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SideQuality {
    pub correctness: u8,
    pub completeness: u8,
    pub usability: u8,
    pub groundedness: u8,
    pub conciseness: u8,
}

impl SideQuality {
    /// Arithmetic mean across the five axes.
    pub fn average(&self) -> f64 {
        f64::from(
            u16::from(self.correctness)
                + u16::from(self.completeness)
                + u16::from(self.usability)
                + u16::from(self.groundedness)
                + u16::from(self.conciseness),
        ) / 5.0
    }
}

/// One single-call output from the judge (one `claude -p` invocation).
/// The protocol calls the judge `2 * n` times per scenario — `n`
/// self-consistency runs per position — and aggregates per-axis means
/// via [`aggregate_self_consistency`]. The raw runs are preserved on
/// [`QualityScores::runs`] for inter-rater statistics (Cohen's kappa,
/// Krippendorff's alpha).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerRunScores {
    pub position: Position,
    pub run_index: u8,
    pub mcp: SideQuality,
    pub non_mcp: SideQuality,
    pub verdict: String,
}

/// Full judge verdict for a single scenario: per-side scores on the
/// 5-axis rubric, a one-line summary, the model that produced them,
/// position-bias flags, and self-consistency provenance. The ensemble
/// wrapper carries two of these (one per model) inside its own
/// `EnsembleQualityScores`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityScores {
    pub mcp: SideQuality,
    pub non_mcp: SideQuality,
    pub verdict: String,
    pub model: String,
    pub runs: Vec<PerRunScores>,
    pub flags: Vec<String>,
    pub reference_answer_used: bool,
}

/// JSON schema passed to `claude -p --json-schema` for every call. The
/// `enum: [0, 3, 5, 7, 10]` anchor set structurally enforces the rubric's
/// "no interpolation, nearest anchor" rule — the model literally cannot
/// emit `6` or `8.5`. See `docs/benchmark-v2/judge-core.md` §4.
///
/// The schema is declared as a raw string so it can be copy-pasted into
/// the CLI argument vector without a runtime allocation per call. It is
/// round-tripped through `serde_json::from_str` in the unit test below to
/// catch any typo that would otherwise silently break the schema contract.
pub const JUDGE_JSON_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["a", "b", "verdict"],
  "properties": {
    "a": {
      "type": "object",
      "additionalProperties": false,
      "required": ["correctness","completeness","usability","groundedness","conciseness"],
      "properties": {
        "correctness":  {"type":"integer","enum":[0,3,5,7,10]},
        "completeness": {"type":"integer","enum":[0,3,5,7,10]},
        "usability":    {"type":"integer","enum":[0,3,5,7,10]},
        "groundedness": {"type":"integer","enum":[0,3,5,7,10]},
        "conciseness":  {"type":"integer","enum":[0,3,5,7,10]}
      }
    },
    "b": {
      "type": "object",
      "additionalProperties": false,
      "required": ["correctness","completeness","usability","groundedness","conciseness"],
      "properties": {
        "correctness":  {"type":"integer","enum":[0,3,5,7,10]},
        "completeness": {"type":"integer","enum":[0,3,5,7,10]},
        "usability":    {"type":"integer","enum":[0,3,5,7,10]},
        "groundedness": {"type":"integer","enum":[0,3,5,7,10]},
        "conciseness":  {"type":"integer","enum":[0,3,5,7,10]}
      }
    },
    "verdict": {"type": "string", "maxLength": 200}
  }
}"#;

/// System prompt appended to every judge call via `claude -p
/// --append-system-prompt`. This is one of the three determinism layers
/// described in `docs/benchmark-v2/judge-core.md` §1.fallback — the
/// others are `--json-schema` (above) and self-consistency averaging
/// (below). None of them is as strong as a real `temperature=0` knob,
/// but we cannot pin temperature through the Claude Code CLI.
pub const DETERMINISM_SYSTEM_PROMPT: &str = "You are an evaluator. Be deterministic: for identical input, produce identical output. Do not add creativity or variation.";

/// Anchor set for every axis in the rubric. The judge is only
/// permitted to emit these values (enforced by [`JUDGE_JSON_SCHEMA`]),
/// and [`aggregate_self_consistency`] snaps arithmetic means back onto
/// this same set so the output axis values stay inside the rubric's
/// legal range.
const AXIS_ANCHORS: [u8; 5] = [0, 3, 5, 7, 10];

/// Number of self-consistency retry attempts when `claude -p` exits
/// non-zero. Research.md §6 risk 8: 12 calls / scenario at 8 workers
/// edges the Max subscription's rate limit; a small retry loop with
/// exponential backoff eats the transient failures without masking real
/// configuration errors. Backoff delays are in [`RETRY_BACKOFF_MS`].
const RETRY_ATTEMPTS: u32 = 3;

/// Backoff delays between retry attempts, in milliseconds. Three entries
/// matching [`RETRY_ATTEMPTS`]. The pattern is 2s / 5s / 10s per PLAN.md
/// §6 risk 8. The final entry is the ceiling — if three retries all
/// fail, the scenario's `score_scenario` call propagates the error
/// and the runner records it as an unjudged scenario.
const RETRY_BACKOFF_MS: [u64; RETRY_ATTEMPTS as usize] = [2000, 5000, 10000];

/// Produces the fallback grounding block inserted into `build_prompt`
/// for either side when no programmatic grounding scores are available.
/// Exact wording from `docs/benchmark-v2/verifiable-grounding.md` §7.
/// Slice A always emits this fallback; slice B (grounding layer) later
/// replaces the call sites with real verification output.
pub fn grounding_fallback_block(label: &str) -> String {
    format!("Programmatic grounding for {label}:\n  no verifiable claims extracted — score N/A")
}

/// Builds the judge prompt. **Pure function** of the inputs — no env
/// access, no clock reads, no RNG. The ensemble wrapper relies on this
/// to hand byte-identical prompts to both models so that inter-rater
/// statistics are meaningful.
///
/// The `position` parameter controls which answer goes in slot A and
/// which in slot B. The `reference` parameter is the (optional) golden
/// answer injected under `GOLDEN ANSWER`. The `grounding_mcp` /
/// `grounding_non_mcp` strings are inserted between `ANSWERS TO GRADE`
/// and `OUTPUT FORMAT`.
pub fn build_prompt(
    scenario: &ScenarioReport,
    position: Position,
    reference: Option<&str>,
    grounding_mcp: &str,
    grounding_non_mcp: &str,
) -> String {
    let mcp_src = if scenario.mcp.full_output.is_empty() {
        &scenario.mcp.response_preview
    } else {
        &scenario.mcp.full_output
    };
    let non_mcp_src = if scenario.non_mcp.full_output.is_empty() {
        &scenario.non_mcp.response_preview
    } else {
        &scenario.non_mcp.full_output
    };
    let mcp_body = truncate_for_judge(mcp_src);
    let non_mcp_body = truncate_for_judge(non_mcp_src);
    let sim_steps = scenario
        .non_mcp
        .steps
        .as_ref()
        .map(|s| s.join(" → "))
        .unwrap_or_else(|| "-".to_string());

    let mcp_label = format!("MCP (tool={})", scenario.tool);
    let non_mcp_label = "non-MCP (Glob+Grep+Read sim)".to_string();

    let (label_a, label_b, body_a, body_b, grounding_a, grounding_b) = match position {
        Position::McpFirst => (
            mcp_label,
            non_mcp_label,
            mcp_body,
            non_mcp_body,
            grounding_mcp,
            grounding_non_mcp,
        ),
        Position::NonMcpFirst => (
            non_mcp_label,
            mcp_label,
            non_mcp_body,
            mcp_body,
            grounding_non_mcp,
            grounding_mcp,
        ),
    };

    let golden_section = match reference {
        Some(text) => text.to_string(),
        None => "No golden answer was provided for this scenario. Judge against the rubric anchors alone.".to_string(),
    };

    format!(
        "Grade two coding-assistant tool responses for the same task. Score each independently 0-10 on five axes (anchors: 0, 3, 5, 7, 10 only — no interpolation).\n\
         \n\
         TASK: \"{description}\"\n\
         Tool: `{tool}` | Non-MCP workflow: {sim_steps}\n\
         \n\
         RUBRIC\n\
         correctness:  0=refusal/wrong path/fabricated line | 3=right area, factual errors | 5=correct but hedged/missing | 7=all verifiable, minor omissions | 10=matches golden, no hallucinations\n\
         completeness: 0=wrong question/empty | 3=fragment, 3+ more calls needed | 5=main question only, omits secondary | 7=main+most secondary, ≤1 follow-up | 10=full coverage, no follow-up needed\n\
         usability:    0=unparseable wall/dump | 3=extractable but needs re-grep | 5=structured but noisy | 7=directly actionable, no extra calls | 10=maximally actionable with edit context\n\
         groundedness: 0=majority unverifiable | 3=plausible but unverifiable | 5=mostly verifiable, some filler | 7=all specific claims verifiable | 10=every claim tied to repo artifact, zero filler\n\
         conciseness:  0=dominated by irrelevant content | 3=useful but >50%% filler | 5=balanced signal/filler | 7=mostly signal, minor filler | 10=every token earns its place, zero repetition\n\
         \n\
         GOLDEN ANSWER\n\
         {golden_section}\n\
         \n\
         ANSWER A ({label_a}):\n\
         ```\n\
         {body_a}\n\
         ```\n\
         \n\
         ANSWER B ({label_b}):\n\
         ```\n\
         {body_b}\n\
         ```\n\
         \n\
         {grounding_a}\n\
         \n\
         {grounding_b}\n\
         \n\
         Respond with a single JSON object matching the --json-schema. No fences, no prose outside JSON.\n",
        description = scenario.description,
        tool = scenario.tool,
        sim_steps = sim_steps,
        golden_section = golden_section,
        label_a = label_a,
        label_b = label_b,
        body_a = body_a,
        body_b = body_b,
        grounding_a = grounding_a,
        grounding_b = grounding_b,
    )
}

/// Parses a single judge response back into a [`PerRunScores`] with
/// the scores un-swapped to the `(mcp, non_mcp)` orientation based on
/// `position`. The caller is expected to pass the raw `claude -p` stdout;
/// any Markdown fences are defensively stripped via [`strip_fence`] in
/// case the model ignored the prompt's "no fences" rule. Validation
/// rejects any axis score outside [`AXIS_ANCHORS`] — that is a schema
/// violation the model should never emit under `--json-schema` and we
/// surface it as an error rather than silently clamping.
pub fn parse_judge_response(
    raw: &str,
    position: Position,
    run_index: u8,
) -> Result<PerRunScores> {
    let clean = strip_fence(raw);

    #[derive(Deserialize)]
    struct RawAxis {
        correctness: u8,
        completeness: u8,
        usability: u8,
        groundedness: u8,
        conciseness: u8,
    }
    #[derive(Deserialize)]
    struct RawResponse {
        a: RawAxis,
        b: RawAxis,
        verdict: String,
    }

    let raw_resp: RawResponse = serde_json::from_str(clean)
        .with_context(|| format!("parse judge JSON (raw stdout: `{clean}`)"))?;

    let side_from_raw = |r: RawAxis, side_label: &str| -> Result<SideQuality> {
        let q = SideQuality {
            correctness: r.correctness,
            completeness: r.completeness,
            usability: r.usability,
            groundedness: r.groundedness,
            conciseness: r.conciseness,
        };
        validate_axis_anchor(q.correctness, "correctness", side_label)?;
        validate_axis_anchor(q.completeness, "completeness", side_label)?;
        validate_axis_anchor(q.usability, "usability", side_label)?;
        validate_axis_anchor(q.groundedness, "groundedness", side_label)?;
        validate_axis_anchor(q.conciseness, "conciseness", side_label)?;
        Ok(q)
    };

    let a = side_from_raw(raw_resp.a, "a")?;
    let b = side_from_raw(raw_resp.b, "b")?;

    let (mcp, non_mcp) = match position {
        Position::McpFirst => (a, b),
        Position::NonMcpFirst => (b, a),
    };

    Ok(PerRunScores {
        position,
        run_index,
        mcp,
        non_mcp,
        verdict: raw_resp.verdict,
    })
}

fn validate_axis_anchor(value: u8, axis: &str, side: &str) -> Result<()> {
    if AXIS_ANCHORS.contains(&value) {
        Ok(())
    } else {
        anyhow::bail!(
            "judge emitted score {} on axis `{}` (side `{}`); must be one of {:?}",
            value,
            axis,
            side,
            AXIS_ANCHORS
        )
    }
}

/// Snaps a floating-point mean onto the nearest anchor in
/// [`AXIS_ANCHORS`]. Ties are broken toward the lower anchor because the
/// anchor set is not uniformly spaced (`[0, 3, 5, 7, 10]` — gaps of 3, 2,
/// 2, 3), and preferring the lower anchor matches the rubric's strict-
/// grading posture: when in doubt, grade one step down.
fn snap_to_anchor(mean: f64) -> u8 {
    let mut best: u8 = AXIS_ANCHORS[0];
    let mut best_delta = (f64::from(best) - mean).abs();
    for &anchor in AXIS_ANCHORS.iter().skip(1) {
        let delta = (f64::from(anchor) - mean).abs();
        if delta < best_delta {
            best_delta = delta;
            best = anchor;
        }
    }
    best
}

/// Aggregates a set of self-consistency runs into a single per-side
/// pair. The mean is computed per axis in `f64`, then snapped back onto
/// the nearest [`AXIS_ANCHORS`] value so the result stays in the legal
/// rubric range. An empty `runs` slice returns two zero-filled
/// [`SideQuality`] and does not panic — an empty run set is a
/// legitimate state when every `claude -p` call failed and the caller
/// wants a sentinel rather than an `anyhow::bail!` bubbled back up.
pub fn aggregate_self_consistency(runs: &[PerRunScores]) -> (SideQuality, SideQuality) {
    if runs.is_empty() {
        let zero = SideQuality {
            correctness: 0,
            completeness: 0,
            usability: 0,
            groundedness: 0,
            conciseness: 0,
        };
        return (zero, zero);
    }

    let n = runs.len() as f64;
    let mcp_means = [
        runs.iter()
            .map(|r| f64::from(r.mcp.correctness))
            .sum::<f64>()
            / n,
        runs.iter()
            .map(|r| f64::from(r.mcp.completeness))
            .sum::<f64>()
            / n,
        runs.iter().map(|r| f64::from(r.mcp.usability)).sum::<f64>() / n,
        runs.iter()
            .map(|r| f64::from(r.mcp.groundedness))
            .sum::<f64>()
            / n,
        runs.iter()
            .map(|r| f64::from(r.mcp.conciseness))
            .sum::<f64>()
            / n,
    ];
    let non_mcp_means = [
        runs.iter()
            .map(|r| f64::from(r.non_mcp.correctness))
            .sum::<f64>()
            / n,
        runs.iter()
            .map(|r| f64::from(r.non_mcp.completeness))
            .sum::<f64>()
            / n,
        runs.iter()
            .map(|r| f64::from(r.non_mcp.usability))
            .sum::<f64>()
            / n,
        runs.iter()
            .map(|r| f64::from(r.non_mcp.groundedness))
            .sum::<f64>()
            / n,
        runs.iter()
            .map(|r| f64::from(r.non_mcp.conciseness))
            .sum::<f64>()
            / n,
    ];

    let mcp = SideQuality {
        correctness: snap_to_anchor(mcp_means[0]),
        completeness: snap_to_anchor(mcp_means[1]),
        usability: snap_to_anchor(mcp_means[2]),
        groundedness: snap_to_anchor(mcp_means[3]),
        conciseness: snap_to_anchor(mcp_means[4]),
    };
    let non_mcp = SideQuality {
        correctness: snap_to_anchor(non_mcp_means[0]),
        completeness: snap_to_anchor(non_mcp_means[1]),
        usability: snap_to_anchor(non_mcp_means[2]),
        groundedness: snap_to_anchor(non_mcp_means[3]),
        conciseness: snap_to_anchor(non_mcp_means[4]),
    };

    (mcp, non_mcp)
}

/// Tuple of (axis name, getter) used by [`detect_position_bias`] to walk
/// the five axes in a single loop. Spelled out as a type alias so the
/// fixed-size array declaration below stays under clippy's type
/// complexity threshold.
type AxisAccessor = (&'static str, fn(&SideQuality) -> u8);

/// Emits the position-bias flag strings described in
/// `docs/benchmark-v2/judge-core.md` §5.1. For each of the five axes we
/// compute the per-position mean over `runs[i].mcp.axis`, then measure
/// the absolute gap between the two positions. A gap of 7+ is `severe`,
/// 3-6 is `warning`, <3 is silent. Exact thresholds are from the design
/// doc.
pub fn detect_position_bias(runs: &[PerRunScores]) -> Vec<String> {
    if runs.is_empty() {
        return Vec::new();
    }

    let mut flags = Vec::new();

    let axes: [AxisAccessor; 5] = [
        ("correctness", |s| s.correctness),
        ("completeness", |s| s.completeness),
        ("usability", |s| s.usability),
        ("groundedness", |s| s.groundedness),
        ("conciseness", |s| s.conciseness),
    ];

    for (axis_name, getter) in axes {
        let (pass1_sum, pass1_n, pass2_sum, pass2_n) = runs.iter().fold(
            (0.0f64, 0usize, 0.0f64, 0usize),
            |(p1s, p1n, p2s, p2n), run| match run.position {
                Position::McpFirst => (p1s + f64::from(getter(&run.mcp)), p1n + 1, p2s, p2n),
                Position::NonMcpFirst => (p1s, p1n, p2s + f64::from(getter(&run.mcp)), p2n + 1),
            },
        );
        if pass1_n == 0 || pass2_n == 0 {
            continue;
        }
        let pass1_mean = pass1_sum / pass1_n as f64;
        let pass2_mean = pass2_sum / pass2_n as f64;
        let gap = (pass1_mean - pass2_mean).abs();
        if gap >= 7.0 {
            flags.push(format!("position_bias_severe:{axis_name}"));
        } else if gap >= 3.0 {
            flags.push(format!("position_bias_warning:{axis_name}"));
        }
    }

    flags
}

/// Extracts the `structured_output` field from a
/// `claude -p --output-format json` envelope.
///
/// When `--json-schema` is passed to `claude -p`, the CLI validates the
/// model's response against the schema but only surfaces the validated
/// object when `--output-format json` is also set. In that mode stdout
/// is a single top-level envelope of the form
///
/// ```json
/// {
///   "type": "result",
///   "subtype": "success",
///   "is_error": false,
///   "result": "<plain text model reply>",
///   "structured_output": { /* schema-validated JSON */ },
///   "usage": { ... }
/// }
/// ```
///
/// Our callers ([`parse_judge_response`]) expect the inner schema
/// body, not the envelope, so we re-serialize the `structured_output`
/// field and return that. On `is_error: true` or a missing
/// `structured_output` we surface the outer `result` string as error
/// context so the operator can tell a CLI refusal apart from a schema
/// mismatch.
///
/// # Errors
///
/// - The envelope itself is not valid JSON.
/// - The envelope has `is_error: true`.
/// - The envelope lacks a `structured_output` field (usually means the
///   model declined to produce a schema-conformant object and replied
///   only in `result`).
fn extract_structured_output(envelope: &str) -> Result<String> {
    let parsed: serde_json::Value = serde_json::from_str(envelope).with_context(|| {
        format!("parse `claude -p --output-format json` envelope (raw: `{envelope}`)")
    })?;

    if parsed
        .get("is_error")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        let msg = parsed
            .get("result")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("<no result field>");
        anyhow::bail!("claude -p returned is_error=true: {msg}");
    }

    let structured = parsed.get("structured_output").ok_or_else(|| {
        anyhow::anyhow!(
            "claude -p envelope has no `structured_output` field (result: `{}`)",
            parsed
                .get("result")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("<none>")
        )
    })?;

    serde_json::to_string(structured).context("re-serialize structured_output")
}

/// Runs a single `claude -p` call with the structured output flags —
/// `--append-system-prompt <DETERMINISM_SYSTEM_PROMPT>`,
/// `--json-schema <JUDGE_JSON_SCHEMA>`, and `--output-format json`.
/// The `--output-format json` flag is load-bearing: without it the CLI
/// silently discards the `structured_output` side-channel and stdout
/// becomes the model's plain-text chat reply, which under the rubric
/// prompt is empty and breaks [`parse_judge_response`]. Retries up
/// to [`RETRY_ATTEMPTS`] times with the delays in [`RETRY_BACKOFF_MS`]
/// when the subprocess exits non-zero. The final attempt's error is
/// returned.
///
/// Previously this function passed `--effort max` for stronger rubric
/// adherence. PLAN.md §6 risk 8 already noted that "12 calls / scenario
/// at 8 workers edges the Max subscription's rate limit"; the maximum
/// effort knob made every call ~5–15 s slower under that pressure.
/// Under the strict `--json-schema` enum `{0, 3, 5, 7, 10}` the model's
/// output surface is structurally constrained to a tiny number of
/// integers per axis, so the extra thinking budget translates almost
/// entirely into latency rather than rubric-adherence improvements.
/// Dropping `--effort max` roughly halves the per-call wall time on the
/// Max subscription without measurably moving the MCP-vs-non-MCP gap;
/// the Phase 5 self-bench rerun is the verification (PLAN.md §6 open
/// risks item 8).
fn run_judge_subprocess(prompt: &str, model: &str) -> Result<String> {
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 0..RETRY_ATTEMPTS {
        let spawn = Command::new("claude")
            .arg("-p")
            .arg("--model")
            .arg(model)
            .arg("--append-system-prompt")
            .arg(DETERMINISM_SYSTEM_PROMPT)
            .arg("--json-schema")
            .arg(JUDGE_JSON_SCHEMA)
            .arg("--output-format")
            .arg("json")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawn {
            Ok(c) => c,
            Err(e) => {
                last_error = Some(
                    anyhow::Error::new(e)
                        .context("spawn `claude -p` (is Claude Code installed and on PATH?)"),
                );
                if attempt + 1 < RETRY_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(
                        RETRY_BACKOFF_MS[attempt as usize],
                    ));
                }
                continue;
            }
        };

        {
            let stdin = match child.stdin.as_mut() {
                Some(s) => s,
                None => {
                    last_error = Some(anyhow::anyhow!("claude child missing stdin pipe"));
                    if attempt + 1 < RETRY_ATTEMPTS {
                        std::thread::sleep(std::time::Duration::from_millis(
                            RETRY_BACKOFF_MS[attempt as usize],
                        ));
                    }
                    continue;
                }
            };
            if let Err(e) = stdin.write_all(prompt.as_bytes()) {
                last_error = Some(anyhow::Error::new(e).context("write prompt to claude stdin"));
                if attempt + 1 < RETRY_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(
                        RETRY_BACKOFF_MS[attempt as usize],
                    ));
                }
                continue;
            }
        }

        let out = match child.wait_with_output() {
            Ok(out) => out,
            Err(e) => {
                last_error = Some(anyhow::Error::new(e).context("wait on `claude -p`"));
                if attempt + 1 < RETRY_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(
                        RETRY_BACKOFF_MS[attempt as usize],
                    ));
                }
                continue;
            }
        };

        if !out.status.success() {
            last_error = Some(anyhow::anyhow!(
                "`claude -p` exited {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
            if attempt + 1 < RETRY_ATTEMPTS {
                std::thread::sleep(std::time::Duration::from_millis(
                    RETRY_BACKOFF_MS[attempt as usize],
                ));
            }
            continue;
        }

        let stdout = String::from_utf8(out.stdout).context("claude stdout not valid UTF-8")?;
        match extract_structured_output(&stdout) {
            Ok(inner) => return Ok(inner),
            Err(e) => {
                last_error = Some(e);
                if attempt + 1 < RETRY_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(
                        RETRY_BACKOFF_MS[attempt as usize],
                    ));
                }
                continue;
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("claude -p failed after retries")))
}

/// Scores a scenario with the judge protocol: two position passes,
/// `n` self-consistency runs per position, aggregation, and
/// position-bias detection. See `docs/benchmark-v2/judge-core.md` §5 and
/// PLAN.md slice A.
///
/// `grounding_mcp` / `grounding_non_mcp` are the pre-rendered grounding
/// fact-check blocks that [`build_prompt`] embeds between
/// `ANSWERS TO GRADE` and `OUTPUT FORMAT` (PLAN.md §2.3). Callers that
/// have not enabled grounding pass [`grounding_fallback_block`] strings;
/// slice B's runner passes real verification output from
/// `grounding::render_prompt_block`. Both argument strings are ALWAYS
/// named in MCP / non-MCP orientation — [`build_prompt`] un-swaps
/// them internally when `Position::NonMcpFirst` fires.
///
/// # Errors
///
/// - Either side has an error or both sides are empty.
/// - Any of the `2 * n` `claude -p` calls fails after [`RETRY_ATTEMPTS`]
///   retries.
/// - A response fails JSON parse or axis-anchor validation.
pub fn score_scenario(
    scenario: &ScenarioReport,
    model: &str,
    reference: Option<&str>,
    n: usize,
    grounding_mcp: &str,
    grounding_non_mcp: &str,
) -> Result<QualityScores> {
    if scenario.mcp.error.is_some() || scenario.non_mcp.error.is_some() {
        anyhow::bail!(
            "scenario `{}` had a tool error; skipping",
            scenario.scenario_id
        );
    }
    let mcp_has_body =
        !scenario.mcp.full_output.is_empty() || !scenario.mcp.response_preview.is_empty();
    let non_mcp_has_body =
        !scenario.non_mcp.full_output.is_empty() || !scenario.non_mcp.response_preview.is_empty();
    if !mcp_has_body && !non_mcp_has_body {
        anyhow::bail!(
            "scenario `{}` has empty bodies on both sides; nothing to judge",
            scenario.scenario_id
        );
    }

    let prompt_mcp_first = build_prompt(
        scenario,
        Position::McpFirst,
        reference,
        grounding_mcp,
        grounding_non_mcp,
    );
    let prompt_non_mcp_first = build_prompt(
        scenario,
        Position::NonMcpFirst,
        reference,
        grounding_mcp,
        grounding_non_mcp,
    );

    let total_runs = 2 * n;
    let mut runs: Vec<PerRunScores> = Vec::with_capacity(total_runs);
    let mut raw_bodies: Vec<String> = Vec::with_capacity(total_runs);

    for position in [Position::McpFirst, Position::NonMcpFirst] {
        let prompt = match position {
            Position::McpFirst => &prompt_mcp_first,
            Position::NonMcpFirst => &prompt_non_mcp_first,
        };
        for i in 0..n {
            let raw = run_judge_subprocess(prompt, model).with_context(|| {
                format!(
                    "score_scenario `{}` position={:?} run={}",
                    scenario.scenario_id, position, i
                )
            })?;
            let clean = strip_fence(&raw).to_string();
            let parsed = parse_judge_response(&raw, position, i as u8).with_context(|| {
                format!(
                    "parse response for `{}` position={:?} run={}",
                    scenario.scenario_id, position, i
                )
            })?;
            runs.push(parsed);
            raw_bodies.push(clean);
        }
    }

    // SC diversity debug check: if every parsed JSON body is
    // byte-identical across all runs, the self-consistency signal has
    // collapsed to zero. PLAN.md §6 risk 7 requires us to log a warning
    // so the operator knows whether to bump `n` or switch to explicit
    // prompt perturbation.
    if raw_bodies.len() >= 2 && raw_bodies.iter().all(|b| b == &raw_bodies[0]) {
        tracing::warn!(
            "SC collapse on scenario `{}` — all {} runs identical",
            scenario.scenario_id, total_runs
        );
    }

    let (mcp, non_mcp) = aggregate_self_consistency(&runs);
    let flags = detect_position_bias(&runs);
    let verdict = runs
        .iter()
        .find(|r| !r.verdict.is_empty())
        .map(|r| r.verdict.clone())
        .unwrap_or_default();

    Ok(QualityScores {
        mcp,
        non_mcp,
        verdict,
        model: model.to_string(),
        runs,
        flags,
        reference_answer_used: reference.is_some(),
    })
}

// ---------------------------------------------------------------------------
// Judge reuse cache (mirrors `build_non_mcp_cache` pattern in mod.rs).
// ---------------------------------------------------------------------------
//
// `--reuse-judge <path>` lets iterative MCP development re-use a prior
// run's judge verdicts when the scenario's input fingerprint is unchanged.
// First run pays the full `claude -p` cost; subsequent runs while the
// developer tweaks one tool only re-judge the scenario whose input
// actually drifted. The git-SHA gate from `build_non_mcp_cache` is reused
// verbatim so a code change anywhere in the tree invalidates the cache by
// default; pass `--allow-stale-judge-cache` to bypass it for renderer-
// only workflows.

/// Cache key for one scenario in [`build_judge_cache`].
///
/// All five components must match between the cached entry and the
/// freshly-measured scenario for the entry to qualify as a hit. Output
/// hashes are derived from a stable serialized fingerprint
/// (`response_bytes`, `tokens`, `naive_tokens`, `response_preview`)
/// rather than the literal `full_output` field, because `full_output`
/// is `#[serde(skip)]` and does not survive a JSON round-trip — see
/// [`SideReport::full_output`](crate::benchmark::report::SideReport).
/// Two outputs that share the byte count, both token counts, and the
/// 240-byte preview are overwhelmingly likely to be byte-identical, so
/// the proxy is sufficient for cache validation. Grounding scores are
/// stored as `f64::to_bits()` so equality is exact and no epsilon games
/// are needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JudgeCacheKey {
    pub mcp_output_hash: u64,
    pub non_mcp_output_hash: u64,
    pub reference_hash: u64,
    pub mcp_grounding_score: Option<u64>,
    pub non_mcp_grounding_score: Option<u64>,
}

/// One cache entry: the key tying it to a specific scenario fingerprint
/// plus the cached [`QualityScores`] verdict ready to be cloned into
/// `ScenarioReport::quality` on a hit. The map [`build_judge_cache`]
/// returns is keyed by `scenario_id`; the entry's `key` field is the
/// validation fingerprint a current run must match to reuse `scores`.
#[derive(Debug, Clone)]
pub struct CachedJudge {
    pub key: JudgeCacheKey,
    pub scores: QualityScores,
}

/// Hashes a stable fingerprint of one [`SideReport`] for the judge cache
/// key.
///
/// The hash inputs are deliberately limited to fields that survive JSON
/// serialization: `response_bytes`, `tokens`, `naive_tokens`, and the
/// 240-byte `response_preview`. The `full_output` field is
/// `#[serde(skip)]` (PLAN.md §2.5 trade-off — keeps reports under
/// ~200 KB) so a cache loaded from a prior JSON has it empty; hashing it
/// would produce zero on the cached side and a real hash on the current
/// side, missing every entry. Hashing the serialized fingerprint instead
/// makes both sides agree.
///
/// Uses [`std::hash::DefaultHasher`] (SipHash) — strong enough for
/// cache validation, present in `std`, no new crate.
fn hash_side_for_judge_cache(side: &crate::benchmark::report::SideReport) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    side.response_bytes.hash(&mut h);
    side.tokens.hash(&mut h);
    side.naive_tokens.hash(&mut h);
    side.response_preview.hash(&mut h);
    h.finish()
}

/// Hashes a string for the `reference_hash` slot.
///
/// Returns `0` for the `None` case so two scenarios that both lack a
/// reference answer share the same `reference_hash` and qualify as a
/// match. Using a constant sentinel rather than a real hash of the
/// empty string keeps the cache key trivially comparable across the
/// `Some(_)` ↔ `None` boundary.
fn hash_reference_for_judge_cache(reference: Option<&str>) -> u64 {
    use std::hash::{Hash, Hasher};
    let Some(s) = reference else { return 0 };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

impl JudgeCacheKey {
    /// Computes the cache key from a [`ScenarioReport`].
    ///
    /// Used at both cache build time (over the deserialized prior
    /// report) and cache check time (over the freshly-measured current
    /// scenario). Both call sites compute the key from the same fields,
    /// so a hit only fires when the current run's MCP output, non-MCP
    /// output, reference answer, and both grounding scores all match
    /// the cached entry.
    pub fn from_scenario(scenario: &ScenarioReport) -> Self {
        JudgeCacheKey {
            mcp_output_hash: hash_side_for_judge_cache(&scenario.mcp),
            non_mcp_output_hash: hash_side_for_judge_cache(&scenario.non_mcp),
            reference_hash: hash_reference_for_judge_cache(scenario.reference_answer.as_deref()),
            mcp_grounding_score: scenario.mcp.grounding.as_ref().map(|g| g.score.to_bits()),
            non_mcp_grounding_score: scenario
                .non_mcp
                .grounding
                .as_ref()
                .map(|g| g.score.to_bits()),
        }
    }
}

/// Builds a judge reuse cache from a previously-written
/// [`crate::benchmark::report::BenchmarkReport`].
///
/// Walks every scenario that carries `quality: Some(_)`, computes
/// its [`JudgeCacheKey`] from the same fields the current-run check
/// site uses, and stores the pair in a `HashMap<scenario_id,
/// CachedJudge>`. The git-SHA gate is identical to
/// [`crate::benchmark::build_non_mcp_cache`]: when `expected_sha` is
/// `Some` and the cached report's `git_sha` mismatches, an empty map
/// is returned. Pass `allow_stale: true` to bypass the gate; the
/// caller surfaces the corresponding `--allow-stale-judge-cache` flag.
///
/// `allow_stale` also has a second effect: even when the SHA matches
/// (so the gate would have passed anyway) it widens the in-run cache
/// check from "key equality" to "scenario_id equality", letting the
/// cached `quality` flow through regardless of input drift. This is
/// the "render-only" workflow from PLAN.md §6 risk 8: rerun a fresh
/// renderer from cached judge output without paying for a new judge
/// pass.
pub fn build_judge_cache(
    prior: &crate::benchmark::report::BenchmarkReport,
    expected_sha: Option<&str>,
    allow_stale: bool,
) -> std::collections::HashMap<String, CachedJudge> {
    if !allow_stale
        && let (Some(want), Some(have)) = (expected_sha, prior.git_sha.as_deref())
        && want != have
    {
        return std::collections::HashMap::new();
    }
    prior
        .scenarios
        .iter()
        .filter_map(|s| {
            s.quality.as_ref().map(|q| {
                (
                    s.scenario_id.clone(),
                    CachedJudge {
                        key: JudgeCacheKey::from_scenario(s),
                        scores: q.clone(),
                    },
                )
            })
        })
        .collect()
}

/// Returns `Some(scores)` when the cached entry for `scenario` is a
/// reusable hit, otherwise `None`.
///
/// Hit semantics depend on `allow_stale`:
///
/// - `allow_stale = false` — strict mode. The current scenario's
///   [`JudgeCacheKey`] must equal the cached key. Any drift in MCP
///   output, non-MCP output, reference answer, or either grounding
///   score invalidates the entry.
/// - `allow_stale = true` — lenient mode. The cached entry is reused
///   whenever the scenario_id appears in the cache, regardless of key
///   equality. Used for render-only workflows.
pub fn lookup_judge_cache<'a>(
    cache: &'a std::collections::HashMap<String, CachedJudge>,
    scenario: &ScenarioReport,
    allow_stale: bool,
) -> Option<&'a QualityScores> {
    let entry = cache.get(&scenario.scenario_id)?;
    if allow_stale {
        return Some(&entry.scores);
    }
    let current = JudgeCacheKey::from_scenario(scenario);
    if current == entry.key {
        Some(&entry.scores)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Judge ensemble wrapper.
// ---------------------------------------------------------------------------
//
// Wraps two parallel judge calls (primary + secondary) with a DAFE-style
// arbiter escalation on disagreement. Cohen's weighted kappa and
// Krippendorff's alpha are hand-rolled — no new crates.

/// Output of the two-judge ensemble pipeline. Carries both independent
/// ratings, an optional arbiter verdict on disagreement, the final
/// aggregated score that drives the Matrix column, and the per-axis
/// absolute deltas used for the disagreement gate and the "Top 3
/// most-disputed scenarios" Markdown table.
///
/// `abs_delta_per_axis` is ten floats — five axes for the MCP side
/// followed by five axes for the non-MCP side, in the canonical axis
/// order: correctness, completeness, usability, groundedness,
/// conciseness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsembleQualityScores {
    pub primary: QualityScores,
    pub secondary: QualityScores,
    pub arbiter: Option<QualityScores>,
    pub final_score: QualityScores,
    pub agreement: bool,
    pub abs_delta_per_axis: Vec<f64>,
}

/// Axis names emitted for the "Top 3 most-disputed scenarios" table.
/// Walks in the canonical five-axis order for the MCP side, then again
/// for the non-MCP side — matches the layout produced by
/// [`abs_delta_per_axis`] so index `i` maps to `AXIS_NAMES_TEN[i]`.
pub const AXIS_NAMES_TEN: [&str; 10] = [
    "correctness (MCP)",
    "completeness (MCP)",
    "usability (MCP)",
    "groundedness (MCP)",
    "conciseness (MCP)",
    "correctness (non-MCP)",
    "completeness (non-MCP)",
    "usability (non-MCP)",
    "groundedness (non-MCP)",
    "conciseness (non-MCP)",
];

/// Per-axis absolute delta between two [`SideQuality`] values. Returns
/// five floats in the canonical axis order. Used internally by
/// [`abs_delta_per_axis`] to assemble the full ten-axis delta.
pub fn abs_delta_per_axis_side(a: &SideQuality, b: &SideQuality) -> Vec<f64> {
    vec![
        (f64::from(a.correctness) - f64::from(b.correctness)).abs(),
        (f64::from(a.completeness) - f64::from(b.completeness)).abs(),
        (f64::from(a.usability) - f64::from(b.usability)).abs(),
        (f64::from(a.groundedness) - f64::from(b.groundedness)).abs(),
        (f64::from(a.conciseness) - f64::from(b.conciseness)).abs(),
    ]
}

/// Per-axis absolute delta across both sides of an ensemble pair. Returns
/// ten floats — five MCP axes followed by five non-MCP axes, matching
/// [`AXIS_NAMES_TEN`]. This is the shape consumed by the disagreement
/// gate in [`score_ensemble_scenario`] and by the "Top 3 most-disputed"
/// Markdown table in the renderer.
///
/// Concatenating both sides (rather than only the MCP side) matches
/// `docs/benchmark-v2/ensemble-and-agreement.md` §1.a: the arbiter should
/// fire when EITHER side sees per-axis disagreement above the threshold.
pub fn abs_delta_per_axis(a: &QualityScores, b: &QualityScores) -> Vec<f64> {
    let mut out = abs_delta_per_axis_side(&a.mcp, &b.mcp);
    out.extend(abs_delta_per_axis_side(&a.non_mcp, &b.non_mcp));
    out
}

/// Per-axis elementwise mean of two [`SideQuality`] values, snapped
/// back onto the nearest anchor in [`AXIS_ANCHORS`]. Used when the
/// primary and secondary judges agree (`abs_delta <= τ`) to produce a
/// single final `SideQuality` for the Matrix column. Reuses the
/// slice-A private [`snap_to_anchor`] so the rounding rule is identical
/// across both call sites.
pub fn elementwise_mean_side(a: &SideQuality, b: &SideQuality) -> SideQuality {
    let mean = |x: u8, y: u8| (f64::from(x) + f64::from(y)) / 2.0;
    SideQuality {
        correctness: snap_to_anchor(mean(a.correctness, b.correctness)),
        completeness: snap_to_anchor(mean(a.completeness, b.completeness)),
        usability: snap_to_anchor(mean(a.usability, b.usability)),
        groundedness: snap_to_anchor(mean(a.groundedness, b.groundedness)),
        conciseness: snap_to_anchor(mean(a.conciseness, b.conciseness)),
    }
}

/// Elementwise mean of two [`QualityScores`] values. Per-side scores
/// go through [`elementwise_mean_side`]; the verdict is taken from
/// `a` when non-empty, else from `b`; `model` advertises both judges.
/// The merged `runs` list concatenates both inputs so downstream
/// consumers see every position-pass / self-consistency vote that fed
/// into the final agreement score.
pub fn elementwise_mean_quality(a: &QualityScores, b: &QualityScores) -> QualityScores {
    let mut runs = a.runs.clone();
    runs.extend(b.runs.iter().cloned());
    let mut flags = a.flags.clone();
    for f in &b.flags {
        if !flags.contains(f) {
            flags.push(f.clone());
        }
    }
    QualityScores {
        mcp: elementwise_mean_side(&a.mcp, &b.mcp),
        non_mcp: elementwise_mean_side(&a.non_mcp, &b.non_mcp),
        verdict: if a.verdict.is_empty() {
            b.verdict.clone()
        } else {
            a.verdict.clone()
        },
        model: format!("{} + {}", a.model, b.model),
        runs,
        flags,
        reference_answer_used: a.reference_answer_used || b.reference_answer_used,
    }
}

/// Formats one [`SideQuality`] as a single line for the
/// `PRIOR_RATINGS:` block. Keeps the exact axis order and field names
/// from `docs/benchmark-v2/ensemble-and-agreement.md` §8 so the arbiter
/// prompt is reproducible byte-for-byte.
fn format_side_prior(side: &SideQuality) -> String {
    format!(
        "correctness={}, completeness={}, usability={}, groundedness={}, conciseness={}",
        side.correctness, side.completeness, side.usability, side.groundedness, side.conciseness
    )
}

/// Appends a `PRIOR_RATINGS:` block to a judge prompt, embedding the
/// primary and secondary judges' per-side scores and verdict strings so
/// the arbiter tie-breaks with full context. Exact block shape is
/// specified in `docs/benchmark-v2/ensemble-and-agreement.md` §8; Phase
/// 4 lifts it verbatim. No surgery on [`build_prompt`] — the prior
/// block is informational and lives after `OUTPUT FORMAT`.
pub fn append_prior_ratings(
    prompt: &str,
    primary: &QualityScores,
    secondary: &QualityScores,
) -> String {
    format!(
        "{prompt}\nPRIOR_RATINGS:\n\
         Primary (model={primary_model}):\n  \
         MCP:     {primary_mcp}\n  \
         non-MCP: {primary_non}\n  \
         verdict: \"{primary_verdict}\"\n\
         Secondary (model={secondary_model}):\n  \
         MCP:     {secondary_mcp}\n  \
         non-MCP: {secondary_non}\n  \
         verdict: \"{secondary_verdict}\"\n\
         The two judges disagree. Re-rate both answers using the same rubric.\n",
        primary_model = primary.model,
        primary_mcp = format_side_prior(&primary.mcp),
        primary_non = format_side_prior(&primary.non_mcp),
        primary_verdict = primary.verdict,
        secondary_model = secondary.model,
        secondary_mcp = format_side_prior(&secondary.mcp),
        secondary_non = format_side_prior(&secondary.non_mcp),
        secondary_verdict = secondary.verdict,
    )
}

/// Runs the arbiter for one scenario. Called by [`score_ensemble_scenario`]
/// when the primary/secondary disagreement crosses the per-axis threshold.
/// Issues a single `claude -p` call per position pass (n=1, no self-
/// consistency) with the same schema / effort / determinism flags as
/// slice A. Rationale per `docs/benchmark-v2/ensemble-and-agreement.md`
/// §1.a: the arbiter's job is a tie-break, and the self-consistency
/// variance-reduction signal is already baked into `primary` and
/// `secondary`, so running SC on the arbiter would buy little at
/// meaningful cost.
#[allow(
    clippy::too_many_arguments,
    reason = "dedicated arbiter entry point; flattening would require a new context struct that is only used here"
)]
fn score_arbiter_scenario(
    scenario: &ScenarioReport,
    arbiter_model: &str,
    reference: Option<&str>,
    primary: &QualityScores,
    secondary: &QualityScores,
    grounding_mcp: &str,
    grounding_non_mcp: &str,
) -> Result<QualityScores> {
    let base_mcp_first = build_prompt(
        scenario,
        Position::McpFirst,
        reference,
        grounding_mcp,
        grounding_non_mcp,
    );
    let base_non_mcp_first = build_prompt(
        scenario,
        Position::NonMcpFirst,
        reference,
        grounding_mcp,
        grounding_non_mcp,
    );
    let prompt_mcp_first = append_prior_ratings(&base_mcp_first, primary, secondary);
    let prompt_non_mcp_first = append_prior_ratings(&base_non_mcp_first, primary, secondary);

    let mut runs: Vec<PerRunScores> = Vec::with_capacity(2);

    for position in [Position::McpFirst, Position::NonMcpFirst] {
        let prompt = match position {
            Position::McpFirst => &prompt_mcp_first,
            Position::NonMcpFirst => &prompt_non_mcp_first,
        };
        let raw = run_judge_subprocess(prompt, arbiter_model).with_context(|| {
            format!(
                "arbiter scenario `{}` position={:?}",
                scenario.scenario_id, position
            )
        })?;
        let parsed = parse_judge_response(&raw, position, 0).with_context(|| {
            format!(
                "arbiter parse response for `{}` position={:?}",
                scenario.scenario_id, position
            )
        })?;
        runs.push(parsed);
    }

    let (mcp, non_mcp) = aggregate_self_consistency(&runs);
    let flags = detect_position_bias(&runs);
    let verdict = runs
        .iter()
        .find(|r| !r.verdict.is_empty())
        .map(|r| r.verdict.clone())
        .unwrap_or_default();

    Ok(QualityScores {
        mcp,
        non_mcp,
        verdict,
        model: arbiter_model.to_string(),
        runs,
        flags,
        reference_answer_used: reference.is_some(),
    })
}

/// Scores a scenario with the two-judge ensemble pipeline. Primary and
/// secondary both run the full slice-A protocol (position swap × SC n);
/// on disagreement the arbiter fires with n=1 per position per §1.a of
/// the ensemble design doc.
///
/// Returns the full [`EnsembleQualityScores`] with `final_score` set to
/// either the elementwise mean of primary and secondary (agreement) or
/// the arbiter's scores (disagreement). The arbiter result is NOT
/// averaged with primary and secondary — per
/// `docs/benchmark-v2/ensemble-and-agreement.md` §1 the arbiter is a
/// tie-break by construction and averaging would re-introduce the bias
/// it is meant to resolve.
///
/// # Errors
///
/// Propagated from the underlying [`score_scenario`] and
/// [`score_arbiter_scenario`] calls. Any subprocess spawn failure,
/// non-zero exit, schema-violation parse error, or tool-error side
/// gate terminates the scenario's ensemble scoring.
#[allow(
    clippy::too_many_arguments,
    reason = "dedicated ensemble entry point; flattening would require a new config struct that is only used here"
)]
pub fn score_ensemble_scenario(
    scenario: &ScenarioReport,
    primary_model: &str,
    secondary_model: &str,
    arbiter_model: &str,
    reference: Option<&str>,
    n: usize,
    disagreement_threshold: f64,
    grounding_mcp: &str,
    grounding_non_mcp: &str,
) -> Result<EnsembleQualityScores> {
    let primary = score_scenario(
        scenario,
        primary_model,
        reference,
        n,
        grounding_mcp,
        grounding_non_mcp,
    )
    .with_context(|| {
        format!(
            "score_ensemble_scenario primary `{}` model={}",
            scenario.scenario_id, primary_model
        )
    })?;
    let secondary = score_scenario(
        scenario,
        secondary_model,
        reference,
        n,
        grounding_mcp,
        grounding_non_mcp,
    )
    .with_context(|| {
        format!(
            "score_ensemble_scenario secondary `{}` model={}",
            scenario.scenario_id, secondary_model
        )
    })?;

    let delta = abs_delta_per_axis(&primary, &secondary);
    let max_delta = delta.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let agreement = max_delta.is_finite() && max_delta <= disagreement_threshold;

    let (arbiter, final_score, agreement_flag) = if agreement {
        (None, elementwise_mean_quality(&primary, &secondary), true)
    } else {
        let arb = score_arbiter_scenario(
            scenario,
            arbiter_model,
            reference,
            &primary,
            &secondary,
            grounding_mcp,
            grounding_non_mcp,
        )
        .with_context(|| {
            format!(
                "score_ensemble_scenario arbiter `{}` model={}",
                scenario.scenario_id, arbiter_model
            )
        })?;
        let final_score = arb.clone();
        (Some(arb), final_score, false)
    };

    Ok(EnsembleQualityScores {
        primary,
        secondary,
        arbiter,
        final_score,
        agreement: agreement_flag,
        abs_delta_per_axis: delta,
    })
}

/// Variant of [`score_ensemble_scenario`] that accepts a pre-computed
/// primary [`QualityScores`] instead of running it fresh.
///
/// Used by the ensemble runner when `--reuse-judge` produces a cache
/// hit on the primary judge: the cached verdict drops in for the
/// primary slot, and only the secondary (and optional arbiter) pay the
/// `claude -p` cost. The secondary is intentionally NOT cached — it
/// runs every time so the Cohen's κ measurement and the disagreement
/// gate stay honest. The arbiter still fires on disagreement above
/// `disagreement_threshold`.
///
/// All other behavior is identical to [`score_ensemble_scenario`].
///
/// # Errors
///
/// Propagated from the underlying [`score_scenario`] (secondary)
/// and [`score_arbiter_scenario`] calls.
#[allow(
    clippy::too_many_arguments,
    reason = "ensemble fast-path; flattening would require a config struct used only here"
)]
pub fn score_ensemble_scenario_with_primary(
    scenario: &ScenarioReport,
    primary: QualityScores,
    secondary_model: &str,
    arbiter_model: &str,
    reference: Option<&str>,
    n: usize,
    disagreement_threshold: f64,
    grounding_mcp: &str,
    grounding_non_mcp: &str,
) -> Result<EnsembleQualityScores> {
    let secondary = score_scenario(
        scenario,
        secondary_model,
        reference,
        n,
        grounding_mcp,
        grounding_non_mcp,
    )
    .with_context(|| {
        format!(
            "score_ensemble_scenario_with_primary secondary `{}` model={}",
            scenario.scenario_id, secondary_model
        )
    })?;

    let delta = abs_delta_per_axis(&primary, &secondary);
    let max_delta = delta.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let agreement = max_delta.is_finite() && max_delta <= disagreement_threshold;

    let (arbiter, final_score, agreement_flag) = if agreement {
        (None, elementwise_mean_quality(&primary, &secondary), true)
    } else {
        let arb = score_arbiter_scenario(
            scenario,
            arbiter_model,
            reference,
            &primary,
            &secondary,
            grounding_mcp,
            grounding_non_mcp,
        )
        .with_context(|| {
            format!(
                "score_ensemble_scenario_with_primary arbiter `{}` model={}",
                scenario.scenario_id, arbiter_model
            )
        })?;
        let final_score = arb.clone();
        (Some(arb), final_score, false)
    };

    Ok(EnsembleQualityScores {
        primary,
        secondary,
        arbiter,
        final_score,
        agreement: agreement_flag,
        abs_delta_per_axis: delta,
    })
}

// ---------------------------------------------------------------------------
// Inter-rater agreement math (hand-rolled per PLAN.md §5 rule 5).
// ---------------------------------------------------------------------------

/// Cohen's weighted kappa (quadratic weights) for two raters on an
/// ordinal `k`-category scale. Takes a slice of `(rater_a, rater_b)`
/// pairs where both rater values are already bucketed into `0..k`.
/// Returns κ_w in `[-1.0, 1.0]` with the Landis-Koch interpretation
/// bands applied by the renderer.
///
/// Formula per `docs/benchmark-v2/ensemble-and-agreement.md` §3.a:
/// quadratic weights `w[i][j] = 1 - (i - j)^2 / (k - 1)^2`, observed
/// agreement `p_o = Σ w[i][j] * O[i][j] / N`, expected agreement
/// `p_e = Σ w[i][j] * (n_i * n_j) / N^2`,
/// `κ_w = (p_o - p_e) / (1 - p_e)`.
///
/// Edge cases:
///   * `pairs.len() < 2` → [`f64::NAN`] (kappa undefined).
///   * `k < 2` → [`f64::NAN`] (kappa undefined).
///   * `p_e == 1.0` (both raters scored every pair identically) → `1.0`.
///   * Any out-of-range rating (value >= `k`) → [`f64::NAN`] rather
///     than panicking.
pub fn cohens_weighted_kappa(pairs: &[(u8, u8)], k: usize) -> f64 {
    if pairs.len() < 2 || k < 2 {
        return f64::NAN;
    }
    for &(a, b) in pairs {
        if (a as usize) >= k || (b as usize) >= k {
            return f64::NAN;
        }
    }

    let mut observed: Vec<Vec<u32>> = vec![vec![0; k]; k];
    for &(a, b) in pairs {
        observed[a as usize][b as usize] += 1;
    }
    let n_total: f64 = pairs.len() as f64;

    let mut row_totals = vec![0u32; k];
    let mut col_totals = vec![0u32; k];
    for i in 0..k {
        for j in 0..k {
            row_totals[i] += observed[i][j];
            col_totals[j] += observed[i][j];
        }
    }

    let denom = ((k - 1) * (k - 1)) as f64;
    let weight = |i: usize, j: usize| -> f64 {
        let d = (i as f64) - (j as f64);
        1.0 - (d * d) / denom
    };

    let mut p_o = 0.0;
    let mut p_e = 0.0;
    for i in 0..k {
        for j in 0..k {
            let w = weight(i, j);
            p_o += w * f64::from(observed[i][j]);
            p_e += w * f64::from(row_totals[i]) * f64::from(col_totals[j]);
        }
    }
    p_o /= n_total;
    p_e /= n_total * n_total;

    if (1.0 - p_e).abs() < f64::EPSILON {
        return 1.0;
    }
    (p_o - p_e) / (1.0 - p_e)
}

/// Krippendorff's alpha (interval metric) for n raters with possible
/// missing values. Each `units[u]` is a `Vec<Option<f64>>` where the
/// outer index enumerates units (scenarios × axes) and the inner index
/// enumerates raters. `None` marks a missing rating — used for the
/// arbiter-only subset where the arbiter rated disputed scenarios only.
///
/// Formula per `docs/benchmark-v2/ensemble-and-agreement.md` §3.b:
/// interval metric (squared differences), unit-level observed
/// disagreement normalized by `n_u - 1`, expected disagreement from
/// the flattened non-missing pool.
///
/// Returns `None` when fewer than three non-missing ratings are
/// available in total (the metric is unstable below that), or when the
/// expected disagreement is zero.
pub fn krippendorff_alpha_interval(units: &[Vec<Option<f64>>]) -> Option<f64> {
    let mut pool: Vec<f64> = Vec::new();
    for unit in units {
        for x in unit.iter().flatten() {
            pool.push(*x);
        }
    }
    if pool.len() < 3 {
        return None;
    }

    let mut d_o_num = 0.0f64;
    let mut d_o_den = 0.0f64;
    for unit in units {
        let present: Vec<f64> = unit.iter().filter_map(|v| *v).collect();
        let n_u = present.len();
        if n_u < 2 {
            continue;
        }
        let pair_weight = (n_u - 1) as f64;
        for i in 0..n_u {
            for j in (i + 1)..n_u {
                let d = present[i] - present[j];
                d_o_num += d * d / pair_weight;
            }
        }
        d_o_den += (n_u * (n_u - 1)) as f64 / 2.0;
    }
    if d_o_den == 0.0 {
        return None;
    }
    let d_o = d_o_num / d_o_den;

    let n_pool = pool.len();
    let pool_pair_count = (n_pool * (n_pool - 1)) as f64;
    if pool_pair_count == 0.0 {
        return None;
    }
    let mut d_e = 0.0f64;
    for i in 0..n_pool {
        for j in 0..n_pool {
            if i == j {
                continue;
            }
            let d = pool[i] - pool[j];
            d_e += d * d;
        }
    }
    d_e /= pool_pair_count;

    if d_e == 0.0 {
        return None;
    }
    Some(1.0 - d_o / d_e)
}

// ---------------------------------------------------------------------------
// Batch judge — single LLM call for all scenarios
// ---------------------------------------------------------------------------

/// JSON schema for the batch judge response. Only 2 LLM-scored axes
/// (correctness, usability); the other 3 are computed programmatically.
pub const JUDGE_BATCH_JSON_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["scores"],
  "properties": {
    "scores": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "mcp", "non_mcp", "verdict"],
        "properties": {
          "id": {"type": "string"},
          "mcp": {
            "type": "object",
            "additionalProperties": false,
            "required": ["correctness", "usability"],
            "properties": {
              "correctness": {"type":"integer","enum":[0,3,5,7,10]},
              "usability":   {"type":"integer","enum":[0,3,5,7,10]}
            }
          },
          "non_mcp": {
            "type": "object",
            "additionalProperties": false,
            "required": ["correctness", "usability"],
            "properties": {
              "correctness": {"type":"integer","enum":[0,3,5,7,10]},
              "usability":   {"type":"integer","enum":[0,3,5,7,10]}
            }
          },
          "verdict": {"type": "string", "maxLength": 120}
        }
      }
    }
  }
}"#;

/// Programmatic scores for axes not scored by the LLM in batch mode.
#[derive(Debug, Clone)]
pub struct ProgrammaticScores {
    pub conciseness: u8,
    pub completeness: u8,
    pub groundedness: u8,
}

/// Computes the three programmatic axes (conciseness, completeness,
/// groundedness) from scenario data, returning `(mcp_scores, non_mcp_scores)`.
pub fn compute_programmatic_axes(
    scenario: &ScenarioReport,
) -> (ProgrammaticScores, ProgrammaticScores) {
    let savings_pct = scenario.savings.tokens_pct;

    // Conciseness: derived from token savings percentage.
    let (mcp_conciseness, non_mcp_conciseness) = if savings_pct >= 80.0 {
        (10, 3)
    } else if savings_pct >= 50.0 {
        (7, 5)
    } else if savings_pct >= 20.0 {
        (5, 5)
    } else if savings_pct >= 0.0 {
        (7, 7)
    } else {
        (3, 7)
    };

    // Completeness: derived from set_comparison recall.
    let recall = scenario
        .set_comparison
        .as_ref()
        .map(|sc| sc.recall);
    let completeness_score = match recall {
        Some(r) if r >= 0.95 => 10,
        Some(r) if r >= 0.8 => 7,
        Some(r) if r >= 0.5 => 5,
        Some(r) if r >= 0.3 => 3,
        Some(_) => 0,
        None => 7,
    };

    // Groundedness: derived from grounding.score.
    let mcp_groundedness = match scenario.mcp.grounding.as_ref() {
        Some(g) if g.score >= 0.95 => 10,
        Some(g) if g.score >= 0.8 => 7,
        Some(g) if g.score >= 0.5 => 5,
        Some(_) => 3,
        None => 7,
    };
    let non_mcp_groundedness = match scenario.non_mcp.grounding.as_ref() {
        Some(g) if g.score >= 0.95 => 10,
        Some(g) if g.score >= 0.8 => 7,
        Some(g) if g.score >= 0.5 => 5,
        Some(_) => 3,
        None => 7,
    };

    (
        ProgrammaticScores {
            conciseness: mcp_conciseness,
            completeness: completeness_score,
            groundedness: mcp_groundedness,
        },
        ProgrammaticScores {
            conciseness: non_mcp_conciseness,
            completeness: completeness_score,
            groundedness: non_mcp_groundedness,
        },
    )
}

/// Builds the batch prompt containing all scenarios for a single LLM call.
pub fn build_batch_prompt(scenarios: &[&ScenarioReport]) -> String {
    let mut prompt = String::from(
        "Grade pairs of coding-assistant tool responses. \
         For each scenario, score both sides 0-10 on two axes \
         (anchors: 0, 3, 5, 7, 10 only).\n\n\
         RUBRIC\n\
         correctness: 0=wrong/fabricated | 3=right area, errors | \
         5=correct but incomplete | 7=all verifiable | 10=perfect\n\
         usability:   0=unparseable | 3=needs re-grep | \
         5=structured but noisy | 7=directly actionable | 10=maximally actionable\n\n",
    );

    for (i, scenario) in scenarios.iter().enumerate() {
        let mcp_src = if scenario.mcp.full_output.is_empty() {
            &scenario.mcp.response_preview
        } else {
            &scenario.mcp.full_output
        };
        let non_mcp_src = if scenario.non_mcp.full_output.is_empty() {
            &scenario.non_mcp.response_preview
        } else {
            &scenario.non_mcp.full_output
        };
        let mcp_body = truncate_for_judge(mcp_src);
        let non_mcp_body = truncate_for_judge(non_mcp_src);

        prompt.push_str(&format!(
            "---\nSCENARIO {n}: \"{desc}\" [id={id}]\n\
             Tool: `{tool}`\n\n\
             MCP output:\n```\n{mcp}\n```\n\n\
             Non-MCP output:\n```\n{non_mcp}\n```\n\n",
            n = i + 1,
            desc = scenario.description,
            id = scenario.scenario_id,
            tool = scenario.tool,
            mcp = mcp_body,
            non_mcp = non_mcp_body,
        ));
    }

    prompt.push_str(
        "Respond with JSON matching the schema. \
         Array order must match scenario order above.\n",
    );
    prompt
}

/// Raw per-scenario entry from the batch judge response.
#[derive(Debug, serde::Deserialize)]
#[expect(dead_code, reason = "id is part of the JSON schema contract for traceability")]
struct BatchScoreEntry {
    id: String,
    mcp: BatchSideEntry,
    non_mcp: BatchSideEntry,
    verdict: String,
}

/// Raw per-side scores from the batch judge response.
#[derive(Debug, serde::Deserialize)]
struct BatchSideEntry {
    correctness: u8,
    usability: u8,
}

/// Wrapper for the batch response.
#[derive(Debug, serde::Deserialize)]
struct BatchResponse {
    scores: Vec<BatchScoreEntry>,
}

/// Runs the batch judge subprocess with the batch JSON schema.
fn run_batch_judge_subprocess(prompt: &str, model: &str) -> Result<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 0..RETRY_ATTEMPTS {
        let spawn = Command::new("claude")
            .arg("-p")
            .arg("--model")
            .arg(model)
            .arg("--append-system-prompt")
            .arg(DETERMINISM_SYSTEM_PROMPT)
            .arg("--json-schema")
            .arg(JUDGE_BATCH_JSON_SCHEMA)
            .arg("--output-format")
            .arg("json")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawn {
            Ok(c) => c,
            Err(e) => {
                last_error = Some(
                    anyhow::Error::new(e)
                        .context("spawn `claude -p` for batch judge"),
                );
                if attempt + 1 < RETRY_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(
                        RETRY_BACKOFF_MS[attempt as usize],
                    ));
                }
                continue;
            }
        };

        {
            let stdin = match child.stdin.as_mut() {
                Some(s) => s,
                None => {
                    last_error = Some(anyhow::anyhow!("claude child missing stdin pipe"));
                    continue;
                }
            };
            if let Err(e) = stdin.write_all(prompt.as_bytes()) {
                last_error = Some(anyhow::Error::new(e).context("write batch prompt to stdin"));
                continue;
            }
        }

        let out = match child.wait_with_output() {
            Ok(out) => out,
            Err(e) => {
                last_error = Some(anyhow::Error::new(e).context("wait on batch `claude -p`"));
                continue;
            }
        };

        if !out.status.success() {
            last_error = Some(anyhow::anyhow!(
                "batch `claude -p` exited {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
            if attempt + 1 < RETRY_ATTEMPTS {
                std::thread::sleep(std::time::Duration::from_millis(
                    RETRY_BACKOFF_MS[attempt as usize],
                ));
            }
            continue;
        }

        let stdout = String::from_utf8(out.stdout).context("claude stdout not valid UTF-8")?;
        match extract_structured_output(&stdout) {
            Ok(inner) => return Ok(inner),
            Err(e) => {
                last_error = Some(e);
                if attempt + 1 < RETRY_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(
                        RETRY_BACKOFF_MS[attempt as usize],
                    ));
                }
                continue;
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("batch claude -p failed after retries")))
}

/// Scores all scenarios in a single batch LLM call. Merges the 2
/// LLM-scored axes (correctness, usability) with 3 programmatic
/// axes (conciseness, completeness, groundedness) into full
/// [`QualityScores`].
pub fn score_batch(
    scenarios: &[&ScenarioReport],
    model: &str,
) -> Result<Vec<QualityScores>> {
    if scenarios.is_empty() {
        return Ok(Vec::new());
    }

    // Filter out scenarios with errors or empty bodies.
    let valid_indices: Vec<usize> = scenarios
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            s.mcp.error.is_none()
                && s.non_mcp.error.is_none()
                && (!s.mcp.full_output.is_empty()
                    || !s.mcp.response_preview.is_empty()
                    || !s.non_mcp.full_output.is_empty()
                    || !s.non_mcp.response_preview.is_empty())
        })
        .map(|(i, _)| i)
        .collect();

    let valid_scenarios: Vec<&ScenarioReport> =
        valid_indices.iter().map(|&i| scenarios[i]).collect();

    // Build and execute the batch prompt.
    let prompt = build_batch_prompt(&valid_scenarios);
    let raw = run_batch_judge_subprocess(&prompt, model)?;

    let batch: BatchResponse = serde_json::from_str(strip_fence(&raw))
        .with_context(|| format!("parse batch judge response: {raw}"))?;

    if batch.scores.len() != valid_scenarios.len() {
        anyhow::bail!(
            "batch judge returned {} entries but expected {}",
            batch.scores.len(),
            valid_scenarios.len(),
        );
    }

    // Build results for all scenarios (including skipped ones).
    let mut results: Vec<QualityScores> = Vec::with_capacity(scenarios.len());
    let mut valid_iter = batch.scores.into_iter();

    for (i, scenario) in scenarios.iter().enumerate() {
        if valid_indices.contains(&i) {
            let entry = valid_iter.next().expect("valid_iter length mismatch");
            let (prog_mcp, prog_non_mcp) = compute_programmatic_axes(scenario);

            results.push(QualityScores {
                mcp: SideQuality {
                    correctness: entry.mcp.correctness,
                    completeness: prog_mcp.completeness,
                    usability: entry.mcp.usability,
                    groundedness: prog_mcp.groundedness,
                    conciseness: prog_mcp.conciseness,
                },
                non_mcp: SideQuality {
                    correctness: entry.non_mcp.correctness,
                    completeness: prog_non_mcp.completeness,
                    usability: entry.non_mcp.usability,
                    groundedness: prog_non_mcp.groundedness,
                    conciseness: prog_non_mcp.conciseness,
                },
                verdict: entry.verdict,
                model: model.to_string(),
                runs: Vec::new(),
                flags: vec!["batch".to_string()],
                reference_answer_used: false,
            });
        } else {
            // Skipped scenario — zero out LLM-judged axes so they don't
            // inflate aggregate metrics. Programmatic axes are still valid.
            let (prog_mcp, prog_non_mcp) = compute_programmatic_axes(scenario);
            results.push(QualityScores {
                mcp: SideQuality {
                    correctness: 0,
                    completeness: prog_mcp.completeness,
                    usability: 0,
                    groundedness: prog_mcp.groundedness,
                    conciseness: prog_mcp.conciseness,
                },
                non_mcp: SideQuality {
                    correctness: 0,
                    completeness: prog_non_mcp.completeness,
                    usability: 0,
                    groundedness: prog_non_mcp.groundedness,
                    conciseness: prog_non_mcp.conciseness,
                },
                verdict: "skipped (error or empty)".to_string(),
                model: model.to_string(),
                runs: Vec::new(),
                flags: vec!["batch-skipped".to_string()],
                reference_answer_used: false,
            });
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::report::{LatencyStats, Savings, SideReport, Verdict};

    #[test]
    fn side_quality_average_five_axes() {
        let q = SideQuality {
            correctness: 10,
            completeness: 10,
            usability: 10,
            groundedness: 10,
            conciseness: 10,
        };
        assert!((q.average() - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn strip_fence_handles_plain_fence() {
        let raw = "```\n{\"a\":1}\n```";
        assert_eq!(strip_fence(raw), "{\"a\":1}");
    }

    #[test]
    fn strip_fence_handles_json_fence() {
        let raw = "```json\n{\"a\":1}\n```";
        assert_eq!(strip_fence(raw), "{\"a\":1}");
    }

    #[test]
    fn strip_fence_passes_through_plain_json() {
        let raw = "  {\"a\":1}\n";
        assert_eq!(strip_fence(raw), "{\"a\":1}");
    }

    #[test]
    fn truncate_for_judge_respects_cap() {
        let s = "x".repeat(MAX_BODY_BYTES + 500);
        let out = truncate_for_judge(&s);
        assert!(out.len() < s.len());
        assert!(out.contains("[...truncated"));
    }

    #[test]
    fn truncate_for_judge_skips_short_input() {
        let s = "hello";
        assert_eq!(truncate_for_judge(s), "hello");
    }

    // -- test helpers -----------------------------------------------------

    fn make_side_report(body: &str) -> SideReport {
        SideReport {
            response_bytes: body.len(),
            response_preview: body.chars().take(80).collect(),
            tokens: 0,
            naive_tokens: 0,
            latency: LatencyStats {
                mean_us: 0.0,
                stdev_us: 0.0,
                p50_us: 0.0,
                p95_us: 0.0,
                samples: 0,
            },
            error: None,
            args: None,
            steps: Some(vec!["Glob **/*.rs".to_string()]),
            reused: false,
            full_output: body.to_string(),
            grounding: None,
        }
    }

    fn make_scenario(mcp_body: &str, non_mcp_body: &str) -> ScenarioReport {
        ScenarioReport {
            tool: "qartez_find".to_string(),
            scenario_id: "test_scenario".to_string(),
            description: "Locate the Config struct.".to_string(),
            mcp: make_side_report(mcp_body),
            non_mcp: make_side_report(non_mcp_body),
            savings: Savings {
                tokens_pct: 0.0,
                bytes_pct: 0.0,
                latency_ratio: 1.0,
                effective_savings_pct: None,
            },
            verdict: Verdict {
                winner: "mcp".to_string(),
                pros: Vec::new(),
                cons: Vec::new(),
                summary: String::new(),
            },
            non_mcp_is_complete: true,
            reference_answer: None,
            quality: None,
            ensemble_quality: None,
            set_comparison: None,
            compilation_check: None,
            tier: 1,
        }
    }

    fn make_side_quality(value: u8) -> SideQuality {
        SideQuality {
            correctness: value,
            completeness: value,
            usability: value,
            groundedness: value,
            conciseness: value,
        }
    }

    fn make_run(
        position: Position,
        run_index: u8,
        mcp: SideQuality,
        non_mcp: SideQuality,
    ) -> PerRunScores {
        PerRunScores {
            position,
            run_index,
            mcp,
            non_mcp,
            verdict: String::new(),
        }
    }

    // -- prompt + parse tests ---------------------------------------------

    #[test]
    fn build_prompt_is_pure() {
        let scenario = make_scenario("mcp body alpha", "non-mcp body beta");
        let a = build_prompt(
            &scenario,
            Position::McpFirst,
            Some("golden"),
            "grounding mcp",
            "grounding non",
        );
        let b = build_prompt(
            &scenario,
            Position::McpFirst,
            Some("golden"),
            "grounding mcp",
            "grounding non",
        );
        assert_eq!(a, b);
    }

    #[test]
    fn build_prompt_contains_grounding_slots() {
        let scenario = make_scenario("mcp body", "non-mcp body");
        let slot_a = grounding_fallback_block("ANSWER A (MCP)");
        let slot_b = grounding_fallback_block("ANSWER B (non-MCP)");
        let prompt = build_prompt(&scenario, Position::McpFirst, None, &slot_a, &slot_b);
        assert!(prompt.contains("Programmatic grounding for ANSWER A"));
        assert!(prompt.contains("Programmatic grounding for ANSWER B"));
    }

    #[test]
    fn build_prompt_golden_answer_present() {
        let scenario = make_scenario("mcp body", "non-mcp body");
        let prompt = build_prompt(&scenario, Position::McpFirst, Some("my golden"), "g1", "g2");
        assert!(prompt.contains("my golden"));
    }

    #[test]
    fn build_prompt_golden_answer_absent() {
        let scenario = make_scenario("mcp body", "non-mcp body");
        let prompt = build_prompt(&scenario, Position::McpFirst, None, "g1", "g2");
        assert!(prompt.contains("No golden answer was provided"));
    }

    #[test]
    fn build_prompt_position_swap_changes_labels() {
        let scenario = make_scenario("mcp body alpha", "non-mcp body beta");
        let a = build_prompt(&scenario, Position::McpFirst, None, "g1", "g2");
        let b = build_prompt(&scenario, Position::NonMcpFirst, None, "g1", "g2");
        assert_ne!(a, b);
        // Both bodies must appear in both prompts regardless of
        // position; only the label + slot ordering changes.
        assert!(a.contains("mcp body alpha"));
        assert!(a.contains("non-mcp body beta"));
        assert!(b.contains("mcp body alpha"));
        assert!(b.contains("non-mcp body beta"));
    }

    #[test]
    fn json_schema_parses() {
        let value: serde_json::Value =
            serde_json::from_str(JUDGE_JSON_SCHEMA).expect("judge schema must be valid JSON");
        assert_eq!(value["type"], "object");
    }

    #[test]
    fn side_quality_average_divides_by_five() {
        let full = SideQuality {
            correctness: 10,
            completeness: 10,
            usability: 10,
            groundedness: 10,
            conciseness: 10,
        };
        assert!((full.average() - 10.0).abs() < f64::EPSILON);
        let mid = make_side_quality(5);
        assert!((mid.average() - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn aggregate_self_consistency_empty_does_not_panic() {
        let (mcp, non_mcp) = aggregate_self_consistency(&[]);
        assert_eq!(mcp.correctness, 0);
        assert_eq!(mcp.completeness, 0);
        assert_eq!(mcp.usability, 0);
        assert_eq!(mcp.groundedness, 0);
        assert_eq!(mcp.conciseness, 0);
        assert_eq!(non_mcp.correctness, 0);
    }

    #[test]
    fn aggregate_self_consistency_snaps_to_nearest_anchor() {
        // Mean of [5, 5, 7] = 5.67 → nearest anchor is 5.
        let runs_a = vec![
            make_run(
                Position::McpFirst,
                0,
                make_side_quality(5),
                make_side_quality(0),
            ),
            make_run(
                Position::McpFirst,
                1,
                make_side_quality(5),
                make_side_quality(0),
            ),
            make_run(
                Position::McpFirst,
                2,
                make_side_quality(7),
                make_side_quality(0),
            ),
        ];
        let (mcp_a, _) = aggregate_self_consistency(&runs_a);
        assert_eq!(mcp_a.correctness, 5);

        // Mean of [5, 7, 7] = 6.33 → nearest anchor is 7.
        let runs_b = vec![
            make_run(
                Position::McpFirst,
                0,
                make_side_quality(5),
                make_side_quality(0),
            ),
            make_run(
                Position::McpFirst,
                1,
                make_side_quality(7),
                make_side_quality(0),
            ),
            make_run(
                Position::McpFirst,
                2,
                make_side_quality(7),
                make_side_quality(0),
            ),
        ];
        let (mcp_b, _) = aggregate_self_consistency(&runs_b);
        assert_eq!(mcp_b.correctness, 7);

        // Mean of [0, 3, 10] = 4.33 → nearest anchor is 5
        // (|5-4.33|=0.67 vs |3-4.33|=1.33).
        let runs_c = vec![
            make_run(
                Position::McpFirst,
                0,
                make_side_quality(0),
                make_side_quality(0),
            ),
            make_run(
                Position::McpFirst,
                1,
                make_side_quality(3),
                make_side_quality(0),
            ),
            make_run(
                Position::McpFirst,
                2,
                make_side_quality(10),
                make_side_quality(0),
            ),
        ];
        let (mcp_c, _) = aggregate_self_consistency(&runs_c);
        assert_eq!(mcp_c.correctness, 5);
    }

    #[test]
    fn detect_position_bias_flags_severe_on_seven_point_gap() {
        let pass1 = make_run(
            Position::McpFirst,
            0,
            SideQuality {
                correctness: 10,
                completeness: 5,
                usability: 5,
                groundedness: 5,
                conciseness: 5,
            },
            make_side_quality(0),
        );
        let pass2 = make_run(
            Position::NonMcpFirst,
            0,
            SideQuality {
                correctness: 3,
                completeness: 5,
                usability: 5,
                groundedness: 5,
                conciseness: 5,
            },
            make_side_quality(0),
        );
        let flags = detect_position_bias(&[pass1, pass2]);
        assert!(flags.contains(&"position_bias_severe:correctness".to_string()));
    }

    #[test]
    fn detect_position_bias_flags_warning_on_three_point_gap() {
        let pass1 = make_run(
            Position::McpFirst,
            0,
            SideQuality {
                correctness: 8,
                completeness: 5,
                usability: 5,
                groundedness: 5,
                conciseness: 5,
            },
            make_side_quality(0),
        );
        let pass2 = make_run(
            Position::NonMcpFirst,
            0,
            SideQuality {
                correctness: 5,
                completeness: 5,
                usability: 5,
                groundedness: 5,
                conciseness: 5,
            },
            make_side_quality(0),
        );
        let flags = detect_position_bias(&[pass1, pass2]);
        assert!(flags.contains(&"position_bias_warning:correctness".to_string()));
    }

    #[test]
    fn detect_position_bias_empty_runs() {
        assert!(detect_position_bias(&[]).is_empty());
    }

    // -- ensemble + Cohen's kappa + Krippendorff's alpha -----------------

    #[test]
    fn cohens_weighted_kappa_perfect_agreement() {
        // All pairs identical: κ must be exactly 1.0 regardless of k.
        let pairs = vec![(0u8, 0u8), (5, 5), (7, 7), (10, 10), (3, 3)];
        let k = cohens_weighted_kappa(&pairs, 11);
        assert!(
            (k - 1.0).abs() < 1e-9,
            "expected κ=1.0 for identical pairs, got {k}"
        );
    }

    #[test]
    fn cohens_weighted_kappa_complete_disagreement() {
        // Reference value from a Python session:
        //   from sklearn.metrics import cohen_kappa_score
        //   a = [0, 0, 10, 10]
        //   b = [10, 10, 0, 0]
        //   cohen_kappa_score(a, b, weights="quadratic", labels=list(range(11)))
        //   -> -1.0
        // Two raters, maximum quadratic disagreement on an 11-category
        // scale → κ_w = -1.0.
        let pairs = vec![(0u8, 10u8), (0, 10), (10, 0), (10, 0)];
        let k = cohens_weighted_kappa(&pairs, 11);
        assert!(
            (k - -1.0).abs() < 1e-6,
            "expected κ=-1.0 for mirror disagreement, got {k}"
        );
    }

    #[test]
    fn cohens_weighted_kappa_insufficient_data() {
        assert!(cohens_weighted_kappa(&[], 11).is_nan());
        assert!(cohens_weighted_kappa(&[(5, 5)], 11).is_nan());
    }

    #[test]
    fn cohens_weighted_kappa_p_e_is_one() {
        // Two raters who both scored every pair 5 on a k=11 scale.
        // p_e hits 1.0 because the only non-zero entry in the expected
        // table is row 5 × column 5 — special-cased to κ=1.0.
        let pairs = vec![(5u8, 5u8), (5, 5), (5, 5)];
        let k = cohens_weighted_kappa(&pairs, 11);
        assert!(
            (k - 1.0).abs() < 1e-9,
            "expected κ=1.0 when p_e == 1.0, got {k}"
        );
    }

    #[test]
    fn cohens_weighted_kappa_out_of_range_is_nan() {
        // 12 is outside 0..=10 so the helper returns NAN rather than
        // panicking on an out-of-bounds index.
        let pairs = vec![(0u8, 0u8), (5, 12)];
        assert!(cohens_weighted_kappa(&pairs, 11).is_nan());
    }

    #[test]
    fn krippendorff_alpha_interval_three_raters() {
        // Reference value from a hand computation:
        //   units = [[5, 5, 5], [7, 7, 7], [10, 10, 10]]
        //   All raters agree on every unit → D_o = 0 → α = 1.0.
        let units = vec![
            vec![Some(5.0), Some(5.0), Some(5.0)],
            vec![Some(7.0), Some(7.0), Some(7.0)],
            vec![Some(10.0), Some(10.0), Some(10.0)],
        ];
        let alpha = krippendorff_alpha_interval(&units);
        // Perfect agreement → D_e collapses to 0 so the helper returns
        // None, or the metric sits at its ceiling → Some(1.0). Either
        // outcome is acceptable because the interval metric cannot
        // distinguish between the two on a fully-agreeing set.
        if let Some(v) = alpha {
            assert!(
                (v - 1.0).abs() < 1e-6,
                "expected α≈1.0 for perfect agreement, got {v}"
            );
        }
    }

    #[test]
    fn krippendorff_alpha_interval_insufficient_data() {
        // Two total non-missing ratings → None (< 3 threshold).
        let units = vec![vec![Some(5.0), None, None], vec![None, Some(7.0), None]];
        assert!(krippendorff_alpha_interval(&units).is_none());
    }

    #[test]
    fn krippendorff_alpha_interval_with_missing() {
        // Four units with a single missing value on unit #3. The
        // helper should still compute a finite α because the unit-level
        // normalizer divides by `n_u - 1` per §3.b.
        let units = vec![
            vec![Some(5.0), Some(5.0), Some(7.0)],
            vec![Some(5.0), Some(7.0), Some(5.0)],
            vec![Some(7.0), Some(7.0), Some(7.0)],
            vec![Some(5.0), Some(5.0), None],
        ];
        let alpha = krippendorff_alpha_interval(&units);
        assert!(
            alpha.is_some(),
            "expected Some(α) on partial-missing input, got None"
        );
        let v = alpha.unwrap();
        assert!(
            (-1.5..=1.0).contains(&v),
            "α should be ≤ 1.0 and reasonably above -1.5, got {v}"
        );
    }

    #[test]
    fn abs_delta_per_axis_side_basic() {
        let a = SideQuality {
            correctness: 7,
            completeness: 5,
            usability: 10,
            groundedness: 3,
            conciseness: 0,
        };
        let b = SideQuality {
            correctness: 5,
            completeness: 7,
            usability: 10,
            groundedness: 0,
            conciseness: 3,
        };
        let d = abs_delta_per_axis_side(&a, &b);
        assert_eq!(d.len(), 5);
        assert!((d[0] - 2.0).abs() < 1e-9);
        assert!((d[1] - 2.0).abs() < 1e-9);
        assert!((d[2] - 0.0).abs() < 1e-9);
        assert!((d[3] - 3.0).abs() < 1e-9);
        assert!((d[4] - 3.0).abs() < 1e-9);
    }

    #[test]
    fn abs_delta_per_axis_ten_elements() {
        // Same side twice → all zeros (10 values).
        let a = QualityScores {
            mcp: make_side_quality(7),
            non_mcp: make_side_quality(5),
            verdict: String::new(),
            model: "x".to_string(),
            runs: Vec::new(),
            flags: Vec::new(),
            reference_answer_used: false,
        };
        let d = abs_delta_per_axis(&a, &a);
        assert_eq!(d.len(), 10);
        for v in d {
            assert!((v - 0.0).abs() < 1e-9);
        }
    }

    #[test]
    fn elementwise_mean_side_snaps_to_anchor() {
        // Per-axis mean of (5, 7) = 6.0 → nearest anchor in
        // {0, 3, 5, 7, 10}. Both 5 and 7 are 1 away; snap_to_anchor
        // ties low-anchor, so result is 5.
        let a = make_side_quality(5);
        let b = make_side_quality(7);
        let m = elementwise_mean_side(&a, &b);
        assert_eq!(m.correctness, 5);
        assert_eq!(m.completeness, 5);

        // Mean of (3, 7) = 5.0 → exactly the 5 anchor.
        let a3 = make_side_quality(3);
        let b7 = make_side_quality(7);
        let m2 = elementwise_mean_side(&a3, &b7);
        assert_eq!(m2.correctness, 5);

        // Mean of (7, 10) = 8.5 → nearest anchor is 10 (8.5−10=1.5 vs
        // 8.5−7=1.5; snap_to_anchor keeps the first-seen lower anchor,
        // so result is 7).
        let a7 = make_side_quality(7);
        let b10 = make_side_quality(10);
        let m3 = elementwise_mean_side(&a7, &b10);
        assert_eq!(m3.correctness, 7);
    }

    #[test]
    fn elementwise_mean_quality_merges_metadata() {
        let a = QualityScores {
            mcp: make_side_quality(7),
            non_mcp: make_side_quality(5),
            verdict: "alpha".to_string(),
            model: "opus".to_string(),
            runs: Vec::new(),
            flags: vec!["position_bias_warning:correctness".to_string()],
            reference_answer_used: false,
        };
        let b = QualityScores {
            mcp: make_side_quality(5),
            non_mcp: make_side_quality(7),
            verdict: "beta".to_string(),
            model: "sonnet".to_string(),
            runs: Vec::new(),
            flags: Vec::new(),
            reference_answer_used: true,
        };
        let m = elementwise_mean_quality(&a, &b);
        // Per-side mean (7, 5) = 6 → nearest anchor 5 (low-anchor tie).
        assert_eq!(m.mcp.correctness, 5);
        assert_eq!(m.non_mcp.correctness, 5);
        assert_eq!(m.verdict, "alpha");
        assert!(m.model.contains("opus"));
        assert!(m.model.contains("sonnet"));
        assert!(m.reference_answer_used);
        assert_eq!(m.flags.len(), 1);
    }

    #[test]
    fn append_prior_ratings_embeds_both_models() {
        let a = QualityScores {
            mcp: make_side_quality(7),
            non_mcp: make_side_quality(5),
            verdict: "opus verdict".to_string(),
            model: "claude-opus-4-6".to_string(),
            runs: Vec::new(),
            flags: Vec::new(),
            reference_answer_used: false,
        };
        let b = QualityScores {
            mcp: make_side_quality(3),
            non_mcp: make_side_quality(10),
            verdict: "sonnet verdict".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            runs: Vec::new(),
            flags: Vec::new(),
            reference_answer_used: false,
        };
        let p = append_prior_ratings("BASE_PROMPT", &a, &b);
        assert!(p.starts_with("BASE_PROMPT\n"));
        assert!(p.contains("PRIOR_RATINGS:"));
        assert!(p.contains("claude-opus-4-6"));
        assert!(p.contains("claude-sonnet-4-6"));
        assert!(p.contains("opus verdict"));
        assert!(p.contains("sonnet verdict"));
        assert!(p.contains("The two judges disagree"));
    }

    #[test]
    fn extract_structured_output_happy_path() {
        let envelope = r#"{"type":"result","subtype":"success","is_error":false,"result":"Done.","structured_output":{"a":{"correctness":5,"completeness":5,"usability":5,"groundedness":5,"conciseness":5},"b":{"correctness":7,"completeness":7,"usability":7,"groundedness":7,"conciseness":7},"verdict":"test verdict"}}"#;
        let inner = extract_structured_output(envelope).expect("envelope parses");
        assert!(inner.contains("\"correctness\":5"));
        assert!(inner.contains("\"correctness\":7"));
        assert!(inner.contains("\"verdict\":\"test verdict\""));
        // Must be parseable by the downstream schema validator.
        let _: serde_json::Value = serde_json::from_str(&inner).expect("inner is valid JSON");
    }

    #[test]
    fn extract_structured_output_surfaces_is_error() {
        let envelope = r#"{"is_error":true,"result":"model refused the task"}"#;
        let err = extract_structured_output(envelope).unwrap_err();
        assert!(err.to_string().contains("model refused the task"));
    }

    #[test]
    fn extract_structured_output_missing_field() {
        let envelope = r#"{"type":"result","is_error":false,"result":"just a chat reply with no schema object"}"#;
        let err = extract_structured_output(envelope).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("structured_output"));
        assert!(msg.contains("just a chat reply"));
    }

    #[test]
    fn extract_structured_output_malformed_envelope() {
        let envelope = "not json at all";
        let err = extract_structured_output(envelope).unwrap_err();
        assert!(err.to_string().contains("envelope"));
    }

    // -- judge reuse cache tests ------------------------------------------

    fn make_quality(model: &str) -> QualityScores {
        QualityScores {
            mcp: make_side_quality(7),
            non_mcp: make_side_quality(3),
            verdict: format!("verdict from {model}"),
            model: model.to_string(),
            runs: Vec::new(),
            flags: Vec::new(),
            reference_answer_used: false,
        }
    }

    fn make_grounding(score: f64) -> crate::benchmark::grounding::GroundingScores {
        crate::benchmark::grounding::GroundingScores {
            total_claims: 10,
            verified_claims: (score * 10.0).round() as usize,
            file_claims: 5,
            line_claims: 3,
            symbol_claims: 2,
            verified_files: 5,
            verified_lines: 3,
            verified_symbols: 2,
            unverified: Vec::new(),
            score,
            elapsed_us: 0,
            degraded: false,
        }
    }

    fn make_report(
        sha: Option<&str>,
        scenarios: Vec<ScenarioReport>,
    ) -> crate::benchmark::report::BenchmarkReport {
        crate::benchmark::report::BenchmarkReport {
            generated_at_unix: 0,
            git_sha: sha.map(str::to_string),
            tokenizer: "tiktoken-cl100k_base".to_string(),
            language: "rust".to_string(),
            scenarios,
        }
    }

    #[test]
    fn build_judge_cache_respects_git_sha() {
        let mut scenario = make_scenario("mcp body", "non-mcp body");
        scenario.quality = Some(make_quality("opus"));
        let prior = make_report(Some("aaaa"), vec![scenario]);

        let cache = build_judge_cache(&prior, Some("bbbb"), false);
        assert!(
            cache.is_empty(),
            "mismatched SHA must invalidate the cache"
        );
    }

    #[test]
    fn build_judge_cache_allow_stale_bypasses_sha_gate() {
        let mut scenario = make_scenario("mcp body", "non-mcp body");
        scenario.quality = Some(make_quality("opus"));
        let prior = make_report(Some("aaaa"), vec![scenario]);

        let cache = build_judge_cache(&prior, Some("bbbb"), true);
        assert_eq!(
            cache.len(),
            1,
            "allow_stale must populate the cache despite SHA mismatch"
        );
    }

    #[test]
    fn build_judge_cache_skips_unjudged_scenarios() {
        let mut judged = make_scenario("mcp", "non-mcp");
        judged.scenario_id = "judged".to_string();
        judged.quality = Some(make_quality("opus"));

        let mut unjudged = make_scenario("mcp", "non-mcp");
        unjudged.scenario_id = "unjudged".to_string();
        // quality stays None.

        let prior = make_report(None, vec![judged, unjudged]);
        let cache = build_judge_cache(&prior, None, false);
        assert_eq!(cache.len(), 1);
        assert!(cache.contains_key("judged"));
        assert!(!cache.contains_key("unjudged"));
    }

    #[test]
    fn cache_key_matches_same_inputs() {
        let scenario = make_scenario("alpha mcp body", "beta non-mcp body");
        let a = JudgeCacheKey::from_scenario(&scenario);
        let b = JudgeCacheKey::from_scenario(&scenario);
        assert_eq!(a, b);
    }

    #[test]
    fn cache_key_differs_on_mcp_output_change() {
        let scenario_a = make_scenario("alpha mcp body", "non-mcp body");
        let scenario_b = make_scenario("alpha mcp body!", "non-mcp body");
        let a = JudgeCacheKey::from_scenario(&scenario_a);
        let b = JudgeCacheKey::from_scenario(&scenario_b);
        assert_ne!(a.mcp_output_hash, b.mcp_output_hash);
        assert_ne!(a, b);
    }

    #[test]
    fn cache_key_differs_on_non_mcp_output_change() {
        let scenario_a = make_scenario("mcp body", "alpha non-mcp body");
        let scenario_b = make_scenario("mcp body", "alpha non-mcp body!");
        let a = JudgeCacheKey::from_scenario(&scenario_a);
        let b = JudgeCacheKey::from_scenario(&scenario_b);
        assert_ne!(a.non_mcp_output_hash, b.non_mcp_output_hash);
        assert_ne!(a, b);
    }

    #[test]
    fn cache_key_differs_on_reference_answer_change() {
        let mut scenario_a = make_scenario("mcp", "non-mcp");
        scenario_a.reference_answer = Some("the right answer".to_string());
        let mut scenario_b = make_scenario("mcp", "non-mcp");
        scenario_b.reference_answer = Some("a different answer".to_string());
        assert_ne!(
            JudgeCacheKey::from_scenario(&scenario_a),
            JudgeCacheKey::from_scenario(&scenario_b)
        );
    }

    #[test]
    fn cache_key_treats_none_reference_as_zero() {
        let scenario_a = make_scenario("mcp", "non-mcp");
        let scenario_b = make_scenario("mcp", "non-mcp");
        let a = JudgeCacheKey::from_scenario(&scenario_a);
        let b = JudgeCacheKey::from_scenario(&scenario_b);
        assert_eq!(a.reference_hash, 0);
        assert_eq!(b.reference_hash, 0);
        assert_eq!(a, b);
    }

    #[test]
    fn cache_key_differs_on_grounding_score_change() {
        let mut scenario_a = make_scenario("mcp", "non-mcp");
        scenario_a.mcp.grounding = Some(make_grounding(0.917));
        let mut scenario_b = make_scenario("mcp", "non-mcp");
        scenario_b.mcp.grounding = Some(make_grounding(0.918));
        let a = JudgeCacheKey::from_scenario(&scenario_a);
        let b = JudgeCacheKey::from_scenario(&scenario_b);
        assert_ne!(a.mcp_grounding_score, b.mcp_grounding_score);
        assert_ne!(a, b);
    }

    #[test]
    fn cache_key_grounding_some_vs_none_differs() {
        let scenario_a = make_scenario("mcp", "non-mcp");
        let mut scenario_b = make_scenario("mcp", "non-mcp");
        scenario_b.non_mcp.grounding = Some(make_grounding(0.5));
        assert_ne!(
            JudgeCacheKey::from_scenario(&scenario_a),
            JudgeCacheKey::from_scenario(&scenario_b)
        );
    }

    #[test]
    fn lookup_judge_cache_strict_hit() {
        let mut scenario = make_scenario("mcp", "non-mcp");
        scenario.scenario_id = "scn".to_string();
        scenario.quality = Some(make_quality("opus"));
        let prior = make_report(None, vec![scenario.clone()]);
        let cache = build_judge_cache(&prior, None, false);
        let hit = lookup_judge_cache(&cache, &scenario, false);
        assert!(hit.is_some(), "identical inputs must produce a cache hit");
    }

    #[test]
    fn lookup_judge_cache_strict_miss_on_drift() {
        let mut original = make_scenario("mcp", "non-mcp");
        original.scenario_id = "scn".to_string();
        original.quality = Some(make_quality("opus"));
        let prior = make_report(None, vec![original]);
        let cache = build_judge_cache(&prior, None, false);

        // Same scenario_id, different MCP body — must miss in strict mode.
        let mut drifted = make_scenario("mcp drifted", "non-mcp");
        drifted.scenario_id = "scn".to_string();
        let hit = lookup_judge_cache(&cache, &drifted, false);
        assert!(
            hit.is_none(),
            "drifted MCP output must not match in strict mode"
        );
    }

    #[test]
    fn lookup_judge_cache_allow_stale_ignores_drift() {
        let mut original = make_scenario("mcp", "non-mcp");
        original.scenario_id = "scn".to_string();
        original.quality = Some(make_quality("opus"));
        let prior = make_report(None, vec![original]);
        let cache = build_judge_cache(&prior, None, false);

        let mut drifted = make_scenario("mcp drifted", "non-mcp drifted");
        drifted.scenario_id = "scn".to_string();
        let hit = lookup_judge_cache(&cache, &drifted, true);
        assert!(
            hit.is_some(),
            "allow_stale must reuse by scenario_id alone"
        );
    }

    #[test]
    fn lookup_judge_cache_unknown_id_misses() {
        let cache = std::collections::HashMap::new();
        let scenario = make_scenario("mcp", "non-mcp");
        assert!(lookup_judge_cache(&cache, &scenario, false).is_none());
        assert!(lookup_judge_cache(&cache, &scenario, true).is_none());
    }

    // -- Batch judge tests --------------------------------------------------

    #[test]
    fn batch_json_schema_parses() {
        let _: serde_json::Value = serde_json::from_str(JUDGE_BATCH_JSON_SCHEMA)
            .expect("JUDGE_BATCH_JSON_SCHEMA must be valid JSON");
    }

    #[test]
    fn build_batch_prompt_contains_all_scenarios() {
        let s1 = make_scenario("mcp body 1", "non-mcp body 1");
        let s2 = make_scenario("mcp body 2", "non-mcp body 2");
        let refs: Vec<&ScenarioReport> = vec![&s1, &s2];
        let prompt = build_batch_prompt(&refs);
        assert!(prompt.contains("SCENARIO 1"));
        assert!(prompt.contains("SCENARIO 2"));
        assert!(prompt.contains("MCP output:"));
        assert!(prompt.contains("Non-MCP output:"));
        assert!(prompt.contains("correctness:"));
        assert!(prompt.contains("usability:"));
    }

    #[test]
    fn build_batch_prompt_empty_scenarios() {
        let prompt = build_batch_prompt(&[]);
        assert!(prompt.contains("RUBRIC"));
        assert!(!prompt.contains("SCENARIO"));
    }

    #[test]
    fn compute_programmatic_axes_high_savings() {
        let mut scenario = make_scenario("mcp", "non-mcp");
        scenario.savings.tokens_pct = 85.0;
        let (mcp, non_mcp) = compute_programmatic_axes(&scenario);
        assert_eq!(mcp.conciseness, 10);
        assert_eq!(non_mcp.conciseness, 3);
        assert_eq!(mcp.completeness, 7);
        assert_eq!(mcp.groundedness, 7);
    }

    #[test]
    fn compute_programmatic_axes_negative_savings() {
        let mut scenario = make_scenario("mcp", "non-mcp");
        scenario.savings.tokens_pct = -15.0;
        let (mcp, non_mcp) = compute_programmatic_axes(&scenario);
        assert_eq!(mcp.conciseness, 3);
        assert_eq!(non_mcp.conciseness, 7);
    }

    #[test]
    fn compute_programmatic_axes_with_recall() {
        let mut scenario = make_scenario("mcp", "non-mcp");
        scenario.savings.tokens_pct = 50.0;
        scenario.set_comparison = Some(crate::benchmark::set_compare::SetComparisonScores {
            mcp_items: 10,
            non_mcp_items: 10,
            intersection: 9,
            precision: 0.9,
            recall: 0.9,
            mcp_only: Vec::new(),
            non_mcp_only: Vec::new(),
        });
        let (mcp, _) = compute_programmatic_axes(&scenario);
        assert_eq!(mcp.completeness, 7);
    }

    #[test]
    fn compute_programmatic_axes_perfect_recall() {
        let mut scenario = make_scenario("mcp", "non-mcp");
        scenario.set_comparison = Some(crate::benchmark::set_compare::SetComparisonScores {
            mcp_items: 10,
            non_mcp_items: 10,
            intersection: 10,
            precision: 1.0,
            recall: 1.0,
            mcp_only: Vec::new(),
            non_mcp_only: Vec::new(),
        });
        let (mcp, _) = compute_programmatic_axes(&scenario);
        assert_eq!(mcp.completeness, 10);
    }
}
