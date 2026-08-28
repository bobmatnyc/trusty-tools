//! GET /api/v1/sessions/managed/reconcile-worktrees — the report-only
//! worktree inventory (#4207 slice 3, #4288).
//!
//! Why: the reconciled inventory is worthless if nothing surfaces it. Every
//! existing worktree surface (`tm doctor`, `tm session prune-worktrees`) shows
//! only the ADMITTED reclaim candidates, so 33 of the 118 registered worktrees
//! on the dogfood machine — six of them holding uncommitted work, including
//! both landmines #4288 documents — are invisible to an operator today.
//!
//! What: a GET with no body and no side effects. It reads the session store and
//! runs `git worktree list` / `git rev-parse`; it writes nothing, deletes
//! nothing, and takes no `dry_run` parameter because there is no non-dry-run
//! form of it. This is deliberately a separate route from `prune-worktrees`
//! rather than a flag on it: a reporting verb that shares an entry point with a
//! destructive one eventually grows a `force` flag.
//! Test: `reconcile_worktrees_route_reports_without_mutating`.

use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    response::IntoResponse,
    routing::{get, post},
};
use tracing::warn;

use crate::daemon::rpc::managed::outcome::RouteOutcome;
use crate::daemon::state::DaemonState;

/// The two worktree routes as one sub-router (#4288).
///
/// Why: `api.rs` is 1,200+ SLOC and grandfathered under a frozen line-cap
/// budget, so a new route may not simply be appended there. Owning both
/// worktree verbs in one place also keeps them registered as a pair — the
/// destructive one and the read-only one are meant to be read together, and a
/// reviewer looking at either sees the other.
/// What: `POST …/prune-worktrees` (the pre-existing reclaimer, unchanged) and
/// `GET …/reconcile-worktrees` (this module). Both are literal segments, so
/// axum matches them ahead of the `…/managed/{id}` param route regardless of
/// where this sub-router is merged.
/// Test: `reconcile_worktrees_route_reports_without_mutating`;
/// `prune_spares_a_stopped_records_workspace` covers the prune half.
pub fn worktree_routes() -> Router<Arc<DaemonState>> {
    Router::new()
        .route(
            "/api/v1/sessions/managed/prune-worktrees",
            post(super::prune::prune_worktrees_route),
        )
        .route(
            "/api/v1/sessions/managed/reconcile-worktrees",
            get(reconcile_worktrees_route),
        )
}

/// GET /api/v1/sessions/managed/reconcile-worktrees (#4288).
///
/// Why: see the module doc — this is the only surface that reports the excluded
/// set, the three-state classification with a reason per row, and the
/// proposed-adoption list.
/// What: delegates to
/// [`SessionManager::reconcile_worktree_inventory`](crate::session_manager::SessionManager::reconcile_worktree_inventory)
/// against the configured managed workspace root and returns the
/// [`ReconcileReport`](crate::session_manager::worktree_reconcile::ReconcileReport)
/// as JSON. A scan panic becomes a 500 rather than a silently empty inventory —
/// an empty report reads as "nothing to reconcile", which is the worst possible
/// lie for this surface.
/// Test: `reconcile_worktrees_route_reports_without_mutating`.
pub async fn reconcile_worktrees_route(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    reconcile_worktrees_core(&state).await // #6288
}

/// The transport-neutral body of `GET .../managed/reconcile-worktrees` (#6288),
/// served over the socket as `mpm.managed.reconcile_worktrees`.
///
/// Test: `managed_reconcile_worktrees_parity` in `daemon::rpc::managed_tests`.
pub(crate) async fn reconcile_worktrees_core(state: &Arc<DaemonState>) -> RouteOutcome {
    let mgr = state.session_manager().await;
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    let repos_root = crate::core::trusty_tools_config::workspace_root(&config);
    match mgr.reconcile_worktree_inventory(&repos_root).await {
        Ok(report) => RouteOutcome::ok(&report),
        Err(e) => {
            warn!("reconcile-worktrees route: inventory scan failed: {e}");
            RouteOutcome::text(500, format!("worktree reconciliation failed: {e}"))
        }
    }
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod reconcile_tests;
