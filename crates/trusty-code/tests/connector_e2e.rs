//! API-driven end-to-end tests for [`trusty_code::session::TcodeConnector`]
//! (DOC-44 twin Phase 1, issue #3007 test-layer (c)).
//!
//! Why: mirrors `tests/session_e2e.rs`'s black-box discipline (vision spec
//! Testability requirement §9) — drives the REAL `tcode serve --http`
//! binary over its real HTTP surface via `support::spawn_http_daemon`,
//! never calling into `trusty_code`'s Rust API directly except through the
//! connector itself (the thing under test). Lives at the top level
//! (`tests/connector_e2e.rs`), not inside `src/`, because this crate has no
//! unit-test-reachable seam for spawning the real compiled binary — the
//! same reason `session_e2e.rs`/`task_e2e.rs` live here.
//! What: create -> list -> status -> send_input -> attach against a live
//! daemon, plus the unknown-id error-mapping and the two backend-parameter/
//! `delegate` checks that need no daemon at all (both are rejected/refused
//! before any HTTP call is made).
//! Test: this file IS the test; see `support` for the process/protocol
//! plumbing shared with every other `*_e2e.rs` file in this crate.

mod support;

use std::path::PathBuf;

use trusty_agents_common::connectors::{
    AgentSpec, AttachHandle, BackendParams, ConnectorError, CreateSessionReq, WorkstreamConnector,
};
use trusty_code::session::TcodeConnector;

/// `CreateSessionReq::backend` carrying `BackendParams::Tm` must be rejected
/// client-side — a caller bug, not a daemon round-trip. Needs no daemon.
#[tokio::test]
async fn create_session_wrong_backend_params_is_invalid_request() {
    let connector = TcodeConnector::with_daemon_url("http://127.0.0.1:0");
    let req = CreateSessionReq {
        task: "irrelevant".into(),
        name_hint: None,
        agent: None,
        backend: BackendParams::Tm {
            repo_url: "https://example/repo".into(),
            git_ref: "main".into(),
            runtime: None,
            ephemeral: false,
        },
    };
    let err = connector.create_session(req).await.unwrap_err();
    assert!(
        matches!(err, ConnectorError::InvalidRequest(_)),
        "expected InvalidRequest, got {err:?}"
    );
}

/// `delegate` must always be `NotSupported`, with no daemon call at all —
/// works even against an address nothing is listening on.
#[tokio::test]
async fn delegate_is_not_supported() {
    let connector = TcodeConnector::with_daemon_url("http://127.0.0.1:0");
    let spec = AgentSpec {
        agent_name: "research".into(),
        task: "find the bug".into(),
        tier: None,
    };
    let err = connector
        .delegate("does-not-matter", &spec)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ConnectorError::NotSupported(_)),
        "expected NotSupported, got {err:?}"
    );
}

#[tokio::test]
async fn list_sessions_empty_fleet_returns_empty_vec() {
    let daemon = support::spawn_http_daemon().await;
    let connector = TcodeConnector::with_daemon_url(daemon.base_url.clone());
    let sessions = connector.list_sessions().await.expect("list_sessions");
    assert!(sessions.is_empty(), "fresh daemon must have no sessions");
}

#[tokio::test]
async fn session_status_unknown_id_is_not_found() {
    let daemon = support::spawn_http_daemon().await;
    let connector = TcodeConnector::with_daemon_url(daemon.base_url.clone());
    let err = connector
        .session_status("does-not-exist")
        .await
        .unwrap_err();
    assert!(err.is_not_found(), "expected NotFound, got {err:?}");
}

#[tokio::test]
async fn send_input_unknown_id_is_not_found() {
    let daemon = support::spawn_http_daemon().await;
    let connector = TcodeConnector::with_daemon_url(daemon.base_url.clone());
    let err = connector
        .send_input("does-not-exist", "hi")
        .await
        .unwrap_err();
    assert!(err.is_not_found(), "expected NotFound, got {err:?}");
}

#[tokio::test]
async fn attach_unknown_id_is_not_found() {
    let daemon = support::spawn_http_daemon().await;
    let connector = TcodeConnector::with_daemon_url(daemon.base_url.clone());
    let err = connector.attach("does-not-exist").await.unwrap_err();
    assert!(err.is_not_found(), "expected NotFound, got {err:?}");
}

/// End-to-end: create -> list -> status -> send_input -> attach, against a
/// REAL `tcode serve --http` daemon.
#[tokio::test]
async fn create_session_full_lifecycle() {
    let daemon = support::spawn_http_daemon().await;
    let connector = TcodeConnector::with_daemon_url(daemon.base_url.clone());
    let project: PathBuf = std::env::temp_dir();

    let req = CreateSessionReq {
        task: "list files".into(),
        name_hint: None,
        agent: None,
        backend: BackendParams::Tcode { project },
    };
    let info = connector
        .create_session(req)
        .await
        .expect("create_session must succeed against a real project directory");
    assert!(!info.id.is_empty(), "created session must have an id");
    assert_eq!(info.state, "running", "M1 sessions go straight to running");
    assert_eq!(info.task.as_deref(), Some("list files"));

    let listed = connector.list_sessions().await.expect("list_sessions");
    assert!(
        listed.iter().any(|s| s.id == info.id),
        "list_sessions must include the just-created session"
    );

    let status = connector
        .session_status(&info.id)
        .await
        .expect("session_status");
    assert_eq!(status.id, info.id);
    assert_eq!(status.state, "running");

    connector
        .send_input(&info.id, "hello from the connector")
        .await
        .expect("send_input must succeed for a live session");

    let attach = connector.attach(&info.id).await.expect("attach");
    match attach {
        AttachHandle::EventStream {
            session_id,
            stream_url,
            ..
        } => {
            assert_eq!(session_id, info.id);
            assert_eq!(stream_url, format!("/sessions/{}/events", info.id));
        }
        other => panic!("tcode connector must return EventStream, got {other:?}"),
    }
}
