use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
use crate::runner::command_exists;
use crate::runner::run_command;

static PROF_SEQ: AtomicU64 = AtomicU64::new(0);

/// A single profiled hotspot mapped back to source location and snippet when available.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hotspot {
    pub function: String,
    pub file: String,
    pub line: usize,
    pub self_ms: f64,
    pub cum_ms: f64,
    pub self_pct: f64,
    pub cum_pct: f64,
    pub calls: u64,
    pub in_project: bool,
    pub snippet: Option<String>,
}

/// Top memory allocation site (e.g., from Python `tracemalloc`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocSite {
    pub file: String,
    pub line: usize,
    pub size_kb: f64,
    pub count: u64,
}

/// Optional Linux hardware performance counters from `perf stat`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HardwareCounters {
    pub cycles: Option<u64>,
    pub instructions: Option<u64>,
    pub ipc: Option<f64>,
    pub branches: Option<u64>,
    pub branch_misses: Option<u64>,
    pub branch_miss_pct: Option<f64>,
    pub cache_references: Option<u64>,
    pub cache_misses: Option<u64>,
    pub cache_miss_pct: Option<f64>,
}

/// Structured, token-efficient hotspot dossier returned by `oa profile`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileReport {
    pub profiler: String,
    pub wall_ms: f64,
    pub user_ms: Option<f64>,
    pub sys_ms: Option<f64>,
    pub peak_rss_kb: Option<u64>,
    pub peak_alloc_kb: Option<f64>,
    pub bottleneck_class: String,
    pub diagnosis: String,
    pub counters: Option<HardwareCounters>,
    pub hotspots: Vec<Hotspot>,
    pub alloc_sites: Vec<AllocSite>,
    pub notes: Vec<String>,
}

/// Per-hotspot change between two profiles (`diff_from`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotspotDelta {
    pub function: String,
    pub file: String,
    pub before_self_ms: f64,
    pub after_self_ms: f64,
    pub delta_ms: f64,
    pub delta_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileDiff {
    pub before_wall_ms: f64,
    pub after_wall_ms: f64,
    pub wall_delta_pct: f64,
    pub new_top_hotspot: Option<String>,
    pub changes: Vec<HotspotDelta>,
}

fn read_source_snippet(root: &Path, file: &str, line: usize, radius: usize) -> Option<String> {
    if line == 0 || file.is_empty() || file.starts_with('<') {
        return None;
    }
    let path = if Path::new(file).is_absolute() {
        Path::new(file).to_path_buf()
    } else {
        root.join(file)
    };
    let content = fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    if line > lines.len() {
        return None;
    }
    let start = line.saturating_sub(radius + 1);
    let end = (line + radius).min(lines.len());
    let mut out = String::new();
    for (idx, text) in lines[start..end].iter().enumerate() {
        let lineno = start + idx + 1;
        let marker = if lineno == line { ">" } else { " " };
        out.push_str(&format!("{marker}{lineno:>5} | {text}\n"));
    }
    Some(out)
}

fn detect_python_invocation(cmd: &str) -> Option<(String, Vec<String>)> {
    let trimmed = cmd.trim();
    if trimmed.contains('|') || trimmed.contains("&&") || trimmed.contains(';') {
        return None;
    }
    let tokens: Vec<String> = trimmed.split_whitespace().map(str::to_string).collect();
    if tokens.len() < 2 {
        return None;
    }
    let bin = Path::new(&tokens[0])
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if matches!(bin, "python" | "python3" | "python.exe" | "python3.exe") {
        if tokens[1] == "-c" {
            return None;
        }
        return Some((tokens[0].clone(), tokens[1..].to_vec()));
    }
    None
}

const PYTHON_PROFILE_HARNESS: &str = r#"
import cProfile, json, os, pstats, runpy, sys, time, tracemalloc

out_path = sys.argv[1]
project_root = os.path.realpath(sys.argv[2])
target_args = sys.argv[3:]

tracemalloc.start()
pr = cProfile.Profile()
t0 = time.perf_counter()
exit_code = 0
pr.enable()
try:
    if target_args[0] == "-m":
        mod = target_args[1]
        sys.argv = [mod] + target_args[2:]
        runpy.run_module(mod, run_name="__main__", alter_sys=True)
    else:
        script = target_args[0]
        sys.argv = target_args
        sys.path.insert(0, os.path.dirname(os.path.realpath(script)))
        runpy.run_path(script, run_name="__main__")
except SystemExit as exc:
    code = exc.code
    if code is None:
        exit_code = 0
    elif isinstance(code, int):
        exit_code = code
    else:
        sys.stderr.write(str(code) + "\n")
        exit_code = 1
finally:
    pr.disable()
if exit_code != 0:
    sys.exit(exit_code)
wall_ms = (time.perf_counter() - t0) * 1000.0
curr_mem, peak_mem = tracemalloc.get_traced_memory()
snap = tracemalloc.take_snapshot()
tracemalloc.stop()

stats = pstats.Stats(pr)
total_tt = sum(v[2] for v in stats.stats.values()) or 1e-9
rows = []
for (filename, lineno, func), (cc, nc, tt, ct, callers) in stats.stats.items():
    real = os.path.realpath(filename) if filename and not filename.startswith("<") else filename
    in_proj = bool(real and real.startswith(project_root + os.sep))
    display_file = os.path.relpath(real, project_root) if in_proj else filename
    rows.append({
        "function": func,
        "file": display_file,
        "line": lineno,
        "self_ms": round(tt * 1000.0, 3),
        "cum_ms": round(ct * 1000.0, 3),
        "self_pct": round((tt / total_tt) * 100.0, 2),
        "cum_pct": round((ct / total_tt) * 100.0, 2),
        "calls": nc,
        "in_project": in_proj,
    })
rows.sort(key=lambda r: (r["self_ms"], r["cum_ms"]), reverse=True)

allocs = []
for st in snap.statistics("lineno")[:10]:
    fr = st.traceback[0]
    real = os.path.realpath(fr.filename) if fr.filename and not fr.filename.startswith("<") else fr.filename
    in_proj = bool(real and real.startswith(project_root + os.sep))
    display_file = os.path.relpath(real, project_root) if in_proj else fr.filename
    allocs.append({
        "file": display_file,
        "line": fr.lineno,
        "size_kb": round(st.size / 1024.0, 2),
        "count": st.count,
    })

payload = {
    "wall_ms": round(wall_ms, 3),
    "peak_alloc_kb": round(peak_mem / 1024.0, 2),
    "hotspots": rows[:15],
    "alloc_sites": allocs,
}
with open(out_path, "w", encoding="utf-8") as f:
    json.dump(payload, f)
"#;

#[derive(Deserialize)]
struct RawPyProfile {
    wall_ms: f64,
    peak_alloc_kb: f64,
    hotspots: Vec<RawPyHotspot>,
    alloc_sites: Vec<AllocSite>,
}

#[derive(Deserialize)]
struct RawPyHotspot {
    function: String,
    file: String,
    line: usize,
    self_ms: f64,
    cum_ms: f64,
    self_pct: f64,
    cum_pct: f64,
    calls: u64,
    in_project: bool,
}

fn profile_python(
    python_bin: &str,
    target_args: &[String],
    cwd: &Path,
    top_k: usize,
) -> Result<ProfileReport, String> {
    let seq = PROF_SEQ.fetch_add(1, Ordering::Relaxed);
    let out_file =
        std::env::temp_dir().join(format!("oa_pyprof_{}_{}.json", std::process::id(), seq));

    let mut args = vec![
        "-c".to_string(),
        PYTHON_PROFILE_HARNESS.to_string(),
        out_file.to_string_lossy().to_string(),
        cwd.to_string_lossy().to_string(),
    ];
    args.extend(target_args.iter().cloned());

    let output = Command::new(python_bin)
        .args(&args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("failed to run Python profiler: {e}"))?;

    if !output.status.success() {
        let _ = fs::remove_file(&out_file);
        return Err(format!(
            "Python profiler exited {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(400)
                .collect::<String>()
        ));
    }

    let raw_json = fs::read_to_string(&out_file)
        .map_err(|e| format!("failed to read Python profile output: {e}"))?;
    let _ = fs::remove_file(&out_file);

    let parsed: RawPyProfile = serde_json::from_str(&raw_json)
        .map_err(|e| format!("failed to parse Python profile JSON: {e}"))?;

    let mut hotspots = Vec::new();
    for h in parsed.hotspots.into_iter().take(top_k) {
        let snippet = if h.in_project && hotspots.len() < 5 {
            read_source_snippet(cwd, &h.file, h.line, 5)
        } else {
            None
        };
        hotspots.push(Hotspot {
            function: h.function,
            file: h.file,
            line: h.line,
            self_ms: h.self_ms,
            cum_ms: h.cum_ms,
            self_pct: h.self_pct,
            cum_pct: h.cum_pct,
            calls: h.calls,
            in_project: h.in_project,
            snippet,
        });
    }

    let (bottleneck_class, diagnosis) = classify_python_profile(&hotspots, parsed.peak_alloc_kb);

    Ok(ProfileReport {
        profiler: "python:cProfile+tracemalloc".to_string(),
        wall_ms: parsed.wall_ms,
        user_ms: None,
        sys_ms: None,
        peak_rss_kb: None,
        peak_alloc_kb: Some(parsed.peak_alloc_kb),
        bottleneck_class,
        diagnosis,
        counters: None,
        hotspots,
        alloc_sites: parsed.alloc_sites,
        notes: vec![
            "cProfile + tracemalloc add profiling overhead; use oa_evaluate for unbiased wall-time speedups."
                .to_string(),
        ],
    })
}

fn classify_python_profile(hotspots: &[Hotspot], peak_alloc_kb: f64) -> (String, String) {
    if let Some(top) = hotspots.first() {
        let lower_fn = top.function.to_ascii_lowercase();
        let lower_file = top.file.to_ascii_lowercase();
        if lower_file.contains("re/")
            || lower_fn.contains("compile")
            || lower_fn.contains("search")
            || lower_fn.contains("match")
        {
            return (
                "REGEX_BOUND".to_string(),
                format!(
                    "Top hotspot {} ({:.1}% self time, {} calls) is in regex matching/compilation; consider precompiling patterns or literal-anchored str.find prechecks.",
                    top.function, top.self_pct, top.calls
                ),
            );
        }
        if lower_fn.contains("read")
            || lower_fn.contains("stat")
            || lower_fn.contains("scandir")
            || lower_fn.contains("open")
        {
            return (
                "IO_BOUND".to_string(),
                format!(
                    "Top hotspot {} ({:.1}% self time, {} calls) is filesystem/I/O bound; reduce redundant walks or batch reads.",
                    top.function, top.self_pct, top.calls
                ),
            );
        }
        if top.calls > 50_000 && top.self_pct >= 20.0 {
            return (
                "CALL_OVERHEAD_BOUND".to_string(),
                format!(
                    "Top hotspot {} ({:.1}% self time across {} calls) is dominated by high call frequency; vectorize, hoist, or eliminate inner-loop calls.",
                    top.function, top.self_pct, top.calls
                ),
            );
        }
        if peak_alloc_kb > 64_000.0 {
            return (
                "ALLOC_PRESSURE".to_string(),
                format!(
                    "Peak traced allocations reached {:.1} MiB while {} accounts for {:.1}% self time; reduce temporary copies or stream iterators.",
                    peak_alloc_kb / 1024.0, top.function, top.self_pct
                ),
            );
        }
        return (
            "CPU_ALGORITHMIC".to_string(),
            format!(
                "Top hotspot {} at {}:{} accounts for {:.1}% self / {:.1}% cumulative time ({} calls).",
                top.function, top.file, top.line, top.self_pct, top.cum_pct, top.calls
            ),
        );
    }
    (
        "UNKNOWN".to_string(),
        "No significant Python hotspots captured.".to_string(),
    )
}

#[cfg(target_os = "linux")]
fn parse_perf_stat_csv(stderr: &str) -> Option<HardwareCounters> {
    let mut counters = HardwareCounters::default();
    let mut found_any = false;
    for line in stderr.lines() {
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 3 {
            continue;
        }
        let val = cols[0].trim().parse::<u64>().ok();
        let event = cols[2].trim();
        let Some(v) = val else { continue };
        found_any = true;
        if event.starts_with("cycles") {
            counters.cycles = Some(v);
        } else if event.starts_with("instructions") {
            counters.instructions = Some(v);
        } else if event.starts_with("branch-misses") {
            counters.branch_misses = Some(v);
        } else if event.starts_with("branches") {
            counters.branches = Some(v);
        } else if event.starts_with("cache-misses") {
            counters.cache_misses = Some(v);
        } else if event.starts_with("cache-references") {
            counters.cache_references = Some(v);
        }
    }
    if !found_any {
        return None;
    }
    if let (Some(c), Some(i)) = (counters.cycles, counters.instructions) {
        if c > 0 {
            counters.ipc = Some((i as f64) / (c as f64));
        }
    }
    if let (Some(b), Some(bm)) = (counters.branches, counters.branch_misses) {
        if b > 0 {
            counters.branch_miss_pct = Some((bm as f64) * 100.0 / (b as f64));
        }
    }
    if let (Some(cr), Some(cm)) = (counters.cache_references, counters.cache_misses) {
        if cr > 0 {
            counters.cache_miss_pct = Some((cm as f64) * 100.0 / (cr as f64));
        }
    }
    Some(counters)
}

#[cfg(target_os = "linux")]
fn profile_linux_native(cmd: &str, cwd: &Path, top_k: usize) -> Result<ProfileReport, String> {
    let base = run_command(cmd, cwd, None, true)?;
    let mut notes = Vec::new();
    let mut counters = None;
    let mut hotspots = Vec::new();
    let mut profiler = "linux:rusage".to_string();

    if command_exists("perf") {
        let full_events =
            "cycles,instructions,branches,branch-misses,cache-references,cache-misses";
        let user_events =
            "cycles:u,instructions:u,branches:u,branch-misses:u,cache-references:u,cache-misses:u";

        let mut used_user_only = false;
        let mut parsed_counters = None;
        let mut saw_paranoid = false;

        for (events, is_user) in [(full_events, false), (user_events, true)] {
            if let Ok(out) = Command::new("perf")
                .args(["stat", "-x", ",", "-e", events, "--", "sh", "-c", cmd])
                .current_dir(cwd)
                .output()
            {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if let Some(parsed) = parse_perf_stat_csv(&stderr) {
                    parsed_counters = Some(parsed);
                    used_user_only = is_user;
                    break;
                }
                if stderr.contains("paranoid")
                    || stderr.contains("Access to performance monitoring")
                {
                    saw_paranoid = true;
                }
            }
        }

        if let Some(parsed) = parsed_counters {
            counters = Some(parsed);
            profiler = if used_user_only {
                notes.push(
                    "Used rootless user-space hardware counters (:u) because kernel.perf_event_paranoid restricts kernel samples."
                        .to_string(),
                );
                "linux:perf(user)".to_string()
            } else {
                "linux:perf".to_string()
            };

            let seq = PROF_SEQ.fetch_add(1, Ordering::Relaxed);
            let perf_data =
                std::env::temp_dir().join(format!("oa_perf_{}_{}.data", std::process::id(), seq));
            let rec_event = if used_user_only { "cycles:u" } else { "cycles" };
            let rec = Command::new("perf")
                .args([
                    "record",
                    "-q",
                    "-e",
                    rec_event,
                    "-o",
                    &perf_data.to_string_lossy(),
                    "--",
                    "sh",
                    "-c",
                    cmd,
                ])
                .current_dir(cwd)
                .output();
            if matches!(rec, Ok(ref r) if r.status.success()) {
                if let Ok(rep) = Command::new("perf")
                    .args([
                        "report",
                        "--stdio",
                        "--no-children",
                        "--percent-limit",
                        "1.0",
                        "-i",
                        &perf_data.to_string_lossy(),
                    ])
                    .current_dir(cwd)
                    .output()
                {
                    let rep_txt = String::from_utf8_lossy(&rep.stdout);
                    for line in rep_txt.lines() {
                        let t = line.trim();
                        if t.is_empty() || t.starts_with('#') {
                            continue;
                        }
                        let cols: Vec<&str> = t.split_whitespace().collect();
                        if cols.len() >= 5 && cols[0].ends_with('%') {
                            if let Ok(pct) = cols[0].trim_end_matches('%').parse::<f64>() {
                                let dso = cols[2].to_string();
                                let sym = cols[4..].join(" ");
                                hotspots.push(Hotspot {
                                    function: sym,
                                    file: dso,
                                    line: 0,
                                    self_ms: (base.wall_ms * pct) / 100.0,
                                    cum_ms: (base.wall_ms * pct) / 100.0,
                                    self_pct: pct,
                                    cum_pct: pct,
                                    calls: 0,
                                    in_project: false,
                                    snippet: None,
                                });
                                if hotspots.len() >= top_k {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            let _ = fs::remove_file(&perf_data);
        } else if saw_paranoid {
            notes.push(
                "Linux perf counters restricted by kernel.perf_event_paranoid; run 'sudo sysctl kernel.perf_event_paranoid=-1' to enable hardware IPC/cache profiling."
                    .to_string(),
            );
        }
    }

    let (bottleneck_class, diagnosis) =
        classify_native_profile(&base, counters.as_ref(), &hotspots);

    Ok(ProfileReport {
        profiler,
        wall_ms: base.wall_ms,
        user_ms: base.user_ms,
        sys_ms: base.sys_ms,
        peak_rss_kb: base.peak_rss_kb,
        peak_alloc_kb: None,
        bottleneck_class,
        diagnosis,
        counters,
        hotspots,
        alloc_sites: Vec::new(),
        notes,
    })
}

fn classify_native_profile(
    outcome: &crate::runner::RunOutcome,
    counters: Option<&HardwareCounters>,
    hotspots: &[Hotspot],
) -> (String, String) {
    if let Some(c) = counters {
        if let Some(bm) = c.branch_miss_pct {
            if bm >= 5.0 {
                return (
                    "BRANCH_MISPREDICT_BOUND".to_string(),
                    format!(
                        "High branch miss rate ({bm:.2}%); consider branchless logic, lookup tables, or sorting inputs."
                    ),
                );
            }
        }
        if let (Some(ipc), Some(cm)) = (c.ipc, c.cache_miss_pct) {
            if ipc < 1.0 && cm >= 15.0 {
                return (
                    "MEMORY_CACHE_BOUND".to_string(),
                    format!(
                        "Low IPC ({ipc:.2}) with high cache miss rate ({cm:.1}%); focus on contiguous memory layouts, struct packing, and cache locality."
                    ),
                );
            }
        }
        if let Some(ipc) = c.ipc {
            let top_hint = hotspots
                .first()
                .map(|h| format!("; top symbol {} ({:.1}%)", h.function, h.self_pct))
                .unwrap_or_default();
            if ipc >= 2.0 {
                return (
                    "COMPUTE_BOUND".to_string(),
                    format!(
                        "High IPC ({ipc:.2}) indicates compute-bound execution{top_hint}; look for algorithmic reduction, SIMD, or hoisting invariant work."
                    ),
                );
            }
            return (
                "CPU_MIXED".to_string(),
                format!("Measured IPC {ipc:.2}{top_hint}."),
            );
        }
    }

    if let (Some(u), Some(s)) = (outcome.user_ms, outcome.sys_ms) {
        let total = (u + s).max(1e-6);
        if s / total > 0.35 {
            return (
                "SYSCALL_IO_BOUND".to_string(),
                format!(
                    "Kernel/sys CPU time is {:.1}% of CPU time ({s:.1} ms sys vs {u:.1} ms user); batch syscalls, buffer I/O, or reduce filesystem stats.",
                    (s / total) * 100.0
                ),
            );
        }
        if u + s < outcome.wall_ms * 0.5 && outcome.wall_ms > 50.0 {
            return (
                "IO_OR_WAIT_BOUND".to_string(),
                format!(
                    "Active CPU ({:.1} ms) is below half of wall time ({:.1} ms); workload is waiting on I/O, locks, or subprocesses.",
                    u + s,
                    outcome.wall_ms
                ),
            );
        }
    }

    (
        "CPU_GENERAL".to_string(),
        format!(
            "Wall time {:.2} ms (peak RSS {:?} KiB).",
            outcome.wall_ms, outcome.peak_rss_kb
        ),
    )
}

fn detect_node_invocation(cmd: &str) -> Option<(String, Vec<String>)> {
    let trimmed = cmd.trim();
    if trimmed.contains('|') || trimmed.contains("&&") || trimmed.contains(';') {
        return None;
    }
    let tokens: Vec<String> = trimmed.split_whitespace().map(str::to_string).collect();
    if tokens.len() < 2 {
        return None;
    }
    let bin = Path::new(&tokens[0])
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if matches!(bin, "node" | "node.exe") {
        if tokens[1] == "-e" || tokens[1] == "--eval" {
            return None;
        }
        return Some((tokens[0].clone(), tokens[1..].to_vec()));
    }
    None
}

#[derive(Deserialize)]
struct V8CallFrame {
    #[serde(rename = "functionName", default)]
    function_name: String,
    #[serde(default)]
    url: String,
    #[serde(rename = "lineNumber", default)]
    line_number: i64,
}

#[derive(Deserialize)]
struct V8Node {
    id: u64,
    #[serde(rename = "callFrame")]
    call_frame: V8CallFrame,
    #[serde(default)]
    children: Vec<u64>,
}

#[derive(Deserialize)]
struct V8CpuProfile {
    #[serde(default)]
    nodes: Vec<V8Node>,
    #[serde(default)]
    samples: Vec<u64>,
    #[serde(rename = "timeDeltas", default)]
    time_deltas: Vec<i64>,
}

/// Parses a V8 `.cpuprofile` JSON payload (produced by `node --cpu-prof`) into a `ProfileReport`.
pub fn parse_v8_cpuprofile(
    raw_json: &str,
    wall_ms: f64,
    cwd: &Path,
    top_k: usize,
) -> Result<ProfileReport, String> {
    let parsed: V8CpuProfile = serde_json::from_str(raw_json)
        .map_err(|e| format!("failed to parse V8 cpuprofile JSON: {e}"))?;

    let mut self_us_by_id: BTreeMap<u64, f64> = BTreeMap::new();
    for (idx, &sample_id) in parsed.samples.iter().enumerate() {
        let dt = parsed.time_deltas.get(idx).copied().unwrap_or(0).max(0) as f64;
        *self_us_by_id.entry(sample_id).or_insert(0.0) += dt;
    }

    let mut parent_of: BTreeMap<u64, u64> = BTreeMap::new();
    for n in &parsed.nodes {
        for &child in &n.children {
            parent_of.insert(child, n.id);
        }
    }

    let mut cum_us_by_id: BTreeMap<u64, f64> = BTreeMap::new();
    for (&nid, &self_us) in &self_us_by_id {
        let mut cur = Some(nid);
        let mut depth = 0usize;
        while let Some(id) = cur {
            *cum_us_by_id.entry(id).or_insert(0.0) += self_us;
            cur = parent_of.get(&id).copied();
            depth += 1;
            if depth > 256 {
                break;
            }
        }
    }

    let total_us: f64 = self_us_by_id.values().sum::<f64>().max(1.0);
    let canon_cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());

    let mut agg: BTreeMap<(String, String, usize, bool), (f64, f64)> = BTreeMap::new();
    for n in &parsed.nodes {
        let func = if n.call_frame.function_name.is_empty() {
            "(anonymous)".to_string()
        } else {
            n.call_frame.function_name.clone()
        };
        if matches!(func.as_str(), "(root)" | "(idle)" | "(program)") {
            continue;
        }
        let self_us = self_us_by_id.get(&n.id).copied().unwrap_or(0.0);
        let cum_us = cum_us_by_id.get(&n.id).copied().unwrap_or(0.0);
        if self_us <= 0.0 && cum_us <= 0.0 {
            continue;
        }

        let clean_url = n
            .call_frame
            .url
            .strip_prefix("file://")
            .unwrap_or(&n.call_frame.url);
        let (display_file, in_project) = if !clean_url.is_empty() && !clean_url.starts_with("node:")
        {
            let p = Path::new(clean_url);
            let canon = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            if let Ok(rel) = canon.strip_prefix(&canon_cwd) {
                (rel.to_string_lossy().to_string(), true)
            } else {
                (clean_url.to_string(), false)
            }
        } else {
            (clean_url.to_string(), false)
        };
        let line = if n.call_frame.line_number >= 0 {
            (n.call_frame.line_number + 1) as usize
        } else {
            0
        };

        let entry = agg
            .entry((func, display_file, line, in_project))
            .or_insert((0.0, 0.0));
        entry.0 += self_us;
        entry.1 = entry.1.max(cum_us);
    }

    let mut rows: Vec<(String, String, usize, bool, f64, f64)> = agg
        .into_iter()
        .map(|((f, file, line, in_proj), (s, c))| (f, file, line, in_proj, s, c))
        .collect();
    rows.sort_by(|a, b| b.4.total_cmp(&a.4).then_with(|| b.5.total_cmp(&a.5)));

    let mut hotspots = Vec::new();
    for (func, file, line, in_project, self_us, cum_us) in rows.into_iter().take(top_k) {
        let snippet = if in_project && hotspots.len() < 5 {
            read_source_snippet(cwd, &file, line, 5)
        } else {
            None
        };
        hotspots.push(Hotspot {
            function: func,
            file,
            line,
            self_ms: (self_us / 1000.0 * 1000.0).round() / 1000.0,
            cum_ms: (cum_us / 1000.0 * 1000.0).round() / 1000.0,
            self_pct: ((self_us / total_us) * 10000.0).round() / 100.0,
            cum_pct: ((cum_us / total_us) * 10000.0).round() / 100.0,
            calls: 0,
            in_project,
            snippet,
        });
    }

    let (bottleneck_class, diagnosis) = if let Some(top) = hotspots.first() {
        let lower = top.function.to_ascii_lowercase();
        if lower.contains("gc") || lower.contains("garbage") {
            (
                "GC_OR_ALLOC_BOUND".to_string(),
                format!(
                    "Top V8 hotspot {} ({:.1}% self) indicates GC/allocation pressure; reuse buffers and avoid temporary object churn.",
                    top.function, top.self_pct
                ),
            )
        } else if lower.contains("json") || lower.contains("stringify") || lower.contains("parse") {
            (
                "SERIALIZATION_BOUND".to_string(),
                format!(
                    "Top V8 hotspot {} at {}:{} ({:.1}% self) is serialization/parsing bound.",
                    top.function, top.file, top.line, top.self_pct
                ),
            )
        } else {
            (
                "CPU_ALGORITHMIC".to_string(),
                format!(
                    "Top V8 hotspot {} at {}:{} accounts for {:.1}% self / {:.1}% cumulative time.",
                    top.function, top.file, top.line, top.self_pct, top.cum_pct
                ),
            )
        }
    } else {
        (
            "UNKNOWN".to_string(),
            "No significant V8 CPU samples captured.".to_string(),
        )
    };

    Ok(ProfileReport {
        profiler: "node:v8-cpu-prof".to_string(),
        wall_ms,
        user_ms: None,
        sys_ms: None,
        peak_rss_kb: None,
        peak_alloc_kb: None,
        bottleneck_class,
        diagnosis,
        counters: None,
        hotspots,
        alloc_sites: Vec::new(),
        notes: vec![
            "Sampling profile via node --cpu-prof; use oa_evaluate for unbiased interleaved A/B timings."
                .to_string(),
        ],
    })
}

fn profile_node(
    node_bin: &str,
    target_args: &[String],
    cwd: &Path,
    top_k: usize,
) -> Result<ProfileReport, String> {
    let seq = PROF_SEQ.fetch_add(1, Ordering::Relaxed);
    let out_dir = std::env::temp_dir().join(format!("oa_nodeprof_{}_{}", std::process::id(), seq));
    fs::create_dir_all(&out_dir).map_err(|e| format!("failed to create node profile dir: {e}"))?;
    let prof_name = "profile.cpuprofile";
    let prof_path = out_dir.join(prof_name);

    let mut args = vec![
        "--cpu-prof".to_string(),
        format!("--cpu-prof-dir={}", out_dir.to_string_lossy()),
        format!("--cpu-prof-name={prof_name}"),
    ];
    args.extend(target_args.iter().cloned());

    let t0 = std::time::Instant::now();
    let output = Command::new(node_bin)
        .args(&args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("failed to run Node.js profiler: {e}"))?;
    let wall_ms = t0.elapsed().as_secs_f64() * 1000.0;

    if !output.status.success() {
        let _ = fs::remove_dir_all(&out_dir);
        return Err(format!(
            "Node.js profiler exited {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(400)
                .collect::<String>()
        ));
    }

    let raw_json = fs::read_to_string(&prof_path)
        .map_err(|e| format!("failed to read V8 cpuprofile output: {e}"))?;
    let _ = fs::remove_dir_all(&out_dir);
    parse_v8_cpuprofile(&raw_json, wall_ms, cwd, top_k)
}

/// Runs the best available profiler adapter for `cmd` inside `cwd`.
pub fn run_profile(
    cmd: &str,
    cwd: &Path,
    mode: &str,
    top_k: usize,
) -> Result<ProfileReport, String> {
    let selected = mode.to_ascii_lowercase();
    if selected == "python" || (selected == "auto" && detect_python_invocation(cmd).is_some()) {
        if let Some((py_bin, target_args)) = detect_python_invocation(cmd) {
            return profile_python(&py_bin, &target_args, cwd, top_k);
        }
        if selected == "python" {
            return Err(
                "python profiler requires a direct 'python ...' or 'python3 ...' command"
                    .to_string(),
            );
        }
    }

    if selected == "node" || (selected == "auto" && detect_node_invocation(cmd).is_some()) {
        if let Some((node_bin, target_args)) = detect_node_invocation(cmd) {
            return profile_node(&node_bin, &target_args, cwd, top_k);
        }
        if selected == "node" {
            return Err("node profiler requires a direct 'node <script> ...' command".to_string());
        }
    }

    #[cfg(target_os = "linux")]
    {
        if selected == "auto" || selected == "perf" || selected == "native" {
            return profile_linux_native(cmd, cwd, top_k);
        }
    }

    let base = run_command(cmd, cwd, None, true)?;
    let (bottleneck_class, diagnosis) = classify_native_profile(&base, None, &[]);
    Ok(ProfileReport {
        profiler: format!("{}:rusage", std::env::consts::OS),
        wall_ms: base.wall_ms,
        user_ms: base.user_ms,
        sys_ms: base.sys_ms,
        peak_rss_kb: base.peak_rss_kb,
        peak_alloc_kb: None,
        bottleneck_class,
        diagnosis,
        counters: None,
        hotspots: Vec::new(),
        alloc_sites: Vec::new(),
        notes: Vec::new(),
    })
}

/// Computes hotspot deltas between a prior profile and the current profile.
pub fn diff_profiles(before: &ProfileReport, after: &ProfileReport) -> ProfileDiff {
    let mut before_map: BTreeMap<(String, String), f64> = BTreeMap::new();
    for h in &before.hotspots {
        *before_map
            .entry((h.function.clone(), h.file.clone()))
            .or_insert(0.0) += h.self_ms;
    }

    let mut after_map: BTreeMap<(String, String), f64> = BTreeMap::new();
    for h in &after.hotspots {
        *after_map
            .entry((h.function.clone(), h.file.clone()))
            .or_insert(0.0) += h.self_ms;
    }

    let mut keys: Vec<(String, String)> =
        before_map.keys().chain(after_map.keys()).cloned().collect();
    keys.sort();
    keys.dedup();

    let mut changes: Vec<HotspotDelta> = keys
        .into_iter()
        .map(|(func, file)| {
            let b = before_map
                .get(&(func.clone(), file.clone()))
                .copied()
                .unwrap_or(0.0);
            let a = after_map
                .get(&(func.clone(), file.clone()))
                .copied()
                .unwrap_or(0.0);
            let delta_ms = a - b;
            let delta_pct = if b > 1e-6 {
                (delta_ms / b) * 100.0
            } else if a > 0.0 {
                100.0
            } else {
                0.0
            };
            HotspotDelta {
                function: func,
                file,
                before_self_ms: b,
                after_self_ms: a,
                delta_ms,
                delta_pct,
            }
        })
        .collect();

    changes.sort_by(|x, y| y.delta_ms.abs().total_cmp(&x.delta_ms.abs()));
    changes.truncate(12);

    let wall_delta_pct = if before.wall_ms > 1e-6 {
        ((after.wall_ms - before.wall_ms) / before.wall_ms) * 100.0
    } else {
        0.0
    };

    let new_top_hotspot = after.hotspots.first().map(|h| {
        format!(
            "{} ({}:{}, {:.1}% self)",
            h.function, h.file, h.line, h.self_pct
        )
    });

    ProfileDiff {
        before_wall_ms: before.wall_ms,
        after_wall_ms: after.wall_ms,
        wall_delta_pct,
        new_top_hotspot,
        changes,
    }
}
