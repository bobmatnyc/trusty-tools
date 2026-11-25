//! `tm memory` tool verbs against a fake daemon on a socket (#11625).
//!
//! Why: the verbs exist so a session with a dead MCP connection still reaches
//! these tools. What decides that is observable only from the binary: the
//! method it sends, the argument keys, the palace, and a non-zero exit when
//! nothing listens.
//! What: one test per verb through [`check`], which runs the spawned `tm`
//! against the stub from `memory_verbs_socket` and asserts the recorded call
//! and the `--json` envelope; plus one daemon-down test.
//! Test: this file IS the test module.

use serde_json::{Value, json};

use crate::memory_verbs_socket::{only_call, run_tm, serve};

/// Run `tm memory <args> --json` against a stub answering `body`, and assert
/// the one call carried `method` and exactly `keys`; return the envelope.
async fn check(args: &[&str], body: Value, method: &str, keys: &[&str]) -> Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("memory.sock");
    let (_stop, calls) = serve(&socket, body).await;
    let mut argv = args.to_vec();
    argv.push("--json");
    let out = run_tm(&socket, Some("session-palace"), &argv).await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (sent, params) = only_call(&calls);
    assert_eq!(sent, method);
    let mut got: Vec<&str> = params
        .as_object()
        .expect("params object")
        .keys()
        .map(String::as_str)
        .collect();
    got.sort_unstable();
    let mut want = keys.to_vec();
    want.sort_unstable();
    assert_eq!(got, want, "argument keys sent for `{method}`: {params}");

    let envelope: Value = serde_json::from_slice(&out.stdout).expect("parseable JSON");
    assert_eq!(envelope["socket"], json!(socket.display().to_string()));
    envelope
}

/// A body holding `n` rows under `key`.
fn rows(key: &str, n: usize) -> Value {
    let rows: Vec<Value> = (0..n)
        .map(|i| json!({ "drawer_id": format!("d{i}"), "content": "c" }))
        .collect();
    json!({ "palace": "session-palace", key: rows })
}

#[tokio::test(flavor = "multi_thread")]
async fn recall_deep_sends_memory_recall_deep() {
    let env = check(
        &["recall-deep", "q", "--top-k", "2"],
        rows("results", 2),
        "memory_recall_deep",
        &["palace", "query", "top_k"],
    )
    .await;
    assert_eq!(env["verb"], json!("recall-deep"));
    assert_eq!(env["palace"], json!("session-palace"));
    assert_eq!(env["count"], json!(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn recall_all_sends_no_palace() {
    let env = check(
        &["recall-all", "q", "--scope", "all"],
        rows("results", 1),
        "memory_recall_all",
        &["q", "scope"],
    )
    .await;
    assert_eq!(env["palace"], Value::Null, "a cross-palace tool names none");
    assert_eq!(env["count"], json!(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn list_sends_memory_list() {
    let env = check(
        &["list", "--tag", "t", "--palace", "other"],
        rows("drawers", 3),
        "memory_list",
        &["palace", "tag"],
    )
    .await;
    assert_eq!(
        env["palace"],
        json!("other"),
        "--palace outranks the session"
    );
    assert_eq!(env["count"], json!(3));
}

#[tokio::test(flavor = "multi_thread")]
async fn prompt_context_sends_get_prompt_context() {
    let env = check(
        &["prompt-context", "tga"],
        json!("## Aliases\n- tga"),
        "get_prompt_context",
        &["query"],
    )
    .await;
    assert_eq!(env["result"], json!("## Aliases\n- tga"));
    assert_eq!(env["count"], Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn task_add_sends_task_add() {
    let env = check(
        &["task", "add", "ship it", "--tag", "a"],
        json!({ "status": "stored", "drawer_id": "d1" }),
        "task_add",
        &["palace", "content", "tags"],
    )
    .await;
    assert_eq!(env["verb"], json!("task add"));
}

#[tokio::test(flavor = "multi_thread")]
async fn task_list_sends_task_list() {
    let env = check(
        &["task", "list", "--include-completed"],
        rows("tasks", 1),
        "task_list",
        &["palace", "include_completed"],
    )
    .await;
    assert_eq!(env["count"], json!(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn task_complete_sends_task_complete() {
    check(
        &["task", "complete", "d1"],
        json!({ "status": "completed", "drawer_id": "d1" }),
        "task_complete",
        &["palace", "drawer_id"],
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn kg_assert_sends_kg_assert() {
    check(
        &["kg", "assert", "s", "p", "o", "--confidence", "0.5"],
        json!({ "status": "asserted" }),
        "kg_assert",
        &["palace", "subject", "predicate", "object", "confidence"],
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn kg_query_sends_kg_query() {
    let body = json!({ "triples": [{ "subject": "s", "predicate": "p", "object": "o" }] });
    let env = check(
        &["kg", "query", "s"],
        body,
        "kg_query",
        &["palace", "subject"],
    )
    .await;
    assert_eq!(env["count"], json!(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn kg_gaps_sends_kg_gaps() {
    let env = check(
        &["kg", "gaps"],
        json!({ "palace": "session-palace", "gaps": [] }),
        "kg_gaps",
        &["palace"],
    )
    .await;
    assert_eq!(env["count"], json!(0));
}

/// Why: daemon down is the condition these verbs exist for. It must exit
/// non-zero, name the socket, and print no envelope.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_tool_verb_with_a_dead_daemon_exits_non_zero_naming_the_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");
    let out = run_tm(&socket, Some("session-palace"), &["kg", "gaps", "--json"]).await;
    assert!(!out.status.success(), "a dead daemon must not exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&socket.display().to_string()),
        "the error must name the socket: {stderr}"
    );
    assert!(out.stdout.is_empty(), "no envelope may be printed");
}
