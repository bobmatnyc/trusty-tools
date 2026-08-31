//! `GET /api/agents/:name/chat-history` tests (#4278).
//!
//! Why: this route exists so a page reload stops discarding the visible
//! conversation, and it has exactly two ways to fail the user. It can return
//! the WRONG SLICE — the oldest turns instead of the newest, or a page that
//! skips or repeats a message while paging backwards — which rehydrates a
//! confidently wrong history, worse than rehydrating none. Or it can turn an
//! ordinary "nothing persisted yet" state (no palace bound, no session, daemon
//! down) into an error the chat view has to render as a failure. Both halves
//! are pinned here.
//!
//! The mock is [`crate::uds_mock`] on a temp socket, and the agent config is a
//! `tempfile::TempDir`, so nothing races a sibling test on cwd, `$HOME`, or the
//! developer's live daemon (the `agent_kg` / `agent_stores` pattern).
//!
//! What: newest-slice bounding; the absolute `until` cursor paging backwards
//! without a gap or overlap, and staying stable across appends; `has_more`; no
//! palace bound / session absent / daemon undiscoverable / daemon unreachable →
//! `200` + `available: false`; a MALFORMED payload reported as unavailable
//! rather than as an empty conversation; an absent palace not mislabelled as an
//! absent session; unknown agent → 404; traversal name → 400; and the route
//! being wired into the real router.
//! Test: This module IS the test.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::api::server::chat_history::chat_history_at;
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;
use crate::uds_mock::{self, MockMemoryDaemon, RpcError};

/// An agent binding a palace — the only shape whose turns are persisted at all.
const BOUND_FIXTURE: &str = r#"[agent]
name = "izzie"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[[stores]]
name = "bob-kb"
palace = "owner-profile"
"#;

/// A store binding with NO palace. `spawn_persist_turn` is a no-op for this
/// agent, so there is nothing to rehydrate — an ordinary state, not an error.
const NO_PALACE_FIXTURE: &str = r#"[agent]
name = "plain"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[[stores]]
name = "docs-only"
"#;

/// Six messages — three user/assistant turns, chronological, as
/// `chat_turn_append` writes them.
fn history(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| {
            if i % 2 == 0 {
                json!({ "role": "user", "content": format!("q{}", i / 2) })
            } else {
                json!({ "role": "assistant", "content": format!("a{}", i / 2) })
            }
        })
        .collect()
}

/// Mock trusty-memory answering `chat_session_recall` for `persona-izzie` in
/// palace `owner-profile` only. Every other session is refused the way the
/// real daemon refuses one: a `tools/call` error, which is always `-32603`,
/// never a not-found code — the reason `fetch_session` matches on wording.
async fn mock_memory(messages: usize) -> MockMemoryDaemon {
    uds_mock::spawn(move |method: &str, params: Value| {
        let method = method.to_string();
        Box::pin(async move {
            if method != "tools/call" {
                return Err(RpcError::method_not_found(&method, &[]));
            }
            let args = params.get("arguments").cloned().unwrap_or_default();
            let palace = args
                .get("palace")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let session = args
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if palace != "owner-profile" || session != "persona-izzie" {
                return Err(RpcError::new(
                    -32603,
                    format!("chat_session_recall: session not found: {session}"),
                ));
            }
            Ok(uds_mock::tools_call_envelope(&json!({
                "id": session,
                "title": Value::Null,
                "created_at": "2026-08-01T00:00:00Z",
                "updated_at": "2026-08-30T12:00:00Z",
                "history": history(messages),
            })))
        })
    })
    .await
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn agent_dir(name: &str, fixture: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(format!("{name}.toml")), fixture).unwrap();
    dir
}

/// The `content` strings of a response's `messages`, for order assertions.
fn contents(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["content"].as_str().unwrap().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Bounding and paging — the "wrong slice" failure mode
// ---------------------------------------------------------------------------

/// The bug this route fixes is an empty chat view after reload; the bug it
/// must not introduce is hydrating the OLDEST turns. A bounded read takes the
/// newest end.
#[tokio::test]
async fn chat_history_returns_bounded_newest_slice() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let daemon = mock_memory(10).await;

    let resp = chat_history_at(
        &[dir.path().to_path_buf()],
        "izzie",
        Some(daemon.socket()),
        4,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert_eq!(body["available"], true);
    assert_eq!(body["palace"], "owner-profile");
    assert_eq!(body["session_id"], "persona-izzie");
    assert_eq!(body["total"], 10);
    assert_eq!(body["start"], 6);
    assert_eq!(body["has_more"], true, "6 older messages remain");
    assert_eq!(body["updated_at"], "2026-08-30T12:00:00Z");
    // The last four of ten, in chronological order — NOT the first four.
    assert_eq!(contents(&body), vec!["q3", "a3", "q4", "a4"]);
}

/// The client pages backwards by echoing the `start` it was given as the next
/// `until`. Consecutive pages must abut exactly: nothing skipped, nothing
/// served twice.
#[tokio::test]
async fn chat_history_until_cursor_pages_backwards() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let daemon = mock_memory(10).await;
    let dirs = [dir.path().to_path_buf()];

    let first =
        body_json(chat_history_at(&dirs, "izzie", Some(daemon.socket()), 4, None).await).await;
    let cursor = first["start"].as_u64().unwrap() as usize;
    let second =
        body_json(chat_history_at(&dirs, "izzie", Some(daemon.socket()), 4, Some(cursor)).await)
            .await;

    assert_eq!(contents(&first), vec!["q3", "a3", "q4", "a4"]);
    assert_eq!(contents(&second), vec!["q1", "a1", "q2", "a2"]);
    assert_eq!(second["start"], 2);
    assert_eq!(second["has_more"], true, "q0/a0 are still older");

    // The oldest page reports no more, and does not over-read past the start.
    let third =
        body_json(chat_history_at(&dirs, "izzie", Some(daemon.socket()), 4, Some(2)).await).await;
    assert_eq!(contents(&third), vec!["q0", "a0"]);
    assert_eq!(third["start"], 0);
    assert_eq!(third["has_more"], false);
}

/// The correctness argument for absolute indexing: a page addressed by index
/// returns the SAME messages after new turns append. An offset-from-the-end
/// cursor would shift by the number of appends and replay them.
#[tokio::test]
async fn chat_history_until_cursor_is_stable_across_appends() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let dirs = [dir.path().to_path_buf()];

    // Client reads the page at absolute [2, 6) of a 10-message session.
    let small = mock_memory(10).await;
    let before =
        body_json(chat_history_at(&dirs, "izzie", Some(small.socket()), 4, Some(6)).await).await;

    // Four more messages land; the client asks for the same window again.
    let grown = mock_memory(14).await;
    let after =
        body_json(chat_history_at(&dirs, "izzie", Some(grown.socket()), 4, Some(6)).await).await;

    assert_eq!(contents(&before), contents(&after));
    assert_eq!(before["start"], after["start"]);
    assert_eq!(after["total"], 14, "the session did grow");
}

/// A hand-crafted `until` past the end must clamp, not panic on the slice.
#[tokio::test]
async fn chat_history_clamps_an_out_of_range_until() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let daemon = mock_memory(4).await;

    let body = body_json(
        chat_history_at(
            &[dir.path().to_path_buf()],
            "izzie",
            Some(daemon.socket()),
            10,
            Some(9999),
        )
        .await,
    )
    .await;
    assert_eq!(body["total"], 4);
    assert_eq!(body["start"], 0);
    assert_eq!(contents(&body), vec!["q0", "a0", "q1", "a1"]);
}

/// A session shorter than one page is returned whole, with nothing older.
#[tokio::test]
async fn chat_history_short_session_has_no_more() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let daemon = mock_memory(2).await;

    let body = body_json(
        chat_history_at(
            &[dir.path().to_path_buf()],
            "izzie",
            Some(daemon.socket()),
            100,
            None,
        )
        .await,
    )
    .await;
    assert_eq!(body["total"], 2);
    assert_eq!(body["has_more"], false);
    assert_eq!(contents(&body), vec!["q0", "a0"]);
}

// ---------------------------------------------------------------------------
// Degradation — every "nothing to rehydrate" state is a 200
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chat_history_empty_when_no_palace_bound() {
    let dir = agent_dir("plain", NO_PALACE_FIXTURE);
    let daemon = mock_memory(4).await;

    let resp = chat_history_at(
        &[dir.path().to_path_buf()],
        "plain",
        Some(daemon.socket()),
        100,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["available"], false);
    assert_eq!(body["palace"], Value::Null);
    assert_eq!(body["messages"], json!([]));
    assert_eq!(body["session_id"], "persona-plain");
    assert!(
        body["reason"]
            .as_str()
            .unwrap()
            .contains("binds no memory palace"),
        "got {}",
        body["reason"]
    );
}

/// An agent whose first chat turn has not happened yet has no session. The
/// daemon refuses the read; the GUI must see an empty conversation, not a
/// failure toast.
#[tokio::test]
async fn chat_history_empty_when_session_absent() {
    let dir = agent_dir("ghost", BOUND_FIXTURE.replace("izzie", "ghost").as_str());
    let daemon = mock_memory(4).await;

    let body = body_json(
        chat_history_at(
            &[dir.path().to_path_buf()],
            "ghost",
            Some(daemon.socket()),
            100,
            None,
        )
        .await,
    )
    .await;
    assert_eq!(body["available"], false);
    assert_eq!(body["messages"], json!([]));
    assert_eq!(body["total"], 0);
    assert!(
        body["reason"]
            .as_str()
            .unwrap()
            .contains("no persisted chat session"),
        "an absent session reads as empty, not as a daemon fault: got {}",
        body["reason"]
    );
}

#[tokio::test]
async fn chat_history_degrades_when_memory_undiscoverable() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);

    let body =
        body_json(chat_history_at(&[dir.path().to_path_buf()], "izzie", None, 100, None).await)
            .await;
    assert_eq!(body["available"], false);
    assert_eq!(body["palace"], "owner-profile");
    assert!(
        body["reason"]
            .as_str()
            .unwrap()
            .contains("not discoverable"),
        "got {}",
        body["reason"]
    );
}

#[tokio::test]
async fn chat_history_degrades_when_memory_unreachable() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let dead = std::path::Path::new("/nonexistent/trusty-memory/trusty-memory.sock");

    let resp = chat_history_at(&[dir.path().to_path_buf()], "izzie", Some(dead), 100, None).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a down daemon must never break the chat view"
    );
    let body = body_json(resp).await;
    assert_eq!(body["available"], false);
    assert_eq!(body["messages"], json!([]));
}

// ---------------------------------------------------------------------------
// Input validation and wiring
// ---------------------------------------------------------------------------

/// A payload the daemon answers but this crate cannot use must NOT read as an
/// empty conversation. Rendering "no history" for a decode failure is the
/// confidently-incomplete history the owner ruled worse than none.
#[tokio::test]
async fn chat_history_reports_a_malformed_session() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    // A session object with everything EXCEPT the `history` array.
    let daemon = uds_mock::spawn(|_method: &str, _params: Value| {
        Box::pin(async move {
            Ok(uds_mock::tools_call_envelope(&json!({
                "id": "persona-izzie",
                "updated_at": "2026-08-30T12:00:00Z",
            })))
        })
    })
    .await;

    let body = body_json(
        chat_history_at(
            &[dir.path().to_path_buf()],
            "izzie",
            Some(daemon.socket()),
            100,
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        body["available"], false,
        "a malformed payload is not a healthy empty session"
    );
    assert!(
        body["reason"].as_str().unwrap().contains("history"),
        "the reason must name the field that was missing: got {}",
        body["reason"]
    );
}

/// A MISSING PALACE and a MISSING SESSION are different operator problems.
/// `describe_failure`'s palace wording ("does not exist") once matched the
/// session-absence check, so a palace that was never created reported as a
/// session that had simply not started yet.
#[tokio::test]
async fn chat_history_absent_palace_is_not_reported_as_absent_session() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let daemon = uds_mock::spawn(|_method: &str, _params: Value| {
        Box::pin(async move {
            Err(RpcError::new(
                trusty_common::memory_rpc::CODE_NOT_FOUND,
                "palace not found: owner-profile".to_string(),
            ))
        })
    })
    .await;

    let body = body_json(
        chat_history_at(
            &[dir.path().to_path_buf()],
            "izzie",
            Some(daemon.socket()),
            100,
            None,
        )
        .await,
    )
    .await;
    let reason = body["reason"].as_str().unwrap();
    assert_eq!(body["available"], false);
    assert!(
        reason.contains("palace"),
        "an absent palace must say so: got {reason}"
    );
    assert!(
        !reason.contains("no persisted chat session"),
        "an absent palace must not be reported as an absent session: got {reason}"
    );
}

#[tokio::test]
async fn chat_history_unknown_agent_404() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let resp = chat_history_at(&[dir.path().to_path_buf()], "nobody", None, 100, None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn chat_history_rejects_traversal_name() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    for name in ["..", ".", "../../etc/passwd", ""] {
        let resp = chat_history_at(&[dir.path().to_path_buf()], name, None, 100, None).await;
        assert!(
            resp.status() == StatusCode::BAD_REQUEST || resp.status() == StatusCode::NOT_FOUND,
            "{name} must never resolve an agent config"
        );
    }
}

/// The handler is useless if the router never dials it — the path string is
/// part of the contract `ui/src/lib/chatHistory.ts` codes against. An
/// unregistered route 404s with an EMPTY body, so asserting on the handler's
/// own `unknown agent` body is what tells the two 404s apart.
#[tokio::test]
async fn chat_history_route_is_wired_into_the_router() {
    let app: Router = build_router(AppState::default());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/agents/definitely-not-an-agent-4278/chat-history?limit=5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"], "unknown agent",
        "a router-level 404 would carry no body — the route is not registered"
    );
}
