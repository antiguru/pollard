//! Insta snapshot tests for `describe_profile` and `top_functions`.
//!
//! The fixture `tests/fixtures/tiny.json.gz` is a real samply-recorded profile
//! of `rustfmt --version`. Sample counts and profile_id are redacted in
//! snapshots because they are non-deterministic across machines/runs.

use insta::assert_json_snapshot;

const FIXTURE: &str = "tests/fixtures/tiny.json.gz";

#[tokio::test]
async fn describe_snapshot() {
    let registry = pollard::registry::SessionRegistry::new(2);
    let (id, _evicted) = registry
        .load(std::path::Path::new(FIXTURE), Some("tiny"))
        .await
        .unwrap();
    let session = registry.get(&id).await.unwrap();
    let desc = pollard::query::describe::describe(
        session.profile(),
        session.id(),
        session.name(),
        session.path().display().to_string().as_str(),
        session.unsymbolicated_pct(),
    );

    assert_json_snapshot!(desc, {
        ".profile_id" => "[id]",
        ".path" => "[path]",
        ".duration_ms" => "[duration]",
        ".total_samples" => "[total]",
        ".unsymbolicated_pct" => "[pct]",
        ".processes[].threads[].samples" => "[n]",
        ".processes[].threads[].duration_ms" => "[dur]",
    });
}

#[tokio::test]
async fn top_functions_snapshot() {
    let registry = pollard::registry::SessionRegistry::new(2);
    let (id, _evicted) = registry
        .load(std::path::Path::new(FIXTURE), Some("tiny"))
        .await
        .unwrap();
    let session = registry.get(&id).await.unwrap();
    let result = pollard::query::top_functions::top_functions(
        session.profile(),
        &pollard::query::top_functions::Args::default(),
    )
    .unwrap();

    assert_json_snapshot!(result, {
        ".total_samples" => "[total]",
        ".functions[].self_samples" => "[n]",
        ".functions[].self_pct" => "[pct]",
        ".functions[].total_samples" => "[n]",
        ".functions[].total_pct" => "[pct]",
    });
}
