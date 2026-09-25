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

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
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

/// Payload key that asks the shared-tree route who holds a tree, counting the
/// asking session's own agents (#8161).
///
/// Why: #6797 scopes a HEAD-write answer to OTHER sessions, which is right for a
/// shared main checkout and wrong for a parked linked worktree, where the live
/// agent a consolidating `reset --keep` would clobber is usually the asking
/// PM's own. A `true` value answers with `HeadWrite { caller: None }`.
/// Test: `shared_tree_route_tree_holders_counts_the_callers_own_agent`.
pub const TREE_HOLDERS_MARKER: &str = "tree_holders";

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
    /// #8257: each blocking record — id, owner, age, clearing command — for a
    /// dispatch the answer denies. Empty for a HEAD-write query and for a
    /// claim; absent from an older daemon, which the guard tolerates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<crate::daemon::services::delegation_records::DelegationRecordView>,
    /// #8161: echoes [`TREE_HOLDERS_MARKER`] when this answer counted the
    /// asking session's own agents. A daemon older than #8161 answers the same
    /// route without it, scoped to other sessions, so the guard treats its
    /// absence on a tree-holders query as no answer.
    #[serde(default)]
    pub tree_holders: bool,
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
        // #7602: the operator repair verb. Keyed on the agent id, not a session
        // — the whole case is a record whose session the daemon has lost.
        .route(
            "/api/v1/delegations/{agent_id}/repair",
            post(repair_delegation_as_route),
        )
        // #8257: the same repair addressed by delegation id — the only address
        // a record matched by agent type ever has — and the read-only listing.
        .route(
            "/api/v1/delegations/by-id/{delegation_id}/repair",
            post(repair_delegation_by_id_route),
        )
        .route("/api/v1/delegations", get(list_delegations_route))
}

/// Query of [`list_delegations_route`].
#[derive(Debug, Deserialize)]
pub struct ListDelegationsQuery {
    /// The directory whose records to list.
    pub cwd: PathBuf,
}

/// `GET /api/v1/delegations?cwd=<dir>` (#8257).
///
/// Why: the read-only listing `tm repair delegation --list` prints. Before it,
/// the only reads of the map were POSTs that CLAIM a directory.
/// What: [`crate::daemon::services::delegation_records::list_for_dir`] as JSON.
/// Test: `list_route_names_the_blocking_record_8257`.
pub async fn list_delegations_route(
    State(state): State<Arc<DaemonState>>,
    Query(q): Query<ListDelegationsQuery>,
) -> Json<crate::daemon::services::delegation_records::DelegationListing> {
    let records = crate::daemon::services::delegation_records::list_for_dir(&state, &q.cwd);
    Json(
        crate::daemon::services::delegation_records::DelegationListing {
            cwd: q.cwd,
            records,
        },
    )
}

/// `POST /api/v1/delegations/by-id/{delegation_id}/repair` (#8257).
///
/// Why: see [`crate::daemon::services::delegation_repair::repair_delegation_by_id`].
/// What: a malformed id is a 400; otherwise the outcome, always 200, exactly
/// as [`repair_delegation_route`] answers.
/// Test: `repair_by_id_route_ends_a_record_with_no_agent_id_8257`.
pub async fn repair_delegation_by_id_route(
    State(state): State<Arc<DaemonState>>,
    Path(delegation_id): Path<String>,
    headers: axum::http::HeaderMap,
    body: Option<Json<crate::daemon::services::delegation_repair::RepairDelegationRequest>>,
) -> Result<Json<crate::daemon::services::delegation_repair::RepairOutcome>, DaemonError> {
    let id = uuid::Uuid::parse_str(&delegation_id)
        .map(crate::core::agent::DelegationId)
        .map_err(|_| {
            DaemonError::InvalidRequest(format!("malformed delegation id: {delegation_id}"))
        })?;
    let (force, caller) = force_and_caller(&headers, body);
    Ok(Json(
        repair_off_worker(move || {
            crate::daemon::services::delegation_repair::repair_delegation_by_id(
                &state, id, force, &caller,
            )
        })
        .await,
    ))
}

/// Run one repair on tokio's blocking pool (#8257 critic R6).
///
/// Why: the repair's OS probe runs a system-wide `lsof` with no timeout, plus
/// `git worktree list` and `sysinfo`. On a runtime worker a hung `lsof` pins
/// that worker, and a starved runtime makes `tm hook` deny dispatches that
/// should pass. Same shape as `agent_worktree_reap::reap_and_record`.
/// What: the repair's own outcome; a join failure (a panic or a cancelled
/// task) is a `Refused` naming the failure, never a success.
/// Test: `a_repair_task_that_panics_is_a_refusal_8257`.
async fn repair_off_worker(
    repair: impl FnOnce() -> crate::daemon::services::delegation_repair::RepairOutcome + Send + 'static,
) -> crate::daemon::services::delegation_repair::RepairOutcome {
    tokio::task::spawn_blocking(repair)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("delegation: repair task failed before answering: {e} (#8257)");
            crate::daemon::services::delegation_repair::RepairOutcome::Refused {
                reason: format!(
                    "the repair task failed before it answered ({e}), so its result is \
                     unknown — `tm repair delegation --list` shows which records are still \
                     live (#8257)"
                ),
            }
        })
}

/// The body's `force` flag and the caller-session header of a repair (#8257).
fn force_and_caller(
    headers: &axum::http::HeaderMap,
    body: Option<Json<crate::daemon::services::delegation_repair::RepairDelegationRequest>>,
) -> (
    bool,
    crate::daemon::services::delegation_repair::RepairCaller,
) {
    use crate::daemon::services::delegation_repair::{CALLER_SESSION_HEADER, RepairCaller};
    let force = body.is_some_and(|Json(b)| b.force);
    let raw = headers.get(CALLER_SESSION_HEADER).map(|v| v.to_str());
    let caller = match raw {
        Some(Err(_)) => RepairCaller::Unestablished(format!(
            "the {CALLER_SESSION_HEADER} header is not valid text"
        )),
        Some(Ok(s)) => RepairCaller::from_request(Some(s)),
        None => RepairCaller::from_request(None),
    };
    (force, caller)
}

/// `POST /api/v1/delegations/{agent_id}/repair` (#7602).
///
/// Why: a delegation stuck non-terminal has no other way out — `SubagentStop`
/// is the only signal that ends one, and by construction it never arrived. The
/// daemon is the only process holding the delegation map, so the decision and
/// the write both have to happen here; `tm repair delegation <agent-id>` is the
/// client.
/// What: hands the agent id and the caller's `force` assertion to
/// [`crate::daemon::services::delegation_repair::repair_delegation`], which owns
/// every refusal arm, and returns its outcome as JSON. Always 200 — a refusal is
/// an ANSWER, and a client that read it as a transport error would retry it.
/// Test: `repair_route_ends_a_stuck_record_7602`,
/// `repair_route_refuses_a_live_owner_7602`.
pub async fn repair_delegation_route(
    State(state): State<Arc<DaemonState>>,
    Path(agent_id): Path<String>,
    body: Option<Json<crate::daemon::services::delegation_repair::RepairDelegationRequest>>,
) -> Json<crate::daemon::services::delegation_repair::RepairOutcome> {
    // #8257: no headers, so no caller session — the owner path never opens.
    repair_delegation_as_route(
        State(state),
        Path(agent_id),
        axum::http::HeaderMap::new(),
        body,
    )
    .await
}

/// [`repair_delegation_route`] reading the caller-session header (#8257).
///
/// Why: the owner ruling lets the owning session clear its own live record,
/// so the router registers this form; the header-less one keeps its public
/// signature and simply never establishes a caller.
/// Test: `repair_route_lets_the_owning_session_clear_its_record_8257`,
/// `repair_route_ignores_an_owner_id_the_caller_supplies_8257`.
pub async fn repair_delegation_as_route(
    State(state): State<Arc<DaemonState>>,
    Path(agent_id): Path<String>,
    headers: axum::http::HeaderMap,
    body: Option<Json<crate::daemon::services::delegation_repair::RepairDelegationRequest>>,
) -> Json<crate::daemon::services::delegation_repair::RepairOutcome> {
    let (force, caller) = force_and_caller(&headers, body);
    Json(
        repair_off_worker(move || {
            crate::daemon::services::delegation_repair::repair_delegation_as(
                &state, &agent_id, force, &caller,
            )
        })
        .await,
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
///
/// A non-empty answer to an eligible grant is a deny, and it releases the
/// denied dispatch's record exactly as the sibling route does (#7487).
/// Test: `a_dispatch_the_grant_path_denies_records_no_claim_7487`,
/// `a_grant_deny_keeps_the_running_occupant_counted_7487`;
/// `a_grant_and_the_tracker_converge_in_either_order` covers both the
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
    // #7487 (recurrence 2026-09-13): `eligible && !claimed` is the grant path's
    // deny — `evaluate_granted_worktree` denies on any non-empty answer. The
    // dispatch never runs, so the record the tracker writes from the ORIGINAL
    // unisolated payload must not occupy the checkout. Same release as the
    // sibling route; the guard's rewrite leaves `tool_use_id` unchanged.
    if eligible && !claimed {
        crate::daemon::services::delegation_tracker::release_denied_dispatch(
            state, session, payload,
        );
    }
    let mut response = writers_response(&names, claimed);
    response.records = blocking_records(state, &cwd, exclude, &names);
    Ok(response)
}

/// The records behind a non-empty dispatch answer, for the deny text (#8257).
///
/// Why: the names alone left a denied PM nothing to act on. Read after the
/// claim rather than inside it: the text is advisory, the deny is decided by
/// the names, and a record that changed in between is at worst named stale.
fn blocking_records(
    state: &DaemonState,
    cwd: &std::path::Path,
    exclude: Option<&str>,
    names: &[String],
) -> Vec<crate::daemon::services::delegation_records::DelegationRecordView> {
    if names.is_empty() {
        return Vec::new();
    }
    let now = chrono::Utc::now();
    state
        .shared_tree_records(cwd, exclude, true, None)
        .iter()
        .map(|d| {
            crate::daemon::services::delegation_records::DelegationRecordView::of(
                state, d, now, true,
            )
        })
        .collect()
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
    // #6797: a HEAD write carries the asking session, so its own agents are not
    // reported to it as foreign live writers. The id is the path's, which this
    // route already parsed and which `tm hook --pm-guard` fills from the
    // payload's `session_id` — the same id space a delegation's `session` field
    // holds, so the comparison is exact rather than heuristic.
    // #8161: a HEAD move into a LINKED worktree asks who holds that tree, and
    // the asking session's own agent is exactly who a `reset --keep` would
    // clobber — so that query marks itself and hears every session.
    let tree_holders = payload.get(TREE_HOLDERS_MARKER).and_then(Value::as_bool) == Some(true);
    let question = if is_dispatch {
        SharedTreeQuestion::Dispatch
    } else {
        SharedTreeQuestion::HeadWrite {
            caller: (!tree_holders).then_some(session),
        }
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

    // #7487: `eligible && !claimed` IS the guard's deny — `claimed` is
    // `eligible && occupants.is_empty()`, and `evaluate_shared_tree_dispatch`
    // denies on exactly that non-empty answer. The dispatch will not run, so the
    // record the tracker's own `matcher: "*"` hook writes for it (before or
    // after this call) must not go on occupying the tree.
    if eligible && !claimed {
        crate::daemon::services::delegation_tracker::release_denied_dispatch(
            state, session, payload,
        );
    }

    let mut response = writers_response(&names, claimed);
    response.tree_holders = tree_holders;
    // #8257: only a dispatch's deny names records; a HEAD-write answer is
    // scoped differently and keeps its own text.
    if question == SharedTreeQuestion::Dispatch {
        response.records = blocking_records(state, &cwd, exclude, &names);
    }
    Ok(response)
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
        records: Vec::new(),
        tree_holders: false,
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
