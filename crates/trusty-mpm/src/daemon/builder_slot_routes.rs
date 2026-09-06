//! `POST /api/v1/sessions/{id}/delegations/builder-slot` — claim one of this
//! machine's builder slots, answered while claiming it (#6892).
//!
//! Why: `tm hook --pm-guard` decides, per `Agent` dispatch, whether the machine
//! has room for another builder. Only the daemon can answer: it holds every
//! session's delegations, and the cap the answer is measured against is a
//! property of the host, not of the asking session. A hook-side ledger would be
//! a second implementation of delegation tracking and would drift.
//!
//! Why POST and not GET: a read-only query cannot close the window it opens.
//! Asking "is a slot free?" and acting on the answer are two steps, and two
//! dispatches issued in ONE PM turn can both ask before either is recorded. So
//! the answer and the record are one operation — see
//! [`DaemonState::claim_builder_slot`].
//!
//! **The daemon resolves the cap, never the caller.**
//! [`resolve_max_concurrent`](crate::core::builders::resolve_max_concurrent)
//! reads `~/.trusty-mpm/config.toml` here, in the process that does the
//! counting. A `tm` older or newer than the daemon would otherwise argue for a
//! number the live leases were not admitted under, and the guard's whole value
//! is that one authority counts.
//!
//! **Eligibility is re-derived here too, never taken on trust.** The caller says
//! which agent it is dispatching; whether that agent claims a builder slot is
//! this daemon's policy call, shared with the guard through the one
//! [`agent_is_builder`](crate::core::dispatch_isolation::agent_is_builder)
//! classifier. A non-builder payload therefore claims nothing even if a caller
//! posts it to this route.
//!
//! It lives in its own module, merged as a sub-router, for the same reason
//! [`delegation_routes`](crate::daemon::delegation_routes) does: `api.rs` is
//! grandfathered at a frozen line-cap budget.
//! Test: the `#[cfg(test)]` suite below.

use std::sync::Arc;

use axum::{Json, Router, extract::Path, extract::State, routing::get, routing::post};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::agent::is_subagent_dispatch_tool;
use crate::core::builders::resolve_max_concurrent;
use crate::core::dispatch_isolation::{agent_is_builder, dispatch_agent};
use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::daemon::error::DaemonError;
use crate::daemon::state::{BuilderHolder, BuilderSlotCensus, DaemonState};

/// Request body of [`builder_slot_route`].
///
/// Why: the route both answers and records, and the recording is done by the
/// delegation tracker's own observer — so the body is simply the payload that
/// observer already consumes, built by `tm hook`'s single `build_hook_payload`.
/// Re-describing the dispatch in a bespoke schema here would be a second
/// construction of one record.
/// What: `payload` carries `cwd`, `tool`, `input` (with `subagent_type`), and
/// `tool_use_id`.
/// Test: `builder_slot_route_claims_a_free_slot`.
#[derive(Debug, Deserialize)]
pub struct BuilderSlotRequest {
    /// The `PreToolUse` hook payload, in the daemon's own forwarded shape.
    pub payload: Value,
}

/// Response of [`builder_slot_route`].
///
/// Why: the guard needs a verdict to act on and names to explain it — a deny
/// that cannot say which builders are running reads as arbitrary and gets
/// retried identically.
/// What: `holders` is one entry per live builder lease, longest-running first;
/// `cap` is the machine's effective `builders.max_concurrent`; `claimed` says
/// whether THIS call took a slot.
///
/// **`ineligible` exists because `claimed: false` had two meanings (#6892 critic
/// round).** It meant both "the machine is full" and "this payload could never
/// have claimed anything", and the hook read the second as the first — denying
/// an idle machine with a message naming zero holders. The two are now separate
/// fields, so a caller cannot conflate them by reading only one.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BuilderSlotResponse {
    /// Builders already holding a slot, excluding this dispatch's own record.
    pub holders: Vec<BuilderHolder>,
    /// The machine's effective builder cap.
    pub cap: u32,
    /// Whether this call claimed a slot.
    #[serde(default)]
    pub claimed: bool,
    /// Whether the daemon judged this payload unable to claim at all — a
    /// non-builder agent, a non-dispatch tool, or no `tool_use_id`. Never a
    /// statement about the machine, and never `true` alongside `claimed`.
    #[serde(default)]
    pub ineligible: bool,
}

/// The builder-slot sub-router (#6892).
///
/// Why: see the module doc — `api.rs` is grandfathered at a frozen line-cap
/// budget, so a new route is registered here and merged rather than appended
/// there.
/// What: the claiming POST under `/sessions/{id}`, and the read-only census GET
/// `tm doctor` reads.
/// Test: `builder_slot_route_claims_a_free_slot`,
/// `builder_slot_census_route_reports_holders_and_the_cap`.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new()
        .route(
            "/api/v1/sessions/{id}/delegations/builder-slot",
            post(builder_slot_route),
        )
        .route("/api/v1/builder-slots", get(builder_slot_census_route))
}

/// `POST /api/v1/sessions/{id}/delegations/builder-slot` (#6892).
///
/// Why: see the module doc.
/// What: parses the session id, resolves the machine's cap, and hands the
/// scan-and-claim to [`builder_slot_op`]. A malformed session id is a 400; an
/// unknown session is not an error — a session the daemon has no record of has
/// no delegations, and a 404 would read to the guard as "the daemon could not
/// answer", which this guard denies on.
/// Test: `builder_slot_route_claims_a_free_slot`,
/// `builder_slot_route_rejects_a_malformed_session_id`.
pub async fn builder_slot_route(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(req): Json<BuilderSlotRequest>,
) -> Result<Json<BuilderSlotResponse>, DaemonError> {
    // The cap is resolved HERE, in the counting process — see the module doc.
    Ok(Json(builder_slot_op(
        &state,
        &id,
        req,
        resolve_max_concurrent(),
    )?))
}

/// [`builder_slot_route`]'s body, with the cap supplied and no transport in it.
///
/// Why: the cap is the one input that would otherwise make this untestable —
/// [`resolve_max_concurrent`] reads the operator's real `~/.trusty-mpm`, so a
/// test driving the route would depend on the machine it runs on. Taking it as
/// a parameter keeps the route's own logic hermetic and leaves exactly one line
/// (the caller above) asserting which loader answers.
///
/// # Errors
///
/// [`DaemonError::InvalidRequest`] when `id` is not a UUID.
///
/// What: re-derives whether this dispatch claims a slot from the payload —
/// a dispatch tool, a `subagent_type`, and [`agent_is_builder`] — then runs the
/// atomic claim. A payload with no `tool_use_id` is still ANSWERED but claims
/// nothing: without that key the record could not be excluded from its own
/// count, and a dispatch that denied itself would be worse than one that went
/// uncounted. That answer reports `ineligible: true` rather than leaving the
/// caller to read `claimed: false` as a full machine (#6892 critic round).
///
/// A refusal that IS eligible releases the record the guard's preceding
/// shared-tree or worktree-grant claim wrote for this same dispatch — see
/// [`DaemonState::claim_builder_slot`].
/// Test: `builder_slot_route_claims_a_free_slot`,
/// `builder_slot_route_denies_over_the_cap_and_names_the_holders`,
/// `builder_slot_route_claims_nothing_for_a_non_builder`,
/// `builder_slot_route_claims_nothing_without_a_tool_use_id`,
/// `a_denied_builder_releases_the_record_the_dispatch_just_claimed`,
/// `a_payload_with_no_tool_use_id_is_ineligible_not_full`.
pub fn builder_slot_op(
    state: &Arc<DaemonState>,
    id: &str,
    req: BuilderSlotRequest,
    cap: u32,
) -> Result<BuilderSlotResponse, DaemonError> {
    let session = uuid::Uuid::parse_str(id)
        .map(SessionId)
        .map_err(|_| DaemonError::InvalidRequest(format!("malformed session id: {id}")))?;

    let payload = &req.payload;
    let exclude = str_field(payload, "tool_use_id");
    let input = payload.get("input");
    let eligible = exclude.is_some()
        && str_field(payload, "tool").is_some_and(is_subagent_dispatch_tool)
        && dispatch_agent(input).is_some_and(agent_is_builder);

    let (holders, claimed) = state.claim_builder_slot(
        cap,
        exclude,
        eligible,
        |s| {
            crate::daemon::services::delegation_tracker::observe(
                s,
                session,
                HookEvent::PreToolUse,
                payload,
            );
        },
        // #6892 critic round: a refusal is a DENY, and the guard's preceding
        // shared-tree or worktree-grant claim already recorded this dispatch as
        // Running. Nothing downstream will ever close that record, because a
        // `PreToolUse` deny means the tool never runs.
        |s| {
            s.release_denied_builder_dispatch(session, exclude);
        },
    );
    Ok(BuilderSlotResponse {
        holders,
        cap,
        claimed,
        ineligible: !eligible,
    })
}

/// `GET /api/v1/builder-slots` — the read-only census `tm doctor` renders.
///
/// Why: the deny message names holders at the moment of a dispatch; an operator
/// asking "what is holding my machine" has no dispatch to hang that on. This is
/// that question, and it takes no slot: a diagnostic that claimed one would
/// change the thing it reports.
/// What: [`DaemonState::builder_slot_census`] under the machine's resolved cap.
/// Session-free by construction — the cap is machine-wide, so there is no id in
/// the path to scope it by.
/// Test: `builder_slot_census_route_reports_holders_and_the_cap`.
pub async fn builder_slot_census_route(
    State(state): State<Arc<DaemonState>>,
) -> Json<BuilderSlotCensus> {
    Json(state.builder_slot_census(resolve_max_concurrent()))
}

/// Read a non-empty string field from the forwarded hook payload.
fn str_field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent::{Delegation, DelegationStatus, ModelTier};
    use crate::core::paths::FrameworkPaths;

    /// A hermetic state plus one session id, mirroring `delegation_routes_tests`.
    fn hermetic() -> (Arc<DaemonState>, tempfile::TempDir, SessionId) {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = FrameworkPaths::under(dir.path());
        let state = Arc::new(DaemonState::with_paths(&paths));
        (state, dir, SessionId(uuid::Uuid::new_v4()))
    }

    /// One dispatch in the daemon's forwarded hook shape — what the guard POSTs.
    fn dispatch(agent: &str, tool_use_id: Option<&str>) -> BuilderSlotRequest {
        let mut payload = serde_json::json!({
            "cwd": "/repo",
            "tool": "Agent",
            "input": {"subagent_type": agent, "description": "go"},
        });
        if let Some(id) = tool_use_id {
            payload["tool_use_id"] = Value::String(id.to_string());
        }
        BuilderSlotRequest { payload }
    }

    /// Insert one running builder owned by `session`.
    fn insert_builder(state: &DaemonState, session: SessionId, agent: &str) {
        let mut d = Delegation::new(session, None, agent, ModelTier::Sonnet, "build");
        d.status = DelegationStatus::Running;
        d.started_at = Some(chrono::Utc::now());
        state.upsert_delegation(d);
    }

    #[test]
    fn builder_slot_route_claims_a_free_slot() {
        let (state, _dir, session) = hermetic();
        let body = builder_slot_op(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_A")),
            2,
        )
        .expect("route succeeds");
        assert!(body.claimed, "an idle machine admits the first builder");
        assert_eq!(body.cap, 2);
        assert!(body.holders.is_empty());
        // The claim IS a delegation record, so the next caller can see it.
        assert_eq!(state.builder_slot_holders(None).len(), 1);
    }

    /// Criterion 1 through the wire shape: the deny names the actual holders,
    /// with agent, session and elapsed time — not a generic string.
    #[test]
    fn builder_slot_route_denies_over_the_cap_and_names_the_holders() {
        let (state, _dir, session) = hermetic();
        insert_builder(&state, session, "rust-engineer");
        insert_builder(&state, session, "local-ops");

        let body = builder_slot_op(
            &state,
            &session.0.to_string(),
            dispatch("python-engineer", Some("toolu_C")),
            2,
        )
        .expect("route succeeds");
        assert!(!body.claimed);
        assert_eq!(body.holders.len(), 2);
        let names: Vec<&str> = body.holders.iter().map(|h| h.agent.as_str()).collect();
        assert!(names.contains(&"rust-engineer"), "{names:?}");
        assert!(names.contains(&"local-ops"), "{names:?}");
        assert!(body.holders.iter().all(|h| h.session == session));
    }

    /// Criterion 7 at the route: the daemon re-derives eligibility, so a
    /// research dispatch posted here claims nothing even on an idle machine.
    #[test]
    fn builder_slot_route_claims_nothing_for_a_non_builder() {
        let (state, _dir, session) = hermetic();
        for agent in ["research", "ticketing", "documentation", "version-control"] {
            let body = builder_slot_op(
                &state,
                &session.0.to_string(),
                dispatch(agent, Some("toolu_X")),
                4,
            )
            .expect("route succeeds");
            assert!(!body.claimed, "{agent} must not take a builder slot");
        }
        assert!(state.builder_slot_holders(None).is_empty());
    }

    #[test]
    fn builder_slot_route_claims_nothing_without_a_tool_use_id() {
        let (state, _dir, session) = hermetic();
        let body = builder_slot_op(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", None),
            4,
        )
        .expect("route succeeds");
        assert!(!body.claimed);
        assert!(state.builder_slot_holders(None).is_empty());
    }

    #[test]
    fn builder_slot_route_rejects_a_malformed_session_id() {
        let (state, _dir, _session) = hermetic();
        let err = builder_slot_op(
            &state,
            "not-a-uuid",
            dispatch("rust-engineer", Some("toolu_A")),
            2,
        )
        .expect_err("a malformed id is a 400");
        assert!(matches!(err, DaemonError::InvalidRequest(_)));
    }

    /// What the shared-tree claim or the worktree grant wrote for THIS dispatch
    /// before the cap was asked.
    ///
    /// Why both shapes: the cap is asked at three ALLOW exits and each is
    /// preceded by a different recorder — the grant's `record_granted_isolation`
    /// upsert on the `Rewrite` arm, and `observe` on the `InPlace` arm and the
    /// fall-through. Both key the record by `tool_use_id`, so the release is
    /// exit-independent, and driving both is what proves that rather than
    /// assuming it.
    fn record_the_preceding_claim_wrote(
        state: &DaemonState,
        session: SessionId,
        payload: &Value,
        granted: bool,
    ) {
        if granted {
            let mut isolated = payload.clone();
            isolated["input"]["isolation"] = Value::String("worktree".to_string());
            crate::daemon::services::delegation_tracker::record_granted_isolation(
                state, session, &isolated,
            );
        } else {
            crate::daemon::services::delegation_tracker::observe(
                state,
                session,
                HookEvent::PreToolUse,
                payload,
            );
        }
    }

    /// The #6892 critic round, HIGH. The call that runs BEFORE the cap at every
    /// one of the guard's three ALLOW exits records a `Running` delegation for
    /// this dispatch on its empty answer — that is how both the #4480 claim and
    /// the ADR-0048 grant work. The cap then denies, and nothing sends the
    /// `SubagentStop` that would close the record, because a `PreToolUse` deny
    /// means the tool never runs. Without a compensating release the record is
    /// live for the six hours of `RUNNING_STALE_AFTER_SECS`, so the "queue and
    /// re-issue" the deny message offers is itself refused — by #4480, with a
    /// message that never mentions the cap.
    ///
    /// Fails before this round: the record stays `Running`, `shared_tree_occupants`
    /// names it, and the retry's claim is refused.
    #[test]
    fn a_denied_builder_releases_the_record_the_dispatch_just_claimed() {
        for granted in [false, true] {
            let (state, _dir, session) = hermetic();
            // The machine's two slots, held by agents with no cwd of their own —
            // so the only thing that can occupy `/repo` is the denied dispatch.
            insert_builder(&state, session, "rust-engineer");
            insert_builder(&state, session, "local-ops");

            let req = dispatch("python-engineer", Some("toolu_DENIED"));
            record_the_preceding_claim_wrote(&state, session, &req.payload, granted);
            let recorded = state.find_delegation(session, |d| {
                d.tool_use_id.as_deref() == Some("toolu_DENIED")
            });
            assert!(
                recorded.is_some(),
                "premise (granted={granted}): the preceding claim records this dispatch"
            );

            let body =
                builder_slot_op(&state, &session.0.to_string(), req, 2).expect("route succeeds");
            assert!(!body.claimed, "granted={granted}: the machine is full");

            // The record must be terminal, not deleted: keeping it is what makes
            // the tracker's own `matcher: "*"` hook a no-op if it lands after the
            // deny (`on_dispatch_locked` returns early on a known `tool_use_id`).
            let id = state
                .find_delegation(session, |d| {
                    d.tool_use_id.as_deref() == Some("toolu_DENIED")
                })
                .expect("the record is retained, not removed");
            let released = state
                .all_delegations()
                .into_iter()
                .find(|d| d.id == id)
                .expect("delegation");
            assert!(
                !released.status.is_live(),
                "granted={granted}: a denied dispatch must not keep a live record: {:?}",
                released.status
            );
            assert!(released.ended_at.is_some(), "granted={granted}");

            // And the remedy the deny offers must actually work: nothing occupies
            // the checkout, so the retry is ADMITTED.
            assert!(
                state
                    .shared_tree_occupants(std::path::Path::new("/repo"), Some("toolu_RETRY"))
                    .is_empty(),
                "granted={granted}: the denied dispatch still occupies the checkout"
            );
            let (occupants, claimed) = state.claim_shared_tree_dispatch(
                std::path::Path::new("/repo"),
                Some("toolu_RETRY"),
                true,
                crate::daemon::state::sessions::SharedTreeQuestion::Dispatch,
                |_| {},
            );
            assert!(
                claimed,
                "granted={granted}: the re-issued dispatch must be admitted, \
                 but #4480 named {occupants:?}"
            );
        }
    }

    /// The release is keyed to the DENIED dispatch and nothing else. A sibling
    /// builder already holding a slot must survive another dispatch's refusal.
    #[test]
    fn a_denied_builder_releases_only_its_own_record() {
        let (state, _dir, session) = hermetic();
        insert_builder(&state, session, "rust-engineer");
        insert_builder(&state, session, "local-ops");

        let req = dispatch("python-engineer", Some("toolu_DENIED"));
        record_the_preceding_claim_wrote(&state, session, &req.payload, false);
        let body = builder_slot_op(&state, &session.0.to_string(), req, 2).expect("route succeeds");
        assert!(!body.claimed);

        assert_eq!(
            state.builder_slot_holders(None).len(),
            2,
            "the two holders keep their slots"
        );
    }

    /// An ADMITTED claim releases nothing — the record it just wrote IS the
    /// lease.
    #[test]
    fn an_admitted_builder_keeps_its_record() {
        let (state, _dir, session) = hermetic();
        let body = builder_slot_op(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_OK")),
            2,
        )
        .expect("route succeeds");
        assert!(body.claimed);
        assert_eq!(state.builder_slot_holders(None).len(), 1);
    }

    /// The #6892 critic round, MEDIUM. A payload with no `tool_use_id` cannot be
    /// claimed OR excluded from its own count, and answering it `claimed: false`
    /// made the hook read an idle machine as full and deny naming zero holders.
    /// The route now says so in its own field rather than overloading `claimed`.
    #[test]
    fn a_payload_with_no_tool_use_id_is_ineligible_not_full() {
        let (state, _dir, session) = hermetic();
        let body = builder_slot_op(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", None),
            4,
        )
        .expect("route succeeds");
        assert!(!body.claimed);
        assert!(
            body.ineligible,
            "an unclaimable payload must not read as a full machine"
        );
        assert!(state.builder_slot_holders(None).is_empty());
    }

    /// The counterpart: a real refusal is NOT ineligible, or the hook would read
    /// a full machine as a payload defect and allow the build.
    #[test]
    fn a_full_machine_is_not_reported_ineligible() {
        let (state, _dir, session) = hermetic();
        insert_builder(&state, session, "rust-engineer");
        let body = builder_slot_op(
            &state,
            &session.0.to_string(),
            dispatch("python-engineer", Some("toolu_C")),
            1,
        )
        .expect("route succeeds");
        assert!(!body.claimed);
        assert!(!body.ineligible);
    }

    #[tokio::test]
    async fn builder_slot_census_route_reports_holders_and_the_cap() {
        let (state, _dir, session) = hermetic();
        insert_builder(&state, session, "rust-engineer");
        let Json(census) = builder_slot_census_route(State(Arc::clone(&state))).await;
        assert_eq!(census.holders.len(), 1);
        assert_eq!(census.holders[0].agent, "rust-engineer");
        assert!(census.expired.is_empty());
        // The cap comes from the host config; only its presence is assertable
        // here, since the number depends on the machine running the test.
        assert!(census.cap <= 64);
    }
}
