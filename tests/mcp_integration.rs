//! MCP integration tests: one test per tool (happy-path) + structured error-envelope tests.
//!
//! Each test spawns the pollard binary, performs initialize, then exercises a tool
//! over JSON-RPC. The robustifier loop skips notifications/out-of-order messages
//! until it finds the response with the matching `id`.

use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

// ---------------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------------

struct Server {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}

impl Server {
    async fn spawn() -> Self {
        let bin = env!("CARGO_BIN_EXE_pollard");
        let mut child = Command::new(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("failed to spawn pollard");

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout).lines();

        let mut server = Self { child, stdin, reader };

        // Send initialize and wait for the response before returning.
        let init = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"integration","version":"0.1"}}}"#;
        server.send(init).await;
        server.recv(0).await; // discard; just need to sync

        server
    }

    async fn send(&mut self, msg: &str) {
        self.stdin.write_all(msg.as_bytes()).await.unwrap();
        self.stdin.write_all(b"\n").await.unwrap();
    }

    /// Read lines until we find the response with the given `id`. Returns the parsed JSON.
    async fn recv(&mut self, id: i64) -> serde_json::Value {
        for _ in 0..50 {
            let line = self
                .reader
                .next_line()
                .await
                .expect("IO error reading from server")
                .expect("EOF before expected response");
            let v: serde_json::Value = serde_json::from_str(&line).expect("invalid JSON from server");
            if v.get("id").and_then(serde_json::Value::as_i64) == Some(id) {
                return v;
            }
        }
        panic!("never received response with id={id}");
    }

    async fn call_tool(
        &mut self,
        id: i64,
        tool: &str,
        args: serde_json::Value,
    ) -> serde_json::Value {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args }
        });
        self.send(&req.to_string()).await;
        self.recv(id).await
    }

    /// Call load_profile and return the profile_id from structuredContent.
    async fn load_fixture(&mut self, id: i64, fixture_path: &str) -> String {
        let resp = self
            .call_tool(
                id,
                "load_profile",
                serde_json::json!({ "path": fixture_path }),
            )
            .await;
        resp["result"]["structuredContent"]["profile_id"]
            .as_str()
            .unwrap_or_else(|| panic!("load_profile did not return a profile_id; response: {resp}"))
            .to_owned()
    }

    async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

fn fixture(name: &str) -> String {
    let manifest = env!("CARGO_MANIFEST_DIR");
    format!("{manifest}/tests/fixtures/{name}")
}

// ---------------------------------------------------------------------------
// Happy-path tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn load_profile_returns_id_and_description() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let resp = srv.call_tool(1, "load_profile", serde_json::json!({ "path": path })).await;

    let sc = &resp["result"]["structuredContent"];
    let pid = sc["profile_id"].as_str().expect("profile_id missing");
    assert!(!pid.is_empty(), "profile_id is empty");

    let desc = &sc["description"];
    assert_eq!(desc["total_samples"].as_u64(), Some(100));
    assert_eq!(desc["name"].as_str(), Some("two_functions"));

    srv.kill().await;
}

#[tokio::test]
async fn unload_profile_frees() {
    let mut srv = Server::spawn().await;
    let path = fixture("minimal_profile.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(2, "unload_profile", serde_json::json!({ "profile_id": pid }))
        .await;
    let freed = resp["result"]["structuredContent"]["freed"]
        .as_bool()
        .expect("freed missing");
    assert!(freed, "expected freed=true after unloading");

    srv.kill().await;
}

#[tokio::test]
async fn list_profiles_after_load() {
    let mut srv = Server::spawn().await;
    let path = fixture("minimal_profile.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv.call_tool(2, "list_profiles", serde_json::json!({})).await;
    let profiles = resp["result"]["structuredContent"]["profiles"]
        .as_array()
        .expect("profiles missing");

    let found = profiles.iter().any(|p| p["profile_id"].as_str() == Some(&pid));
    assert!(found, "loaded profile not found in list_profiles; list={profiles:?}");

    srv.kill().await;
}

#[tokio::test]
async fn describe_profile_returns_processes() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(2, "describe_profile", serde_json::json!({ "profile_id": pid }))
        .await;
    let sc = &resp["result"]["structuredContent"];
    let procs = sc["processes"].as_array().expect("processes missing");
    assert!(!procs.is_empty(), "processes is empty");

    // Main thread should be present.
    let threads = procs[0]["threads"].as_array().expect("threads missing");
    let has_main = threads.iter().any(|t| t["name"].as_str() == Some("Main"));
    assert!(has_main, "thread 'Main' not found in describe_profile response");

    srv.kill().await;
}

#[tokio::test]
async fn top_functions_returns_ranked_list() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(2, "top_functions", serde_json::json!({ "profile_id": pid }))
        .await;
    let sc = &resp["result"]["structuredContent"];
    let fns = sc["functions"].as_array().expect("functions missing");
    assert!(!fns.is_empty(), "functions is empty");

    // "hot" should be first (90 samples out of 100).
    assert_eq!(
        fns[0]["function"].as_str(),
        Some("hot"),
        "expected 'hot' to be first; got: {:?}",
        fns[0]
    );

    srv.kill().await;
}

#[tokio::test]
async fn call_tree_returns_tree() {
    let mut srv = Server::spawn().await;
    let path = fixture("linear_chain.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(2, "call_tree", serde_json::json!({ "profile_id": pid }))
        .await;
    let sc = &resp["result"]["structuredContent"];

    // linear_chain.json has a→b→c→d with 100 samples.
    // After linear-chain compression the root node is "a" with chain ["b","c","d"].
    let total = sc["total_samples"].as_u64().expect("total_samples missing");
    assert_eq!(total, 100, "expected 100 total samples");

    // tree is a single root node (Option<Node> serialized as the node or null).
    let tree = &sc["tree"];
    assert!(tree.is_object(), "call_tree tree should be an object; got {tree}");
    assert_eq!(
        tree["function"].as_str(),
        Some("a"),
        "expected root function 'a'; got {tree}"
    );

    srv.kill().await;
}

#[tokio::test]
async fn stacks_containing_returns_distinct_stacks() {
    let mut srv = Server::spawn().await;
    let path = fixture("stacks_containing.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "stacks_containing",
            serde_json::json!({ "profile_id": pid, "function": "alloc" }),
        )
        .await;
    let sc = &resp["result"]["structuredContent"];

    // Fixture has alloc_buf and alloc_str — two distinct stacks.
    let stacks = sc["stacks"].as_array().expect("stacks missing");
    assert_eq!(stacks.len(), 2, "expected 2 distinct stacks containing 'alloc'; got {stacks:?}");

    // Both stacks should have a frame with matched=true.
    for stack in stacks {
        let frames = stack["frames"].as_array().expect("frames missing");
        let has_matched = frames.iter().any(|f| f["matched"].as_bool() == Some(true));
        assert!(has_matched, "no matched frame in stack: {stack:?}");
    }

    srv.kill().await;
}

/// `source_for_function` happy path requires an absolute path in the fixture.
/// The source_attribution.json fixture uses a relative path ("src/server.rs"),
/// so it can't be read from disk. We test the error path instead:
/// a function that doesn't exist → function_not_found.
#[tokio::test]
async fn source_for_function_with_unknown_function_returns_function_not_found() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "source_for_function",
            serde_json::json!({ "profile_id": pid, "function": "does_not_exist" }),
        )
        .await;

    // Should be a JSON-RPC error with data.error == "function_not_found".
    let data = &resp["error"]["data"];
    assert_eq!(
        data["error"].as_str(),
        Some("function_not_found"),
        "expected function_not_found; response={resp}"
    );

    srv.kill().await;
}

/// `asm_for_function` is a v1 stub — always returns an internal error.
#[tokio::test]
async fn asm_for_function_returns_stub_error() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "asm_for_function",
            serde_json::json!({ "profile_id": pid, "function": "hot" }),
        )
        .await;

    // The stub returns ToolError::Internal; should surface as a JSON-RPC error.
    let err = &resp["error"];
    assert!(err.is_object(), "expected a JSON-RPC error; got {resp}");
    let msg = err["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("not") || msg.contains("stub") || msg.contains("implement"),
        "unexpected error message: {msg}; full response={resp}"
    );

    srv.kill().await;
}

// ---------------------------------------------------------------------------
// Error-envelope tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn load_profile_with_missing_path_returns_file_not_found() {
    let mut srv = Server::spawn().await;

    let resp = srv
        .call_tool(
            1,
            "load_profile",
            serde_json::json!({ "path": "/no/such/file.json" }),
        )
        .await;

    // rmcp wraps ToolError in a JSON-RPC error envelope: response.error.data contains the ToolError JSON.
    let data = &resp["error"]["data"];
    assert_eq!(
        data["error"].as_str(),
        Some("file_not_found"),
        "expected file_not_found in error.data; full response={resp}"
    );
    assert!(
        data["path"].as_str().is_some(),
        "expected path field in error.data; got {data}"
    );

    srv.kill().await;
}

#[tokio::test]
async fn top_functions_with_unknown_thread_returns_thread_not_found() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "top_functions",
            serde_json::json!({ "profile_id": pid, "thread": "Nope" }),
        )
        .await;

    let data = &resp["error"]["data"];
    assert_eq!(
        data["error"].as_str(),
        Some("thread_not_found"),
        "expected thread_not_found; full response={resp}"
    );

    // available_threads should be populated.
    let threads = data["available_threads"].as_array().expect("available_threads missing");
    assert!(!threads.is_empty(), "available_threads should not be empty");

    // Should contain the "Main" thread from the fixture.
    let has_main = threads.iter().any(|t| t["name"].as_str() == Some("Main"));
    assert!(has_main, "expected 'Main' in available_threads; got {threads:?}");

    srv.kill().await;
}

#[tokio::test]
async fn describe_profile_with_unknown_id_returns_profile_not_found() {
    let mut srv = Server::spawn().await;

    let resp = srv
        .call_tool(
            1,
            "describe_profile",
            serde_json::json!({ "profile_id": "deadbeef" }),
        )
        .await;

    let data = &resp["error"]["data"];
    assert_eq!(
        data["error"].as_str(),
        Some("profile_not_found"),
        "expected profile_not_found; full response={resp}"
    );
    assert_eq!(
        data["profile_id"].as_str(),
        Some("deadbeef"),
        "expected profile_id echoed back; got {data}"
    );

    srv.kill().await;
}
