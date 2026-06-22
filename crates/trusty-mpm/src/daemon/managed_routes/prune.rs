//! HTTP handlers for the #1508 ephemeral bulk-teardown + by-state prune routes.
//!
//! Why: the managed-session API needs two new teardown endpoints — one to tear
//! down every ephemeral (test) session, and a general by-state prune that can also
//! purge the legacy 239 stale records. They live in their own file so the route
//! module (`mod.rs`) stays under the 500-SLOC production cap.
//! What: [`decommission_ephemeral_route`] (POST …/managed/decommission-ephemeral)
//! and [`prune_managed_route`] (POST …/managed/prune) handlers, plus the
//! [`PruneRequest`] body. Both delegate to the shared
//! [`crate::session_manager::SessionManager`] prune engine so HTTP, CLI, and MCP
//! share ONE implementation.
//! Test: `prune_route_*` in tests/session_manager_mvp.rs.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use tracing::warn;

use crate::daemon::state::DaemonState;
use crate::session_manager::PruneFilter;

/// Request body for POST /api/v1/sessions/managed/prune (#1508).
///
/// Why: the by-state prune must let an operator choose WHICH records to target
/// (`ephemeral`/`stopped`/`decommissioned`/`all`), preview with `dry_run`, and —
/// only when they really mean it — also include RUNNING sessions via
/// `include_active`. Modelling it as a typed body keeps the safety defaults
/// explicit (running sessions are spared unless `include_active` is `true`).
/// What: `state` (the [`PruneFilter`] spelling), `dry_run` (default false), and
/// `include_active` (default false).
/// Test: `prune_route_dry_run_reports`, `prune_route_rejects_bad_state`.
#[derive(Debug, Deserialize)]
pub struct PruneRequest {
    /// Which records to target: `ephemeral` | `stopped` | `decommissioned` | `all`.
    pub state: String,
    /// When true, report what WOULD be pruned without mutating anything.
    #[serde(default)]
    pub dry_run: bool,
    /// When true, also tear down RUNNING (`Active`/`Provisioning`) sessions.
    /// Defaults to false — the fail-closed safety default.
    #[serde(default)]
    pub include_active: bool,
}

/// POST /api/v1/sessions/managed/decommission-ephemeral — bulk-tear-down ephemeral (#1508).
///
/// Why: the one-shot "clean up all my throwaway test sessions" verb for e2e
/// harnesses and operators. REAL sessions default `ephemeral=false` and are
/// unreachable, so this can never harm durable work.
/// What: delegates to
/// [`SessionManager::decommission_all_ephemeral`](crate::session_manager::SessionManager::decommission_all_ephemeral)
/// and returns `{ decommissioned: <count> }`.
/// Test: `decommission_ephemeral_route_tears_down_only_ephemeral` in
/// tests/session_manager_mvp.rs.
pub async fn decommission_ephemeral_route(
    State(state): State<Arc<DaemonState>>,
) -> impl IntoResponse {
    let mgr = state.session_manager().await;
    match mgr.decommission_all_ephemeral().await {
        Ok(count) => Json(serde_json::json!({ "decommissioned": count })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// POST /api/v1/sessions/managed/prune — by-state prune + compaction (#1508).
///
/// Why: the general teardown tool, exposed so the legacy 239 stale records can be
/// purged over HTTP with the SAME engine that cleans up ephemeral sessions.
/// What: parses the `state` filter (400 on an unknown value), then delegates to
/// [`SessionManager::prune_managed`](crate::session_manager::SessionManager::prune_managed)
/// with the request's `dry_run`/`include_active`. Returns the
/// [`crate::session_manager::PruneOutcome`] JSON (its `dry_run`, `filter`, and
/// per-session `action` list).
/// Test: `prune_route_dry_run_reports`, `prune_route_rejects_bad_state` in
/// tests/session_manager_mvp.rs.
pub async fn prune_managed_route(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<PruneRequest>,
) -> impl IntoResponse {
    let filter = match PruneFilter::parse(&req.state) {
        Ok(f) => f,
        Err(e) => {
            warn!("prune route: {e}");
            return (StatusCode::BAD_REQUEST, e).into_response();
        }
    };
    let mgr = state.session_manager().await;
    match mgr
        .prune_managed(filter, req.dry_run, req.include_active)
        .await
    {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
