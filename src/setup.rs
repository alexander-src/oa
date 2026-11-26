use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Map, Value};

const EMBEDDED_SKILL_MD: &str = include_str!("../SKILL.md");

#[derive(Debug, Clone, Serialize)]
pub struct SetupAction {
    pub agent: String,
    pub config_path: String,
    pub skill_path: Option<String>,
    pub status: String,
}

fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "could not determine home directory (HOME / USERPROFILE unset)".to_string())
}

fn current_oa_binary() -> Result<String, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("failed to locate current oa executable: {e}"))?;
    Ok(exe.to_string_lossy().to_string())
}

fn backup_file(path: &Path) -> Result<(), String> {
    if path.exists() {
        let mut bak = path.as_os_str().to_os_string();
        bak.push(".oa.bak");
        fs::copy(path, PathBuf::from(bak))
            .map_err(|e| format!("failed to back up '{}': {e}", path.display()))?;
    }
    Ok(())
}

fn patch_json_mcp_config(path: &Path, oa_bin: &str, dry_run: bool) -> Result<String, String> {
    let mut root: Value = if path.exists() {
        let raw = fs::read_to_string(path)
            .map_err(|e| format!("failed to read '{}': {e}", path.display()))?;
        if raw.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&raw)
                .map_err(|e| format!("failed to parse JSON '{}': {e}", path.display()))?
        }
    } else {
        json!({})
    };

    let obj = root
        .as_object_mut()
        .ok_or_else(|| format!("'{}' is not a JSON object", path.display()))?;

    let servers = obj
        .entry("mcpServers".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| format!("'mcpServers' in '{}' is not an object", path.display()))?;

    servers_obj.insert(
        "oa".to_string(),
        json!({
            "command": oa_bin,
            "args": ["mcp"]
        }),
    );

    if dry_run {
        return Ok("would update mcpServers.oa".to_string());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create '{}': {e}", parent.display()))?;
    }
    backup_file(path)?;
    let formatted =
        serde_json::to_string_pretty(&root).map_err(|e| format!("failed to format JSON: {e}"))?;
    fs::write(path, format!("{formatted}\n"))
        .map_err(|e| format!("failed to write '{}': {e}", path.display()))?;
    Ok("updated mcpServers.oa (backup saved to .oa.bak)".to_string())
}

fn patch_codex_toml(path: &Path, oa_bin: &str, dry_run: bool) -> Result<String, String> {
    let existing = if path.exists() {
        fs::read_to_string(path).map_err(|e| format!("failed to read '{}': {e}", path.display()))?
    } else {
        String::new()
    };

    if existing.contains("[mcp_servers.oa]") {
        return Ok("already configured ([mcp_servers.oa] present)".to_string());
    }

    let escaped = oa_bin.replace('\\', "\\\\").replace('"', "\\\"");
    let block = format!("\n[mcp_servers.oa]\ncommand = \"{escaped}\"\nargs = [\"mcp\"]\n");

    if dry_run {
        return Ok("would append [mcp_servers.oa] block".to_string());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create '{}': {e}", parent.display()))?;
    }
    backup_file(path)?;
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&block);
    fs::write(path, updated).map_err(|e| format!("failed to write '{}': {e}", path.display()))?;
    Ok("appended [mcp_servers.oa] (backup saved to .oa.bak)".to_string())
}

fn install_skill(skill_path: &Path, dry_run: bool) -> Result<(), String> {
    if dry_run {
        return Ok(());
    }
    if let Some(parent) = skill_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create '{}': {e}", parent.display()))?;
    }
    fs::write(skill_path, EMBEDDED_SKILL_MD)
        .map_err(|e| format!("failed to write '{}': {e}", skill_path.display()))
}

struct AgentTarget {
    id: &'static str,
    label: &'static str,
    detect_dir: PathBuf,
    config_path: PathBuf,
    skill_path: Option<PathBuf>,
    is_toml: bool,
}

fn discover_targets(home: &Path) -> Vec<AgentTarget> {
    vec![
        AgentTarget {
            id: "claude",
            label: "Claude Code",
            detect_dir: home.join(".claude"),
            config_path: home.join(".claude.json"),
            skill_path: Some(home.join(".claude/skills/oa/SKILL.md")),
            is_toml: false,
        },
        AgentTarget {
            id: "antigravity",
            label: "Antigravity",
            detect_dir: home.join(".gemini/antigravity"),
            config_path: home.join(".gemini/antigravity/mcp_config.json"),
            skill_path: Some(home.join(".gemini/config/skills/oa/SKILL.md")),
            is_toml: false,
        },
        AgentTarget {
            id: "cursor",
            label: "Cursor",
            detect_dir: home.join(".cursor"),
            config_path: home.join(".cursor/mcp.json"),
            skill_path: None,
            is_toml: false,
        },
        AgentTarget {
            id: "gemini",
            label: "Gemini CLI",
            detect_dir: home.join(".gemini"),
            config_path: home.join(".gemini/settings.json"),
            skill_path: None,
            is_toml: false,
        },
        AgentTarget {
            id: "codex",
            label: "Codex",
            detect_dir: home.join(".codex"),
            config_path: home.join(".codex/config.toml"),
            skill_path: None,
            is_toml: true,
        },
    ]
}

/// Registers the `oa` MCP server and `SKILL.md` for detected or explicitly chosen coding agents.
pub fn run_setup(agent_filter: &str, dry_run: bool) -> Result<Vec<SetupAction>, String> {
    let home = home_dir()?;
    let oa_bin = current_oa_binary()?;
    let filter = agent_filter.to_ascii_lowercase();
    let targets = discover_targets(&home);

    let mut actions = Vec::new();
    for target in targets {
        let explicit = filter == target.id;
        if filter != "all" && !explicit {
            continue;
        }
        let detected = target.detect_dir.exists() || target.config_path.exists();
        if !explicit && !detected {
            continue;
        }

        let status = if target.is_toml {
            patch_codex_toml(&target.config_path, &oa_bin, dry_run)?
        } else {
            patch_json_mcp_config(&target.config_path, &oa_bin, dry_run)?
        };

        let mut skill_str = None;
        if let Some(ref sp) = target.skill_path {
            install_skill(sp, dry_run)?;
            skill_str = Some(sp.to_string_lossy().to_string());
        }

        actions.push(SetupAction {
            agent: target.label.to_string(),
            config_path: target.config_path.to_string_lossy().to_string(),
            skill_path: skill_str,
            status,
        });
    }

    Ok(actions)
}
