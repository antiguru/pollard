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

        let mut server = Self {
            child,
            stdin,
            reader,
        };

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
            let v: serde_json::Value =
                serde_json::from_str(&line).expect("invalid JSON from server");
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
    let resp = srv
        .call_tool(1, "load_profile", serde_json::json!({ "path": path }))
        .await;

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
        .call_tool(
            2,
            "unload_profile",
            serde_json::json!({ "profile_id": pid }),
        )
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

    let resp = srv
        .call_tool(2, "list_profiles", serde_json::json!({}))
        .await;
    let profiles = resp["result"]["structuredContent"]["profiles"]
        .as_array()
        .expect("profiles missing");

    let found = profiles
        .iter()
        .any(|p| p["profile_id"].as_str() == Some(&pid));
    assert!(
        found,
        "loaded profile not found in list_profiles; list={profiles:?}"
    );

    srv.kill().await;
}

#[tokio::test]
async fn describe_profile_returns_processes() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "describe_profile",
            serde_json::json!({ "profile_id": pid }),
        )
        .await;
    let sc = &resp["result"]["structuredContent"];
    let procs = sc["processes"].as_array().expect("processes missing");
    assert!(!procs.is_empty(), "processes is empty");

    // Main thread should be present.
    let threads = procs[0]["threads"].as_array().expect("threads missing");
    let has_main = threads.iter().any(|t| t["name"].as_str() == Some("Main"));
    assert!(
        has_main,
        "thread 'Main' not found in describe_profile response"
    );

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
    assert!(
        tree.is_object(),
        "call_tree tree should be an object; got {tree}"
    );
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
    assert_eq!(
        stacks.len(),
        2,
        "expected 2 distinct stacks containing 'alloc'; got {stacks:?}"
    );

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

/// `asm_for_function` with a synthetic profile (no native addresses/libs) returns
/// an error because the function cannot be disassembled.
#[tokio::test]
async fn asm_for_function_returns_error_for_synthetic_profile() {
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

    // Synthetic fixtures have no native addresses, so the function cannot be
    // located for disassembly — expect function_not_found or internal error.
    let err = &resp["error"];
    assert!(err.is_object(), "expected a JSON-RPC error; got {resp}");

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
    let threads = data["available_threads"]
        .as_array()
        .expect("available_threads missing");
    assert!(!threads.is_empty(), "available_threads should not be empty");

    // Should contain the "Main" thread from the fixture.
    let has_main = threads.iter().any(|t| t["name"].as_str() == Some("Main"));
    assert!(
        has_main,
        "expected 'Main' in available_threads; got {threads:?}"
    );

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

/// `time_range` slices the profile at the sample level. linear_chain.json
/// has 100 samples on a 1ms cadence (timestamps 0..99) all on the leaf
/// `d`. A `[10, 19]` slice must yield exactly 10 samples there.
#[tokio::test]
async fn top_functions_time_range_slices_sample_count() {
    let mut srv = Server::spawn().await;
    let path = fixture("linear_chain.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "top_functions",
            serde_json::json!({ "profile_id": pid, "time_range": [10.0, 19.0] }),
        )
        .await;
    let sc = &resp["result"]["structuredContent"];
    let total = sc["total_samples"].as_u64().expect("total_samples missing");
    assert_eq!(
        total, 10,
        "expected 10 samples in [10,19] window; got {total} (full response={resp})"
    );

    let leaf = sc["functions"]
        .as_array()
        .expect("functions missing")
        .iter()
        .find(|f| f["function"].as_str() == Some("d"))
        .expect("'d' must appear in the slice");
    assert_eq!(leaf["self_samples"].as_u64(), Some(10));

    srv.kill().await;
}

#[tokio::test]
async fn top_functions_time_range_outside_profile_returns_out_of_bounds() {
    let mut srv = Server::spawn().await;
    let path = fixture("linear_chain.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "top_functions",
            serde_json::json!({ "profile_id": pid, "time_range": [5_000.0, 6_000.0] }),
        )
        .await;
    let data = &resp["error"]["data"];
    assert_eq!(
        data["error"].as_str(),
        Some("out_of_bounds"),
        "expected out_of_bounds; full response={resp}"
    );
    assert_eq!(data["original_range"][0].as_f64(), Some(5_000.0));
    assert_eq!(data["original_range"][1].as_f64(), Some(6_000.0));

    srv.kill().await;
}

/// Unknown `sort_by` value used to fall through to the default
/// (`SortBy::SelfTime`). It must now hard-error so a typo doesn't
/// silently rank by something the caller didn't ask for.
#[tokio::test]
async fn top_functions_unknown_sort_by_returns_invalid_value() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "top_functions",
            serde_json::json!({ "profile_id": pid, "sort_by": "selfTime" }),
        )
        .await;
    let data = &resp["error"]["data"];
    assert_eq!(
        data["error"].as_str(),
        Some("invalid_value"),
        "expected invalid_value; full response={resp}"
    );
    assert_eq!(data["field"].as_str(), Some("sort_by"));
    assert_eq!(data["value"].as_str(), Some("selfTime"));
    let accepted = data["accepted"].as_array().expect("accepted list missing");
    let names: Vec<&str> = accepted.iter().filter_map(|v| v.as_str()).collect();
    for expected in ["self", "total", "descendants"] {
        assert!(
            names.contains(&expected),
            "{expected} missing from accepted={names:?}"
        );
    }

    srv.kill().await;
}

#[tokio::test]
async fn top_groups_unknown_group_by_returns_invalid_value() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "top_groups",
            serde_json::json!({ "profile_id": pid, "group_by": "lib" }),
        )
        .await;
    let data = &resp["error"]["data"];
    assert_eq!(data["error"].as_str(), Some("invalid_value"));
    assert_eq!(data["field"].as_str(), Some("group_by"));

    srv.kill().await;
}

/// `pid:` is an opt-in to integer matching; a malformed payload used
/// to silently fall back to a literal name match (`pid:abc` would
/// look for a process literally named `pid:abc`). Reject it instead.
#[tokio::test]
async fn process_with_malformed_pid_prefix_returns_invalid_value() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "top_functions",
            serde_json::json!({ "profile_id": pid, "process": "pid:abc" }),
        )
        .await;
    let data = &resp["error"]["data"];
    assert_eq!(
        data["error"].as_str(),
        Some("invalid_value"),
        "expected invalid_value; full response={resp}"
    );
    assert_eq!(data["field"].as_str(), Some("process"));
    assert_eq!(data["value"].as_str(), Some("pid:abc"));

    srv.kill().await;
}

#[tokio::test]
async fn thread_with_malformed_tid_prefix_returns_invalid_value() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "top_functions",
            serde_json::json!({ "profile_id": pid, "thread": "tid:abc" }),
        )
        .await;
    let data = &resp["error"]["data"];
    assert_eq!(data["error"].as_str(), Some("invalid_value"));
    assert_eq!(data["field"].as_str(), Some("thread"));
    assert_eq!(data["value"].as_str(), Some("tid:abc"));

    srv.kill().await;
}

/// `pid:NNN.M` (well-formed sub-pid) must still parse — only the
/// malformed `pid:NNN.X.Y` variant is rejected.
#[tokio::test]
async fn process_with_extra_dot_in_pid_prefix_returns_invalid_value() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "top_functions",
            serde_json::json!({ "profile_id": pid, "process": "pid:1.2.3" }),
        )
        .await;
    let data = &resp["error"]["data"];
    assert_eq!(data["error"].as_str(), Some("invalid_value"));
    assert_eq!(data["field"].as_str(), Some("process"));

    srv.kill().await;
}

/// Empty `function` on tools that require a function pattern used to
/// silently match every frame because `"".contains("")` is always
/// true — `stacks_containing` would return the entire profile keyed
/// as if every frame matched. Reject it as `invalid_value` so the
/// caller sees the accepted-pattern syntax.
#[tokio::test]
async fn stacks_containing_empty_function_returns_invalid_value() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "stacks_containing",
            serde_json::json!({ "profile_id": pid, "function": "" }),
        )
        .await;
    let data = &resp["error"]["data"];
    assert_eq!(
        data["error"].as_str(),
        Some("invalid_value"),
        "expected invalid_value; full response={resp}"
    );
    assert_eq!(data["field"].as_str(), Some("function"));
    assert_eq!(data["value"].as_str(), Some(""));
    let accepted = data["accepted"].as_array().expect("accepted list missing");
    assert!(
        !accepted.is_empty(),
        "accepted list should describe the syntax"
    );

    srv.kill().await;
}

/// `re:` with no body still produces an empty regex that matches
/// every position; same rejection path.
#[tokio::test]
async fn stacks_containing_empty_regex_body_returns_invalid_value() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "stacks_containing",
            serde_json::json!({ "profile_id": pid, "function": "re:" }),
        )
        .await;
    let data = &resp["error"]["data"];
    assert_eq!(data["error"].as_str(), Some("invalid_value"));
    assert_eq!(data["field"].as_str(), Some("function"));

    srv.kill().await;
}

/// `call_tree.root_function` is optional but only meaningful when
/// narrowing — empty must not silently defeat the narrowing.
#[tokio::test]
async fn call_tree_empty_root_function_returns_invalid_value() {
    let mut srv = Server::spawn().await;
    let path = fixture("linear_chain.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "call_tree",
            serde_json::json!({ "profile_id": pid, "root_function": "" }),
        )
        .await;
    let data = &resp["error"]["data"];
    assert_eq!(data["error"].as_str(), Some("invalid_value"));
    assert_eq!(data["field"].as_str(), Some("root_function"));

    srv.kill().await;
}

/// `list_profiles` must mark derived views with `base_profile_id`
/// pointing back to the base profile, so callers can spot views
/// vs. profiles loaded from disk. The base entry itself must omit
/// the field (or carry `null`) — only views set it.
#[tokio::test]
async fn list_profiles_reports_view_base_id() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let base_id = srv.load_fixture(1, &path).await;

    // Create a default-transforms view of the loaded profile.
    let view_resp = srv
        .call_tool(
            2,
            "create_view",
            serde_json::json!({ "profile_id": base_id }),
        )
        .await;
    let view_id = view_resp["result"]["structuredContent"]["profile_id"]
        .as_str()
        .unwrap_or_else(|| panic!("create_view did not return a profile_id; response: {view_resp}"))
        .to_owned();
    assert_ne!(view_id, base_id, "view id must differ from base id");

    let resp = srv
        .call_tool(3, "list_profiles", serde_json::json!({}))
        .await;
    let profiles = resp["result"]["structuredContent"]["profiles"]
        .as_array()
        .expect("profiles missing");

    let view_entry = profiles
        .iter()
        .find(|p| p["profile_id"].as_str() == Some(&view_id))
        .unwrap_or_else(|| panic!("view not found in list_profiles; list={profiles:?}"));
    assert_eq!(
        view_entry["base_profile_id"].as_str(),
        Some(base_id.as_str()),
        "view entry should report base_profile_id={base_id}; got {view_entry:?}"
    );

    let base_entry = profiles
        .iter()
        .find(|p| p["profile_id"].as_str() == Some(&base_id))
        .unwrap_or_else(|| panic!("base not found in list_profiles; list={profiles:?}"));
    // `skip_serializing_if = "Option::is_none"` means the field is absent
    // (not `null`) for non-view sessions. Accept either to stay robust to
    // future serialization tweaks.
    let base_field = base_entry.get("base_profile_id");
    assert!(
        base_field.is_none() || base_field.is_some_and(serde_json::Value::is_null),
        "base entry must not report a base_profile_id; got {base_entry:?}"
    );

    srv.kill().await;
}

/// `create_view` must surface per-rule diagnostic counts so users can
/// spot a typo in `hide_frames` / `hide_modules` / `rename` without
/// running downstream tools and noticing nothing changed.
/// `describe_view` must return the same counts plus the composed
/// transform shape and the immediate parent base id, so the stats
/// stay queryable after creation.
#[tokio::test]
async fn create_and_describe_view_report_rule_stats() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let base_id = srv.load_fixture(1, &path).await;

    // One real-pattern rule (`hot` is a function in two_functions.json) and
    // one obvious typo. The first must report nonzero matches; the second
    // must come back with zero — that's the typo signal.
    let view_resp = srv
        .call_tool(
            2,
            "create_view",
            serde_json::json!({
                "profile_id": base_id,
                "hide_frames": ["hot", "definitely_not_a_real_function"],
            }),
        )
        .await;
    let sc = &view_resp["result"]["structuredContent"];
    let view_id = sc["profile_id"]
        .as_str()
        .unwrap_or_else(|| panic!("create_view missing profile_id; resp={view_resp}"))
        .to_owned();
    let stats = sc["rule_stats"]
        .as_array()
        .expect("rule_stats missing on create_view response");
    assert_eq!(stats.len(), 2, "expected one stat per rule; got {stats:?}");

    let real = stats
        .iter()
        .find(|s| s["pattern"].as_str() == Some("hot"))
        .expect("hot rule missing");
    assert!(
        real["frames_matched"].as_u64().unwrap_or(0) > 0,
        "real pattern should match at least one frame: {real}"
    );
    let typo = stats
        .iter()
        .find(|s| s["pattern"].as_str() == Some("definitely_not_a_real_function"))
        .expect("typo rule missing");
    assert_eq!(
        typo["frames_matched"].as_u64(),
        Some(0),
        "typo pattern should report zero matches: {typo}"
    );
    assert!(sc["total_base_samples"].as_u64().unwrap_or(0) > 0);

    // describe_view should round-trip the same stats and surface the
    // composed transform shape.
    let desc_resp = srv
        .call_tool(
            3,
            "describe_view",
            serde_json::json!({ "profile_id": view_id }),
        )
        .await;
    let dsc = &desc_resp["result"]["structuredContent"];
    assert_eq!(dsc["base_profile_id"].as_str(), Some(base_id.as_str()));
    let hide = dsc["transforms"]["hide_frames"]
        .as_array()
        .expect("hide_frames missing");
    let patterns: Vec<&str> = hide.iter().filter_map(|v| v.as_str()).collect();
    assert!(patterns.contains(&"hot"));
    assert!(patterns.contains(&"definitely_not_a_real_function"));
    let desc_stats = dsc["rule_stats"]
        .as_array()
        .expect("rule_stats missing on describe_view");
    assert_eq!(desc_stats.len(), stats.len());

    // describe_view on a non-view profile must reject with invalid_value
    // — symmetric with describe_profile only working on loaded profiles.
    let bad = srv
        .call_tool(
            4,
            "describe_view",
            serde_json::json!({ "profile_id": base_id }),
        )
        .await;
    assert_eq!(
        bad["error"]["data"]["error"].as_str(),
        Some("invalid_value"),
        "describe_view on a non-view should fail; got {bad}"
    );

    srv.kill().await;
}

/// `create_view` accepts `process` / `thread` / `time_range` scope
/// arguments. The scope round-trips on `describe_view`, and per-call
/// filters that conflict with a pinned scope are rejected with
/// `invalid_value` — the sub-slice contract from issue #90.
#[tokio::test]
async fn create_view_pins_scope_and_rejects_widening_per_call_filter() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let base_id = srv.load_fixture(1, &path).await;

    // Pin a thread scope on the view. tid 1 is the only thread in the
    // fixture, so the scope is a no-op for results — what we care about
    // is that it round-trips and gates per-call filters.
    let view_resp = srv
        .call_tool(
            2,
            "create_view",
            serde_json::json!({
                "profile_id": base_id,
                "thread": "tid:1",
            }),
        )
        .await;
    let view_id = view_resp["result"]["structuredContent"]["profile_id"]
        .as_str()
        .unwrap_or_else(|| panic!("create_view missing profile_id; resp={view_resp}"))
        .to_owned();

    // describe_view exposes the pinned scope.
    let desc_resp = srv
        .call_tool(
            3,
            "describe_view",
            serde_json::json!({ "profile_id": view_id }),
        )
        .await;
    let dsc = &desc_resp["result"]["structuredContent"];
    assert_eq!(
        dsc["scope"]["thread"].as_str(),
        Some("tid:1"),
        "scope.thread should round-trip; got {dsc}"
    );

    // A per-call filter that picks a *different* thread must be
    // rejected with invalid_value (sub-slice contract).
    let bad = srv
        .call_tool(
            4,
            "top_functions",
            serde_json::json!({ "profile_id": view_id, "thread": "tid:2" }),
        )
        .await;
    assert_eq!(
        bad["error"]["data"]["error"].as_str(),
        Some("invalid_value"),
        "conflicting per-call thread should be rejected; got {bad}"
    );
    assert_eq!(
        bad["error"]["data"]["field"].as_str(),
        Some("thread"),
        "expected field=thread; got {bad}"
    );

    // A per-call filter that matches the scope's thread is accepted —
    // sub-slice (equality counts) — and returns aggregated results.
    let ok = srv
        .call_tool(
            5,
            "top_functions",
            serde_json::json!({ "profile_id": view_id, "thread": "tid:1" }),
        )
        .await;
    let sc = &ok["result"]["structuredContent"];
    assert!(
        sc["functions"].as_array().is_some_and(|a| !a.is_empty()),
        "matching per-call thread should aggregate; got {ok}"
    );

    srv.kill().await;
}

/// The genuinely-optional `filter` field on `top_functions` must keep
/// treating empty as "no filter" — that's what callers mean when they
/// "leave blank". The rejection only fires for required / narrowing
/// pattern arguments.
#[tokio::test]
async fn top_functions_empty_filter_is_treated_as_no_filter() {
    let mut srv = Server::spawn().await;
    let path = fixture("two_functions.json");
    let pid = srv.load_fixture(1, &path).await;

    let resp = srv
        .call_tool(
            2,
            "top_functions",
            serde_json::json!({ "profile_id": pid, "filter": "" }),
        )
        .await;
    let sc = &resp["result"]["structuredContent"];
    let fns = sc["functions"].as_array().expect("functions missing");
    assert!(
        !fns.is_empty(),
        "expected unfiltered results; full response={resp}"
    );

    srv.kill().await;
}
