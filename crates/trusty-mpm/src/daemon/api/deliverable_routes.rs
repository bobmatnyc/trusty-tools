//! Deliverable/Milestone CRUD routes (DOC-35 §10.2/§10.5; #2378 + #2380).
//!
//! Why: the L3 substrate exposes its Deliverable/Milestone ledger over the SAME
//! HTTP surface every other subsystem uses, so any consumer (the CLI #2381, the
//! TUI #2383, `tm manager` #2109) reads and mutates it deterministically. These
//! handlers are the create/read/update seam nested under the projects namespace
//! (`/api/v1/projects/{name}/deliverables[/{id}]` and `.../milestones[/{id}]`),
//! and they are where the §10.3 status state machine is ENFORCED (#2380): an
//! illegal `set-status` PATCH is rejected with a structured 409 naming the legal
//! next states. Everything here is a pure function of stored state — no LLM, no
//! inference, single-project scope (§11).
//! What: eight handlers (list/create/get/patch for Deliverables and Milestones),
//! their request/response DTOs, and small store-error → [`DaemonError`] mappers.
//! Status-transition validation delegates to
//! [`crate::deliverable::validate_transition`] (unit-tested exhaustively in
//! `deliverable::status`), so the enforcement rule has ONE source of truth. Per
//! the #2395 review HIGH, both PATCH handlers run their entire
//! fetch→validate→mutate→persist sequence inside ONE closure passed to
//! [`crate::deliverable::DeliverableManager::update_deliverable_with`] /
//! [`update_milestone_with`](crate::deliverable::DeliverableManager::update_milestone_with) —
//! a single write-lock guard held for the whole sequence, so two concurrent
//! PATCHes can never interleave between a separate fetch-lock and a separate
//! persist-lock (the two-lock shape this replaced).
//! Test: `tests` submodule drives every handler in-process, including the full
//! illegal-transition rejection path and the blank-name-on-PATCH rejection. The
//! concurrency regression itself is proven at the manager layer —
//! `deliverable::manager::tests::update_deliverable_with_serializes_concurrent_transitions`
//! and `..::read_then_upsert_pattern_reproduces_lost_update_pre_fix`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Json, extract::rejection::JsonRejection};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::daemon::error::DaemonError;
use crate::daemon::state::DaemonState;
use crate::deliverable::manager::UpdateError;
use crate::deliverable::{
    Deliverable, DeliverableId, DeliverableKind, DeliverableStatus, EstimationTier, Milestone,
    MilestoneId, MilestoneStatus, StoreError, validate_transition,
};

#[cfg(test)]
mod tests;

/// Map a store I/O/serialization error to a 500; `NotFound` never reaches here
/// on write paths, so it is bucketed as an internal fault.
///
/// Why: create/list/save paths have no legitimate `NotFound`; surfacing any
/// store failure as a 500 keeps those handlers honest about unexpected I/O.
/// What: renders the error as [`DaemonError::Internal`].
/// Test: covered indirectly by the happy-path handler tests.
fn store_internal(e: StoreError) -> DaemonError {
    DaemonError::Internal(e.to_string())
}

/// Translate a `NotFound` on a Deliverable lookup into a typed 404.
///
/// Why: the GET/PATCH handlers must return a Deliverable-specific 404 (not a
/// generic session 404) so the error body names the right resource.
/// What: `NotFound` → [`DaemonError::DeliverableNotFound`]; any other store error
/// → 500.
/// Test: `get_unknown_deliverable_is_404`.
fn deliverable_lookup_err(e: StoreError, id: &str) -> DaemonError {
    match e {
        StoreError::NotFound(_) => DaemonError::DeliverableNotFound { id: id.to_string() },
        other => store_internal(other),
    }
}

/// Translate a `NotFound` on a Milestone lookup into a typed 404.
fn milestone_lookup_err(e: StoreError, id: &str) -> DaemonError {
    match e {
        StoreError::NotFound(_) => DaemonError::MilestoneNotFound { id: id.to_string() },
        other => store_internal(other),
    }
}

/// Translate an atomic-update [`UpdateError`] into the right [`DaemonError`].
///
/// Why: [`crate::deliverable::DeliverableManager::update_deliverable_with`] (the
/// #2395-review fix for the lost-update hazard) returns one error type covering
/// both a store failure and a rejected §10.3 transition; the route layer must
/// still map each to its own HTTP status — 404 for not-found/wrong-project, 409
/// for an illegal transition — exactly as the pre-fix two-call path did.
/// What: `UpdateError::Store(NotFound)` → [`DaemonError::DeliverableNotFound`];
/// any other `Store` variant → 500; `UpdateError::Transition` →
/// [`DaemonError::InvalidTransition`] carrying the legal next states.
/// Test: `patch_illegal_transition_is_409_with_allowed_next`,
/// `get_unknown_deliverable_is_404` (via `get_wrong_project_is_404`'s PATCH twin).
fn update_err(e: UpdateError, id: &str) -> DaemonError {
    match e {
        UpdateError::Store(store_err) => deliverable_lookup_err(store_err, id),
        UpdateError::Transition(te) => DaemonError::InvalidTransition {
            from: te.from.to_string(),
            to: te.to.to_string(),
            allowed: te.allowed.iter().map(|s| s.as_str().to_string()).collect(),
        },
    }
}

/// Reject a PATCH `name` field that is present but blank after trimming.
///
/// Why: `create_deliverable`/`create_milestone` already enforce a non-empty
/// trimmed name; PATCH left the same field settable to whitespace-only or empty
/// (#2395 review MEDIUM) — the same rule must apply on both paths so a record
/// can never end up with a blank name via an update that creation would have
/// rejected outright.
/// What: `None` (field absent from the PATCH body) is always fine — the caller
/// is not touching `name`. `Some(name)` where `name.trim()` is empty is
/// rejected with a 400; any other `Some` passes.
/// Test: `patch_deliverable_rejects_blank_name`, `patch_milestone_rejects_blank_name`.
fn reject_blank_patch_name(name: &Option<String>, resource: &str) -> Result<(), DaemonError> {
    if let Some(n) = name
        && n.trim().is_empty()
    {
        return Err(DaemonError::InvalidRequest(format!(
            "{resource} name must not be empty"
        )));
    }
    Ok(())
}

/// Unwrap a JSON body, mapping a deserialization rejection to a 400.
///
/// Why: axum's default `JsonRejection` renders a bare 422 with a plain-text
/// body; routing it through [`DaemonError::InvalidRequest`] gives a consistent
/// `{ "error": … }` shape and a 400 for malformed input.
/// What: returns the parsed body or an `InvalidRequest` error.
/// Test: exercised by the malformed-body handler test.
fn body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, DaemonError> {
    body.map(|Json(v)| v)
        .map_err(|e| DaemonError::InvalidRequest(e.body_text()))
}

// ───────────────────────── Deliverable DTOs ──────────────────────────

/// Request body for `POST /api/v1/projects/{name}/deliverables`.
///
/// Why: creation fixes the immutable facts of a Deliverable; `status` is always
/// `Proposed` at creation (§10.3 forbids starting anywhere else), `id` and
/// `created_at` are server-assigned, so neither is accepted from the client.
/// What: the required `name`, `kind`, and `estimated_effort` plus the optional
/// description and opaque `ticket_ref`/`spec_ref`/`target_date` slots (§13 Q6).
///
/// **Referential integrity is deferred by design, not a bug:** `project_name`
/// (taken from the URL path, not the body) is never checked against the
/// registry-B project store — a Deliverable can be created under a
/// `project_name` that does not (yet, or ever) exist as a registered `Project`.
/// This matches §13 Q6's opaque-reference treatment of `ticket_ref`/`spec_ref`
/// (validation deferred, gh-first / plain-path convention, no cross-store
/// lookups added here) and keeps this endpoint's boundary strictly deterministic
/// per §11 (no cross-store reasoning). A future consumer (the #2382 rollup
/// endpoint, or `tm manager` #2109) can surface a dangling `project_name` as a
/// read-only signal without this write path needing to enforce it.
/// Test: `create_then_get_round_trips`.
#[derive(Debug, Deserialize)]
pub struct CreateDeliverable {
    /// Human-readable name (required, non-empty).
    pub name: String,
    /// Free-form description.
    #[serde(default)]
    pub description: String,
    /// The category of work.
    pub kind: DeliverableKind,
    /// Coarse effort estimate (S/M/L/XL).
    pub estimated_effort: EstimationTier,
    /// Opaque gh-first ticket reference (§13 Q6).
    #[serde(default)]
    pub ticket_ref: Option<String>,
    /// Repo-relative spec path (§10.4).
    #[serde(default)]
    pub spec_ref: Option<String>,
    /// Optional target completion date.
    #[serde(default)]
    pub target_date: Option<DateTime<Utc>>,
}

/// Request body for `PATCH /api/v1/projects/{name}/deliverables/{id}`.
///
/// Why: PATCH is a partial, idempotent update; every field is optional and only
/// the present fields are applied. A present `status` drives the §10.3 state
/// machine (#2380) — a change to an illegal next state is rejected; setting
/// `status` to the record's CURRENT value is a no-op (not an error), so echoing
/// state alongside a field edit never spuriously fails. `name`, if present, must
/// be non-blank after trimming (same rule `create_deliverable` enforces).
/// What: all Deliverable-mutable fields as `Option`s.
///
/// **v1 limitation (deferred by design, not a bug):** because every field here
/// is a single `Option`, there is no way to distinguish "absent from the JSON
/// body" from "explicitly set to `null`" for `ticket_ref`/`spec_ref`/
/// `target_date` — both deserialize to `None` and this handler treats `None` as
/// "leave unchanged" (see `patch_deliverable`'s `if patch.ticket_ref.is_some()`
/// guards). Clearing an already-set `ticket_ref`/`spec_ref`/`target_date` back to
/// absent is therefore NOT supported in v1; that would require a double-`Option`
/// (`Option<Option<String>>`, distinguishing "key omitted" from "key present with
/// value `null`") which is deliberately deferred until a real caller needs it.
/// Test: `patch_updates_fields`, `patch_illegal_transition_is_409_with_allowed_next`,
/// `patch_same_status_is_noop`, `patch_deliverable_rejects_blank_name`.
#[derive(Debug, Default, Deserialize)]
pub struct PatchDeliverable {
    /// New name, if changing.
    #[serde(default)]
    pub name: Option<String>,
    /// New description, if changing.
    #[serde(default)]
    pub description: Option<String>,
    /// New kind, if changing.
    #[serde(default)]
    pub kind: Option<DeliverableKind>,
    /// New estimate, if changing.
    #[serde(default)]
    pub estimated_effort: Option<EstimationTier>,
    /// New ticket ref, if changing.
    #[serde(default)]
    pub ticket_ref: Option<String>,
    /// New spec ref, if changing.
    #[serde(default)]
    pub spec_ref: Option<String>,
    /// New status — validated against the §10.3 state machine (#2380).
    #[serde(default)]
    pub status: Option<DeliverableStatus>,
    /// New target date, if changing.
    #[serde(default)]
    pub target_date: Option<DateTime<Utc>>,
}

/// Query parameters for `GET .../deliverables`.
///
/// Why: `tm projects deliverables list <project> [--status <s>]` (§10.8) filters
/// by status; the optional query param mirrors that.
/// What: an optional [`DeliverableStatus`] filter.
/// Test: `list_filters_by_status_and_scopes_to_project`.
#[derive(Debug, Default, Deserialize)]
pub struct DeliverableListQuery {
    /// Optional status filter.
    #[serde(default)]
    pub status: Option<DeliverableStatus>,
}

/// Response body for `GET .../deliverables`.
#[derive(Debug, Serialize)]
pub struct DeliverablesResponse {
    /// The project's Deliverables (optionally status-filtered).
    pub deliverables: Vec<Deliverable>,
}

// ───────────────────────── Deliverable handlers ──────────────────────

/// `POST /api/v1/projects/{name}/deliverables` — create a Deliverable.
///
/// Why: the entry point for tracking a new unit of work against a project.
/// What: builds a `Deliverable` with a fresh id, `status = Proposed`, and
/// server-assigned `created_at`; rejects an empty name with 400; persists it and
/// returns 201 with the created record.
/// Test: `create_then_get_round_trips`, `create_rejects_empty_name`.
pub async fn create_deliverable(
    State(state): State<Arc<DaemonState>>,
    Path(project): Path<String>,
    body_in: Result<Json<CreateDeliverable>, JsonRejection>,
) -> Result<(StatusCode, Json<Deliverable>), DaemonError> {
    let req = body(body_in)?;
    if req.name.trim().is_empty() {
        return Err(DaemonError::InvalidRequest(
            "deliverable name must not be empty".into(),
        ));
    }
    let deliverable = Deliverable {
        id: DeliverableId::new(),
        project_name: project,
        name: req.name,
        description: req.description,
        kind: req.kind,
        ticket_ref: req.ticket_ref,
        spec_ref: req.spec_ref,
        status: DeliverableStatus::Proposed,
        estimated_effort: req.estimated_effort,
        created_at: Utc::now(),
        target_date: req.target_date,
    };
    let mgr = state.deliverable_manager().await;
    mgr.upsert_deliverable(deliverable.clone())
        .await
        .map_err(store_internal)?;
    Ok((StatusCode::CREATED, Json(deliverable)))
}

/// `GET /api/v1/projects/{name}/deliverables` — list a project's Deliverables.
///
/// Why: the project-scoped list view (§10.8), optionally filtered by status.
/// What: returns every Deliverable whose `project_name` matches, applying the
/// optional `?status=` filter.
/// Test: `list_filters_by_status_and_scopes_to_project`.
pub async fn list_deliverables(
    State(state): State<Arc<DaemonState>>,
    Path(project): Path<String>,
    Query(query): Query<DeliverableListQuery>,
) -> Result<Json<DeliverablesResponse>, DaemonError> {
    let mgr = state.deliverable_manager().await;
    let mut deliverables = mgr
        .deliverables_by_project(&project)
        .await
        .map_err(store_internal)?;
    if let Some(status) = query.status {
        deliverables.retain(|d| d.status == status);
    }
    Ok(Json(DeliverablesResponse { deliverables }))
}

/// Fetch a Deliverable and confirm it belongs to `project` (404 otherwise).
///
/// Why: a Deliverable id is global, but the route nests it under a project; a
/// mismatched project must 404 rather than leak another project's record.
/// What: looks up by id, maps `NotFound` to a typed 404, then enforces the
/// project scope.
/// Test: `get_wrong_project_is_404`.
async fn fetch_scoped(
    state: &Arc<DaemonState>,
    project: &str,
    id: &str,
) -> Result<Deliverable, DaemonError> {
    let mgr = state.deliverable_manager().await;
    let d = mgr
        .get_deliverable(id)
        .await
        .map_err(|e| deliverable_lookup_err(e, id))?;
    if d.project_name != project {
        return Err(DaemonError::DeliverableNotFound { id: id.to_string() });
    }
    Ok(d)
}

/// `GET /api/v1/projects/{name}/deliverables/{id}` — fetch one Deliverable.
///
/// Why: point lookup without paging the full list.
/// What: returns the Deliverable if it exists AND belongs to `project`; 404
/// otherwise.
/// Test: `create_then_get_round_trips`, `get_unknown_deliverable_is_404`.
pub async fn get_deliverable(
    State(state): State<Arc<DaemonState>>,
    Path((project, id)): Path<(String, String)>,
) -> Result<Json<Deliverable>, DaemonError> {
    let d = fetch_scoped(&state, &project, &id).await?;
    Ok(Json(d))
}

/// `PATCH /api/v1/projects/{name}/deliverables/{id}` — update a Deliverable.
///
/// Why: the single mutation seam — field edits and the `set-status` transition
/// both flow through here. This is where #2380 is enforced. Per the #2395
/// review HIGH, the fetch, the transition validation, the field mutation, AND
/// the persist all run inside ONE closure passed to
/// [`DeliverableManager::update_deliverable_with`](crate::deliverable::DeliverableManager::update_deliverable_with),
/// which holds a single write-lock guard across the whole sequence — no other
/// concurrent PATCH can observe or clobber the intermediate state (see that
/// method's doc for the exact hazard this closes).
/// What: rejects a present-but-blank `name` before touching the store (MEDIUM
/// fix — matches `create_deliverable`'s rule). Then atomically: fetches the
/// project-scoped record, and — when `status` changes — validates the
/// transition against the §10.3 machine via [`validate_transition`], rejecting
/// an illegal change with a structured 409 naming the legal next states. A
/// `status` equal to the current value is a no-op. Applies every other present
/// field, persists, and returns the updated record — all under the manager's
/// single lock.
/// Test: `patch_updates_fields`, `patch_full_legal_lifecycle_succeeds`,
/// `patch_illegal_transition_is_409_with_allowed_next`, `patch_same_status_is_noop`,
/// `patch_deliverable_rejects_blank_name`.
pub async fn patch_deliverable(
    State(state): State<Arc<DaemonState>>,
    Path((project, id)): Path<(String, String)>,
    body_in: Result<Json<PatchDeliverable>, JsonRejection>,
) -> Result<Json<Deliverable>, DaemonError> {
    let patch = body(body_in)?;
    reject_blank_patch_name(&patch.name, "deliverable")?;

    let mgr = state.deliverable_manager().await;
    let updated = mgr
        .update_deliverable_with(&id, &project, move |mut d| {
            // #2380: enforce the status state machine BEFORE any other mutation,
            // so a rejected transition leaves the record untouched. Validated
            // against `d.status`, which was fetched and will be persisted under
            // the SAME lock this closure runs inside — never a stale read.
            if let Some(new_status) = patch.status
                && new_status != d.status
            {
                validate_transition(d.status, new_status)?;
                d.status = new_status;
            }

            if let Some(name) = patch.name {
                d.name = name;
            }
            if let Some(description) = patch.description {
                d.description = description;
            }
            if let Some(kind) = patch.kind {
                d.kind = kind;
            }
            if let Some(effort) = patch.estimated_effort {
                d.estimated_effort = effort;
            }
            if patch.ticket_ref.is_some() {
                d.ticket_ref = patch.ticket_ref;
            }
            if patch.spec_ref.is_some() {
                d.spec_ref = patch.spec_ref;
            }
            if patch.target_date.is_some() {
                d.target_date = patch.target_date;
            }
            Ok(d)
        })
        .await
        .map_err(|e| update_err(e, &id))?;
    Ok(Json(updated))
}

// ───────────────────────── Milestone DTOs ────────────────────────────

/// Request body for `POST /api/v1/projects/{name}/milestones`.
///
/// Why: creation fixes the immutable facts; `id`/`created_at` are server-assigned.
/// What: required `name` and `target_date`, optional description, member
/// Deliverable ids, and a rollup `status` (default `Proposed`).
///
/// **Referential integrity is deferred by design, not a bug:** neither
/// `project_name` (URL path, not checked against the registry-B project store —
/// same deferral as `CreateDeliverable`) nor each entry of `deliverables` (not
/// checked against the deliverables store — a Milestone can reference a
/// `DeliverableId` that does not exist, belongs to a different project, or is
/// later deleted) is validated here. No cross-store lookups are added to this
/// write path (§11 boundary); a dangling member id is a signal the #2382 rollup
/// endpoint (or a future `tm manager` #2109 consumer) can surface read-only.
/// Test: `milestone_create_then_get`.
#[derive(Debug, Deserialize)]
pub struct CreateMilestone {
    /// Human-readable name (required, non-empty).
    pub name: String,
    /// Free-form description.
    #[serde(default)]
    pub description: String,
    /// The date this Milestone targets.
    pub target_date: DateTime<Utc>,
    /// Member Deliverable ids.
    #[serde(default)]
    pub deliverables: Vec<DeliverableId>,
    /// Rollup status (default `Proposed`, §10.5).
    #[serde(default)]
    pub status: MilestoneStatus,
}

/// Request body for `PATCH /api/v1/projects/{name}/milestones/{id}`.
///
/// Why: partial update. Milestone `status` is a rollup field (§10.3/§10.5), NOT
/// a user-driven state machine, so — unlike Deliverable — it is set directly with
/// no transition enforcement (the deterministic rollup that overwrites it is
/// #2382 / the read-only status endpoint's job). `name`, if present, must be
/// non-blank after trimming (same rule `create_milestone` enforces).
/// What: all Milestone-mutable fields as `Option`s.
/// Test: `milestone_patch_updates_fields`, `patch_milestone_rejects_blank_name`.
#[derive(Debug, Default, Deserialize)]
pub struct PatchMilestone {
    /// New name, if changing.
    #[serde(default)]
    pub name: Option<String>,
    /// New description, if changing.
    #[serde(default)]
    pub description: Option<String>,
    /// New target date, if changing.
    #[serde(default)]
    pub target_date: Option<DateTime<Utc>>,
    /// New rollup status, if changing (no transition enforcement — see above).
    #[serde(default)]
    pub status: Option<MilestoneStatus>,
    /// Replacement member Deliverable id list, if changing.
    #[serde(default)]
    pub deliverables: Option<Vec<DeliverableId>>,
}

/// Response body for `GET .../milestones`.
#[derive(Debug, Serialize)]
pub struct MilestonesResponse {
    /// The project's Milestones.
    pub milestones: Vec<Milestone>,
}

// ───────────────────────── Milestone handlers ────────────────────────

/// `POST /api/v1/projects/{name}/milestones` — create a Milestone.
pub async fn create_milestone(
    State(state): State<Arc<DaemonState>>,
    Path(project): Path<String>,
    body_in: Result<Json<CreateMilestone>, JsonRejection>,
) -> Result<(StatusCode, Json<Milestone>), DaemonError> {
    let req = body(body_in)?;
    if req.name.trim().is_empty() {
        return Err(DaemonError::InvalidRequest(
            "milestone name must not be empty".into(),
        ));
    }
    let milestone = Milestone {
        id: MilestoneId::new(),
        project_name: project,
        name: req.name,
        description: req.description,
        target_date: req.target_date,
        status: req.status,
        deliverables: req.deliverables,
        created_at: Utc::now(),
    };
    let mgr = state.deliverable_manager().await;
    mgr.upsert_milestone(milestone.clone())
        .await
        .map_err(store_internal)?;
    Ok((StatusCode::CREATED, Json(milestone)))
}

/// `GET /api/v1/projects/{name}/milestones` — list a project's Milestones.
pub async fn list_milestones(
    State(state): State<Arc<DaemonState>>,
    Path(project): Path<String>,
) -> Result<Json<MilestonesResponse>, DaemonError> {
    let mgr = state.deliverable_manager().await;
    let milestones = mgr
        .milestones_by_project(&project)
        .await
        .map_err(store_internal)?;
    Ok(Json(MilestonesResponse { milestones }))
}

/// Fetch a Milestone and confirm it belongs to `project` (404 otherwise).
async fn fetch_scoped_milestone(
    state: &Arc<DaemonState>,
    project: &str,
    id: &str,
) -> Result<Milestone, DaemonError> {
    let mgr = state.deliverable_manager().await;
    let m = mgr
        .get_milestone(id)
        .await
        .map_err(|e| milestone_lookup_err(e, id))?;
    if m.project_name != project {
        return Err(DaemonError::MilestoneNotFound { id: id.to_string() });
    }
    Ok(m)
}

/// `GET /api/v1/projects/{name}/milestones/{id}` — fetch one Milestone.
pub async fn get_milestone(
    State(state): State<Arc<DaemonState>>,
    Path((project, id)): Path<(String, String)>,
) -> Result<Json<Milestone>, DaemonError> {
    let m = fetch_scoped_milestone(&state, &project, &id).await?;
    Ok(Json(m))
}

/// `PATCH /api/v1/projects/{name}/milestones/{id}` — update a Milestone.
///
/// Why: mirrors `patch_deliverable`'s atomicity fix (#2395 review HIGH) even
/// though Milestone has no state machine to invalidate — a read-then-upsert
/// PATCH can still silently drop a concurrent field edit. The whole
/// fetch-mutate-persist sequence runs inside one closure passed to
/// [`DeliverableManager::update_milestone_with`](crate::deliverable::DeliverableManager::update_milestone_with),
/// which holds a single write-lock guard across it.
/// What: rejects a present-but-blank `name` before touching the store (MEDIUM
/// fix). Then atomically fetches the project-scoped record, applies every
/// present field, and persists — all under the manager's single lock.
/// Test: `milestone_patch_updates_fields`, `patch_milestone_rejects_blank_name`.
pub async fn patch_milestone(
    State(state): State<Arc<DaemonState>>,
    Path((project, id)): Path<(String, String)>,
    body_in: Result<Json<PatchMilestone>, JsonRejection>,
) -> Result<Json<Milestone>, DaemonError> {
    let patch = body(body_in)?;
    reject_blank_patch_name(&patch.name, "milestone")?;

    let mgr = state.deliverable_manager().await;
    let updated = mgr
        .update_milestone_with(&id, &project, move |mut m| {
            if let Some(name) = patch.name {
                m.name = name;
            }
            if let Some(description) = patch.description {
                m.description = description;
            }
            if let Some(target_date) = patch.target_date {
                m.target_date = target_date;
            }
            if let Some(status) = patch.status {
                m.status = status;
            }
            if let Some(deliverables) = patch.deliverables {
                m.deliverables = deliverables;
            }
            m
        })
        .await
        .map_err(|e| milestone_lookup_err(e, &id))?;
    Ok(Json(updated))
}
