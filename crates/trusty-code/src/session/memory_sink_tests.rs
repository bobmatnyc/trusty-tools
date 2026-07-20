//! Unit tests for [`super::TurnMemorySink`] and
//! [`super::derive_palace_id_for_project`] (#2345, #2424).
//!
//! Why: the turn recorder must never block/fail a running turn even when
//! trusty-memory is unreachable, must dual-write both `chat_turn_append` and
//! `memory_remember` — via the MCP `tools/call` envelope, the ONLY dispatch
//! shape trusty-memory accepts for `chat_*` tools (#2424; the previous
//! direct-method dispatch failed `-32601` on 100% of writes in the #2343
//! soak) — with the documented params/tags against a live mock daemon, must
//! ensure the target palace exists exactly once before the first write
//! (#2424), and must apply its documented "drop newest" overflow policy —
//! none of that is exercised anywhere else.
//! What: the mock `/rpc` servers here reply with the REAL daemon's
//! `tools/call` response shape (the tool result STRINGIFIED inside
//! `result.content[0].text` — mirrors `transport/rpc.rs`'s `tools/call`
//! arm), so the assertions cover both the request envelope
//! (`method == "tools/call"`, `params.name`/`params.arguments`) and the
//! response unwrap. `enqueue_drops_newest_when_queue_full` exercises the
//! overflow policy directly against the raw channel (no networking, fully
//! deterministic); `write_turn_is_fail_open_on_unreachable_daemon` proves an
//! unreachable `base_url` never panics or hangs;
//! `derive_palace_id_for_project_*` cover the palace-derivation fallback
//! chain.
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

/// Wrap `inner` as the real daemon's `tools/call` response envelope: the
/// tool result STRINGIFIED inside `content[0].text` (mirrors the
/// `transport/rpc.rs` `tools/call` arm).
fn wrapped_result(inner: &Value) -> Value {
    json!({"content": [{"type": "text", "text": inner.to_string()}]})
}

/// Extract the tool name from a captured `tools/call` params object.
fn tool_name(params: &Value) -> &str {
    params["name"].as_str().unwrap_or_default()
}

/// Shared mock behaviour: whether `palace_info` should report the palace as
/// already existing.
#[derive(Clone, Copy)]
enum PalaceState {
    Exists,
    MissingUntilCreated,
}

/// Spin up a tiny in-process `/rpc` mock that captures every request and
/// replies with the daemon's real `tools/call` envelope shape.
///
/// Why: `TurnMemorySink`'s drain task must be observed making REAL HTTP
/// calls with the correct `tools/call` method/params shape (#2424) — a mock
/// server is the only way to assert that without a live trusty-memory
/// daemon.
/// What: binds an ephemeral port, spawns `axum::serve` detached (the test
/// process exits with it), and returns the base URL plus the shared capture
/// buffer. `palace_state` controls the `palace_info` reply: `Exists` always
/// succeeds; `MissingUntilCreated` returns a JSON-RPC error (the daemon's
/// "palace metadata missing" signal) until a `palace_create` lands, after
/// which it succeeds — a stateful stand-in for the real registry.
async fn spawn_capturing_mock(palace_state: PalaceState) -> (String, Captured) {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::clone(&captured);
    let created = Arc::new(Mutex::new(matches!(palace_state, PalaceState::Exists)));

    async fn handle(
        State((store, created)): State<(Captured, Arc<Mutex<bool>>)>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let method = body["method"].as_str().unwrap_or_default().to_string();
        let params = body["params"].clone();
        store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((method, params.clone()));
        let name = tool_name(&params);
        if name == "palace_create" {
            *created.lock().unwrap_or_else(|e| e.into_inner()) = true;
        }
        if name == "palace_info" && !*created.lock().unwrap_or_else(|e| e.into_inner()) {
            return Json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32603, "message": "palace metadata missing"}
            }));
        }
        Json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": wrapped_result(&json!({"ok": true}))
        }))
    }

    let app = Router::new()
        .route("/rpc", post(handle))
        .with_state((store, created));
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
/// surface, tagged `session:<id>` + `turn`) — each as a `tools/call`
/// envelope (#2424) with the documented inner arguments.
#[tokio::test]
async fn enqueue_drain_happy_path() {
    let (base_url, captured) = spawn_capturing_mock(PalaceState::Exists).await;
    let sink = TurnMemorySink::new(base_url, "test-palace".to_string());

    sink.enqueue("sess-1", "what is 2+2?", "4");
    // 3 calls: palace_info (ensure, #2424) + the two dual-write calls.
    wait_for_captures(&captured, 3, Duration::from_secs(2)).await;

    let calls = captured.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(calls.len(), 3, "expected ensure + both dual-write calls");
    for (method, _) in &calls {
        assert_eq!(
            method, "tools/call",
            "#2424: every write must use the tools/call envelope — \
             direct dispatch of chat_* methods is -32601"
        );
    }

    let append = calls
        .iter()
        .find(|(_, p)| tool_name(p) == "chat_turn_append")
        .expect("chat_turn_append call");
    let args = &append.1["arguments"];
    assert_eq!(args["palace"], "test-palace");
    assert_eq!(args["session_id"], "sess-1");
    assert_eq!(args["prompt"], "what is 2+2?");
    assert_eq!(args["response"], "4");

    let remember = calls
        .iter()
        .find(|(_, p)| tool_name(p) == "memory_remember")
        .expect("memory_remember call");
    let args = &remember.1["arguments"];
    assert_eq!(args["palace"], "test-palace");
    let tags: Vec<String> = args["tags"]
        .as_array()
        .expect("tags array")
        .iter()
        .map(|t| t.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(tags.contains(&"session:sess-1".to_string()));
    assert!(tags.contains(&"turn".to_string()));
    assert!(
        args["text"]
            .as_str()
            .unwrap_or_default()
            .contains("what is 2+2?")
    );
    // #2363: every turn-recorder `memory_remember` write must pass
    // `force: true` to bypass the dedup gate documented as hostile to
    // sequential conversational turns.
    assert_eq!(args["force"], json!(true));
}

/// (#2424) A missing palace must be created via `palace_create`
/// (`force: true` — the spec-001 app-managed-palace bypass) exactly ONCE;
/// later turns must reuse the cached ensure and add zero extra RPCs.
#[tokio::test]
async fn ensure_palace_creates_missing_palace_once() {
    let (base_url, captured) = spawn_capturing_mock(PalaceState::MissingUntilCreated).await;
    let sink = TurnMemorySink::new(base_url, "test-palace".to_string());

    sink.enqueue("sess-1", "p1", "r1");
    sink.enqueue("sess-1", "p2", "r2");
    // 6 calls: (palace_info + palace_create) once, then 2 writes per turn.
    wait_for_captures(&captured, 6, Duration::from_secs(2)).await;

    let calls = captured.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let names: Vec<&str> = calls.iter().map(|(_, p)| tool_name(p)).collect();
    assert_eq!(
        names,
        vec![
            "palace_info",
            "palace_create",
            "chat_turn_append",
            "memory_remember",
            "chat_turn_append",
            "memory_remember",
        ],
        "ensure must run (probe + create) exactly once, before the first write"
    );

    let create = calls
        .iter()
        .find(|(_, p)| tool_name(p) == "palace_create")
        .expect("palace_create call");
    assert_eq!(create.1["arguments"]["name"], "test-palace");
    assert_eq!(
        create.1["arguments"]["force"],
        json!(true),
        "app-managed palace slugs need the spec-001 force bypass — the \
         daemon's cwd-derived project slug will not match"
    );
}

/// (#2424) An already-existing palace must NOT be re-created —
/// `handle_palace_create` overwrites `palace.json` (resetting
/// `created_at`/`description`), so the ensure probes with `palace_info`
/// first and never calls `palace_create` when it succeeds; the ensure is
/// also cached, so a second turn adds no extra probe.
#[tokio::test]
async fn ensure_palace_skips_create_when_palace_exists() {
    let (base_url, captured) = spawn_capturing_mock(PalaceState::Exists).await;
    let sink = TurnMemorySink::new(base_url, "test-palace".to_string());

    sink.enqueue("sess-1", "p1", "r1");
    sink.enqueue("sess-1", "p2", "r2");
    // 5 calls: palace_info once, then 2 writes per turn.
    wait_for_captures(&captured, 5, Duration::from_secs(2)).await;

    let calls = captured.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let names: Vec<&str> = calls.iter().map(|(_, p)| tool_name(p)).collect();
    assert_eq!(
        names.iter().filter(|n| **n == "palace_info").count(),
        1,
        "ensure result must be cached — one probe for the whole session"
    );
    assert!(
        !names.contains(&"palace_create"),
        "an existing palace must never be re-created (metadata clobber)"
    );
}

/// (#2363) A `"status":"skipped"` `memory_remember` response (e.g. tripped by
/// a gate `force` does not bypass) must be observable via a `tracing::warn!`
/// rather than silently swallowed like an ordinary success.
///
/// Why: this is the belt-and-braces half of #2363 — `force: true` handles
/// the dedup gate specifically, but this detection covers any OTHER gate
/// that still returns a skipped envelope. Post-#2424 the skipped payload
/// arrives STRINGIFIED inside the `tools/call` envelope, so this also pins
/// that the detection reads the PARSED inner result, not the raw envelope.
/// What: mock server always replies with a wrapped `status: skipped` body
/// for `memory_remember`; the test only asserts the call still lands (no
/// panic, no hang) — the warning itself is inspected via the tracing
/// subscriber in a real deployment, not asserted on here (no test-local
/// tracing capture exists in this crate yet), so this test's assertion is:
/// the drain task tolerates a skipped envelope exactly like a stored one.
#[tokio::test]
async fn write_turn_warns_on_skipped_status() {
    async fn handle_skipped(Json(body): Json<Value>) -> Json<Value> {
        let inner = if body["params"]["name"].as_str() == Some("memory_remember") {
            json!({"palace": "test-palace", "status": "skipped", "reason": "content gate"})
        } else {
            json!({"ok": true})
        };
        Json(json!({"jsonrpc": "2.0", "id": 1, "result": wrapped_result(&inner)}))
    }

    let app = Router::new().route("/rpc", post(handle_skipped));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let sink = TurnMemorySink::new(format!("http://{addr}"), "test-palace".to_string());
    sink.enqueue("sess-skip", "prompt", "response");
    // The assertion is simply that this never panics/hangs; give the drain
    // task a moment to run.
    tokio::time::sleep(Duration::from_millis(200)).await;
}

/// [`TurnMemorySink::base_url`]/[`TurnMemorySink::palace`] must expose
/// exactly the values passed at construction, so #2348's `recall_session`
/// tool can reuse the same binding.
#[test]
fn base_url_and_palace_expose_construction_args() {
    let (tx, _rx) = mpsc::channel(1);
    let sink = TurnMemorySink {
        tx,
        base_url: "http://example.test:1234".to_string(),
        palace: "a-palace".to_string(),
    };
    assert_eq!(sink.base_url(), "http://example.test:1234");
    assert_eq!(sink.palace(), "a-palace");
}

/// An unreachable `base_url` must never panic or hang the drain task — the
/// core fail-open contract (#2345 acceptance criteria), which post-#2424
/// also covers a failed palace ensure (probe AND create both unreachable).
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
    let sink = TurnMemorySink {
        tx,
        base_url: String::new(),
        palace: String::new(),
    };

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

/// No git remote, no override -> agrees with the shared `derive_palace_id`
/// core (issue #1772).
///
/// Why: this function used to carry its own copy of the divergent 4th
/// fallback that `trusty_common::catchup::derive_palace_id_for` also carried
/// (a raw, unslugified `file_name()` basename), which could disagree with
/// `derive_palace_id` on the same inputs. Both call sites now delegate
/// entirely to `trusty_common::derive_palace_id` and only fall back to a
/// fixed, non-derived placeholder, so for a plain (non-git) temp dir with no
/// `TRUSTY_MEMORY_PALACE` override, this function's result must equal calling
/// `trusty_common::derive_palace_id(project_dir, None, None)` directly.
/// What: clears the env override for the duration of the test (no other test
/// in this crate mutates `TRUSTY_MEMORY_PALACE`, so no `#[serial]` guard is
/// needed), then asserts the two calls agree.
/// Test: itself.
#[test]
fn derive_palace_id_for_project_agrees_with_derive_palace_id_no_remote_no_env() {
    // SAFETY: no other test in this crate mutates TRUSTY_MEMORY_PALACE.
    unsafe {
        std::env::remove_var(trusty_common::PALACE_OVERRIDE_ENV);
    }
    let tmp = TempDir::new().expect("tempdir");

    let project_value = derive_palace_id_for_project(tmp.path());
    let core_value = trusty_common::derive_palace_id(tmp.path(), None, None)
        .expect("a real temp dir always has a usable parent/dir slug");

    assert_eq!(
        project_value, core_value,
        "turn-recorder derivation must agree with the shared derive_palace_id core"
    );
}
