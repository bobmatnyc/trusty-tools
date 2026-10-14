//! Coverage for the trusty-memory reachability probe (#7685).
//!
//! Why: `tm doctor`'s `auto_memory` row names this answer. Every classification
//! has to be pinned, including the case the naive implementation gets wrong — a
//! daemon that answers its health call while its worker pool is wedged is NOT
//! reachable.
//! What: drives [`super::probe_socket_reachability`] against the shared stub
//! JSON-RPC daemon answering each health status.
//! Test: this file.

use serde_json::{Value, json};
use trusty_common::memory_rpc::MemoryHealthStatus;

use super::MemoryReachability;
use crate::uds_mock::RpcError;

/// A stub whose `memory.health` answers `body`.
async fn daemon_answering(body: Value) -> crate::uds_mock::MockUdsDaemon {
    crate::uds_mock::spawn(move |_method: &str, _params: Value| {
        let body = body.clone();
        Box::pin(async move { Ok(body) })
    })
    .await
}

/// Probe a stub answering `body` and return the classification.
async fn probe_body(body: Value) -> MemoryReachability {
    let daemon = daemon_answering(body).await;
    super::probe_socket_reachability(daemon.socket()).await
}

#[tokio::test]
async fn reachable_when_the_daemon_reports_ok() {
    let reach = probe_body(json!({ "status": "ok", "version": "x" })).await;
    assert!(reach.is_reachable(), "{reach:?}");
    assert_eq!(reach, MemoryReachability::Reachable);
}

#[tokio::test]
async fn wedged_daemon_is_not_reachable() {
    // #7685: the listener answers while trusty-memory reports its worker pool
    // wedged. The probe only reads that reported status string — it does not
    // itself detect a stuck palace write lock (#4001 tracks that deeper
    // observation). Reading "answered" as reachable turns auto memory OFF
    // beside a memory that has told us it is not keeping up, leaving the
    // session with none.
    let reach = probe_body(json!({
        "status": "wedged",
        "worker": { "in_flight": 3, "oldest_age_secs": 600, "wedged": true },
    }))
    .await;
    assert!(!reach.is_reachable(), "{reach:?}");
    assert_eq!(
        reach,
        MemoryReachability::Unhealthy(MemoryHealthStatus::Wedged)
    );
    assert!(reach.describe().contains("WEDGED"), "{}", reach.describe());
}

#[tokio::test]
async fn degraded_daemon_is_not_reachable() {
    let reach = probe_body(json!({ "status": "degraded", "detail": "store failed" })).await;
    assert!(!reach.is_reachable(), "{reach:?}");
    assert_eq!(
        reach,
        MemoryReachability::Unhealthy(MemoryHealthStatus::Degraded)
    );
}

#[tokio::test]
async fn malformed_health_body_is_not_reachable() {
    // A result that is not a health object at all: nothing in it says healthy.
    let reach = probe_body(json!("<html>502 bad gateway</html>")).await;
    assert!(!reach.is_reachable(), "{reach:?}");
    assert_eq!(
        reach,
        MemoryReachability::Unhealthy(MemoryHealthStatus::Missing)
    );
}

#[tokio::test]
async fn health_body_without_a_status_is_not_reachable() {
    let reach = probe_body(json!({ "version": "1.2.3", "uptime_secs": 9 })).await;
    assert!(!reach.is_reachable(), "{reach:?}");
    assert_eq!(
        reach,
        MemoryReachability::Unhealthy(MemoryHealthStatus::Missing)
    );
}

#[tokio::test]
async fn unknown_health_status_is_not_reachable() {
    let reach = probe_body(json!({ "status": "warming" })).await;
    assert!(!reach.is_reachable(), "{reach:?}");
    assert_eq!(
        reach,
        MemoryReachability::Unhealthy(MemoryHealthStatus::Unrecognised("warming".to_string()))
    );
}

#[tokio::test]
async fn refused_health_call_is_not_reachable() {
    // #7685: only a healthy status counts. A daemon that refuses its own health
    // call has said nothing about whether it can write, so the fallback stays on.
    let daemon = crate::uds_mock::spawn(|_method: &str, _params: Value| {
        Box::pin(async move { Err(RpcError::internal("no such method")) })
    })
    .await;
    let reach = super::probe_socket_reachability(daemon.socket()).await;
    assert!(!reach.is_reachable(), "{reach:?}");
    assert!(matches!(reach, MemoryReachability::Refused(_)), "{reach:?}");
}

#[tokio::test]
async fn unreachable_socket_answers_false() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let absent = tmp.path().join("no-daemon-here.sock");
    let reach = super::probe_socket_reachability(&absent).await;
    assert_eq!(reach, MemoryReachability::Unreachable);
    assert!(
        reach.describe().contains("unreachable"),
        "{}",
        reach.describe()
    );
}
