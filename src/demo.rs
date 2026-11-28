use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::Serialize;

use crate::guard::EquivalencePolicy;
use crate::session::{
    checkpoint_session, evaluate_session, init_session, CheckpointAction, CheckpointParams,
    EvalVerdict, EvaluateParams, InitParams, MetricTarget,
};

static DEMO_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize)]
pub struct DemoResult {
    pub baseline_median_ms: f64,
    pub optimized_median_ms: f64,
    pub speedup: f64,
    pub ci95_low: f64,
    pub ci95_high: f64,
    pub mann_whitney_p: f64,
    pub top_hotspot: String,
    pub cheat_guard_blocked: bool,
    pub equivalence_gate_blocked: bool,
    pub stdout_sha256: String,
    pub elapsed_s: f64,
}

struct TempDemoDir(PathBuf);

impl Drop for TempDemoDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn find_python() -> Result<String, String> {
    for bin in ["python3", "python"] {
        if matches!(
            Command::new(bin).arg("--version").output(),
            Ok(out) if out.status.success()
        ) {
            return Ok(bin.to_string());
        }
    }
    Err("oa demo requires python3 or python on PATH".to_string())
}

fn run_git(dir: &PathBuf, args: &[&str]) -> Result<(), String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// Runs a self-contained end-to-end demo in a temporary git repository and cleans up on exit.
pub fn run_demo(quiet: bool) -> Result<DemoResult, String> {
    let t0 = Instant::now();
    let py = find_python()?;
    let seq = DEMO_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("oa_demo_{}_{}", std::process::id(), seq));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create demo dir: {e}"))?;
    let _cleanup = TempDemoDir(dir.clone());

    run_git(&dir, &["init"])?;
    run_git(&dir, &["config", "user.name", "oa demo"])?;
    run_git(&dir, &["config", "user.email", "demo@example.com"])?;

    let slow_src = "\
def count_unique(n: int) -> int:
    seen = []
    for i in range(n):
        val = (i * 37) % 1400
        if val not in seen:
            seen.append(val)
    return len(seen)

if __name__ == '__main__':
    print(count_unique(22000))
";
    let test_src = "\
from work import count_unique
assert count_unique(500) == 500
assert count_unique(1800) == 1400
print('ok')
";
    fs::write(dir.join("work.py"), slow_src).map_err(|e| e.to_string())?;
    fs::write(dir.join("test_work.py"), test_src).map_err(|e| e.to_string())?;
    run_git(&dir, &["add", "."])?;
    run_git(&dir, &["commit", "-m", "initial slow O(N^2) workload"])?;

    let path_str = dir.to_string_lossy().to_string();
    let bench_cmd = format!("{py} work.py");
    let test_cmd = format!("{py} test_work.py");

    if !quiet {
        println!("Running self-contained oa demo in temporary git repo...");
    }

    // 1. Initialize session
    let init = init_session(InitParams {
        path: Some(path_str.clone()),
        bench_cmd,
        test_cmd: Some(test_cmd),
        build_cmd: None,
        profile_cmd: None,
        metric: MetricTarget::Wall,
        max_rss_regression: None,
        max_wall_regression: None,
        output_files: Vec::new(),
        lock_files: Vec::new(),
        auto_lock_tests: true,
        equivalence: EquivalencePolicy::Exact,
        min_speedup: 1.05,
        alpha: 0.05,
        warmup: 1,
        pairs: 6,
        pin_core: None,
        allow_dirty: false,
    })?;

    let top_hotspot = init
        .profile
        .hotspots
        .first()
        .map(|h| format!("{} ({:.1}% self)", h.function, h.self_pct))
        .unwrap_or_else(|| "none".to_string());
    let sha_prefix = init.baseline.output_snapshot.stdout_sha256[..16].to_string();

    if !quiet {
        println!(
            "  [1/4] oa init      : baseline {:.2} ms | top hotspot {} | {} locked file(s) | sha256 {}..",
            init.baseline.timing.median_ms,
            top_hotspot,
            init.locked_files.len(),
            sha_prefix
        );
    }

    // 2. Demonstrate Cheat Guard blocking a tampered test
    fs::write(dir.join("test_work.py"), "print('bypassed')\n").map_err(|e| e.to_string())?;
    let cheat_eval = evaluate_session(EvaluateParams {
        path: Some(path_str.clone()),
        ..Default::default()
    })?;
    let cheat_guard_blocked = cheat_eval.verdict == EvalVerdict::CheatBlocked;
    if !cheat_guard_blocked {
        return Err(format!(
            "expected CheatBlocked in demo, got {:?}",
            cheat_eval.verdict
        ));
    }
    checkpoint_session(CheckpointParams {
        path: Some(path_str.clone()),
        action: CheckpointAction::Revert,
        hypothesis: "demo test tamper".to_string(),
        dead_end: Some("blocked by Cheat Guard".to_string()),
        force: false,
    })?;
    if !quiet {
        println!("  [2/4] Cheat Guard  : blocked edit to locked test_work.py (Verdict: CheatBlocked) -> reverted");
    }

    // 3. Demonstrate Equivalence Gate blocking wrong stdout
    fs::write(
        dir.join("work.py"),
        "def count_unique(n: int) -> int:\n    return 1400 if n >= 1400 else n\n\nif __name__ == '__main__':\n    print(999999)\n",
    )
    .map_err(|e| e.to_string())?;
    let eq_eval = evaluate_session(EvaluateParams {
        path: Some(path_str.clone()),
        ..Default::default()
    })?;
    let equivalence_gate_blocked = eq_eval.verdict == EvalVerdict::EquivalenceFail;
    if !equivalence_gate_blocked {
        return Err(format!(
            "expected EquivalenceFail in demo, got {:?}",
            eq_eval.verdict
        ));
    }
    checkpoint_session(CheckpointParams {
        path: Some(path_str.clone()),
        action: CheckpointAction::Revert,
        hypothesis: "demo broken fast path".to_string(),
        dead_end: Some("diverged stdout SHA-256".to_string()),
        force: false,
    })?;
    if !quiet {
        println!("  [3/4] Equiv Gate   : blocked wrong output despite faster runtime (Verdict: EquivalenceFail) -> reverted");
    }

    // 4. Genuine O(N) set optimization
    let fast_src = "\
def count_unique(n: int) -> int:
    seen = set()
    for i in range(n):
        seen.add((i * 37) % 1400)
    return len(seen)

if __name__ == '__main__':
    print(count_unique(22000))
";
    fs::write(dir.join("work.py"), fast_src).map_err(|e| e.to_string())?;
    let win = evaluate_session(EvaluateParams {
        path: Some(path_str.clone()),
        ..Default::default()
    })?;
    if win.verdict != EvalVerdict::KeepRecommended {
        return Err(format!(
            "expected KeepRecommended in demo, got {:?}: {}",
            win.verdict, win.reason
        ));
    }
    let st = win
        .stats
        .ok_or_else(|| "missing benchmark stats in demo".to_string())?;
    checkpoint_session(CheckpointParams {
        path: Some(path_str),
        action: CheckpointAction::Keep,
        hypothesis: "replace O(N^2) list scan with O(1) set lookup".to_string(),
        dead_end: None,
        force: false,
    })?;

    let elapsed_s = t0.elapsed().as_secs_f64();
    if !quiet {
        println!(
            "  [4/4] Verified win : {:.2} ms -> {:.2} ms ({:.2}x, 95% CI [{:.2}x, {:.2}x], p={:.4}, exact SHA-256)",
            st.baseline.median_ms,
            st.candidate.median_ms,
            st.paired_ratio_median,
            st.ci95_low,
            st.ci95_high,
            st.mann_whitney_p
        );
        println!(
            "Demo finished in {:.1}s and cleaned up temp repo. Next: run `oa setup` or `oa init --bench \"<cmd>\"` in your repo.",
            elapsed_s
        );
    }

    Ok(DemoResult {
        baseline_median_ms: st.baseline.median_ms,
        optimized_median_ms: st.candidate.median_ms,
        speedup: st.paired_ratio_median,
        ci95_low: st.ci95_low,
        ci95_high: st.ci95_high,
        mann_whitney_p: st.mann_whitney_p,
        top_hotspot,
        cheat_guard_blocked,
        equivalence_gate_blocked,
        stdout_sha256: sha_prefix,
        elapsed_s,
    })
}
