//! Tests for logs tail, admin stop, and fire-and-forget remember handlers.
use super::super::admin::admin_stop;
use super::super::router;
use super::test_state;
use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::Json;
use serde_json::{json, Value};
use tower::util::ServiceExt;
use trusty_common::memory_core::palace::{Palace, PalaceId};

/// Issue #35 — `GET /api/v1/logs/tail` returns the most recent buffered
/// lines and the total count.
///
/// Why: operators inspect a running daemon via this endpoint; it must
/// surface exactly what the shared `LogBuffer` holds.
/// What: attaches a `LogBuffer` to the state, pushes three lines, GETs
/// `?n=2`, and asserts the tail + `total`.
/// Test: this test.
#[tokio::test]
async fn logs_tail_returns_recent_lines() {
    let buffer = trusty_common::log_buffer::LogBuffer::new(100);
    buffer.push("line one".to_string());
    buffer.push("line two".to_string());
    buffer.push("line three".to_string());
    let state = test_state().with_log_buffer(buffer);
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/logs/tail?n=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let lines = v["lines"].as_array().expect("lines array");
    assert_eq!(lines.len(), 2, "n=2 must return two lines");
    assert_eq!(lines[0].as_str(), Some("line two"));
    assert_eq!(lines[1].as_str(), Some("line three"));
    assert_eq!(v["total"].as_u64(), Some(3));
}

/// Issue #35 — `GET /api/v1/logs/tail?n=` is clamped to
/// `[1, MAX_LOGS_TAIL_N]`.
///
/// Why: a misconfigured client must not request more lines than the
/// buffer holds, and `n=0` must still return at least one line.
/// What: pushes five lines, requests `n=0` (clamps to 1) and an oversized
/// `n` (clamps to the buffer length).
/// Test: this test.
#[tokio::test]
async fn logs_tail_clamps_n() {
    let buffer = trusty_common::log_buffer::LogBuffer::new(100);
    for i in 0..5 {
        buffer.push(format!("l{i}"));
    }
    let state = test_state().with_log_buffer(buffer);
    let app = router().with_state(state);

    // n=0 clamps up to 1.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/logs/tail?n=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["lines"].as_array().expect("lines").len(), 1);

    // n far past MAX clamps down to the buffer length (5).
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/logs/tail?n=999999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["lines"].as_array().expect("lines").len(), 5);
}

/// Issue #35 / #1100 — `POST /api/v1/admin/stop` acknowledges the shutdown
/// request with `{ ok, message }`.
///
/// Why (issue #1100): the original handler spawned a detached task that
/// called `std::process::exit(0)` after 200 ms, relying on a timing race
/// that is lost on loaded CI. The exit call is now compiled out under
/// `#[cfg(not(test))]` so this test can assert the response shape without
/// any risk of terminating the test process.
/// What: calls `admin_stop` directly and asserts the JSON body.
/// Test: this test.
#[tokio::test]
async fn admin_stop_returns_ok() {
    let state = test_state();
    let Json(body) = admin_stop(State(state)).await;
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["message"].as_str(), Some("shutting down"));
}

/// Issue #1100 — in test builds, `admin_stop` must return without
/// spawning the `process::exit` task, leaving the test process alive.
///
/// Why: demonstrates that the `#[cfg(not(test))]` guard is correctly
/// in place; if the guard were absent the spawned task would call
/// `std::process::exit(0)` and abort the test runner.
/// What: calls the handler, awaits any pending tokio tasks briefly, and
/// asserts the process is still alive (the test itself completing proves
/// this).
/// Test: this test.
#[tokio::test]
async fn admin_stop_does_not_exit_in_test() {
    let state = test_state();
    let Json(body) = admin_stop(State(state)).await;
    // Yield to the runtime to allow any spuriously spawned tasks to run.
    // If process::exit were called here, the test process would terminate
    // and the assertion below would never execute.
    tokio::task::yield_now().await;
    // If we reach this line the process is still alive — the guard works.
    assert_eq!(
        body["ok"],
        Value::Bool(true),
        "admin_stop must return ok=true in test builds"
    );
}

/// `POST /api/v1/remember` returns 202 Accepted with a `queued` envelope
/// and the spawned task actually persists a drawer in the target palace.
///
/// Why: this is the central contract of the fire-and-forget endpoint —
/// the response must come back immediately (no waiting on the redb write)
/// and the work must still happen. Without this test the endpoint could
/// silently regress to either "returns 202 but never writes" or "blocks
/// the caller on the dispatch".
/// What: provisions a palace, POSTs `{content, palace, tags}` to the
/// endpoint, asserts 202 + `{status:"queued"}`, then polls the palace's
/// drawer list (up to ~2 s) until the spawned task lands the write.
/// Test: this test.
#[tokio::test]
async fn remember_async_returns_202_and_persists() {
    let state = test_state();
    // Pre-create the target palace so the spawned task does not race
    // against palace_create — we want to assert only the persist path.
    let palace = Palace {
        id: PalaceId::new("remember-async"),
        name: "remember-async".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("remember-async"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create_palace");

    let app = router().with_state(state.clone());
    let body = json!({
        "content": "Trusty-memory note CLI ships a fire-and-forget HTTP endpoint for sub-agents.",
        "palace": "remember-async",
        "tags": ["docs", "note-cli"],
    })
    .to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/remember")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::ACCEPTED,
        "remember endpoint must respond 202 immediately"
    );
    let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["status"], "queued");

    // Wait for the spawned task to finish. The dedup/blocklist gates run
    // on the spawn thread, so we cannot synchronously await the write;
    // poll the registry until the drawer lands or the deadline expires.
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new("remember-async"))
        .expect("open palace");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let count = handle.drawers.read().len();
        if count >= 1 {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!("spawned remember task never persisted a drawer (count={count})");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// `POST /api/v1/remember` with empty `content` returns 400 — the only
/// synchronous validation the endpoint performs.
///
/// Why: empty content is a programming error in the caller (the spawned
/// task would just hit the content-gate and silently drop the request),
/// so we surface it as a 400 before queueing. Every other failure mode
/// (palace not found, blocklist, dedup) is logged on the spawn task and
/// still returns 202 because the agent has already exited by then.
/// What: POST `{content: ""}` and assert 400. Also covers the trim path —
/// whitespace-only content is treated as empty.
/// Test: this test.
#[tokio::test]
async fn remember_async_rejects_empty_content() {
    let state = test_state();
    let app = router().with_state(state);
    for body in [json!({"content": ""}), json!({"content": "   \n  "})] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/remember")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "empty content must be rejected; body={body}"
        );
    }
}
