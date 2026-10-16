//! `search.warm.start` / `search.warm.status` against their HTTP twins (#9027).
//!
//! Why: the console reaches warm-all over the socket and the embedded UI over
//! HTTP; the two must report the same state and refuse the same malformed start.
//! What: one shared state read by the warm RPC family and by the cores the HTTP
//! routes wrapped (#9535: the router is gone).
//! Test: this module.

use std::sync::Arc;

use trusty_common::uds::server::{RpcRouter, CODE_INVALID_PARAMS};

use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::rpc::warm;
use crate::service::server::{warm_status_report, SearchAppState, WarmStartRequest};

fn fixed_rss() -> Option<u64> {
    Some(1)
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
    let rpc_router = warm::register(RpcRouter::new(), &state);

    let over_http = warm_status_report(&state);
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
    // #9027: a yield-capped wait fell through silently while the run was still
    // going on another worker, so the HTTP and socket reads saw different
    // states. Wait on wall-clock time and fail loudly instead.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while warm_status_report(&state)["run"]["running"] != serde_json::json!(false) {
        assert!(
            std::time::Instant::now() < deadline,
            "the warm run did not finish within 10 s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let over_http = warm_status_report(&state);
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

    // `POST /warm`'s extractor refused this decode before the core ran.
    assert!(
        serde_json::from_value::<WarmStartRequest>(serde_json::json!({ "bogus": 1 })).is_err(),
        "HTTP refuses a stray field"
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

/// Why: a window past the 24 h cap is refused on the socket exactly as on HTTP
/// (`400` maps to `invalid_params`), before any run starts (#9027).
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn the_socket_refuses_a_warm_window_past_the_cap() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    state.warm.set_rss_probe(fixed_rss);
    let rpc_router = warm::register(RpcRouter::new(), &state);

    let refused = rpc(
        &rpc_router,
        warm::METHOD_WARM_START,
        serde_json::json!({ "window_secs": u64::MAX }),
    )
    .await
    .error
    .expect("an oversized window is refused");
    assert_eq!(refused.code, CODE_INVALID_PARAMS);
    assert_eq!(warm_status_report(&state)["run"], serde_json::Value::Null);
}
