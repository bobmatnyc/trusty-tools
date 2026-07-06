//! HTTP JSON-RPC 2.0 transport for `tcode serve --http` (#2053).
//!
//! Why: matches how `trusty-memory` (`web::router` + `run_http_on`) and
//! `trusty-search` (`service::server::build_router`) stand up their axum
//! daemons: a pure `axum::Router` builder — testable via `oneshot`, no real
//! socket — wrapped in `trusty_common::server::with_standard_middleware`
//! (CORS/trace/gzip), plus a bind+serve entry point that installs the
//! shared `trusty_common::shutdown_signal()` graceful-shutdown watcher
//! (SIGTERM + SIGINT), exactly like `trusty-memory`'s `run_http_on`. `POST
//! /rpc` and `GET /health` dispatch through the SAME [`crate::jsonrpc::Router`]
//! the STDIO transport (`crate::serve::transport`) uses via
//! [`Router::dispatch_json`] — there is exactly one implementation of each
//! method (`ping`, `health`, …), not one per transport.
//!
//! What: [`build_axum_router`] (pure, unit-testable) and [`run_http`] (binds
//! a `TcpListener` — port `0` yields an OS-assigned ephemeral port — logs
//! the bound address to stderr, never stdout, and serves until graceful
//! shutdown).
//!
//! Deferred: batch JSON-RPC requests (a JSON array body) are not supported —
//! `POST /rpc` accepts a single request object per call, matching what the
//! ticket asked for ("single-request is fine for now").
//!
//! Test: `http::tests::*` drive [`build_axum_router`] via
//! `tower::util::ServiceExt::oneshot` (no real socket): `POST /rpc` ping
//! success, `POST /rpc` malformed JSON -> `-32700`, `GET /health`.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Json, Router as AxumRouter,
    body::Bytes,
    extract::State,
    routing::{get, post},
};
use tokio::net::TcpListener;
use tracing::info;
use trusty_common::mcp::Response;

use crate::jsonrpc::Router;
use crate::serve::methods::health_payload;

/// Build the pure axum router (no listener bound) exposing `POST /rpc` and
/// `GET /health`.
///
/// Why: kept separate from [`run_http`] so unit tests exercise routing and
/// dispatch via `tower::util::ServiceExt::oneshot` without opening a real
/// socket, and so the standard CORS/trace/gzip middleware stack wraps it
/// identically to trusty-memory/trusty-search.
/// What: `POST /rpc` dispatches the request body through
/// `Router::dispatch_json` (shared with the STDIO transport); `GET /health`
/// returns [`health_payload`] — the exact payload the `health` JSON-RPC
/// method returns, so HTTP and JSON-RPC callers see the same shape.
/// Test: `http_rpc_ping_returns_pong`,
/// `http_rpc_malformed_json_returns_parse_error`,
/// `http_health_matches_jsonrpc_health_payload`.
pub fn build_axum_router(router: Arc<Router>) -> AxumRouter {
    let app = AxumRouter::new()
        .route("/rpc", post(rpc_handler))
        .route("/health", get(health_handler))
        .with_state(router);
    trusty_common::server::with_standard_middleware(app)
}

/// `POST /rpc` — JSON-RPC 2.0 dispatch endpoint.
///
/// Why: lets browser clients, curl, and any HTTP-only caller reach the same
/// method surface the STDIO transport exposes, without learning a REST
/// vocabulary per method.
/// What: dispatches the raw request body through `Router::dispatch_json`
/// (single request per call — batch/array bodies are not supported, see
/// module docs). Always returns HTTP 200; JSON-RPC errors, including
/// malformed JSON (mapped to `-32700`), are carried in the response
/// envelope's `error` field rather than the HTTP status.
/// Test: `http_rpc_ping_returns_pong`, `http_rpc_malformed_json_returns_parse_error`.
async fn rpc_handler(State(router): State<Arc<Router>>, body: Bytes) -> Json<Response> {
    Json(router.dispatch_json(&body).await)
}

/// `GET /health` — liveness probe matching the `health` JSON-RPC method.
///
/// Why: the ecosystem convention (trusty-search, trusty-memory) is an
/// unauthenticated `GET /health`; trusty-console's gateway polls it to
/// confirm a daemon is alive. Reusing [`health_payload`] — the exact
/// function the `health` JSON-RPC method calls — keeps the two transports
/// from drifting into two different health shapes.
/// What: always returns HTTP 200 with `{"server","version","status"}`.
/// Test: `http_health_matches_jsonrpc_health_payload`.
async fn health_handler() -> Json<serde_json::Value> {
    Json(health_payload())
}

/// Bind a `TcpListener` and serve `POST /rpc` + `GET /health` until SIGTERM/
/// SIGINT (graceful) or an internal axum error.
///
/// Why: the top-level entry point `crate::serve::run_http` delegates to this
/// once the router is assembled.
/// What: binds `127.0.0.1:port` (`port = 0` lets the OS assign an ephemeral
/// port), logs the real bound address to stderr — mirroring
/// `trusty-memory`'s `run_http_on`, and never touching stdout — then serves
/// with `trusty_common::shutdown_signal()` installed via
/// `with_graceful_shutdown`: SIGTERM/SIGINT stop new connections and drain
/// in-flight requests before this function returns (issue #534 convention).
/// Test: not directly unit-tested (would require a real socket + a real
/// signal); [`build_axum_router`]'s tests cover the routing/dispatch logic
/// this serves.
pub async fn run_http(router: Router, port: u16) -> Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("tcode serve --http: bind {addr}"))?;
    let bound = listener
        .local_addr()
        .context("tcode serve --http: read bound local address")?;
    info!("tcode serve --http: listening on http://{bound}");
    eprintln!("tcode serve --http: listening on http://{bound}");

    let app = build_axum_router(Arc::new(router));
    axum::serve(listener, app)
        .with_graceful_shutdown(trusty_common::shutdown_signal())
        .await
        .context("tcode serve --http: axum serve failed")?;

    info!("tcode serve --http: stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use tower::util::ServiceExt;

    fn router_with_methods() -> Arc<Router> {
        let mut router = Router::new();
        crate::serve::methods::register(&mut router);
        Arc::new(router)
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// `POST /rpc` with a `ping` request must return HTTP 200 and the same
    /// `{"pong": true}` result the STDIO transport returns.
    #[tokio::test]
    async fn http_rpc_ping_returns_pong() {
        let app = build_axum_router(router_with_methods());
        let req_body = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["result"], json!({"pong": true}));
        assert_eq!(v["id"], 1);
    }

    /// `POST /rpc` with a malformed body must still return HTTP 200 with a
    /// JSON-RPC `-32700 Parse error` envelope, not a bare HTTP 400.
    #[tokio::test]
    async fn http_rpc_malformed_json_returns_parse_error() {
        let app = build_axum_router(router_with_methods());

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .body(Body::from("not json at all"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(
            v["error"]["code"],
            trusty_common::mcp::error_codes::PARSE_ERROR
        );
    }

    /// `GET /health` must return the exact payload `health_payload()`
    /// (and thus the `health` JSON-RPC method) returns.
    #[tokio::test]
    async fn http_health_matches_jsonrpc_health_payload() {
        let app = build_axum_router(router_with_methods());

        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v, health_payload());
    }
}
