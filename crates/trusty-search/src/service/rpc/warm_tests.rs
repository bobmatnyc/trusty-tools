//! `search.warm.start` / `search.warm.status` against their HTTP twins (#9027).
//!
//! Why: the console reaches warm-all over the socket and the embedded UI over
//! HTTP; the two must report the same state and refuse the same malformed start.
//! What: one shared state behind the real axum router and the warm RPC family.
//! Test: this module.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;
use trusty_common::uds::server::{RpcRouter, CODE_INVALID_PARAMS};

use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::rpc::warm;
use crate::service::server::{build_router_on, SearchAppState};

fn fixed_rss() -> Option<u64> {
    Some(1)
}

async fn http(
    router: &axum::Router,
    method: &str,
    uri: &str,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = router.clone().oneshot(request).await.expect("answer");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, value)
}

async fn rpc(
    router: &RpcRouter,
    method: &str,
    params: serde_json::Value,
) -> trusty_common::uds::server::RpcResponse {
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .expect("frame");
    router.dispatch(&frame).await
}

/// Why: both transports must serve one body and refuse one malformed start.
/// What: compares the status before and after a socket-started warm, and sends
/// a stray field to both starts.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn warm_over_the_socket_matches_the_http_body() {
    let registry = IndexRegistry::new();
    let root = "/nonexistent/warm-9027";
    registry.register(IndexHandle::bare(
        IndexId::new("w".to_string()),
        Arc::new(tokio::sync::RwLock::new(CodeIndexer::new("w", root))),
        root.into(),
    ));
    let state = Arc::new(SearchAppState::new(registry));
    state.warm.set_rss_probe(fixed_rss);
    let http_router = build_router_on(
        Arc::clone(&state),
        trusty_common::server::SelfOrigins::default(),
    );
    let rpc_router = warm::register(RpcRouter::new(), &state);

    let (status, over_http) = http(&http_router, "GET", "/warm/status", "").await;
    assert_eq!(status, StatusCode::OK);
    let over_socket = rpc(
        &rpc_router,
        warm::METHOD_WARM_STATUS,
        serde_json::Value::Null,
    )
    .await
    .result
    .expect("status result");
    assert_eq!(over_socket, over_http);

    let started = rpc(
        &rpc_router,
        warm::METHOD_WARM_START,
        serde_json::Value::Null,
    )
    .await
    .result
    .expect("a null-params start is the default start");
    assert_eq!(started["joined"], serde_json::json!(false), "{started}");
    for _ in 0..200 {
        if crate::service::server::warm_status_report(&state)["run"]["running"]
            == serde_json::json!(false)
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    let (_, over_http) = http(&http_router, "GET", "/warm/status", "").await;
    let over_socket = rpc(&rpc_router, warm::METHOD_WARM_STATUS, serde_json::json!({}))
        .await
        .result
        .expect("status result");
    assert_eq!(over_socket["run"], over_http["run"]);
    assert_eq!(over_socket["totals"], over_http["totals"]);
    assert_eq!(
        over_http["totals"]["warm"],
        serde_json::json!(1),
        "{over_http}"
    );

    let (status, _) = http(&http_router, "POST", "/warm", r#"{"bogus":1}"#).await;
    assert!(
        status.is_client_error(),
        "HTTP refuses a stray field: {status}"
    );
    let refused = rpc(
        &rpc_router,
        warm::METHOD_WARM_START,
        serde_json::json!({ "bogus": 1 }),
    )
    .await
    .error
    .expect("the socket refuses a stray field");
    assert_eq!(refused.code, CODE_INVALID_PARAMS);
}
