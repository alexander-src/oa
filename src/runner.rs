use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Result of executing a shell command in a target working directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunOutcome {
    pub exit_code: i32,
    pub wall_ms: f64,
    pub user_ms: Option<f64>,
    pub sys_ms: Option<f64>,
    pub peak_rss_kb: Option<u64>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Host hardware and OS metadata recorded in benchmark reports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    pub os: String,
    pub arch: String,
    pub cpu_model: String,
    pub logical_cpus: usize,
}

pub fn collect_host_info() -> HostInfo {
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let logical_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let cpu_model = detect_cpu_model().unwrap_or_else(|| format!("{arch} ({logical_cpus} CPUs)"));

    HostInfo {
        os,
        arch,
        cpu_model,
        logical_cpus,
    }
}

fn detect_cpu_model() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = fs::read_to_string("/proc/cpuinfo") {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("model name") {
                    if let Some((_, val)) = rest.split_once(':') {
                        let trimmed = val.trim();
                        if !trimmed.is_empty() {
                            return Some(trimmed.to_string());
                        }
                    }
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
        {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

pub fn shell_command(cmd: &str, cwd: &Path) -> Command {
    #[cfg(target_os = "windows")]
    let mut c = {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("cmd");
        c.raw_arg("/S /C").raw_arg(format!("\"{cmd}\""));
        c
    };
    #[cfg(not(target_os = "windows"))]
    let mut c = {
        let mut c = Command::new("sh");
        c.args(["-c", cmd]);
        c
    };
    c.current_dir(cwd);
    c
}

pub fn command_exists(bin: &str) -> bool {
    #[cfg(target_os = "windows")]
    let check = Command::new("where")
        .arg(bin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(not(target_os = "windows"))]
    let check = Command::new("sh")
        .args(["-c", &format!("command -v {bin}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    matches!(check, Ok(s) if s.success())
}

#[cfg(unix)]
const PY_RUSAGE_WRAPPER: &str = "import resource,subprocess,sys; tf,cmd=sys.argv[1],sys.argv[2]; p=subprocess.run(['sh','-c',cmd]); r=resource.getrusage(resource.RUSAGE_CHILDREN); rss=r.ru_maxrss//1024 if sys.platform=='darwin' else r.ru_maxrss; open(tf,'w').write(f'OA_RUSAGE {r.ru_utime:.6f} {r.ru_stime:.6f} {rss}\\n'); sys.exit(p.returncode)";

/// Runs a command with optional CPU core pinning (`taskset` on Linux) and optional resource stats (`/usr/bin/time` or Python `resource` fallback).
pub fn run_command(
    cmd: &str,
    cwd: &Path,
    pin_core: Option<usize>,
    measure_rusage: bool,
) -> Result<RunOutcome, String> {
    let mut user_ms = None;
    let mut sys_ms = None;
    let mut peak_rss_kb = None;

    #[cfg(target_os = "linux")]
    let use_gnu_time = measure_rusage && Path::new("/usr/bin/time").is_file();
    #[cfg(not(target_os = "linux"))]
    let use_gnu_time = false;

    #[cfg(target_os = "macos")]
    let use_bsd_time = measure_rusage && Path::new("/usr/bin/time").is_file();
    #[cfg(not(target_os = "macos"))]
    let use_bsd_time = false;

    #[cfg(unix)]
    let use_py_rusage =
        measure_rusage && !use_gnu_time && !use_bsd_time && command_exists("python3");
    #[cfg(not(unix))]
    let use_py_rusage = {
        let _ = measure_rusage;
        false
    };

    let time_file = if use_gnu_time || use_py_rusage {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("oa_rusage_{}_{}.txt", std::process::id(), seq));
        Some(p)
    } else {
        None
    };

    let mut command = if use_gnu_time {
        let tf = time_file.as_ref().expect("time_file set");
        let mut c = if let Some(core) = pin_core {
            if command_exists("taskset") {
                let mut tc = Command::new("taskset");
                tc.args(["-c", &core.to_string(), "/usr/bin/time"]);
                tc
            } else {
                Command::new("/usr/bin/time")
            }
        } else {
            Command::new("/usr/bin/time")
        };
        c.args([
            "-f",
            "OA_RUSAGE %U %S %M",
            "-o",
            &tf.to_string_lossy(),
            "sh",
            "-c",
            cmd,
        ]);
        c.current_dir(cwd);
        c
    } else if use_py_rusage {
        #[cfg(unix)]
        let mut c = {
            let tf = time_file.as_ref().expect("time_file set");
            #[cfg(target_os = "linux")]
            let mut inner = if let Some(core) = pin_core {
                if command_exists("taskset") {
                    let mut tc = Command::new("taskset");
                    tc.args(["-c", &core.to_string(), "python3"]);
                    tc
                } else {
                    Command::new("python3")
                }
            } else {
                Command::new("python3")
            };
            #[cfg(not(target_os = "linux"))]
            let mut inner = {
                let _ = pin_core;
                Command::new("python3")
            };
            inner.args(["-c", PY_RUSAGE_WRAPPER, &tf.to_string_lossy(), cmd]);
            inner.current_dir(cwd);
            inner
        };
        #[cfg(not(unix))]
        let mut c = shell_command(cmd, cwd);
        c.current_dir(cwd);
        c
    } else if use_bsd_time {
        let mut c = Command::new("/usr/bin/time");
        c.args(["-l", "sh", "-c", cmd]);
        c.current_dir(cwd);
        c
    } else {
        #[cfg(target_os = "linux")]
        let mut c = if let Some(core) = pin_core {
            if command_exists("taskset") {
                let mut tc = Command::new("taskset");
                tc.args(["-c", &core.to_string(), "sh", "-c", cmd]);
                tc.current_dir(cwd);
                tc
            } else {
                shell_command(cmd, cwd)
            }
        } else {
            shell_command(cmd, cwd)
        };
        #[cfg(not(target_os = "linux"))]
        let mut c = {
            let _ = pin_core;
            shell_command(cmd, cwd)
        };
        c.current_dir(cwd);
        c
    };

    let start = Instant::now();
    let output = command
        .output()
        .map_err(|e| format!("failed to execute '{cmd}' in '{}': {e}", cwd.display()))?;
    let wall_ms = start.elapsed().as_secs_f64() * 1000.0;

    if let Some(tf) = time_file {
        if let Ok(content) = fs::read_to_string(&tf) {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("OA_RUSAGE ") {
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    if parts.len() == 3 {
                        user_ms = parts[0].parse::<f64>().ok().map(|s| s * 1000.0);
                        sys_ms = parts[1].parse::<f64>().ok().map(|s| s * 1000.0);
                        peak_rss_kb = parts[2].parse::<u64>().ok();
                    }
                }
            }
        }
        let _ = fs::remove_file(tf);
    } else if use_bsd_time {
        let stderr_str = String::from_utf8_lossy(&output.stderr);
        for line in stderr_str.lines() {
            let trimmed = line.trim();
            if trimmed.ends_with("maximum resident set size") {
                if let Some(first) = trimmed.split_whitespace().next() {
                    if let Ok(bytes) = first.parse::<u64>() {
                        peak_rss_kb = Some(bytes / 1024);
                    }
                }
            } else if trimmed.contains(" real ")
                && trimmed.contains(" user ")
                && trimmed.contains(" sys")
            {
                let cols: Vec<&str> = trimmed.split_whitespace().collect();
                for w in cols.windows(2) {
                    if w[1] == "user" {
                        user_ms = w[0].parse::<f64>().ok().map(|s| s * 1000.0);
                    } else if w[1] == "sys" {
                        sys_ms = w[0].parse::<f64>().ok().map(|s| s * 1000.0);
                    }
                }
            }
        }
    }

    Ok(RunOutcome {
        exit_code: output.status.code().unwrap_or(-1),
        wall_ms,
        user_ms,
        sys_ms,
        peak_rss_kb,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}
