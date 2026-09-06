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
//! **The daemon resolves the cap, never the caller.** [`resolve_max_concurrent`]
//! reads `~/.trusty-mpm/config.toml` here, in the process that does the
//! counting. A `tm` older or newer than the daemon would otherwise argue for a
//! number the live leases were not admitted under, and the guard's whole value
//! is that one authority counts.
//!
//! **Eligibility is re-derived here too, never taken on trust.** The caller says
//! which agent it is dispatching; whether that agent claims a builder slot is
//! this daemon's policy call, shared with the guard through the one
//! [`agent_is_builder`] classifier. A non-builder payload therefore claims
//! nothing even if a caller posts it to this route.
//!
//! It lives in its own module, merged as a sub-router, for the same reason
//! [`super::delegation_routes`] does: `api.rs` is grandfathered at a frozen
//! line-cap budget.
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
/// What: `holders` is one entry per live builder lease, newest-running last;
/// `cap` is the machine's effective `builders.max_concurrent`; `claimed` says
/// whether THIS call took a slot. `claimed = false` with an under-cap
/// `holders` list means the daemon classified the dispatch as a non-builder —
/// see [`builder_slot_op`].
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BuilderSlotResponse {
    /// Builders already holding a slot, excluding this dispatch's own record.
    pub holders: Vec<BuilderHolder>,
    /// The machine's effective builder cap.
    pub cap: u32,
    /// Whether this call claimed a slot.
    #[serde(default)]
    pub claimed: bool,
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
/// uncounted.
/// Test: `builder_slot_route_claims_a_free_slot`,
/// `builder_slot_route_denies_over_the_cap_and_names_the_holders`,
/// `builder_slot_route_claims_nothing_for_a_non_builder`,
/// `builder_slot_route_claims_nothing_without_a_tool_use_id`.
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

    let (holders, claimed) = state.claim_builder_slot(cap, exclude, eligible, |s| {
        crate::daemon::services::delegation_tracker::observe(
            s,
            session,
            HookEvent::PreToolUse,
            payload,
        );
    });
    Ok(BuilderSlotResponse {
        holders,
        cap,
        claimed,
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
