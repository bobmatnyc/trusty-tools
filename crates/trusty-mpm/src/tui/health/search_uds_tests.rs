//! The health screen's trusty-search legs over the daemon's Unix socket (#9214).
//!
//! Why: ADR-0032 retires trusty-search's TCP listener, so every search leg of
//! the health client — poll, counts, collections, log tail, stop — has to reach
//! the daemon over its socket. These tests stand up a `uds_mock` daemon and
//! drive the real client against it; no TCP listener is involved.
//! What: one test per leg plus one per fail-open branch (a refused index list,
//! a refused log tail, a refused stop).
//! Test: this file is the test suite.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::probes::socket_or_unreachable;
use super::{Daemon, PanelState, client_for, resolve_search_socket_or_unreachable};
use crate::uds_mock::{self, MockUdsDaemon, RpcError};

/// Every `(method, params)` pair the mock daemon received, in arrival order.
type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// A mock daemon's answer for one call.
type Answer = fn(&str, &Value) -> Result<Value, RpcError>;

/// Start a mock search daemon that answers through `answer` and records calls.
async fn search_mock(answer: Answer) -> (MockUdsDaemon, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&calls);
    let daemon = uds_mock::spawn(move |method: &str, params: Value| {
        let recorder = Arc::clone(&recorder);
        let method = method.to_string();
        Box::pin(async move {
            let reply = answer(&method, &params);
            recorder
                .lock()
                .expect("call recorder")
                .push((method, params));
            reply
        })
    })
    .await;
    (daemon, calls)
}

/// The method names `calls` recorded.
fn methods(calls: &Calls) -> Vec<String> {
    calls
        .lock()
        .expect("call recorder")
        .iter()
        .map(|(m, _)| m.clone())
        .collect()
}

/// A healthy daemon with three indexes; `gamma`'s status is refused.
fn healthy(method: &str, params: &Value) -> Result<Value, RpcError> {
    match method {
        "search.health" => Ok(json!({
            "version": "9.9.9", "rss_mb": 64, "cpu_pct": 1.5,
            "uptime_secs": 10, "disk_bytes": 2048,
        })),
        "search.indexes.list" => Ok(json!({ "indexes": ["alpha", "beta", "gamma"] })),
        "search.index.status" => match params.get("index_id").and_then(Value::as_str) {
            Some("alpha") => Ok(json!({
                "chunk_count": 10, "disk_bytes": 100, "has_context_embedding": true,
            })),
            Some("beta") => Ok(json!({ "chunk_count": 32 })),
            _ => Err(RpcError::internal("status refused")),
        },
        "search.graph.stats" => Ok(json!({
            "node_count": 5, "edge_count": 7, "edge_kinds": { "contains": 3, "calls": 4 },
        })),
        "search.logs.tail" => Ok(json!({ "lines": ["one", "two"], "total": 9 })),
        "search.admin.stop" => Ok(json!({ "stopping": true })),
        other => Err(RpcError::internal(format!("unexpected method {other}"))),
    }
}

/// A daemon that answers health and refuses everything else.
fn health_only(method: &str, params: &Value) -> Result<Value, RpcError> {
    match method {
        "search.health" => healthy(method, params),
        _ => Err(RpcError::internal("refused")),
    }
}

/// A daemon that refuses every call.
fn refuses_all(_method: &str, _params: &Value) -> Result<Value, RpcError> {
    Err(RpcError::internal("refused"))
}

/// Why: acceptance criterion 2 — the poller must reach search over the socket
/// that `TRUSTY_SEARCH_SOCKET` names, and project the index counts.
/// What: points the env override at a mock, resolves the socket with
/// [`resolve_search_socket_or_unreachable`] as the TUI does, polls, and asserts `Online` with 3 indexes and 42 chunks (`gamma`'s
/// refused status contributes no chunks but still counts as an index).
/// Test: this IS the test.
#[tokio::test]
#[serial_test::serial]
async fn search_poll_over_the_socket_is_online_with_index_counts() {
    let (daemon, calls) = search_mock(healthy).await;
    unsafe {
        std::env::set_var(
            trusty_common::search_rpc::TRUSTY_SEARCH_SOCKET_ENV,
            daemon.socket(),
        );
    }
    let socket = resolve_search_socket_or_unreachable();
    unsafe {
        std::env::remove_var(trusty_common::search_rpc::TRUSTY_SEARCH_SOCKET_ENV);
    }
    assert_eq!(socket, daemon.socket());

    let state = client_for(Daemon::Search, &socket.display().to_string())
        .poll()
        .await;
    let PanelState::Online(data) = state else {
        panic!("expected Online, got {state:?}");
    };
    assert_eq!(data.version, "9.9.9");
    assert_eq!(data.rss_mb, 64);
    assert_eq!((data.count_a, data.count_b), (3, 42));
    assert!(methods(&calls).contains(&"search.health".to_string()));
}

/// Why: Fail-Open Check — a refused index list must degrade the counts to zero
/// while the panel still reports Online, as the HTTP client did.
/// Test: this IS the test.
#[tokio::test]
async fn search_counts_degrade_to_zero_when_the_index_list_is_refused() {
    let (daemon, calls) = search_mock(health_only).await;
    let state = client_for(Daemon::Search, &daemon.socket().display().to_string())
        .poll()
        .await;
    let PanelState::Online(data) = state else {
        panic!("expected Online, got {state:?}");
    };
    assert_eq!((data.count_a, data.count_b), (0, 0));
    assert!(methods(&calls).contains(&"search.indexes.list".to_string()));
}

/// Why: the Collections pane reads per-index status and graph stats over the
/// socket; the dead `/communities` route has no socket method, so the
/// community fields stay at zero and nothing asks for them.
/// Test: this IS the test.
#[tokio::test]
async fn search_collections_over_the_socket_read_status_and_graph_stats() {
    let (daemon, calls) = search_mock(healthy).await;
    let rows = client_for(Daemon::Search, &daemon.socket().display().to_string())
        .search_collections()
        .await;
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["alpha", "beta", "gamma"]);

    let alpha = &rows[0];
    assert_eq!(alpha.count, 10);
    assert_eq!(alpha.disk_bytes, 100);
    assert!(alpha.has_context_embedding);
    assert_eq!((alpha.node_count, alpha.edge_count), (5, 7));
    assert_eq!(
        alpha.edge_kinds,
        vec![("calls".to_string(), 4), ("contains".to_string(), 3)]
    );
    assert_eq!(alpha.community_count, 0);
    assert_eq!(rows[1].count, 32);
    assert_eq!(rows[2].count, 0, "a refused status reads as zero chunks");

    let recorded = calls.lock().expect("call recorder").clone();
    assert!(
        recorded
            .iter()
            .any(|(m, p)| m == "search.graph.stats" && p["index_id"] == "alpha"),
        "graph stats are asked per index: {recorded:?}"
    );
    assert!(
        !recorded.iter().any(|(m, _)| m.contains("communit")),
        "no socket method serves communities: {recorded:?}"
    );
}

/// Why: Fail-Open Check — a refused index list yields an empty Collections
/// pane rather than an error.
/// Test: this IS the test.
#[tokio::test]
async fn search_collections_are_empty_when_the_index_list_is_refused() {
    let (daemon, calls) = search_mock(refuses_all).await;
    let rows = client_for(Daemon::Search, &daemon.socket().display().to_string())
        .search_collections()
        .await;
    assert!(rows.is_empty());
    assert_eq!(methods(&calls), vec!["search.indexes.list".to_string()]);
}

/// Why: the Logs tab tails the daemon's ring over the socket.
/// Test: this IS the test.
#[tokio::test]
async fn search_logs_tail_over_the_socket_reads_the_page() {
    let (daemon, calls) = search_mock(healthy).await;
    let page = client_for(Daemon::Search, &daemon.socket().display().to_string())
        .logs_tail(50)
        .await
        .expect("logs_tail never errors");
    assert_eq!(page, (vec!["one".to_string(), "two".to_string()], 9));
    let recorded = calls.lock().expect("call recorder").clone();
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].0, "search.logs.tail");
    assert_eq!(recorded[0].1["n"], 50);
}

/// Why: acceptance criterion 5 / Fail-Open Check — a refused tail degrades to
/// an empty page, not an error, so the tab shows its placeholder.
/// Test: this IS the test.
#[tokio::test]
async fn search_logs_tail_refusal_degrades_to_an_empty_page() {
    let (daemon, calls) = search_mock(refuses_all).await;
    let page = client_for(Daemon::Search, &daemon.socket().display().to_string())
        .logs_tail(50)
        .await
        .expect("a refused tail is not an error");
    assert_eq!(page, (Vec::new(), 0));
    assert_eq!(methods(&calls), vec!["search.logs.tail".to_string()]);
}

/// Why: acceptance criterion 4 — `[X]` sends the daemon's stop method.
/// Test: this IS the test.
#[tokio::test]
async fn search_stop_sends_the_stop_method() {
    let (daemon, calls) = search_mock(healthy).await;
    client_for(Daemon::Search, &daemon.socket().display().to_string())
        .stop()
        .await
        .expect("the daemon accepted the stop");
    assert_eq!(methods(&calls), vec!["search.admin.stop".to_string()]);
}

/// Why: acceptance criterion 4 — a daemon refusal surfaces as `Err`, so the
/// dashboard reports the failure instead of "stopping…".
/// Test: this IS the test.
#[tokio::test]
async fn search_stop_surfaces_a_daemon_refusal_as_err() {
    let (daemon, calls) = search_mock(refuses_all).await;
    let outcome = client_for(Daemon::Search, &daemon.socket().display().to_string())
        .stop()
        .await;
    assert!(outcome.is_err(), "a refused stop must be an error");
    assert_eq!(methods(&calls), vec!["search.admin.stop".to_string()]);
}

/// Why: Fail-Open Check / acceptance criterion 6 — a search socket that cannot
/// be resolved must not panic the TUI; the panel shows Offline instead.
/// What: feeds the fallback an `Err`, asserts it yields a path, and polls that
/// path to `Offline`.
/// Test: this IS the test.
#[tokio::test]
async fn an_unresolvable_search_socket_falls_back_to_a_dead_path() {
    let socket = socket_or_unreachable(Err(anyhow::anyhow!("no data dir")));
    assert!(socket.is_absolute(), "fallback is a path: {socket:?}");
    assert!(!socket.exists(), "nothing binds the fallback: {socket:?}");
    let state = client_for(Daemon::Search, &socket.display().to_string())
        .poll()
        .await;
    assert!(
        matches!(state, PanelState::Offline { ref last_error } if !last_error.is_empty()),
        "expected Offline with a reason, got {state:?}"
    );
}

/// Why: an `Ok` resolution passes through untouched.
/// Test: this IS the test.
#[test]
fn a_resolved_search_socket_passes_through() {
    let path = std::path::PathBuf::from("/tmp/trusty-search-test.sock");
    assert_eq!(socket_or_unreachable(Ok(path.clone())), path);
}

/// Why: a stopped search daemon renders Offline with the dial error, the
/// branch every transport failure in `poll` takes.
/// Test: this IS the test.
#[tokio::test]
async fn search_poll_over_a_missing_socket_is_offline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("missing.sock");
    let state = client_for(Daemon::Search, &socket.display().to_string())
        .poll()
        .await;
    assert!(
        matches!(state, PanelState::Offline { ref last_error } if !last_error.is_empty()),
        "expected Offline with a reason, got {state:?}"
    );
}
