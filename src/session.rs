use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::git;
use crate::guard::{
    self, EquivalencePolicy, EquivalenceResult, FileLocks, OutputSnapshot, TamperViolation,
};
use crate::profiler::{self, ProfileDiff, ProfileReport};
use crate::runner::{self, HostInfo};
use crate::stats::{self, BenchVerdict, ComparisonStats, SampleSummary};

pub const OA_DIR: &str = ".oa";
pub const REF_PRE_SESSION: &str = "refs/oa/pre-session";
pub const REF_BASELINE: &str = "refs/oa/baseline";
pub const REF_BEST: &str = "refs/oa/best";

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn oa_path(root: &Path, name: &str) -> PathBuf {
    root.join(OA_DIR).join(name)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory '{}': {e}", parent.display()))?;
    }
    let data = serde_json::to_string_pretty(value)
        .map_err(|e| format!("failed to serialize '{}': {e}", path.display()))?;
    fs::write(path, data).map_err(|e| format!("failed to write '{}': {e}", path.display()))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("failed to read '{}': {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("failed to parse '{}': {e}", path.display()))
}

fn resolve_root(path: Option<&str>) -> Result<PathBuf, String> {
    let target = path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    git::repo_root(&target)
}

fn short_sha(sha: &str) -> &str {
    if sha.len() >= 10 {
        &sha[..10]
    } else {
        sha
    }
}

fn snippet_output(bytes: &[u8], max_chars: usize) -> String {
    String::from_utf8_lossy(bytes)
        .trim()
        .chars()
        .take(max_chars)
        .collect()
}

/// Optimization objective compared in interleaved A/B evaluation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricTarget {
    #[default]
    Wall,
    Rss,
}

impl MetricTarget {
    pub fn parse_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "wall" | "time" | "latency" => Ok(Self::Wall),
            "rss" | "ram" | "memory" | "mem" => Ok(Self::Rss),
            other => Err(format!(
                "invalid metric '{other}': expected 'wall' or 'rss'"
            )),
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Self::Wall => "ms",
            Self::Rss => "KiB",
        }
    }
}

/// Parameters for initializing an optimization session (`oa init` / `oa_init`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitParams {
    pub path: Option<String>,
    pub bench_cmd: String,
    pub test_cmd: Option<String>,
    pub build_cmd: Option<String>,
    pub profile_cmd: Option<String>,
    #[serde(default)]
    pub metric: MetricTarget,
    #[serde(default)]
    pub output_files: Vec<String>,
    #[serde(default)]
    pub lock_files: Vec<String>,
    #[serde(default = "default_true")]
    pub auto_lock_tests: bool,
    #[serde(default)]
    pub equivalence: EquivalencePolicy,
    #[serde(default = "default_min_speedup")]
    pub min_speedup: f64,
    #[serde(default = "default_alpha")]
    pub alpha: f64,
    #[serde(default = "default_warmup")]
    pub warmup: usize,
    #[serde(default = "default_pairs")]
    pub pairs: usize,
    pub pin_core: Option<usize>,
    #[serde(default)]
    pub allow_dirty: bool,
}

fn default_true() -> bool {
    true
}
fn default_min_speedup() -> f64 {
    1.03
}
fn default_alpha() -> f64 {
    0.01
}
fn default_warmup() -> usize {
    2
}
fn default_pairs() -> usize {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub version: String,
    pub created_at_unix: u64,
    pub host: HostInfo,
    pub bench_cmd: String,
    pub test_cmd: Option<String>,
    pub build_cmd: Option<String>,
    pub profile_cmd: String,
    #[serde(default)]
    pub metric: MetricTarget,
    pub output_files: Vec<String>,
    pub equivalence: EquivalencePolicy,
    pub min_speedup: f64,
    pub alpha: f64,
    pub warmup: usize,
    pub pairs: usize,
    pub pin_core: Option<usize>,
    pub initial_head: String,
    pub baseline_commit: String,
    pub best_commit: String,
    pub best_median_ms: f64,
    pub best_peak_rss_kb: Option<u64>,
    #[serde(default)]
    pub best_cumulative_gain: Option<f64>,
    pub pre_session_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineRecord {
    pub commit: String,
    pub output_snapshot: OutputSnapshot,
    pub timing: SampleSummary,
    pub peak_rss_kb: Option<u64>,
    pub test_passed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitReport {
    pub repo_root: String,
    pub baseline_commit: String,
    pub locked_files: Vec<String>,
    pub baseline: BaselineRecord,
    pub profile: ProfileReport,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileParams {
    pub path: Option<String>,
    pub mode: Option<String>,
    pub top_k: Option<usize>,
    pub diff_from: Option<String>,
    #[serde(default)]
    pub skip_build: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedProfile {
    fingerprint: String,
    report: ProfileReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileResponse {
    pub report: ProfileReport,
    pub diff: Option<ProfileDiff>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvalVerdict {
    KeepRecommended,
    NoiseReject,
    RegressionReject,
    EquivalenceFail,
    TestsFail,
    BuildFail,
    CheatBlocked,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvaluateParams {
    pub path: Option<String>,
    pub compare_against: Option<String>,
    pub metric: Option<MetricTarget>,
    pub pairs: Option<usize>,
    pub warmup: Option<usize>,
    pub min_speedup: Option<f64>,
    pub alpha: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRecord {
    pub evaluated_at_unix: u64,
    pub tree_fingerprint: String,
    pub compare_against: String,
    #[serde(default)]
    pub metric: MetricTarget,
    pub reference_commit: String,
    pub verdict: EvalVerdict,
    pub reason: String,
    pub tamper_violations: Vec<TamperViolation>,
    pub build_passed: Option<bool>,
    pub tests_passed: Option<bool>,
    pub equivalence: Option<EquivalenceResult>,
    pub stats: Option<ComparisonStats>,
    pub speedup_vs_baseline: Option<f64>,
    pub candidate_peak_rss_kb: Option<u64>,
    pub reference_peak_rss_kb: Option<u64>,
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointAction {
    Keep,
    Revert,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointParams {
    pub path: Option<String>,
    pub action: CheckpointAction,
    pub hypothesis: String,
    pub dead_end: Option<String>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: u32,
    pub timestamp_unix: u64,
    pub action: String,
    pub verdict: String,
    pub hypothesis: String,
    pub dead_end: Option<String>,
    pub commit: Option<String>,
    pub changed_files: Vec<String>,
    pub speedup_vs_prev: Option<f64>,
    pub speedup_vs_baseline: Option<f64>,
    pub ci95_low: Option<f64>,
    pub ci95_high: Option<f64>,
    pub mann_whitney_p: Option<f64>,
    pub candidate_median_ms: Option<f64>,
    pub candidate_peak_rss_kb: Option<u64>,
    pub stdout_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointResponse {
    pub entry: LedgerEntry,
    pub best_commit: String,
    pub cumulative_speedup_vs_baseline: f64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerResponse {
    pub repo_root: String,
    pub baseline_commit: String,
    pub best_commit: String,
    pub baseline_median_ms: f64,
    pub best_median_ms: f64,
    pub cumulative_speedup: f64,
    pub total_experiments: usize,
    pub kept_count: usize,
    pub reverted_count: usize,
    pub entries: Vec<LedgerEntry>,
    pub pareto_frontier_ids: Vec<u32>,
    pub dead_ends: Vec<String>,
}

fn cumulative_gain(config: &SessionConfig, baseline: &BaselineRecord) -> f64 {
    if let Some(paired) = config.best_cumulative_gain {
        if paired.is_finite() && paired > 0.0 {
            return paired;
        }
    }
    match config.metric {
        MetricTarget::Wall => {
            if config.best_median_ms > 1e-9 {
                baseline.timing.median_ms / config.best_median_ms
            } else {
                1.0
            }
        }
        MetricTarget::Rss => match (baseline.peak_rss_kb, config.best_peak_rss_kb) {
            (Some(b), Some(best)) if best > 0 => b as f64 / best as f64,
            _ => 1.0,
        },
    }
}

fn ensure_worktree_ready(
    root: &Path,
    name: &str,
    commit_sha: &str,
    build_cmd: Option<&str>,
) -> Result<PathBuf, String> {
    let wt_path = oa_path(root, &format!("worktrees/{name}"));
    let stamp_path = oa_path(root, &format!("worktree_stamp_{name}"));

    let already_synced = wt_path.exists()
        && fs::read_to_string(&stamp_path)
            .map(|s| s.trim() == commit_sha)
            .unwrap_or(false);

    if !already_synced {
        git::ensure_shadow_worktree(root, &wt_path, commit_sha)?;
        if let Some(cmd) = build_cmd {
            let out = runner::run_command(cmd, &wt_path, None, false)?;
            if out.exit_code != 0 {
                return Err(format!(
                    "build_cmd failed in reference worktree '{name}' (exit {}): {}",
                    out.exit_code,
                    snippet_output(&out.stderr, 300)
                ));
            }
        }
        fs::write(&stamp_path, commit_sha)
            .map_err(|e| format!("failed to write worktree stamp: {e}"))?;
    }
    Ok(wt_path)
}

pub fn init_session(params: InitParams) -> Result<InitReport, String> {
    if params.bench_cmd.trim().is_empty() {
        return Err("bench_cmd must not be empty".to_string());
    }
    let root = resolve_root(params.path.as_deref())?;
    let initial_head = git::head_sha(&root)?;
    let dirty = git::has_uncommitted_tracked_changes(&root)?;
    if dirty && !params.allow_dirty {
        return Err(
            "repository has uncommitted tracked changes; commit/stash them or pass allow_dirty=true (--allow-dirty)"
                .to_string(),
        );
    }

    fs::create_dir_all(root.join(OA_DIR))
        .map_err(|e| format!("failed to create .oa directory: {e}"))?;
    git::ensure_oa_git_excluded(&root)?;

    let mut warnings = Vec::new();
    let (baseline_commit, pre_session_ref) = if dirty {
        let snap =
            git::snapshot_working_tree(&root, "HEAD", REF_PRE_SESSION, "oa pre-session snapshot")?;
        git::update_ref(&root, REF_BASELINE, &snap)?;
        git::update_ref(&root, REF_BEST, &snap)?;
        warnings.push(format!(
            "working tree was dirty; saved snapshot at {} ({})",
            REF_PRE_SESSION,
            short_sha(&snap)
        ));
        (snap, Some(REF_PRE_SESSION.to_string()))
    } else {
        git::update_ref(&root, REF_BASELINE, &initial_head)?;
        git::update_ref(&root, REF_BEST, &initial_head)?;
        (initial_head.clone(), None)
    };

    let locks = guard::build_locks(&root, &params.lock_files, params.auto_lock_tests)?;

    if let Some(ref bcmd) = params.build_cmd {
        let out = runner::run_command(bcmd, &root, None, false)?;
        if out.exit_code != 0 {
            return Err(format!(
                "baseline build_cmd failed (exit {}): {}",
                out.exit_code,
                snippet_output(&out.stderr, 300)
            ));
        }
    }

    // Prepare detached baseline and best shadow worktrees.
    let _ = fs::remove_file(oa_path(&root, "worktree_stamp_baseline"));
    let _ = fs::remove_file(oa_path(&root, "worktree_stamp_best"));
    ensure_worktree_ready(
        &root,
        "baseline",
        &baseline_commit,
        params.build_cmd.as_deref(),
    )?;
    ensure_worktree_ready(&root, "best", &baseline_commit, params.build_cmd.as_deref())?;

    let test_passed = if let Some(ref tcmd) = params.test_cmd {
        let out = runner::run_command(tcmd, &root, None, false)?;
        if out.exit_code != 0 {
            return Err(format!(
                "baseline test_cmd failed (exit {}): {}{}",
                out.exit_code,
                snippet_output(&out.stdout, 200),
                snippet_output(&out.stderr, 200)
            ));
        }
        Some(true)
    } else {
        None
    };

    // Warmups + baseline output snapshot + baseline timing distribution.
    for _ in 0..params.warmup {
        let _ = runner::run_command(&params.bench_cmd, &root, params.pin_core, false)?;
    }

    let snap_outcome = runner::run_command(&params.bench_cmd, &root, params.pin_core, true)?;
    if snap_outcome.exit_code != 0 {
        return Err(format!(
            "baseline bench_cmd failed (exit {}): {}",
            snap_outcome.exit_code,
            snippet_output(&snap_outcome.stderr, 300)
        ));
    }
    let output_snapshot = guard::capture_snapshot(
        &snap_outcome,
        &root,
        &params.output_files,
        &params.equivalence,
    )?;

    if let EquivalencePolicy::Oracle { ref command } = params.equivalence {
        let oracle_out = runner::run_command(command, &root, None, false)?;
        if oracle_out.exit_code != 0 {
            return Err(format!(
                "baseline oracle command failed (exit {}): {}",
                oracle_out.exit_code,
                snippet_output(&oracle_out.stderr, 300)
            ));
        }
    }

    let sample_count = params.pairs.clamp(3, 10);
    let mut times = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        let out = runner::run_command(&params.bench_cmd, &root, params.pin_core, false)?;
        times.push(out.wall_ms);
    }
    let timing = stats::summarize(&times);
    if timing.cv_pct > 15.0 {
        warnings.push(format!(
            "baseline timing variance is high (CV {:.1}%); consider increasing workload size or pinning a CPU core with pin_core",
            timing.cv_pct
        ));
    }

    let profile_cmd = params
        .profile_cmd
        .clone()
        .unwrap_or_else(|| params.bench_cmd.clone());
    let profile = profiler::run_profile(&profile_cmd, &root, "auto", 10)?;

    let baseline = BaselineRecord {
        commit: baseline_commit.clone(),
        output_snapshot,
        timing: timing.clone(),
        peak_rss_kb: snap_outcome.peak_rss_kb,
        test_passed,
    };

    let config = SessionConfig {
        version: env!("CARGO_PKG_VERSION").to_string(),
        created_at_unix: now_unix(),
        host: runner::collect_host_info(),
        bench_cmd: params.bench_cmd,
        test_cmd: params.test_cmd,
        build_cmd: params.build_cmd,
        profile_cmd,
        metric: params.metric,
        output_files: params.output_files,
        equivalence: params.equivalence,
        min_speedup: params.min_speedup.max(1.001),
        alpha: params.alpha.clamp(0.0001, 0.2),
        warmup: params.warmup,
        pairs: params.pairs.max(3),
        pin_core: params.pin_core,
        initial_head,
        baseline_commit: baseline_commit.clone(),
        best_commit: baseline_commit.clone(),
        best_median_ms: timing.median_ms,
        best_peak_rss_kb: snap_outcome.peak_rss_kb,
        best_cumulative_gain: Some(1.0),
        pre_session_ref,
    };

    write_json(&oa_path(&root, "config.json"), &config)?;
    write_json(&oa_path(&root, "locks.json"), &locks)?;
    write_json(&oa_path(&root, "baseline.json"), &baseline)?;
    write_json(&oa_path(&root, "baseline_profile.json"), &profile)?;
    write_json(&oa_path(&root, "best_profile.json"), &profile)?;
    let _ = fs::remove_file(oa_path(&root, "last_eval.json"));
    let _ = fs::remove_file(oa_path(&root, "last_profile.json"));
    fs::write(oa_path(&root, "ledger.jsonl"), "")
        .map_err(|e| format!("failed to initialize ledger.jsonl: {e}"))?;

    let locked_files: Vec<String> = locks.files.keys().cloned().collect();

    Ok(InitReport {
        repo_root: root.to_string_lossy().to_string(),
        baseline_commit,
        locked_files,
        baseline,
        profile,
        warnings,
    })
}

pub fn profile_session(params: ProfileParams) -> Result<ProfileResponse, String> {
    let root = resolve_root(params.path.as_deref())?;
    let config: SessionConfig = read_json(&oa_path(&root, "config.json"))?;

    if !params.skip_build {
        if let Some(ref bcmd) = config.build_cmd {
            let out = runner::run_command(bcmd, &root, None, false)?;
            if out.exit_code != 0 {
                return Err(format!(
                    "build_cmd failed before profiling (exit {}): {}",
                    out.exit_code,
                    snippet_output(&out.stderr, 300)
                ));
            }
        }
    }

    let mode = params.mode.as_deref().unwrap_or("auto");
    let top_k = params.top_k.unwrap_or(10).clamp(1, 30);
    let report = profiler::run_profile(&config.profile_cmd, &root, mode, top_k)?;

    let fingerprint = git::working_tree_fingerprint(&root)?;
    let _ = write_json(
        &oa_path(&root, "last_profile.json"),
        &CachedProfile {
            fingerprint,
            report: report.clone(),
        },
    );

    let diff = match params.diff_from.as_deref() {
        Some("baseline") => {
            let prev: ProfileReport = read_json(&oa_path(&root, "baseline_profile.json"))?;
            Some(profiler::diff_profiles(&prev, &report))
        }
        Some("best") => {
            let prev: ProfileReport = read_json(&oa_path(&root, "best_profile.json"))?;
            Some(profiler::diff_profiles(&prev, &report))
        }
        Some(other) => {
            return Err(format!(
                "invalid diff_from '{other}': expected 'baseline' or 'best'"
            ));
        }
        None => None,
    };

    Ok(ProfileResponse { report, diff })
}

pub fn evaluate_session(params: EvaluateParams) -> Result<EvalRecord, String> {
    let root = resolve_root(params.path.as_deref())?;
    let config: SessionConfig = read_json(&oa_path(&root, "config.json"))?;
    let locks: FileLocks = read_json(&oa_path(&root, "locks.json"))?;
    let baseline: BaselineRecord = read_json(&oa_path(&root, "baseline.json"))?;

    let compare_against = params
        .compare_against
        .as_deref()
        .unwrap_or("best")
        .to_ascii_lowercase();
    let (target_name, target_ref, reference_commit) = match compare_against.as_str() {
        "best" => ("best", REF_BEST, config.best_commit.clone()),
        "baseline" => ("baseline", REF_BASELINE, config.baseline_commit.clone()),
        other => {
            return Err(format!(
                "invalid compare_against '{other}': expected 'best' or 'baseline'"
            ));
        }
    };

    let metric = params.metric.unwrap_or(config.metric);
    let changed_files = git::changed_files_vs_ref(&root, target_ref)?;
    let tree_fingerprint = git::working_tree_fingerprint(&root)?;

    // 1. Cheat Guard check on locked test/bench/oracle files.
    let tamper_violations = guard::check_locks(&root, &locks);
    if !tamper_violations.is_empty() {
        let paths: Vec<&str> = tamper_violations.iter().map(|v| v.path.as_str()).collect();
        let rec = EvalRecord {
            evaluated_at_unix: now_unix(),
            tree_fingerprint,
            compare_against,
            metric,
            reference_commit,
            verdict: EvalVerdict::CheatBlocked,
            reason: format!(
                "Cheat Guard blocked evaluation: protected file(s) modified or deleted ({})",
                paths.join(", ")
            ),
            tamper_violations,
            build_passed: None,
            tests_passed: None,
            equivalence: None,
            stats: None,
            speedup_vs_baseline: None,
            candidate_peak_rss_kb: None,
            reference_peak_rss_kb: None,
            changed_files,
            warnings: Vec::new(),
        };
        write_json(&oa_path(&root, "last_eval.json"), &rec)?;
        return Ok(rec);
    }

    // 2. Build candidate and verify reference worktree is built.
    let build_passed = if let Some(ref bcmd) = config.build_cmd {
        let out = runner::run_command(bcmd, &root, None, false)?;
        if out.exit_code != 0 {
            let rec = EvalRecord {
                evaluated_at_unix: now_unix(),
                tree_fingerprint,
                compare_against,
                metric,
                reference_commit,
                verdict: EvalVerdict::BuildFail,
                reason: format!(
                    "candidate build_cmd failed (exit {}): {}",
                    out.exit_code,
                    snippet_output(&out.stderr, 300)
                ),
                tamper_violations: Vec::new(),
                build_passed: Some(false),
                tests_passed: None,
                equivalence: None,
                stats: None,
                speedup_vs_baseline: None,
                candidate_peak_rss_kb: None,
                reference_peak_rss_kb: None,
                changed_files,
                warnings: Vec::new(),
            };
            write_json(&oa_path(&root, "last_eval.json"), &rec)?;
            return Ok(rec);
        }
        Some(true)
    } else {
        None
    };

    let ref_wt = ensure_worktree_ready(
        &root,
        target_name,
        &reference_commit,
        config.build_cmd.as_deref(),
    )?;

    // 3. Test suite gate.
    let tests_passed = if let Some(ref tcmd) = config.test_cmd {
        let out = runner::run_command(tcmd, &root, None, false)?;
        if out.exit_code != 0 {
            let rec = EvalRecord {
                evaluated_at_unix: now_unix(),
                tree_fingerprint,
                compare_against,
                metric,
                reference_commit,
                verdict: EvalVerdict::TestsFail,
                reason: format!(
                    "test_cmd failed (exit {}): {}{}",
                    out.exit_code,
                    snippet_output(&out.stdout, 200),
                    snippet_output(&out.stderr, 200)
                ),
                tamper_violations: Vec::new(),
                build_passed,
                tests_passed: Some(false),
                equivalence: None,
                stats: None,
                speedup_vs_baseline: None,
                candidate_peak_rss_kb: None,
                reference_peak_rss_kb: None,
                changed_files,
                warnings: Vec::new(),
            };
            write_json(&oa_path(&root, "last_eval.json"), &rec)?;
            return Ok(rec);
        }
        Some(true)
    } else {
        None
    };

    // 4. Behavioral equivalence gate + candidate peak RSS measurement.
    let cand_check = runner::run_command(&config.bench_cmd, &root, config.pin_core, true)?;
    let mut candidate_peak_rss_kb = cand_check.peak_rss_kb;
    let eq = guard::verify_equivalence(
        &baseline.output_snapshot,
        &cand_check,
        &root,
        &config.output_files,
        &config.equivalence,
    )?;
    if !eq.passed {
        let rec = EvalRecord {
            evaluated_at_unix: now_unix(),
            tree_fingerprint,
            compare_against,
            metric,
            reference_commit,
            verdict: EvalVerdict::EquivalenceFail,
            reason: format!("equivalence check failed: {}", eq.details),
            tamper_violations: Vec::new(),
            build_passed,
            tests_passed,
            equivalence: Some(eq),
            stats: None,
            speedup_vs_baseline: None,
            candidate_peak_rss_kb,
            reference_peak_rss_kb: None,
            changed_files,
            warnings: Vec::new(),
        };
        write_json(&oa_path(&root, "last_eval.json"), &rec)?;
        return Ok(rec);
    }

    // 5. Reference rusage capture + balanced interleaved warmups & measured A/B pairs.
    let ref_check = runner::run_command(&config.bench_cmd, &ref_wt, config.pin_core, true)?;
    let mut reference_peak_rss_kb = ref_check.peak_rss_kb;

    let measure_rss = metric == MetricTarget::Rss;
    if measure_rss && (candidate_peak_rss_kb.is_none() || reference_peak_rss_kb.is_none()) {
        return Err(
            "metric 'rss' requires peak RSS measurement (/usr/bin/time or python3 on Linux/macOS; python Win32 K32GetProcessMemoryInfo on Windows)"
                .to_string(),
        );
    }

    let warmup = params.warmup.unwrap_or(config.warmup);
    let pairs = params.pairs.unwrap_or(config.pairs).max(3);
    let min_speedup = params.min_speedup.unwrap_or(config.min_speedup).max(1.001);
    let alpha = params.alpha.unwrap_or(config.alpha).clamp(0.0001, 0.2);

    for i in 0..warmup {
        if i % 2 == 0 {
            let _ = runner::run_command(&config.bench_cmd, &ref_wt, config.pin_core, false)?;
            let _ = runner::run_command(&config.bench_cmd, &root, config.pin_core, false)?;
        } else {
            let _ = runner::run_command(&config.bench_cmd, &root, config.pin_core, false)?;
            let _ = runner::run_command(&config.bench_cmd, &ref_wt, config.pin_core, false)?;
        }
    }

    let mut ref_samples = Vec::with_capacity(pairs);
    let mut cand_samples = Vec::with_capacity(pairs);

    for i in 0..pairs {
        let (r, c) = if i % 2 == 0 {
            let r = runner::run_command(&config.bench_cmd, &ref_wt, config.pin_core, measure_rss)?;
            let c = runner::run_command(&config.bench_cmd, &root, config.pin_core, measure_rss)?;
            (r, c)
        } else {
            let c = runner::run_command(&config.bench_cmd, &root, config.pin_core, measure_rss)?;
            let r = runner::run_command(&config.bench_cmd, &ref_wt, config.pin_core, measure_rss)?;
            (r, c)
        };
        if measure_rss {
            let r_kb = r
                .peak_rss_kb
                .ok_or_else(|| "missing reference peak_rss_kb during rss evaluation".to_string())?;
            let c_kb = c
                .peak_rss_kb
                .ok_or_else(|| "missing candidate peak_rss_kb during rss evaluation".to_string())?;
            ref_samples.push(r_kb as f64);
            cand_samples.push(c_kb as f64);
        } else {
            ref_samples.push(r.wall_ms);
            cand_samples.push(c.wall_ms);
        }
    }

    let cmp = stats::compare_samples(&ref_samples, &cand_samples, min_speedup, alpha);
    let verdict = match cmp.verdict {
        BenchVerdict::Faster => EvalVerdict::KeepRecommended,
        BenchVerdict::Noise => EvalVerdict::NoiseReject,
        BenchVerdict::Slower => EvalVerdict::RegressionReject,
    };

    if measure_rss {
        reference_peak_rss_kb = Some(cmp.baseline.median_ms.round() as u64);
        candidate_peak_rss_kb = Some(cmp.candidate.median_ms.round() as u64);
    }

    let speedup_vs_baseline = if compare_against == "baseline" {
        cmp.paired_ratio_median
    } else if measure_rss {
        match (baseline.peak_rss_kb, candidate_peak_rss_kb) {
            (Some(b), Some(c)) if c > 0 => b as f64 / c as f64,
            _ => cmp.paired_ratio_median,
        }
    } else {
        cumulative_gain(&config, &baseline) * cmp.paired_ratio_median
    };

    let warnings = collect_noise_warnings(&cmp, metric);
    let rec = EvalRecord {
        evaluated_at_unix: now_unix(),
        tree_fingerprint,
        compare_against,
        metric,
        reference_commit,
        verdict,
        reason: cmp.reason.clone(),
        tamper_violations: Vec::new(),
        build_passed,
        tests_passed,
        equivalence: Some(eq),
        stats: Some(cmp),
        speedup_vs_baseline: Some(speedup_vs_baseline),
        candidate_peak_rss_kb,
        reference_peak_rss_kb,
        changed_files,
        warnings,
    };

    write_json(&oa_path(&root, "last_eval.json"), &rec)?;
    Ok(rec)
}

fn read_ledger_entries(root: &Path) -> Result<Vec<LedgerEntry>, String> {
    let path = oa_path(root, "ledger.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path).map_err(|e| format!("failed to read ledger.jsonl: {e}"))?;
    let mut entries = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let item: LedgerEntry = serde_json::from_str(t)
            .map_err(|e| format!("failed to parse ledger.jsonl line {}: {e}", idx + 1))?;
        entries.push(item);
    }
    Ok(entries)
}

fn append_ledger_entry(root: &Path, entry: &LedgerEntry) -> Result<(), String> {
    let path = oa_path(root, "ledger.jsonl");
    let line = serde_json::to_string(entry)
        .map_err(|e| format!("failed to serialize ledger entry: {e}"))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("failed to open ledger.jsonl: {e}"))?;
    writeln!(file, "{line}").map_err(|e| format!("failed to append ledger.jsonl: {e}"))
}

pub fn checkpoint_session(params: CheckpointParams) -> Result<CheckpointResponse, String> {
    let root = resolve_root(params.path.as_deref())?;
    let mut config: SessionConfig = read_json(&oa_path(&root, "config.json"))?;
    let locks: FileLocks = read_json(&oa_path(&root, "locks.json"))?;
    let baseline: BaselineRecord = read_json(&oa_path(&root, "baseline.json"))?;
    let entries = read_ledger_entries(&root)?;
    let next_id = entries.len() as u32 + 1;

    let last_eval: Option<EvalRecord> = read_json(&oa_path(&root, "last_eval.json")).ok();

    match params.action {
        CheckpointAction::Keep => {
            let violations = guard::check_locks(&root, &locks);
            if !violations.is_empty() {
                let paths: Vec<&str> = violations.iter().map(|v| v.path.as_str()).collect();
                return Err(format!(
                    "Cheat Guard refused keep: protected file(s) modified or deleted ({})",
                    paths.join(", ")
                ));
            }
            let eval = last_eval.ok_or_else(|| {
                "cannot keep without a completed evaluation; run oa_evaluate first".to_string()
            })?;
            let current_fp = git::working_tree_fingerprint(&root)?;
            if current_fp != eval.tree_fingerprint {
                return Err(
                    "Cheat Guard refused keep: working tree changed since last oa_evaluate; re-run oa_evaluate"
                        .to_string(),
                );
            }
            if eval.verdict != EvalVerdict::KeepRecommended && !params.force {
                return Err(format!(
                    "cannot keep: last evaluation verdict was {:?} ({}). Pass force=true only if intentionally overriding.",
                    eval.verdict, eval.reason
                ));
            }

            let commit_msg = format!("oa exp #{next_id}: {}", params.hypothesis);
            let exp_ref = format!("refs/oa/exp/{next_id}");
            let new_sha = git::snapshot_working_tree(&root, REF_BEST, REF_BEST, &commit_msg)?;
            git::update_ref(&root, &exp_ref, &new_sha)?;

            let _ = fs::remove_file(oa_path(&root, "worktree_stamp_best"));
            ensure_worktree_ready(&root, "best", &new_sha, config.build_cmd.as_deref())?;

            config.best_commit = new_sha.clone();
            if eval.metric == MetricTarget::Wall {
                if let Some(ref st) = eval.stats {
                    config.best_median_ms = st.candidate.median_ms;
                }
            }
            if eval.candidate_peak_rss_kb.is_some() {
                config.best_peak_rss_kb = eval.candidate_peak_rss_kb;
            }
            if let Some(vs_base) = eval.speedup_vs_baseline {
                config.best_cumulative_gain = Some(vs_base);
            }
            write_json(&oa_path(&root, "config.json"), &config)?;

            if let Ok(cached) = read_json::<CachedProfile>(&oa_path(&root, "last_profile.json")) {
                if cached.fingerprint == current_fp {
                    let _ = write_json(&oa_path(&root, "best_profile.json"), &cached.report);
                }
            }

            let entry = LedgerEntry {
                id: next_id,
                timestamp_unix: now_unix(),
                action: "KEPT".to_string(),
                verdict: format!("{:?}", eval.verdict),
                hypothesis: params.hypothesis,
                dead_end: None,
                commit: Some(new_sha.clone()),
                changed_files: eval.changed_files,
                speedup_vs_prev: eval.stats.as_ref().map(|s| s.paired_ratio_median),
                speedup_vs_baseline: eval.speedup_vs_baseline,
                ci95_low: eval.stats.as_ref().map(|s| s.ci95_low),
                ci95_high: eval.stats.as_ref().map(|s| s.ci95_high),
                mann_whitney_p: eval.stats.as_ref().map(|s| s.mann_whitney_p),
                candidate_median_ms: eval.stats.as_ref().map(|s| s.candidate.median_ms),
                candidate_peak_rss_kb: eval.candidate_peak_rss_kb,
                stdout_sha256: eval.equivalence.map(|e| e.candidate_stdout_sha256),
            };
            append_ledger_entry(&root, &entry)?;

            let cumulative = cumulative_gain(&config, &baseline);

            Ok(CheckpointResponse {
                entry,
                best_commit: new_sha.clone(),
                cumulative_speedup_vs_baseline: cumulative,
                message: format!(
                    "Kept experiment #{next_id} at {} (cumulative {:.2}x vs baseline).",
                    short_sha(&new_sha),
                    cumulative
                ),
            })
        }
        CheckpointAction::Revert => {
            let changed_files = git::changed_files_vs_ref(&root, REF_BEST)?;
            git::restore_working_tree_to_ref(&root, REF_BEST)?;
            let _ = fs::remove_file(oa_path(&root, "last_eval.json"));

            let (verdict_str, default_dead_end, stats_opt, cand_rss) = match last_eval {
                Some(ev) => (
                    format!("{:?}", ev.verdict),
                    Some(ev.reason),
                    ev.stats,
                    ev.candidate_peak_rss_kb,
                ),
                None => ("MANUAL_REVERT".to_string(), None, None, None),
            };

            let entry = LedgerEntry {
                id: next_id,
                timestamp_unix: now_unix(),
                action: "REVERTED".to_string(),
                verdict: verdict_str,
                hypothesis: params.hypothesis,
                dead_end: params.dead_end.or(default_dead_end),
                commit: None,
                changed_files,
                speedup_vs_prev: stats_opt.as_ref().map(|s| s.paired_ratio_median),
                speedup_vs_baseline: None,
                ci95_low: stats_opt.as_ref().map(|s| s.ci95_low),
                ci95_high: stats_opt.as_ref().map(|s| s.ci95_high),
                mann_whitney_p: stats_opt.as_ref().map(|s| s.mann_whitney_p),
                candidate_median_ms: stats_opt.as_ref().map(|s| s.candidate.median_ms),
                candidate_peak_rss_kb: cand_rss,
                stdout_sha256: None,
            };
            append_ledger_entry(&root, &entry)?;

            let cumulative = cumulative_gain(&config, &baseline);

            Ok(CheckpointResponse {
                entry,
                best_commit: config.best_commit.clone(),
                cumulative_speedup_vs_baseline: cumulative,
                message: format!(
                    "Reverted working tree to best commit {} and recorded dead end #{next_id}.",
                    short_sha(&config.best_commit)
                ),
            })
        }
    }
}

fn compute_pareto_frontier(entries: &[LedgerEntry]) -> Vec<u32> {
    let kept: Vec<&LedgerEntry> = entries
        .iter()
        .filter(|e| e.action == "KEPT" && e.candidate_median_ms.is_some())
        .collect();

    let mut frontier = Vec::new();
    for a in &kept {
        let a_ms = a.candidate_median_ms.unwrap_or(f64::INFINITY);
        let a_rss = a.candidate_peak_rss_kb.unwrap_or(u64::MAX);
        let dominated = kept.iter().any(|b| {
            if b.id == a.id {
                return false;
            }
            let b_ms = b.candidate_median_ms.unwrap_or(f64::INFINITY);
            let b_rss = b.candidate_peak_rss_kb.unwrap_or(u64::MAX);
            b_ms <= a_ms && b_rss <= a_rss && (b_ms < a_ms || b_rss < a_rss)
        });
        if !dominated {
            frontier.push(a.id);
        }
    }
    frontier
}

pub fn get_ledger(path: Option<&str>) -> Result<LedgerResponse, String> {
    let root = resolve_root(path)?;
    let config: SessionConfig = read_json(&oa_path(&root, "config.json"))?;
    let baseline: BaselineRecord = read_json(&oa_path(&root, "baseline.json"))?;
    let entries = read_ledger_entries(&root)?;

    let kept_count = entries.iter().filter(|e| e.action == "KEPT").count();
    let reverted_count = entries.iter().filter(|e| e.action == "REVERTED").count();
    let pareto_frontier_ids = compute_pareto_frontier(&entries);
    let dead_ends: Vec<String> = entries
        .iter()
        .filter(|e| e.action == "REVERTED")
        .map(|e| {
            let why = e.dead_end.as_deref().unwrap_or("reverted");
            format!("#{} ({}): {}", e.id, e.hypothesis, why)
        })
        .collect();

    let cumulative_speedup = cumulative_gain(&config, &baseline);

    Ok(LedgerResponse {
        repo_root: root.to_string_lossy().to_string(),
        baseline_commit: config.baseline_commit,
        best_commit: config.best_commit,
        baseline_median_ms: baseline.timing.median_ms,
        best_median_ms: config.best_median_ms,
        cumulative_speedup,
        total_experiments: entries.len(),
        kept_count,
        reverted_count,
        entries,
        pareto_frontier_ids,
        dead_ends,
    })
}

pub fn render_markdown_report(path: Option<&str>) -> Result<String, String> {
    let root = resolve_root(path)?;
    let config: SessionConfig = read_json(&oa_path(&root, "config.json"))?;
    let baseline: BaselineRecord = read_json(&oa_path(&root, "baseline.json"))?;
    let ledger = get_ledger(path)?;

    let mut md = String::new();
    md.push_str("# oa Optimization Report\n\n");
    md.push_str(&format!(
        "- Repository: `{}`\n",
        root.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("workspace")
    ));
    md.push_str(&format!(
        "- Host: `{} {}` ({}, {} CPUs)\n",
        config.host.os, config.host.arch, config.host.cpu_model, config.host.logical_cpus
    ));
    md.push_str(&format!("- Benchmark command: `{}`\n", config.bench_cmd));
    if let Some(ref tc) = config.test_cmd {
        md.push_str(&format!("- Test command: `{tc}`\n"));
    }
    md.push_str(&format!(
        "- Baseline commit: `{}` -> Best commit: `{}`\n",
        short_sha(&ledger.baseline_commit),
        short_sha(&ledger.best_commit)
    ));
    md.push_str(&format!(
        "- Output equivalence SHA-256: `{}` (exit {})\n\n",
        &baseline.output_snapshot.stdout_sha256[..16],
        baseline.output_snapshot.exit_code
    ));

    md.push_str("## Summary\n\n");
    md.push_str("| Metric | Baseline | Best | Delta |\n");
    md.push_str("| --- | ---: | ---: | ---: |\n");
    if config.metric == MetricTarget::Wall {
        md.push_str(&format!(
            "| Median wall time | {:.2} ms | {:.2} ms | **{:.2}x** |\n",
            ledger.baseline_median_ms, ledger.best_median_ms, ledger.cumulative_speedup
        ));
    } else {
        let wall_ratio = if ledger.best_median_ms > 1e-9 {
            ledger.baseline_median_ms / ledger.best_median_ms
        } else {
            1.0
        };
        md.push_str(&format!(
            "| Median wall time | {:.2} ms | {:.2} ms | {:.2}x |\n",
            ledger.baseline_median_ms, ledger.best_median_ms, wall_ratio
        ));
    }
    if let (Some(b_rss), Some(best_rss)) = (baseline.peak_rss_kb, config.best_peak_rss_kb) {
        let delta_pct = if b_rss > 0 {
            ((best_rss as f64 - b_rss as f64) / b_rss as f64) * 100.0
        } else {
            0.0
        };
        if config.metric == MetricTarget::Rss && best_rss > 0 {
            let ratio = b_rss as f64 / best_rss as f64;
            md.push_str(&format!(
                "| Peak RSS | {} KiB | {} KiB | **{:.2}x** ({:+.1}%) |\n",
                b_rss, best_rss, ratio, delta_pct
            ));
        } else {
            md.push_str(&format!(
                "| Peak RSS | {} KiB | {} KiB | {:+.1}% |\n",
                b_rss, best_rss, delta_pct
            ));
        }
    }
    md.push_str(&format!(
        "| Experiments | 0 | {} ({} kept, {} reverted) | Pareto IDs: {:?} |\n\n",
        ledger.total_experiments,
        ledger.kept_count,
        ledger.reverted_count,
        ledger.pareto_frontier_ids
    ));

    md.push_str("## Experiment Ledger\n\n");
    if ledger.entries.is_empty() {
        md.push_str("No experiments recorded yet.\n\n");
    } else {
        let (gain_col, med_col) = match config.metric {
            MetricTarget::Wall => ("Speedup", "Median ms"),
            MetricTarget::Rss => ("Reduction", "Median KiB"),
        };
        md.push_str(&format!(
            "| # | Status | Hypothesis | Files | {gain_col} | 95% CI | p-value | {med_col} |\n"
        ));
        md.push_str("| ---: | --- | --- | --- | ---: | --- | ---: | ---: |\n");
        for e in &ledger.entries {
            let files = if e.changed_files.is_empty() {
                "-".to_string()
            } else {
                e.changed_files.join(", ")
            };
            let sp = e
                .speedup_vs_prev
                .map(|v| format!("{v:.2}x"))
                .unwrap_or_else(|| "-".to_string());
            let ci = match (e.ci95_low, e.ci95_high) {
                (Some(lo), Some(hi)) => format!("[{lo:.2}x, {hi:.2}x]"),
                _ => "-".to_string(),
            };
            let pval = e
                .mann_whitney_p
                .map(|v| format!("{v:.4}"))
                .unwrap_or_else(|| "-".to_string());
            let med = e
                .candidate_median_ms
                .map(|v| format!("{v:.2}"))
                .unwrap_or_else(|| "-".to_string());
            md.push_str(&format!(
                "| {} | {} | {} | `{}` | {} | {} | {} | {} |\n",
                e.id, e.action, e.hypothesis, files, sp, ci, pval, med
            ));
        }
        md.push('\n');
    }

    if !ledger.dead_ends.is_empty() {
        md.push_str("## Dead Ends Ruled Out\n\n");
        for d in &ledger.dead_ends {
            md.push_str(&format!("- {d}\n"));
        }
        md.push('\n');
    }

    md.push_str("## Reproduce\n\n");
    md.push_str("```bash\noa verify\noa evaluate --against baseline\n```\n");
    Ok(md)
}

/// Renders a dark or light SVG chart of the current optimization session.
pub fn render_svg_report(
    path: Option<&str>,
    theme: crate::svg::SvgTheme,
) -> Result<String, String> {
    let root = resolve_root(path)?;
    let config: SessionConfig = read_json(&oa_path(&root, "config.json"))?;
    let baseline: BaselineRecord = read_json(&oa_path(&root, "baseline.json"))?;
    let locks: FileLocks = read_json(&oa_path(&root, "locks.json"))?;
    let ledger = get_ledger(path)?;

    let repo_name = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("workspace")
        .to_string();

    let (baseline_value, best_value) = match config.metric {
        MetricTarget::Wall => (ledger.baseline_median_ms, ledger.best_median_ms),
        MetricTarget::Rss => (
            baseline.peak_rss_kb.unwrap_or(0) as f64,
            config
                .best_peak_rss_kb
                .or(baseline.peak_rss_kb)
                .unwrap_or(0) as f64,
        ),
    };

    let mut steps = Vec::new();
    for e in ledger.entries.iter().filter(|e| e.action == "KEPT") {
        let is_best = e.commit.as_deref() == Some(config.best_commit.as_str());
        let raw_val = match config.metric {
            MetricTarget::Wall => e.candidate_median_ms.unwrap_or(ledger.best_median_ms),
            MetricTarget::Rss => e
                .candidate_peak_rss_kb
                .map(|v| v as f64)
                .or(e.candidate_median_ms)
                .unwrap_or(best_value),
        };
        let gain = if is_best && ledger.cumulative_speedup > 0.0 {
            ledger.cumulative_speedup
        } else if let Some(g) = e.speedup_vs_baseline.filter(|g| g.is_finite() && *g > 0.0) {
            g
        } else if raw_val > 1e-9 && baseline_value > 1e-9 {
            baseline_value / raw_val
        } else {
            1.0
        };
        let val = if is_best && best_value > 1e-9 {
            best_value
        } else if config.metric == MetricTarget::Wall && baseline_value > 1e-9 && gain > 1e-9 {
            let expected = baseline_value / gain;
            if (raw_val - expected).abs() / expected > 0.15 {
                expected
            } else {
                raw_val
            }
        } else {
            raw_val
        };
        let subtitle = if e.changed_files.is_empty() {
            format!("#{} kept", e.id)
        } else {
            format!("#{} {}", e.id, e.changed_files.join(", "))
        };
        steps.push(crate::svg::SvgStep {
            label: e.hypothesis.clone(),
            subtitle,
            value: val,
            gain,
            p_value: e.mann_whitney_p,
        });
    }

    let data = crate::svg::SvgChartData {
        repo_name,
        metric: config.metric,
        baseline_value,
        best_value,
        cumulative_gain: ledger.cumulative_speedup,
        rss_before_kb: baseline.peak_rss_kb,
        rss_after_kb: config.best_peak_rss_kb,
        sha256_prefix: baseline.output_snapshot.stdout_sha256[..16].to_string(),
        locked_files: locks.files.len(),
        kept_count: ledger.kept_count,
        reverted_count: ledger.reverted_count,
        steps,
    };

    Ok(crate::svg::render_session_svg(&data, theme))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifyParams {
    pub path: Option<String>,
    pub metric: Option<MetricTarget>,
    pub pairs: Option<usize>,
    pub warmup: Option<usize>,
    pub min_speedup: Option<f64>,
    pub alpha: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    pub passed: bool,
    pub badge: String,
    pub locked_files_count: usize,
    pub cheat_guard_ok: bool,
    pub evaluation: EvalRecord,
}

/// Runs a fresh end-to-end verification of HEAD against `refs/oa/baseline` (CI / PR gate).
pub fn verify_session(params: VerifyParams) -> Result<VerifyReport, String> {
    let root = resolve_root(params.path.as_deref())?;
    let locks: FileLocks = read_json(&oa_path(&root, "locks.json"))?;
    let locked_files_count = locks.files.len();

    let eval = evaluate_session(EvaluateParams {
        path: params.path,
        compare_against: Some("baseline".to_string()),
        metric: params.metric,
        pairs: Some(params.pairs.unwrap_or(15)),
        warmup: params.warmup,
        min_speedup: params.min_speedup,
        alpha: params.alpha,
    })?;

    let cheat_guard_ok = eval.tamper_violations.is_empty();
    let passed = eval.verdict == EvalVerdict::KeepRecommended && cheat_guard_ok;
    if passed {
        if let Ok(mut cfg) = read_json::<SessionConfig>(&oa_path(&root, "config.json")) {
            if let Some(ref st) = eval.stats {
                cfg.best_cumulative_gain = Some(st.paired_ratio_median);
                if eval.metric == MetricTarget::Wall {
                    cfg.best_median_ms = st.candidate.median_ms;
                }
            }
            if eval.candidate_peak_rss_kb.is_some() {
                cfg.best_peak_rss_kb = eval.candidate_peak_rss_kb;
            }
            let _ = write_json(&oa_path(&root, "config.json"), &cfg);
        }
    }

    let badge = if passed {
        let st = eval.stats.as_ref();
        let metric_name = match eval.metric {
            MetricTarget::Wall => "wall",
            MetricTarget::Rss => "peak RSS",
        };
        let gain_part = st
            .map(|s| {
                format!(
                    "{:.2}x {metric_name} (95% CI [{:.2}x, {:.2}x], p={:.4})",
                    s.paired_ratio_median, s.ci95_low, s.ci95_high, s.mann_whitney_p
                )
            })
            .unwrap_or_else(|| "verified".to_string());
        let rss_part = match (eval.reference_peak_rss_kb, eval.candidate_peak_rss_kb) {
            (Some(r), Some(c)) if r > 0 && eval.metric == MetricTarget::Wall => {
                let pct = ((c as f64 - r as f64) / r as f64) * 100.0;
                format!(", {pct:+.1}% peak RSS")
            }
            _ => String::new(),
        };
        format!(
            "VERIFIED: {gain_part}{rss_part}, SHA-256 exact, {locked_files_count} locked file(s) intact"
        )
    } else {
        format!("FAILED ({:?}): {}", eval.verdict, eval.reason)
    };

    Ok(VerifyReport {
        passed,
        badge,
        locked_files_count,
        cheat_guard_ok,
        evaluation: eval,
    })
}

fn collect_noise_warnings(cmp: &ComparisonStats, metric: MetricTarget) -> Vec<String> {
    let mut warnings = Vec::new();
    if metric == MetricTarget::Wall && cmp.candidate.median_ms < 15.0 {
        warnings.push(format!(
            "candidate median is {:.2} ms (<15 ms); process spawn overhead may dilute ratios (increase inner workload iterations for cleaner signal)",
            cmp.candidate.median_ms
        ));
    }
    if cmp.baseline.cv_pct > 8.0 || cmp.candidate.cv_pct > 8.0 {
        warnings.push(format!(
            "elevated run-to-run variance detected (ref CV {:.1}%, cand CV {:.1}%); consider --pin-core or more --pairs",
            cmp.baseline.cv_pct, cmp.candidate.cv_pct
        ));
    }
    warnings
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStatus {
    pub repo_root: String,
    pub metric: MetricTarget,
    pub bench_cmd: String,
    pub test_cmd: Option<String>,
    pub baseline_commit: String,
    pub best_commit: String,
    pub baseline_median_ms: f64,
    pub best_median_ms: f64,
    pub baseline_peak_rss_kb: Option<u64>,
    pub best_peak_rss_kb: Option<u64>,
    pub cumulative_gain: f64,
    pub locked_files_count: usize,
    pub cheat_guard_ok: bool,
    pub cheat_violations: Vec<String>,
    pub modified_vs_best: Vec<String>,
    pub kept_count: usize,
    pub reverted_count: usize,
    pub pareto_frontier_ids: Vec<u32>,
    pub diff_stat_vs_baseline: String,
}

/// Returns live health, Cheat Guard status, progress, and diff summary of an active session.
pub fn session_status(path: Option<&str>) -> Result<SessionStatus, String> {
    let root = resolve_root(path)?;
    let config: SessionConfig = read_json(&oa_path(&root, "config.json"))?;
    let baseline: BaselineRecord = read_json(&oa_path(&root, "baseline.json"))?;
    let locks: FileLocks = read_json(&oa_path(&root, "locks.json"))?;
    let ledger = get_ledger(path)?;

    let violations = guard::check_locks(&root, &locks);
    let cheat_violations: Vec<String> = violations
        .iter()
        .map(|v| format!("{}: {}", v.path, v.reason))
        .collect();
    let modified_vs_best = git::changed_files_vs_ref(&root, REF_BEST)?;
    let diff_stat_vs_baseline = git::diff_stat_refs(&root, REF_BASELINE, REF_BEST);

    Ok(SessionStatus {
        repo_root: root.to_string_lossy().to_string(),
        metric: config.metric,
        bench_cmd: config.bench_cmd.clone(),
        test_cmd: config.test_cmd.clone(),
        baseline_commit: config.baseline_commit.clone(),
        best_commit: config.best_commit.clone(),
        baseline_median_ms: baseline.timing.median_ms,
        best_median_ms: config.best_median_ms,
        baseline_peak_rss_kb: baseline.peak_rss_kb,
        best_peak_rss_kb: config.best_peak_rss_kb,
        cumulative_gain: cumulative_gain(&config, &baseline),
        locked_files_count: locks.files.len(),
        cheat_guard_ok: cheat_violations.is_empty(),
        cheat_violations,
        modified_vs_best,
        kept_count: ledger.kept_count,
        reverted_count: ledger.reverted_count,
        pareto_frontier_ids: ledger.pareto_frontier_ids,
        diff_stat_vs_baseline,
    })
}

#[derive(Debug, Clone)]
pub struct CompareParams {
    pub path: Option<String>,
    pub cmd_a: String,
    pub cmd_b: String,
    pub metric: MetricTarget,
    pub pairs: usize,
    pub warmup: usize,
    pub min_speedup: f64,
    pub alpha: f64,
    pub pin_core: Option<usize>,
    pub check_output: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareResult {
    pub cmd_a: String,
    pub cmd_b: String,
    pub metric: MetricTarget,
    pub verdict: stats::BenchVerdict,
    pub stats: ComparisonStats,
    pub peak_rss_a_kb: Option<u64>,
    pub peak_rss_b_kb: Option<u64>,
    pub stdout_sha256_a: String,
    pub stdout_sha256_b: String,
    pub outputs_identical: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Standalone interleaved A/B comparison of any two shell commands (no `oa init` needed).
pub fn compare_commands(params: CompareParams) -> Result<CompareResult, String> {
    let cwd = match params.path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().map_err(|e| format!("failed to get cwd: {e}"))?,
    };
    let pairs = params.pairs.max(3);
    let warmup = params.warmup;
    let measure_rss = params.metric == MetricTarget::Rss;

    // Initial check + SHA-256 + Peak RSS measurement for both commands.
    let probe_a = runner::run_command(&params.cmd_a, &cwd, params.pin_core, true)?;
    if probe_a.exit_code != 0 {
        return Err(format!(
            "command A exited {}: {}",
            probe_a.exit_code,
            String::from_utf8_lossy(&probe_a.stderr)
                .chars()
                .take(300)
                .collect::<String>()
        ));
    }
    let probe_b = runner::run_command(&params.cmd_b, &cwd, params.pin_core, true)?;
    if probe_b.exit_code != 0 {
        return Err(format!(
            "command B exited {}: {}",
            probe_b.exit_code,
            String::from_utf8_lossy(&probe_b.stderr)
                .chars()
                .take(300)
                .collect::<String>()
        ));
    }

    let sha_a = crate::sha256::sha256_hex(&probe_a.stdout);
    let sha_b = crate::sha256::sha256_hex(&probe_b.stdout);
    let outputs_identical = sha_a == sha_b;
    if params.check_output && !outputs_identical {
        return Err(format!(
            "stdout SHA-256 mismatch: A={}.. vs B={}..",
            &sha_a[..16],
            &sha_b[..16]
        ));
    }

    for i in 0..warmup {
        if i % 2 == 0 {
            let _ = runner::run_command(&params.cmd_a, &cwd, params.pin_core, false)?;
            let _ = runner::run_command(&params.cmd_b, &cwd, params.pin_core, false)?;
        } else {
            let _ = runner::run_command(&params.cmd_b, &cwd, params.pin_core, false)?;
            let _ = runner::run_command(&params.cmd_a, &cwd, params.pin_core, false)?;
        }
    }

    let mut samples_a = Vec::with_capacity(pairs);
    let mut samples_b = Vec::with_capacity(pairs);
    let mut rss_a_samples = Vec::new();
    let mut rss_b_samples = Vec::new();
    if let Some(r) = probe_a.peak_rss_kb {
        rss_a_samples.push(r);
    }
    if let Some(r) = probe_b.peak_rss_kb {
        rss_b_samples.push(r);
    }

    for i in 0..pairs {
        let (out_a, out_b) = if i % 2 == 0 {
            let a = runner::run_command(&params.cmd_a, &cwd, params.pin_core, measure_rss)?;
            let b = runner::run_command(&params.cmd_b, &cwd, params.pin_core, measure_rss)?;
            (a, b)
        } else {
            let b = runner::run_command(&params.cmd_b, &cwd, params.pin_core, measure_rss)?;
            let a = runner::run_command(&params.cmd_a, &cwd, params.pin_core, measure_rss)?;
            (a, b)
        };
        if out_a.exit_code != 0 {
            return Err(format!("command A failed on pair {}", i + 1));
        }
        if out_b.exit_code != 0 {
            return Err(format!("command B failed on pair {}", i + 1));
        }
        if let Some(r) = out_a.peak_rss_kb {
            rss_a_samples.push(r);
        }
        if let Some(r) = out_b.peak_rss_kb {
            rss_b_samples.push(r);
        }
        match params.metric {
            MetricTarget::Wall => {
                samples_a.push(out_a.wall_ms);
                samples_b.push(out_b.wall_ms);
            }
            MetricTarget::Rss => {
                let ra = out_a.peak_rss_kb.ok_or_else(|| {
                    "peak RSS measurement unavailable on this platform".to_string()
                })?;
                let rb = out_b.peak_rss_kb.ok_or_else(|| {
                    "peak RSS measurement unavailable on this platform".to_string()
                })?;
                samples_a.push(ra as f64);
                samples_b.push(rb as f64);
            }
        }
    }

    let cmp = stats::compare_samples(&samples_a, &samples_b, params.min_speedup, params.alpha);
    let verdict = cmp.verdict;
    let warnings = collect_noise_warnings(&cmp, params.metric);
    let peak_rss_a_kb = median_u64(&rss_a_samples);
    let peak_rss_b_kb = median_u64(&rss_b_samples);

    Ok(CompareResult {
        cmd_a: params.cmd_a,
        cmd_b: params.cmd_b,
        metric: params.metric,
        verdict,
        stats: cmp,
        peak_rss_a_kb,
        peak_rss_b_kb,
        stdout_sha256_a: sha_a,
        stdout_sha256_b: sha_b,
        outputs_identical,
        warnings,
    })
}

/// Formats a standalone `oa compare` result as a GitHub-flavored Markdown table.
pub fn render_compare_markdown(res: &CompareResult) -> String {
    let unit = res.metric.unit();
    let rss_row = match (res.peak_rss_a_kb, res.peak_rss_b_kb) {
        (Some(a), Some(b)) if a > 0 => {
            let pct = ((b as f64 - a as f64) / a as f64) * 100.0;
            format!("| Peak RSS | {a} KiB | {b} KiB | {pct:+.1}% |\n")
        }
        _ => String::new(),
    };
    let eq_status = if res.outputs_identical {
        format!("identical (`{}`)", &res.stdout_sha256_a[..16])
    } else {
        format!(
            "differs (`{}` vs `{}`)",
            &res.stdout_sha256_a[..16],
            &res.stdout_sha256_b[..16]
        )
    };
    format!(
        "### oa compare (`{:?}`)\n\n\
         - **Cmd A:** `{}`\n\
         - **Cmd B:** `{}`\n\
         - **Output SHA-256:** {}\n\n\
         | Metric | Cmd A | Cmd B | Result |\n\
         | --- | ---: | ---: | ---: |\n\
         | Median ({unit}) | {:.2} | {:.2} | **{:.2}x** |\n\
         | p95 / MAD ({unit}) | {:.2} / {:.2} | {:.2} / {:.2} | 95% CI [{:.2}x, {:.2}x] |\n\
         {rss_row}\
         | Significance | n={} pairs | Mann-Whitney U | `p = {:.4}` |\n",
        res.verdict,
        res.cmd_a,
        res.cmd_b,
        eq_status,
        res.stats.baseline.median_ms,
        res.stats.candidate.median_ms,
        res.stats.paired_ratio_median,
        res.stats.baseline.p95_ms,
        res.stats.baseline.mad_ms,
        res.stats.candidate.p95_ms,
        res.stats.candidate.mad_ms,
        res.stats.ci95_low,
        res.stats.ci95_high,
        res.stats.baseline.count,
        res.stats.mann_whitney_p,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffReport {
    pub repo_root: String,
    pub baseline_commit: String,
    pub best_commit: String,
    pub changed_files: Vec<String>,
    pub diff_stat: String,
    pub patch: String,
}

/// Exports the clean unified diff between `refs/oa/baseline` and `refs/oa/best`.
pub fn export_diff(path: Option<&str>) -> Result<DiffReport, String> {
    let root = resolve_root(path)?;
    let config: SessionConfig = read_json(&oa_path(&root, "config.json"))?;
    let changed_files = git::diff_name_only_refs(&root, REF_BASELINE, REF_BEST);
    let diff_stat = git::diff_stat_refs(&root, REF_BASELINE, REF_BEST);
    let patch = git::diff_refs(&root, REF_BASELINE, REF_BEST)?;

    Ok(DiffReport {
        repo_root: root.to_string_lossy().to_string(),
        baseline_commit: config.baseline_commit,
        best_commit: config.best_commit,
        changed_files,
        diff_stat,
        patch,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanReport {
    pub repo_root: String,
    pub removed_worktrees: Vec<String>,
    pub removed_oa_dir: bool,
    pub deleted_refs: Vec<String>,
}

/// Removes `.oa/` shadow worktrees and session state (and optionally `refs/oa/*`) without touching working tree edits.
pub fn clean_session(path: Option<&str>, delete_refs: bool) -> Result<CleanReport, String> {
    let root = resolve_root(path)?;
    let oa_dir = root.join(OA_DIR);
    let worktrees_dir = oa_dir.join("worktrees");
    let mut removed_worktrees = Vec::new();

    if worktrees_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&worktrees_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && git::remove_shadow_worktree(&root, &p) {
                    if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                        removed_worktrees.push(name.to_string());
                    }
                }
            }
        }
    }
    for name in ["baseline", "best"] {
        let p = worktrees_dir.join(name);
        if p.exists()
            && git::remove_shadow_worktree(&root, &p)
            && !removed_worktrees.iter().any(|x| x == name)
        {
            removed_worktrees.push(name.to_string());
        }
    }
    removed_worktrees.sort();

    let removed_oa_dir = if oa_dir.exists() {
        fs::remove_dir_all(&oa_dir)
            .map_err(|e| format!("failed to remove '{}': {e}", oa_dir.display()))?;
        true
    } else {
        false
    };

    let deleted_refs = if delete_refs {
        git::delete_oa_refs(&root)?
    } else {
        Vec::new()
    };

    Ok(CleanReport {
        repo_root: root.to_string_lossy().to_string(),
        removed_worktrees,
        removed_oa_dir,
        deleted_refs,
    })
}

fn median_u64(vals: &[u64]) -> Option<u64> {
    if vals.is_empty() {
        return None;
    }
    let mut s = vals.to_vec();
    s.sort_unstable();
    Some(s[s.len() / 2])
}
