//! Tests for `routes::chat` (#9979): routing, the dedicated handle, and every
//! failure arm of a chat turn.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use super::*;
use crate::server::build_router;

/// POST `body` to the chat route on `state`'s router; status, content type and
/// body text.
async fn post_chat(
    state: AppState,
    origin: Option<&str>,
    body: Value,
) -> (StatusCode, String, String) {
    let mut req = Request::builder()
        .method("POST")
        .uri(MPM_CHAT_API_PATH)
        .header("content-type", "application/json");
    if let Some(origin) = origin {
        req = req.header("origin", origin);
    }
    let req = req.body(Body::from(body.to_string())).expect("request");
    let resp = build_router(state).oneshot(req).await.expect("response");
    let status = resp.status();
    let ctype = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, ctype, String::from_utf8_lossy(&bytes).into_owned())
}

fn turn() -> Value {
    json!({ "message": "list sessions", "actions": true })
}

/// A state whose dedicated chat handle is `Connected` but lacks
/// `sessions_chat`, so a turn that reaches it answers a 503 naming the tool.
async fn state_with_chat_missing_the_tool() -> AppState {
    let state = AppState::new(vec![]);
    state
        .mpm_chat
        .handle()
        .prime_connected_missing_tool_for_test(SESSIONS_CHAT_TOOL)
        .await;
    state
}

/// Why: the static chat route shares its prefix with `/sessions/{id}`, which
/// has no POST; a shadowed route would answer 405 and the page would never
/// reach mpm.
/// What: a POST reaching `chat_handler` gets the dedicated handle's 503
/// naming `sessions_chat`; a shadowed one would get 405.
/// Test: this test.
#[tokio::test]
async fn chat_route_is_not_shadowed_by_the_id_capture() {
    let state = state_with_chat_missing_the_tool().await;
    let (status, _, body) = post_chat(state, None, turn()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body.contains(SESSIONS_CHAT_TOOL),
        "the chat route must reach chat_handler; got: {body}"
    );
}

/// Why: ruling — no mpm means a clear 503 with a text body, never a hang or
/// a 500.
/// What: a chat handle whose binary is not on PATH answers 503 `text/plain`
/// naming the failure, well inside the turn bound.
/// Test: this test.
#[tokio::test]
async fn chat_without_mpm_is_a_503_with_a_text_body() {
    let mut state = AppState::new(vec![]);
    let absent = McpServiceHandle::new("trusty-console-no-such-tm-9979", vec![]);
    state.mpm_chat = MpmChat::with_timeout(Arc::new(absent), CHAT_CALL_TIMEOUT);
    let started = Instant::now();
    let (status, ctype, body) = post_chat(state, None, turn()).await;
    assert!(started.elapsed() < Duration::from_secs(5), "must not hang");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(ctype.starts_with("text/plain"), "text body, got {ctype:?}");
    assert!(body.contains("mpm chat is unavailable"), "{body}");
    assert!(body.contains("not installed"), "{body}");
}

/// Why: #9979 — a chat turn on the shared `"trusty-mpm"` handle would hold
/// the mutex the Sessions/Disk/Config routes and the metrics poller share.
/// What: the chat handle is a different `Arc` from the shared one, spawns
/// `tm serve --stdio --allow-chat` (the shared one has no `--allow-chat`),
/// and a turn reaches it: with the shared handle held `Degraded` and the chat
/// handle missing the tool, the 503 names `sessions_chat`, which only the
/// chat handle's capability gate says.
/// Test: this test.
#[tokio::test]
async fn chat_uses_the_dedicated_handle_not_the_shared_one() {
    let state = state_with_chat_missing_the_tool().await;
    let handles = state.mcp_handles();
    let shared = handles.get("trusty-mpm").expect("shared mpm handle");
    shared.prime_degraded_for_test().await;
    let chat = state.mpm_chat.handle();
    assert!(!Arc::ptr_eq(chat, shared), "chat must not share the handle");
    assert!(
        !handles.values().any(|h| Arc::ptr_eq(h, chat)),
        "the chat handle must stay out of the polled handle map"
    );
    let (binary, args) = chat.command_for_test();
    assert_eq!(binary, "tm");
    assert_eq!(args, ["serve", "--stdio", "--allow-chat"]);
    assert!(
        !shared
            .command_for_test()
            .1
            .iter()
            .any(|a| a == "--allow-chat")
    );

    let (status, _, body) = post_chat(state.clone(), None, turn()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body.contains("tm does not expose `sessions_chat`"),
        "the turn must go to the chat handle; got: {body}"
    );
}

/// Why: Fail-Open check — a fired timeout on the dedicated handle must reach
/// the page as an error, not a default reply.
/// What: the chat handle is connected over `sleep` (never answers) with a
/// 200 ms bound; the turn answers 503 naming the timeout, inside 5 s.
/// Test: this test.
#[tokio::test]
async fn a_timed_out_turn_is_a_503_naming_the_timeout() {
    let mut state = AppState::new(vec![]);
    let handle = Arc::new(McpServiceHandle::new("tm", vec![]));
    let tools = test_tool_set(&[SESSIONS_CHAT_TOOL]);
    handle
        .prime_connected_for_test("sleep", &["30"], tools)
        .await;
    state.mpm_chat = MpmChat::with_timeout(handle, Duration::from_millis(200));
    let started = Instant::now();
    let (status, _, body) = post_chat(state, None, turn()).await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "bound must apply"
    );
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("timed out"), "{body}");
}

/// Why: a tool that answers `isError: true` (no model configured, a failed
/// turn) must not be shown as a reply.
/// What: the envelope becomes `ToolFailed` carrying the tool's text, and its
/// response is a 503 with that text.
/// Test: this test.
#[tokio::test]
async fn a_tool_error_envelope_is_a_tool_failure() {
    let text = "sessions_chat failed (503): no LLM provider configured";
    let raw = json!({ "content": [{ "type": "text", "text": text }], "isError": true });
    let err = interpret_envelope(raw).expect_err("isError must be an error");
    assert!(
        matches!(&err, ChatTurnError::ToolFailed(t) if t == text),
        "{err:?}"
    );
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    assert!(String::from_utf8_lossy(&bytes).contains(text));
}

/// Why: the page reads `reply`, `conv_id`, `actions_taken` and `cost` from the
/// turn's JSON, so a success envelope must unwrap to exactly that object.
/// Test: this test.
#[test]
fn a_success_envelope_unwraps_to_the_reply() {
    let reply =
        json!({ "reply": "2 sessions", "conv_id": "c1", "actions_taken": ["list"], "cost": 0.01 });
    let raw =
        json!({ "content": [{ "type": "text", "text": reply.to_string() }], "isError": false });
    assert_eq!(interpret_envelope(raw).expect("success"), reply);
}

/// Why: the chat POST is a state-changing write, so the router-wide origin
/// guard must refuse it from another site.
/// What: a cross-origin POST answers 403 before the handler runs.
/// Test: this test.
#[tokio::test]
async fn a_cross_origin_chat_post_is_403() {
    let state = state_with_chat_missing_the_tool().await;
    let (status, _, _) = post_chat(state, Some("http://evil.example.com"), turn()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
