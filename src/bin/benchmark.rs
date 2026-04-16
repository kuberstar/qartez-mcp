//! CLI entry point for the per-tool Qartez MCP benchmark harness.
//!
//! Usage:
//!
//! ```text
//! cargo run --release --features benchmark --bin benchmark -- \
//!     --project-root . \
//!     --out-json reports/benchmark.json \
//!     --out-md reports/benchmark.md
//! ```
//!
//! By default, the binary opens the project's existing
//! `.qartez/index.db` and runs all 17 scenarios against the Rust
//! profile. Pass `--lang <name>` to switch language profiles once a
//! Wave 2 agent has wired up the corresponding fixture and profile
//! module. Pass `--filter <substr>` to run only scenarios whose tool
//! name or id contains the substring.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;

use std::collections::HashMap;

use qartez_mcp::benchmark::{
    BenchmarkRunner, LatencyConfig, build_non_mcp_cache, grounding, judge, profiles,
    report::{self, BenchmarkReport, ScenarioReport},
    targets,
};
use qartez_mcp::server::QartezServer;
use qartez_mcp::storage;

#[derive(Parser, Debug)]
#[command(
    name = "benchmark",
    about = "Per-tool comparative benchmark for Qartez MCP vs Glob/Grep/Read"
)]
struct Cli {
    /// Project root to benchmark against. Defaults to the current directory.
    #[arg(long, default_value = ".")]
    project_root: PathBuf,

    /// Path to the qartez SQLite database. Defaults to `<project_root>/.qartez/index.db`.
    #[arg(long)]
    db: Option<PathBuf>,

    /// Output path for the JSON report.
    #[arg(long, default_value = "reports/benchmark.json")]
    out_json: PathBuf,

    /// Output path for the Markdown report.
    #[arg(long, default_value = "reports/benchmark.md")]
    out_md: PathBuf,

    /// Only run scenarios whose tool name or id contains this substring.
    #[arg(long)]
    filter: Option<String>,

    /// Language profile to use. Controls extension filters, exclude
    /// globs, and the pool of file/symbol targets the scenarios run
    /// against. `rust` is fully implemented; other languages require
    /// a Wave 2 profile module.
    #[arg(long, default_value = "rust")]
    lang: String,

    /// Fixture root for non-Rust profiles. When set, the benchmark
    /// resolves paths relative to `<fixture-root>/<profile.fixture_subdir>`.
    /// Defaults to the project root used for the Rust profile, which is
    /// the qartez-mcp repo itself.
    #[arg(long)]
    fixture_root: Option<PathBuf>,

    /// Warmup runs per scenario per side.
    #[arg(long, default_value_t = 3)]
    warmup: usize,

    /// Measured runs per scenario per side (before outlier trimming).
    #[arg(long, default_value_t = 7)]
    runs: usize,

    /// If set, compare the run against this baseline JSON and fail on regressions.
    #[arg(long)]
    baseline: Option<PathBuf>,

    /// Regression threshold in percent (default 10%).
    #[arg(long, default_value_t = 10.0)]
    threshold_pct: f64,

    /// If set, overwrite the baseline file with the current run.
    #[arg(long)]
    update_baseline: bool,

    /// Reuse the non-MCP side of each scenario from a prior benchmark JSON file.
    /// The non-MCP workflow (Glob/Grep/Read/git log) is invariant in the codebase,
    /// so it does not need to be re-measured when iterating on MCP-side changes.
    /// By default the cached git SHA must match the current one; pass
    /// --allow-stale-cache to ignore mismatches.
    #[arg(long)]
    reuse_non_mcp: Option<PathBuf>,

    /// Allow `--reuse-non-mcp` even when the cached run's git SHA differs.
    #[arg(long)]
    allow_stale_cache: bool,

    /// Aggregate this run with all sibling `reports/benchmark-*.json`
    /// files into a single cross-language summary at
    /// `reports/benchmark-summary.md`. Sibling files that fail to parse
    /// are skipped with a warning.
    #[arg(long)]
    cross_lang_summary: bool,

    /// Skip the benchmark run entirely and only regenerate the
    /// cross-language summary from existing `reports/benchmark*.json`
    /// files. Useful for CI when only the aggregator output needs to be
    /// refreshed. Implies `--cross-lang-summary`.
    #[arg(long)]
    cross_lang_summary_only: bool,

    /// Output path for the cross-language summary Markdown. Defaults to
    /// `reports/benchmark-summary.md`. Only used when one of the
    /// `--cross-lang-summary*` flags is set.
    #[arg(long, default_value = "reports/benchmark-summary.md")]
    cross_lang_summary_out: PathBuf,

    /// Skip the benchmark run entirely and only re-render the per-language
    /// Markdown report from an existing `--out-json` file. Useful when the
    /// Markdown renderer changes and the committed JSON baseline does not,
    /// so callers can refresh `reports/benchmark-<lang>.md` without paying
    /// the cost of a full re-measurement. Pass `--render-md-only-all` to
    /// re-render every `reports/benchmark*.json` sibling in one invocation.
    #[arg(long)]
    render_md_only: bool,

    /// Like `--render-md-only`, but processes every `reports/benchmark*.json`
    /// sibling under `<project_root>/reports/` and rewrites each matching
    /// `.md` file next to its JSON. Skips `benchmark-summary.md` because that
    /// file is generated by `--cross-lang-summary*` from a different code
    /// path. Useful after a renderer tweak to refresh all language reports
    /// in one shot.
    #[arg(long)]
    render_md_only_all: bool,

    /// Score each pair of (MCP, non-MCP) responses with a 5-axis
    /// rubric-anchored LLM judge via `claude -p`. Uses position swap,
    /// self-consistency averaging, and `--json-schema` enum anchors.
    /// Requires the Claude Code CLI on PATH.
    #[arg(long)]
    judge: bool,

    /// Claude Code model passed to `claude -p --model <model>` when
    /// `--judge` is set. Defaults to `claude-opus-4-6`.
    #[arg(long, default_value = qartez_mcp::benchmark::judge::DEFAULT_JUDGE_MODEL)]
    judge_model: String,

    /// Maximum number of parallel `claude -p` invocations when judging.
    #[arg(long, default_value_t = 12)]
    judge_workers: usize,

    /// Self-consistency count for `--judge`. Each scenario is judged
    /// `2 * n` times - `n` runs per position pass. With position swap
    /// n=1 still gives 2 independent runs per scenario. Set n=2 for
    /// higher confidence at 2x the cost.
    #[arg(long, default_value_t = 1)]
    judge_n: usize,

    /// Reuse the `quality` field of each scenario from a prior
    /// `--judge` benchmark JSON file when the scenario's fingerprint
    /// is unchanged. Dramatically reduces iteration time when
    /// developing a single MCP tool. Under `--judge-ensemble` the
    /// cache is consulted only for the primary judge's slot; the
    /// secondary and arbiter still run freshly.
    #[arg(long)]
    reuse_judge: Option<PathBuf>,

    /// Allow `--reuse-judge` even when the cached run's git SHA
    /// differs, AND treat every cached entry as a hit regardless of
    /// input fingerprint drift.
    #[arg(long)]
    allow_stale_judge_cache: bool,

    /// Force programmatic grounding verification on. Default is
    /// implied by `--judge`. Mutually exclusive with `--no-grounding`.
    #[arg(long)]
    grounding: bool,

    /// Force programmatic grounding off.
    #[arg(long)]
    no_grounding: bool,

    /// Opt into the two-judge ensemble with arbiter escalation.
    /// Implies `--judge` (auto-enables it). Scores each scenario
    /// with a primary model + a secondary, then escalates to an
    /// arbiter on disagreement above the per-axis threshold.
    #[arg(long)]
    judge_ensemble: bool,

    /// Secondary judge model for `--judge-ensemble`.
    #[arg(long, default_value = "claude-sonnet-4-6")]
    judge_model_secondary: String,

    /// Arbiter model for `--judge-ensemble`. DAFE-style: the arbiter
    /// sees both prior scores and tie-breaks.
    #[arg(long, default_value = qartez_mcp::benchmark::judge::DEFAULT_JUDGE_MODEL)]
    judge_arbiter: String,

    /// Per-axis absolute difference threshold above which the arbiter
    /// is called. 2.0 matches published LLM-judge noise floors.
    #[arg(long, default_value_t = 2.0)]
    judge_disagreement_threshold: f64,

    /// Maximum scenario tier to include (1 = default, 2+ includes
    /// edge-case scenarios). Defaults to 1.
    #[arg(long, default_value_t = 1)]
    tier: u8,

    /// Run `cargo check` in a temp copy after refactoring tools
    /// (qartez_rename, qartez_move) to verify compilation.
    #[arg(long)]
    compile_check: bool,

    /// Use the batch judge path (single LLM call for all scenarios).
    /// This is the default when `--judge` is set.
    #[arg(long)]
    judge_batch: bool,

    /// Force the legacy per-scenario judge path instead of batch.
    #[arg(long)]
    judge_legacy: bool,
}

fn main() -> Result<()> {
    let mut cli = Cli::parse();

    if cli.judge_ensemble && !cli.judge {
        cli.judge = true;
    }
    if cli.grounding && cli.no_grounding {
        anyhow::bail!(
            "--grounding and --no-grounding are mutually exclusive; pick one grounding mode"
        );
    }
    let effective_grounding = !cli.no_grounding && (cli.grounding || cli.judge);

    // Summary-only mode short-circuits the entire run. Useful in CI
    // pipelines where the per-language reports were generated by
    // separate jobs and the aggregator just needs to refresh the
    // headline file.
    if cli.cross_lang_summary_only {
        let reports_dir = cli.project_root.join("reports");
        write_cross_lang_summary(&reports_dir, &cli.cross_lang_summary_out, None)?;
        return Ok(());
    }

    // Render-only modes also short-circuit the benchmark run. The JSON is
    // the machine-readable source of truth, so when the renderer changes
    // without touching the numbers we re-deserialize and re-emit Markdown
    // instead of paying for a full measurement pass.
    if cli.render_md_only_all {
        let reports_dir = cli.project_root.join("reports");
        rerender_all_markdown(&reports_dir)?;
        return Ok(());
    }
    if cli.render_md_only {
        rerender_single_markdown(&cli.out_json, &cli.out_md)?;
        return Ok(());
    }

    // Resolve the language profile first - unknown / unimplemented
    // languages should fail before we touch the database.
    let profile = profiles::by_name(&cli.lang).with_context(|| {
        let implemented = profiles::implemented_languages().join(", ");
        format!(
            "language profile `{}` is not implemented yet (available: {implemented})",
            cli.lang
        )
    })?;

    // The fixture root lets non-Rust profiles point at a shared fixture
    // tree outside the repo. When not provided, the effective project
    // root is the CLI --project-root (typical for Rust self-bench
    // runs).
    let base_root = cli
        .fixture_root
        .as_ref()
        .cloned()
        .unwrap_or_else(|| cli.project_root.clone());
    let effective_root = if profile.fixture_subdir.is_empty() {
        base_root
    } else {
        base_root.join(profile.fixture_subdir)
    };
    let project_root = effective_root
        .canonicalize()
        .with_context(|| format!("canonicalize project root {effective_root:?}"))?;

    let db_path = cli
        .db
        .clone()
        .unwrap_or_else(|| project_root.join(".qartez").join("index.db"));

    if !db_path.exists() {
        anyhow::bail!(
            "database not found at {}. Run `qartez-mcp index` first.",
            db_path.display()
        );
    }

    let conn =
        storage::open_db(&db_path).with_context(|| format!("open db at {}", db_path.display()))?;

    // Resolve scenario targets: prefer the profile's override (Rust) so
    // the baseline stays byte-identical; otherwise ask the live
    // qartez database to pick sensible defaults.
    let resolved_targets = match profile.target_override {
        Some(f) => f(),
        None => targets::resolve(&conn, profile)
            .with_context(|| format!("resolve targets for profile {}", profile.name))?,
    };

    let server = QartezServer::new(conn, project_root.clone(), 300);

    let config = LatencyConfig {
        warmup_runs: cli.warmup,
        measured_runs: cli.runs,
        trim_outliers: true,
    };
    let mut runner = BenchmarkRunner::new(&server, &project_root)
        .with_config(config)
        .with_grounding_enabled(effective_grounding);

    if let Some(cache_path) = &cli.reuse_non_mcp {
        let text = std::fs::read_to_string(cache_path)
            .with_context(|| format!("read non-mcp cache {}", cache_path.display()))?;
        let prior: BenchmarkReport =
            serde_json::from_str(&text).context("parse cached benchmark json")?;
        let current_sha = current_git_sha();
        let expected = if cli.allow_stale_cache {
            None
        } else {
            current_sha.as_deref()
        };
        let cache = build_non_mcp_cache(&prior, expected);
        if cache.is_empty() {
            if cli.allow_stale_cache {
                eprintln!(
                    "warning: cache at {} contained no usable entries",
                    cache_path.display()
                );
            } else {
                anyhow::bail!(
                    "cache at {} has git SHA {:?} but current is {:?}; pass --allow-stale-cache to force",
                    cache_path.display(),
                    prior.git_sha,
                    current_sha,
                );
            }
        }
        println!(
            "Reusing non-MCP side from {} ({} scenarios cached)",
            cache_path.display(),
            cache.len()
        );
        runner = runner.with_non_mcp_cache(cache);
    }

    println!(
        "Running benchmark with lang={}, warmup={}, runs={}, filter={:?}, tier={}",
        profile.name, cli.warmup, cli.runs, cli.filter, cli.tier
    );
    let mut scenarios =
        runner.run_all_with_tier(&resolved_targets, profile, cli.filter.as_deref(), cli.tier);
    println!("Completed {} scenario(s).", scenarios.len());

    let judge_cache: Option<HashMap<String, judge::CachedJudge>> = if let Some(cache_path) =
        &cli.reuse_judge
    {
        let text = std::fs::read_to_string(cache_path)
            .with_context(|| format!("read judge cache {}", cache_path.display()))?;
        let prior: BenchmarkReport =
            serde_json::from_str(&text).context("parse cached judge benchmark json")?;
        let current_sha = current_git_sha();
        let expected = if cli.allow_stale_judge_cache {
            None
        } else {
            current_sha.as_deref()
        };
        let cache = judge::build_judge_cache(&prior, expected, cli.allow_stale_judge_cache);
        if cache.is_empty() {
            if cli.allow_stale_judge_cache {
                eprintln!(
                    "warning: judge cache at {} contained no usable entries",
                    cache_path.display()
                );
            } else {
                anyhow::bail!(
                    "judge cache at {} has git SHA {:?} but current is {:?}; pass --allow-stale-judge-cache to force",
                    cache_path.display(),
                    prior.git_sha,
                    current_sha,
                );
            }
        }
        println!(
            "Reusing judge verdicts from {} ({} scenarios cached)",
            cache_path.display(),
            cache.len()
        );
        Some(cache)
    } else {
        None
    };

    // Batch judge is the default when --judge is set and --judge-legacy is not.
    let use_batch_judge = cli.judge && !cli.judge_legacy && !cli.judge_ensemble;

    if use_batch_judge {
        let scenario_refs: Vec<&report::ScenarioReport> = scenarios.iter().collect();
        println!(
            "Scoring {} scenario(s) with batch judge (`{}`, 1 LLM call)…",
            scenarios.len(),
            cli.judge_model,
        );
        match judge::score_batch(&scenario_refs, &cli.judge_model) {
            Ok(scores) => {
                for (i, q) in scores.into_iter().enumerate() {
                    scenarios[i].quality = Some(q);
                }
                let scored = scenarios.iter().filter(|s| s.quality.is_some()).count();
                println!(
                    "Batch judge finished: {scored}/{} scenario(s) scored.",
                    scenarios.len()
                );
            }
            Err(e) => {
                eprintln!("Batch judge failed: {e:#}. Falling back to per-scenario judge.");
                let workers = cli.judge_workers.max(1);
                let n = cli.judge_n.max(1);
                run_judge(
                    &mut scenarios,
                    &cli.judge_model,
                    workers,
                    n,
                    judge_cache.as_ref(),
                    cli.allow_stale_judge_cache,
                );
            }
        }
    } else if cli.judge && cli.judge_ensemble {
        let workers = cli.judge_workers.max(1);
        let n = cli.judge_n.max(1);
        let calls_no_arbitration = 2 * 2 * n;
        println!(
            "Scoring {} scenario(s) with ensemble judge (primary `{}`, secondary `{}`, arbiter `{}`, τ={:.1}, `claude -p` × {} per scenario, {} worker(s))…",
            scenarios.len(),
            cli.judge_model,
            cli.judge_model_secondary,
            cli.judge_arbiter,
            cli.judge_disagreement_threshold,
            calls_no_arbitration,
            workers,
        );
        run_judge_ensemble(
            &mut scenarios,
            &cli.judge_model,
            &cli.judge_model_secondary,
            &cli.judge_arbiter,
            cli.judge_disagreement_threshold,
            workers,
            n,
            judge_cache.as_ref(),
            cli.allow_stale_judge_cache,
        );
        let scored = scenarios
            .iter()
            .filter(|s| s.ensemble_quality.is_some())
            .count();
        let arbitrated = scenarios
            .iter()
            .filter(|s| {
                s.ensemble_quality
                    .as_ref()
                    .is_some_and(|e| e.arbiter.is_some())
            })
            .count();
        println!(
            "Judge ensemble finished: {scored}/{} scenario(s) scored ({arbitrated} required arbitration).",
            scenarios.len()
        );
    } else if cli.judge {
        let workers = cli.judge_workers.max(1);
        let n = cli.judge_n.max(1);
        println!(
            "Scoring {} scenario(s) with `{}` via judge (`claude -p` × {}, {} worker(s))…",
            scenarios.len(),
            cli.judge_model,
            2 * n,
            workers,
        );
        run_judge(
            &mut scenarios,
            &cli.judge_model,
            workers,
            n,
            judge_cache.as_ref(),
            cli.allow_stale_judge_cache,
        );
        let scored = scenarios.iter().filter(|s| s.quality.is_some()).count();
        println!(
            "Judge finished: {scored}/{} scenario(s) scored.",
            scenarios.len()
        );
    }

    let report = BenchmarkReport::new_with_language(scenarios, profile.name.to_string());

    report::write_json(&cli.out_json, &report)
        .with_context(|| format!("write json to {}", cli.out_json.display()))?;
    report::write_markdown(&cli.out_md, &report)
        .with_context(|| format!("write markdown to {}", cli.out_md.display()))?;

    println!("Wrote JSON to {}", cli.out_json.display());
    println!("Wrote Markdown to {}", cli.out_md.display());

    if cli.cross_lang_summary {
        let reports_dir = cli.project_root.join("reports");
        write_cross_lang_summary(
            &reports_dir,
            &cli.cross_lang_summary_out,
            Some((&cli.out_json, &report)),
        )?;
    }

    print_summary(&report);

    if cli.update_baseline {
        if let Some(baseline_path) = &cli.baseline {
            report::write_json(baseline_path, &report)?;
            println!("Updated baseline at {}", baseline_path.display());
        } else {
            anyhow::bail!("--update-baseline requires --baseline <path>");
        }
        return Ok(());
    }

    if let Some(baseline_path) = &cli.baseline {
        compare_against_baseline(&report, baseline_path, cli.threshold_pct)?;
    }

    Ok(())
}

fn compare_against_baseline(
    current: &BenchmarkReport,
    baseline_path: &Path,
    threshold_pct: f64,
) -> Result<()> {
    let text = std::fs::read_to_string(baseline_path)
        .with_context(|| format!("read baseline {}", baseline_path.display()))?;
    let baseline: BenchmarkReport = serde_json::from_str(&text).context("parse baseline json")?;

    let findings = report::check_regression(current, &baseline, threshold_pct);
    if findings.is_empty() {
        println!(
            "No regressions above {threshold_pct:.1}% threshold vs baseline {}",
            baseline_path.display()
        );
        return Ok(());
    }

    for f in &findings {
        eprintln!(
            "REGRESSION [{}] {}: tokens {} → {} ({:+.1}%), latency {:.2}ms → {:.2}ms ({:+.1}%)",
            f.severity,
            f.scenario_id,
            f.baseline_tokens,
            f.current_tokens,
            f.token_delta_pct,
            f.baseline_latency_ms,
            f.current_latency_ms,
            f.latency_delta_pct,
        );
    }
    anyhow::bail!("{} regression(s) detected", findings.len())
}

fn current_git_sha() -> Option<String> {
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

/// Loads every `reports/benchmark*.json` sibling file from `reports_dir`,
/// optionally substitutes the just-written current run for its on-disk
/// version, and writes the cross-language summary to `out_md`.
///
/// Files that fail to parse are skipped with a stderr warning rather than
/// aborted, so a corrupt baseline does not block the aggregator. The
/// resulting language order is alphabetical by file stem with `rust`
/// pinned first, because the Rust report is the canonical baseline that
/// downstream tooling compares everything else to.
fn write_cross_lang_summary(
    reports_dir: &Path,
    out_md: &Path,
    current: Option<(&Path, &BenchmarkReport)>,
) -> Result<()> {
    use std::collections::BTreeMap;

    if !reports_dir.is_dir() {
        anyhow::bail!(
            "cross-language summary needs an existing reports/ directory at {}",
            reports_dir.display()
        );
    }

    // Collect all sibling JSON reports indexed by their file stem
    // ("benchmark", "benchmark-typescript", ...). The stem becomes the
    // label that appears in the headline matrix.
    let mut by_label: BTreeMap<String, BenchmarkReport> = BTreeMap::new();
    let entries = std::fs::read_dir(reports_dir)
        .with_context(|| format!("read reports dir {}", reports_dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| "iterate reports dir")?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("benchmark") || !name.ends_with(".json") {
            continue;
        }
        // The "baseline-*.json" files live alongside "benchmark-*.json"
        // and contain identical data, so we filter on the prefix to
        // avoid double-counting languages.
        if !name.starts_with("benchmark") {
            continue;
        }
        let stem = name.trim_end_matches(".json").to_string();
        match std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|t| serde_json::from_str::<BenchmarkReport>(&t).map_err(Into::into))
        {
            Ok(parsed) => {
                by_label.insert(stem, parsed);
            }
            Err(e) => {
                eprintln!(
                    "warning: skipping {} for cross-language summary: {e:#}",
                    path.display()
                );
            }
        }
    }

    // If the caller passed a freshly-built report, splice it in by file
    // stem so the summary always reflects the latest run rather than the
    // version that was on disk before the write.
    if let Some((path, report)) = current
        && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
    {
        by_label.insert(stem.to_string(), report.clone());
    }

    if by_label.is_empty() {
        eprintln!(
            "warning: no benchmark*.json files found under {}; cross-language summary will be empty",
            reports_dir.display()
        );
    }

    // Order: Rust first as the canonical baseline, then the other
    // Wave 2 languages in their CLI declaration order (TypeScript →
    // Python → Go → Java), then any unrecognized labels alphabetically
    // tail-appended. This ordering matches `KNOWN_LANGUAGES` in
    // `src/benchmark/profiles/mod.rs` and gives readers a stable
    // top-to-bottom narrative.
    const PREFERRED_LANG_ORDER: &[&str] = &["rust", "typescript", "python", "go", "java"];
    let mut ordered: Vec<(String, BenchmarkReport)> = Vec::new();
    if let Some(rust) = by_label.remove("benchmark") {
        ordered.push(("rust".to_string(), rust));
    }
    for lang in PREFERRED_LANG_ORDER.iter().skip(1) {
        let stem = format!("benchmark-{lang}");
        if let Some(report) = by_label.remove(&stem) {
            ordered.push(((*lang).to_string(), report));
        }
    }
    // Tail-append anything left over (custom fixtures, future
    // languages) in alphabetical order so the output stays
    // deterministic.
    for (stem, report) in by_label {
        let label = stem
            .strip_prefix("benchmark-")
            .map(|s| s.to_string())
            .unwrap_or(stem);
        ordered.push((label, report));
    }

    report::write_cross_language_summary(out_md, &ordered)
        .with_context(|| format!("write cross-language summary to {}", out_md.display()))?;
    println!(
        "Wrote cross-language summary to {} ({} language(s))",
        out_md.display(),
        ordered.len(),
    );
    Ok(())
}

/// Reads a single benchmark JSON and rewrites its sibling Markdown using
/// the current [`report::render_markdown`] implementation. Used by
/// `--render-md-only` to refresh a report after a renderer tweak without
/// re-running the (much slower) measurement pass.
fn rerender_single_markdown(json_path: &Path, md_path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(json_path)
        .with_context(|| format!("read benchmark json {}", json_path.display()))?;
    let report: BenchmarkReport =
        serde_json::from_str(&text).with_context(|| "parse benchmark json")?;
    report::write_markdown(md_path, &report)
        .with_context(|| format!("write markdown to {}", md_path.display()))?;
    println!(
        "Re-rendered {} → {}",
        json_path.display(),
        md_path.display()
    );
    Ok(())
}

/// Walks `reports_dir` for every `benchmark*.json` sibling and rewrites
/// the matching `.md` file next to each one. Used by `--render-md-only-all`
/// to refresh all per-language reports in a single invocation. Files that
/// fail to parse are skipped with a warning rather than aborting the loop
/// so one corrupt report does not block the rest.
fn rerender_all_markdown(reports_dir: &Path) -> Result<()> {
    if !reports_dir.is_dir() {
        anyhow::bail!(
            "render-md-only-all needs an existing reports/ directory at {}",
            reports_dir.display()
        );
    }
    let mut count: usize = 0;
    let entries = std::fs::read_dir(reports_dir)
        .with_context(|| format!("read reports dir {}", reports_dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| "iterate reports dir")?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("benchmark") || !name.ends_with(".json") {
            continue;
        }
        let md_path = path.with_extension("md");
        match rerender_single_markdown(&path, &md_path) {
            Ok(()) => count += 1,
            Err(e) => eprintln!("warning: skipping {}: {e:#}", path.display()),
        }
    }
    println!("Re-rendered {count} per-language report(s).");
    Ok(())
}

/// Generic work-stealing parallel runner. Hands out scenario indices to
/// `workers` threads via an `AtomicUsize` counter. `score_fn` is called
/// once per scenario and returns `Ok(T)` on success or `Err` on failure
/// (logged to stderr, not fatal). Results are collected into a Vec of
/// `Option<T>` and returned in scenario order.
fn run_judge_parallel<T: Send>(
    scenarios: &[ScenarioReport],
    workers: usize,
    score_fn: impl Fn(&ScenarioReport, usize) -> Result<T> + Sync,
) -> Vec<Option<T>> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let count = scenarios.len();
    if count == 0 {
        return Vec::new();
    }
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<T>>> = (0..count).map(|_| Mutex::new(None)).collect();

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let idx = next.fetch_add(1, Ordering::Relaxed);
                    if idx >= count {
                        return;
                    }
                    let scenario = &scenarios[idx];
                    let label = &scenario.scenario_id;
                    match score_fn(scenario, idx) {
                        Ok(val) => {
                            *results[idx].lock().expect("results mutex poisoned") = Some(val);
                            eprintln!("  ✓ [{pos}/{count}] {label}", pos = idx + 1);
                        }
                        Err(e) => {
                            eprintln!("  ✗ [{pos}/{count}] {label}: {e:#}", pos = idx + 1);
                        }
                    }
                }
            });
        }
    });

    results
        .into_iter()
        .map(|slot| slot.into_inner().expect("results mutex poisoned"))
        .collect()
}

/// Scores every scenario with the judge protocol in parallel.
fn run_judge(
    scenarios: &mut [ScenarioReport],
    model: &str,
    workers: usize,
    n: usize,
    cache: Option<&HashMap<String, judge::CachedJudge>>,
    allow_stale: bool,
) {
    let results = run_judge_parallel(scenarios, workers, |scenario, _idx| {
        if let Some(c) = cache
            && let Some(cached) = judge::lookup_judge_cache(c, scenario, allow_stale)
        {
            eprintln!("    (reused from judge cache)");
            return Ok(cached.clone());
        }

        let reference = scenario.reference_answer.as_deref();
        let grounding_mcp =
            grounding::render_prompt_block("ANSWER A (MCP)", scenario.mcp.grounding.as_ref());
        let grounding_non_mcp = grounding::render_prompt_block(
            "ANSWER B (non-MCP)",
            scenario.non_mcp.grounding.as_ref(),
        );
        judge::score_scenario(
            scenario,
            model,
            reference,
            n,
            &grounding_mcp,
            &grounding_non_mcp,
        )
    });

    for (i, result) in results.into_iter().enumerate() {
        if let Some(q) = result {
            scenarios[i].quality = Some(q);
        }
    }
}

/// Scores every scenario with the two-judge ensemble protocol in parallel.
#[allow(
    clippy::too_many_arguments,
    reason = "dedicated ensemble runner; flattening would require a config struct used only here"
)]
fn run_judge_ensemble(
    scenarios: &mut [ScenarioReport],
    primary_model: &str,
    secondary_model: &str,
    arbiter_model: &str,
    disagreement_threshold: f64,
    workers: usize,
    n: usize,
    cache: Option<&HashMap<String, judge::CachedJudge>>,
    allow_stale: bool,
) {
    let results = run_judge_parallel(scenarios, workers, |scenario, _idx| {
        let reference = scenario.reference_answer.as_deref();
        let grounding_mcp =
            grounding::render_prompt_block("ANSWER A (MCP)", scenario.mcp.grounding.as_ref());
        let grounding_non_mcp = grounding::render_prompt_block(
            "ANSWER B (non-MCP)",
            scenario.non_mcp.grounding.as_ref(),
        );

        let cached_primary: Option<judge::QualityScores> = cache
            .and_then(|c| judge::lookup_judge_cache(c, scenario, allow_stale))
            .cloned();

        if let Some(primary) = cached_primary {
            eprintln!("    (primary reused from judge cache)");
            judge::score_ensemble_scenario_with_primary(
                scenario,
                primary,
                secondary_model,
                arbiter_model,
                reference,
                n,
                disagreement_threshold,
                &grounding_mcp,
                &grounding_non_mcp,
            )
        } else {
            judge::score_ensemble_scenario(
                scenario,
                primary_model,
                secondary_model,
                arbiter_model,
                reference,
                n,
                disagreement_threshold,
                &grounding_mcp,
                &grounding_non_mcp,
            )
        }
    });

    for (i, result) in results.into_iter().enumerate() {
        if let Some(ens) = result {
            scenarios[i].quality = Some(ens.final_score.clone());
            scenarios[i].ensemble_quality = Some(ens);
        }
    }
}

fn print_summary(report: &BenchmarkReport) {
    let total = report.scenarios.len();
    let mcp_wins = report
        .scenarios
        .iter()
        .filter(|r| r.verdict.winner == "mcp")
        .count();
    let ties = report
        .scenarios
        .iter()
        .filter(|r| r.verdict.winner == "tie")
        .count();
    let non_mcp_wins = report
        .scenarios
        .iter()
        .filter(|r| r.verdict.winner == "non_mcp")
        .count();
    let errors = report
        .scenarios
        .iter()
        .filter(|r| r.mcp.error.is_some() || r.non_mcp.error.is_some())
        .count();

    let sum_mcp_tokens: usize = report.scenarios.iter().map(|r| r.mcp.tokens).sum();
    let sum_non_mcp_tokens: usize = report.scenarios.iter().map(|r| r.non_mcp.tokens).sum();
    let aggregate_savings_pct = if sum_non_mcp_tokens == 0 {
        0.0
    } else {
        ((sum_non_mcp_tokens - sum_mcp_tokens.min(sum_non_mcp_tokens)) as f64
            / sum_non_mcp_tokens as f64)
            * 100.0
    };

    println!();
    println!("=== Summary ===");
    println!(
        "Scenarios: {total} | MCP wins: {mcp_wins} | ties: {ties} | non-MCP wins: {non_mcp_wins} | errors: {errors}"
    );
    println!("Total tokens: MCP {sum_mcp_tokens}, non-MCP {sum_non_mcp_tokens}");
    println!("Aggregate token savings: {aggregate_savings_pct:.1}%");

    // Effective aggregate savings (recall-weighted) for scenarios with set comparison.
    let recall_scenarios: Vec<_> = report
        .scenarios
        .iter()
        .filter(|s| s.set_comparison.is_some())
        .collect();
    if !recall_scenarios.is_empty() {
        let sum_non_mcp_recall: f64 = recall_scenarios
            .iter()
            .map(|s| s.non_mcp.tokens as f64)
            .sum();
        if sum_non_mcp_recall > 0.0 {
            let sum_effective: f64 = recall_scenarios
                .iter()
                .map(|s| {
                    let recall = s.set_comparison.as_ref().map(|sc| sc.recall).unwrap_or(1.0);
                    s.non_mcp.tokens as f64 - s.mcp.tokens as f64 * recall
                })
                .sum();
            let effective_pct = (sum_effective / sum_non_mcp_recall) * 100.0;
            println!("Effective aggregate savings (recall-weighted): {effective_pct:.1}%");
        }
    }

    // Session cost context
    let savings_tokens = sum_non_mcp_tokens.saturating_sub(sum_mcp_tokens);
    /// Approximate token overhead of one empty Claude Code session.
    const SESSION_BASE_TOKENS: f64 = 20_000.0;
    let sessions = savings_tokens as f64 / SESSION_BASE_TOKENS;
    println!("Session cost context: {savings_tokens} tokens saved ({sessions:.1} empty sessions)");
}
