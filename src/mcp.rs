use std::io::{self, BufRead, BufReader, Write};

use serde_json::{json, Value};

use crate::guard::EquivalencePolicy;
use crate::session::{
    checkpoint_session, evaluate_session, get_ledger, init_session, profile_session,
    render_markdown_report, CheckpointAction, CheckpointParams, EvaluateParams, InitParams,
    MetricTarget, ProfileParams,
};

#[derive(Clone, Copy)]
enum Framing {
    Ndjson,
    ContentLength,
}

fn read_rpc_message<R: BufRead>(reader: &mut R) -> io::Result<Option<(Value, Framing)>> {
    let mut first_line = String::new();
    loop {
        first_line.clear();
        let n = reader.read_line(&mut first_line)?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = first_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(len_str) = strip_content_length_header(trimmed) {
            let content_len: usize = len_str.trim().parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length header")
            })?;
            // Consume remaining headers up to blank line.
            let mut header_line = String::new();
            loop {
                header_line.clear();
                let hn = reader.read_line(&mut header_line)?;
                if hn == 0 || header_line.trim().is_empty() {
                    break;
                }
            }
            let mut body = vec![0u8; content_len];
            reader.read_exact(&mut body)?;
            let val: Value = serde_json::from_slice(&body)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            return Ok(Some((val, Framing::ContentLength)));
        }

        let val: Value = serde_json::from_str(trimmed)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        return Ok(Some((val, Framing::Ndjson)));
    }
}

fn strip_content_length_header(line: &str) -> Option<&str> {
    let prefix = "content-length:";
    if line.len() >= prefix.len() && line[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&line[prefix.len()..])
    } else {
        None
    }
}

fn write_rpc_message<W: Write>(writer: &mut W, msg: &Value, framing: Framing) -> io::Result<()> {
    let payload = serde_json::to_string(msg).map_err(io::Error::other)?;
    match framing {
        Framing::ContentLength => {
            write!(
                writer,
                "Content-Length: {}\r\n\r\n{}",
                payload.len(),
                payload
            )?;
        }
        Framing::Ndjson => {
            writeln!(writer, "{payload}")?;
        }
    }
    writer.flush()
}

fn tool_schemas() -> Value {
    json!([
        {
            "name": "oa_init",
            "description": "Initialize an oa optimization session in a git repository: locks protected test/bench files, snapshots baseline/best shadow worktrees, captures baseline timing and output SHA-256, and returns the initial hotspot profile.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Repository path (defaults to current working directory)." },
                    "bench_cmd": { "type": "string", "description": "Deterministic command whose wall time will be optimized." },
                    "test_cmd": { "type": "string", "description": "Optional test suite command required to pass before accepting changes." },
                    "build_cmd": { "type": "string", "description": "Optional build command executed before benchmarking or profiling." },
                    "profile_cmd": { "type": "string", "description": "Optional profiler command (defaults to bench_cmd)." },
                    "metric": { "type": "string", "enum": ["wall", "rss"], "description": "Primary optimization target: wall (median latency ms, default) or rss (peak memory KiB)." },
                    "output_files": { "type": "array", "items": { "type": "string" }, "description": "Relative output file paths whose SHA-256 must match baseline." },
                    "lock_files": { "type": "array", "items": { "type": "string" }, "description": "Extra file paths locked by Cheat Guard against edits." },
                    "auto_lock_tests": { "type": "boolean", "description": "Automatically lock tracked test/bench files (default true)." },
                    "ignore_substrings": { "type": "array", "items": { "type": "string" }, "description": "Optional line substrings to ignore in normalized stdout/file equivalence mode." },
                    "oracle_cmd": { "type": "string", "description": "Optional external oracle command used instead of exact SHA-256 matching." },
                    "min_speedup": { "type": "number", "description": "Minimum 95% CI lower-bound speedup ratio to accept (default 1.03)." },
                    "alpha": { "type": "number", "description": "Mann-Whitney U two-sided significance threshold (default 0.01)." },
                    "warmup": { "type": "integer", "description": "Interleaved warmup pairs before timed pairs (default 2)." },
                    "pairs": { "type": "integer", "description": "Number of measured interleaved A/B pairs (default 10)." },
                    "pin_core": { "type": "integer", "description": "Optional Linux CPU core ID for taskset pinning." },
                    "allow_dirty": { "type": "boolean", "description": "Allow starting on a dirty working tree by snapshotting refs/oa/pre-session." }
                },
                "required": ["bench_cmd"]
            }
        },
        {
            "name": "oa_profile",
            "description": "Profile the current working tree and return ranked hotspots with source snippets, allocation sites, bottleneck classification, and optional hotspot deltas vs baseline or best.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "mode": { "type": "string", "enum": ["auto", "python", "node", "perf", "native"] },
                    "top_k": { "type": "integer", "description": "Maximum number of hotspots to return (default 10)." },
                    "diff_from": { "type": "string", "enum": ["baseline", "best"], "description": "Optional baseline or best profile to diff hotspots against." },
                    "skip_build": { "type": "boolean" }
                }
            }
        },
        {
            "name": "oa_evaluate",
            "description": "Evaluate the current working tree: enforces Cheat Guard locks, runs build/tests/equivalence gate, then runs balanced interleaved A/B pairs against best or baseline to return a statistically grounded verdict.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "compare_against": { "type": "string", "enum": ["best", "baseline"], "description": "Reference worktree to interleave against (default best)." },
                    "metric": { "type": "string", "enum": ["wall", "rss"], "description": "Optional metric override for this evaluation (wall or rss)." },
                    "pairs": { "type": "integer" },
                    "warmup": { "type": "integer" },
                    "min_speedup": { "type": "number" },
                    "alpha": { "type": "number" }
                }
            }
        },
        {
            "name": "oa_checkpoint",
            "description": "Keep a verified optimization (advancing refs/oa/best and the shadow worktree) or revert the working tree to refs/oa/best and record the dead end in the experiment ledger.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "action": { "type": "string", "enum": ["keep", "revert"] },
                    "hypothesis": { "type": "string", "description": "Concise description of the optimization attempt." },
                    "dead_end": { "type": "string", "description": "Optional explanation of why a reverted attempt failed." },
                    "force": { "type": "boolean", "description": "Override a non-KEEP_RECOMMENDED verdict when keeping (never overrides Cheat Guard)." }
                },
                "required": ["action", "hypothesis"]
            }
        },
        {
            "name": "oa_ledger",
            "description": "Return the full optimization experiment ledger, Pareto frontier IDs, cumulative speedup, and ruled-out dead ends.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                }
            }
        },
        {
            "name": "oa_report",
            "description": "Generate a PR-ready Markdown or JSON optimization report with host specs, verified SHA-256 equivalence, experiment table, and reproduce command.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "format": { "type": "string", "enum": ["markdown", "json"] }
                }
            }
        }
    ])
}

fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn str_vec_arg(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn handle_tool_call(name: &str, args: &Value) -> Result<String, String> {
    match name {
        "oa_init" => {
            let bench_cmd = str_arg(args, "bench_cmd")
                .ok_or_else(|| "missing required argument 'bench_cmd'".to_string())?;
            let ignore_substrings = str_vec_arg(args, "ignore_substrings");
            let equivalence = if let Some(cmd) = str_arg(args, "oracle_cmd") {
                EquivalencePolicy::Oracle { command: cmd }
            } else if !ignore_substrings.is_empty() {
                EquivalencePolicy::Normalized { ignore_substrings }
            } else {
                EquivalencePolicy::Exact
            };

            let metric = match str_arg(args, "metric") {
                Some(m) => MetricTarget::parse_str(&m)?,
                None => MetricTarget::Wall,
            };

            let params = InitParams {
                path: str_arg(args, "path"),
                bench_cmd,
                test_cmd: str_arg(args, "test_cmd"),
                build_cmd: str_arg(args, "build_cmd"),
                profile_cmd: str_arg(args, "profile_cmd"),
                metric,
                output_files: str_vec_arg(args, "output_files"),
                lock_files: str_vec_arg(args, "lock_files"),
                auto_lock_tests: args
                    .get("auto_lock_tests")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
                equivalence,
                min_speedup: args
                    .get("min_speedup")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.03),
                alpha: args.get("alpha").and_then(|v| v.as_f64()).unwrap_or(0.01),
                warmup: args
                    .get("warmup")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize)
                    .unwrap_or(2),
                pairs: args
                    .get("pairs")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize)
                    .unwrap_or(10),
                pin_core: args
                    .get("pin_core")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize),
                allow_dirty: args
                    .get("allow_dirty")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            };
            let report = init_session(params)?;
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
        }
        "oa_profile" => {
            let params = ProfileParams {
                path: str_arg(args, "path"),
                mode: str_arg(args, "mode"),
                top_k: args
                    .get("top_k")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize),
                diff_from: str_arg(args, "diff_from"),
                skip_build: args
                    .get("skip_build")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            };
            let res = profile_session(params)?;
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }
        "oa_evaluate" => {
            let metric = match str_arg(args, "metric") {
                Some(m) => Some(MetricTarget::parse_str(&m)?),
                None => None,
            };
            let params = EvaluateParams {
                path: str_arg(args, "path"),
                compare_against: str_arg(args, "compare_against"),
                metric,
                pairs: args
                    .get("pairs")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize),
                warmup: args
                    .get("warmup")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize),
                min_speedup: args.get("min_speedup").and_then(|v| v.as_f64()),
                alpha: args.get("alpha").and_then(|v| v.as_f64()),
            };
            let res = evaluate_session(params)?;
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }
        "oa_checkpoint" => {
            let action_str = str_arg(args, "action")
                .ok_or_else(|| "missing required argument 'action'".to_string())?;
            let action = match action_str.as_str() {
                "keep" => CheckpointAction::Keep,
                "revert" => CheckpointAction::Revert,
                other => {
                    return Err(format!(
                        "invalid action '{other}': expected 'keep' or 'revert'"
                    ));
                }
            };
            let hypothesis = str_arg(args, "hypothesis")
                .ok_or_else(|| "missing required argument 'hypothesis'".to_string())?;
            let params = CheckpointParams {
                path: str_arg(args, "path"),
                action,
                hypothesis,
                dead_end: str_arg(args, "dead_end"),
                force: args.get("force").and_then(|v| v.as_bool()).unwrap_or(false),
            };
            let res = checkpoint_session(params)?;
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }
        "oa_ledger" => {
            let path = str_arg(args, "path");
            let res = get_ledger(path.as_deref())?;
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }
        "oa_report" => {
            let path = str_arg(args, "path");
            let format = str_arg(args, "format").unwrap_or_else(|| "markdown".to_string());
            if format.eq_ignore_ascii_case("json") {
                let res = get_ledger(path.as_deref())?;
                serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
            } else {
                render_markdown_report(path.as_deref())
            }
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

/// Runs the stdio JSON-RPC 2.0 MCP server loop until EOF.
pub fn run_stdio_server() -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    while let Some((msg, framing)) =
        read_rpc_message(&mut reader).map_err(|e| format!("MCP read error: {e}"))?
    {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");

        // JSON-RPC notifications have no id and expect no response.
        if id.is_none() {
            continue;
        }
        let req_id = id.unwrap_or(Value::Null);

        let response = match method {
            "initialize" => {
                let client_proto = msg
                    .get("params")
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("2024-11-05");
                json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "protocolVersion": client_proto,
                        "capabilities": {
                            "tools": { "listChanged": false }
                        },
                        "serverInfo": {
                            "name": "oa",
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }
                })
            }
            "ping" => json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {}
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "tools": tool_schemas()
                }
            }),
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
                let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let tool_args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match handle_tool_call(tool_name, &tool_args) {
                    Ok(text) => json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "content": [{ "type": "text", "text": text }],
                            "isError": false
                        }
                    }),
                    Err(err_msg) => json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "content": [{ "type": "text", "text": err_msg }],
                            "isError": true
                        }
                    }),
                }
            }
            other => json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "error": {
                    "code": -32601,
                    "message": format!("method not found: {other}")
                }
            }),
        };

        write_rpc_message(&mut writer, &response, framing)
            .map_err(|e| format!("MCP write error: {e}"))?;
    }

    Ok(())
}
