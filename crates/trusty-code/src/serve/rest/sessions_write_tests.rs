//! Route-level tests for `sessions_write.rs` (#2983 Slice 3), split into its
//! own file purely to keep `sessions_write.rs` under the crate's 500-SLOC
//! production cap — see that file's trailing `#[path = ...]` doc for the
//! established convention (`crate::session::registry`'s identical
//! `registry_tests.rs` split).
//!
//! Why: proves each write route end-to-end against a REAL
//! `session::protocol::register` router (never RPC-layer mocks), exactly
//! like `sessions.rs`'s own `tests` module — a route only passes if the
//! whole stack (axum extraction -> `rest::respond` -> `Router::dispatch` ->
//! the real `session.*` handler) agrees.
//! What: one success case, one domain-error case (404 `session_not_found`
//! for the four id-bearing routes; 400 `invalid_argument` for `POST
//! /sessions`, which has no id to be missing), and one malformed-body case
//! per route — except `POST /sessions/{id}/cancel`, which takes no request
//! body at all (see `cancel_session`'s docs), so its third case is an
//! idempotency check instead.
//! Test: this IS the test module.

use axum::Router as AxumRouter;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use std::sync::Arc;
use tower::util::ServiceExt;

use super::routes;
use crate::jsonrpc::Router;
use crate::session::SessionRegistry;

/// Build a router wired with every `session.*` method plus a fresh
/// `SessionRegistry`, then this module's write route group over it —
/// mirrors `sessions::tests::app_and_registry`.
fn app_and_registry() -> (AxumRouter, Arc<SessionRegistry>) {
    let sessions = Arc::new(SessionRegistry::new());
    let mut router = Router::new();
    crate::session::protocol::register(&mut router, sessions.clone());
    let app = routes(Arc::new(router));
    (app, sessions)
}

/// Seed `id`'s `pm_transcript` the same way a real `task.run` would, so
/// `set_goal`/`clear_goal` get past the "no transcript yet" guard — mirrors
/// `protocol_goals::tests::seed_pm_transcript`.
fn seed_pm_transcript(sessions: &SessionRegistry, id: &str) {
    let transcript = sessions
        .begin_pm_transcript(id, "you are the pm", "first task")
        .unwrap();
    sessions.store_pm_transcript(id, transcript);
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn send(
    app: &AxumRouter,
    method: &str,
    uri: &str,
    body: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let body = match body {
        Some(b) => Body::from(b.to_string()),
        None => Body::empty(),
    };
    app.clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap()
}

// ---------------------------------------------------------------------
// POST /sessions -> session.create
// ---------------------------------------------------------------------

/// A well-formed `POST /sessions` must return `201 Created` with a running
/// `Session` JSON.
#[tokio::test]
async fn create_session_returns_201_with_running_session() {
    let (app, _sessions) = app_and_registry();

    let resp = send(&app, "POST", "/sessions", Some(r#"{"task": "do it"}"#)).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let v = body_json(resp).await;
    assert_eq!(v["status"], "running");
    assert_eq!(v["task"], "do it");
}

/// An empty `task` must map to `400` (`-32003 invalid_argument`) — the
/// `POST /sessions` stand-in for the other routes' "missing session" 404
/// case, since a create request has no `session_id` to be missing.
#[tokio::test]
async fn create_session_empty_task_returns_400() {
    let (app, _sessions) = app_and_registry();

    let resp = send(&app, "POST", "/sessions", Some(r#"{"task": "   "}"#)).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], -32003);
}

/// Syntactically invalid JSON must never reach the handler — axum's `Json`
/// extractor rejects it with a client error before `session.create` runs.
#[tokio::test]
async fn create_session_malformed_body_returns_400() {
    let (app, _sessions) = app_and_registry();

    let resp = send(&app, "POST", "/sessions", Some("{not valid json")).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------
// POST /sessions/{id}/messages -> session.send
// ---------------------------------------------------------------------

/// A message sent to a real session must return `200` with
/// `{"acknowledged": true}`.
#[tokio::test]
async fn send_message_returns_200_acknowledged() {
    let (app, sessions) = app_and_registry();
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let resp = send(
        &app,
        "POST",
        &format!("/sessions/{}/messages", session.id),
        Some(r#"{"input": "hi"}"#),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["acknowledged"], true);
}

/// Sending a message to an unknown session must be a real `404` with a
/// `session_not_found` envelope.
#[tokio::test]
async fn send_message_missing_session_returns_404() {
    let (app, _sessions) = app_and_registry();

    let resp = send(
        &app,
        "POST",
        "/sessions/does-not-exist/messages",
        Some(r#"{"input": "hi"}"#),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], -32007);
}

/// A body missing the required `input` field must be rejected by the `Json`
/// extractor before `session.send` runs.
#[tokio::test]
async fn send_message_malformed_body_returns_400() {
    let (app, sessions) = app_and_registry();
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let resp = send(
        &app,
        "POST",
        &format!("/sessions/{}/messages", session.id),
        Some(r#"{}"#),
    )
    .await;
    assert!(resp.status().is_client_error());
}

// ---------------------------------------------------------------------
// POST /sessions/{id}/cancel -> session.cancel
// ---------------------------------------------------------------------

/// Cancelling a real, non-executing session must return `200` with the
/// terminal `Session` snapshot.
#[tokio::test]
async fn cancel_session_returns_200_with_session_json() {
    let (app, sessions) = app_and_registry();
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let resp = send(
        &app,
        "POST",
        &format!("/sessions/{}/cancel", session.id),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["status"], "cancelled");
}

/// Cancelling an unknown session must be a real `404`.
#[tokio::test]
async fn cancel_session_missing_session_returns_404() {
    let (app, _sessions) = app_and_registry();

    let resp = send(&app, "POST", "/sessions/does-not-exist/cancel", None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], -32007);
}

/// Cancelling twice must be idempotent success both times, matching
/// `session::protocol::cancel`'s documented semantics — this route's
/// substitute for the "malformed body" case, since it takes no body.
#[tokio::test]
async fn cancel_session_is_idempotent() {
    let (app, sessions) = app_and_registry();
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let first = send(
        &app,
        "POST",
        &format!("/sessions/{}/cancel", session.id),
        None,
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);

    let second = send(
        &app,
        "POST",
        &format!("/sessions/{}/cancel", session.id),
        None,
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------
// PUT /sessions/{id}/goal -> session.set_goal
// ---------------------------------------------------------------------

/// Setting a goal slot on a session with a seeded transcript must return
/// `200` with `{}`.
#[tokio::test]
async fn set_goal_returns_200_empty_object() {
    let (app, sessions) = app_and_registry();
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    seed_pm_transcript(&sessions, &session.id);

    let resp = send(
        &app,
        "PUT",
        &format!("/sessions/{}/goal", session.id),
        Some(r#"{"slot": 2, "text": "ship it"}"#),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v, serde_json::json!({}));
}

/// Setting a goal on an unknown session must be a real `404`.
#[tokio::test]
async fn set_goal_missing_session_returns_404() {
    let (app, _sessions) = app_and_registry();

    let resp = send(
        &app,
        "PUT",
        "/sessions/does-not-exist/goal",
        Some(r#"{"slot": 1, "text": "x"}"#),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], -32007);
}

/// A body missing the required `text` field must be rejected by the `Json`
/// extractor before `session.set_goal` runs.
#[tokio::test]
async fn set_goal_malformed_body_returns_400() {
    let (app, sessions) = app_and_registry();
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let resp = send(
        &app,
        "PUT",
        &format!("/sessions/{}/goal", session.id),
        Some(r#"{"slot": 1}"#),
    )
    .await;
    assert!(resp.status().is_client_error());
}

// ---------------------------------------------------------------------
// DELETE /sessions/{id}/goal -> session.clear_goal
// ---------------------------------------------------------------------

/// Clearing a previously-set goal slot must return `200` with `{}`.
#[tokio::test]
async fn clear_goal_returns_200_empty_object() {
    let (app, sessions) = app_and_registry();
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    seed_pm_transcript(&sessions, &session.id);
    sessions.set_goal(&session.id, 3, "temp").unwrap();

    let resp = send(
        &app,
        "DELETE",
        &format!("/sessions/{}/goal", session.id),
        Some(r#"{"slot": 3}"#),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v, serde_json::json!({}));
}

/// Clearing a goal on an unknown session must be a real `404`.
#[tokio::test]
async fn clear_goal_missing_session_returns_404() {
    let (app, _sessions) = app_and_registry();

    let resp = send(
        &app,
        "DELETE",
        "/sessions/does-not-exist/goal",
        Some(r#"{"slot": 1}"#),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], -32007);
}

/// A body missing the required `slot` field must be rejected by the `Json`
/// extractor before `session.clear_goal` runs.
#[tokio::test]
async fn clear_goal_malformed_body_returns_400() {
    let (app, sessions) = app_and_registry();
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let resp = send(
        &app,
        "DELETE",
        &format!("/sessions/{}/goal", session.id),
        Some(r#"{}"#),
    )
    .await;
    assert!(resp.status().is_client_error());
}
