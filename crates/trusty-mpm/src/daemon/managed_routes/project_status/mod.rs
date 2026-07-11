//! Deterministic project status-aggregation route (#2117, DOC-35 §4.1).
//!
//! Why: `tm projects status <name>` (#2115) and the multipane-TUI Projects-pane
//! aggregate glyph (#2118) need a single-call rollup of a project's session
//! landscape without every consumer re-implementing the count/max logic. DOC-35
//! §11 draws a hard boundary: this endpoint is L3-substrate (deterministic
//! control plane) — it polls and reports already-computed state and MUST NEVER
//! call an LLM, reason across projects, or infer anything. It is a pure function
//! of already-materialized state: re-running it with no state change between
//! calls yields byte-identical output. #2382 (Wave 2) extends the rollup with
//! Deliverable/Milestone status histograms (DOC-35 §4.1 extension) computed the
//! same way, over the same kind of already-persisted state.
//! What: defines the response shapes ([`SessionStateCounts`],
//! [`ProjectConfigFlags`], [`DeliverableStatusCounts`],
//! [`MilestoneStatusCounts`], [`ProjectStatusResponse`]), the pure aggregation
//! function [`aggregate_project_status`], its counting helpers
//! ([`count_deliverable_statuses`], [`count_milestone_statuses`]), and the axum
//! handler [`project_status_route`] for `GET /api/v1/projects/{name}/status`.
//! Split into this directory module (`mod.rs` + `tests.rs`) to stay under the
//! 500-SLOC production cap once #2382 added the histogram types/logic —
//! mirrors the `core/session_launch/` mod.rs+tests.rs split pattern.
//! Test: the `aggregate_project_status_*` and `count_*_statuses_*` unit tests in
//! `tests.rs` (pure-rollup contract) and
//! `status_route_returns_deterministic_rollup` /
//! `status_route_unknown_project_is_404` /
//! `status_route_includes_deliverable_and_milestone_histograms` HTTP handler
//! tests in `tests/project_status_route.rs`.

use std::collections::HashSet;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tracing::warn;

use crate::daemon::state::DaemonState;
use crate::deliverable::{
    Deliverable, DeliverableId, DeliverableStatus, Milestone, MilestoneStatus,
};
use crate::project::{Project, ProjectStoreError, fleet_by_project};
use crate::session_manager::{ManagedSessionState, SessionRecord};

/// Histogram of a project's sessions by [`ManagedSessionState`].
///
/// Why: the aggregate view needs a per-state count so a consumer can render
/// "3 active, 1 errored" without walking the raw session list. A typed struct
/// (one field per variant) keeps the wire shape explicit and self-documenting
/// rather than a map keyed by stringly-typed states.
/// What: one `usize` per lifecycle variant plus `total` (the sum of all five —
/// the number of session records bound to the project, tombstones included).
/// Every field is a pure count over already-persisted `SessionRecord.state`.
/// Test: `aggregate_project_status_counts_by_state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionStateCounts {
    /// Count of sessions in [`ManagedSessionState::Provisioning`].
    pub provisioning: usize,
    /// Count of sessions in [`ManagedSessionState::Active`].
    pub active: usize,
    /// Count of sessions in [`ManagedSessionState::Stopped`].
    pub stopped: usize,
    /// Count of sessions in [`ManagedSessionState::Errored`].
    pub errored: usize,
    /// Count of sessions in [`ManagedSessionState::Decommissioned`] (tombstones).
    pub decommissioned: usize,
    /// Total session records bound to the project (sum of all five counts).
    pub total: usize,
}

/// Boolean config-completeness flags for a project (DOC-35 §4.1).
///
/// Why: the aggregate view surfaces at-a-glance whether a project is fully
/// configured for delegated `gh` operations without the consumer reading the
/// full [`Project`] record. Every flag is a pure `Option::is_some()` check over
/// already-persisted config — no validation, no network call, no `gh auth`
/// probe (that mutating/side-effecting check belongs to the PATCH route, §4).
/// What: one bool per config field that gates project automation. `jira_config`
/// is intentionally absent until #2082/#2122 land its persisted field; per §4.1
/// it will be added here additively as `jira_config_set` at that point.
/// Test: `aggregate_project_status_config_flags`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectConfigFlags {
    /// Whether [`Project::gh_user`] is set (preferred `gh` login, #2081).
    pub gh_user_set: bool,
    /// Whether [`Project::github`] (per-project `gh` identity binding, #2184) is set.
    pub github_binding_set: bool,
}

/// Histogram of a project's Deliverables by [`DeliverableStatus`] (#2382, DOC-35
/// §4.1 extension).
///
/// Why: the rollup surfaces at-a-glance how many Deliverables sit in each
/// §10.3 lifecycle state, without every consumer walking
/// [`crate::deliverable::DeliverableManager::all_deliverables`] and
/// re-implementing the tally. A typed struct (one field per variant, like
/// [`SessionStateCounts`]) keeps the wire shape explicit and lets
/// [`count_deliverable_statuses`] use an exhaustive `match` — a future new
/// [`DeliverableStatus`] variant fails this file's build rather than being
/// silently dropped from the count.
/// What: one `usize` per [`DeliverableStatus`] variant plus `total` (the sum of
/// all six). Every field is a pure count over already-persisted
/// `Deliverable::status` values, scoped to one project (§11: single-project
/// scope, zero inference — no normalization, no derived business rules).
/// Test: `count_deliverable_statuses_empty_is_all_zero`,
/// `count_deliverable_statuses_mixed_tally`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeliverableStatusCounts {
    /// Count of Deliverables in [`DeliverableStatus::Proposed`].
    pub proposed: usize,
    /// Count of Deliverables in [`DeliverableStatus::InProgress`].
    pub in_progress: usize,
    /// Count of Deliverables in [`DeliverableStatus::Blocked`].
    pub blocked: usize,
    /// Count of Deliverables in [`DeliverableStatus::Complete`].
    pub complete: usize,
    /// Count of Deliverables in [`DeliverableStatus::Delivered`].
    pub delivered: usize,
    /// Count of Deliverables in [`DeliverableStatus::Shipped`].
    pub shipped: usize,
    /// Total Deliverables scoped to the project (sum of all six counts).
    pub total: usize,
}

/// Histogram of a project's Milestones by [`MilestoneStatus`], plus a
/// referential-integrity SIGNAL — never a repair (#2382, DOC-35 §4.1
/// extension).
///
/// Why: mirrors [`DeliverableStatusCounts`] for Milestones.
/// `dangling_deliverable_refs` surfaces the #2378/#2395 write-path deferral
/// (referential integrity between a Milestone's `deliverables` member ids and
/// the Deliverable store is explicitly NOT validated at write time, per §13
/// Q6 and §11's no-cross-store-reasoning boundary) as a read-only count. This
/// endpoint counts dangling references; it does not validate, fix, or drop
/// them, and it performs no cross-store write.
/// What: one `usize` per [`MilestoneStatus`] variant, `total` (the sum of all
/// four), and `dangling_deliverable_refs` — the count of the project's
/// Milestones that reference at least one [`DeliverableId`] absent from the
/// project's own Deliverable set.
/// Test: `count_milestone_statuses_empty_is_all_zero`,
/// `count_milestone_statuses_mixed_tally`,
/// `count_milestone_statuses_flags_dangling_deliverable_refs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MilestoneStatusCounts {
    /// Count of Milestones in [`MilestoneStatus::Proposed`].
    pub proposed: usize,
    /// Count of Milestones in [`MilestoneStatus::InProgress`].
    pub in_progress: usize,
    /// Count of Milestones in [`MilestoneStatus::Complete`].
    pub complete: usize,
    /// Count of Milestones in [`MilestoneStatus::Shipped`].
    pub shipped: usize,
    /// Total Milestones scoped to the project (sum of all four counts).
    pub total: usize,
    /// Count of Milestones referencing at least one Deliverable id that does
    /// not exist in the project's Deliverable set. A signal only — the #2378
    /// deferred integrity check surfaced read-only, never repaired here.
    pub dangling_deliverable_refs: usize,
}

/// Deterministic rollup of one project's status (`GET .../{name}/status`).
///
/// Why: the single response body for #2117 — the pure-rollup contract of
/// DOC-35 §4.1. It was designed to extend additively, and #2382 (Wave 2) does
/// exactly that: `deliverables`/`milestones` histogram fields, computed the
/// same way over the same kind of already-persisted state now that the §10
/// data model (#2378) has landed. Adding serde-serialized fields is
/// non-breaking for existing consumers, so no version bump or shape change is
/// required.
/// What: the project identity (`project_name`/`repo_url`), the session-state
/// histogram, the most-recent `last_activity_at` across the project's sessions,
/// the config-completeness flags, and (as of #2382) the Deliverable/Milestone
/// status histograms. Every field is a pure function of the inputs — zero
/// inference, zero LLM, single-project scope.
/// Test: `aggregate_project_status_counts_by_state`,
/// `aggregate_project_status_max_activity`, `aggregate_project_status_config_flags`,
/// `aggregate_project_status_includes_deliverable_and_milestone_histograms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectStatusResponse {
    /// Registered project name (the request path key).
    pub project_name: String,
    /// Repository URL for the project.
    pub repo_url: String,
    /// Per-state session histogram.
    pub sessions: SessionStateCounts,
    /// Most recent `last_activity_at` across the project's sessions, if any.
    ///
    /// `None` when no bound session has ever recorded activity.
    pub last_activity_at: Option<DateTime<Utc>>,
    /// Config-completeness flags.
    pub config: ProjectConfigFlags,
    /// Deliverable status histogram, scoped to this project (#2382).
    pub deliverables: DeliverableStatusCounts,
    /// Milestone status histogram, scoped to this project (#2382).
    pub milestones: MilestoneStatusCounts,
}

/// Compute the deterministic status rollup for a project.
///
/// Why: the pure core of #2117 (extended by #2382), extracted from the handler
/// so the rollup logic is unit-testable with hand-constructed inputs and so its
/// determinism is self-evident — it takes owned/borrowed state, performs only
/// counting and a `max`, and does zero I/O, no LLM call, and no cross-project
/// reasoning (DOC-35 §11 boundary contract). Given the same inputs it returns a
/// byte-identical [`ProjectStatusResponse`] every time.
/// What: filters `all_sessions` to those bound to `project` (reusing the shared
/// [`fleet_by_project`] URL-matching so binding stays consistent with the
/// `/fleet` route), tallies a per-state histogram, takes the maximum
/// `last_activity_at` across the bound sessions, and reads the two config flags
/// off the record. Sessions with no `repo_url` (or an unmatched one) are omitted,
/// exactly as `fleet_by_project` specifies. As of #2382, also filters
/// `all_deliverables`/`all_milestones` to those whose `project_name` matches
/// `project.name`, then delegates to [`count_deliverable_statuses`] /
/// [`count_milestone_statuses`] for the two new histograms.
/// Test: `aggregate_project_status_counts_by_state`,
/// `aggregate_project_status_max_activity`, `aggregate_project_status_config_flags`,
/// `aggregate_project_status_is_deterministic`,
/// `aggregate_project_status_includes_deliverable_and_milestone_histograms`.
pub fn aggregate_project_status(
    project: &Project,
    all_sessions: &[SessionRecord],
    all_deliverables: &[Deliverable],
    all_milestones: &[Milestone],
) -> ProjectStatusResponse {
    // Reuse the shared fleet grouping so "which sessions belong to this project"
    // is answered identically here and by the /fleet route. Passing a
    // single-project slice yields exactly one ProjectFleet.
    let bound: Vec<SessionRecord> = fleet_by_project(all_sessions, std::slice::from_ref(project))
        .into_iter()
        .next()
        .map(|fleet| fleet.sessions)
        .unwrap_or_default();

    let mut counts = SessionStateCounts {
        provisioning: 0,
        active: 0,
        stopped: 0,
        errored: 0,
        decommissioned: 0,
        total: 0,
    };
    let mut last_activity_at: Option<DateTime<Utc>> = None;

    for session in &bound {
        match session.state {
            ManagedSessionState::Provisioning => counts.provisioning += 1,
            ManagedSessionState::Active => counts.active += 1,
            ManagedSessionState::Stopped => counts.stopped += 1,
            ManagedSessionState::Errored => counts.errored += 1,
            ManagedSessionState::Decommissioned => counts.decommissioned += 1,
        }
        counts.total += 1;

        if let Some(ts) = session.last_activity_at {
            last_activity_at = Some(match last_activity_at {
                Some(current) => current.max(ts),
                None => ts,
            });
        }
    }

    // Scope Deliverables/Milestones to this project ONLY (§11: single-project
    // scope) before tallying either histogram.
    let project_deliverables: Vec<&Deliverable> = all_deliverables
        .iter()
        .filter(|d| d.project_name == project.name)
        .collect();
    let project_milestones: Vec<&Milestone> = all_milestones
        .iter()
        .filter(|m| m.project_name == project.name)
        .collect();
    let project_deliverable_ids: HashSet<DeliverableId> =
        project_deliverables.iter().map(|d| d.id).collect();

    ProjectStatusResponse {
        project_name: project.name.clone(),
        repo_url: project.repo_url.clone(),
        sessions: counts,
        last_activity_at,
        config: ProjectConfigFlags {
            gh_user_set: project.gh_user.is_some(),
            github_binding_set: project.github.is_some(),
        },
        deliverables: count_deliverable_statuses(&project_deliverables),
        milestones: count_milestone_statuses(&project_milestones, &project_deliverable_ids),
    }
}

/// Tally a project-scoped Deliverable slice into a [`DeliverableStatusCounts`].
///
/// Why: the pure counting core DOC-35 §11 requires — no normalization, no
/// derived business rules, just an exhaustive tally so a future
/// [`DeliverableStatus`] variant fails to compile here instead of silently
/// under-counting.
/// What: one pass over `deliverables`, incrementing exactly one bucket per
/// record via an exhaustive `match` over every [`DeliverableStatus`] variant
/// (no wildcard arm), plus `total`.
/// Test: `count_deliverable_statuses_empty_is_all_zero`,
/// `count_deliverable_statuses_mixed_tally`.
fn count_deliverable_statuses(deliverables: &[&Deliverable]) -> DeliverableStatusCounts {
    let mut counts = DeliverableStatusCounts {
        proposed: 0,
        in_progress: 0,
        blocked: 0,
        complete: 0,
        delivered: 0,
        shipped: 0,
        total: 0,
    };
    for d in deliverables {
        match d.status {
            DeliverableStatus::Proposed => counts.proposed += 1,
            DeliverableStatus::InProgress => counts.in_progress += 1,
            DeliverableStatus::Blocked => counts.blocked += 1,
            DeliverableStatus::Complete => counts.complete += 1,
            DeliverableStatus::Delivered => counts.delivered += 1,
            DeliverableStatus::Shipped => counts.shipped += 1,
        }
        counts.total += 1;
    }
    counts
}

/// Tally a project-scoped Milestone slice into a [`MilestoneStatusCounts`],
/// including the dangling-deliverable-ref signal.
///
/// Why: same pure-counting contract as [`count_deliverable_statuses`], plus the
/// #2378-deferred referential-integrity signal — surfaced here as a count,
/// never repaired or (re-)validated (§11, §13 Q6).
/// What: one pass over `milestones`, incrementing exactly one status bucket per
/// record via an exhaustive `match` over every [`MilestoneStatus`] variant (no
/// wildcard arm), `total`, and `dangling_deliverable_refs` — incremented once
/// per Milestone that references at least one id absent from
/// `project_deliverable_ids`.
/// Test: `count_milestone_statuses_empty_is_all_zero`,
/// `count_milestone_statuses_mixed_tally`,
/// `count_milestone_statuses_flags_dangling_deliverable_refs`.
fn count_milestone_statuses(
    milestones: &[&Milestone],
    project_deliverable_ids: &HashSet<DeliverableId>,
) -> MilestoneStatusCounts {
    let mut counts = MilestoneStatusCounts {
        proposed: 0,
        in_progress: 0,
        complete: 0,
        shipped: 0,
        total: 0,
        dangling_deliverable_refs: 0,
    };
    for m in milestones {
        match m.status {
            MilestoneStatus::Proposed => counts.proposed += 1,
            MilestoneStatus::InProgress => counts.in_progress += 1,
            MilestoneStatus::Complete => counts.complete += 1,
            MilestoneStatus::Shipped => counts.shipped += 1,
        }
        counts.total += 1;
        if m.deliverables
            .iter()
            .any(|id| !project_deliverable_ids.contains(id))
        {
            counts.dangling_deliverable_refs += 1;
        }
    }
    counts
}

/// GET /api/v1/projects/{name}/status — deterministic project status rollup.
///
/// Why: exposes [`aggregate_project_status`] over HTTP so the deterministic CLI
/// (#2115) and TUI (#2118) — and any other consumer, including #2109 per the
/// §11 "expose data over MCP/HTTP for ANY consumer to read" row — can read the
/// rollup without an MCP client. The handler is a thin composition over
/// `ProjectRegistry::get` + `SessionManager::list` + (as of #2382)
/// `DeliverableManager::all_deliverables`/`all_milestones`; it adds no
/// persistence and no reasoning.
/// What: looks up the named project (404 if unregistered), lists all live
/// session records plus all Deliverable/Milestone records, and returns
/// `Json(aggregate_project_status(..))`. A project store read error (other than
/// not-found) or a Deliverable/Milestone store read error degrades to 500 with
/// a logged warning rather than a panic — the library never `unwrap`s a store
/// result.
/// Test: `status_route_returns_deterministic_rollup`,
/// `status_route_unknown_project_is_404`,
/// `status_route_includes_deliverable_and_milestone_histograms` in
/// `tests/project_status_route.rs`.
pub async fn project_status_route(
    State(state): State<Arc<DaemonState>>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    let registry = state.project_registry().await;
    let project = match registry.get(&name).await {
        Ok(project) => project,
        Err(ProjectStoreError::NotFound(_)) => {
            return (StatusCode::NOT_FOUND, format!("project {name} not found")).into_response();
        }
        Err(e) => {
            warn!(
                error = %e,
                project = %name,
                "project_status_route: project registry read failed"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "project registry read failed".to_string(),
            )
                .into_response();
        }
    };

    let mgr = state.session_manager().await;
    let sessions = mgr.list().await;

    let deliverable_mgr = state.deliverable_manager().await;
    let deliverables = match deliverable_mgr.all_deliverables().await {
        Ok(d) => d,
        Err(e) => {
            warn!(
                error = %e,
                project = %name,
                "project_status_route: deliverable store read failed"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "deliverable store read failed".to_string(),
            )
                .into_response();
        }
    };
    let milestones = match deliverable_mgr.all_milestones().await {
        Ok(m) => m,
        Err(e) => {
            warn!(
                error = %e,
                project = %name,
                "project_status_route: milestone store read failed"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "milestone store read failed".to_string(),
            )
                .into_response();
        }
    };

    Json(aggregate_project_status(
        &project,
        &sessions,
        &deliverables,
        &milestones,
    ))
    .into_response()
}

#[cfg(test)]
mod tests;
