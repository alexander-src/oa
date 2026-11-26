#![forbid(unsafe_code)]

use std::env;
use std::fs;
use std::process::ExitCode;

use oa::demo;
use oa::doctor;
use oa::guard::EquivalencePolicy;
use oa::mcp;
use oa::session::{
    checkpoint_session, compare_commands, evaluate_session, get_ledger, init_session,
    profile_session, render_compare_markdown, render_markdown_report, render_svg_report,
    session_status, verify_session, CheckpointAction, CheckpointParams, CompareParams, EvalVerdict,
    EvaluateParams, InitParams, MetricTarget, ProfileParams, VerifyParams,
};
use oa::setup;
use oa::svg::SvgTheme;

const HELP: &str = "\
oa - Optimize Anything: cross-language profiling, equivalence-gated benchmarking, and MCP harness

USAGE:
  oa <COMMAND> [OPTIONS]

COMMANDS:
  init        Initialize an optimization session (locks tests, snapshots baseline, runs initial profile)
  profile     Profile current code, classify bottleneck, and optionally diff hotspots
  evaluate    Run Cheat Guard, tests, equivalence check, and interleaved A/B benchmark (alias: bench)
  checkpoint  Keep a verified optimization or revert to best and log a dead end
  verify      Fresh end-to-end verification of HEAD vs baseline (CI / PR gate, non-zero exit on fail)
  status      Inspect live session health, Cheat Guard locks, cumulative gain, and diff stat
  ledger      Inspect the experiment ledger, Pareto frontier, and dead ends
  report      Generate a PR-ready Markdown, JSON, or dark/light SVG chart report
  compare     Standalone interleaved A/B comparison of any two commands (no session required)
  demo        Run a 4-second self-contained live demo (Cheat Guard, Equivalence Gate, verified speedup)
  doctor      Inspect local profilers (Python, Node, Go, perf, sample), RSS, and agent configs
  setup       Register the oa MCP server and SKILL.md for Claude Code, Antigravity, Cursor, Codex, Gemini CLI
  mcp         Run the stdio JSON-RPC 2.0 MCP server

Run 'oa <COMMAND> --help' for command-specific options.
";

fn print_subcommand_help(cmd: &str) {
    match cmd {
        "init" => println!(
            "USAGE:\n  oa init --bench <CMD> [OPTIONS]\n\nOPTIONS:\n  --bench <CMD>          Benchmark command to optimize (required)\n  --test <CMD>           Test suite command required before keeping changes\n  --build <CMD>          Build command run before profiling/evaluating\n  --profile-cmd <CMD>    Optional command for profiling (defaults to --bench)\n  --metric <wall|rss>    Primary metric: wall (latency ms, default) or rss (peak memory KiB)\n  --output-file <PATH>   Output file path whose SHA-256 must match baseline (repeatable)\n  --lock <PATH>          Extra file locked by Cheat Guard (repeatable)\n  --no-auto-lock         Disable automatic locking of tracked test/bench files\n  --ignore-line <SUBSTR> Ignore lines containing substring during equivalence check (repeatable)\n  --oracle <CMD>         External oracle command used instead of SHA-256 equivalence\n  --min-speedup <RATIO>  Minimum 95% CI lower bound to accept (default 1.03)\n  --alpha <P>            Mann-Whitney U alpha threshold (default 0.01)\n  --warmup <N>           Warmup runs/pairs (default 2)\n  --pairs <N>            Interleaved A/B pairs (default 10)\n  --pin-core <ID>        Pin benchmark runs to Linux CPU core via taskset\n  --allow-dirty          Allow starting on a dirty working tree\n  --path <DIR>           Target repository directory\n  --json                 Emit raw JSON output"
        ),
        "profile" => println!(
            "USAGE:\n  oa profile [OPTIONS]\n\nOPTIONS:\n  --mode <auto|python|node|go|perf|sample|native>  Profiler backend (default auto)\n  --top <N>                                        Max hotspots (default 10)\n  --diff-from <baseline|best>                      Compare hotspot self-time against baseline or best\n  --skip-build                                     Skip build_cmd before profiling\n  --path <DIR>                                     Target repository directory\n  --json                                           Emit raw JSON output"
        ),
        "evaluate" | "bench" => println!(
            "USAGE:\n  oa evaluate [OPTIONS]\n\nOPTIONS:\n  --against <best|baseline>  Reference worktree to interleave against (default best)\n  --metric <wall|rss>        Override metric (wall latency ms or rss peak KiB)\n  --pairs <N>                Override interleaved A/B pairs\n  --warmup <N>               Override warmup pairs\n  --min-speedup <RATIO>      Override min speedup threshold\n  --alpha <P>                Override Mann-Whitney alpha\n  --path <DIR>               Target repository directory\n  --json                     Emit raw JSON output"
        ),
        "checkpoint" => println!(
            "USAGE:\n  oa checkpoint (--keep | --revert) --hypothesis <TEXT> [OPTIONS]\n\nOPTIONS:\n  --keep                 Keep verified working tree as refs/oa/best\n  --revert               Revert working tree to refs/oa/best and record dead end\n  --hypothesis <TEXT>    Short description of the change (required)\n  --dead-end <TEXT>      Reason a reverted experiment failed\n  --force                Override a non-KEEP_RECOMMENDED verdict (never overrides Cheat Guard)\n  --path <DIR>           Target repository directory\n  --json                 Emit raw JSON output"
        ),
        "ledger" => println!(
            "USAGE:\n  oa ledger [--path <DIR>] [--json]"
        ),
        "report" => println!(
            "USAGE:\n  oa report [--format md|json|svg] [--theme dark|light] [--out <FILE>] [--path <DIR>]"
        ),
        "verify" => println!(
            "USAGE:\n  oa verify [OPTIONS]\n\nOPTIONS:\n  --metric <wall|rss>    Override metric (wall latency ms or rss peak KiB)\n  --pairs <N>            Interleaved A/B pairs vs baseline (default 15)\n  --warmup <N>           Warmup pairs\n  --min-speedup <RATIO>  Override minimum 95% CI lower bound\n  --alpha <P>            Override Mann-Whitney alpha\n  --path <DIR>           Target repository directory\n  --json                 Emit raw JSON output"
        ),
        "status" => println!(
            "USAGE:\n  oa status [--path <DIR>] [--json]"
        ),
        "compare" => println!(
            "USAGE:\n  oa compare <CMD_A> <CMD_B> [OPTIONS]\n\nOPTIONS:\n  --metric <wall|rss>    Metric to compare: wall (ms, default) or rss (peak KiB)\n  --pairs <N>            Interleaved A/B pairs (default 10)\n  --warmup <N>           Warmup pairs (default 2)\n  --min-speedup <RATIO>  Minimum 95% CI lower bound (default 1.03)\n  --alpha <P>            Mann-Whitney U alpha (default 0.01)\n  --pin-core <ID>        Pin runs to Linux CPU core via taskset\n  --check-output         Fail if stdout SHA-256 differs between CMD_A and CMD_B\n  --markdown             Emit a paste-ready GitHub Markdown comparison table\n  --path <DIR>           Working directory\n  --json                 Emit raw JSON output"
        ),
        "demo" => println!(
            "USAGE:\n  oa demo [--json]\n\nRuns a self-contained 4-second demo in a temporary git repository and cleans up on exit."
        ),
        "doctor" => println!(
            "USAGE:\n  oa doctor [--json]"
        ),
        "setup" => println!(
            "USAGE:\n  oa setup [--agent all|claude|antigravity|cursor|gemini|codex] [--dry-run] [--json]"
        ),
        "mcp" => println!(
            "USAGE:\n  oa mcp\n\nRuns the stdio JSON-RPC 2.0 MCP server (exposes oa_init, oa_profile, oa_evaluate, oa_checkpoint, oa_verify, oa_status, oa_ledger, oa_report, oa_compare)."
        ),
        _ => print!("{HELP}"),
    }
}

fn next_val(args: &[String], idx: &mut usize, flag: &str) -> Result<String, String> {
    *idx += 1;
    args.get(*idx)
        .cloned()
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn run_cli() -> Result<ExitCode, String> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print!("{HELP}");
        return Ok(ExitCode::SUCCESS);
    }

    let cmd = args[1].as_str();
    match cmd {
        "-h" | "--help" | "help" => {
            print!("{HELP}");
            return Ok(ExitCode::SUCCESS);
        }
        "-V" | "--version" => {
            println!("oa {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }

    let rest = &args[2..];
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        print_subcommand_help(cmd);
        return Ok(ExitCode::SUCCESS);
    }

    match cmd {
        "init" => {
            let mut bench_cmd = None;
            let mut test_cmd = None;
            let mut build_cmd = None;
            let mut profile_cmd = None;
            let mut metric = MetricTarget::Wall;
            let mut output_files = Vec::new();
            let mut lock_files = Vec::new();
            let mut auto_lock_tests = true;
            let mut ignore_lines = Vec::new();
            let mut oracle_cmd = None;
            let mut min_speedup = 1.03;
            let mut alpha = 0.01;
            let mut warmup = 2usize;
            let mut pairs = 10usize;
            let mut pin_core = None;
            let mut allow_dirty = false;
            let mut path = None;
            let mut as_json = false;

            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--bench" => bench_cmd = Some(next_val(rest, &mut i, "--bench")?),
                    "--test" => test_cmd = Some(next_val(rest, &mut i, "--test")?),
                    "--build" => build_cmd = Some(next_val(rest, &mut i, "--build")?),
                    "--profile-cmd" => profile_cmd = Some(next_val(rest, &mut i, "--profile-cmd")?),
                    "--metric" => {
                        let v = next_val(rest, &mut i, "--metric")?;
                        metric = MetricTarget::parse_str(&v)?;
                    }
                    "--output-file" => output_files.push(next_val(rest, &mut i, "--output-file")?),
                    "--lock" => lock_files.push(next_val(rest, &mut i, "--lock")?),
                    "--no-auto-lock" => auto_lock_tests = false,
                    "--ignore-line" => ignore_lines.push(next_val(rest, &mut i, "--ignore-line")?),
                    "--oracle" => oracle_cmd = Some(next_val(rest, &mut i, "--oracle")?),
                    "--min-speedup" => {
                        let v = next_val(rest, &mut i, "--min-speedup")?;
                        min_speedup = v
                            .parse()
                            .map_err(|_| format!("invalid --min-speedup value '{v}'"))?;
                    }
                    "--alpha" => {
                        let v = next_val(rest, &mut i, "--alpha")?;
                        alpha = v
                            .parse()
                            .map_err(|_| format!("invalid --alpha value '{v}'"))?;
                    }
                    "--warmup" => {
                        let v = next_val(rest, &mut i, "--warmup")?;
                        warmup = v
                            .parse()
                            .map_err(|_| format!("invalid --warmup value '{v}'"))?;
                    }
                    "--pairs" => {
                        let v = next_val(rest, &mut i, "--pairs")?;
                        pairs = v
                            .parse()
                            .map_err(|_| format!("invalid --pairs value '{v}'"))?;
                    }
                    "--pin-core" => {
                        let v = next_val(rest, &mut i, "--pin-core")?;
                        pin_core = Some(
                            v.parse()
                                .map_err(|_| format!("invalid --pin-core value '{v}'"))?,
                        );
                    }
                    "--allow-dirty" => allow_dirty = true,
                    "--path" => path = Some(next_val(rest, &mut i, "--path")?),
                    "--json" => as_json = true,
                    other => return Err(format!("unknown option '{other}' for init")),
                }
                i += 1;
            }

            let bench_cmd = bench_cmd.ok_or_else(|| "init requires --bench <CMD>".to_string())?;
            let equivalence = if let Some(command) = oracle_cmd {
                EquivalencePolicy::Oracle { command }
            } else if !ignore_lines.is_empty() {
                EquivalencePolicy::Normalized {
                    ignore_substrings: ignore_lines,
                }
            } else {
                EquivalencePolicy::Exact
            };

            let report = init_session(InitParams {
                path,
                bench_cmd,
                test_cmd,
                build_cmd,
                profile_cmd,
                metric,
                output_files,
                lock_files,
                auto_lock_tests,
                equivalence,
                min_speedup,
                alpha,
                warmup,
                pairs,
                pin_core,
                allow_dirty,
            })?;

            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
            } else {
                println!("Initialized oa session in {}", report.repo_root);
                println!(
                    "  Baseline commit : {}",
                    &report.baseline_commit[..10.min(report.baseline_commit.len())]
                );
                println!("  Locked files    : {} file(s)", report.locked_files.len());
                println!(
                    "  Baseline timing : {:.2} ms median (p95 {:.2} ms, CV {:.1}%)",
                    report.baseline.timing.median_ms,
                    report.baseline.timing.p95_ms,
                    report.baseline.timing.cv_pct
                );
                if let Some(rss) = report.baseline.peak_rss_kb {
                    println!("  Peak RSS        : {rss} KiB");
                }
                println!(
                    "  Output SHA-256  : {}.. ({} bytes)",
                    &report.baseline.output_snapshot.stdout_sha256[..16],
                    report.baseline.output_snapshot.stdout_bytes
                );
                println!(
                    "  Profiler        : {} [{}]",
                    report.profile.profiler, report.profile.bottleneck_class
                );
                println!("  Diagnosis       : {}", report.profile.diagnosis);
                if !report.profile.hotspots.is_empty() {
                    println!("  Top hotspots:");
                    for h in report.profile.hotspots.iter().take(5) {
                        println!(
                            "    - {:>5.1}% self ({:>7.2} ms, {:>6} calls)  {}  ({}:{})",
                            h.self_pct, h.self_ms, h.calls, h.function, h.file, h.line
                        );
                    }
                }
                for w in &report.warnings {
                    println!("  Warning: {w}");
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        "profile" => {
            let mut mode = None;
            let mut top_k = None;
            let mut diff_from = None;
            let mut skip_build = false;
            let mut path = None;
            let mut as_json = false;

            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--mode" => mode = Some(next_val(rest, &mut i, "--mode")?),
                    "--top" => {
                        let v = next_val(rest, &mut i, "--top")?;
                        top_k = Some(
                            v.parse()
                                .map_err(|_| format!("invalid --top value '{v}'"))?,
                        );
                    }
                    "--diff-from" => diff_from = Some(next_val(rest, &mut i, "--diff-from")?),
                    "--skip-build" => skip_build = true,
                    "--path" => path = Some(next_val(rest, &mut i, "--path")?),
                    "--json" => as_json = true,
                    other => return Err(format!("unknown option '{other}' for profile")),
                }
                i += 1;
            }

            let res = profile_session(ProfileParams {
                path,
                mode,
                top_k,
                diff_from,
                skip_build,
            })?;
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&res).map_err(|e| e.to_string())?
                );
            } else {
                let p = &res.report;
                println!(
                    "Profile [{}] wall {:.2} ms | class: {}",
                    p.profiler, p.wall_ms, p.bottleneck_class
                );
                println!("Diagnosis: {}", p.diagnosis);
                if let Some(ref c) = p.counters {
                    println!(
                        "HW Cntrs : IPC {:?} | branch-miss {:?}% | cache-miss {:?}%",
                        c.ipc.map(|v| format!("{v:.2}")),
                        c.branch_miss_pct.map(|v| format!("{v:.2}")),
                        c.cache_miss_pct.map(|v| format!("{v:.2}"))
                    );
                }
                if let Some(alloc) = p.peak_alloc_kb {
                    println!("Peak traced allocations: {:.1} KiB", alloc);
                }
                if !p.hotspots.is_empty() {
                    println!("Hotspots:");
                    for h in &p.hotspots {
                        println!(
                            "  {:>5.1}% self | {:>7.2} ms self | {:>7.2} ms cum | {:>7} calls | {} ({}:{})",
                            h.self_pct, h.self_ms, h.cum_ms, h.calls, h.function, h.file, h.line
                        );
                    }
                }
                if let Some(ref d) = res.diff {
                    println!(
                        "Diff vs prior: {:.2} ms -> {:.2} ms ({:+.1}%)",
                        d.before_wall_ms, d.after_wall_ms, d.wall_delta_pct
                    );
                    for c in d.changes.iter().take(6) {
                        println!(
                            "  {:>+8.2} ms ({:>+6.1}%)  {} ({})",
                            c.delta_ms, c.delta_pct, c.function, c.file
                        );
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        "evaluate" | "bench" => {
            let mut compare_against = None;
            let mut metric = None;
            let mut pairs = None;
            let mut warmup = None;
            let mut min_speedup = None;
            let mut alpha = None;
            let mut path = None;
            let mut as_json = false;

            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--against" => compare_against = Some(next_val(rest, &mut i, "--against")?),
                    "--metric" => {
                        let v = next_val(rest, &mut i, "--metric")?;
                        metric = Some(MetricTarget::parse_str(&v)?);
                    }
                    "--pairs" => {
                        let v = next_val(rest, &mut i, "--pairs")?;
                        pairs = Some(
                            v.parse()
                                .map_err(|_| format!("invalid --pairs value '{v}'"))?,
                        );
                    }
                    "--warmup" => {
                        let v = next_val(rest, &mut i, "--warmup")?;
                        warmup = Some(
                            v.parse()
                                .map_err(|_| format!("invalid --warmup value '{v}'"))?,
                        );
                    }
                    "--min-speedup" => {
                        let v = next_val(rest, &mut i, "--min-speedup")?;
                        min_speedup = Some(
                            v.parse()
                                .map_err(|_| format!("invalid --min-speedup value '{v}'"))?,
                        );
                    }
                    "--alpha" => {
                        let v = next_val(rest, &mut i, "--alpha")?;
                        alpha = Some(
                            v.parse()
                                .map_err(|_| format!("invalid --alpha value '{v}'"))?,
                        );
                    }
                    "--path" => path = Some(next_val(rest, &mut i, "--path")?),
                    "--json" => as_json = true,
                    other => return Err(format!("unknown option '{other}' for evaluate")),
                }
                i += 1;
            }

            let rec = evaluate_session(EvaluateParams {
                path,
                compare_against,
                metric,
                pairs,
                warmup,
                min_speedup,
                alpha,
            })?;

            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&rec).map_err(|e| e.to_string())?
                );
            } else {
                let unit = rec.metric.unit();
                println!(
                    "Verdict: {:?} (vs {}, metric {:?})",
                    rec.verdict, rec.compare_against, rec.metric
                );
                println!("Reason : {}", rec.reason);
                if let Some(ref eq) = rec.equivalence {
                    println!("Equiv  : {}", eq.details);
                }
                if let Some(ref st) = rec.stats {
                    println!(
                        "Ref    : {:.2} {unit} median (p95 {:.2} {unit}, MAD {:.2} {unit}, n={})",
                        st.baseline.median_ms,
                        st.baseline.p95_ms,
                        st.baseline.mad_ms,
                        st.baseline.count
                    );
                    println!(
                        "Cand   : {:.2} {unit} median (p95 {:.2} {unit}, MAD {:.2} {unit}, n={})",
                        st.candidate.median_ms,
                        st.candidate.p95_ms,
                        st.candidate.mad_ms,
                        st.candidate.count
                    );
                    println!(
                        "Gain   : {:.2}x paired (95% CI [{:.2}x, {:.2}x], Mann-Whitney p={:.4})",
                        st.paired_ratio_median, st.ci95_low, st.ci95_high, st.mann_whitney_p
                    );
                }
                if let (Some(r_rss), Some(c_rss)) =
                    (rec.reference_peak_rss_kb, rec.candidate_peak_rss_kb)
                {
                    let pct = if r_rss > 0 {
                        ((c_rss as f64 - r_rss as f64) / r_rss as f64) * 100.0
                    } else {
                        0.0
                    };
                    println!("PeakRSS: {r_rss} KiB -> {c_rss} KiB ({pct:+.1}%)");
                }
                if let Some(vs_base) = rec.speedup_vs_baseline {
                    println!("Cumul. : {:.2}x vs baseline", vs_base);
                }
                for w in &rec.warnings {
                    println!("Warning: {w}");
                }
            }
            if matches!(
                rec.verdict,
                EvalVerdict::CheatBlocked
                    | EvalVerdict::BuildFail
                    | EvalVerdict::TestsFail
                    | EvalVerdict::EquivalenceFail
            ) {
                return Ok(ExitCode::from(2));
            }
            Ok(ExitCode::SUCCESS)
        }
        "checkpoint" => {
            let mut action = None;
            let mut hypothesis = None;
            let mut dead_end = None;
            let mut force = false;
            let mut path = None;
            let mut as_json = false;

            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--keep" => action = Some(CheckpointAction::Keep),
                    "--revert" => action = Some(CheckpointAction::Revert),
                    "--hypothesis" => hypothesis = Some(next_val(rest, &mut i, "--hypothesis")?),
                    "--dead-end" => dead_end = Some(next_val(rest, &mut i, "--dead-end")?),
                    "--force" => force = true,
                    "--path" => path = Some(next_val(rest, &mut i, "--path")?),
                    "--json" => as_json = true,
                    other => return Err(format!("unknown option '{other}' for checkpoint")),
                }
                i += 1;
            }

            let action = action
                .ok_or_else(|| "checkpoint requires either --keep or --revert".to_string())?;
            let hypothesis =
                hypothesis.ok_or_else(|| "checkpoint requires --hypothesis <TEXT>".to_string())?;

            let res = checkpoint_session(CheckpointParams {
                path,
                action,
                hypothesis,
                dead_end,
                force,
            })?;

            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&res).map_err(|e| e.to_string())?
                );
            } else {
                println!("{}", res.message);
            }
            Ok(ExitCode::SUCCESS)
        }
        "ledger" => {
            let mut path = None;
            let mut as_json = false;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--path" => path = Some(next_val(rest, &mut i, "--path")?),
                    "--json" => as_json = true,
                    other => return Err(format!("unknown option '{other}' for ledger")),
                }
                i += 1;
            }
            let res = get_ledger(path.as_deref())?;
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&res).map_err(|e| e.to_string())?
                );
            } else {
                println!(
                    "oa ledger: {:.2} ms -> {:.2} ms ({:.2}x cumulative, {} kept / {} reverted)",
                    res.baseline_median_ms,
                    res.best_median_ms,
                    res.cumulative_speedup,
                    res.kept_count,
                    res.reverted_count
                );
                for e in &res.entries {
                    let sp = e
                        .speedup_vs_prev
                        .map(|v| format!("{v:.2}x"))
                        .unwrap_or_else(|| "-".to_string());
                    println!("  #{:<2} {:<8} {:>6}  {}", e.id, e.action, sp, e.hypothesis);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        "report" => {
            let mut format = "md".to_string();
            let mut theme = SvgTheme::Dark;
            let mut out_path = None;
            let mut path = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--format" => format = next_val(rest, &mut i, "--format")?,
                    "--theme" => {
                        let v = next_val(rest, &mut i, "--theme")?;
                        theme = SvgTheme::parse_str(&v)?;
                    }
                    "--svg" => {
                        format = "svg".to_string();
                        out_path = Some(next_val(rest, &mut i, "--svg")?);
                    }
                    "--out" => out_path = Some(next_val(rest, &mut i, "--out")?),
                    "--path" => path = Some(next_val(rest, &mut i, "--path")?),
                    other => return Err(format!("unknown option '{other}' for report")),
                }
                i += 1;
            }

            let content = if format.eq_ignore_ascii_case("json") {
                let res = get_ledger(path.as_deref())?;
                serde_json::to_string_pretty(&res).map_err(|e| e.to_string())?
            } else if format.eq_ignore_ascii_case("svg") {
                render_svg_report(path.as_deref(), theme)?
            } else {
                render_markdown_report(path.as_deref())?
            };

            if let Some(file) = out_path {
                fs::write(&file, &content)
                    .map_err(|e| format!("failed to write report to '{file}': {e}"))?;
                println!("Wrote report to {file}");
            } else {
                print!("{content}");
            }
            Ok(ExitCode::SUCCESS)
        }
        "verify" => {
            let mut metric = None;
            let mut pairs = None;
            let mut warmup = None;
            let mut min_speedup = None;
            let mut alpha = None;
            let mut path = None;
            let mut as_json = false;

            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--metric" => {
                        let v = next_val(rest, &mut i, "--metric")?;
                        metric = Some(MetricTarget::parse_str(&v)?);
                    }
                    "--pairs" => {
                        let v = next_val(rest, &mut i, "--pairs")?;
                        pairs = Some(
                            v.parse()
                                .map_err(|_| format!("invalid --pairs value '{v}'"))?,
                        );
                    }
                    "--warmup" => {
                        let v = next_val(rest, &mut i, "--warmup")?;
                        warmup = Some(
                            v.parse()
                                .map_err(|_| format!("invalid --warmup value '{v}'"))?,
                        );
                    }
                    "--min-speedup" => {
                        let v = next_val(rest, &mut i, "--min-speedup")?;
                        min_speedup = Some(
                            v.parse()
                                .map_err(|_| format!("invalid --min-speedup value '{v}'"))?,
                        );
                    }
                    "--alpha" => {
                        let v = next_val(rest, &mut i, "--alpha")?;
                        alpha = Some(
                            v.parse()
                                .map_err(|_| format!("invalid --alpha value '{v}'"))?,
                        );
                    }
                    "--path" => path = Some(next_val(rest, &mut i, "--path")?),
                    "--json" => as_json = true,
                    other => return Err(format!("unknown option '{other}' for verify")),
                }
                i += 1;
            }

            let rep = verify_session(VerifyParams {
                path,
                metric,
                pairs,
                warmup,
                min_speedup,
                alpha,
            })?;

            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?
                );
            } else {
                println!("{}", rep.badge);
                for w in &rep.evaluation.warnings {
                    println!("Warning: {w}");
                }
            }
            if rep.passed {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(2))
            }
        }
        "status" => {
            let mut path = None;
            let mut as_json = false;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--path" => path = Some(next_val(rest, &mut i, "--path")?),
                    "--json" => as_json = true,
                    other => return Err(format!("unknown option '{other}' for status")),
                }
                i += 1;
            }
            let st = session_status(path.as_deref())?;
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&st).map_err(|e| e.to_string())?
                );
            } else {
                println!("oa session: {}", st.repo_root);
                println!("  Metric    : {:?}", st.metric);
                println!(
                    "  Commits   : {} -> {}",
                    &st.baseline_commit[..10.min(st.baseline_commit.len())],
                    &st.best_commit[..10.min(st.best_commit.len())]
                );
                println!(
                    "  Wall time : {:.2} ms -> {:.2} ms",
                    st.baseline_median_ms, st.best_median_ms
                );
                if let (Some(b_rss), Some(best_rss)) =
                    (st.baseline_peak_rss_kb, st.best_peak_rss_kb)
                {
                    let pct = if b_rss > 0 {
                        ((best_rss as f64 - b_rss as f64) / b_rss as f64) * 100.0
                    } else {
                        0.0
                    };
                    println!("  Peak RSS  : {b_rss} KiB -> {best_rss} KiB ({pct:+.1}%)");
                }
                println!(
                    "  Gain      : {:.2}x cumulative ({} kept, {} reverted, Pareto {:?})",
                    st.cumulative_gain, st.kept_count, st.reverted_count, st.pareto_frontier_ids
                );
                println!(
                    "  Locks     : {} file(s) ({})",
                    st.locked_files_count,
                    if st.cheat_guard_ok { "OK" } else { "VIOLATION" }
                );
                for v in &st.cheat_violations {
                    println!("    ! {v}");
                }
                if !st.modified_vs_best.is_empty() {
                    println!("  Dirty     : {}", st.modified_vs_best.join(", "));
                }
                if !st.diff_stat_vs_baseline.trim().is_empty() {
                    println!("  Diff vs baseline:");
                    for line in st.diff_stat_vs_baseline.lines() {
                        println!("    {line}");
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        "compare" => {
            let mut positional = Vec::new();
            let mut metric = MetricTarget::Wall;
            let mut pairs = 10usize;
            let mut warmup = 2usize;
            let mut min_speedup = 1.03;
            let mut alpha = 0.01;
            let mut pin_core = None;
            let mut check_output = false;
            let mut as_markdown = false;
            let mut path = None;
            let mut as_json = false;

            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--metric" => {
                        let v = next_val(rest, &mut i, "--metric")?;
                        metric = MetricTarget::parse_str(&v)?;
                    }
                    "--pairs" => {
                        let v = next_val(rest, &mut i, "--pairs")?;
                        pairs = v
                            .parse()
                            .map_err(|_| format!("invalid --pairs value '{v}'"))?;
                    }
                    "--warmup" => {
                        let v = next_val(rest, &mut i, "--warmup")?;
                        warmup = v
                            .parse()
                            .map_err(|_| format!("invalid --warmup value '{v}'"))?;
                    }
                    "--min-speedup" => {
                        let v = next_val(rest, &mut i, "--min-speedup")?;
                        min_speedup = v
                            .parse()
                            .map_err(|_| format!("invalid --min-speedup value '{v}'"))?;
                    }
                    "--alpha" => {
                        let v = next_val(rest, &mut i, "--alpha")?;
                        alpha = v
                            .parse()
                            .map_err(|_| format!("invalid --alpha value '{v}'"))?;
                    }
                    "--pin-core" => {
                        let v = next_val(rest, &mut i, "--pin-core")?;
                        pin_core = Some(
                            v.parse()
                                .map_err(|_| format!("invalid --pin-core value '{v}'"))?,
                        );
                    }
                    "--check-output" => check_output = true,
                    "--markdown" | "--md" => as_markdown = true,
                    "--path" => path = Some(next_val(rest, &mut i, "--path")?),
                    "--json" => as_json = true,
                    other if other.starts_with('-') => {
                        return Err(format!("unknown option '{other}' for compare"));
                    }
                    pos => positional.push(pos.to_string()),
                }
                i += 1;
            }

            if positional.len() != 2 {
                return Err(
                    "compare requires exactly two commands: oa compare <CMD_A> <CMD_B>".to_string(),
                );
            }

            let res = compare_commands(CompareParams {
                path,
                cmd_a: positional[0].clone(),
                cmd_b: positional[1].clone(),
                metric,
                pairs,
                warmup,
                min_speedup,
                alpha,
                pin_core,
                check_output,
            })?;

            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&res).map_err(|e| e.to_string())?
                );
            } else if as_markdown {
                print!("{}", render_compare_markdown(&res));
            } else {
                let unit = match res.metric {
                    MetricTarget::Wall => "ms",
                    MetricTarget::Rss => "KiB",
                };
                println!("Verdict : {:?} (metric {:?})", res.verdict, res.metric);
                println!(
                    "Cmd A   : {:.2} {unit} median (p95 {:.2} {unit}, MAD {:.2} {unit}, n={})  [{}]",
                    res.stats.baseline.median_ms,
                    res.stats.baseline.p95_ms,
                    res.stats.baseline.mad_ms,
                    res.stats.baseline.count,
                    res.cmd_a
                );
                println!(
                    "Cmd B   : {:.2} {unit} median (p95 {:.2} {unit}, MAD {:.2} {unit}, n={})  [{}]",
                    res.stats.candidate.median_ms,
                    res.stats.candidate.p95_ms,
                    res.stats.candidate.mad_ms,
                    res.stats.candidate.count,
                    res.cmd_b
                );
                println!(
                    "Gain    : {:.2}x paired (95% CI [{:.2}x, {:.2}x], Mann-Whitney p={:.4})",
                    res.stats.paired_ratio_median,
                    res.stats.ci95_low,
                    res.stats.ci95_high,
                    res.stats.mann_whitney_p
                );
                if let (Some(ra), Some(rb)) = (res.peak_rss_a_kb, res.peak_rss_b_kb) {
                    let pct = if ra > 0 {
                        ((rb as f64 - ra as f64) / ra as f64) * 100.0
                    } else {
                        0.0
                    };
                    println!("PeakRSS : {ra} KiB -> {rb} KiB ({pct:+.1}%)");
                }
                println!(
                    "Output  : {} (A={}.. B={}..)",
                    if res.outputs_identical {
                        "identical SHA-256"
                    } else {
                        "differs"
                    },
                    &res.stdout_sha256_a[..16],
                    &res.stdout_sha256_b[..16]
                );
                for w in &res.warnings {
                    println!("Warning : {w}");
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        "demo" => {
            let mut as_json = false;
            for arg in rest {
                match arg.as_str() {
                    "--json" => as_json = true,
                    other => return Err(format!("unknown option '{other}' for demo")),
                }
            }
            let res = demo::run_demo(as_json)?;
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&res).map_err(|e| e.to_string())?
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        "doctor" => {
            let mut as_json = false;
            for arg in rest {
                match arg.as_str() {
                    "--json" => as_json = true,
                    other => return Err(format!("unknown option '{other}' for doctor")),
                }
            }
            let rep = doctor::run_doctor();
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?
                );
            } else {
                println!(
                    "oa {} doctor ({} {} | {}, {} CPUs)",
                    rep.version,
                    rep.host.os,
                    rep.host.arch,
                    rep.host.cpu_model,
                    rep.host.logical_cpus
                );
                println!("\nTooling & profilers:");
                for c in &rep.checks {
                    let mark = if c.available { "ok" } else { "--" };
                    println!("  [{mark}] {:<28} {}", c.name, c.detail);
                }
                println!("\nCoding agents:");
                for a in &rep.agents {
                    let mark = if a.available { "ok" } else { "--" };
                    println!("  [{mark}] {:<14} {}", a.name, a.detail);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        "setup" => {
            let mut agent = "all".to_string();
            let mut dry_run = false;
            let mut as_json = false;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--agent" => agent = next_val(rest, &mut i, "--agent")?,
                    "--dry-run" => dry_run = true,
                    "--json" => as_json = true,
                    "--yes" => {}
                    other => return Err(format!("unknown option '{other}' for setup")),
                }
                i += 1;
            }
            let actions = setup::run_setup(&agent, dry_run)?;
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&actions).map_err(|e| e.to_string())?
                );
            } else if actions.is_empty() {
                println!(
                    "No supported agent directories detected. Pass --agent claude|antigravity|cursor|gemini|codex to configure one explicitly."
                );
            } else {
                for a in &actions {
                    println!("[{}] {} -> {}", a.agent, a.config_path, a.status);
                    if let Some(ref sp) = a.skill_path {
                        let verb = if dry_run {
                            "would install"
                        } else {
                            "installed"
                        };
                        println!("  {verb} SKILL.md -> {sp}");
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        "mcp" => {
            mcp::run_stdio_server()?;
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command '{other}'. Run 'oa --help'.")),
    }
}

fn main() -> ExitCode {
    match run_cli() {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}
