//! `POST /api/v1/sessions/{id}/delegations/shared-tree-dispatch` — who else is
//! already writing into this working directory, answered while claiming it
//! (#4480, made atomic by #5324).
//!
//! Why: `tm hook --pm-guard` decides, per `Agent`/`Task` dispatch, whether the
//! PM is about to put a SECOND file-mutating subagent into a working directory
//! that already has one. The daemon is the only process that knows: its
//! delegation tracker is where a dispatch's liveness is resolved from real
//! `SubagentStop` signals rather than guessed from a timer. The guard is a
//! short-lived hook process with no such state, so it has to ask.
//!
//! Why a route rather than a shared file: a second, hook-side ledger would be an
//! independent implementation of delegation tracking — the same records, the
//! same correlation keys, the same lifecycle — and the two would drift. The
//! daemon already receives every dispatch through the `matcher: "*"` PreToolUse
//! hook; this exposes what it already knows.
//!
//! Why POST and not GET (#5324): a read-only query cannot close the window it
//! opens. Asking "is anyone writing here?" and acting on the answer are two
//! steps, and two dispatches issued in ONE PM turn — the framework's own
//! documented pattern for parallel work — can both ask before either is
//! recorded, both see an empty set, and both be admitted. So the answer and the
//! record are one operation: this route claims the directory for the asking
//! dispatch in the same critical section that produced its answer. It mutates,
//! so it is a POST.
//!
//! What the claim IS: the delegation record the tracker would have written
//! anyway. The route hands the posted payload to
//! [`crate::daemon::services::delegation_tracker::observe`] — the tracker's own
//! `PreToolUse` observer — so the record is byte-identical to the one the
//! daemon's `matcher: "*"` hook produces for the same dispatch, and whichever
//! of the two hooks arrives second is a no-op (that observer is idempotent on
//! `tool_use_id`). No second kind of state, no new expiry, and nothing new to
//! clean up: the claim ends when the delegation ends.
//!
//! The caller passes its own `tool_use_id` so its own in-flight dispatch is
//! excluded — the daemon's hook and the guard's hook race on the same event, and
//! without that exclusion the very first dispatch of a session could find itself
//! and be denied.
//!
//! **Version skew fails open in both directions.** A new `tm hook` against an
//! old daemon POSTs to a path that does not exist; an old `tm hook` against a
//! new daemon GETs one that no longer does. Both get a 404, which the guard
//! reads as "nobody else is here" — the behaviour that shipped before #4480.
//!
//! It lives in its own module, merged as a sub-router, because `api.rs` is over
//! 1,100 SLOC and frozen at its line-cap budget — the same reason
//! [`super::managed_routes::reconcile`] gives.
//! Test: `shared_tree_dispatch_route_*` below.
//!
//! [`super::managed_routes::reconcile`]: crate::daemon::managed_routes::reconcile

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{Json, Router, extract::Path, extract::State, routing::post};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::agent::is_subagent_dispatch_tool;
use crate::core::dispatch_isolation::{
    blocked_by_shared_tree, dispatch_agent, dispatch_isolation, isolation_separates_working_tree,
};
use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::daemon::error::DaemonError;
use crate::daemon::state::DaemonState;
use crate::daemon::state::sessions::SharedTreeQuestion;

/// Request body of [`shared_tree_dispatch_route`].
///
/// Why: the route both answers and records, and the recording is done by the
/// delegation tracker's own observer — so the body is simply the payload that
/// observer already consumes, built by `tm hook`'s single
/// `build_hook_payload`. Re-describing the same dispatch in a bespoke schema
/// here would be a second construction of one record, which is exactly the
/// drift the daemon-side route exists to avoid.
/// What: `payload` carries `cwd`, `tool`, `input` (with `subagent_type` and
/// `isolation`), `tool_use_id`, and `transcript_path`. `cwd` is read from the
/// payload rather than taken separately so the directory that is compared is
/// the same one that is recorded.
/// Test: `shared_tree_dispatch_route_reserves_the_tree_on_an_empty_answer`.
#[derive(Debug, Deserialize)]
pub struct SharedTreeDispatchRequest {
    /// The `PreToolUse` hook payload, in the daemon's own forwarded shape.
    pub payload: Value,
}

/// Response of [`shared_tree_dispatch_route`].
///
/// Why: the guard needs a count to decide and names to explain — a deny that
/// cannot say which agent it is protecting reads as arbitrary and gets retried.
/// What: `agents` holds one entry per live unisolated writer, deduplicated with
/// a count so two concurrent `rust-engineer`s render as one row rather than a
/// repeated name. `total` is the number of delegations, not of distinct names.
/// `claimed` reports whether this call WROTE anything, and what that write was
/// depends on which route answered. On
/// [`shared_tree_dispatch_route`] it means the directory was reserved for an
/// unisolated dispatch. On [`granted_worktree_route`] it means the granted
/// isolation was recorded — an isolated dispatch reserves nothing, so there is
/// no directory to take. The two share the field because both answer the same
/// question the caller asks of it: did the daemon act, or only look? A single
/// name for "the daemon wrote" keeps an older guard that ignores the field
/// behaving identically on both.
///
/// Since #5769 that field is no longer purely diagnostic on the granted route:
/// the guard reads it to tell "the checkout is free" from "the daemon declined
/// to record", which are otherwise the same empty answer. A daemon too old to
/// send it reads as not-written, which is the conservative direction — see
/// `pm_guard_dispatch::warn_on_unrecorded_grant`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SharedTreeWritersResponse {
    /// Distinct agent names, each with how many of its delegations are live.
    pub agents: Vec<SharedTreeWriter>,
    /// Total live unisolated writers, across all names.
    pub total: usize,
    /// Whether this call claimed the directory for the asking dispatch.
    #[serde(default)]
    pub claimed: bool,
}

/// One agent name with its live unisolated delegation count.
#[derive(Debug, Serialize, Deserialize)]
pub struct SharedTreeWriter {
    /// The agent's name.
    pub agent: String,
    /// How many of that agent's delegations are live in the queried directory.
    pub count: usize,
}

/// The delegation sub-router (#4480).
///
/// Why: see the module doc — `api.rs` is grandfathered at a frozen line-cap
/// budget, so a new route is registered here and merged rather than appended
/// there.
/// What: one POST on a literal-suffixed path under `/sessions/{id}`.
/// Test: `shared_tree_dispatch_route_reports_live_unisolated_writers`.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new()
        .route(
            "/api/v1/sessions/{id}/delegations/shared-tree-dispatch",
            post(shared_tree_dispatch_route),
        )
        .route(
            "/api/v1/sessions/{id}/delegations/granted-worktree",
            post(granted_worktree_route),
        )
}

/// `POST /api/v1/sessions/{id}/delegations/granted-worktree` (#5769).
///
/// Why: [`shared_tree_dispatch_route`] cannot record a granted dispatch, and
/// that is deliberate rather than an oversight — it re-derives eligibility with
/// [`blocked_by_shared_tree`], which an `isolation: "worktree"` input makes
/// false, so its record closure never runs (pinned by
/// `shared_tree_dispatch_route_does_not_reserve_a_read_only_agent`). But a
/// dispatch the guard just rewrote from unisolated to isolated is exactly the
/// one whose record needs correcting: the tracker's `matcher: "*"` hook observes
/// the ORIGINAL payload and writes `isolation: None`, so every granted writer
/// stays named as a shared-checkout writer and ADR-0048 decision 10 denies
/// a `git merge` or `git rebase` there on a phantom. This route is the second
/// half the grant needs.
///
/// What: the same scan-and-claim as its sibling, with two differences. The
/// record is [`crate::daemon::services::delegation_tracker::record_granted_isolation`]
/// — an upsert that OVERWRITES `isolation` on an existing record, so the guard
/// and the tracker converge whichever arrives first. And eligibility asks
/// whether the dispatch declares an isolating mode, the exact inverse of the
/// sibling's question, because that is what a grant looks like on the wire.
///
/// It re-derives that from the payload rather than trusting the caller, for the
/// same reason the sibling does: whether a dispatch may hold a directory is this
/// daemon's policy call. A payload with no `cwd`, no `tool_use_id`, a
/// non-dispatch `tool`, or no isolating `isolation` answers the query and
/// records nothing.
/// Test: `a_grant_and_the_tracker_converge_in_either_order` covers both the
/// correct-an-existing-record and the record-arrives-first halves;
/// `a_grant_and_the_tracker_race_without_losing_the_isolation`,
/// `granted_worktree_route_records_nothing_without_isolation_or_a_tool_use_id`,
/// `record_granted_isolation_refuses_a_non_separating_mode`,
/// `granted_worktree_route_reports_a_live_writer_without_claiming`.
pub async fn granted_worktree_route(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(req): Json<SharedTreeDispatchRequest>,
) -> Result<Json<SharedTreeWritersResponse>, DaemonError> {
    // #6288: the body is shared with `mpm.delegation.granted_worktree`.
    Ok(Json(granted_worktree_op(&state, &id, req)?))
}

/// [`granted_worktree_route`]'s body, with no transport in it (#6288 slice 5).
///
/// # Errors
///
/// [`DaemonError::InvalidRequest`] when `id` is not a UUID.
///
/// Test: `parity_delegation_granted_worktree_agrees_across_transports`.
pub fn granted_worktree_op(
    state: &Arc<DaemonState>,
    id: &str,
    req: SharedTreeDispatchRequest,
) -> Result<SharedTreeWritersResponse, DaemonError> {
    let session = uuid::Uuid::parse_str(id)
        .map(SessionId)
        .map_err(|_| DaemonError::InvalidRequest(format!("malformed session id: {id}")))?;

    let payload = &req.payload;
    let Some(cwd) = str_field(payload, "cwd").map(PathBuf::from) else {
        return Ok(SharedTreeWritersResponse::default());
    };
    let exclude = str_field(payload, "tool_use_id");
    let input = payload.get("input");
    let eligible = exclude.is_some()
        && str_field(payload, "tool").is_some_and(is_subagent_dispatch_tool)
        && isolation_separates_working_tree(dispatch_isolation(input));

    // #6556: always `Dispatch`, and NOT derived from `eligible`. Every caller of
    // this route is deciding whether to admit a dispatch —
    // `evaluate_granted_worktree` denies on a non-empty answer — and `eligible`
    // is false here whenever the grant cannot be recorded, which is exactly when
    // the deny still has to see an occupant.
    let (names, claimed) = state.claim_shared_tree_dispatch(
        &cwd,
        exclude,
        eligible,
        SharedTreeQuestion::Dispatch,
        |s| {
            crate::daemon::services::delegation_tracker::record_granted_isolation(
                s, session, payload,
            );
        },
    );
    Ok(writers_response(&names, claimed))
}

/// `POST /api/v1/sessions/{id}/delegations/shared-tree-dispatch` (#4480, #5324).
///
/// Why: see the module doc.
/// What: parses the session id, then hands the whole scan-and-claim to
/// [`DaemonState::claim_shared_tree_dispatch`], which holds one mutex across
/// both halves. The claim is taken only when the answer is empty AND this
/// dispatch would itself [`blocked_by_shared_tree`] — the daemon re-derives
/// that from the payload rather than trusting the caller to have checked, so a
/// read-only or isolated dispatch can never occupy a directory. A malformed
/// session id is a 400; an unknown session is an EMPTY answer, not a 404 — a
/// session the daemon has no record of has no delegations, and a 404 would read
/// to the guard as an error rather than as "nobody else is here".
///
/// A payload with no `cwd` is answered empty and claims nothing: there is no
/// directory to compare against, and inventing one would be the only way this
/// route could produce a false deny.
/// Test: `shared_tree_dispatch_route_reports_live_unisolated_writers`,
/// `shared_tree_dispatch_route_excludes_the_callers_own_dispatch`,
/// `shared_tree_dispatch_route_rejects_a_malformed_session_id`,
/// `shared_tree_dispatch_route_denies_the_second_claim`,
/// `shared_tree_dispatch_route_does_not_reserve_a_read_only_agent`,
/// `shared_tree_dispatch_route_is_empty_without_a_cwd`.
pub async fn shared_tree_dispatch_route(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(req): Json<SharedTreeDispatchRequest>,
) -> Result<Json<SharedTreeWritersResponse>, DaemonError> {
    // #6288: the body is shared with `mpm.delegation.shared_tree_dispatch`.
    Ok(Json(shared_tree_dispatch_op(&state, &id, req)?))
}

/// [`shared_tree_dispatch_route`]'s body, with no transport in it (#6288
/// slice 5).
///
/// # Errors
///
/// [`DaemonError::InvalidRequest`] when `id` is not a UUID.
///
/// Test: `parity_delegation_shared_tree_dispatch_agrees_across_transports`,
/// `rpc_delegation_rejects_a_malformed_session_id`.
pub fn shared_tree_dispatch_op(
    state: &Arc<DaemonState>,
    id: &str,
    req: SharedTreeDispatchRequest,
) -> Result<SharedTreeWritersResponse, DaemonError> {
    let session = uuid::Uuid::parse_str(id)
        .map(SessionId)
        .map_err(|_| DaemonError::InvalidRequest(format!("malformed session id: {id}")))?;

    let payload = &req.payload;
    let Some(cwd) = str_field(payload, "cwd").map(PathBuf::from) else {
        return Ok(SharedTreeWritersResponse::default());
    };
    let exclude = str_field(payload, "tool_use_id");
    let input = payload.get("input");
    // Re-derived here, never taken on trust: the caller says which agent it is
    // dispatching, but whether that dispatch may occupy a directory is this
    // daemon's policy call, shared with the guard through one classifier.
    // ADR-0056: `blocked_by_shared_tree`, not `shares_the_callers_tree` — the
    // guard's admission question, so the two halves stay one policy.
    let is_dispatch = str_field(payload, "tool").is_some_and(is_subagent_dispatch_tool);
    let eligible = is_dispatch
        && dispatch_agent(input)
            .is_some_and(|agent| blocked_by_shared_tree(agent, dispatch_isolation(input)));
    // #6556: this route serves BOTH questions. `tm hook` posts here for a
    // `Bash` payload too — that is the ADR-0049 documents-only commit and the
    // ADR-0048 HEAD-move query — and those must not hear a record #6556
    // reconciled, or the commit they exist to unblock stays denied. The tool
    // name is the discriminator, not `eligible`: an isolated or read-only
    // dispatch is still a dispatch.
    let question = if is_dispatch {
        SharedTreeQuestion::Dispatch
    } else {
        SharedTreeQuestion::HeadWrite
    };

    let (names, claimed) =
        state.claim_shared_tree_dispatch(&cwd, exclude, eligible, question, |s| {
            crate::daemon::services::delegation_tracker::observe(
                s,
                session,
                HookEvent::PreToolUse,
                payload,
            );
        });

    Ok(writers_response(&names, claimed))
}

/// Fold a list of live writer names into the wire response.
///
/// Why: both routes answer the same question in the same shape, and the
/// deduplication ("two `rust-engineer`s render as one row with a count") is the
/// part a second copy would drift on.
fn writers_response(names: &[String], claimed: bool) -> SharedTreeWritersResponse {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for name in names {
        *counts.entry(name.as_str()).or_default() += 1;
    }
    SharedTreeWritersResponse {
        agents: counts
            .into_iter()
            .map(|(agent, count)| SharedTreeWriter {
                agent: agent.to_string(),
                count,
            })
            .collect(),
        total: names.len(),
        claimed,
    }
}

/// Read a non-empty string field from the forwarded hook payload.
fn str_field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
#[path = "delegation_routes_tests.rs"]
mod tests;
