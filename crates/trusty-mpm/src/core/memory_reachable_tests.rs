//! Coverage for the launch-time trusty-memory reachability gate (#7685).
//!
//! Why: this one answer decides whether a managed session keeps Claude Code's
//! auto memory as a fallback or turns it off. Both directions have to be pinned,
//! including the case the naive implementation gets wrong — a daemon that
//! answers its health call while its worker pool is wedged is NOT reachable.
//! What: drives [`super::probe_socket_reachability`] against the shared stub
//! JSON-RPC daemon answering each health status, plus the blocking bridge from
//! inside a tokio runtime.
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

/// A stub that answers `memory.health` with a healthy status.
async fn answering_daemon() -> crate::uds_mock::MockUdsDaemon {
    daemon_answering(json!({ "status": "ok" })).await
}

/// Probe a stub answering `body` and return the classification.
async fn probe_body(body: Value) -> MemoryReachability {
    let daemon = daemon_answering(body).await;
    super::probe_socket_reachability(daemon.socket()).await
}

/// The blocking bridge over an explicit socket, as a boolean.
fn blocking_reachable(socket: &std::path::Path) -> bool {
    super::block_on_probe(async {
        super::probe_socket_reachability(socket)
            .await
            .is_reachable()
    })
}

#[tokio::test]
async fn reachable_when_the_daemon_reports_ok() {
    let reach = probe_body(json!({ "status": "ok", "version": "x" })).await;
    assert!(reach.is_reachable(), "{reach:?}");
    assert_eq!(reach, MemoryReachability::Reachable);
}

#[tokio::test]
async fn wedged_daemon_is_not_reachable() {
    // #7685: the #4001 state — the listener answers while a palace write lock is
    // stuck. Reading it as reachable turns auto memory OFF beside a memory that
    // cannot write, leaving the session with none.
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

#[tokio::test(flavor = "multi_thread")]
async fn resolved_reachability_skips_the_probe() {
    // #7685: the whole point of threading the launch's answer into the adapter.
    // A counted `probe_socket_reachability` against a LIVE stub proves the skip is
    // real: the socket would answer `true` if anything dialled it, so a `false`
    // verdict with a zero count can only come from the injected value.
    let daemon = answering_daemon().await;
    let socket = daemon.socket().to_path_buf();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let probe = || {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        blocking_reachable(&socket)
    };

    let resolved = super::resolve_memory_reachable_with(Some(false), probe);

    assert!(
        !resolved,
        "the caller's resolved value must be taken verbatim"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a resolved reachability must dial no socket at all"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unresolved_reachability_runs_the_probe() {
    // The other half: `None` still means "ask the host", so collapsing the
    // second probe cannot have silently removed the first.
    let daemon = answering_daemon().await;
    let socket = daemon.socket().to_path_buf();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let probe = || {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        blocking_reachable(&socket)
    };

    assert!(super::resolve_memory_reachable_with(None, probe));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn a_panicking_probe_answers_false_without_propagating() {
    // #7685: the bridge's fail-safe direction. A panic inside the probe must
    // come back as `false` — auto memory stays ON — rather than unwinding into
    // a launch path that has no business dying over a health check.
    assert!(
        !super::block_on_probe(async { panic!("probe blew up") }),
        "a panicking probe must answer false"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn blocking_probe_answers_inside_a_runtime() {
    // The bridge exists so a synchronous launch path can ask this question from
    // inside an async handler; `Handle::current().block_on` would panic there.
    let daemon = answering_daemon().await;
    let socket = daemon.socket().to_path_buf();
    assert!(
        blocking_reachable(&socket),
        "the blocking bridge must reach a live daemon from inside a runtime"
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let absent = tmp.path().join("no-daemon-here.sock");
    assert!(
        !blocking_reachable(&absent),
        "an absent socket must answer false rather than hang"
    );
}
