//! Smoke test: spawn pollard, ask for tool list, assert all 9 tools are present.

use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[tokio::test]
async fn lists_all_nine_tools() {
    let bin = env!("CARGO_BIN_EXE_pollard");
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    // initialize
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0.1"}}}"#;
    stdin.write_all(init.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();

    // Read lines until we find the initialize response (id == 1)
    let mut init_found = false;
    for _ in 0..10 {
        let line = reader
            .next_line()
            .await
            .unwrap()
            .expect("EOF before initialize reply");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        if v.get("id").and_then(serde_json::Value::as_i64) == Some(1) {
            init_found = true;
            break;
        }
    }
    assert!(init_found, "never received initialize reply");

    // tools/list
    let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    stdin.write_all(req.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();

    // Read lines until we find the tools/list response (id == 2), skipping notifications
    let mut tools_resp: Option<serde_json::Value> = None;
    for _ in 0..10 {
        let line = reader
            .next_line()
            .await
            .unwrap()
            .expect("EOF before tools/list reply");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        if v.get("id").and_then(serde_json::Value::as_i64) == Some(2) {
            tools_resp = Some(v);
            break;
        }
    }
    let v = tools_resp.expect("never received tools/list reply");
    let tools = v["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    for expected in &[
        "load_profile",
        "unload_profile",
        "list_profiles",
        "describe_profile",
        "top_functions",
        "call_tree",
        "stacks_containing",
        "source_for_function",
        "asm_for_function",
    ] {
        assert!(names.contains(expected), "missing tool: {}", expected);
    }

    child.kill().await.unwrap();
}
