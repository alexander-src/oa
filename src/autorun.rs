use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::discover::discover_repo;
use crate::git;
use crate::guard::EquivalencePolicy;
use crate::profiler::ProfileReport;
use crate::runner;
use crate::session::{
    checkpoint_session, evaluate_session, export_diff, get_ledger, init_session, profile_session,
    render_markdown_report, render_svg_report, CheckpointAction, CheckpointParams, EvalVerdict,
    EvaluateParams, InitParams, MetricTarget, ProfileParams,
};
use crate::svg::SvgTheme;

/// Parameters for `oa run` autonomous profile -> edit -> evaluate -> checkpoint loop.
#[derive(Debug, Clone)]
pub struct AutoRunParams {
    pub path: Option<String>,
    pub agent: String,
    pub agent_cmd: Option<String>,
    pub bench_cmd: Option<String>,
    pub test_cmd: Option<String>,
    pub build_cmd: Option<String>,
    pub profile_cmd: Option<String>,
    pub metric: MetricTarget,
    pub max_rss_regression: Option<f64>,
    pub max_wall_regression: Option<f64>,
    pub max_iters: usize,
    pub max_stale: usize,
    pub pairs: usize,
    pub warmup: usize,
    pub min_speedup: f64,
    pub alpha: f64,
    pub pin_core: Option<usize>,
    pub allow_dirty: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoRunIteration {
    pub iteration: usize,
    pub prompt_file: String,
    pub hypothesis: String,
    pub agent_exit_code: Option<i32>,
    pub verdict: String,
    pub action: String,
    pub reason: String,
    pub speedup_vs_prev: Option<f64>,
    pub speedup_vs_baseline: Option<f64>,
    pub candidate_peak_rss_kb: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoRunReport {
    pub repo_root: String,
    pub agent: String,
    pub metric: MetricTarget,
    pub bench_cmd: String,
    pub test_cmd: Option<String>,
    pub build_cmd: Option<String>,
    pub dry_run: bool,
    pub stopped_reason: String,
    pub iterations: Vec<AutoRunIteration>,
    pub kept_count: usize,
    pub reverted_count: usize,
    pub cumulative_gain: f64,
    pub artifacts_dir: Option<String>,
}

fn build_agent_command(
    agent: &str,
    custom_cmd: Option<&str>,
    prompt_file: &Path,
) -> Result<String, String> {
    if let Some(cmd) = custom_cmd {
        return Ok(cmd.to_string());
    }
    let p = prompt_file.to_string_lossy();
    match agent.to_ascii_lowercase().as_str() {
        "claude" => Ok(format!("claude -p \"$(cat '{p}')\"")),
        "codex" => Ok(format!("codex exec \"$(cat '{p}')\"")),
        "gemini" => Ok(format!("gemini -p \"$(cat '{p}')\"")),
        "opencode" => Ok(format!("opencode run \"$(cat '{p}')\"")),
        "cmd" => Err("agent 'cmd' requires --agent-cmd <COMMAND>".to_string()),
        other => Err(format!(
            "unknown --agent '{other}': expected claude, codex, gemini, opencode, or cmd (with --agent-cmd)"
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_iteration_prompt(
    iter: usize,
    metric: MetricTarget,
    max_rss_reg: Option<f64>,
    max_wall_reg: Option<f64>,
    bench_cmd: &str,
    test_cmd: Option<&str>,
    profile: &ProfileReport,
    dead_ends: &[String],
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# oa Autonomous Optimization Iteration #{iter}\n\n"
    ));
    out.push_str(&format!("Objective metric: {:?}\n", metric));
    if let Some(r) = max_rss_reg {
        out.push_str(&format!(
            "Peak RSS guardrail: candidate must not exceed {:.2}x reference RSS.\n",
            r
        ));
    }
    if let Some(w) = max_wall_reg {
        out.push_str(&format!(
            "Wall time guardrail: candidate must not exceed {:.2}x reference wall time.\n",
            w
        ));
    }
    out.push_str(&format!("Benchmark command: `{bench_cmd}`\n"));
    if let Some(tc) = test_cmd {
        out.push_str(&format!("Test command: `{tc}`\n"));
    }
    out.push_str("\n## Rules\n");
    out.push_str("- Never modify locked test or benchmark files (Cheat Guard reverts and rejects tampered runs).\n");
    out.push_str("- Preserve exact stdout/file behavior (SHA-256 equivalence gate).\n");
    out.push_str("- Make ONE focused, structural change targeting the top CPU hotspot, allocation site, or struct-padding waste below.\n");
    out.push_str(
        "- Print a final line formatted exactly as `HYPOTHESIS: <short description>`.\n\n",
    );

    out.push_str(&format!(
        "## Current Profile ({}, class: {})\n",
        profile.profiler, profile.bottleneck_class
    ));
    out.push_str(&format!("Wall: {:.2} ms", profile.wall_ms));
    if let Some(rss) = profile.peak_rss_kb {
        out.push_str(&format!(" | Peak RSS: {rss} KiB"));
    }
    if let Some(alloc) = profile.peak_alloc_kb {
        out.push_str(&format!(" | Peak traced allocations: {alloc:.1} KiB"));
    }
    out.push('\n');
    out.push_str(&format!("Diagnosis: {}\n\n", profile.diagnosis));

    if let Some(ref c) = profile.counters {
        out.push_str(&format!(
            "Hardware counters: IPC {:?}, branch-miss {:?}%, cache-miss {:?}%\n\n",
            c.ipc, c.branch_miss_pct, c.cache_miss_pct
        ));
    }

    if !profile.hotspots.is_empty() {
        out.push_str("### Top CPU Hotspots\n");
        for h in profile.hotspots.iter().take(8) {
            out.push_str(&format!(
                "- {:.1}% self ({:.2} ms self, {:.2} ms cum, {} calls): `{}` at `{}:{}`\n",
                h.self_pct, h.self_ms, h.cum_ms, h.calls, h.function, h.file, h.line
            ));
            if let Some(ref snip) = h.snippet {
                out.push_str("```\n");
                out.push_str(snip);
                out.push_str("```\n");
            }
        }
        out.push('\n');
    }

    if !profile.alloc_sites.is_empty() {
        out.push_str("### Top Allocation / RSS-Growth Sites\n");
        for a in profile.alloc_sites.iter().take(8) {
            let fn_part = a
                .function
                .as_deref()
                .map(|f| format!("`{f}` @ "))
                .unwrap_or_default();
            out.push_str(&format!(
                "- {:.1} KiB ({} samples): {}`{}:{}`\n",
                a.size_kb, a.count, fn_part, a.file, a.line
            ));
        }
        out.push('\n');
    }

    if !profile.struct_padding.is_empty() {
        out.push_str("### Packable Structs (pahole -P)\n");
        for s in &profile.struct_padding {
            out.push_str(&format!(
                "- `{}`: {}B -> {}B (save {}B, {:.1}%)\n",
                s.name, s.current_bytes, s.packed_bytes, s.saved_bytes, s.savings_pct
            ));
        }
        out.push('\n');
    }

    if !dead_ends.is_empty() {
        out.push_str("### Ruled-Out Dead Ends (DO NOT RETRY)\n");
        for d in dead_ends {
            out.push_str(&format!("- {d}\n"));
        }
        out.push('\n');
    }

    out
}

fn extract_hypothesis(stdout: &[u8], iter: usize) -> String {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("HYPOTHESIS:")
            .or_else(|| trimmed.strip_prefix("hypothesis:"))
        {
            let h = rest.trim();
            if !h.is_empty() {
                return h.chars().take(140).collect();
            }
        }
    }
    format!("autonomous optimization iteration #{iter}")
}

/// Runs the autonomous `oa run` loop: discover -> init -> profile -> agent edit -> evaluate -> checkpoint/revert -> export.
pub fn run_autonomous_loop(params: AutoRunParams) -> Result<AutoRunReport, String> {
    let start = match params.path.as_deref() {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().map_err(|e| format!("failed to get cwd: {e}"))?,
    };
    let root = git::repo_root(&start)?;
    let root_str = root.to_string_lossy().to_string();

    // Auto-discover commands if --bench was omitted.
    let (bench_cmd, test_cmd, build_cmd) = if let Some(b) = params.bench_cmd.clone() {
        (b, params.test_cmd.clone(), params.build_cmd.clone())
    } else {
        let disc = discover_repo(Some(&root_str))?;
        let candidate = disc.bench_candidates.first().ok_or_else(|| {
            format!(
                "oa discover found no benchmark candidate in {}; pass --bench <CMD> explicitly",
                disc.repo_root
            )
        })?;
        (
            candidate.cmd.clone(),
            params.test_cmd.clone().or(disc.suggested_test_cmd),
            params.build_cmd.clone().or(disc.suggested_build_cmd),
        )
    };

    let oa_config = root.join(".oa").join("config.json");
    if !oa_config.exists() {
        init_session(InitParams {
            path: Some(root_str.clone()),
            bench_cmd: bench_cmd.clone(),
            test_cmd: test_cmd.clone(),
            build_cmd: build_cmd.clone(),
            profile_cmd: params.profile_cmd.clone(),
            metric: params.metric,
            max_rss_regression: params.max_rss_regression,
            max_wall_regression: params.max_wall_regression,
            output_files: Vec::new(),
            lock_files: Vec::new(),
            auto_lock_tests: true,
            equivalence: EquivalencePolicy::Exact,
            min_speedup: params.min_speedup,
            alpha: params.alpha,
            warmup: params.warmup,
            pairs: params.pairs,
            pin_core: params.pin_core,
            allow_dirty: params.allow_dirty,
        })?;
    }

    let max_iters = params.max_iters.max(1);
    let max_stale = params.max_stale.max(1);
    let mut iterations = Vec::new();
    let mut stale_count = 0usize;
    let mut stopped_reason = format!("reached max_iters ({max_iters})");

    for iter in 1..=max_iters {
        let diff_from = if iter > 1 {
            Some("best".to_string())
        } else {
            None
        };
        let prof = profile_session(ProfileParams {
            path: Some(root_str.clone()),
            cmd: None,
            mode: None,
            top_k: Some(10),
            diff_from,
            skip_build: false,
        })?;
        let ledger = get_ledger(Some(&root_str))?;

        let prompt_text = render_iteration_prompt(
            iter,
            params.metric,
            params.max_rss_regression,
            params.max_wall_regression,
            &bench_cmd,
            test_cmd.as_deref(),
            &prof.report,
            &ledger.dead_ends,
        );
        let prompt_file = root.join(".oa").join(format!("iter_{iter}_prompt.md"));
        fs::write(&prompt_file, &prompt_text)
            .map_err(|e| format!("failed to write iteration prompt: {e}"))?;

        let agent_shell =
            build_agent_command(&params.agent, params.agent_cmd.as_deref(), &prompt_file)?;

        if params.dry_run {
            iterations.push(AutoRunIteration {
                iteration: iter,
                prompt_file: prompt_file.to_string_lossy().to_string(),
                hypothesis: format!("dry-run: would invoke `{agent_shell}`"),
                agent_exit_code: None,
                verdict: "DRY_RUN".to_string(),
                action: "SKIPPED".to_string(),
                reason: "dry_run requested; prompt dossier written".to_string(),
                speedup_vs_prev: None,
                speedup_vs_baseline: None,
                candidate_peak_rss_kb: prof.report.peak_rss_kb,
            });
            stopped_reason = "dry_run completed".to_string();
            break;
        }

        let env_vars = vec![
            (
                "OA_PROMPT_FILE".to_string(),
                prompt_file.to_string_lossy().to_string(),
            ),
            ("OA_ITER".to_string(), iter.to_string()),
            ("OA_REPO_ROOT".to_string(), root_str.clone()),
        ];
        let agent_out = runner::run_command_with_env(&agent_shell, &root, None, false, &env_vars)?;
        let hypothesis = extract_hypothesis(&agent_out.stdout, iter);

        let eval = evaluate_session(EvaluateParams {
            path: Some(root_str.clone()),
            compare_against: Some("best".to_string()),
            metric: Some(params.metric),
            max_rss_regression: params.max_rss_regression,
            max_wall_regression: params.max_wall_regression,
            pairs: Some(params.pairs),
            warmup: Some(params.warmup),
            min_speedup: Some(params.min_speedup),
            alpha: Some(params.alpha),
        })?;

        let (action_label, dead_end_opt) = if eval.verdict == EvalVerdict::KeepRecommended {
            let _ = checkpoint_session(CheckpointParams {
                path: Some(root_str.clone()),
                action: CheckpointAction::Keep,
                hypothesis: hypothesis.clone(),
                dead_end: None,
                force: false,
            })?;
            stale_count = 0;
            ("KEPT".to_string(), None)
        } else {
            let dead_end = format!("{:?}: {}", eval.verdict, eval.reason);
            let _ = checkpoint_session(CheckpointParams {
                path: Some(root_str.clone()),
                action: CheckpointAction::Revert,
                hypothesis: hypothesis.clone(),
                dead_end: Some(dead_end.clone()),
                force: false,
            })?;
            stale_count += 1;
            ("REVERTED".to_string(), Some(dead_end))
        };

        iterations.push(AutoRunIteration {
            iteration: iter,
            prompt_file: prompt_file.to_string_lossy().to_string(),
            hypothesis,
            agent_exit_code: Some(agent_out.exit_code),
            verdict: format!("{:?}", eval.verdict),
            action: action_label,
            reason: dead_end_opt.unwrap_or(eval.reason),
            speedup_vs_prev: eval.stats.as_ref().map(|s| s.paired_ratio_median),
            speedup_vs_baseline: eval.speedup_vs_baseline,
            candidate_peak_rss_kb: eval.candidate_peak_rss_kb,
        });

        if stale_count >= max_stale {
            stopped_reason =
                format!("plateau reached ({stale_count} consecutive non-kept iterations)");
            break;
        }
    }

    let final_ledger = get_ledger(Some(&root_str))?;
    let artifacts_dir = if !params.dry_run {
        let dir = root.join(".oa").join("artifacts");
        fs::create_dir_all(&dir).map_err(|e| format!("failed to create artifacts dir: {e}"))?;
        if let Ok(md) = render_markdown_report(Some(&root_str)) {
            let _ = fs::write(dir.join("REPORT.md"), md);
        }
        if let Ok(svg) = render_svg_report(Some(&root_str), SvgTheme::Dark) {
            let _ = fs::write(dir.join("bench.svg"), svg);
        }
        if let Ok(diff) = export_diff(Some(&root_str)) {
            let _ = fs::write(dir.join("optimization.patch"), diff.patch);
        }
        Some(dir.to_string_lossy().to_string())
    } else {
        None
    };

    Ok(AutoRunReport {
        repo_root: root_str,
        agent: params.agent,
        metric: params.metric,
        bench_cmd,
        test_cmd,
        build_cmd,
        dry_run: params.dry_run,
        stopped_reason,
        iterations,
        kept_count: final_ledger.kept_count,
        reverted_count: final_ledger.reverted_count,
        cumulative_gain: final_ledger.cumulative_speedup,
        artifacts_dir,
    })
}
