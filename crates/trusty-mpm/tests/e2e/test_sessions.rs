//! E2E: session registry — create, list, filter, remove, reap.
//!
//! Note on status codes: the daemon's `register_session` / `register_project`
//! handlers return `axum::Json`, which serialises with a `200 OK` status (the
//! OpenAPI annotations document a *semantic* `201`, but the wire status is
//! `200`). These tests assert the real wire behaviour.

use crate::harness::TestDaemon;
use serde_json::{Value, json};

/// Register a session, then confirm it appears in the listing with an id and
/// a friendly `tmpm-` name.
#[tokio::test]
async fn create_and_list_session() {
    let daemon = TestDaemon::spawn().await;
    let client = daemon.client();

    let created: Value = client
        .post(daemon.url("/sessions"))
        .json(&json!({ "project": "/tmp/e2e-create" }))
        .send()
        .await
        .expect("create session")
        .json()
        .await
        .expect("create body");

    let id = created["id"].as_str().expect("id present");
    let name = created["name"].as_str().expect("name present");
    assert!(!id.is_empty());
    assert!(name.starts_with("tmpm-"), "friendly name: {name}");

    let listed: Value = client
        .get(daemon.url("/sessions"))
        .send()
        .await
        .expect("list sessions")
        .json()
        .await
        .expect("list body");
    let sessions = listed["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], id);
}

/// The friendly session name follows the `tmpm-<adj>-<noun>` pattern.
#[tokio::test]
async fn session_has_friendly_name() {
    let daemon = TestDaemon::spawn().await;
    let created: Value = daemon
        .client()
        .post(daemon.url("/sessions"))
        .json(&json!({ "project": "/tmp/e2e-name" }))
        .send()
        .await
        .expect("create session")
        .json()
        .await
        .expect("create body");

    let name = created["name"].as_str().expect("name present");
    // Shape: `tmpm-` then two lowercase-letter words separated by a dash.
    let rest = name.strip_prefix("tmpm-").expect("tmpm- prefix");
    let parts: Vec<&str> = rest.split('-').collect();
    assert_eq!(parts.len(), 2, "expected tmpm-<adj>-<noun>, got {name}");
    for part in parts {
        assert!(!part.is_empty(), "empty word in {name}");
        assert!(
            part.chars().all(|c| c.is_ascii_lowercase()),
            "non-lowercase word in {name}"
        );
    }
}

/// Creating then deleting a session leaves the registry empty.
#[tokio::test]
async fn stop_session() {
    let daemon = TestDaemon::spawn().await;
    let client = daemon.client();

    let created: Value = client
        .post(daemon.url("/sessions"))
        .json(&json!({ "project": "/tmp/e2e-stop" }))
        .send()
        .await
        .expect("create session")
        .json()
        .await
        .expect("create body");
    let id = created["id"].as_str().expect("id present").to_string();

    let del = client
        .delete(daemon.url(&format!("/sessions/{id}")))
        .send()
        .await
        .expect("delete session");
    assert_eq!(del.status(), 200);

    let listed: Value = client
        .get(daemon.url("/sessions"))
        .send()
        .await
        .expect("list sessions")
        .json()
        .await
        .expect("list body");
    assert!(listed["sessions"].as_array().unwrap().is_empty());
}

/// Deleting a well-formed-but-unknown session id returns `404`.
#[tokio::test]
async fn stop_nonexistent_session() {
    let daemon = TestDaemon::spawn().await;
    // A valid UUID that was never registered.
    let unknown = uuid::Uuid::new_v4();
    let resp = daemon
        .client()
        .delete(daemon.url(&format!("/sessions/{unknown}")))
        .send()
        .await
        .expect("delete request");
    assert_eq!(resp.status(), 404);
}

/// A fresh daemon lists zero sessions.
#[tokio::test]
async fn list_sessions_empty() {
    let daemon = TestDaemon::spawn().await;
    let listed: Value = daemon
        .client()
        .get(daemon.url("/sessions"))
        .send()
        .await
        .expect("list sessions")
        .json()
        .await
        .expect("list body");
    assert!(
        listed["sessions"]
            .as_array()
            .expect("sessions array")
            .is_empty()
    );
}

/// A newly-registered session has an empty per-session event feed.
#[tokio::test]
async fn session_events_empty() {
    let daemon = TestDaemon::spawn().await;
    let client = daemon.client();

    let created: Value = client
        .post(daemon.url("/sessions"))
        .json(&json!({ "project": "/tmp/e2e-events" }))
        .send()
        .await
        .expect("create session")
        .json()
        .await
        .expect("create body");
    let id = created["id"].as_str().expect("id present").to_string();

    let resp = client
        .get(daemon.url(&format!("/sessions/{id}/events/poll")))
        .send()
        .await
        .expect("session events");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("events body");
    assert!(body["events"].as_array().expect("events array").is_empty());
}

/// `GET /sessions?project=X` returns only sessions of that project.
#[tokio::test]
async fn session_filter_by_project() {
    let daemon = TestDaemon::spawn().await;
    let client = daemon.client();

    for path in ["/work/alpha", "/work/beta"] {
        client
            .post(daemon.url("/sessions"))
            .json(&json!({ "project": path, "project_path": path }))
            .send()
            .await
            .expect("create session");
    }

    let all: Value = client
        .get(daemon.url("/sessions"))
        .send()
        .await
        .expect("list all")
        .json()
        .await
        .expect("all body");
    assert_eq!(all["sessions"].as_array().unwrap().len(), 2);

    let scoped: Value = client
        .get(daemon.url("/sessions"))
        .query(&[("project", "/work/alpha")])
        .send()
        .await
        .expect("list scoped")
        .json()
        .await
        .expect("scoped body");
    let scoped_sessions = scoped["sessions"].as_array().expect("scoped array");
    assert_eq!(scoped_sessions.len(), 1);
    assert_eq!(scoped_sessions[0]["project_path"], "/work/alpha");
}

/// `POST /sessions` in spawn mode (with `workdir`) must satisfy the issue
/// #93 contract on a host that lacks the `claude` binary: refuse with HTTP
/// 422 and leave the session registry empty.
///
/// Why this test is portable: CI runners do not have `claude` installed, so
/// the missing-binary branch is the deterministic one to assert at the
/// e2e (HTTP-level) seam. The happy-path is exercised in the handler-level
/// `spawn_session_without_claude_returns_422` plus the `tmux_service`
/// `spawn_claude_*` unit tests, which use a process-wide override to control
/// the lookup outcome.
#[tokio::test]
async fn spawn_session_without_claude_returns_422() {
    // Skip the test only when a real `claude` binary is on PATH (some
    // developer hosts) — there the spawn would actually run and the test
    // would no longer cover the "missing-claude" contract.
    if std::process::Command::new("which")
        .arg("claude")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        eprintln!("`claude` is on PATH; skipping 422 spawn test");
        return;
    }

    let daemon = TestDaemon::spawn().await;
    let client = daemon.client();

    let resp = client
        .post(daemon.url("/sessions"))
        .json(&json!({
            "project": "/tmp/e2e-spawn",
            "workdir": "/tmp/e2e-spawn",
        }))
        .send()
        .await
        .expect("spawn request");
    assert_eq!(resp.status(), 422, "missing claude must yield 422");

    let listed: Value = client
        .get(daemon.url("/sessions"))
        .send()
        .await
        .expect("list sessions")
        .json()
        .await
        .expect("list body");
    assert!(
        listed["sessions"].as_array().unwrap().is_empty(),
        "failed spawn must not leave a half-registered session"
    );
}

/// `DELETE /sessions/dead` returns a well-formed `{ "removed": N }` body.
///
/// The exact count depends on whether tmux is installed on the host: with tmux
/// the lone registered session (no live `tmpm-*` tmux session) is reaped;
/// without tmux nothing is reaped. Either way the response shape is fixed and
/// the registry must not afterwards contain a session tmux does not host.
#[tokio::test]
async fn reap_dead_sessions() {
    let daemon = TestDaemon::spawn().await;
    let client = daemon.client();

    client
        .post(daemon.url("/sessions"))
        .json(&json!({ "project": "/tmp/e2e-reap" }))
        .send()
        .await
        .expect("create session");

    let resp = client
        .delete(daemon.url("/sessions/dead"))
        .send()
        .await
        .expect("reap request");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("reap body");
    let removed = body["removed"].as_u64().expect("removed is a number");
    assert!(removed <= 1, "at most the one test session is reaped");

    let listed: Value = client
        .get(daemon.url("/sessions"))
        .send()
        .await
        .expect("list after reap")
        .json()
        .await
        .expect("list body");
    assert_eq!(
        listed["sessions"].as_array().unwrap().len() as u64,
        1 - removed
    );
}
