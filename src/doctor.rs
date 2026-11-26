use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde::Serialize;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::runner::command_exists;
use crate::runner::{collect_host_info, HostInfo};

#[derive(Debug, Clone, Serialize)]
pub struct CheckItem {
    pub name: String,
    pub available: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub version: String,
    pub host: HostInfo,
    pub checks: Vec<CheckItem>,
    pub agents: Vec<CheckItem>,
}

fn first_version_line(bin: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(bin).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stdout
        .lines()
        .chain(stderr.lines())
        .map(str::trim)
        .find(|l| !l.is_empty())?;
    Some(line.to_string())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn check_agent_config(
    agent: &str,
    config_path: PathBuf,
    skill_path: PathBuf,
    marker: &str,
) -> CheckItem {
    let cfg_configured = fs::read_to_string(&config_path)
        .map(|s| s.contains(marker))
        .unwrap_or(false);
    let skill_present = skill_path.exists();
    let dir_exists = config_path.parent().map(|p| p.exists()).unwrap_or(false);
    let detail = if cfg_configured && skill_present {
        format!("MCP + SKILL.md configured ({})", config_path.display())
    } else if cfg_configured {
        format!(
            "MCP configured; run `oa setup` for SKILL.md ({})",
            config_path.display()
        )
    } else if dir_exists {
        format!("detected, not configured yet (run `oa setup --agent {agent}`)")
    } else {
        "not installed".to_string()
    };
    CheckItem {
        name: agent.to_string(),
        available: cfg_configured,
        detail,
    }
}

/// Inspects local host tooling, profiler backends, RSS sampling, and agent configs.
pub fn run_doctor() -> DoctorReport {
    let host = collect_host_info();
    let mut checks = Vec::new();

    // Git
    if let Some(ver) = first_version_line("git", &["--version"]) {
        checks.push(CheckItem {
            name: "git".to_string(),
            available: true,
            detail: ver,
        });
    } else {
        checks.push(CheckItem {
            name: "git".to_string(),
            available: false,
            detail: "git not found on PATH (required for shadow worktrees)".to_string(),
        });
    }

    // Python profiler
    let py = first_version_line("python3", &["--version"])
        .or_else(|| first_version_line("python", &["--version"]));
    checks.push(CheckItem {
        name: "python:cProfile+tracemalloc".to_string(),
        available: py.is_some(),
        detail: py.unwrap_or_else(|| "python3/python not found on PATH".to_string()),
    });

    // Node.js V8 profiler
    let node = first_version_line("node", &["--version"]);
    checks.push(CheckItem {
        name: "node:v8-cpu-prof".to_string(),
        available: node.is_some(),
        detail: node
            .map(|v| format!("Node.js {v}"))
            .unwrap_or_else(|| "node not found on PATH".to_string()),
    });

    // Go pprof profiler
    let go = first_version_line("go", &["version"]);
    checks.push(CheckItem {
        name: "go:pprof".to_string(),
        available: go.is_some(),
        detail: go.unwrap_or_else(|| "go not found on PATH".to_string()),
    });

    // OS native profiler
    #[cfg(target_os = "linux")]
    {
        let has_perf = command_exists("perf");
        let paranoid = fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok());
        let detail = match (has_perf, paranoid) {
            (true, Some(p)) if p <= 1 => format!("perf ready (perf_event_paranoid={p})"),
            (true, Some(p)) => format!(
                "perf ready with automatic rootless :u user-space counters (perf_event_paranoid={p})"
            ),
            (true, None) => "perf found on PATH".to_string(),
            (false, _) => "perf not installed (install linux-tools / perf for native symbol hotspots)"
                .to_string(),
        };
        checks.push(CheckItem {
            name: "linux:perf".to_string(),
            available: has_perf,
            detail,
        });

        let has_taskset = command_exists("taskset");
        checks.push(CheckItem {
            name: "cpu-pinning".to_string(),
            available: has_taskset,
            detail: if has_taskset {
                "taskset available (--pin-core enabled)".to_string()
            } else {
                "taskset not found (util-linux)".to_string()
            },
        });
    }

    #[cfg(target_os = "macos")]
    {
        let has_sample = command_exists("sample");
        checks.push(CheckItem {
            name: "macos:sample".to_string(),
            available: has_sample,
            detail: if has_sample {
                "/usr/bin/sample available".to_string()
            } else {
                "sample not found (install Xcode Command Line Tools)".to_string()
            },
        });
    }

    // Peak RSS (--metric rss)
    #[cfg(target_os = "linux")]
    {
        let has_gnu_time = std::path::Path::new("/usr/bin/time").exists();
        let has_py = command_exists("python3");
        let detail = if has_gnu_time {
            "/usr/bin/time available (--metric rss enabled)".to_string()
        } else if has_py {
            "python3 resource.getrusage fallback available (--metric rss enabled)".to_string()
        } else {
            "install GNU time or python3 to enable --metric rss".to_string()
        };
        checks.push(CheckItem {
            name: "metric:rss".to_string(),
            available: has_gnu_time || has_py,
            detail,
        });
    }
    #[cfg(target_os = "macos")]
    {
        let has_time = std::path::Path::new("/usr/bin/time").exists();
        checks.push(CheckItem {
            name: "metric:rss".to_string(),
            available: has_time,
            detail: if has_time {
                "/usr/bin/time -l available (--metric rss enabled)".to_string()
            } else {
                "/usr/bin/time not found".to_string()
            },
        });
    }
    #[cfg(windows)]
    {
        checks.push(CheckItem {
            name: "metric:rss".to_string(),
            available: false,
            detail: "Windows Peak RSS tracking is not implemented yet (see issue #3)".to_string(),
        });
    }

    let mut agents = Vec::new();
    if let Some(home) = home_dir() {
        agents.push(check_agent_config(
            "claude",
            home.join(".claude.json"),
            home.join(".claude")
                .join("skills")
                .join("oa")
                .join("SKILL.md"),
            "\"oa\"",
        ));
        agents.push(check_agent_config(
            "antigravity",
            home.join(".gemini")
                .join("antigravity")
                .join("mcp_config.json"),
            home.join(".gemini")
                .join("config")
                .join("skills")
                .join("oa")
                .join("SKILL.md"),
            "\"oa\"",
        ));
        agents.push(check_agent_config(
            "cursor",
            home.join(".cursor").join("mcp.json"),
            home.join(".cursor")
                .join("skills")
                .join("oa")
                .join("SKILL.md"),
            "\"oa\"",
        ));
        agents.push(check_agent_config(
            "gemini",
            home.join(".gemini").join("settings.json"),
            home.join(".gemini")
                .join("skills")
                .join("oa")
                .join("SKILL.md"),
            "\"oa\"",
        ));
        agents.push(check_agent_config(
            "codex",
            home.join(".codex").join("config.toml"),
            home.join(".codex")
                .join("skills")
                .join("oa")
                .join("SKILL.md"),
            "[mcp_servers.oa]",
        ));
    }

    DoctorReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        host,
        checks,
        agents,
    }
}
