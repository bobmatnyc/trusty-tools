//! Tests for `console.machine_status` and `console.history` (#10942, #10943).
//!
//! Why: the two methods must answer the HTTP routes' data from the same
//! caches, answer a cold cache without an error, and refuse a malformed or
//! unknown request with a JSON-RPC error rather than a hang.
//! What: router-level dispatch, equality with the HTTP bodies, and wire tests
//! over a real hardened socket, in the style of `tests.rs`.
//! Test: this module IS the suite for the metrics half of `console_uds/mod.rs`.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tower::ServiceExt;
use trusty_common::console_metrics::{ServiceHealth, make_report};
use trusty_common::console_rpc::metrics::{ConsoleMachineStatus, HistorySnapshot};
use trusty_common::console_rpc::{METHOD_HISTORY, METHOD_MACHINE_STATUS};
use trusty_common::host_metrics::HostSampler;
use trusty_common::host_metrics::history::HOST_HISTORY_CAPACITY;
use trusty_common::uds::send_framed_request;
use trusty_common::uds::server::{
    CODE_INVALID_PARAMS, CODE_METHOD_NOT_FOUND, CODE_PARSE_ERROR, RpcResponse,
};

use super::*;
use crate::connector::ServiceStatus;
use crate::machine_history::service_samples::{ServiceSample, ServiceSampleBatch};

const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// A state with no connectors: these methods never read the services roster.
fn state() -> AppState {
    AppState::new(Vec::new())
}

async fn dispatch(router: &RpcRouter, method: &str, params: Value) -> RpcResponse {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    router.dispatch(frame.to_string().as_bytes()).await
}

/// `GET uri` against the HTTP router; answers the status and the JSON body.
async fn http_json(state: &AppState, uri: &str) -> (StatusCode, Option<Value>) {
    let response = crate::server::build_router(state.clone())
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, serde_json::from_slice(&bytes).ok())
}

/// `value` as a client decodes it off the wire: encoded to text, then parsed.
///
/// `serde_json::to_value` widens an `f32` to `f64` exactly
/// (`12.74902629852295`), while text carries the shortest `f32` form
/// (`12.749026`); both sides must take the same path to compare equal.
fn wire_json(value: &impl serde::Serialize) -> Value {
    let text = serde_json::to_string(value).expect("encode");
    serde_json::from_str(&text).expect("decode")
}

/// One service sample batch for `ids`, each measured.
fn batch(at: u64, ids: &[&str]) -> ServiceSampleBatch {
    ServiceSampleBatch {
        sampled_at_unix: at,
        services: ids
            .iter()
            .map(|id| ServiceSample {
                id: (*id).to_string(),
                status: ServiceStatus::Running,
                cpu_pct: Some(3.5),
                rss_bytes: Some(64 * 1024 * 1024),
            })
            .collect(),
    }
}

/// Why: "no samples yet" is a normal answer, where HTTP answers `503`.
/// What: a cold host cache answers `ready: false` and `machine: null`.
/// Test: this test itself.
#[tokio::test]
async fn machine_status_on_a_cold_cache_answers_not_ready() {
    let state = state();
    let response = dispatch(&router(state.clone()), METHOD_MACHINE_STATUS, json!({})).await;
    let answer: ConsoleMachineStatus =
        serde_json::from_value(response.result.expect("a result, not an error")).expect("decode");
    assert!(!answer.ready);
    assert!(answer.machine.is_none());
    let (status, _) = http_json(&state, "/api/console/machine-status").await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "HTTP route unchanged"
    );
}

/// Why: #10942 — the UDS answer is the HTTP body, from the same caches.
/// What: warms the host and one metrics cache, then compares the UDS
/// `machine` with the HTTP body, minus the per-call assembly timestamp.
/// Test: this test itself.
#[tokio::test]
async fn machine_status_equals_the_http_machine_status_body() {
    let state = state();
    state
        .host_metrics_cache()
        .set(HostSampler::new().sample())
        .await;
    state
        .metrics_cache()
        .set(make_report(
            "trusty-analyze",
            "Analyze",
            "0.7.0",
            ServiceHealth::Ok,
            json!({}),
            1,
        ))
        .await;

    let answer = machine_status(&state).await;
    assert!(answer.ready);
    let mut uds = wire_json(&answer.machine.expect("machine"));
    let (status, http) = http_json(&state, "/api/console/machine-status").await;
    assert_eq!(status, StatusCode::OK);
    let mut http = http.expect("json body");
    for body in [&mut uds, &mut http] {
        body.as_object_mut()
            .expect("object")
            .remove("assembled_at_unix");
    }
    assert_eq!(uds, http);
    assert_eq!(uds["services"]["total"], json!(1));
}

/// Why: before the sampler's first tick the window is empty, not an error.
/// What: a fresh state answers empty collections and the real capacities.
/// Test: this test itself.
#[tokio::test]
async fn history_on_a_cold_cache_is_an_empty_window() {
    let response = dispatch(&router(state()), METHOD_HISTORY, Value::Null).await;
    let snapshot: HistorySnapshot =
        serde_json::from_value(response.result.expect("a result, not an error")).expect("decode");
    assert!(snapshot.samples.is_empty());
    assert!(snapshot.transitions.is_empty());
    assert!(snapshot.service_samples.is_empty());
    assert_eq!(snapshot.sample_capacity, HOST_HISTORY_CAPACITY);
}

/// Why: #10942 serves the HTTP history body; #10943 needs the per-service
/// CPU/RSS rings in it.
/// What: records host samples, two service batches and a transition, then
/// compares the UDS answer with the HTTP body and reads the service rings.
/// Test: this test itself.
#[tokio::test]
async fn history_equals_the_http_history_body_and_carries_service_samples() {
    let state = state();
    let history_handle = state.machine_history();
    let mut sampler = HostSampler::new();
    history_handle.record_sample(sampler.sample()).await;
    history_handle
        .record_service_samples(batch(1, &["trusty-search"]))
        .await;
    history_handle
        .record_service_samples(batch(2, &["trusty-search"]))
        .await;
    for health in [ServiceHealth::Ok, ServiceHealth::Error] {
        history_handle
            .observe_services(&[make_report(
                "trusty-search",
                "Search",
                "1",
                health,
                json!({}),
                1,
            )])
            .await;
    }

    let snapshot = history(&state).await;
    assert_eq!(snapshot.samples.len(), 1);
    assert_eq!(snapshot.transitions.len(), 1);
    let ring = &snapshot.service_samples["trusty-search"];
    assert_eq!(ring.len(), 2);
    assert_eq!(ring[1].cpu_pct, Some(3.5));
    assert_eq!(ring[1].rss_bytes, Some(64 * 1024 * 1024));

    let (status, http) = http_json(&state, "/api/console/machine-status/history").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(wire_json(&snapshot), http.expect("json"));
}

/// Why: a malformed request must come back as a JSON-RPC error, never a hang.
/// What: a scalar and a non-empty array as params are `invalid_params`; an
/// unknown method is `method_not_found`.
/// Test: this test itself.
#[tokio::test]
async fn malformed_params_and_unknown_methods_are_rpc_errors() {
    let router = router(state());
    for method in [METHOD_MACHINE_STATUS, METHOD_HISTORY] {
        for params in [json!("x"), json!([1])] {
            let response = dispatch(&router, method, params).await;
            assert!(response.result.is_none());
            assert_eq!(response.error.expect("error").code, CODE_INVALID_PARAMS);
        }
    }
    let response = dispatch(&router, "console.machine_history", json!({})).await;
    assert_eq!(response.error.expect("error").code, CODE_METHOD_NOT_FOUND);
}

/// Why: the TUI reads both methods over the real socket, and a full 10-minute
/// window must fit the client's frame budget.
/// What: fills the host ring and six service rings to capacity, calls both
/// methods with the trusty-common client (which enforces `MAX_FRAME_BYTES`),
/// then sends an unknown method and an unparseable frame on raw connections.
/// Test: this test itself.
#[tokio::test]
async fn listener_serves_the_metrics_methods_over_a_real_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("console.sock");
    let state = state();
    let host = HostSampler::new().sample();
    state.host_metrics_cache().set(host.clone()).await;
    let ids = ["a", "b", "c", "d", "e", "f"];
    for tick in 0..HOST_HISTORY_CAPACITY as u64 {
        state.machine_history().record_sample(host.clone()).await;
        state
            .machine_history()
            .record_service_samples(batch(tick, &ids))
            .await;
    }
    let socket = start_at(state, &path).await.expect("bind");

    let call = |method: &str| json!({"jsonrpc": "2.0", "id": 9, "method": method});
    let response: RpcResponse =
        send_framed_request(&path, &call(METHOD_MACHINE_STATUS), CALL_TIMEOUT)
            .await
            .expect("machine_status call");
    let answer: ConsoleMachineStatus =
        serde_json::from_value(response.result.expect("result")).expect("decode");
    assert!(answer.ready);

    let response: RpcResponse = send_framed_request(&path, &call(METHOD_HISTORY), CALL_TIMEOUT)
        .await
        .expect("a full window fits the frame budget");
    let snapshot: HistorySnapshot =
        serde_json::from_value(response.result.expect("result")).expect("decode");
    assert_eq!(snapshot.samples.len(), HOST_HISTORY_CAPACITY);
    assert_eq!(snapshot.service_samples.len(), ids.len());
    assert!(
        snapshot
            .service_samples
            .values()
            .all(|r| r.len() == HOST_HISTORY_CAPACITY)
    );

    let response: RpcResponse = send_framed_request(&path, &call("console.nope"), CALL_TIMEOUT)
        .await
        .expect("unknown method call");
    assert_eq!(response.error.expect("error").code, CODE_METHOD_NOT_FOUND);

    let mut stream = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    stream.write_all(b"{not json\n").await.expect("write");
    let mut line = String::new();
    tokio::time::timeout(
        CALL_TIMEOUT,
        BufReader::new(&mut stream).read_line(&mut line),
    )
    .await
    .expect("an answer, not a hang")
    .expect("read");
    let response: RpcResponse = serde_json::from_str(&line).expect("an RPC response");
    assert_eq!(response.error.expect("error").code, CODE_PARSE_ERROR);

    socket.shutdown().await;
    assert!(!path.exists(), "shutdown unlinks the socket");
}
