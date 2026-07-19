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
//! before any HTTP call is made). The unknown-id/not-found, `delegate`-
//! unsupported, and create->list->status->send_input assertions run through
//! [`ConnectorTestKit`] (shared with
//! `crates/trusty-mpm/src/connectors/tm_tests.rs`) rather than being
//! hand-rolled — only `attach`'s tcode-specific `EventStream` shape (which
//! the kit deliberately does not model, since tm's `attach` returns a
//! different shape entirely) stays hand-rolled here.
//! Test: this file IS the test; see `support` for the process/protocol
//! plumbing shared with every other `*_e2e.rs` file in this crate.

mod support;

use trusty_agents_common::connectors::{
    AgentSpec, AttachHandle, BackendParams, ConnectorError, ConnectorTestKit, CreateSessionReq,
    WorkstreamConnector,
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
/// works even against an address nothing is listening on. Shared conformance
/// assertion — see [`ConnectorTestKit::assert_delegate_not_supported`].
#[tokio::test]
async fn delegate_is_not_supported() {
    let connector = TcodeConnector::with_daemon_url("http://127.0.0.1:0");
    let spec = AgentSpec {
        agent_name: "research".into(),
        task: "find the bug".into(),
        tier: None,
    };
    ConnectorTestKit::assert_delegate_not_supported(&connector, "does-not-matter", &spec).await;
}

#[tokio::test]
async fn list_sessions_empty_fleet_returns_empty_vec() {
    let daemon = support::spawn_http_daemon().await;
    let connector = TcodeConnector::with_daemon_url(daemon.base_url.clone());
    let sessions = connector.list_sessions().await.expect("list_sessions");
    assert!(sessions.is_empty(), "fresh daemon must have no sessions");
}

/// Shared conformance assertion — see [`ConnectorTestKit::assert_status_not_found_for_unknown_id`].
#[tokio::test]
async fn session_status_unknown_id_is_not_found() {
    let daemon = support::spawn_http_daemon().await;
    let connector = TcodeConnector::with_daemon_url(daemon.base_url.clone());
    ConnectorTestKit::assert_status_not_found_for_unknown_id(&connector, "does-not-exist").await;
}

/// Shared conformance assertion — see [`ConnectorTestKit::assert_send_not_found_for_unknown_id`].
#[tokio::test]
async fn send_input_unknown_id_is_not_found() {
    let daemon = support::spawn_http_daemon().await;
    let connector = TcodeConnector::with_daemon_url(daemon.base_url.clone());
    ConnectorTestKit::assert_send_not_found_for_unknown_id(&connector, "does-not-exist").await;
}

#[tokio::test]
async fn attach_unknown_id_is_not_found() {
    let daemon = support::spawn_http_daemon().await;
    let connector = TcodeConnector::with_daemon_url(daemon.base_url.clone());
    let err = connector.attach("does-not-exist").await.unwrap_err();
    assert!(err.is_not_found(), "expected NotFound, got {err:?}");
}

/// End-to-end: create -> list -> status -> send_input (via
/// [`ConnectorTestKit::assert_basic_lifecycle`]) -> attach, against a REAL
/// `tcode serve --http` daemon.
#[tokio::test]
async fn create_session_full_lifecycle() {
    let daemon = support::spawn_http_daemon().await;
    let connector = TcodeConnector::with_daemon_url(daemon.base_url.clone());
    // A dedicated tempdir (not the shared OS temp root) held alive for the
    // whole test, mirroring `tm_tests.rs::local_bare_repo`'s TempDir-guard
    // pattern — `session.create`'s `ProjectBinding::resolve` just needs SOME
    // real, exclusively-ours directory to bind to.
    let project_dir = tempfile::tempdir().expect("project tempdir");

    let req = CreateSessionReq {
        task: "list files".into(),
        name_hint: None,
        agent: None,
        backend: BackendParams::Tcode {
            project: project_dir.path().to_path_buf(),
        },
    };
    let info =
        ConnectorTestKit::assert_basic_lifecycle(&connector, req, "hello from the connector").await;
    assert_eq!(info.state, "running", "M1 sessions go straight to running");
    assert_eq!(
        info.task.as_deref(),
        Some("list files"),
        "the kit's lifecycle assertion doesn't check task content — do it here"
    );

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
