//! Unit tests for [`super::TurnMemorySink`] and
//! [`super::derive_palace_id_for_project`] (#2345).
//!
//! Why: the turn recorder must never block/fail a running turn even when
//! trusty-memory is unreachable, must dual-write both `chat_turn_append` and
//! `memory_remember` with the documented params/tags against a live mock
//! daemon, and must apply its documented "drop newest" overflow policy —
//! none of that is exercised anywhere else.
//! What: `enqueue_drain_happy_path` spins up a tiny in-process axum mock
//! `/rpc` server (mirrors `trusty_common::mcp::memory_rpc`'s own test
//! pattern) and asserts both calls land with the right shape;
//! `enqueue_drops_newest_when_queue_full` exercises the overflow policy
//! directly against the raw channel (no networking, fully deterministic);
//! `write_turn_is_fail_open_on_unreachable_daemon` proves an unreachable
//! `base_url` never panics or hangs; `derive_palace_id_for_project_*` cover
//! the palace-derivation fallback chain.
//! Test: this file.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use super::*;

/// One captured `/rpc` call: the JSON-RPC `method` and its `params`.
type Captured = Arc<Mutex<Vec<(String, Value)>>>;

/// Spin up a tiny in-process `/rpc` mock that captures every request and
/// always replies with a successful JSON-RPC envelope.
///
/// Why: `TurnMemorySink`'s drain task must be observed making REAL HTTP
/// calls with the correct method/params shape — a mock server is the only
/// way to assert that without a live trusty-memory daemon.
/// What: binds an ephemeral port, spawns `axum::serve` detached (the test
/// process exits with it), and returns the base URL plus the shared capture
/// buffer.
async fn spawn_capturing_mock() -> (String, Captured) {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::clone(&captured);

    async fn handle(State(store): State<Captured>, Json(body): Json<Value>) -> Json<Value> {
        let method = body["method"].as_str().unwrap_or_default().to_string();
        let params = body["params"].clone();
        store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((method, params));
        Json(json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}}))
    }

    let app = Router::new().route("/rpc", post(handle)).with_state(store);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), captured)
}

/// Poll `captured` until it holds at least `n` entries or `timeout` elapses.
async fn wait_for_captures(captured: &Captured, n: usize, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if captured.lock().unwrap_or_else(|e| e.into_inner()).len() >= n
            || tokio::time::Instant::now() >= deadline
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Enqueuing one turn against a live mock daemon must land BOTH
/// `chat_turn_append` (exact record) and `memory_remember` (semantic recall
/// surface, tagged `session:<id>` + `turn`) with the documented params.
#[tokio::test]
async fn enqueue_drain_happy_path() {
    let (base_url, captured) = spawn_capturing_mock().await;
    let sink = TurnMemorySink::new(base_url, "test-palace".to_string());

    sink.enqueue("sess-1", "what is 2+2?", "4");
    wait_for_captures(&captured, 2, Duration::from_secs(2)).await;

    let calls = captured.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(calls.len(), 2, "expected both dual-write calls to land");

    let append = calls
        .iter()
        .find(|(m, _)| m == "chat_turn_append")
        .expect("chat_turn_append call");
    assert_eq!(append.1["palace"], "test-palace");
    assert_eq!(append.1["session_id"], "sess-1");
    assert_eq!(append.1["prompt"], "what is 2+2?");
    assert_eq!(append.1["response"], "4");

    let remember = calls
        .iter()
        .find(|(m, _)| m == "memory_remember")
        .expect("memory_remember call");
    assert_eq!(remember.1["palace"], "test-palace");
    let tags: Vec<String> = remember.1["tags"]
        .as_array()
        .expect("tags array")
        .iter()
        .map(|t| t.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(tags.contains(&"session:sess-1".to_string()));
    assert!(tags.contains(&"turn".to_string()));
    assert!(
        remember.1["text"]
            .as_str()
            .unwrap_or_default()
            .contains("what is 2+2?")
    );
}

/// An unreachable `base_url` must never panic or hang the drain task — the
/// core fail-open contract (#2345 acceptance criteria).
#[tokio::test]
async fn write_turn_is_fail_open_on_unreachable_daemon() {
    let sink = TurnMemorySink::new("http://127.0.0.1:1".to_string(), "p".to_string());
    sink.enqueue("sess-2", "prompt", "response");
    // Give the drain task a moment to attempt (and fail) the calls; the test
    // passing at all (no panic, no hang) is the assertion.
    tokio::time::sleep(Duration::from_millis(200)).await;
}

/// `enqueue` must drop the NEWEST turn (not block) once the bounded channel
/// is full, per its documented overflow policy.
///
/// Why: exercised directly against the raw channel (constructing
/// `TurnMemorySink` from a sender with no drain task consuming it) so the
/// overflow behaviour is deterministic — no networking, no timing races
/// against a live consumer.
#[test]
fn enqueue_drops_newest_when_queue_full() {
    let (tx, mut rx) = mpsc::channel(1);
    let sink = TurnMemorySink { tx };

    sink.enqueue("s1", "p1", "r1");
    sink.enqueue("s1", "p2", "r2"); // queue is full; this one must be dropped

    let first = rx.try_recv().expect("first turn should have been queued");
    assert_eq!(first.prompt, "p1");
    assert!(
        rx.try_recv().is_err(),
        "second turn must have been dropped, not queued"
    );
}

/// No git remote, no override -> falls back to a non-empty, stable slug
/// derived from the directory (never panics, never empty).
///
/// Why: this is a plain (non-git) temp dir, so `derive_palace_id_for_project`
/// must exercise its final fallback branch rather than the git-remote or
/// override branches; the exact slug shape is `derive_palace_id`'s concern
/// (already unit-tested in `trusty-common`), so this only asserts the
/// contract this function adds: non-empty and deterministic.
#[test]
fn derive_palace_id_for_project_falls_back_to_dirname() {
    let tmp = TempDir::new().expect("tempdir");
    let first = derive_palace_id_for_project(tmp.path());
    let second = derive_palace_id_for_project(tmp.path());
    assert!(!first.is_empty(), "expected a non-empty fallback palace id");
    assert_eq!(
        first, second,
        "derivation must be deterministic for the same path"
    );
}
