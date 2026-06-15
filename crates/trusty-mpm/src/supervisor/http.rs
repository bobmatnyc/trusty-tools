//! Supervisor metrics HTTP server: `/metrics` and `/health`.
//!
//! Why: the unattended supervisor exposes fleet state so an operator or a
//! higher-level fleet manager can poll it without attaching to any session. A
//! tiny axum server serving a JSON snapshot satisfies the "metrics exposed on a
//! health endpoint" acceptance criterion while keeping the surface minimal.
//! What: holds a shared [`MetricsHandle`] (an `Arc<RwLock<FleetMetrics>>`) that
//! the supervisor loop updates after each sweep; [`router`] builds the axum
//! [`Router`]; [`serve`] binds and serves it. Gated behind the `daemon` feature
//! because axum is only a dependency there (per the workspace axum-feature rule).
//! Test: `metrics_endpoint_returns_snapshot`, `health_endpoint_ok` in `super::tests`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;
use tokio::sync::RwLock;
use tracing::info;

use super::metrics::FleetMetrics;

/// Shared, lock-guarded fleet-metrics snapshot.
///
/// Why: the loop writes a fresh snapshot after each sweep while HTTP handlers read
/// it concurrently; an `Arc<RwLock<…>>` gives many readers / one writer with no
/// blocking on the (frequent) read path.
/// What: a type alias for the shared handle passed to both the loop and the router.
/// Test: exercised by `metrics_endpoint_returns_snapshot`.
pub type MetricsHandle = Arc<RwLock<FleetMetrics>>;

/// Build a fresh, empty metrics handle.
///
/// Why: the supervisor needs one shared snapshot created before either the loop or
/// the server starts so both reference the same cell.
/// What: wraps a default [`FleetMetrics`] in `Arc<RwLock<…>>`.
/// Test: used by the loop wiring and `metrics_endpoint_returns_snapshot`.
pub fn new_handle() -> MetricsHandle {
    Arc::new(RwLock::new(FleetMetrics::default()))
}

/// Health-check response body.
///
/// Why: `/health` returns a stable, machine-checkable shape so liveness probes
/// (launchd KeepAlive checks, monitoring) can assert `status == "ok"`.
/// What: a one-field struct serialized as `{"status":"ok"}`.
/// Test: `health_endpoint_ok`.
#[derive(Debug, Serialize)]
struct Health {
    /// Always `"ok"` while the server is serving.
    status: &'static str,
}

/// Build the supervisor's metrics router over a shared [`MetricsHandle`].
///
/// Why: separating router construction from `serve` lets tests drive the handlers
/// in-process (via `tower::ServiceExt::oneshot`) without binding a socket.
/// What: returns a [`Router`] with `GET /health` and `GET /metrics`, carrying the
/// handle as axum state.
/// Test: `metrics_endpoint_returns_snapshot`, `health_endpoint_ok`.
pub fn router(handle: MetricsHandle) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .with_state(handle)
}

/// `GET /health` — liveness probe.
///
/// Why: an always-on supervisor needs a trivially-cheap endpoint a process
/// supervisor can hit to confirm it is alive.
/// What: returns `{"status":"ok"}` with a 200.
/// Test: `health_endpoint_ok`.
async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

/// `GET /metrics` — current fleet snapshot.
///
/// Why: the single endpoint a human / fleet manager polls to see counts by
/// lifecycle state, surfaced pending decisions, last activity, and supervisor run
/// stats.
/// What: clones the current [`FleetMetrics`] out from under the read lock and
/// returns it as JSON.
/// Test: `metrics_endpoint_returns_snapshot`.
async fn metrics(State(handle): State<MetricsHandle>) -> Json<FleetMetrics> {
    let snapshot = handle.read().await.clone();
    Json(snapshot)
}

/// Bind and serve the metrics router until the process exits.
///
/// Why: the supervisor runs this as a background task so fleet state is queryable
/// for the whole unattended run.
/// What: binds a `TcpListener` to `addr` and serves [`router`]; logs the bound
/// address. Returns an error only if the bind or serve fails.
/// Test: covered indirectly — handlers are unit-tested via the router; the
/// bind/serve path mirrors the daemon's own `serve_http`.
pub async fn serve(handle: MetricsHandle, addr: SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("supervisor metrics listening on http://{addr}/metrics");
    axum::serve(listener, router(handle)).await?;
    Ok(())
}
