//! Tests for the per-session file-change route (#94).
//!
//! Why: the four properties issue #94 states — session scoping, `?since=`
//! filtering, deduplication, and a typed not-found — are contract, so each gets
//! a test that drives the REAL daemon router rather than the handler body. That
//! makes the route registration part of what is proven: reverting the one-line
//! merge in `api.rs` fails every case here at runtime.
//! What: builds an isolated `DaemonState`, seeds the hook ring buffer with
//! `FileChanged` records at controlled timestamps, and reads the route back
//! over `tower::ServiceExt::oneshot`.
//! Test: this file.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request};
use chrono::{TimeZone, Utc};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
use crate::daemon::state::DaemonState;

/// An isolated daemon state, plus the `TempDir` whose lifetime backs it.
async fn isolated_state() -> (Arc<DaemonState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::with_root_isolated_managed(dir.path().to_path_buf()).await;
    (Arc::new(state), dir)
}

/// Register one daemon session and return its id.
///
/// This is the key space the filesystem watcher writes `FileChanged` records
/// under, and the space a Claude Code session is auto-registered into on
/// `SessionStart`.
fn register_session(state: &DaemonState) -> SessionId {
    let id = SessionId::new();
    let mut session = Session::new(id, "/tmp/tm-94", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    state.register_session(session);
    id
}

/// Push one `FileChanged` record at an exact second, bypassing
/// `HookEventRecord::now` so `?since=` and dedup have fixed timestamps to act on.
fn push_file_change(
    state: &DaemonState,
    session: SessionId,
    path: &str,
    operation: Option<&str>,
    at_secs: i64,
) {
    let mut payload = json!({ "path": path });
    if let Some(op) = operation {
        payload["operation"] = json!(op);
    }
    state.push_hook_event(HookEventRecord {
        session,
        event: HookEvent::FileChanged,
        at: Utc
            .timestamp_opt(at_secs, 0)
            .single()
            .expect("valid timestamp"),
        payload,
    });
}

/// Drive the real daemon router and read status plus body.
async fn get(state: Arc<DaemonState>, uri: &str) -> (u16, Value) {
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .expect("build request");
    let response = crate::daemon::api::router(state)
        .oneshot(request)
        .await
        .expect("route call");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// The `path` of every entry in a `files` array, in order.
fn paths(body: &Value) -> Vec<String> {
    body["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|e| e["path"].as_str().expect("path string").to_owned())
        .collect()
}

#[tokio::test]
async fn files_route_returns_only_that_sessions_records() {
    let (state, _dir) = isolated_state().await;
    let mine = register_session(&state);
    let theirs = register_session(&state);
    push_file_change(&state, mine, "/w/src/mine.rs", Some("write"), 1_700_000_100);
    push_file_change(
        &state,
        theirs,
        "/w/src/theirs.rs",
        Some("write"),
        1_700_000_101,
    );
    // A non-FileChanged event for the same session must not leak in either.
    state.push_hook_event(HookEventRecord::now(
        mine,
        HookEvent::PostToolUse,
        json!({ "path": "/w/src/not-a-file-change.rs" }),
    ));

    let (status, body) = get(
        Arc::clone(&state),
        &format!("/api/v1/sessions/managed/{}/files", mine.0),
    )
    .await;

    assert_eq!(status, 200, "body: {body}");
    assert_eq!(paths(&body), vec!["/w/src/mine.rs".to_owned()]);
    assert_eq!(body["count"], 1);
    assert_eq!(body["session"], mine.0.to_string());
    assert_eq!(body["since"], Value::Null);
    assert_eq!(body["files"][0]["operation"], "write");
    assert_eq!(body["files"][0]["timestamp"], 1_700_000_100_i64);
}

#[tokio::test]
async fn files_route_since_excludes_earlier_records() {
    let (state, _dir) = isolated_state().await;
    let id = register_session(&state);
    push_file_change(&state, id, "/w/src/old.rs", Some("write"), 1_700_000_100);
    push_file_change(&state, id, "/w/src/new.rs", Some("write"), 1_700_000_200);

    let (status, body) = get(
        Arc::clone(&state),
        &format!("/api/v1/sessions/managed/{}/files?since=1700000200", id.0),
    )
    .await;

    assert_eq!(status, 200, "body: {body}");
    assert_eq!(paths(&body), vec!["/w/src/new.rs".to_owned()]);
    assert_eq!(body["since"], 1_700_000_200_i64);

    // Without the bound both records are still reachable, so the cut above is
    // the filter's doing and not a recording failure.
    let (_, all) = get(state, &format!("/api/v1/sessions/managed/{}/files", id.0)).await;
    assert_eq!(all["count"], 2, "body: {all}");
}

#[tokio::test]
async fn files_route_collapses_duplicate_reports() {
    let (state, _dir) = isolated_state().await;
    let id = register_session(&state);
    // The watcher and a posted hook both report the same change: same path,
    // same operation, same second.
    push_file_change(&state, id, "/w/src/a.rs", Some("write"), 1_700_000_100);
    push_file_change(&state, id, "/w/src/a.rs", Some("write"), 1_700_000_100);
    // A DISTINCT operation on the same path in the same second is not a
    // duplicate and must survive.
    push_file_change(&state, id, "/w/src/a.rs", Some("delete"), 1_700_000_100);

    let (status, body) = get(state, &format!("/api/v1/sessions/managed/{}/files", id.0)).await;

    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["count"], 2, "body: {body}");
    let ops: Vec<&str> = body["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|e| e["operation"].as_str().expect("operation"))
        .collect();
    assert_eq!(ops, vec!["write", "delete"]);
}

#[tokio::test]
async fn files_route_unknown_session_is_a_typed_404() {
    let (state, _dir) = isolated_state().await;
    let unknown = uuid::Uuid::new_v4();

    let (status, body) = get(state, &format!("/api/v1/sessions/managed/{unknown}/files")).await;

    assert_eq!(status, 404, "body: {body}");
    assert_eq!(body["session"], unknown.to_string());
    let error = body["error"].as_str().expect("typed error field");
    assert!(error.contains("not found"), "error was: {error}");
}

#[tokio::test]
async fn files_route_reads_a_managed_store_record() {
    let (state, _dir) = isolated_state().await;
    let manager = state.session_manager().await;
    let record = manager
        .create("task".to_owned(), None, None, None, None, None)
        .await
        .expect("create managed session");
    // The watcher keys by the daemon session id, which shares the managed id's
    // UUID — the route must find the record through the managed store alone.
    push_file_change(
        &state,
        SessionId(record.id.0),
        "/w/src/managed.rs",
        None,
        1_700_000_100,
    );

    let (status, body) = get(
        Arc::clone(&state),
        &format!("/api/v1/sessions/managed/{}/files", record.id.0),
    )
    .await;

    assert_eq!(status, 200, "body: {body}");
    assert_eq!(paths(&body), vec!["/w/src/managed.rs".to_owned()]);
    // The watcher records a bare path, so no operation is reported for it.
    assert_eq!(body["files"][0]["operation"], Value::Null);
}

#[tokio::test]
async fn files_route_rejects_a_malformed_id() {
    let (state, _dir) = isolated_state().await;

    let (status, _) = get(state, "/api/v1/sessions/managed/not-a-uuid/files").await;

    assert_eq!(status, 400);
}
