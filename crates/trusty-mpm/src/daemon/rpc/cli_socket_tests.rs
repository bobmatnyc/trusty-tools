//! Tests for the #6288 step-1 socket methods, driven through the real client
//! transport against a real served socket.

use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::json;

use crate::client::DaemonClient;
use crate::core::paths::FrameworkPaths;
use crate::daemon::managed_routes::adopt_worktree::{AdoptWorktreeRequest, adopt_worktree_core};
use crate::daemon::state::DaemonState;
use crate::session_manager::record::ManagedSessionId;

/// A daemon serving only its unix socket, and the client for it.
struct SocketDaemon {
    client: DaemonClient,
    state: Arc<DaemonState>,
    _dir: tempfile::TempDir,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for SocketDaemon {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}

async fn socket_daemon() -> SocketDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(DaemonState::with_paths(&FrameworkPaths::under(dir.path())));
    let socket = dir.path().join("sock").join("trusty-mpm.sock");
    let bound = crate::daemon::socket::bind(&socket).await.expect("bind");
    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    let served = Arc::clone(&state);
    tokio::spawn(crate::daemon::socket::serve_until_shutdown(
        bound,
        served,
        async {
            let _ = shutdown.await;
        },
    ));
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(50)).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    SocketDaemon {
        client: DaemonClient::over_socket(&socket),
        state,
        _dir: dir,
        stop: Some(stop),
    }
}

/// `tm build-lease`'s decision post is acknowledged over the socket.
#[tokio::test]
async fn rpc_build_lease_decision_is_acknowledged() {
    let daemon = socket_daemon().await;
    let resp = daemon
        .client
        .post("/api/v1/build-lease/decisions")
        .json(&json!({ "verdict": "admitted", "command": "cargo test", "ceiling": 2 }))
        .send()
        .await
        .expect("the socket answers");
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Both retired builder-slot routes refuse with 410 and the HTTP body's text.
#[tokio::test]
async fn rpc_builder_slot_methods_answer_gone() {
    let daemon = socket_daemon().await;
    for resp in [
        daemon
            .client
            .post("/api/v1/sessions/5f0e2c1a-1111-4222-8333-944445555666/delegations/builder-slot")
            .json(&json!({ "payload": {} }))
            .send()
            .await
            .expect("answered"),
        daemon
            .client
            .get("/api/v1/builder-slots")
            .send()
            .await
            .expect("answered"),
    ] {
        assert_eq!(resp.status(), StatusCode::GONE);
        let text = resp.text().await.expect("text");
        assert_eq!(text, crate::daemon::builder_slot_routes::RETIRED_MESSAGE);
        assert!(!text.contains("slot_path"), "{text}");
    }
}

/// adopt-worktree answers over the socket exactly what the shared body answers.
#[tokio::test]
async fn rpc_adopt_worktree_refuses_like_http() {
    let daemon = socket_daemon().await;
    let tree = tempfile::tempdir().expect("tree");
    let body = json!({ "path": tree.path(), "as_session": ManagedSessionId::new() });

    let direct = adopt_worktree_core(
        &daemon.state,
        serde_json::from_value::<AdoptWorktreeRequest>(body.clone()).expect("request"),
    )
    .await;
    let resp = daemon
        .client
        .post("/api/v1/sessions/managed/adopt-worktree")
        .json(&body)
        .send()
        .await
        .expect("answered");
    if direct.is_success() {
        // The socket has no created-versus-ok distinction.
        assert!(resp.status().is_success(), "{}", resp.status());
    } else {
        assert_eq!(resp.status().as_u16(), direct.status);
    }
}
