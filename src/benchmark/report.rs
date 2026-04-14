//! Output schema + writers (JSON, Markdown) + regression check.
//!
//! The JSON file is the machine-readable source of truth; the Markdown file
//! is rendered from the same data for human consumption. The regression
//! check compares a current run against a committed baseline and flags any
//! tool whose token efficiency regressed by more than the allowed percentage.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::judge::{
    AXIS_NAMES_TEN, EnsembleQualityScores, QualityScores, cohens_weighted_kappa,
    krippendorff_alpha_interval,
};
use super::scenarios::Scenario;

/// Aggregate LLM-judge quality statistics computed by
/// [`average_quality`] and rendered in the headline. Tracks the five
/// axes so the headline can report per-side averages. The model string
/// is the effective primary judge model — for ensemble runs it carries
/// both models joined by `" + "`.
#[derive(Debug, Clone)]
struct QualityAggregate {
    model: String,
    mcp_avg: f64,
    non_mcp_avg: f64,
    scored_count: usize,
    skipped_count: usize,
}

/// Aggregate inter-rater agreement statistics computed from a full
/// [`BenchmarkReport`]. Returned by [`compute_agreement_stats`] and
/// rendered by [`render_markdown`] in the `## Judge reliability`
/// section when `--judge-ensemble` produced any ensemble rows.
#[derive(Debug, Clone)]
pub struct AgreementStats {
    /// Number of scenarios that carry an ensemble verdict.
    pub n_scenarios: usize,
    /// Cohen's weighted quadratic κ across (primary, secondary) pairs
    /// over all 10 axes × all scenarios. `f64::NAN` when insufficient
    /// rating pairs are available.
    pub cohens_kappa: f64,
    /// Krippendorff's α over (primary, secondary, arbiter) on the
    /// arbitrated subset. `None` when insufficient ratings or when
    /// the arbiter did not fire on enough scenarios.
    pub krippendorff_alpha: Option<f64>,
    /// Count of scenarios where `agreement == false` (at least one
    /// axis exceeded the disagreement threshold).
    pub n_disagreements: usize,
    /// Count of scenarios where the arbiter actually ran. Equals
    /// `n_disagreements` today; exists as a separate field so a
    /// future confidence gate can widen the gap.
    pub n_arbitrated: usize,
    /// Mean `|Δ|` across all 10 axes × all scenarios. Headline
    /// scalar rendered alongside κ.
    pub mean_abs_delta: f64,
}

/// Computes headline quality stats, or `None` when the report has no
/// judge scores at all. Uses the per-side 5-axis `average()` from
/// [`crate::benchmark::judge::SideQuality`].
fn average_quality(report: &BenchmarkReport) -> Option<QualityAggregate> {
    let scored: Vec<&QualityScores> = report
        .scenarios
        .iter()
        .filter_map(|s| s.quality.as_ref())
        .collect();
    if scored.is_empty() {
        return None;
    }
    let n = scored.len() as f64;
    let mcp_avg = scored.iter().map(|q| q.mcp.average()).sum::<f64>() / n;
    let non_mcp_avg = scored.iter().map(|q| q.non_mcp.average()).sum::<f64>() / n;
    Some(QualityAggregate {
        model: scored[0].model.clone(),
        mcp_avg,
        non_mcp_avg,
        scored_count: scored.len(),
        skipped_count: report.scenarios.len() - scored.len(),
    })
}

/// Computes inter-rater agreement stats across every ensemble-scored
/// scenario in `report`. Returns `None` when no scenario carries an
/// `ensemble_quality` entry — the renderer uses that to skip the whole
/// `## Judge reliability` section on legacy reports.
///
/// The rating pool for Cohen's κ is built from every
/// `(primary_axis, secondary_axis)` pair across all 10 axes × all
/// ensemble scenarios. Each axis is already in the rubric range
/// `{0, 3, 5, 7, 10}` which fits inside `0..=10`, so we call
/// [`cohens_weighted_kappa`] with `k = 11`.
///
/// Krippendorff's α is computed on the arbitrated subset where each
/// unit is a `[primary, secondary, arbiter]` rating triple across one
/// axis of one disputed scenario. `None` is returned when too few
/// ratings are available (see [`krippendorff_alpha_interval`]).
pub fn compute_agreement_stats(report: &BenchmarkReport) -> Option<AgreementStats> {
    let ensemble_scenarios: Vec<&EnsembleQualityScores> = report
        .scenarios
        .iter()
        .filter_map(|s| s.ensemble_quality.as_ref())
        .collect();
    if ensemble_scenarios.is_empty() {
        return None;
    }

    let mut pairs: Vec<(u8, u8)> = Vec::new();
    let mut units: Vec<Vec<Option<f64>>> = Vec::new();
    let mut delta_sum = 0.0f64;
    let mut delta_count = 0usize;
    let mut n_disagreements = 0usize;
    let mut n_arbitrated = 0usize;

    for ens in &ensemble_scenarios {
        let primary_axes = extract_ten_axes(&ens.primary);
        let secondary_axes = extract_ten_axes(&ens.secondary);
        let arbiter_axes = ens.arbiter.as_ref().map(extract_ten_axes);

        for i in 0..10 {
            pairs.push((primary_axes[i], secondary_axes[i]));

            let mut unit = vec![
                Some(f64::from(primary_axes[i])),
                Some(f64::from(secondary_axes[i])),
            ];
            if let Some(ref arb) = arbiter_axes {
                unit.push(Some(f64::from(arb[i])));
            } else {
                unit.push(None);
            }
            units.push(unit);
        }

        for d in &ens.abs_delta_per_axis {
            delta_sum += *d;
            delta_count += 1;
        }
        if !ens.agreement {
            n_disagreements += 1;
        }
        if ens.arbiter.is_some() {
            n_arbitrated += 1;
        }
    }

    // k=11 covers the ordinal 0..=10 range used by the rubric's
    // anchor set {0, 3, 5, 7, 10}. Actual values are sparse inside
    // that range but the categorical bucket count matches.
    let kappa = cohens_weighted_kappa(&pairs, 11);

    // Krippendorff's α is only meaningful when the arbiter fired on
    // enough scenarios to generate a three-rater unit set. The helper
    // returns `None` when fewer than three non-missing ratings exist;
    // we also guard at the caller side to keep the Markdown branch
    // predictable.
    let arbitrated_units: Vec<Vec<Option<f64>>> = units
        .iter()
        .filter(|u| u.len() == 3 && u[2].is_some())
        .cloned()
        .collect();
    let alpha = if arbitrated_units.len() >= 3 {
        krippendorff_alpha_interval(&arbitrated_units)
    } else {
        None
    };

    let mean_abs_delta = if delta_count == 0 {
        0.0
    } else {
        delta_sum / delta_count as f64
    };

    Some(AgreementStats {
        n_scenarios: ensemble_scenarios.len(),
        cohens_kappa: kappa,
        krippendorff_alpha: alpha,
        n_disagreements,
        n_arbitrated,
        mean_abs_delta,
    })
}

/// Flattens one [`QualityScores`] into the 10-axis order used by
/// [`AXIS_NAMES_TEN`]: five MCP axes followed by five non-MCP axes in
/// the canonical axis order (correctness, completeness, usability,
/// groundedness, conciseness).
fn extract_ten_axes(q: &QualityScores) -> [u8; 10] {
    [
        q.mcp.correctness,
        q.mcp.completeness,
        q.mcp.usability,
        q.mcp.groundedness,
        q.mcp.conciseness,
        q.non_mcp.correctness,
        q.non_mcp.completeness,
        q.non_mcp.usability,
        q.non_mcp.groundedness,
        q.non_mcp.conciseness,
    ]
}

/// Maps a Cohen's κ value to its Landis & Koch 1977 interpretation
/// band. Exact thresholds from the design doc (`ensemble-and-agreement.md`
/// §7.a). Returns a static string so the caller can embed it in a
/// Markdown template without extra allocation.
fn landis_koch_band(kappa: f64) -> &'static str {
    if kappa.is_nan() {
        return "undefined";
    }
    if kappa < 0.2 {
        "poor"
    } else if kappa < 0.4 {
        "fair"
    } else if kappa < 0.6 {
        "moderate"
    } else if kappa < 0.8 {
        "substantial"
    } else {
        "almost perfect"
    }
}

/// One row of the "Top 3 most-disputed scenarios" table rendered in
/// the `## Judge reliability` section. Sorted descending by
/// `max_delta`; only the first three rows are rendered.
#[derive(Debug, Clone)]
struct DisputedRow {
    scenario_id: String,
    tool: String,
    max_delta: f64,
    axis_name: &'static str,
    primary_avg: f64,
    secondary_avg: f64,
    final_avg: f64,
    arbiter_used: bool,
}

/// Builds the "Top 3 most-disputed scenarios" table rows for the
/// `## Judge reliability` section. Returns all rows sorted descending
/// by `max_delta`; the renderer truncates to the top 3.
fn build_disputed_rows(report: &BenchmarkReport) -> Vec<DisputedRow> {
    let mut rows: Vec<DisputedRow> = Vec::new();
    for s in &report.scenarios {
        let Some(ens) = s.ensemble_quality.as_ref() else {
            continue;
        };
        let (max_idx, max_delta) = ens.abs_delta_per_axis.iter().copied().enumerate().fold(
            (0usize, f64::NEG_INFINITY),
            |(best_idx, best), (i, d)| {
                if d > best { (i, d) } else { (best_idx, best) }
            },
        );
        if !max_delta.is_finite() {
            continue;
        }
        let axis_name = *AXIS_NAMES_TEN.get(max_idx).unwrap_or(&"unknown");
        rows.push(DisputedRow {
            scenario_id: s.scenario_id.clone(),
            tool: s.tool.clone(),
            max_delta,
            axis_name,
            primary_avg: (ens.primary.mcp.average() + ens.primary.non_mcp.average()) / 2.0,
            secondary_avg: (ens.secondary.mcp.average() + ens.secondary.non_mcp.average()) / 2.0,
            final_avg: (ens.final_score.mcp.average() + ens.final_score.non_mcp.average()) / 2.0,
            arbiter_used: ens.arbiter.is_some(),
        });
    }
    rows.sort_by(|a, b| {
        b.max_delta
            .partial_cmp(&a.max_delta)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rows
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyStats {
    pub mean_us: f64,
    pub stdev_us: f64,
    pub p50_us: f64,
    pub p95_us: f64,
    pub samples: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SideReport {
    pub response_bytes: usize,
    pub response_preview: String,
    pub tokens: usize,
    pub naive_tokens: usize,
    pub latency: LatencyStats,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<String>>,
    /// True when this side was loaded from a cached prior run instead of
    /// being freshly measured. Used to annotate the Markdown report.
    #[serde(default)]
    pub reused: bool,
    /// Full response text kept in-process for the LLM judge. Deliberately
    /// excluded from serialization so the JSON report stays small; a
    /// deserialized `SideReport` always starts with an empty string here.
    /// Populated by `BenchmarkRunner::run_mcp` / `run_sim` from the last
    /// measured sample's output.
    #[serde(skip)]
    pub full_output: String,
    /// Claim-level groundedness produced by
    /// [`crate::benchmark::grounding::verify_side`]. `None` means grounding
    /// was disabled (legacy `--judge` path), parser found zero claims, or
    /// the input exceeded the hard caps. Slice B of PLAN.md §3; the Matrix
    /// column and per-scenario detail block that render this field live in
    /// slice C's renderer extension. `serde(default, skip_serializing_if)`
    /// keeps older JSON reports deserializing without a migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding: Option<crate::benchmark::grounding::GroundingScores>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Savings {
    pub tokens_pct: f64,
    pub bytes_pct: f64,
    pub latency_ratio: f64,
    /// Token savings weighted by set-comparison recall. `None` when
    /// set comparison is not available for the tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_savings_pct: Option<f64>,
}

/// Post-benchmark compilation check result for refactoring tools
/// (qartez_rename, qartez_move). Populated by `--compile-check`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilationResult {
    pub passed: bool,
    /// First 500 chars of stderr on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_snippet: Option<String>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub winner: String,
    pub pros: Vec<String>,
    pub cons: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioReport {
    pub tool: String,
    pub scenario_id: String,
    pub description: String,
    pub mcp: SideReport,
    pub non_mcp: SideReport,
    pub savings: Savings,
    pub verdict: Verdict,
    /// Mirror of `Scenario::non_mcp_is_complete`. Preserved in the JSON
    /// artifact so downstream readers (dashboards, regression checks) can
    /// tell "MCP wins by correctness, not bytes" from "MCP wins on bytes"
    /// without re-reading the scenarios table. Defaults to `true` so a
    /// report written by an older version of this binary still loads.
    #[serde(default = "default_true")]
    pub non_mcp_is_complete: bool,
    /// Verbatim `Scenario::reference_answer`. Carried on the report so
    /// the `--judge` runner can retrieve it without re-walking `SCENARIOS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_answer: Option<String>,
    /// LLM-judge scores on the 5-axis rubric (correctness, completeness,
    /// usability, groundedness, conciseness). Populated by `benchmark
    /// --judge` runs. Older JSON reports deserialize cleanly via
    /// `serde(default)`. See [`crate::benchmark::judge::QualityScores`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<QualityScores>,
    /// Ensemble LLM-judge scores from the `--judge-ensemble` path.
    /// Carries primary + secondary [`QualityScores`], an optional arbiter
    /// verdict, and the final aggregated score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ensemble_quality: Option<EnsembleQualityScores>,
    /// Precision/recall from comparing MCP vs non-MCP item sets.
    /// Populated for list-returning tools (qartez_find, qartez_grep, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_comparison: Option<super::set_compare::SetComparisonScores>,
    /// Post-benchmark compilation result for refactoring tools.
    /// Populated by `--compile-check`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compilation_check: Option<CompilationResult>,
    /// Scenario tier (1 = default, 2+ = edge cases). Defaults to 1
    /// for backward compatibility with older JSON reports.
    #[serde(default = "default_tier")]
    pub tier: u8,
}

fn default_true() -> bool {
    true
}

fn default_tier() -> u8 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    pub generated_at_unix: u64,
    pub git_sha: Option<String>,
    pub tokenizer: String,
    /// Language profile the report was produced under (`"rust"`,
    /// `"typescript"`, …). Defaults to `"rust"` when deserializing
    /// older reports written before the multi-language refactor, so
    /// pre-existing `reports/baseline.json` files continue to load
    /// without a schema migration.
    #[serde(default = "default_rust_lang")]
    pub language: String,
    pub scenarios: Vec<ScenarioReport>,
}

fn default_rust_lang() -> String {
    "rust".to_string()
}

impl BenchmarkReport {
    pub fn new(scenarios: Vec<ScenarioReport>) -> Self {
        Self::new_with_language(scenarios, default_rust_lang())
    }

    pub fn new_with_language(scenarios: Vec<ScenarioReport>, language: String) -> Self {
        Self {
            generated_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            git_sha: git_sha(),
            tokenizer: "cl100k_base".to_string(),
            language,
            scenarios,
        }
    }
}

pub fn build_scenario_report(
    scenario: &Scenario,
    mcp: SideReport,
    non_mcp: SideReport,
) -> ScenarioReport {
    let savings = compute_savings(&mcp, &non_mcp);
    let winner = pick_winner(&savings, &mcp, &non_mcp, scenario.non_mcp_is_complete);

    ScenarioReport {
        tool: scenario.tool.to_string(),
        scenario_id: scenario.id.to_string(),
        description: scenario.description.to_string(),
        mcp,
        non_mcp,
        savings,
        verdict: Verdict {
            winner,
            pros: scenario.pros.iter().map(|s| s.to_string()).collect(),
            cons: scenario.cons.iter().map(|s| s.to_string()).collect(),
            summary: scenario.verdict_summary.to_string(),
        },
        non_mcp_is_complete: scenario.non_mcp_is_complete,
        reference_answer: scenario.reference_answer.map(|s| s.to_string()),
        quality: None,
        ensemble_quality: None,
        set_comparison: None,
        compilation_check: None,
        tier: scenario.tier,
    }
}

/// Fills in `effective_savings_pct` on a scenario report after set
/// comparison has been computed. Called from the runner after
/// `build_scenario_report` + set_compare.
pub fn fill_effective_savings(report: &mut ScenarioReport) {
    if let Some(ref sc) = report.set_comparison {
        if sc.recall > 0.0 {
            report.savings.effective_savings_pct =
                Some(report.savings.tokens_pct * sc.recall);
        }
    }
}

fn compute_savings(mcp: &SideReport, non_mcp: &SideReport) -> Savings {
    let tokens_pct = pct_reduction(mcp.tokens, non_mcp.tokens);
    let bytes_pct = pct_reduction(mcp.response_bytes, non_mcp.response_bytes);
    let latency_ratio = if mcp.latency.mean_us > 0.0 {
        non_mcp.latency.mean_us / mcp.latency.mean_us
    } else {
        0.0
    };
    Savings {
        tokens_pct,
        bytes_pct,
        latency_ratio,
        effective_savings_pct: None,
    }
}

fn pct_reduction(mcp: usize, non_mcp: usize) -> f64 {
    if non_mcp == 0 {
        return 0.0;
    }
    // Signed delta: positive = MCP wrote fewer tokens, negative = MCP wrote more.
    // Reporting the sign preserves information about ties that lean against MCP.
    let saved = non_mcp as f64 - mcp as f64;
    (saved / non_mcp as f64) * 100.0
}

/// Margin below which the decision is considered ambiguous and reported as tie.
/// Chosen at 5% because differences smaller than that are usually within
/// run-to-run noise even on a warm SQLite database.
const TIE_MARGIN_PCT: f64 = 5.0;

fn pick_winner(
    savings: &Savings,
    mcp: &SideReport,
    non_mcp: &SideReport,
    non_mcp_is_complete: bool,
) -> String {
    if mcp.error.is_some() && non_mcp.error.is_some() {
        return "both_failed".to_string();
    }
    if mcp.error.is_some() {
        return "non_mcp".to_string();
    }
    if non_mcp.error.is_some() {
        return "mcp".to_string();
    }
    // Correctness gate: when the non-MCP sim cannot produce a semantically
    // complete answer (e.g. `git log -- FILE` for co-change, which has no
    // pair information at all), comparing raw tokens is meaningless. The
    // smaller output is only smaller because it says less. We award MCP the
    // win and let the Markdown renderer annotate the row so a reader
    // understands why the bytes don't track the verdict.
    if !non_mcp_is_complete {
        return "mcp".to_string();
    }
    if savings.tokens_pct > TIE_MARGIN_PCT {
        "mcp".to_string()
    } else if savings.tokens_pct < -TIE_MARGIN_PCT {
        "non_mcp".to_string()
    } else {
        "tie".to_string()
    }
}

pub fn write_json(path: &Path, report: &BenchmarkReport) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| io::Error::other(format!("json serialize: {e}")))?;
    fs::write(path, json)
}

pub fn write_markdown(path: &Path, report: &BenchmarkReport) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, render_markdown(report))
}

pub fn render_markdown(report: &BenchmarkReport) -> String {
    let reused_count = report.scenarios.iter().filter(|s| s.non_mcp.reused).count();
    let incomplete_count = report
        .scenarios
        .iter()
        .filter(|s| !s.non_mcp_is_complete)
        .count();

    let any_quality = report.scenarios.iter().any(|s| s.quality.is_some());
    let any_grounding = report
        .scenarios
        .iter()
        .any(|s| s.mcp.grounding.is_some() || s.non_mcp.grounding.is_some());
    let any_ensemble = report
        .scenarios
        .iter()
        .any(|s| s.ensemble_quality.is_some());

    let mut out = String::new();
    out.push_str("# Qartez MCP Per-Tool Benchmark\n\n");
    out.push_str(&format!(
        "- Generated at: `{}` (unix)\n",
        report.generated_at_unix
    ));
    out.push_str(&format!(
        "- Git SHA: `{}`\n",
        report.git_sha.as_deref().unwrap_or("unknown")
    ));
    out.push_str(&format!("- Tokenizer: `{}`\n", report.tokenizer));
    out.push_str("- Winner column uses a 5% tie margin on token savings.\n");
    out.push_str("- Latency is the mean of trimmed samples; expect noisy ratios near 1×.\n");
    if reused_count > 0 {
        out.push_str(&format!(
            "- ⚠ {reused_count}/{total} scenarios reused the non-MCP side from a cached run (non-MCP latency below is stale).\n",
            total = report.scenarios.len()
        ));
    }
    if incomplete_count > 0 {
        out.push_str(&format!(
            "- ✱ {incomplete_count} scenario(s) marked with ✱ have an incomplete non-MCP sim — the non-MCP side cannot produce a comparable answer, so the token and Savings columns are shown as `—`. MCP is awarded the win on correctness; the Speedup column still reflects the real latency cost the non-MCP side paid for its partial output.\n",
        ));
    }
    out.push('\n');

    // Headline — the single aggregate figure a reader sees at the top of the
    // report. Uses the same formula as `aggregate_savings_pct` and the stdout
    // `print_summary` helper in `src/bin/benchmark.rs`, so all three outputs
    // report the same number. Rendered between the metadata bullets and the
    // per-tool matrix so it stays visible without duplicating the matrix's
    // level-2 heading.
    let sum_mcp_tokens: usize = report.scenarios.iter().map(|s| s.mcp.tokens).sum();
    let sum_non_mcp_tokens: usize = report.scenarios.iter().map(|s| s.non_mcp.tokens).sum();
    out.push_str("## Headline\n\n");
    match aggregate_savings_pct(report) {
        Some(pct) => {
            out.push_str(&format!(
                "**Aggregate token savings vs Glob+Grep+Read: {pct:+.1}%** (Σ MCP {sum_mcp_tokens} / Σ non-MCP {sum_non_mcp_tokens} tokens across {} scenarios)\n",
                report.scenarios.len(),
            ));
            if incomplete_count > 0 {
                out.push_str(&format!(
                    "\n_Note: {incomplete_count}/{total} scenario(s) have an incomplete non-MCP sim. Those rows still contribute their MCP tokens to both sums, so this headline is a conservative under-count of the real win._\n",
                    total = report.scenarios.len(),
                ));
            }
        }
        None => {
            out.push_str(
                "**Aggregate token savings vs Glob+Grep+Read: —** (Σ non-MCP tokens = 0; headline cannot be computed)\n",
            );
        }
    }

    if any_quality && let Some(q) = average_quality(report) {
        out.push_str(&format!(
            "\n**Avg LLM-judge quality ({model}): MCP {mcp_avg:.1}/10 vs non-MCP {non_mcp_avg:.1}/10** (n={n}; 5 axes: correctness/completeness/usability/groundedness/conciseness)\n",
            model = q.model,
            mcp_avg = q.mcp_avg,
            non_mcp_avg = q.non_mcp_avg,
            n = q.scored_count,
        ));
        if q.skipped_count > 0 {
            out.push_str(&format!(
                "\n_Note: {skipped}/{total} scenario(s) were not judged (errors, empty bodies, or `--judge` not covering them)._\n",
                skipped = q.skipped_count,
                total = report.scenarios.len(),
            ));
        }
    }
    out.push('\n');

    // Judge reliability section — slice C adds this block between the
    // Headline and the Matrix when `--judge-ensemble` produced any
    // ensemble rows. Renders Cohen's weighted κ + Landis-Koch band,
    // mean |Δ|, arbitration count, top-3 most-disputed scenarios, and
    // an optional Krippendorff's α row on the arbitrated subset. Exact
    // snippet from `docs/benchmark-v2/ensemble-and-agreement.md` §7.a.
    if any_ensemble && let Some(stats) = compute_agreement_stats(report) {
        render_judge_reliability(&mut out, report, &stats);
    }

    // Session cost context — frames token savings relative to the base
    // cost of a Claude Code session (~20k tokens for system prompt +
    // CLAUDE.md + tools).
    {
        /// Approximate token overhead of one empty Claude Code session
        /// (system prompt + CLAUDE.md + tool schemas).
        const SESSION_BASE_TOKENS: f64 = 20_000.0;

        let savings_tokens = if sum_non_mcp_tokens > sum_mcp_tokens {
            sum_non_mcp_tokens - sum_mcp_tokens
        } else {
            0
        };
        let sessions = savings_tokens as f64 / SESSION_BASE_TOKENS;
        out.push_str("## Session cost context\n\n");
        out.push_str(&format!(
            "Typical Claude Code session base cost: ~20,000 tokens (system prompt + CLAUDE.md + tools).\n\
             MCP tool savings this run: {savings_tokens} tokens ({sessions:.1} empty sessions).\n\n",
        ));
    }

    let any_set_comparison = report
        .scenarios
        .iter()
        .any(|s| s.set_comparison.is_some());
    let any_compilation_check = report
        .scenarios
        .iter()
        .any(|s| s.compilation_check.is_some());

    out.push_str("## Matrix\n\n");
    let (matrix_header, matrix_sep) = matrix_header_and_sep(
        any_quality,
        any_grounding,
        any_set_comparison,
        any_compilation_check,
    );
    out.push_str(&matrix_header);
    out.push_str(&matrix_sep);
    for r in &report.scenarios {
        let marker = if r.non_mcp_is_complete { "" } else { " ✱" };
        let (non_mcp_tok_cell, savings_cell) = if r.non_mcp_is_complete {
            (
                format!("{}", r.non_mcp.tokens),
                format!("{:+.1}%", r.savings.tokens_pct),
            )
        } else {
            ("—".to_string(), "—".to_string())
        };

        let mut row = format!(
            "| `{tool}`{marker} | {mcp_tok} | {non_mcp_tok} | {savings} | {mcp_ms:.2} | {non_mcp_ms:.2} | {speedup:.2}× |",
            tool = r.tool,
            marker = marker,
            mcp_tok = r.mcp.tokens,
            non_mcp_tok = non_mcp_tok_cell,
            savings = savings_cell,
            mcp_ms = r.mcp.latency.mean_us / 1000.0,
            non_mcp_ms = r.non_mcp.latency.mean_us / 1000.0,
            speedup = r.savings.latency_ratio,
        );

        if any_set_comparison {
            let (prec_cell, recall_cell, eff_cell) = match &r.set_comparison {
                Some(sc) => (
                    format!("{:.2}", sc.precision),
                    format!("{:.2}", sc.recall),
                    r.savings
                        .effective_savings_pct
                        .map(|e| format!("{e:+.1}%"))
                        .unwrap_or_else(|| "—".to_string()),
                ),
                None => ("—".to_string(), "—".to_string(), "—".to_string()),
            };
            row.push_str(&format!(" {prec_cell} | {recall_cell} | {eff_cell} |"));
        }
        if any_compilation_check {
            let compile_cell = match &r.compilation_check {
                Some(c) if c.passed => "✓",
                Some(_) => "✗",
                None => "—",
            };
            row.push_str(&format!(" {compile_cell} |"));
        }
        if any_quality {
            let quality_cell = match &r.quality {
                Some(q) => format!("{:.1} / {:.1}", q.mcp.average(), q.non_mcp.average()),
                None => "—".to_string(),
            };
            row.push_str(&format!(" {quality_cell} |"));
        }
        if any_grounding {
            row.push_str(&format!(" {} |", grounding_matrix_cell(r)));
        }
        row.push_str(&format!(" **{}** |\n", r.verdict.winner));
        out.push_str(&row);
    }

    // Quality breakdown table — shows per-axis scores when any scenario
    // has been judged. Distinguishes LLM-scored (correctness, usability)
    // from programmatic (completeness, groundedness, conciseness) axes.
    if any_quality {
        let any_batch = report
            .scenarios
            .iter()
            .any(|s| s.quality.as_ref().is_some_and(|q| q.flags.contains(&"batch".to_string())));

        out.push_str("## Quality");
        if any_batch {
            out.push_str(" (LLM judge + programmatic)");
        }
        out.push_str("\n\n");
        out.push_str("| Tool | Correctness | Usability | Completeness");
        if any_batch {
            out.push('†');
        }
        out.push_str(" | Groundedness");
        if any_batch {
            out.push('†');
        }
        out.push_str(" | Conciseness");
        if any_batch {
            out.push('†');
        }
        out.push_str(" | Avg |\n");
        out.push_str("|---|:---:|:---:|:---:|:---:|:---:|:---:|\n");
        for r in &report.scenarios {
            if let Some(q) = &r.quality {
                out.push_str(&format!(
                    "| `{tool}` | {c_m}/{c_n} | {u_m}/{u_n} | {comp_m}/{comp_n} | {g_m}/{g_n} | {conc_m}/{conc_n} | {avg_m:.1}/{avg_n:.1} |\n",
                    tool = r.tool,
                    c_m = q.mcp.correctness, c_n = q.non_mcp.correctness,
                    u_m = q.mcp.usability, u_n = q.non_mcp.usability,
                    comp_m = q.mcp.completeness, comp_n = q.non_mcp.completeness,
                    g_m = q.mcp.groundedness, g_n = q.non_mcp.groundedness,
                    conc_m = q.mcp.conciseness, conc_n = q.non_mcp.conciseness,
                    avg_m = q.mcp.average(), avg_n = q.non_mcp.average(),
                ));
            }
        }
        if any_batch {
            out.push_str(
                "\n† Programmatic (not LLM-scored). Correctness and Usability are LLM-scored via single batch call.\n\
                 Format: MCP/non-MCP.\n",
            );
            let n = report.scenarios.iter().filter(|s| s.quality.is_some()).count();
            out.push_str(&format!(
                "\nJudge token budget: ~37,000 tokens (1 batch call, {n} scenarios).\n",
            ));
        }
        out.push('\n');
    }

    out.push_str("\n## Per-tool detail\n\n");
    for r in &report.scenarios {
        if r.tier > 1 {
            out.push_str(&format!(
                "### `{}` — {} (tier {})\n\n",
                r.tool, r.scenario_id, r.tier,
            ));
        } else {
            out.push_str(&format!("### `{}` — {}\n\n", r.tool, r.scenario_id));
        }
        out.push_str(&format!("{}\n\n", r.description));

        out.push_str("**MCP side**\n\n");
        out.push_str(&format!(
            "- Args: `{}`\n",
            r.mcp
                .args
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_default()
        ));
        out.push_str(&format!(
            "- Response: {} bytes → {} tokens (naive {})\n",
            r.mcp.response_bytes, r.mcp.tokens, r.mcp.naive_tokens
        ));
        out.push_str(&format!(
            "- Latency: mean {:.3} ms, p50 {:.3} ms, p95 {:.3} ms, σ {:.3} ms (n={})\n",
            r.mcp.latency.mean_us / 1000.0,
            r.mcp.latency.p50_us / 1000.0,
            r.mcp.latency.p95_us / 1000.0,
            r.mcp.latency.stdev_us / 1000.0,
            r.mcp.latency.samples,
        ));
        if let Some(err) = &r.mcp.error {
            out.push_str(&format!("- **ERROR:** `{err}`\n"));
        }

        out.push_str("\n**Non-MCP side**");
        if r.non_mcp.reused {
            out.push_str(" (reused from cache — latency is historical)");
        }
        if !r.non_mcp_is_complete {
            out.push_str(" ✱ **incomplete** — the step sequence below does not produce a comparable answer; byte/token counts are noise, not a measure of efficiency");
        }
        out.push_str("\n\n");
        if let Some(steps) = &r.non_mcp.steps {
            out.push_str("- Steps:\n");
            for step in steps {
                out.push_str(&format!("  - `{step}`\n"));
            }
        }
        out.push_str(&format!(
            "- Response: {} bytes → {} tokens (naive {})\n",
            r.non_mcp.response_bytes, r.non_mcp.tokens, r.non_mcp.naive_tokens
        ));
        out.push_str(&format!(
            "- Latency: mean {:.3} ms, p50 {:.3} ms, p95 {:.3} ms, σ {:.3} ms (n={})\n",
            r.non_mcp.latency.mean_us / 1000.0,
            r.non_mcp.latency.p50_us / 1000.0,
            r.non_mcp.latency.p95_us / 1000.0,
            r.non_mcp.latency.stdev_us / 1000.0,
            r.non_mcp.latency.samples,
        ));
        if let Some(err) = &r.non_mcp.error {
            out.push_str(&format!("- **ERROR:** `{err}`\n"));
        }

        if r.non_mcp_is_complete {
            out.push_str(&format!(
                "\n**Savings:** {:+.1}% tokens, {:+.1}% bytes, {:.2}× speedup\n\n",
                r.savings.tokens_pct, r.savings.bytes_pct, r.savings.latency_ratio,
            ));
        } else {
            out.push_str(&format!(
                "\n**Savings:** — tokens, — bytes, {:.2}× speedup (token comparison skipped: non-MCP sim is incomplete)\n\n",
                r.savings.latency_ratio,
            ));
        }

        if let Some(q) = &r.quality {
            render_per_scenario_quality(&mut out, q);
        }

        if let Some(ens) = &r.ensemble_quality {
            render_per_scenario_ensemble(&mut out, ens);
        }

        if r.mcp.grounding.is_some() || r.non_mcp.grounding.is_some() {
            render_per_scenario_grounding(&mut out, r);
        }

        out.push_str("**Pros (MCP-only)**\n\n");
        for p in &r.verdict.pros {
            out.push_str(&format!("- {p}\n"));
        }
        out.push_str("\n**Cons (what MCP loses vs Grep/Read)**\n\n");
        for c in &r.verdict.cons {
            out.push_str(&format!("- {c}\n"));
        }
        out.push_str(&format!("\n**Verdict:** {}\n\n", r.verdict.summary));
        out.push_str("---\n\n");
    }

    out
}

// ---------------------------------------------------------------------------
// Renderer helpers — Matrix columns, judge reliability section,
// per-scenario quality / ensemble / grounding blocks.
// ---------------------------------------------------------------------------

/// Builds the Matrix header row + separator row from the gate booleans.
fn matrix_header_and_sep(
    any_quality: bool,
    any_grounding: bool,
    any_set_comparison: bool,
    any_compilation_check: bool,
) -> (String, String) {
    let mut header =
        String::from("| Tool | MCP tok | non-MCP tok | Savings | MCP ms | non-MCP ms | Speedup |");
    let mut sep = String::from("|---|---:|---:|---:|---:|---:|---:|");
    if any_set_comparison {
        header.push_str(" Precision | Recall | Eff. Savings |");
        sep.push_str("---:|---:|---:|");
    }
    if any_compilation_check {
        header.push_str(" Compile |");
        sep.push_str(":---:|");
    }
    if any_quality {
        header.push_str(" Quality (MCP / non-MCP) |");
        sep.push_str(":---:|");
    }
    if any_grounding {
        header.push_str(" Grounding (MCP / non-MCP) |");
        sep.push_str(":---:|");
    }
    header.push_str(" Winner |\n");
    sep.push_str("---|\n");
    (header, sep)
}

/// Renders one `GroundingScores` side as `pct% (verified/total)` or a
/// literal `—` when the side carries no grounding entry. Used by the
/// Matrix column cell builder.
fn format_grounding_side(g: Option<&crate::benchmark::grounding::GroundingScores>) -> String {
    match g {
        Some(scores) if scores.total_claims > 0 => format!(
            "{pct:.0}% ({v}/{t})",
            pct = scores.score * 100.0,
            v = scores.verified_claims,
            t = scores.total_claims,
        ),
        _ => "—".to_string(),
    }
}

/// Builds the Matrix `Grounding` cell text for one row as
/// `{mcp} / {non_mcp}`. Called once per row under the `any_grounding`
/// gate.
fn grounding_matrix_cell(r: &ScenarioReport) -> String {
    format!(
        "{} / {}",
        format_grounding_side(r.mcp.grounding.as_ref()),
        format_grounding_side(r.non_mcp.grounding.as_ref()),
    )
}

/// Renders the `## Judge reliability` section per
/// `docs/benchmark-v2/ensemble-and-agreement.md` §7.a. Walks the
/// disputed rows, picks the top 3 by max |Δ|, and appends the
/// Krippendorff's α row only when the arbitrated subset was large
/// enough for the helper to return `Some`.
fn render_judge_reliability(out: &mut String, report: &BenchmarkReport, stats: &AgreementStats) {
    out.push_str("## Judge reliability\n\n");
    let total_pairs = stats.n_scenarios * 10;
    let pct_arbitrated = if stats.n_scenarios == 0 {
        0.0
    } else {
        stats.n_arbitrated as f64 / stats.n_scenarios as f64 * 100.0
    };
    let kappa_str = if stats.cohens_kappa.is_nan() {
        "—".to_string()
    } else {
        format!("{:.2}", stats.cohens_kappa)
    };
    out.push_str(&format!(
        "**Inter-rater agreement (Opus primary vs Sonnet secondary):**\n\
         Cohen's weighted κ = **{kappa}** (_{band}_), n = {n} scenarios × 10 rating pairs per scenario = {total} rating pairs.\n\
         Mean |Δ| per axis: {mean_delta:.2} / 10. Scenarios requiring arbitration: **{n_arb} / {n_sc}** ({pct:.1}%).\n\n\
         _Interpretation band (Landis & Koch 1977): κ < 0.2 poor, 0.2–0.4 fair, 0.4–0.6 moderate, 0.6–0.8 substantial, > 0.8 almost perfect._\n\n",
        kappa = kappa_str,
        band = landis_koch_band(stats.cohens_kappa),
        n = stats.n_scenarios,
        total = total_pairs,
        mean_delta = stats.mean_abs_delta,
        n_arb = stats.n_arbitrated,
        n_sc = stats.n_scenarios,
        pct = pct_arbitrated,
    ));

    let disputed = build_disputed_rows(report);
    if !disputed.is_empty() {
        out.push_str("**Top 3 most-disputed scenarios** (by max per-axis |Δ|):\n\n");
        out.push_str("| Scenario | Max Δ | Axis | Primary avg | Secondary avg | Final avg |\n");
        out.push_str("|---|---:|---|---:|---:|---:|\n");
        for row in disputed.iter().take(3) {
            let final_label = if row.arbiter_used {
                format!("{:.1} (arbiter)", row.final_avg)
            } else {
                format!("{:.1}", row.final_avg)
            };
            out.push_str(&format!(
                "| `{tool}/{id}` | {delta:.1} | {axis} | {p:.1} | {s:.1} | {final_cell} |\n",
                tool = row.tool,
                id = row.scenario_id,
                delta = row.max_delta,
                axis = row.axis_name,
                p = row.primary_avg,
                s = row.secondary_avg,
                final_cell = final_label,
            ));
        }
        out.push('\n');
    }

    if let Some(alpha) = stats.krippendorff_alpha {
        out.push_str(&format!(
            "_Krippendorff's α on the arbitrated subset (3 raters): {alpha:.2}._\n\n"
        ));
    }
}

/// Renders the per-scenario `**LLM-judge (model):**` line plus a
/// "Self-consistency runs" bullet. Exact format from
/// `docs/benchmark-v2/judge-core.md` §7 via the slice-C extension.
fn render_per_scenario_quality(out: &mut String, q: &QualityScores) {
    out.push_str(&format!(
        "**LLM-judge ({model}):** MCP {mcp_avg:.1}/10 (correctness {mc}, completeness {mcp_cp}, usability {mu}, groundedness {mg}, conciseness {mcon}) vs non-MCP {non_mcp_avg:.1}/10 (correctness {nc}, completeness {ncp}, usability {nu}, groundedness {ng}, conciseness {ncon}) — _{verdict}_\n",
        model = q.model,
        mcp_avg = q.mcp.average(),
        mc = q.mcp.correctness,
        mcp_cp = q.mcp.completeness,
        mu = q.mcp.usability,
        mg = q.mcp.groundedness,
        mcon = q.mcp.conciseness,
        non_mcp_avg = q.non_mcp.average(),
        nc = q.non_mcp.correctness,
        ncp = q.non_mcp.completeness,
        nu = q.non_mcp.usability,
        ng = q.non_mcp.groundedness,
        ncon = q.non_mcp.conciseness,
        verdict = q.verdict,
    ));
    if !q.runs.is_empty() || !q.flags.is_empty() {
        let flags_str = if q.flags.is_empty() {
            String::new()
        } else {
            format!("; flags: {}", q.flags.join(", "))
        };
        out.push_str(&format!(
            "- Self-consistency runs: {n_runs}{flags}\n",
            n_runs = q.runs.len(),
            flags = flags_str,
        ));
    }
    out.push('\n');
}

/// Renders the per-scenario `**LLM-judge ensemble:**` block per
/// `docs/benchmark-v2/ensemble-and-agreement.md` §7.b. Two variants:
/// agreement (primary + secondary + final mean) and disagreement
/// (primary + secondary + arbiter + final arbiter).
fn render_per_scenario_ensemble(out: &mut String, ens: &EnsembleQualityScores) {
    out.push_str("**LLM-judge ensemble:**\n");
    let max_delta = ens
        .abs_delta_per_axis
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    render_ensemble_line(out, "Primary", &ens.primary);
    render_ensemble_line(out, "Secondary", &ens.secondary);
    if ens.agreement {
        out.push_str(&format!(
            "- Agreement: yes (max |Δ| = {max_delta:.1} ≤ τ on all axes)\n"
        ));
        out.push_str(&format!(
            "- Final (mean of primary + secondary): MCP {mcp:.1}/10 vs non-MCP {non_mcp:.1}/10 → **avg {avg:.1} / 10**\n\n",
            mcp = ens.final_score.mcp.average(),
            non_mcp = ens.final_score.non_mcp.average(),
            avg = (ens.final_score.mcp.average() + ens.final_score.non_mcp.average()) / 2.0,
        ));
    } else {
        out.push_str(&format!(
            "- Agreement: **no** (max |Δ| = {max_delta:.1} > τ on at least one axis)\n"
        ));
        if let Some(arb) = &ens.arbiter {
            render_ensemble_line(out, "Arbiter", arb);
        }
        out.push_str(&format!(
            "- Final (arbiter): MCP {mcp:.1}/10 vs non-MCP {non_mcp:.1}/10 → **avg {avg:.1} / 10**\n\n",
            mcp = ens.final_score.mcp.average(),
            non_mcp = ens.final_score.non_mcp.average(),
            avg = (ens.final_score.mcp.average() + ens.final_score.non_mcp.average()) / 2.0,
        ));
    }
}

/// One line inside the `**LLM-judge ensemble:**` block. Emits a bullet
/// with the label (Primary / Secondary / Arbiter), model name, MCP and
/// non-MCP averages, and the verdict string.
fn render_ensemble_line(out: &mut String, label: &str, q: &QualityScores) {
    let mcp_avg = q.mcp.average();
    let non_mcp_avg = q.non_mcp.average();
    let pair_avg = (mcp_avg + non_mcp_avg) / 2.0;
    let verdict = if q.verdict.is_empty() {
        "—".to_string()
    } else {
        q.verdict.clone()
    };
    out.push_str(&format!(
        "- {label} ({model}): MCP {mcp_avg:.1} / non-MCP {non_mcp_avg:.1} (pair avg {pair_avg:.1}) — _{verdict}_\n",
        label = label,
        model = q.model,
        mcp_avg = mcp_avg,
        non_mcp_avg = non_mcp_avg,
        pair_avg = pair_avg,
        verdict = verdict,
    ));
}

/// Renders the per-scenario `**Grounding (claim-level fact check):**`
/// block per `docs/benchmark-v2/verifiable-grounding.md` §4. Emits one
/// bullet per side, with the verified fraction, per-category counts,
/// and the first few unverified claims. `—` is emitted for a side that
/// carries no grounding entry.
fn render_per_scenario_grounding(out: &mut String, r: &ScenarioReport) {
    out.push_str("**Grounding (claim-level fact check):**\n");
    out.push_str(&format!(
        "- MCP: {}\n",
        format_grounding_detail(r.mcp.grounding.as_ref()),
    ));
    out.push_str(&format!(
        "- non-MCP: {}\n\n",
        format_grounding_detail(r.non_mcp.grounding.as_ref()),
    ));
}

/// Formats one side's grounding detail line. Non-trivial: includes a
/// `{verified}/{total} verified (score); {files} files, {lines} lines,
/// {symbols} symbols; unverified: {list}` shape plus an optional
/// `degraded` note.
fn format_grounding_detail(g: Option<&crate::benchmark::grounding::GroundingScores>) -> String {
    let Some(scores) = g else {
        return "—".to_string();
    };
    if scores.total_claims == 0 {
        return "no verifiable claims extracted".to_string();
    }
    let mut unverified = if scores.unverified.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", scores.unverified.join(", "))
    };
    if scores.degraded {
        unverified.push_str(" (degraded: symbol check skipped)");
    }
    format!(
        "{verified}/{total} verified ({score:.3}); {files} files, {lines} lines, {symbols} symbols; unverified: {unverified}",
        verified = scores.verified_claims,
        total = scores.total_claims,
        score = scores.score,
        files = scores.file_claims,
        lines = scores.line_claims,
        symbols = scores.symbol_claims,
        unverified = unverified,
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct RegressionFinding {
    pub scenario_id: String,
    pub baseline_tokens: usize,
    pub current_tokens: usize,
    pub token_delta_pct: f64,
    pub baseline_latency_ms: f64,
    pub current_latency_ms: f64,
    pub latency_delta_pct: f64,
    pub severity: &'static str,
}

/// Minimum latency (in microseconds) at which relative regression percentages
/// become meaningful. Below this floor, measured-run variance on the order of
/// single-digit microseconds easily swamps 10-20% relative deltas, so we stop
/// counting those as regressions. Chosen at 200μs because the fastest MCP
/// tools in this repo sit in the 10-50μs band and can legitimately see 2-3×
/// relative swings from scheduler noise alone.
const LATENCY_NOISE_FLOOR_US: f64 = 200.0;

pub fn check_regression(
    current: &BenchmarkReport,
    baseline: &BenchmarkReport,
    threshold_pct: f64,
) -> Vec<RegressionFinding> {
    let mut findings = Vec::new();
    for cur in &current.scenarios {
        let Some(base) = baseline
            .scenarios
            .iter()
            .find(|b| b.scenario_id == cur.scenario_id)
        else {
            continue;
        };

        let token_delta_pct = if base.mcp.tokens == 0 {
            0.0
        } else {
            ((cur.mcp.tokens as f64 - base.mcp.tokens as f64) / base.mcp.tokens as f64) * 100.0
        };
        let latency_delta_pct = if base.mcp.latency.mean_us == 0.0 {
            0.0
        } else {
            ((cur.mcp.latency.mean_us - base.mcp.latency.mean_us) / base.mcp.latency.mean_us)
                * 100.0
        };
        // Suppress latency regressions when both readings sit in the noise
        // floor — relative deltas there are run-to-run variance, not real
        // perf changes.
        let latency_above_noise = cur.mcp.latency.mean_us > LATENCY_NOISE_FLOOR_US
            || base.mcp.latency.mean_us > LATENCY_NOISE_FLOOR_US;
        let latency_regressed = latency_delta_pct > threshold_pct && latency_above_noise;

        if token_delta_pct > threshold_pct || latency_regressed {
            let severity = if token_delta_pct > threshold_pct {
                "tokens"
            } else {
                "latency"
            };
            findings.push(RegressionFinding {
                scenario_id: cur.scenario_id.clone(),
                baseline_tokens: base.mcp.tokens,
                current_tokens: cur.mcp.tokens,
                token_delta_pct,
                baseline_latency_ms: base.mcp.latency.mean_us / 1000.0,
                current_latency_ms: cur.mcp.latency.mean_us / 1000.0,
                latency_delta_pct,
                severity,
            });
        }
    }
    findings
}

fn git_sha() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// Threshold above which a per-tool savings range across languages is
/// considered "high divergence" and surfaces in section 3 of the
/// cross-language summary. 20 percentage points is roughly 4× the per-row
/// tie margin defined in [`TIE_MARGIN_PCT`], so a tool that crosses it is
/// behaving meaningfully differently across language ecosystems rather
/// than reporting noise.
const DIVERGENCE_HIGHLIGHT_PCT: f64 = 20.0;

/// Aggregates multiple per-language [`BenchmarkReport`]s into a single
/// cross-language Markdown summary.
///
/// Each entry of `reports` carries a `(label, report)` pair. The label is
/// used verbatim in the headline table column for the language; the
/// report's own `language` field is preserved in the per-language summary
/// section so callers can spot mismatches between the file name and the
/// stored language tag.
///
/// The output contains four sections:
///
/// 1. **Headline matrix** — one row per language, one column per tool, with
///    the per-tool MCP token savings rendered as `+xx.x%`. Cells where
///    `non_mcp_is_complete = false` (the non-MCP sim cannot produce a
///    comparable answer) render as `—` so a reader does not mistake an
///    incomplete sim for genuine MCP/non-MCP parity.
/// 2. **Per-language summary** — one row per language with fixture name,
///    file/symbol counts (extracted from the `qartez_map` preview when
///    present), aggregate savings, and the win/tie/non-MCP/error tally.
/// 3. **Cross-language divergence** — for each tool, the savings range
///    (max minus min across languages). Tools whose range exceeds
///    [`DIVERGENCE_HIGHLIGHT_PCT`] are flagged because the harness is
///    delivering meaningfully different signals per language and a reader
///    should investigate the per-language report before trusting the
///    aggregate.
/// 4. **Known gotchas roll-up** — short hand-written notes per language
///    pointing readers at the full per-language gotchas section. The
///    per-language gotchas were authored by four different agents with
///    slightly different formatting, so the roll-up is hard-coded here
///    rather than scraped.
pub fn write_cross_language_summary(
    path: &Path,
    reports: &[(String, BenchmarkReport)],
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, render_cross_language_summary(reports))
}

/// Renders the cross-language summary Markdown without writing it to disk.
/// Split out from [`write_cross_language_summary`] so unit tests can assert
/// against the output without touching the filesystem.
pub fn render_cross_language_summary(reports: &[(String, BenchmarkReport)]) -> String {
    let mut out = String::new();
    out.push_str("# Qartez MCP Cross-Language Benchmark Summary\n\n");
    out.push_str(&format!(
        "- Generated at: `{}` (unix)\n",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    ));
    out.push_str(&format!("- Languages: {}\n", reports.len()));
    out.push_str(
        "- Each row is one language profile; each tool is a column. \
         Cells render `+xx.x%` MCP token savings; `—` means the non-MCP \
         sim is incomplete for that scenario and the comparison is not \
         meaningful.\n",
    );
    out.push_str(
        "- Aggregate savings = `(Σ non-MCP tokens − Σ MCP tokens) / Σ non-MCP tokens` \
         across the 17 scenarios per language. Incomplete rows still \
         contribute their MCP tokens to the numerator, so this is a \
         conservative under-count when the per-language gotchas note many \
         empty non-MCP baselines.\n",
    );
    out.push('\n');

    if reports.is_empty() {
        out.push_str("_No language reports supplied — nothing to summarize._\n");
        return out;
    }

    let tools = collect_tool_order(reports);

    out.push_str("## Section 1 — Headline savings matrix\n\n");
    render_headline_matrix(&mut out, reports, &tools);

    out.push_str("\n## Section 2 — Per-language summary\n\n");
    render_per_language_summary(&mut out, reports);

    out.push_str("\n## Section 3 — Cross-language divergence\n\n");
    render_divergence_table(&mut out, reports, &tools);

    out.push_str("\n## Section 4 — Known gotchas roll-up\n\n");
    render_gotchas_rollup(&mut out, reports);

    out
}

/// Collects the union of tool names across reports while preserving the
/// order they first appear in. The Rust report sets the canonical order
/// (since it carries the original 17 scenarios in the order they were
/// authored), and any extra tools added by Wave 2 profiles are appended.
fn collect_tool_order(reports: &[(String, BenchmarkReport)]) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut order: Vec<String> = Vec::new();
    for (_label, r) in reports {
        for s in &r.scenarios {
            if seen.insert(s.tool.clone()) {
                order.push(s.tool.clone());
            }
        }
    }
    order
}

fn render_headline_matrix(
    out: &mut String,
    reports: &[(String, BenchmarkReport)],
    tools: &[String],
) {
    out.push_str("| Language |");
    for t in tools {
        out.push_str(&format!(" `{t}` |"));
    }
    out.push('\n');
    out.push_str("|---|");
    for _ in tools {
        out.push_str("---:|");
    }
    out.push('\n');

    for (label, r) in reports {
        out.push_str(&format!("| {label} |"));
        let by_tool = first_savings_by_tool(r);
        for t in tools {
            match by_tool.get(t) {
                Some(Some(pct)) => out.push_str(&format!(" {pct:+.1}% |")),
                Some(None) => out.push_str(" — |"),
                None => out.push_str(" n/a |"),
            }
        }
        out.push('\n');
    }
}

/// Maps each tool name to its first scenario's MCP savings percentage in
/// the report, or `None` when the scenario is marked incomplete (so the
/// caller renders it as `—`). When a tool appears more than once in a
/// report, only the first scenario is used; this matches the harness's
/// "one canonical scenario per tool" expectation and keeps the headline
/// matrix from blowing up into a multi-row mess for languages whose
/// profiles add tool variants in the future.
fn first_savings_by_tool(r: &BenchmarkReport) -> BTreeMap<String, Option<f64>> {
    let mut out: BTreeMap<String, Option<f64>> = BTreeMap::new();
    for s in &r.scenarios {
        out.entry(s.tool.clone()).or_insert_with(|| {
            if s.non_mcp_is_complete {
                Some(s.savings.tokens_pct)
            } else {
                None
            }
        });
    }
    out
}

fn render_per_language_summary(out: &mut String, reports: &[(String, BenchmarkReport)]) {
    out.push_str("| Language | Fixture | Files | Symbols | Aggregate savings | MCP wins | Ties | non-MCP wins | Errors |\n");
    out.push_str("|---|---|---:|---:|---:|---:|---:|---:|---:|\n");

    for (label, r) in reports {
        let counts = win_counts(r);
        let agg = aggregate_savings_pct(r);
        let agg_cell = match agg {
            Some(pct) => format!("{pct:+.1}%"),
            None => "—".to_string(),
        };
        let (files, symbols) = extract_files_symbols(r);
        let files_cell = files.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
        let symbols_cell = symbols.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
        out.push_str(&format!(
            "| {label} | `{fixture}` | {files_cell} | {symbols_cell} | {agg_cell} | {} | {} | {} | {} |\n",
            counts.mcp,
            counts.tie,
            counts.non_mcp,
            counts.errors,
            fixture = fixture_name_for(label, r),
        ));
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct WinCounts {
    mcp: usize,
    tie: usize,
    non_mcp: usize,
    errors: usize,
}

fn win_counts(r: &BenchmarkReport) -> WinCounts {
    let mut c = WinCounts::default();
    for s in &r.scenarios {
        if s.mcp.error.is_some() || s.non_mcp.error.is_some() {
            c.errors += 1;
        }
        match s.verdict.winner.as_str() {
            "mcp" => c.mcp += 1,
            "tie" => c.tie += 1,
            "non_mcp" => c.non_mcp += 1,
            _ => {}
        }
    }
    c
}

/// Aggregate token savings across the report. Mirrors the formula used by
/// `print_summary` in `src/bin/benchmark.rs` so the cross-language headline
/// row matches the per-language stdout summary byte-for-byte.
fn aggregate_savings_pct(r: &BenchmarkReport) -> Option<f64> {
    let mcp_total: usize = r.scenarios.iter().map(|s| s.mcp.tokens).sum();
    let non_mcp_total: usize = r.scenarios.iter().map(|s| s.non_mcp.tokens).sum();
    if non_mcp_total == 0 {
        return None;
    }
    Some((non_mcp_total as f64 - mcp_total as f64) / non_mcp_total as f64 * 100.0)
}

/// Best-effort extraction of `(files, symbols)` from a report. The
/// `qartez_map_top5_concise` scenario's preview always begins with
/// `<files> files, <symbols> symbols (` because that's the header the
/// `format_concise` formatter writes. Parsing the preview is fragile by
/// design — when the preview format changes the cells fall back to `?`,
/// which the renderer treats as an unknown rather than crashing.
fn extract_files_symbols(r: &BenchmarkReport) -> (Option<usize>, Option<usize>) {
    let Some(scenario) = r
        .scenarios
        .iter()
        .find(|s| s.scenario_id == "qartez_map_top5_concise")
        .or_else(|| r.scenarios.iter().find(|s| s.tool == "qartez_map"))
    else {
        return (None, None);
    };
    parse_files_symbols(&scenario.mcp.response_preview)
}

fn parse_files_symbols(preview: &str) -> (Option<usize>, Option<usize>) {
    // Expected prefix: "<files> files, <symbols> symbols (...".
    // Anything that does not start with two integer fields separated by
    // ", " falls back to (None, None).
    let head = preview.split('(').next().unwrap_or("").trim();
    let mut parts = head.split(',');
    let files_part = parts.next().unwrap_or("").trim();
    let symbols_part = parts.next().unwrap_or("").trim();
    let files = files_part
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<usize>().ok());
    let symbols = symbols_part
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<usize>().ok());
    (files, symbols)
}

/// Maps a `(label, report)` to a human-readable fixture name. The label
/// is the file stem the loader saw on disk (e.g. `typescript`); when
/// possible, prefer the language tag from inside the report so a
/// hand-renamed `reports/benchmark-zod-v4.json` still surfaces as
/// `typescript`. The fixture-specific name (e.g. `zod`, `httpx`,
/// `cobra`, `jackson-core`) is then derived from a hard-coded table
/// — Wave 2 fixtures are pinned in `benchmarks/fixtures.toml` and only
/// change when the team explicitly bumps them, so a static map is the
/// least-fragile place to store the mapping.
fn fixture_name_for(label: &str, r: &BenchmarkReport) -> String {
    let lang = r.language.as_str();
    let key = if !lang.is_empty() && lang != "rust" {
        lang
    } else {
        label
    };
    match key {
        "rust" => "qartez-mcp (self)".to_string(),
        "typescript" => "colinhacks/zod".to_string(),
        "python" => "encode/httpx".to_string(),
        "go" => "spf13/cobra".to_string(),
        "java" => "FasterXML/jackson-core".to_string(),
        other => other.to_string(),
    }
}

fn render_divergence_table(
    out: &mut String,
    reports: &[(String, BenchmarkReport)],
    tools: &[String],
) {
    let mut rows: Vec<DivergenceRow> = Vec::new();
    for tool in tools {
        let mut samples: Vec<(String, f64)> = Vec::new();
        for (label, r) in reports {
            for s in &r.scenarios {
                if &s.tool == tool && s.non_mcp_is_complete {
                    samples.push((label.clone(), s.savings.tokens_pct));
                    break;
                }
            }
        }
        if samples.len() < 2 {
            continue;
        }
        let min = samples
            .iter()
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let max = samples
            .iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let (Some((min_lang, min_pct)), Some((max_lang, max_pct))) = (min, max) else {
            continue;
        };
        rows.push(DivergenceRow {
            tool: tool.clone(),
            range: max_pct - min_pct,
            min_pct: *min_pct,
            min_lang: min_lang.clone(),
            max_pct: *max_pct,
            max_lang: max_lang.clone(),
        });
    }
    rows.sort_by(|a, b| {
        b.range
            .partial_cmp(&a.range)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if rows.is_empty() {
        out.push_str("_Not enough comparable rows to compute divergence._\n");
        return;
    }

    out.push_str(
        "Sorted by `max − min` savings range across languages. \
         Rows where the range exceeds 20 percentage points are flagged \
         with `!!` and indicate scenarios where the harness gives a \
         meaningfully different answer per language — typically a \
         hand-authored sim that misses the target language's idioms.\n\n",
    );
    out.push_str("| Tool | Range | Worst (lang / savings) | Best (lang / savings) | Flag |\n");
    out.push_str("|---|---:|---|---|:---:|\n");
    for row in rows {
        let flag = if row.range > DIVERGENCE_HIGHLIGHT_PCT {
            "**!!**"
        } else {
            ""
        };
        out.push_str(&format!(
            "| `{}` | {:.1}pp | {} ({:+.1}%) | {} ({:+.1}%) | {} |\n",
            row.tool, row.range, row.min_lang, row.min_pct, row.max_lang, row.max_pct, flag,
        ));
    }
}

#[derive(Debug, Clone)]
struct DivergenceRow {
    tool: String,
    range: f64,
    min_pct: f64,
    min_lang: String,
    max_pct: f64,
    max_lang: String,
}

/// Hard-coded gotchas roll-up keyed on the language identifier inside
/// each report. The full per-language gotchas live at the bottom of the
/// `reports/benchmark-<lang>.md` files; this short version exists so a
/// reader of the cross-language summary does not need to context-switch
/// between five files to understand why a column looks the way it does.
fn render_gotchas_rollup(out: &mut String, reports: &[(String, BenchmarkReport)]) {
    for (label, r) in reports {
        let lang = if r.language.is_empty() {
            label.as_str()
        } else {
            r.language.as_str()
        };
        out.push_str(&format!("### {label}\n\n"));
        for line in gotchas_for(lang) {
            out.push_str(&format!("- {line}\n"));
        }
        out.push('\n');
    }
}

/// Returns the headline gotcha bullets for a language. The strings are
/// hand-written so they survive Wave 2's per-agent formatting drift; see
/// `reports/benchmark-<lang>.md` for the full discussion.
fn gotchas_for(language: &str) -> &'static [&'static str] {
    match language {
        "rust" => &[
            "Self-bench against the qartez-mcp repo itself; this is the canonical baseline that the regression gate compares every other run to.",
            "All 17 scenarios produce comparable non-MCP output, so the aggregate savings figure is a clean apples-to-apples number.",
        ],
        "typescript" => &[
            "Several non-MCP sim steps grep `^use crate::` and `^pub (fn|struct|...)`, which are Rust idioms that match zero TypeScript files. Affected: `qartez_deps`, `qartez_context`, `qartez_impact`, `qartez_unused`. Empty baselines collapse those rows to ties.",
            "`qartez_read` over-reads on hard-coded line ranges and shows a nominal non-MCP win because the sim happens to slice a shorter region of the wrong file. The 88.5% aggregate savings figure under-counts the real win.",
            "Auto-resolver picks `README.md` for `rename_file_source`, which makes the `qartez_rename_file` row near-noise.",
        ],
        "python" => &[
            "`qartez_calls_build_overview` errors because the auto-resolver picks the `Response` *class* (the largest exported symbol of `httpx/_models.py`) and `qartez_calls` only walks function bodies.",
            "Same Rust-regex problem hits `qartez_deps`, `qartez_unused`, and `qartez_rename_file` — non-MCP greps for `^use crate::` / `^pub` produce empty baselines.",
            "`qartez_unused` reports many helper functions because Python uses leading-underscore convention for export visibility; treat the list as a starting point for human review, not authoritative dead code.",
            "Hard-coded line ranges (`(260, 290)` etc.) over-read on small Python files; numbers are honest but weaker than Rust.",
        ],
        "go" => &[
            "Rust-syntax greps neutralize `qartez_deps`, `qartez_context`, `qartez_impact`, and `qartez_unused` (Go has no `pub` keyword and uses capitalization-based visibility instead).",
            "`exclude_globs` apply only to the non-MCP walker, not to the qartez re-index, so the auto-resolver picks targets from `doc/man_examples_test.go`. Several rows that depend on `Grep` reaching the chosen file therefore land at 0 non-MCP bytes / tie.",
            "`qartez_map` itself loses by a hair on the small cobra fixture (~36 non-test files): a raw `Glob **/*.go` is genuinely shorter than the PageRank summary. Flips on larger Go fixtures.",
        ],
        "java" => &[
            "Java imports are file-external package paths, so the indexer emits zero cross-file edges and PageRank flattens to `1/N`. The profile ships a hand-coded `target_override` (`jackson_core_targets`) to keep scenarios deterministic.",
            "Same Rust-regex story for `qartez_deps`, `qartez_context`, `qartez_impact`, `qartez_unused`, `qartez_rename_file` — Java imports start with `import tools.jackson.core.…`, never `use crate::`.",
            "`qartez_refs` and `qartez_calls` show non-MCP wins on jackson-core because the chosen targets (`createParser`, `getValueAsBoolean`) have many short references that grep collects cheaply, while MCP collects full call graphs.",
            "`qartez_project` returns 0 bytes because jackson-core's `pom.xml` is not a flat manifest the formatter recognizes — known limitation.",
        ],
        _ => &["No hand-written gotcha summary for this language; see the per-language report."],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_side(tokens: usize) -> SideReport {
        SideReport {
            response_bytes: tokens * 4,
            response_preview: format!("{tokens} files, 99 symbols (rank ...)"),
            tokens,
            naive_tokens: tokens,
            latency: LatencyStats {
                mean_us: 100.0,
                stdev_us: 0.0,
                p50_us: 100.0,
                p95_us: 100.0,
                samples: 1,
            },
            error: None,
            args: None,
            steps: None,
            reused: false,
            full_output: String::new(),
            grounding: None,
        }
    }

    fn dummy_scenario(
        tool: &str,
        scenario_id: &str,
        mcp_tokens: usize,
        non_mcp_tokens: usize,
        complete: bool,
    ) -> ScenarioReport {
        let mcp = dummy_side(mcp_tokens);
        let non_mcp = dummy_side(non_mcp_tokens);
        let savings = if non_mcp_tokens > 0 {
            let pct = (non_mcp_tokens as f64 - mcp_tokens as f64) / non_mcp_tokens as f64 * 100.0;
            Savings {
                tokens_pct: pct,
                bytes_pct: pct,
                latency_ratio: 1.0,
                effective_savings_pct: None,
            }
        } else {
            Savings {
                tokens_pct: 0.0,
                bytes_pct: 0.0,
                latency_ratio: 1.0,
                effective_savings_pct: None,
            }
        };
        ScenarioReport {
            tool: tool.to_string(),
            scenario_id: scenario_id.to_string(),
            description: String::new(),
            mcp,
            non_mcp,
            savings,
            verdict: Verdict {
                winner: "mcp".to_string(),
                pros: Vec::new(),
                cons: Vec::new(),
                summary: String::new(),
            },
            non_mcp_is_complete: complete,
            reference_answer: None,
            quality: None,
            ensemble_quality: None,
            set_comparison: None,
            compilation_check: None,
            tier: 1,
        }
    }

    #[test]
    fn parse_files_symbols_extracts_header() {
        let preview = "417 files, 5981 symbols (rank path PR exp →blast)\n1 packages/...";
        assert_eq!(parse_files_symbols(preview), (Some(417), Some(5981)));
    }

    #[test]
    fn parse_files_symbols_falls_back_on_garbage() {
        assert_eq!(parse_files_symbols(""), (None, None));
        assert_eq!(parse_files_symbols("not a header"), (None, None));
    }

    #[test]
    fn render_summary_handles_empty_input() {
        let s = render_cross_language_summary(&[]);
        assert!(s.contains("No language reports supplied"));
    }

    #[test]
    fn render_summary_emits_all_sections() {
        let rust = BenchmarkReport::new_with_language(
            vec![dummy_scenario(
                "qartez_map",
                "qartez_map_top5_concise",
                80,
                400,
                true,
            )],
            "rust".to_string(),
        );
        let ts = BenchmarkReport::new_with_language(
            vec![dummy_scenario(
                "qartez_map",
                "qartez_map_top5_concise",
                90,
                100,
                true,
            )],
            "typescript".to_string(),
        );
        let summary = render_cross_language_summary(&[
            ("rust".to_string(), rust),
            ("typescript".to_string(), ts),
        ]);
        assert!(summary.contains("Section 1 — Headline savings matrix"));
        assert!(summary.contains("Section 2 — Per-language summary"));
        assert!(summary.contains("Section 3 — Cross-language divergence"));
        assert!(summary.contains("Section 4 — Known gotchas roll-up"));
        assert!(summary.contains("`qartez_map`"));
    }

    #[test]
    fn divergence_flag_fires_above_threshold() {
        let rust = BenchmarkReport::new_with_language(
            vec![dummy_scenario("qartez_deps", "qartez_deps_x", 10, 100, true)],
            "rust".to_string(),
        );
        let ts = BenchmarkReport::new_with_language(
            vec![dummy_scenario("qartez_deps", "qartez_deps_x", 99, 100, true)],
            "typescript".to_string(),
        );
        let summary = render_cross_language_summary(&[
            ("rust".to_string(), rust),
            ("typescript".to_string(), ts),
        ]);
        assert!(summary.contains("**!!**"));
    }

    #[test]
    fn incomplete_rows_render_dash_in_matrix() {
        let rust = BenchmarkReport::new_with_language(
            vec![dummy_scenario("qartez_deps", "qartez_deps_x", 10, 100, false)],
            "rust".to_string(),
        );
        let summary = render_cross_language_summary(&[("rust".to_string(), rust)]);
        // The matrix renders the per-tool cell as `—` when the scenario
        // is incomplete, exactly as the per-language reports do.
        assert!(summary.contains("| — |"));
    }

    #[test]
    fn render_markdown_emits_headline_before_matrix() {
        let report = BenchmarkReport::new_with_language(
            vec![
                dummy_scenario("qartez_map", "qartez_map_top5_concise", 80, 400, true),
                dummy_scenario("qartez_find", "qartez_find_x", 20, 600, true),
            ],
            "rust".to_string(),
        );
        let md = render_markdown(&report);
        // Expected figure: (400 + 600 − 80 − 20) / (400 + 600) = 90%.
        assert!(md.contains("## Headline"), "headline section missing: {md}");
        assert!(
            md.contains("**Aggregate token savings vs Glob+Grep+Read: +90.0%**"),
            "aggregate percent missing: {md}"
        );
        assert!(
            md.contains("Σ MCP 100 / Σ non-MCP 1000 tokens across 2 scenarios"),
            "totals line missing: {md}"
        );
        let headline_pos = md.find("## Headline").expect("headline present");
        let matrix_pos = md.find("## Matrix").expect("matrix present");
        assert!(
            headline_pos < matrix_pos,
            "headline must appear before the matrix"
        );
    }

    #[test]
    fn render_markdown_headline_notes_incomplete_rows() {
        let report = BenchmarkReport::new_with_language(
            vec![
                dummy_scenario("qartez_map", "qartez_map_top5_concise", 80, 400, true),
                dummy_scenario("qartez_cochange", "qartez_cochange_x", 50, 100, false),
            ],
            "rust".to_string(),
        );
        let md = render_markdown(&report);
        assert!(md.contains("## Headline"));
        assert!(
            md.contains("conservative under-count"),
            "incomplete-row note missing: {md}"
        );
    }

    #[test]
    fn render_markdown_headline_handles_zero_non_mcp_total() {
        let report = BenchmarkReport::new_with_language(
            vec![dummy_scenario(
                "qartez_map",
                "qartez_map_top5_concise",
                80,
                0,
                true,
            )],
            "rust".to_string(),
        );
        let md = render_markdown(&report);
        assert!(
            md.contains("**Aggregate token savings vs Glob+Grep+Read: —**"),
            "zero-denominator headline missing: {md}"
        );
    }

    #[test]
    fn effective_savings_pct_computes_correctly() {
        let mut r = dummy_scenario("qartez_find", "test", 200, 1000, true);
        // savings.tokens_pct = (1000-200)/1000 * 100 = 80%
        assert!((r.savings.tokens_pct - 80.0).abs() < 0.1);

        // No set comparison → effective_savings_pct stays None.
        fill_effective_savings(&mut r);
        assert!(r.savings.effective_savings_pct.is_none());

        // With set comparison (recall=0.9) → 80% * 0.9 = 72%.
        r.set_comparison = Some(crate::benchmark::set_compare::SetComparisonScores {
            mcp_items: 10,
            non_mcp_items: 10,
            intersection: 9,
            precision: 0.9,
            recall: 0.9,
            mcp_only: Vec::new(),
            non_mcp_only: Vec::new(),
        });
        fill_effective_savings(&mut r);
        let eff = r.savings.effective_savings_pct.unwrap();
        assert!((eff - 72.0).abs() < 0.1, "expected ~72, got {eff}");
    }

    #[test]
    fn effective_savings_none_for_zero_recall() {
        let mut r = dummy_scenario("qartez_find", "test", 200, 1000, true);
        r.set_comparison = Some(crate::benchmark::set_compare::SetComparisonScores {
            mcp_items: 5,
            non_mcp_items: 10,
            intersection: 0,
            precision: 0.0,
            recall: 0.0,
            mcp_only: Vec::new(),
            non_mcp_only: Vec::new(),
        });
        fill_effective_savings(&mut r);
        assert!(r.savings.effective_savings_pct.is_none());
    }

    #[test]
    fn render_markdown_contains_session_cost_context() {
        let report = BenchmarkReport::new_with_language(
            vec![dummy_scenario("qartez_map", "test", 100, 1000, true)],
            "rust".to_string(),
        );
        let md = render_markdown(&report);
        assert!(md.contains("## Session cost context"), "missing section header");
        assert!(md.contains("~20,000 tokens"), "missing base cost");
        assert!(md.contains("empty sessions"), "missing session count");
    }

    // -- slice C: renderer extensions + agreement stats ------------------

    use crate::benchmark::judge::{
        EnsembleQualityScores, PerRunScores, Position, QualityScores, SideQuality,
    };

    fn side_quality(value: u8) -> SideQuality {
        SideQuality {
            correctness: value,
            completeness: value,
            usability: value,
            groundedness: value,
            conciseness: value,
        }
    }

    fn make_quality(model: &str, mcp: u8, non_mcp: u8) -> QualityScores {
        QualityScores {
            mcp: side_quality(mcp),
            non_mcp: side_quality(non_mcp),
            verdict: format!("{model} verdict"),
            model: model.to_string(),
            runs: vec![PerRunScores {
                position: Position::McpFirst,
                run_index: 0,
                mcp: side_quality(mcp),
                non_mcp: side_quality(non_mcp),
                verdict: format!("{model} run0"),
            }],
            flags: Vec::new(),
            reference_answer_used: false,
        }
    }

    fn ensemble_scenario(
        tool: &str,
        scenario_id: &str,
        primary: QualityScores,
        secondary: QualityScores,
        arbiter: Option<QualityScores>,
        agreement: bool,
        delta: Vec<f64>,
    ) -> ScenarioReport {
        let final_score = arbiter.clone().unwrap_or_else(|| {
            crate::benchmark::judge::elementwise_mean_quality(&primary, &secondary)
        });
        let ens = EnsembleQualityScores {
            primary,
            secondary,
            arbiter,
            final_score: final_score.clone(),
            agreement,
            abs_delta_per_axis: delta,
        };
        let mut s = dummy_scenario(tool, scenario_id, 80, 200, true);
        s.quality = Some(final_score);
        s.ensemble_quality = Some(ens);
        s
    }

    fn grounding_scores(
        verified: usize,
        total: usize,
    ) -> crate::benchmark::grounding::GroundingScores {
        crate::benchmark::grounding::GroundingScores {
            total_claims: total,
            verified_claims: verified,
            file_claims: total,
            line_claims: 0,
            symbol_claims: 0,
            verified_files: verified,
            verified_lines: 0,
            verified_symbols: 0,
            unverified: Vec::new(),
            score: if total == 0 {
                0.0
            } else {
                verified as f64 / total as f64
            },
            elapsed_us: 1,
            degraded: false,
        }
    }

    #[test]
    fn render_markdown_legacy_report_unchanged() {
        let report = BenchmarkReport::new_with_language(
            vec![dummy_scenario(
                "qartez_map",
                "qartez_map_top5_concise",
                80,
                400,
                true,
            )],
            "rust".to_string(),
        );
        let md = render_markdown(&report);
        assert!(
            md.contains(
                "| Tool | MCP tok | non-MCP tok | Savings | MCP ms | non-MCP ms | Speedup | Winner |"
            ),
            "legacy 8-col matrix header missing: {md}"
        );
        assert!(
            !md.contains("Quality (MCP"),
            "Quality column leaked into unjudged report: {md}"
        );
        assert!(
            !md.contains("Grounding ("),
            "Grounding column leaked into unjudged report: {md}"
        );
        assert!(
            !md.contains("## Judge reliability"),
            "Judge reliability section leaked into unjudged report: {md}"
        );
    }

    #[test]
    fn render_markdown_quality_and_grounding_columns() {
        let mut s = dummy_scenario("qartez_find", "qartez_find_x", 80, 400, true);
        s.quality = Some(make_quality("claude-opus-4-6", 7, 5));
        s.mcp.grounding = Some(grounding_scores(9, 10));
        s.non_mcp.grounding = Some(grounding_scores(6, 10));
        let report = BenchmarkReport::new_with_language(vec![s], "rust".to_string());
        let md = render_markdown(&report);
        assert!(
            md.contains("Quality (MCP / non-MCP)"),
            "quality column missing: {md}"
        );
        assert!(
            md.contains("Grounding (MCP / non-MCP)"),
            "grounding column missing: {md}"
        );
        assert!(
            md.contains("90% (9/10)"),
            "mcp grounding cell missing: {md}"
        );
        assert!(
            md.contains("60% (6/10)"),
            "non-mcp grounding cell missing: {md}"
        );
        assert!(
            md.contains("**Avg LLM-judge quality"),
            "headline missing: {md}"
        );
    }

    #[test]
    fn render_markdown_ensemble_section_appears() {
        let primary = make_quality("claude-opus-4-6", 7, 5);
        let secondary = make_quality("claude-sonnet-4-6", 7, 5);
        let delta = vec![0.0; 10];
        let s = ensemble_scenario(
            "qartez_find",
            "qartez_find_x",
            primary,
            secondary,
            None,
            true,
            delta,
        );
        let report = BenchmarkReport::new_with_language(vec![s], "rust".to_string());
        let md = render_markdown(&report);
        assert!(
            md.contains("## Judge reliability"),
            "Judge reliability section missing: {md}"
        );
        assert!(
            md.contains("Cohen's weighted κ"),
            "kappa caption missing: {md}"
        );
        assert!(
            md.contains("**LLM-judge ensemble:**"),
            "per-scenario ensemble block missing: {md}"
        );
    }

    #[test]
    fn compute_agreement_stats_no_ensemble_returns_none() {
        let report = BenchmarkReport::new_with_language(
            vec![dummy_scenario("qartez_map", "x", 80, 400, true)],
            "rust".to_string(),
        );
        assert!(compute_agreement_stats(&report).is_none());
    }

    #[test]
    fn compute_agreement_stats_with_ensemble_returns_some() {
        let primary_a = make_quality("opus", 7, 5);
        let secondary_a = make_quality("sonnet", 7, 5);
        let s1 = ensemble_scenario(
            "qartez_find",
            "a",
            primary_a,
            secondary_a,
            None,
            true,
            vec![0.0; 10],
        );

        let primary_b = make_quality("opus", 7, 10);
        let secondary_b = make_quality("sonnet", 5, 7);
        let s2 = ensemble_scenario(
            "qartez_grep",
            "b",
            primary_b,
            secondary_b,
            None,
            true,
            vec![2.0; 10],
        );

        let report = BenchmarkReport::new_with_language(vec![s1, s2], "rust".to_string());
        let stats = compute_agreement_stats(&report).expect("stats present");
        assert_eq!(stats.n_scenarios, 2);
        assert!(
            (-1.0..=1.0).contains(&stats.cohens_kappa) || stats.cohens_kappa.is_nan(),
            "kappa out of range: {}",
            stats.cohens_kappa
        );
        assert!((stats.mean_abs_delta - 1.0).abs() < 1e-9);
    }

    #[test]
    fn top_3_most_disputed_sorting() {
        let mut rows: Vec<ScenarioReport> = Vec::new();
        for (idx, max_delta) in [3.0, 1.0, 5.0, 2.0, 4.0].iter().enumerate() {
            let mut delta = vec![0.0; 10];
            delta[0] = *max_delta;
            let primary = make_quality("opus", 7, 5);
            let secondary = make_quality("sonnet", 7, 5);
            rows.push(ensemble_scenario(
                "qartez_find",
                &format!("s{idx}"),
                primary,
                secondary,
                None,
                *max_delta <= 2.0,
                delta,
            ));
        }
        let report = BenchmarkReport::new_with_language(rows, "rust".to_string());
        let disputed = build_disputed_rows(&report);
        // Descending by max_delta: 5.0, 4.0, 3.0, 2.0, 1.0
        let maxes: Vec<f64> = disputed.iter().map(|r| r.max_delta).collect();
        assert_eq!(maxes, vec![5.0, 4.0, 3.0, 2.0, 1.0]);
        let md = render_markdown(&report);
        // Rendered table shows the top 3 only.
        assert!(md.contains("5.0"));
        assert!(md.contains("4.0"));
        assert!(md.contains("3.0"));
    }

    #[test]
    fn landis_koch_band_thresholds() {
        assert_eq!(landis_koch_band(0.1), "poor");
        assert_eq!(landis_koch_band(0.3), "fair");
        assert_eq!(landis_koch_band(0.5), "moderate");
        assert_eq!(landis_koch_band(0.7), "substantial");
        assert_eq!(landis_koch_band(0.9), "almost perfect");
        assert_eq!(landis_koch_band(0.199), "poor");
        assert_eq!(landis_koch_band(0.399), "fair");
    }

    #[test]
    fn matrix_header_covers_all_gate_combinations() {
        let (no_gate, _) = matrix_header_and_sep(false, false, false, false);
        assert!(no_gate.contains("Winner"));
        assert!(!no_gate.contains("Quality"));
        assert!(!no_gate.contains("Grounding"));
        assert!(!no_gate.contains("Precision"));
        assert!(!no_gate.contains("Compile"));

        let (q_only, _) = matrix_header_and_sep(true, false, false, false);
        assert!(q_only.contains("Quality (MCP / non-MCP)"));

        let (grounding_only, _) = matrix_header_and_sep(false, true, false, false);
        assert!(grounding_only.contains("Grounding (MCP / non-MCP)"));

        let (all, _) = matrix_header_and_sep(true, true, true, true);
        assert!(all.contains("Quality ("));
        assert!(all.contains("Grounding ("));
        assert!(all.contains("Precision"));
        assert!(all.contains("Recall"));
        assert!(all.contains("Eff. Savings"));
        assert!(all.contains("Compile"));
    }

    #[test]
    fn render_ensemble_disagreement_variant_contains_arbiter() {
        let primary = make_quality("opus", 10, 5);
        let secondary = make_quality("sonnet", 3, 7);
        let arbiter = make_quality("claude-opus-4-6", 7, 5);
        let mut delta = vec![0.0; 10];
        delta[0] = 7.0;
        let s = ensemble_scenario(
            "qartez_find",
            "disputed",
            primary,
            secondary,
            Some(arbiter),
            false,
            delta,
        );
        let report = BenchmarkReport::new_with_language(vec![s], "rust".to_string());
        let md = render_markdown(&report);
        assert!(
            md.contains("**LLM-judge ensemble:**"),
            "ensemble block missing: {md}"
        );
        assert!(
            md.contains("Agreement: **no**"),
            "disagreement label missing: {md}"
        );
        assert!(
            md.contains("Arbiter (claude-opus-4-6)"),
            "arbiter line missing: {md}"
        );
        assert!(md.contains("Final (arbiter)"));
    }

    #[test]
    fn render_per_scenario_grounding_block_appears() {
        let mut s = dummy_scenario("qartez_find", "g_only", 80, 200, true);
        s.mcp.grounding = Some(grounding_scores(9, 10));
        s.non_mcp.grounding = Some(grounding_scores(0, 3));
        let report = BenchmarkReport::new_with_language(vec![s], "rust".to_string());
        let md = render_markdown(&report);
        assert!(
            md.contains("**Grounding (claim-level fact check):**"),
            "grounding detail block missing: {md}"
        );
        assert!(md.contains("9/10 verified"));
        assert!(md.contains("0/3 verified"));
    }
}
