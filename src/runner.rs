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

#[cfg(windows)]
const WIN_RUSAGE_WRAPPER: &str = r#"import ctypes,ctypes.wintypes as wt,subprocess,sys
class PMC(ctypes.Structure):
    _fields_=[('cb',wt.DWORD),('PageFaultCount',wt.DWORD),('PeakWorkingSetSize',ctypes.c_size_t),('WorkingSetSize',ctypes.c_size_t),('QuotaPeakPagedPoolUsage',ctypes.c_size_t),('QuotaPagedPoolUsage',ctypes.c_size_t),('QuotaPeakNonPagedPoolUsage',ctypes.c_size_t),('QuotaNonPagedPoolUsage',ctypes.c_size_t),('PagefileUsage',ctypes.c_size_t),('PeakPagefileUsage',ctypes.c_size_t)]
class JBASIC(ctypes.Structure):
    _fields_=[('PerProcessUserTimeLimit',ctypes.c_int64),('PerJobUserTimeLimit',ctypes.c_int64),('LimitFlags',wt.DWORD),('MinimumWorkingSetSize',ctypes.c_size_t),('MaximumWorkingSetSize',ctypes.c_size_t),('ActiveProcessLimit',wt.DWORD),('Affinity',ctypes.c_size_t),('PriorityClass',wt.DWORD),('SchedulingClass',wt.DWORD)]
class IOC(ctypes.Structure):
    _fields_=[(n,ctypes.c_uint64) for n in ('a','b','c','d','e','f')]
class JEXT(ctypes.Structure):
    _fields_=[('BasicLimitInformation',JBASIC),('IoInfo',IOC),('ProcessMemoryLimit',ctypes.c_size_t),('JobMemoryLimit',ctypes.c_size_t),('PeakProcessMemoryUsed',ctypes.c_size_t),('PeakJobMemoryUsed',ctypes.c_size_t)]
k32=ctypes.WinDLL('kernel32',use_last_error=True)
k32.K32GetProcessMemoryInfo.argtypes=[wt.HANDLE,ctypes.POINTER(PMC),wt.DWORD]
k32.K32GetProcessMemoryInfo.restype=wt.BOOL
k32.GetProcessTimes.argtypes=[wt.HANDLE,ctypes.POINTER(wt.FILETIME),ctypes.POINTER(wt.FILETIME),ctypes.POINTER(wt.FILETIME),ctypes.POINTER(wt.FILETIME)]
k32.GetProcessTimes.restype=wt.BOOL
k32.CreateJobObjectW.argtypes=[ctypes.c_void_p,wt.LPCWSTR]
k32.CreateJobObjectW.restype=wt.HANDLE
k32.AssignProcessToJobObject.argtypes=[wt.HANDLE,wt.HANDLE]
k32.AssignProcessToJobObject.restype=wt.BOOL
k32.QueryInformationJobObject.argtypes=[wt.HANDLE,ctypes.c_int,ctypes.c_void_p,wt.DWORD,ctypes.POINTER(wt.DWORD)]
k32.QueryInformationJobObject.restype=wt.BOOL
k32.CloseHandle.argtypes=[wt.HANDLE]
tf,cmd=sys.argv[1],sys.argv[2]
use_shell=any(ch in cmd for ch in '|&><^%;')
p=None
if not use_shell:
    try:
        p=subprocess.Popen(cmd,shell=False)
    except OSError:
        p=None
if p is None:
    p=subprocess.Popen(cmd,shell=True)
job=k32.CreateJobObjectW(None,None)
if job:
    k32.AssignProcessToJobObject(job,int(p._handle))
rc=p.wait()
pmc=PMC()
pmc.cb=ctypes.sizeof(PMC)
peak_bytes=pmc.PeakWorkingSetSize if k32.K32GetProcessMemoryInfo(int(p._handle),ctypes.byref(pmc),pmc.cb) else 0
if job:
    jext=JEXT()
    ret_len=wt.DWORD(0)
    if k32.QueryInformationJobObject(job,9,ctypes.byref(jext),ctypes.sizeof(JEXT),ctypes.byref(ret_len)):
        peak_bytes=max(peak_bytes,int(jext.PeakJobMemoryUsed))
    k32.CloseHandle(job)
c_t,e_t,k_t,u_t=wt.FILETIME(),wt.FILETIME(),wt.FILETIME(),wt.FILETIME()
user_s,sys_s=0.0,0.0
if k32.GetProcessTimes(int(p._handle),ctypes.byref(c_t),ctypes.byref(e_t),ctypes.byref(k_t),ctypes.byref(u_t)):
    user_s=((u_t.dwHighDateTime<<32)|u_t.dwLowDateTime)/1e7
    sys_s=((k_t.dwHighDateTime<<32)|k_t.dwLowDateTime)/1e7
rss_kb=max(1,int(peak_bytes)//1024) if peak_bytes>0 else 0
if rss_kb>0:
    open(tf,'w').write(f'OA_RUSAGE {user_s:.6f} {sys_s:.6f} {rss_kb}\n')
sys.exit(rc)
"#;

#[cfg(windows)]
fn windows_python_bin() -> Option<&'static str> {
    if command_exists("python") {
        Some("python")
    } else if command_exists("python3") {
        Some("python3")
    } else {
        None
    }
}

/// Runs a command with optional CPU core pinning (`taskset` on Linux) and optional resource stats (`/usr/bin/time`, Python `resource`, or Windows Win32 `K32GetProcessMemoryInfo` / JobObject).
pub fn run_command(
    cmd: &str,
    cwd: &Path,
    pin_core: Option<usize>,
    measure_rusage: bool,
) -> Result<RunOutcome, String> {
    run_command_with_env(cmd, cwd, pin_core, measure_rusage, &[])
}

/// Runs a command with extra environment variables applied cleanly across platforms.
pub fn run_command_with_env(
    cmd: &str,
    cwd: &Path,
    pin_core: Option<usize>,
    measure_rusage: bool,
    env_vars: &[(String, String)],
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
    let use_py_rusage = false;

    #[cfg(windows)]
    let win_py_bin = if measure_rusage {
        windows_python_bin()
    } else {
        None
    };
    #[cfg(not(windows))]
    let win_py_bin: Option<&'static str> = None;

    let time_file = if use_gnu_time || use_py_rusage || win_py_bin.is_some() {
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
    } else if let Some(py_bin) = win_py_bin {
        #[cfg(windows)]
        let mut c = {
            let _ = pin_core;
            let tf = time_file.as_ref().expect("time_file set");
            let mut inner = Command::new(py_bin);
            inner.args(["-c", WIN_RUSAGE_WRAPPER, &tf.to_string_lossy(), cmd]);
            inner.current_dir(cwd);
            inner
        };
        #[cfg(not(windows))]
        let mut c = {
            let _ = py_bin;
            shell_command(cmd, cwd)
        };
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

    for (k, v) in env_vars {
        command.env(k, v);
    }

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
