use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use oa::guard::EquivalencePolicy;
use oa::session::{
    checkpoint_session, evaluate_session, get_ledger, init_session, profile_session,
    render_markdown_report, CheckpointAction, CheckpointParams, EvalVerdict, EvaluateParams,
    InitParams, ProfileParams,
};

static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

fn python_bin() -> String {
    for bin in ["python3", "python"] {
        if matches!(
            Command::new(bin).arg("--version").output(),
            Ok(out) if out.status.success()
        ) {
            return bin.to_string();
        }
    }
    panic!("python3 or python is required to run integration tests");
}

struct TempRepo {
    path: PathBuf,
}

impl TempRepo {
    fn new(name: &str) -> Self {
        let seq = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("oa_it_{name}_{}_{}", std::process::id(), seq));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp repo dir");
        Self { path }
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.path)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(&self, rel: &str, content: &str) {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(full, content).expect("write file");
    }

    fn path_str(&self) -> String {
        self.path.to_string_lossy().to_string()
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn setup_slow_python_repo(name: &str) -> (TempRepo, String) {
    let py = python_bin();
    let repo = TempRepo::new(name);
    repo.git(&["init"]);
    repo.git(&["config", "user.name", "Test"]);
    repo.git(&["config", "user.email", "test@example.com"]);

    repo.write(
        "work.py",
        r#"
def count_unique(n: int) -> int:
    seen = []
    for i in range(n):
        val = (i * 37) % 1200
        if val not in seen:
            seen.append(val)
    return len(seen)

if __name__ == "__main__":
    import sys
    print(count_unique(18000))
    sys.exit(0)
"#,
    );
    repo.write(
        "test_work.py",
        r#"
from work import count_unique
assert count_unique(500) == 500
assert count_unique(1500) == 1200
print("tests ok")
"#,
    );
    repo.git(&["add", "."]);
    repo.git(&["commit", "-m", "initial slow implementation"]);
    (repo, py)
}

#[test]
fn full_session_guards_and_verified_optimization() {
    let (repo, py) = setup_slow_python_repo("full");
    let bench_cmd = format!("{py} work.py");
    let test_cmd = format!("{py} test_work.py");

    // 1. Initialize session.
    let init = init_session(InitParams {
        path: Some(repo.path_str()),
        bench_cmd,
        test_cmd: Some(test_cmd),
        build_cmd: None,
        profile_cmd: None,
        metric: Default::default(),
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
    })
    .expect("init_session should succeed");

    assert!(init.locked_files.contains(&"test_work.py".to_string()));
    assert_eq!(init.profile.profiler, "python:cProfile+tracemalloc");
    assert!(
        init.profile
            .hotspots
            .iter()
            .any(|h| h.function == "count_unique"),
        "expected count_unique in initial hotspots"
    );

    // 2. Cheat Guard blocks modified test file.
    repo.write("test_work.py", "print('tampered')\n");
    let cheated = evaluate_session(EvaluateParams {
        path: Some(repo.path_str()),
        ..Default::default()
    })
    .expect("evaluate returns CheatBlocked record");
    assert_eq!(cheated.verdict, EvalVerdict::CheatBlocked);
    assert_eq!(cheated.tamper_violations.len(), 1);

    // Restore via revert.
    checkpoint_session(CheckpointParams {
        path: Some(repo.path_str()),
        action: CheckpointAction::Revert,
        hypothesis: "attempted test tamper".to_string(),
        dead_end: Some("blocked by Cheat Guard".to_string()),
        force: false,
    })
    .expect("revert after tamper");

    // 3. Equivalence Gate blocks wrong output even if fast.
    repo.write(
        "work.py",
        r#"
def count_unique(n: int) -> int:
    return 1200 if n >= 1200 else n

if __name__ == "__main__":
    print(999999)
"#,
    );
    let broken = evaluate_session(EvaluateParams {
        path: Some(repo.path_str()),
        ..Default::default()
    })
    .expect("evaluate returns EquivalenceFail record");
    assert_eq!(broken.verdict, EvalVerdict::EquivalenceFail);
    assert!(
        checkpoint_session(CheckpointParams {
            path: Some(repo.path_str()),
            action: CheckpointAction::Keep,
            hypothesis: "should be refused".to_string(),
            dead_end: None,
            force: false,
        })
        .is_err(),
        "keep must be refused on EquivalenceFail"
    );
    checkpoint_session(CheckpointParams {
        path: Some(repo.path_str()),
        action: CheckpointAction::Revert,
        hypothesis: "hardcoded wrong stdout".to_string(),
        dead_end: Some("diverged stdout SHA-256".to_string()),
        force: false,
    })
    .expect("revert broken output");

    // 4. Genuine O(N) optimization passes tests, SHA-256 equivalence, and interleaved A/B stats.
    repo.write(
        "work.py",
        r#"
def count_unique(n: int) -> int:
    seen = set()
    for i in range(n):
        seen.add((i * 37) % 1200)
    return len(seen)

if __name__ == "__main__":
    import sys
    print(count_unique(18000))
    sys.exit(0)
"#,
    );
    let eval = evaluate_session(EvaluateParams {
        path: Some(repo.path_str()),
        ..Default::default()
    })
    .expect("evaluate optimized version");
    assert_eq!(
        eval.verdict,
        EvalVerdict::KeepRecommended,
        "expected KeepRecommended, got {:?}: {}",
        eval.verdict,
        eval.reason
    );
    assert!(eval.equivalence.as_ref().unwrap().passed);
    let st = eval.stats.as_ref().unwrap();
    assert!(st.paired_ratio_median > 1.5);

    // 5. Post-eval edit must fail Cheat Guard fingerprint check on keep.
    let orig_fast = fs::read_to_string(Path::new(&repo.path).join("work.py")).unwrap();
    repo.write("work.py", &format!("{orig_fast}# sneaky post-eval edit\n"));
    let tampered_keep = checkpoint_session(CheckpointParams {
        path: Some(repo.path_str()),
        action: CheckpointAction::Keep,
        hypothesis: "post-eval edit".to_string(),
        dead_end: None,
        force: false,
    });
    assert!(
        tampered_keep.is_err(),
        "post-eval edit must be rejected by fingerprint check"
    );

    // Restore exact evaluated bytes and keep cleanly.
    repo.write("work.py", &orig_fast);
    let kept = checkpoint_session(CheckpointParams {
        path: Some(repo.path_str()),
        action: CheckpointAction::Keep,
        hypothesis: "replace O(N^2) list scan with set".to_string(),
        dead_end: None,
        force: false,
    })
    .expect("keep verified speedup");
    assert_eq!(kept.entry.action, "KEPT");
    assert!(kept.cumulative_speedup_vs_baseline > 1.5);

    // 6. Profile diff vs baseline and PR report generation.
    let prof = profile_session(ProfileParams {
        path: Some(repo.path_str()),
        diff_from: Some("baseline".to_string()),
        ..Default::default()
    })
    .expect("profile with diff_from baseline");
    let diff = prof.diff.expect("profile diff present");
    assert!(diff.wall_delta_pct < -20.0);

    let ledger = get_ledger(Some(&repo.path_str())).expect("get ledger");
    assert_eq!(ledger.kept_count, 1);
    assert_eq!(ledger.reverted_count, 2);
    assert_eq!(ledger.pareto_frontier_ids, vec![3]);

    let md = render_markdown_report(Some(&repo.path_str())).expect("render report");
    assert!(md.contains("# oa Optimization Report"));
    assert!(md.contains("replace O(N^2) list scan with set"));
    assert!(md.contains("oa evaluate --against baseline"));

    // 7. Live session_status, verify_session, SVG reports, and doctor self-check.
    let st = oa::session::session_status(Some(&repo.path_str())).expect("session status");
    assert!(st.cheat_guard_ok);
    assert_eq!(st.kept_count, 1);
    assert_eq!(st.reverted_count, 2);
    assert!(st.cumulative_gain > 1.5);
    assert!(st.diff_stat_vs_baseline.contains("work.py"));

    let ver = oa::session::verify_session(oa::session::VerifyParams {
        path: Some(repo.path_str()),
        pairs: Some(6),
        warmup: Some(1),
        ..Default::default()
    })
    .expect("verify_session should run");
    assert!(ver.passed, "expected verify to pass: {}", ver.badge);
    assert!(ver.badge.starts_with("VERIFIED:"));

    let dark_svg = oa::session::render_svg_report(Some(&repo.path_str()), oa::svg::SvgTheme::Dark)
        .expect("dark svg report");
    let light_svg =
        oa::session::render_svg_report(Some(&repo.path_str()), oa::svg::SvgTheme::Light)
            .expect("light svg report");
    assert!(dark_svg.starts_with("<svg ") && dark_svg.contains("</svg>"));
    assert!(dark_svg.contains("#0d1117") && dark_svg.contains("replace O(N^2)"));
    assert!(light_svg.contains("#ffffff") && light_svg.contains("generated by oa report"));

    let doc = oa::doctor::run_doctor();
    assert!(!doc.checks.is_empty());
    assert!(doc.checks.iter().any(|c| c.name == "git" && c.available));
    assert!(doc
        .checks
        .iter()
        .any(|c| c.name == "metric:rss" && c.available));

    // 8. Export clean patch (`oa diff`) and remove session state (`oa clean --refs`).
    let diff = oa::session::export_diff(Some(&repo.path_str())).expect("export_diff");
    assert_eq!(diff.changed_files, vec!["work.py".to_string()]);
    assert!(diff.patch.contains("diff --git a/work.py b/work.py"));
    assert!(diff.patch.contains("+    seen = set()"));

    let cleaned = oa::session::clean_session(Some(&repo.path_str()), true).expect("clean_session");
    assert!(cleaned.removed_oa_dir);
    assert!(!repo.path.join(".oa").exists());
    assert!(cleaned.deleted_refs.iter().any(|r| r == "refs/oa/best"));
    assert!(
        fs::read_to_string(repo.path.join("work.py"))
            .unwrap()
            .contains("seen = set()"),
        "oa clean must preserve optimized working tree files"
    );
}

#[test]
fn self_contained_demo_succeeds() {
    let res = oa::demo::run_demo(true).expect("oa demo should succeed");
    assert!(res.cheat_guard_blocked);
    assert!(res.equivalence_gate_blocked);
    assert!(res.speedup > 1.5);
    assert_eq!(res.stdout_sha256.len(), 16);
}

#[test]
fn standalone_compare_detects_speedup_and_equivalence() {
    use oa::session::{compare_commands, CompareParams, MetricTarget};
    use oa::stats::BenchVerdict;

    let py = python_bin();
    let res = compare_commands(CompareParams {
        path: None,
        cmd_a: format!("{py} -c \"print(sum(i*i for i in range(1200000)))\""),
        cmd_b: format!("{py} -c \"n=1199999; print(n*(n+1)*(2*n+1)//6)\""),
        env_vars: Vec::new(),
        metric: MetricTarget::Wall,
        pairs: 6,
        warmup: 1,
        min_speedup: 1.05,
        alpha: 0.05,
        pin_core: None,
        check_output: true,
    })
    .expect("standalone compare");
    assert!(res.outputs_identical);
    assert_eq!(res.verdict, BenchVerdict::Faster);
    assert!(res.stats.paired_ratio_median > 1.1);
    let md = oa::session::render_compare_markdown(&res);
    assert!(md.contains("### oa compare") && md.contains("Mann-Whitney U"));

    assert_eq!(res.pair_samples.len(), 6);
    assert_eq!(res.pair_samples[0].order, "AB");
    assert_eq!(res.pair_samples[1].order, "BA");

    let csv = oa::session::render_compare_csv(&res);
    let csv_lines: Vec<&str> = csv.lines().collect();
    assert_eq!(csv_lines.len(), 7);
    assert_eq!(csv_lines[0], "pair,order,a_ms,b_ms,ratio");
    assert!(csv_lines[1].starts_with("1,AB,"));
    assert!(csv_lines[2].starts_with("2,BA,"));

    let dark_svg = oa::svg::render_compare_svg(&res, oa::svg::SvgTheme::Dark);
    let light_svg = oa::svg::render_compare_svg(&res, oa::svg::SvgTheme::Light);
    assert!(dark_svg.starts_with("<svg ") && dark_svg.contains("</svg>"));
    assert!(dark_svg.contains("#0d1117") && dark_svg.contains("oa compare"));
    assert!(light_svg.starts_with("<svg ") && light_svg.contains("#ffffff"));
}

#[test]
fn cli_compare_writes_svg_and_csv_without_polluting_json_stdout() {
    let py = python_bin();
    let dir = TempRepo::new("cli_cmp_exports");
    let svg_file = dir.path.join("compare.svg");
    let csv_file = dir.path.join("compare.csv");

    let out = Command::new(env!("CARGO_BIN_EXE_oa"))
        .args([
            "compare",
            &format!("{py} -c \"print(sum(range(50000)))\""),
            &format!("{py} -c \"print(49999*50000//2)\""),
            "--pairs",
            "4",
            "--warmup",
            "1",
            "--check-output",
            "--svg",
            &svg_file.to_string_lossy(),
            "--theme",
            "light",
            "--csv",
            &csv_file.to_string_lossy(),
            "--json",
        ])
        .output()
        .expect("run oa compare cli");

    assert!(
        out.status.success(),
        "oa compare failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must remain valid JSON with --svg/--csv");
    assert_eq!(parsed["pair_samples"].as_array().map(|a| a.len()), Some(4));

    let svg = fs::read_to_string(&svg_file).expect("read exported svg");
    assert!(svg.starts_with("<svg ") && svg.contains("#ffffff") && svg.contains("</svg>"));

    let csv = fs::read_to_string(&csv_file).expect("read exported csv");
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0], "pair,order,a_ms,b_ms,ratio");
}

#[test]
fn cli_compare_passes_env_vars_and_validates_format() {
    let py = python_bin();
    let check_cmd = format!(
        "{py} -c \"import os, sys; assert os.environ['OA_A']=='1' and os.environ['OA_B']=='x=y'; print('ok')\""
    );
    let out = Command::new(env!("CARGO_BIN_EXE_oa"))
        .args([
            "compare",
            &check_cmd,
            &check_cmd,
            "--env",
            "OA_A=1",
            "--env",
            "OA_B=x=y",
            "--pairs",
            "3",
            "--warmup",
            "0",
            "--check-output",
            "--json",
        ])
        .output()
        .expect("run oa compare with --env");
    assert!(
        out.status.success(),
        "oa compare --env failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("valid json from --env compare");
    assert_eq!(parsed["outputs_identical"], true);

    let bad = Command::new(env!("CARGO_BIN_EXE_oa"))
        .args(["compare", &check_cmd, &check_cmd, "--env", "NO_EQUALS"])
        .output()
        .expect("run oa compare with invalid --env");
    assert!(!bad.status.success());
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("expected KEY=VALUE"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&bad.stderr)
    );
}

#[test]
fn rss_metric_detects_memory_reduction() {
    use oa::session::MetricTarget;

    let py = python_bin();
    let repo = TempRepo::new("rss");
    repo.git(&["init"]);
    repo.git(&["config", "user.name", "Test"]);
    repo.git(&["config", "user.email", "test@example.com"]);

    // Baseline materializes distinct mutable buffers (~55 MiB) just to count them.
    repo.write(
        "mem.py",
        "data = [bytearray(128) for _ in range(300_000)]\nprint(len(data))\n",
    );
    repo.git(&["add", "."]);
    repo.git(&["commit", "-m", "high memory baseline"]);

    let init = init_session(InitParams {
        path: Some(repo.path_str()),
        bench_cmd: format!("{py} mem.py"),
        test_cmd: None,
        build_cmd: None,
        profile_cmd: None,
        metric: MetricTarget::Rss,
        output_files: Vec::new(),
        lock_files: Vec::new(),
        auto_lock_tests: true,
        equivalence: EquivalencePolicy::Exact,
        min_speedup: 1.15,
        alpha: 0.05,
        warmup: 1,
        pairs: 4,
        pin_core: None,
        allow_dirty: false,
    })
    .expect("init rss session");
    assert!(
        init.baseline.peak_rss_kb.unwrap_or(0) > 30_000,
        "expected >30 MiB peak RSS, got {:?}",
        init.baseline.peak_rss_kb
    );

    // Streaming O(1) memory version with identical stdout.
    repo.write("mem.py", "print(sum(1 for _ in range(300_000)))\n");

    let eval = evaluate_session(EvaluateParams {
        path: Some(repo.path_str()),
        metric: Some(MetricTarget::Rss),
        ..Default::default()
    })
    .expect("evaluate rss reduction");
    assert_eq!(eval.metric, MetricTarget::Rss);
    assert_eq!(
        eval.verdict,
        EvalVerdict::KeepRecommended,
        "expected KeepRecommended for RSS reduction, got {:?}: {}",
        eval.verdict,
        eval.reason
    );
    assert!(eval.reference_peak_rss_kb.unwrap() > eval.candidate_peak_rss_kb.unwrap() * 2);
}

#[test]
fn standalone_profile_cmd_works_without_session() {
    use oa::session::{profile_session, ProfileParams};

    let py = python_bin();
    let repo = TempRepo::new("standalone_prof");
    repo.write(
        "hot.py",
        "def slow_sum(n):\n    t = 0\n    for i in range(n):\n        t += (i * 17) % 97\n    return t\nprint(slow_sum(250000))\n",
    );
    let res = profile_session(ProfileParams {
        path: Some(repo.path_str()),
        cmd: Some(format!("{py} hot.py")),
        mode: Some("python".to_string()),
        top_k: Some(5),
        diff_from: None,
        skip_build: false,
    })
    .expect("standalone profile should run without oa init");
    assert!(res.diff.is_none());
    assert!(
        res.report
            .hotspots
            .iter()
            .any(|h| h.function.contains("slow_sum")),
        "expected slow_sum hotspot, got {:?}",
        res.report.hotspots
    );
}

#[test]
fn compare_git_revs_detects_speedup_and_cleans_worktrees() {
    use oa::session::{compare_git_revs, render_compare_markdown, CompareGitParams, MetricTarget};
    use oa::stats::BenchVerdict;

    let py = python_bin();
    let repo = TempRepo::new("git_cmp");
    repo.git(&["init"]);
    repo.git(&["config", "user.name", "Test"]);
    repo.git(&["config", "user.email", "test@example.com"]);

    repo.write(
        "algo.py",
        "def compute(n):\n    return sum(i * i for i in range(n))\n",
    );
    repo.git(&["add", "."]);
    repo.git(&["commit", "-m", "slow baseline"]);

    // Untracked local benchmark script copied into detached worktree via --copy.
    repo.write(
        "local_bench.py",
        "import algo\nprint(algo.compute(1200000))\n",
    );

    // Edit working tree to O(1) closed form (uncommitted).
    repo.write(
        "algo.py",
        "def compute(n):\n    m = n - 1\n    return m * (m + 1) * (2 * m + 1) // 6\n",
    );

    let res = compare_git_revs(CompareGitParams {
        path: Some(repo.path_str()),
        git_a: "HEAD".to_string(),
        git_b: None,
        bench_cmd: format!("{py} local_bench.py"),
        build_cmd: None,
        copy_files: vec!["local_bench.py".to_string()],
        env_vars: Vec::new(),
        metric: MetricTarget::Wall,
        pairs: 6,
        warmup: 1,
        min_speedup: 1.05,
        alpha: 0.05,
        pin_core: None,
        check_output: true,
    })
    .expect("compare_git_revs should succeed");

    assert!(res.outputs_identical);
    assert_eq!(res.verdict, BenchVerdict::Faster);
    assert!(res.stats.paired_ratio_median > 1.1);
    assert_eq!(res.cmd_b, "working tree");
    let md = render_compare_markdown(&res);
    assert!(md.contains("Command:") && md.contains("working tree"));

    // Verify no leaked temporary worktrees remain registered.
    let wt_list = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&repo.path)
        .output()
        .expect("git worktree list");
    let wt_text = String::from_utf8_lossy(&wt_list.stdout);
    let wt_count = wt_text
        .lines()
        .filter(|l| l.starts_with("worktree "))
        .count();
    assert_eq!(wt_count, 1, "expected only main worktree, got:\n{wt_text}");
}
