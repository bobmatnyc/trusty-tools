//! Helpers the socket-parity test modules share (#9535).
//!
//! Why: the parity tests call each route's `*_report` core for the HTTP side
//! (#9535: the router is going), and every family's test file had grown its own
//! copy of the same three adapters. One copy keeps the HTTP-side encoding the
//! same for every family.
//! What: [`http_body`] and [`http_refusal`] unwrap a core's answer; [`params`]
//! decodes a JSON request into the core's request type; [`dispatch`],
//! [`rpc_ok`] and [`rpc_err`] send one frame through a real `RpcRouter`.
//! Test: the `*_over_the_socket_matches_the_http_body` cases in `admin_tests.rs`,
//! `queries_tests.rs`, `reads_tests.rs` and `writes_tests.rs` use these.

use axum::http::StatusCode;
use trusty_common::uds::server::{RpcError, RpcResponse, RpcRouter};

/// One JSON-RPC frame for `method`, dispatched through `rpc`.
pub(super) async fn dispatch(
    rpc: &RpcRouter,
    method: &str,
    params: serde_json::Value,
) -> RpcResponse {
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .expect("encode the frame");
    rpc.dispatch(&frame).await
}

/// One RPC call against the real router, asserting a result frame.
pub(super) async fn rpc_ok(
    rpc: &RpcRouter,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let response = dispatch(rpc, method, params).await;
    assert!(
        response.error.is_none(),
        "{method} must answer a result: {:?}",
        response.error
    );
    response.result.expect("a non-error frame carries a result")
}

/// One RPC call against the real router, asserting an error frame.
pub(super) async fn rpc_err(rpc: &RpcRouter, method: &str, params: serde_json::Value) -> RpcError {
    let response = dispatch(rpc, method, params).await;
    response
        .error
        .unwrap_or_else(|| panic!("{method} must be refused; got {:?}", response.result))
}

/// A core's 200 body, encoded the way axum's `Json` writes it (#9535).
///
/// Serialised to bytes and parsed back, not `serde_json::to_value`, so an
/// `f32` field gets the same shortest representation HTTP and `as_http_body`
/// give it.
pub(super) fn http_body<T: serde::Serialize>(
    answer: Result<T, (StatusCode, serde_json::Value)>,
) -> serde_json::Value {
    match answer {
        Ok(body) => {
            let bytes = serde_json::to_vec(&body).expect("encode the body");
            serde_json::from_slice(&bytes).expect("decode the body")
        }
        Err((status, body)) => panic!("the core must answer 200, got {status}: {body}"),
    }
}

/// A core's refusal as `(status, body)`, asserting it is a 4xx or 5xx (#9535).
pub(super) fn http_refusal<T: serde::Serialize>(
    answer: Result<T, (StatusCode, serde_json::Value)>,
) -> (StatusCode, serde_json::Value) {
    match answer {
        Ok(body) => panic!(
            "the core must refuse, got 200: {}",
            serde_json::to_value(body).expect("encode the body")
        ),
        Err((status, body)) => {
            assert!(
                status.is_client_error() || status.is_server_error(),
                "a refusal must be 4xx/5xx, got {status}: {body}"
            );
            (status, body)
        }
    }
}

/// A JSON request decoded as the core's request type.
pub(super) fn params<T: serde::de::DeserializeOwned>(json: serde_json::Value) -> T {
    serde_json::from_value(json).expect("decode the request")
}
