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
    dispatch_agent, dispatch_isolation, shares_the_callers_tree,
};
use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::daemon::error::DaemonError;
use crate::daemon::state::DaemonState;

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
/// `claimed` reports whether this call took the directory; it is diagnostic —
/// the guard decides on `agents` alone, so an older guard that ignores the field
/// behaves identically.
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
    Router::new().route(
        "/api/v1/sessions/{id}/delegations/shared-tree-dispatch",
        post(shared_tree_dispatch_route),
    )
}

/// `POST /api/v1/sessions/{id}/delegations/shared-tree-dispatch` (#4480, #5324).
///
/// Why: see the module doc.
/// What: parses the session id, then hands the whole scan-and-claim to
/// [`DaemonState::claim_shared_tree_dispatch`], which holds one mutex across
/// both halves. The claim is taken only when the answer is empty AND this
/// dispatch would itself [`shares_the_callers_tree`] — the daemon re-derives
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
    let session = uuid::Uuid::parse_str(&id)
        .map(SessionId)
        .map_err(|_| DaemonError::InvalidRequest(format!("malformed session id: {id}")))?;

    let payload = &req.payload;
    let Some(cwd) = str_field(payload, "cwd").map(PathBuf::from) else {
        return Ok(Json(SharedTreeWritersResponse::default()));
    };
    let exclude = str_field(payload, "tool_use_id");
    let input = payload.get("input");
    // Re-derived here, never taken on trust: the caller says which agent it is
    // dispatching, but whether that dispatch may occupy a directory is this
    // daemon's policy call, shared with the guard through one classifier.
    let eligible = str_field(payload, "tool").is_some_and(is_subagent_dispatch_tool)
        && dispatch_agent(input)
            .is_some_and(|agent| shares_the_callers_tree(agent, dispatch_isolation(input)));

    let (names, claimed) =
        state.claim_shared_tree_dispatch(session, &cwd, exclude, eligible, |s| {
            crate::daemon::services::delegation_tracker::observe(
                s,
                session,
                HookEvent::PreToolUse,
                payload,
            );
        });

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for name in &names {
        *counts.entry(name.clone()).or_default() += 1;
    }
    Ok(Json(SharedTreeWritersResponse {
        agents: counts
            .into_iter()
            .map(|(agent, count)| SharedTreeWriter { agent, count })
            .collect(),
        total: names.len(),
        claimed,
    }))
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
