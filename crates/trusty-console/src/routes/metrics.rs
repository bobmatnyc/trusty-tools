//! The five per-service `GET /api/console/metrics/<service>` handlers.
//!
//! Why here rather than in `server/mod.rs`: adding the #6641 machine-status
//! history and stream routes pushed that file to 504 SLOC, four over the
//! production cap. These five handlers are the most cohesive group in it — one
//! shape repeated per service, none of them touching the router, the app state's
//! construction, or the SPA — so they are the split that leaves `server/mod.rs`
//! about what it is for. The `routes` module already exists for exactly this
//! (see its module docs).
//! What: one handler per polled service, each reading that service's
//! `MetricsCache` and answering `200` with the cached
//! `ConsoleMetricsReport` or `503` when no poll has completed yet — the binary
//! is absent, or this is first boot.
//! Test: `crate::server::tests::test_metrics_*_route_cold_cache_returns_503`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::server::AppState;

/// `GET /api/console/metrics/analyze` — the latest trusty-analyze report.
///
/// Why: surfaces analyze health to the SPA without a per-request MCP call; the
/// background poller keeps the cache warm.
/// What: cached `ConsoleMetricsReport` as JSON (200), or 503 before the first
/// successful poll.
/// Test: `test_metrics_analyze_route_cold_cache_returns_503`.
pub async fn metrics_analyze_handler(State(state): State<AppState>) -> axum::response::Response {
    match state.metrics_cache().get().await {
        Some(report) => axum::Json(report).into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// `GET /api/console/metrics/memory` — the latest trusty-memory report.
///
/// Test: `test_metrics_memory_route_cold_cache_returns_503`.
pub async fn metrics_memory_handler(State(state): State<AppState>) -> axum::response::Response {
    match state.memory_metrics_cache().get().await {
        Some(report) => axum::Json(report).into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// `GET /api/console/metrics/search` — the latest trusty-search report.
///
/// Test: `test_metrics_search_route_cold_cache_returns_503`.
pub async fn metrics_search_handler(State(state): State<AppState>) -> axum::response::Response {
    match state.search_metrics_cache().get().await {
        Some(report) => axum::Json(report).into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// `GET /api/console/metrics/review` — the latest trusty-review report.
///
/// Test: `test_metrics_review_route_cold_cache_returns_503`.
pub async fn metrics_review_handler(State(state): State<AppState>) -> axum::response::Response {
    match state.review_metrics_cache().get().await {
        Some(report) => axum::Json(report).into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// `GET /api/console/metrics/mpm` — the latest trusty-mpm report.
///
/// Why this one is coarse: it is the low-frequency session-fleet + supervisor
/// health cache. The Sessions tab polls the live `/api/console/sessions` list at
/// a faster cadence for active monitoring (#1222).
/// What: cached `ConsoleMetricsReport` as JSON (200), or 503 before the first
/// successful poll.
/// Test: `test_metrics_mpm_route_cold_cache_returns_503`.
pub async fn metrics_mpm_handler(State(state): State<AppState>) -> axum::response::Response {
    match state.mpm_metrics_cache().get().await {
        Some(report) => axum::Json(report).into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
