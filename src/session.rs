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
            "metric 'rss' requires peak RSS measurement (/usr/bin/time or python3 resource.getrusage on Linux/macOS)"
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
    } else if cmp.candidate.median_ms > 1e-9 {
        baseline.timing.median_ms / cmp.candidate.median_ms
    } else {
        1.0
    };

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

            let cumulative = if config.best_median_ms > 1e-9 {
                baseline.timing.median_ms / config.best_median_ms
            } else {
                1.0
            };

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

            let cumulative = if config.best_median_ms > 1e-9 {
                baseline.timing.median_ms / config.best_median_ms
            } else {
                1.0
            };

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

    let cumulative_speedup = if config.best_median_ms > 1e-9 {
        baseline.timing.median_ms / config.best_median_ms
    } else {
        1.0
    };

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
    md.push_str(&format!(
        "| Median wall time | {:.2} ms | {:.2} ms | **{:.2}x** |\n",
        ledger.baseline_median_ms, ledger.best_median_ms, ledger.cumulative_speedup
    ));
    if let (Some(b_rss), Some(best_rss)) = (baseline.peak_rss_kb, config.best_peak_rss_kb) {
        let delta_pct = if b_rss > 0 {
            ((best_rss as f64 - b_rss as f64) / b_rss as f64) * 100.0
        } else {
            0.0
        };
        md.push_str(&format!(
            "| Peak RSS | {} KiB | {} KiB | {:+.1}% |\n",
            b_rss, best_rss, delta_pct
        ));
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
        md.push_str(
            "| # | Status | Hypothesis | Files | Speedup | 95% CI | p-value | Median ms |\n",
        );
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
    md.push_str("```bash\noa evaluate --against baseline\n```\n");
    Ok(md)
}
