//! [`RpcHttpClient`]: a pooled JSON-RPC 2.0 client speaking `POST {base}/rpc`
//! against a long-lived `tcode serve --http` daemon (issue #3415).
//!
//! Why: mirrors `crate::cli_client::stdio::StdioRpcClient`'s role over
//! STDIO — one place that builds the wire request, sends it, and unwraps the
//! envelope — but over HTTP against an ALREADY-RUNNING daemon instead of a
//! per-call spawned child. Reuses the exact same wire types
//! (`trusty_common::mcp::{Request, Response}`) `StdioRpcClient` and
//! `crate::serve::http::rpc_handler` already use, so there is exactly one
//! `Request`/`Response` shape in this crate regardless of transport.
//! What: [`RpcHttpClient::call`] mints a monotonic request id, POSTs the
//! envelope, and unwraps the response into either `Ok(result)` or an
//! [`EngineError::Rpc`]/[`EngineError::Transport`]. Holds one
//! `reqwest::Client` for its whole lifetime (pooled keep-alive connections,
//! DOC-50 §3.4 point 5) — callers needing the SSE routes (`sse.rs`) reuse
//! the SAME client via [`RpcHttpClient::http`] rather than building a second
//! one.
//! Test: `rpc_tests::*` against a `wiremock` mock server.

use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::Value;
use trusty_common::mcp::{Request, Response};

use super::error::EngineError;

/// How long a single `POST /rpc` call waits for a response before giving
/// up. Every method this client calls today (`session.*`, `task.run`,
/// `workstream.*`) is a fast, synchronous handler — `task.run` reserves its
/// execution slot and returns immediately rather than blocking on the LLM
/// run (mirrors `crate::cli_client::stdio::DEFAULT_CALL_TIMEOUT`'s docs) —
/// so a real hang here means a genuine daemon-side regression, not a slow
/// but legitimate call.
pub const DEFAULT_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// See module docs.
pub struct RpcHttpClient {
    http: reqwest::Client,
    base_url: String,
    next_id: AtomicI64,
}

impl RpcHttpClient {
    /// Build a client targeting `base_url` (no trailing slash), reusing the
    /// caller-supplied pooled `reqwest::Client`.
    pub fn new(http: reqwest::Client, base_url: String) -> Self {
        Self {
            http,
            base_url,
            next_id: AtomicI64::new(1),
        }
    }

    /// The daemon base URL this client targets (e.g.
    /// `http://127.0.0.1:7882`) — callers building SSE URLs
    /// (`{base}/sessions/{id}/events`) read this rather than duplicating it.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The shared pooled HTTP client — callers building their own requests
    /// (the SSE routes, which aren't JSON-RPC) reuse this rather than
    /// constructing a second `reqwest::Client` (defeating the connection
    /// pool).
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Call `method` with `params` against `POST {base_url}/rpc`, waiting
    /// (bounded by [`DEFAULT_CALL_TIMEOUT`]) for the matching response.
    ///
    /// Why: the one call every `session.*`/`task.*`/`workstream.*` RPC in
    /// this engine goes through.
    /// What: mints a monotonic id, builds the `Request` envelope, POSTs it,
    /// decodes the `Response` envelope, and returns `Ok(result)` or maps a
    /// JSON-RPC error object onto [`EngineError::Rpc`]. A transport-level
    /// failure (connect/timeout/decode) maps onto [`EngineError::Transport`].
    /// Test: `rpc_tests::call_returns_result_on_success`,
    /// `rpc_tests::call_maps_rpc_error_envelope`,
    /// `rpc_tests::call_maps_transport_failure`.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, EngineError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(serde_json::json!(id)),
            method: method.to_string(),
            params: Some(params),
        };
        let url = format!("{}/rpc", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&req)
            .timeout(DEFAULT_CALL_TIMEOUT)
            .send()
            .await
            .map_err(|source| EngineError::Transport {
                url: url.clone(),
                source,
            })?;
        let body: Response = resp
            .json()
            .await
            .map_err(|source| EngineError::Transport { url, source })?;
        if let Some(err) = body.error {
            return Err(EngineError::Rpc {
                code: err.code,
                message: err.message,
                data: err.data,
            });
        }
        Ok(body.result.unwrap_or(Value::Null))
    }
}

#[cfg(test)]
mod rpc_tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A successful `POST /rpc` call must return the envelope's `result`.
    #[tokio::test]
    async fn call_returns_result_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"pong": true},
            })))
            .mount(&server)
            .await;

        let client = RpcHttpClient::new(reqwest::Client::new(), server.uri());
        let result = client.call("ping", json!({})).await.expect("call");
        assert_eq!(result, json!({"pong": true}));
    }

    /// A JSON-RPC error envelope must map onto `EngineError::Rpc`, carrying
    /// the code/message through verbatim.
    #[tokio::test]
    async fn call_maps_rpc_error_envelope() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32002, "message": "workstream not found"},
            })))
            .mount(&server)
            .await;

        let client = RpcHttpClient::new(reqwest::Client::new(), server.uri());
        let err = client
            .call("workstream.get", json!({"id": "missing"}))
            .await
            .expect_err("must be an error");
        match err {
            EngineError::Rpc { code, message, .. } => {
                assert_eq!(code, -32002);
                assert_eq!(message, "workstream not found");
            }
            other => panic!("expected EngineError::Rpc, got {other:?}"),
        }
    }

    /// A transport-level failure (daemon unreachable) must map onto
    /// `EngineError::Transport`, not panic or silently swallow the error.
    #[tokio::test]
    async fn call_maps_transport_failure() {
        // Nothing is listening on this port — a real connection failure.
        let client = RpcHttpClient::new(reqwest::Client::new(), "http://127.0.0.1:1".to_string());
        let err = client
            .call("ping", json!({}))
            .await
            .expect_err("must be an error");
        assert!(matches!(err, EngineError::Transport { .. }));
    }

    /// Two calls in sequence must use two DIFFERENT ids — proves the
    /// monotonic counter actually advances (a daemon matching on id would
    /// otherwise silently misroute a second in-flight call).
    #[tokio::test]
    async fn successive_calls_use_distinct_ids() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {},
            })))
            .mount(&server)
            .await;

        let client = RpcHttpClient::new(reqwest::Client::new(), server.uri());
        client.call("ping", json!({})).await.expect("call 1");
        client.call("ping", json!({})).await.expect("call 2");

        let requests = server.received_requests().await.expect("received");
        assert_eq!(requests.len(), 2);
        let ids: Vec<i64> = requests
            .iter()
            .map(|r| {
                let body: Value = r.body_json().expect("body json");
                body["id"].as_i64().expect("id")
            })
            .collect();
        assert_ne!(ids[0], ids[1]);
    }
}
