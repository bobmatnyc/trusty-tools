//! Integration tests for [`super::TmConnector`] (DOC-44 twin Phase 1, issue
//! #3007 test-layer (b)).
//!
//! Why: drives the connector against a REAL in-process daemon HTTP surface
//! (a real axum router bound to a random loopback port) rather than mocking
//! `reqwest` — the whole point of this connector is the wire contract with
//! the daemon's actual routes, so a mock would only prove the mock is
//! self-consistent. Mirrors `crates/trusty-mpm/src/client/proxy/tests.rs`'s
//! `spawn_test_daemon` pattern exactly (its own doc calls this "the isolated-
//! managed helper", duplicated here rather than shared because it is a
//! four-line, module-private test helper — not worth a cross-module `pub`
//! surface for).
//! What: session-lifecycle-independent tests (unknown-id error mapping,
//! wrong-`BackendParams` client-side rejection) run first, needing no git
//! binary. [`create_session_full_lifecycle`] is the one end-to-end test that
//! exercises the REAL `WorkspaceProvisioner`/`RealGitBackend` path — the
//! daemon's `spawn_managed_cloned` handler always uses `RealGitBackend`
//! (`crates/trusty-mpm/src/daemon/managed_routes/lifecycle.rs:564`), so a
//! hermetic spawn test needs a real (but local-only, no network) git repo —
//! mirroring `tests/session_manager_mvp.rs`'s `live_provision_real_repo`
//! fixture, except NOT `#[ignore]`-gated: cloning `file://<local bare repo>`
//! needs only the `git` binary already required to develop this workspace,
//! never the network. `with_root_isolated_managed`'s `FakeNoopTmuxDriver`
//! fakes every tmux operation (create/send/capture/list), so no real tmux is
//! ever touched.
//! Test: this file IS the test module (`#[path = "tm_tests.rs"] mod tests;`
//! in `tm.rs`).

use std::future::IntoFuture;
use std::process::Command;

use tempfile::TempDir;
use trusty_agents_common::connectors::{AgentSpec, BackendParams, CreateSessionReq};

use super::*;

/// Spawn the daemon's real HTTP API on a random loopback port (empty fleet,
/// tmux faked). Mirrors `client::proxy::tests::spawn_test_daemon`, but —
/// unlike that helper — serves with `into_make_service_with_connect_info`
/// (matching `daemon::mod::run_http`'s real production wiring), because this
/// connector's `delegate` method calls the loopback-gated `POST /rpc` route,
/// which extracts `ConnectInfo<SocketAddr>` to enforce that gate (see
/// `daemon::api::rpc::rpc_handler`'s docs) — a plain `axum::serve(listener,
/// router)` never populates that extension and every `/rpc` call would 500.
async fn spawn_test_daemon() -> String {
    use crate::daemon::{api, state::DaemonState};
    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let router = api::router(std::sync::Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .into_future(),
    );
    format!("http://{addr}")
}

/// Build a local, offline-clonable bare git repo with one commit on `main`.
///
/// Why: [`create_session_full_lifecycle`] needs a `repo_url` the daemon's
/// REAL `RealGitBackend` can clone without ever touching the network.
/// What: `git init --bare -b main`, then clone/commit/push through a scratch
/// checkout — mirrors `tests/session_manager_mvp.rs`'s
/// `live_provision_real_repo` fixture. Returns the `TempDir` (kept alive for
/// the caller's duration) and the `file://` URL to the bare repo.
fn local_bare_repo() -> (TempDir, String) {
    let scratch = TempDir::new().expect("scratch tempdir");
    let bare = scratch.path().join("origin.git");
    let work = scratch.path().join("seed");
    assert!(
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .arg(&bare)
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
        "git init --bare must succeed"
    );
    assert!(
        Command::new("git")
            .args(["clone"])
            .arg(&bare)
            .arg(&work)
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
        "git clone (seed checkout) must succeed"
    );
    std::fs::write(work.join("README.md"), "seed").expect("write seed file");
    for args in [
        vec!["-C", work.to_str().unwrap(), "add", "."],
        vec![
            "-C",
            work.to_str().unwrap(),
            "-c",
            "user.email=connector-test@example.com",
            "-c",
            "user.name=connector-test",
            "commit",
            "-m",
            "seed",
        ],
        vec!["-C", work.to_str().unwrap(), "push", "origin", "main"],
    ] {
        assert!(
            Command::new("git")
                .args(&args)
                .status()
                .map(|s| s.success())
                .unwrap_or(false),
            "git {args:?} must succeed"
        );
    }
    let url = format!("file://{}", bare.display());
    (scratch, url)
}

/// `CreateSessionReq::backend` carrying `BackendParams::Tcode` must be
/// rejected client-side, before any HTTP call — a caller bug, not a daemon
/// round-trip.
#[tokio::test]
async fn create_session_wrong_backend_params_is_invalid_request() {
    let connector = TmConnector::with_daemon_url("http://127.0.0.1:0");
    let req = CreateSessionReq {
        task: "irrelevant".into(),
        name_hint: None,
        agent: None,
        backend: BackendParams::Tcode {
            project: std::path::PathBuf::from("/tmp/proj"),
        },
    };
    let err = connector.create_session(req).await.unwrap_err();
    assert!(
        matches!(err, ConnectorError::InvalidRequest(_)),
        "expected InvalidRequest, got {err:?}"
    );
}

#[tokio::test]
async fn list_sessions_empty_fleet_returns_empty_vec() {
    let url = spawn_test_daemon().await;
    let connector = TmConnector::with_daemon_url(url);
    let sessions = connector.list_sessions().await.expect("list_sessions");
    assert!(sessions.is_empty(), "fresh daemon must have no sessions");
}

#[tokio::test]
async fn session_status_unknown_id_is_not_found() {
    let url = spawn_test_daemon().await;
    let connector = TmConnector::with_daemon_url(url);
    let err = connector
        .session_status("00000000-0000-0000-0000-000000000000")
        .await
        .unwrap_err();
    assert!(err.is_not_found(), "expected NotFound, got {err:?}");
}

#[tokio::test]
async fn send_input_unknown_id_is_not_found() {
    let url = spawn_test_daemon().await;
    let connector = TmConnector::with_daemon_url(url);
    let err = connector
        .send_input("00000000-0000-0000-0000-000000000000", "hi")
        .await
        .unwrap_err();
    assert!(err.is_not_found(), "expected NotFound, got {err:?}");
}

#[tokio::test]
async fn attach_unknown_id_is_not_found() {
    let url = spawn_test_daemon().await;
    let connector = TmConnector::with_daemon_url(url);
    let err = connector
        .attach("00000000-0000-0000-0000-000000000000")
        .await
        .unwrap_err();
    assert!(err.is_not_found(), "expected NotFound, got {err:?}");
}

/// `delegate` against an unknown session must surface as a `Backend` error
/// (the `agent_delegate` MCP tool's own "no such session" domain rejection,
/// carried through the `tools/call` `isError: true` convention — never a
/// panic or a malformed-envelope `Transport` error).
#[tokio::test]
async fn delegate_unknown_session_is_backend_error() {
    let url = spawn_test_daemon().await;
    let connector = TmConnector::with_daemon_url(url);
    let spec = AgentSpec {
        agent_name: "research".into(),
        task: "find the bug".into(),
        tier: None,
    };
    let err = connector
        .delegate("00000000-0000-0000-0000-000000000000", &spec)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ConnectorError::Backend(_)),
        "expected Backend error for an unknown session, got {err:?}"
    );
}

/// End-to-end: create -> list -> status -> send_input -> attach -> delegate,
/// against a REAL daemon spawn (real git clone via `RealGitBackend`, faked
/// tmux via `FakeNoopTmuxDriver`).
#[tokio::test]
async fn create_session_full_lifecycle() {
    let (_scratch, repo_url) = local_bare_repo();
    let url = spawn_test_daemon().await;
    let connector = TmConnector::with_daemon_url(url);

    let req = CreateSessionReq {
        task: "list files".into(),
        name_hint: None,
        agent: None,
        backend: BackendParams::Tm {
            repo_url,
            git_ref: "main".into(),
            runtime: None,
            ephemeral: true,
        },
    };
    let info = connector
        .create_session(req)
        .await
        .expect("create_session must succeed against a real local git repo");
    assert!(!info.id.is_empty(), "created session must have an id");
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

    connector
        .send_input(&info.id, "echo hello")
        .await
        .expect("send_input must succeed for a live session");

    let attach = connector.attach(&info.id).await.expect("attach");
    match attach {
        AttachHandle::ShellCommand(cmd) => {
            assert!(
                cmd.contains("tmux attach"),
                "tm attach handle must be a tmux attach command, got {cmd:?}"
            );
        }
        other => panic!("tm connector must return ShellCommand, got {other:?}"),
    }

    let spec = AgentSpec {
        agent_name: "research".into(),
        task: "find the bug".into(),
        tier: Some("haiku".into()),
    };
    let handle = connector
        .delegate(&info.id, &spec)
        .await
        .expect("delegate must succeed against a live session");
    assert!(
        !handle.delegate_id.is_empty(),
        "delegate must return a non-empty delegate_id"
    );
}
