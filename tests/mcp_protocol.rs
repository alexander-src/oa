use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

static MCP_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

fn python_bin() -> String {
    for bin in ["python3", "python"] {
        if matches!(
            Command::new(bin).arg("--version").output(),
            Ok(out) if out.status.success()
        ) {
            return bin.to_string();
        }
    }
    panic!("python3 or python is required to run MCP tests");
}

fn oa_bin() -> &'static str {
    env!("CARGO_BIN_EXE_oa")
}

#[test]
fn mcp_ndjson_roundtrip_and_tools() {
    let py = python_bin();
    let seq = MCP_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("oa_mcp_test_{}_{}", std::process::id(), seq));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    for args in [
        &["init"][..],
        &["config", "user.name", "Test"][..],
        &["config", "user.email", "test@example.com"][..],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap()
            .status
            .success());
    }
    fs::write(
        dir.join("app.py"),
        "print(sum(i * i for i in range(5000)))\n",
    )
    .unwrap();
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(&dir)
        .output()
        .unwrap()
        .status
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&dir)
        .output()
        .unwrap()
        .status
        .success());

    let mut child = Command::new(oa_bin())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn oa mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    fn send_recv(stdin: &mut impl Write, reader: &mut impl BufRead, req: Value) -> Value {
        writeln!(stdin, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).expect("valid JSON-RPC response")
    }

    // 1. initialize
    let init_resp = send_recv(
        &mut stdin,
        &mut reader,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2024-11-05" }
        }),
    );
    assert_eq!(init_resp["result"]["serverInfo"]["name"], "oa");

    // Notification (no response expected)
    writeln!(
        stdin,
        "{}",
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
    )
    .unwrap();
    stdin.flush().unwrap();

    // 2. tools/list
    let list_resp = send_recv(
        &mut stdin,
        &mut reader,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }),
    );
    let tools = list_resp["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec![
            "oa_init",
            "oa_profile",
            "oa_evaluate",
            "oa_checkpoint",
            "oa_ledger",
            "oa_report",
            "oa_status",
            "oa_compare"
        ]
    );

    // 3. Call oa_init via MCP
    let init_tool = send_recv(
        &mut stdin,
        &mut reader,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "oa_init",
                "arguments": {
                    "path": dir.to_string_lossy(),
                    "bench_cmd": format!("{py} app.py"),
                    "warmup": 1,
                    "pairs": 3
                }
            }
        }),
    );
    assert_eq!(init_tool["result"]["isError"], false);
    let init_text = init_tool["result"]["content"][0]["text"].as_str().unwrap();
    let init_json: Value = serde_json::from_str(init_text).unwrap();
    assert_eq!(
        init_json["profile"]["profiler"],
        "python:cProfile+tracemalloc"
    );

    // 4. Call oa_ledger & oa_report via MCP
    let ledger_tool = send_recv(
        &mut stdin,
        &mut reader,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "oa_ledger",
                "arguments": { "path": dir.to_string_lossy() }
            }
        }),
    );
    assert_eq!(ledger_tool["result"]["isError"], false);

    let report_tool = send_recv(
        &mut stdin,
        &mut reader,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "oa_report",
                "arguments": { "path": dir.to_string_lossy(), "format": "markdown" }
            }
        }),
    );
    assert_eq!(report_tool["result"]["isError"], false);
    let md = report_tool["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(md.contains("# oa Optimization Report"));

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn mcp_content_length_framing() {
    let mut child = Command::new(oa_bin())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn oa mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "ping"
    }))
    .unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    stdin.flush().unwrap();

    let mut header = String::new();
    reader.read_line(&mut header).unwrap();
    let len: usize = header
        .trim()
        .strip_prefix("Content-Length:")
        .expect("Content-Length response header")
        .trim()
        .parse()
        .unwrap();
    let mut blank = String::new();
    reader.read_line(&mut blank).unwrap();
    let mut resp_buf = vec![0u8; len];
    reader.read_exact(&mut resp_buf).unwrap();
    let resp: Value = serde_json::from_slice(&resp_buf).unwrap();
    assert_eq!(resp["id"], 42);

    drop(stdin);
    assert!(child.wait().unwrap().success());
}
