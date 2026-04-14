//! Per-tool comparative benchmark harness for Qartez MCP.
//!
//! Measures each of the 17 MCP tools against the equivalent non-MCP
//! workflow (`Glob`/`Grep`/`Read` as a Claude Code agent would run
//! them) and emits a per-tool matrix with token savings, latency, and
//! hand-authored verdicts.

pub mod grounding;
pub mod judge;
pub mod profiles;
pub mod report;
pub mod scenarios;
pub mod set_compare;
pub mod sim_runner;
pub mod targets;
pub mod tokenize;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::server::QartezServer;

pub use grounding::{FileFacts, GroundingScores};
pub use profiles::LanguageProfile;
pub use report::{BenchmarkReport, LatencyStats, ScenarioReport, SideReport, Verdict};
pub use scenarios::{SCENARIOS, Scenario, SimStep};
pub use targets::ResolvedTargets;

/// Configuration for the latency measurement loop.
///
/// Defaults are tuned for the small 17-scenario matrix on a live `.qartez`
/// database: 3 warmup runs to prime any lazy state, 7 measured runs, and
/// min/max trimming to absorb occasional cache-miss spikes. The resulting
/// 5 post-trim samples are enough to detect 10% efficiency regressions
/// without turning the bench into a long-running suite.
#[derive(Debug, Clone, Copy)]
pub struct LatencyConfig {
    pub warmup_runs: usize,
    pub measured_runs: usize,
    pub trim_outliers: bool,
}

impl Default for LatencyConfig {
    fn default() -> Self {
        Self {
            warmup_runs: 3,
            measured_runs: 7,
            trim_outliers: true,
        }
    }
}

/// Orchestrates per-scenario runs on both the MCP and simulated non-MCP sides.
pub struct BenchmarkRunner<'a> {
    pub server: &'a QartezServer,
    pub project_root: &'a Path,
    pub config: LatencyConfig,
    /// Optional map of scenario_id → cached non-MCP side from a prior run.
    /// When a scenario is found in this map its non-MCP side is taken verbatim
    /// from the cache, so iterative MCP-side development doesn't pay for the
    /// (much slower) Glob/Grep/Read/git-log simulation every time.
    pub non_mcp_cache: HashMap<String, SideReport>,
    /// Enable programmatic grounding verification for both MCP and non-MCP
    /// outputs. Set via [`with_grounding_enabled`](Self::with_grounding_enabled).
    /// Default is `false` so slice A runs (and the legacy `--judge` path)
    /// do not incur the grounding cost.
    pub grounding_enabled: bool,
    /// File verification cache shared across scenarios in a single
    /// `run_all` invocation. Wrapped in a `RefCell` so the grounding
    /// verifier can mutate it from inside `run_one(&self, ...)` without
    /// changing the existing `&self` signature (which slice A's
    /// `run_judge` relies on).
    pub file_cache: RefCell<HashMap<String, Option<FileFacts>>>,
    /// Symbol-lookup cache shared across scenarios, same pattern as
    /// [`file_cache`](Self::file_cache).
    pub symbol_cache: RefCell<HashMap<String, bool>>,
    /// Lazily-built basename index for resolving bare filenames
    /// (`config.json` with no directory). Populated on first miss.
    pub basename_index: RefCell<Option<HashMap<String, Vec<PathBuf>>>>,
}

impl<'a> BenchmarkRunner<'a> {
    pub fn new(server: &'a QartezServer, project_root: &'a Path) -> Self {
        Self {
            server,
            project_root,
            config: LatencyConfig::default(),
            non_mcp_cache: HashMap::new(),
            grounding_enabled: false,
            file_cache: RefCell::new(HashMap::new()),
            symbol_cache: RefCell::new(HashMap::new()),
            basename_index: RefCell::new(None),
        }
    }

    pub fn with_config(mut self, config: LatencyConfig) -> Self {
        self.config = config;
        self
    }

    /// Install a non-MCP cache built from a previously-serialized
    /// `BenchmarkReport`. Callers should verify the git SHA / codebase
    /// identity before loading the cache.
    pub fn with_non_mcp_cache(mut self, cache: HashMap<String, SideReport>) -> Self {
        self.non_mcp_cache = cache;
        self
    }

    /// Enable or disable programmatic grounding verification. Slice B
    /// builder hook; `--judge` sets this via
    /// `src/bin/benchmark.rs`.
    pub fn with_grounding_enabled(mut self, enabled: bool) -> Self {
        self.grounding_enabled = enabled;
        self
    }

    /// Compute claim-level grounding for `output`, reusing the runner's
    /// per-run caches. Returns `None` when grounding is disabled or the
    /// parser extracted zero claims. This is the single site that
    /// bridges `grounding::verify_side` and the runner's borrow state;
    /// both `run_mcp` and `run_sim` call it from inside the hot loop.
    fn grounding_for(&self, output: &str) -> Option<GroundingScores> {
        if !self.grounding_enabled {
            return None;
        }
        let conn_guard = self.server.db_connection();
        let mut file_cache = self.file_cache.borrow_mut();
        let mut symbol_cache = self.symbol_cache.borrow_mut();
        let mut basename_index = self.basename_index.borrow_mut();
        let mut ctx = grounding::GroundingContext {
            project_root: self.project_root,
            conn: Some(&*conn_guard),
            file_cache: &mut file_cache,
            symbol_cache: &mut symbol_cache,
            basename_index: &mut basename_index,
        };
        grounding::verify_side(output, &mut ctx)
    }

    /// Run every scenario, optionally filtered by substring match on
    /// tool name or id. Takes the active [`ResolvedTargets`] and
    /// [`LanguageProfile`] so the scenarios can be parameterized per
    /// language.
    pub fn run_all(
        &self,
        targets: &ResolvedTargets,
        profile: &LanguageProfile,
        filter: Option<&str>,
    ) -> Vec<ScenarioReport> {
        self.run_all_with_tier(targets, profile, filter, 1)
    }

    /// Like [`run_all`](Self::run_all) but accepts a maximum tier level.
    /// Scenarios with `tier > max_tier` are skipped.
    pub fn run_all_with_tier(
        &self,
        targets: &ResolvedTargets,
        profile: &LanguageProfile,
        filter: Option<&str>,
        max_tier: u8,
    ) -> Vec<ScenarioReport> {
        let mut reports = Vec::with_capacity(SCENARIOS.len());
        for scenario in SCENARIOS {
            if scenario.tier > max_tier {
                continue;
            }
            if let Some(f) = filter
                && !scenario.tool.contains(f)
                && !scenario.id.contains(f)
            {
                continue;
            }
            reports.push(self.run_one(scenario, targets, profile));
        }
        reports
    }

    pub fn run_one(
        &self,
        scenario: &Scenario,
        targets: &ResolvedTargets,
        profile: &LanguageProfile,
    ) -> ScenarioReport {
        let mcp = self.run_mcp(scenario, targets, profile);
        let sim = match self.non_mcp_cache.get(scenario.id) {
            Some(cached) => {
                let mut reused = cached.clone();
                reused.reused = true;
                reused
            }
            None => self.run_sim(scenario, targets, profile),
        };
        let set_comparison =
            set_compare::compare(scenario.tool, &mcp.full_output, &sim.full_output);
        let mut report = report::build_scenario_report(scenario, mcp, sim);
        report.set_comparison = set_comparison;
        report::fill_effective_savings(&mut report);
        report
    }

    fn run_mcp(
        &self,
        scenario: &Scenario,
        targets: &ResolvedTargets,
        profile: &LanguageProfile,
    ) -> SideReport {
        let args = (scenario.mcp_args)(targets, profile);
        let mut final_output = String::new();
        let mut final_error: Option<String> = None;

        for _ in 0..self.config.warmup_runs {
            let _ = self.server.call_tool_by_name(scenario.tool, args.clone());
        }

        let mut samples = Vec::with_capacity(self.config.measured_runs);
        for _ in 0..self.config.measured_runs {
            let start = Instant::now();
            match self.server.call_tool_by_name(scenario.tool, args.clone()) {
                Ok(out) => {
                    samples.push(start.elapsed());
                    final_output = out;
                    final_error = None;
                }
                Err(e) => {
                    samples.push(start.elapsed());
                    final_error = Some(e);
                }
            }
        }

        let latency = latency_from_samples(&samples, self.config.trim_outliers);
        let preview = preview_of(&final_output);
        let grounding = self.grounding_for(&final_output);
        SideReport {
            response_bytes: final_output.len(),
            response_preview: preview,
            tokens: tokenize::count_tokens(&final_output),
            naive_tokens: tokenize::naive_count(&final_output),
            latency,
            error: final_error,
            args: Some(args),
            steps: None,
            reused: false,
            full_output: final_output,
            grounding,
        }
    }

    fn run_sim(
        &self,
        scenario: &Scenario,
        targets: &ResolvedTargets,
        profile: &LanguageProfile,
    ) -> SideReport {
        let steps = (scenario.non_mcp_steps)(targets, profile);
        let sim_opts = sim_runner::Options {
            exclude_globs: profile.exclude_globs,
        };
        let mut final_output = String::new();
        let mut final_error: Option<String> = None;

        for _ in 0..self.config.warmup_runs {
            let _ = sim_runner::run_with(self.project_root, &steps, &sim_opts);
        }

        let mut samples = Vec::with_capacity(self.config.measured_runs);
        for _ in 0..self.config.measured_runs {
            let start = Instant::now();
            match sim_runner::run_with(self.project_root, &steps, &sim_opts) {
                Ok(out) => {
                    samples.push(start.elapsed());
                    final_output = out;
                    final_error = None;
                }
                Err(e) => {
                    samples.push(start.elapsed());
                    final_error = Some(e.to_string());
                }
            }
        }

        let latency = latency_from_samples(&samples, self.config.trim_outliers);
        let preview = preview_of(&final_output);
        let grounding = self.grounding_for(&final_output);
        SideReport {
            response_bytes: final_output.len(),
            response_preview: preview,
            tokens: tokenize::count_tokens(&final_output),
            naive_tokens: tokenize::naive_count(&final_output),
            latency,
            error: final_error,
            args: None,
            steps: Some(steps.iter().map(SimStep::describe).collect()),
            reused: false,
            full_output: final_output,
            grounding,
        }
    }
}

/// Build a non-MCP cache map from a previously-written `BenchmarkReport`.
///
/// When `expected_sha` is `Some`, the cache is only populated if the stored
/// report's `git_sha` matches — otherwise an empty map is returned, forcing
/// a fresh non-MCP run to avoid comparing against stale data.
pub fn build_non_mcp_cache(
    prior: &BenchmarkReport,
    expected_sha: Option<&str>,
) -> HashMap<String, SideReport> {
    if let (Some(want), Some(have)) = (expected_sha, prior.git_sha.as_deref())
        && want != have
    {
        return HashMap::new();
    }
    prior
        .scenarios
        .iter()
        .map(|s| (s.scenario_id.clone(), s.non_mcp.clone()))
        .collect()
}

/// Maximum preview length before the rest of the response is truncated.
/// Kept small to keep the JSON report under ~200 KB for 17 scenarios.
const PREVIEW_CAP_BYTES: usize = 240;

fn preview_of(s: &str) -> String {
    if s.len() <= PREVIEW_CAP_BYTES {
        s.to_string()
    } else {
        let mut cap = PREVIEW_CAP_BYTES;
        while !s.is_char_boundary(cap) && cap > 0 {
            cap -= 1;
        }
        format!("{}…", &s[..cap])
    }
}

fn latency_from_samples(samples: &[Duration], trim: bool) -> LatencyStats {
    let mut micros: Vec<f64> = samples.iter().map(|d| d.as_micros() as f64).collect();
    micros.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let effective: &[f64] = if trim && micros.len() >= 3 {
        &micros[1..micros.len() - 1]
    } else {
        &micros[..]
    };

    let n = effective.len() as f64;
    let mean = if n == 0.0 {
        0.0
    } else {
        effective.iter().sum::<f64>() / n
    };
    let variance = if n == 0.0 {
        0.0
    } else {
        effective.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n
    };
    let stdev = variance.sqrt();
    let p50 = percentile(effective, 50.0);
    let p95 = percentile(effective, 95.0);

    LatencyStats {
        mean_us: mean,
        stdev_us: stdev,
        p50_us: p50,
        p95_us: p95,
        samples: effective.len(),
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p / 100.0) * (sorted.len() as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = rank - lo as f64;
        sorted[lo] * (1.0 - frac) + sorted[hi] * frac
    }
}
