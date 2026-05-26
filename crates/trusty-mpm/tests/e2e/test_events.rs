//! E2E: hook relay and event feed.
//!
//! Note: `POST /hooks` parses the session id as a UUID and the event name via
//! `HookEvent::from_wire`. A malformed UUID or an unknown event name yields
//! `400`. A *well-formed* UUID for a session that was never registered is
//! still accepted — the relay records events into a shared ring buffer and
//! does not require the session to exist — so the "invalid session" rejection
//! case below uses a malformed id, which is the contract the daemon enforces.

use crate::harness::TestDaemon;
use serde_json::{Value, json};

/// A fresh daemon has an empty event feed.
#[tokio::test]
async fn events_empty_initially() {
    let daemon = TestDaemon::spawn().await;
    let resp = daemon
        .client()
        .get(daemon.url("/events/poll"))
        .send()
        .await
        .expect("events request");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("events body");
    assert!(body["events"].as_array().expect("events array").is_empty());
}

/// A valid hook event posted for a registered session appears in `/events/poll`.
#[tokio::test]
async fn hook_event_appears_in_feed() {
    let daemon = TestDaemon::spawn().await;
    let client = daemon.client();

    let created: Value = client
        .post(daemon.url("/sessions"))
        .json(&json!({ "project": "/tmp/e2e-hook" }))
        .send()
        .await
        .expect("create session")
        .json()
        .await
        .expect("create body");
    let id = created["id"].as_str().expect("id present").to_string();

    let post = client
        .post(daemon.url("/hooks"))
        .json(&json!({
            "session_id": id,
            "event": "PostToolUse",
            "payload": { "tool": "Edit" },
        }))
        .send()
        .await
        .expect("hook post");
    assert_eq!(post.status(), 200);

    let feed: Value = client
        .get(daemon.url("/events/poll"))
        .send()
        .await
        .expect("events request")
        .json()
        .await
        .expect("events body");
    let events = feed["events"].as_array().expect("events array");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["session"], id);
    assert_eq!(events[0]["event"], "PostToolUse");
}

/// `POST /hooks` with a malformed session id is rejected with `400`.
#[tokio::test]
async fn hook_rejects_invalid_session() {
    let daemon = TestDaemon::spawn().await;
    let resp = daemon
        .client()
        .post(daemon.url("/hooks"))
        .json(&json!({
            "session_id": "not-a-uuid",
            "event": "PreToolUse",
            "payload": {},
        }))
        .send()
        .await
        .expect("hook post");
    assert_eq!(resp.status(), 400);
}

/// `POST /hooks` with an unknown event name is rejected with `422`.
///
/// Why: `HookPost.event` is a typed `HookEvent`, so an unrecognized event name
/// fails JSON deserialization in axum's `Json` extractor before the handler
/// runs — the contract is enforced by the type system, surfacing as a `422`.
#[tokio::test]
async fn hook_rejects_unknown_event_type() {
    let daemon = TestDaemon::spawn().await;
    let resp = daemon
        .client()
        .post(daemon.url("/hooks"))
        .json(&json!({
            "session_id": uuid::Uuid::new_v4().to_string(),
            "event": "WeirdEvent",
            "payload": {},
        }))
        .send()
        .await
        .expect("hook post");
    assert_eq!(resp.status(), 422);
}
