//! Coverage for the launch-time trusty-memory reachability gate (#7685).
//!
//! Why: this one boolean decides whether a managed session keeps Claude Code's
//! auto memory as a fallback or turns it off. Both directions have to be pinned,
//! including the case the naive implementation gets wrong — a daemon that is up
//! but refuses the call is REACHABLE.
//! What: drives [`super::probe_socket_reachable`] against the shared stub
//! JSON-RPC daemon, plus the blocking bridge from inside a tokio runtime.
//! Test: this file.

use serde_json::{Value, json};

use crate::uds_mock::RpcError;

/// A stub that answers `memory.health` successfully.
async fn answering_daemon() -> crate::uds_mock::MockUdsDaemon {
    crate::uds_mock::spawn(|_method: &str, _params: Value| {
        Box::pin(async move { Ok(json!({ "status": "ok" })) })
    })
    .await
}

#[tokio::test]
async fn reachable_when_the_daemon_answers() {
    let daemon = answering_daemon().await;
    assert!(super::probe_socket_reachable(daemon.socket()).await);
}

#[tokio::test]
async fn reachable_when_the_daemon_refuses_the_call() {
    // A JSON-RPC error is the daemon ANSWERING. Reading it as "down" would turn
    // auto memory back on beside a perfectly live trusty-memory.
    let daemon = crate::uds_mock::spawn(|_method: &str, _params: Value| {
        Box::pin(async move { Err(RpcError::internal("no such method")) })
    })
    .await;
    assert!(super::probe_socket_reachable(daemon.socket()).await);
}

#[tokio::test]
async fn unreachable_socket_answers_false() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let absent = tmp.path().join("no-daemon-here.sock");
    assert!(!super::probe_socket_reachable(&absent).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn blocking_probe_answers_inside_a_runtime() {
    // The bridge exists so a synchronous launch path can ask this question from
    // inside an async handler; `Handle::current().block_on` would panic there.
    let daemon = answering_daemon().await;
    let socket = daemon.socket().to_path_buf();
    assert!(
        super::block_on_probe(super::probe_socket_reachable(&socket)),
        "the blocking bridge must reach a live daemon from inside a runtime"
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let absent = tmp.path().join("no-daemon-here.sock");
    assert!(
        !super::block_on_probe(super::probe_socket_reachable(&absent)),
        "an absent socket must answer false rather than hang"
    );
}
