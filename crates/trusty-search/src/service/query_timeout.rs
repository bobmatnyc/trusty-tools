//! Per-request deadline middleware scoped to interactive search/query routes
//! (issue #907).
//!
//! Why: Interactive search queries (`/search`, `/grep`, `/indexes/{id}/search`,
//! etc.) must return within a bounded time — never hang. Long-running operations
//! like `POST /indexes/{id}/reindex` and `POST /indexes/{id}/index-file` are
//! explicitly excluded so a valid reindex is never cut off by this timer.
//!
//! What: The `apply_query_timeout` axum middleware wraps the downstream
//! handler in `tokio::time::timeout`. On expiry it returns HTTP 408 Request
//! Timeout with a JSON error body. The deadline is tunable via
//! `TRUSTY_QUERY_TIMEOUT_SECS` (default 30 s).
//!
//! The middleware reads a `QueryTimeoutConfig` extension that is installed on
//! the router, so the timeout value can be overridden per-test by injecting a
//! short deadline without touching global env.
//!
//! Config:
//!   - `TRUSTY_QUERY_TIMEOUT_SECS` (default 30) — max wall-clock seconds for
//!     a single search/grep request before a 408 is returned.
//!
//! Test: `query_timeout_returns_408_when_handler_stalls`,
//!        `query_timeout_passes_through_fast_response`.

use axum::{
    body::Body,
    extract::Extension,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use std::sync::Arc;

/// Default wall-clock timeout for interactive query routes (issue #907).
///
/// Why: 30 s is longer than the slowest legitimate query (HNSW + BM25 on a
/// large corpus) but short enough that Claude Code / client-side HTTP timeouts
/// will not fire first, and stalled-embedder requests are detected quickly.
const DEFAULT_QUERY_TIMEOUT_SECS: u64 = 30;

/// Shared timeout config installed as an axum `Extension` on the interactive
/// query router subtree.
///
/// Why: storing the deadline in a router extension (rather than reading env
/// vars inside the middleware function) lets tests inject a tiny timeout
/// without touching global state or process-wide caches.
/// What: a single `Duration` wrapped in `Arc` so it can be cloned cheaply
/// into every request.
/// Test: `query_timeout_returns_408_when_handler_stalls` injects a 50 ms
/// deadline through this type.
#[derive(Clone, Debug)]
pub struct QueryTimeoutConfig {
    /// Wall-clock budget for one interactive query request.
    pub timeout: std::time::Duration,
}

impl QueryTimeoutConfig {
    /// Construct from `TRUSTY_QUERY_TIMEOUT_SECS` env var with a 30 s default.
    ///
    /// Why: called once at daemon boot so per-request env-var lookups are avoided.
    /// What: reads env var, parses as u64, falls back to `DEFAULT_QUERY_TIMEOUT_SECS`.
    /// Test: daemon-boot integration path; unit-tested via `from_secs`.
    pub fn from_env() -> Arc<Self> {
        let secs = std::env::var("TRUSTY_QUERY_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_QUERY_TIMEOUT_SECS);
        tracing::info!("query timeout: {}s (TRUSTY_QUERY_TIMEOUT_SECS)", secs);
        Arc::new(Self {
            timeout: std::time::Duration::from_secs(secs),
        })
    }

    /// Construct with an explicit duration (test helper).
    ///
    /// Why: lets tests inject a tiny deadline without touching env vars or
    /// the process-wide `OnceLock` cache.
    /// What: wraps `duration` in `Arc<QueryTimeoutConfig>`.
    /// Test: used by `query_timeout_returns_408_when_handler_stalls`.
    #[cfg(test)]
    pub fn from_duration(duration: std::time::Duration) -> Arc<Self> {
        Arc::new(Self { timeout: duration })
    }
}

/// Build the response body for a query-timeout expiry (HTTP 408).
///
/// Why: a consistent JSON error shape lets clients distinguish a query timeout
/// (408) from a server error (500) or a busy-queue rejection (503).
/// What: returns `{"error":"query_timeout","message":"…"}` with status 408.
/// Test: `query_timeout_returns_408_when_handler_stalls`.
fn timeout_response() -> Response {
    let body = Json(serde_json::json!({
        "error": "query_timeout",
        "message": "Query exceeded the configured time limit — try a narrower query or retry",
    }));
    (StatusCode::REQUEST_TIMEOUT, body).into_response()
}

/// Axum middleware: enforce a per-request wall-clock deadline on interactive
/// query routes (issue #907).
///
/// Why: without this, a stalled embedder sidecar call can block the search
/// handler forever. The middleware wraps the downstream handler future in
/// `tokio::time::timeout`; on expiry the handler is cancelled and the caller
/// receives HTTP 408 immediately.
/// What: reads `QueryTimeoutConfig` from the request extensions (installed by
/// the router builder), runs `tokio::time::timeout(cfg.timeout, next.run(req))`,
/// and returns either the real response or a 408 body.
/// Test: `query_timeout_returns_408_when_handler_stalls` proves 408 fires;
/// `query_timeout_passes_through_fast_response` proves normal paths return 200.
pub async fn apply_query_timeout(
    Extension(cfg): Extension<Arc<QueryTimeoutConfig>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    match tokio::time::timeout(cfg.timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_elapsed) => {
            tracing::warn!(
                timeout_secs = cfg.timeout.as_secs(),
                "query_timeout: interactive query exceeded deadline, returning 408 (issue #907)"
            );
            metrics::counter!("trusty_query_timeouts_total").increment(1);
            timeout_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::post,
        Router,
    };
    use std::time::Duration;
    use tower::ServiceExt;

    fn query_router_with_timeout(cfg: Arc<QueryTimeoutConfig>) -> Router {
        Router::new()
            .route(
                "/search",
                post(|| async {
                    // Handler completes immediately — normal fast path.
                    "ok"
                }),
            )
            .route(
                "/search_slow",
                post(|| async {
                    // Handler stalls — simulates a blocked embedder call.
                    std::future::pending::<&str>().await
                }),
            )
            .route_layer(axum::middleware::from_fn(apply_query_timeout))
            .layer(Extension(cfg))
    }

    /// Verify the happy path: a fast handler returns 200, not 408.
    ///
    /// Why: the timeout middleware must not break normal queries.
    /// What: builds a router with a 100 ms deadline; the `/search` handler
    /// returns immediately; assert the response is 200.
    /// Test: this test.
    #[tokio::test]
    async fn query_timeout_passes_through_fast_response() {
        let cfg = QueryTimeoutConfig::from_duration(Duration::from_millis(100));
        let app = query_router_with_timeout(cfg);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/search")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "fast query must return 200, not be cut off by timeout"
        );
    }

    /// Prove that a stalled handler triggers HTTP 408 within the deadline.
    ///
    /// Why: this is the invariant from issue #907 — interactive queries must
    /// never hang; the timeout MUST fire and return a clean error.
    /// What: handler stalls with `pending()`; middleware timeout of 50 ms
    /// fires; response must be 408 within ~1 s wall clock.
    /// Test: this test.
    #[tokio::test]
    async fn query_timeout_returns_408_when_handler_stalls() {
        let cfg = QueryTimeoutConfig::from_duration(Duration::from_millis(50));
        let app = query_router_with_timeout(cfg);

        let start = std::time::Instant::now();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/search_slow")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let elapsed = start.elapsed();

        assert_eq!(
            resp.status(),
            StatusCode::REQUEST_TIMEOUT,
            "stalled query must receive 408, not hang (elapsed: {:?})",
            elapsed,
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "408 must arrive before the 2 s wall-clock guard (elapsed: {:?})",
            elapsed,
        );
    }
}
