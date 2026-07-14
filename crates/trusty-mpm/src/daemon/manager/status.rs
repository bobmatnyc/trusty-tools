//! Deterministic cross-project portfolio status rollup (WI-2, #2579).
//!
//! Why: DOC-36 §3.2 gives `tm manager` a `GET /api/v1/manager/status` endpoint
//! that answers "what's going on across EVERYTHING" — the one thing Layer 2
//! structurally cannot do (it is single-session/single-project by construction).
//! Per DOC-35 §11 this cross-project synthesis belongs to #2109 EVEN THOUGH it is
//! non-inferential: "reason across MULTIPLE projects" is scoped here regardless
//! of whether the reasoning happens to be a pure aggregation. Critically, this
//! endpoint must NOT reimplement #2108's per-project registry, status
//! computation, or Deliverable/Milestone state machine — it COMPOSES the existing
//! [`aggregate_project_status`] over every registered project (DOC-36 §5 "never
//! reimplements #2108's status computation") and never mutates anything (§2.1
//! read-only boundary). It contains NO LLM call: given the same materialized
//! state it returns a byte-identical rollup every time.
//! What: defines [`PortfolioStatusResponse`]/[`PortfolioTotals`], the pure
//! aggregation [`aggregate_portfolio_status`] (fetch-once, fold across projects,
//! deterministic name ordering), and the axum handler [`manager_status_route`].
//! Test: the `aggregate_portfolio_status_*` unit tests in this file plus the
//! real-HTTP `manager_status_route_rolls_up_all_projects` /
//! `manager_status_route_empty_portfolio` in `tests/manager_routes.rs`.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tracing::warn;

use crate::daemon::managed_routes::{
    DeliverableStatusCounts, MilestoneStatusCounts, ProjectStatusResponse, SessionStateCounts,
    aggregate_project_status,
};
use crate::daemon::state::DaemonState;
use crate::deliverable::{Deliverable, Milestone};
use crate::project::Project;
use crate::session_manager::SessionRecord;

/// Portfolio-wide aggregate totals summed across every registered project.
///
/// Why: the headline "L2 can't do this" number — one histogram per dimension
/// covering the WHOLE portfolio, so an operator sees "11 active sessions, 3
/// blocked deliverables across all my work" without walking each project. Every
/// field is a pure sum of the per-project [`ProjectStatusResponse`] histograms,
/// so the totals are self-evidently consistent with the `projects` breakdown.
/// What: the summed session, Deliverable, and Milestone histograms plus the
/// single most-recent `last_activity_at` across the entire portfolio.
/// Test: `aggregate_portfolio_status_sums_across_projects`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortfolioTotals {
    /// Session-state histogram summed across all projects.
    pub sessions: SessionStateCounts,
    /// Deliverable-status histogram summed across all projects.
    pub deliverables: DeliverableStatusCounts,
    /// Milestone-status histogram (incl. dangling refs) summed across all projects.
    pub milestones: MilestoneStatusCounts,
    /// Most recent `last_activity_at` across every project's sessions, if any.
    pub last_activity_at: Option<DateTime<Utc>>,
}

/// Deterministic rollup of the whole portfolio (`GET /api/v1/manager/status`).
///
/// Why: the single response body for #2579. It is deliberately a COMPOSITION of
/// #2108's per-project rollup, not a reimplementation: `projects` carries each
/// project's verbatim [`ProjectStatusResponse`] and `totals` is their fold. A
/// consumer can drill into any project or read the portfolio total from one call.
/// What: the number of registered projects, the portfolio-wide `totals`, and the
/// per-project breakdown in deterministic (name-sorted) order. Every field is a
/// pure function of the inputs — zero inference, zero LLM, zero mutation.
/// Test: `aggregate_portfolio_status_sums_across_projects`,
/// `aggregate_portfolio_status_is_deterministic`,
/// `aggregate_portfolio_status_empty_portfolio`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortfolioStatusResponse {
    /// Number of registered projects included in the rollup.
    pub project_count: usize,
    /// Portfolio-wide aggregate totals.
    pub totals: PortfolioTotals,
    /// Per-project deterministic rollups, sorted by project name.
    pub projects: Vec<ProjectStatusResponse>,
}

/// Compute the deterministic portfolio rollup across all registered projects.
///
/// Why: the pure core of #2579, extracted from the handler so its determinism is
/// self-evident (owned/borrowed state in, counting + `max` only, zero I/O, no LLM,
/// no mutation) and it is unit-testable with hand-built inputs. It fetches the
/// session/Deliverable/Milestone stores ONCE and folds
/// [`aggregate_project_status`] over each project — avoiding an N+1 re-read per
/// project and reusing #2108's exact per-project scoping so binding stays
/// identical to the per-project endpoint.
/// What: sorts `projects` by name (deterministic output order), computes each
/// project's [`ProjectStatusResponse`] against the shared slices, folds them into
/// [`PortfolioTotals`], and returns the combined [`PortfolioStatusResponse`].
/// Test: `aggregate_portfolio_status_sums_across_projects`,
/// `aggregate_portfolio_status_is_deterministic`,
/// `aggregate_portfolio_status_empty_portfolio`.
pub fn aggregate_portfolio_status(
    projects: &[Project],
    all_sessions: &[SessionRecord],
    all_deliverables: &[Deliverable],
    all_milestones: &[Milestone],
) -> PortfolioStatusResponse {
    let mut ordered: Vec<&Project> = projects.iter().collect();
    ordered.sort_by(|a, b| a.name.cmp(&b.name));

    let per_project: Vec<ProjectStatusResponse> = ordered
        .into_iter()
        .map(|p| aggregate_project_status(p, all_sessions, all_deliverables, all_milestones))
        .collect();

    rollup_of(per_project)
}

/// Fold an already-computed set of per-project rollups into a portfolio response.
///
/// Why: the digest endpoint needs to build a scope-narrowed snapshot (e.g. one
/// project) from the SAME per-project rollups `/status` produces, without
/// re-deriving them — reusing this keeps the "never re-derive #2108's status"
/// guarantee (§5) intact and the totals self-consistent with `projects`.
/// What: folds `per_project` into [`PortfolioTotals`] and wraps both into a
/// [`PortfolioStatusResponse`], preserving the caller's ordering.
/// Test: `aggregate_portfolio_status_sums_across_projects` (via
/// `aggregate_portfolio_status`); the project-scope digest test in
/// `tests/manager_inference.rs`.
pub(crate) fn rollup_of(per_project: Vec<ProjectStatusResponse>) -> PortfolioStatusResponse {
    let totals = fold_totals(&per_project);
    PortfolioStatusResponse {
        project_count: per_project.len(),
        totals,
        projects: per_project,
    }
}

/// Fold per-project rollups into portfolio-wide totals.
///
/// Why: keeps the summation logic in one exhaustive place so a future histogram
/// field added to [`ProjectStatusResponse`] is a single edit here, and so the
/// pure fold is testable without HTTP. The sum is over already-computed
/// per-project histograms, so it can never disagree with the `projects` breakdown.
/// What: zero-initialises each histogram, then adds every project's counts
/// field-by-field and takes the running `max` of `last_activity_at`.
/// Test: `aggregate_portfolio_status_sums_across_projects`.
fn fold_totals(per_project: &[ProjectStatusResponse]) -> PortfolioTotals {
    let mut sessions = SessionStateCounts {
        provisioning: 0,
        active: 0,
        stopped: 0,
        errored: 0,
        decommissioned: 0,
        total: 0,
    };
    let mut deliverables = DeliverableStatusCounts {
        proposed: 0,
        in_progress: 0,
        blocked: 0,
        complete: 0,
        delivered: 0,
        shipped: 0,
        total: 0,
    };
    let mut milestones = MilestoneStatusCounts {
        proposed: 0,
        in_progress: 0,
        complete: 0,
        shipped: 0,
        total: 0,
        dangling_deliverable_refs: 0,
    };
    let mut last_activity_at: Option<DateTime<Utc>> = None;

    for p in per_project {
        sessions.provisioning += p.sessions.provisioning;
        sessions.active += p.sessions.active;
        sessions.stopped += p.sessions.stopped;
        sessions.errored += p.sessions.errored;
        sessions.decommissioned += p.sessions.decommissioned;
        sessions.total += p.sessions.total;

        deliverables.proposed += p.deliverables.proposed;
        deliverables.in_progress += p.deliverables.in_progress;
        deliverables.blocked += p.deliverables.blocked;
        deliverables.complete += p.deliverables.complete;
        deliverables.delivered += p.deliverables.delivered;
        deliverables.shipped += p.deliverables.shipped;
        deliverables.total += p.deliverables.total;

        milestones.proposed += p.milestones.proposed;
        milestones.in_progress += p.milestones.in_progress;
        milestones.complete += p.milestones.complete;
        milestones.shipped += p.milestones.shipped;
        milestones.total += p.milestones.total;
        milestones.dangling_deliverable_refs += p.milestones.dangling_deliverable_refs;

        if let Some(ts) = p.last_activity_at {
            last_activity_at = Some(match last_activity_at {
                Some(current) => current.max(ts),
                None => ts,
            });
        }
    }

    PortfolioTotals {
        sessions,
        deliverables,
        milestones,
        last_activity_at,
    }
}

/// Load the deterministic portfolio rollup from the daemon's live stores.
///
/// Why: `GET /manager/status`, `/manager/digest`, and `/manager/chat` all need the
/// SAME deterministic snapshot — the digest prompt and the chat context are built
/// from exactly what `/status` returns, never a re-derivation (DOC-36 §3.2/§5
/// "never reimplements #2108's status computation"). Extracting the store reads
/// into one helper is the single reuse point that guarantees that, and keeps the
/// error-to-HTTP mapping consistent across all three handlers.
/// What: lists every registered project plus all session/Deliverable/Milestone
/// records ONCE, folds them via [`aggregate_portfolio_status`], and returns the
/// [`PortfolioStatusResponse`]. Any store read error becomes a
/// `(500, message)` pair the caller renders — the library never `unwrap`s a store
/// result. Read-only: it never mutates a record (§2.1 boundary).
/// Test: `manager_status_route_rolls_up_all_projects`,
/// `manager_status_route_empty_portfolio` in `tests/manager_routes.rs`; the digest
/// and chat coverage in `tests/manager_inference.rs` exercise it via those routes.
pub(crate) async fn load_portfolio_status(
    state: &DaemonState,
) -> Result<PortfolioStatusResponse, (StatusCode, String)> {
    let registry = state.project_registry().await;
    let projects = registry.list().await.map_err(|e| {
        warn!(error = %e, "manager status: project registry read failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "project registry read failed".to_string(),
        )
    })?;

    let sessions = state.session_manager().await.list().await;

    let deliverable_mgr = state.deliverable_manager().await;
    let deliverables = deliverable_mgr.all_deliverables().await.map_err(|e| {
        warn!(error = %e, "manager status: deliverable store read failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "deliverable store read failed".to_string(),
        )
    })?;
    let milestones = deliverable_mgr.all_milestones().await.map_err(|e| {
        warn!(error = %e, "manager status: milestone store read failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "milestone store read failed".to_string(),
        )
    })?;

    Ok(aggregate_portfolio_status(
        &projects,
        &sessions,
        &deliverables,
        &milestones,
    ))
}

/// `GET /api/v1/manager/status` — deterministic cross-project portfolio rollup.
///
/// Why: exposes [`aggregate_portfolio_status`] over HTTP so the manager CLI/TUI
/// (later WIs) and a local `curl` can read the whole-portfolio view without an
/// LLM call and without any channel/bot token (DOC-36 §4 local-testability bar).
/// The handler is a thin, read-only composition over the same registry / session
/// / Deliverable stores the per-project endpoint reads — it adds no persistence,
/// no reasoning, and never mutates a record (§2.1 boundary).
/// What: delegates to [`load_portfolio_status`] and renders the rollup as JSON, or
/// the store-read error as its `(500, message)` pair.
/// Test: `manager_status_route_rolls_up_all_projects`,
/// `manager_status_route_empty_portfolio` in `tests/manager_routes.rs`.
pub async fn manager_status_route(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    match load_portfolio_status(&state).await {
        Ok(rollup) => Json(rollup).into_response(),
        Err((code, msg)) => (code, msg).into_response(),
    }
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
