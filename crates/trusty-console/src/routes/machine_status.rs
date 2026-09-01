//! `GET /api/console/machine-status` — the aggregated whole-machine view (#6517).
//!
//! Why: the Foundry dashboard's home view needs ONE payload combining the host's
//! own resources (CPU / memory / disk / network) with a rollup of every local
//! trusty-* service's health. Assembling it server-side keeps the phase-2 UI a
//! pure renderer.
//! What: [`machine_status_handler`] reads the background host-metrics cache and
//! each per-service metrics cache, assembles a
//! [`trusty_common::console_metrics::machine_status::MachineStatus`], and returns
//! it as JSON. Returns 503 until the first host sample has completed — the
//! per-service reports are best-effort and a machine with no services still has a
//! valid host status, so only the host snapshot gates the response.
//! Test: `crate::server::tests::machine_status_route_*`.

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use trusty_common::console_metrics::machine_status::MachineStatus;

use crate::server::AppState;

/// `GET /api/console/machine-status` — host resources + per-service rollup.
///
/// Why: serves the dashboard's whole-machine payload without any per-request
/// sampling — the host sampler runs in the background (see
/// [`crate::host_status`]) and the per-service caches are kept warm by the
/// metrics poller.
/// What: reads the host cache; if empty, returns 503 (no sample yet). Otherwise
/// gathers whichever per-service `ConsoleMetricsReport`s are currently cached
/// (absent services are simply omitted from the rollup) and returns the
/// assembled [`MachineStatus`] as JSON.
/// Test: `machine_status_route_cold_cache_returns_503`,
/// `machine_status_route_warm_cache_returns_json`.
pub async fn machine_status_handler(State(state): State<AppState>) -> axum::response::Response {
    let Some(host) = state.host_metrics_cache().get().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let reports = state.collect_service_reports().await;
    let status = MachineStatus::assemble(host, &reports);
    axum::Json(status).into_response()
}
