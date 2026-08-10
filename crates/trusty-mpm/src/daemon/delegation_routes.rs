//! `GET /api/v1/sessions/{id}/delegations/shared-tree-writers` — who else is
//! already writing into this working directory (#4480).
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
//! What: one read-only GET, no body, no side effects. It answers with the agent
//! names of this session's LIVE delegations that are running in the queried
//! `cwd` without a working tree of their own. The caller passes its own
//! `tool_use_id` so its own in-flight dispatch is excluded — the daemon's hook
//! and the guard's hook race on the same event, and without that exclusion the
//! very first dispatch of a session could find itself and be denied.
//!
//! It lives in its own module, merged as a sub-router, because `api.rs` is over
//! 1,100 SLOC and frozen at its line-cap budget — the same reason
//! [`super::managed_routes::reconcile`] gives.
//! Test: `shared_tree_writers_route_*` below.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{Json, Router, extract::Path, extract::Query, extract::State, routing::get};
use serde::{Deserialize, Serialize};

use crate::core::session::SessionId;
use crate::daemon::error::DaemonError;
use crate::daemon::state::DaemonState;

/// Query string of [`shared_tree_writers_route`].
///
/// Why: `cwd` is required because the guard asks about ONE directory — the one
/// its dispatch would land in — and a session may legitimately have delegations
/// running in several. `exclude_tool_use_id` is the caller's own dispatch; see
/// the module doc for why omitting it would make the answer race-dependent.
/// What: `cwd` (required) and `exclude_tool_use_id` (optional).
/// Test: `shared_tree_writers_route_excludes_the_callers_own_dispatch`.
#[derive(Debug, Deserialize)]
pub struct SharedTreeWritersQuery {
    /// Working directory to ask about.
    pub cwd: PathBuf,
    /// The asking dispatch's own `tool_use_id`, excluded from the answer.
    #[serde(default)]
    pub exclude_tool_use_id: Option<String>,
}

/// Response of [`shared_tree_writers_route`].
///
/// Why: the guard needs a count to decide and names to explain — a deny that
/// cannot say which agent it is protecting reads as arbitrary and gets retried.
/// What: `agents` holds one entry per live unisolated writer, deduplicated with
/// a count so two concurrent `rust-engineer`s render as one row rather than a
/// repeated name. `total` is the number of delegations, not of distinct names.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SharedTreeWritersResponse {
    /// Distinct agent names, each with how many of its delegations are live.
    pub agents: Vec<SharedTreeWriter>,
    /// Total live unisolated writers, across all names.
    pub total: usize,
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
/// What: one GET on a literal-suffixed path under `/sessions/{id}`.
/// Test: `shared_tree_writers_route_reports_live_unisolated_writers`.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new().route(
        "/api/v1/sessions/{id}/delegations/shared-tree-writers",
        get(shared_tree_writers_route),
    )
}

/// `GET /api/v1/sessions/{id}/delegations/shared-tree-writers` (#4480).
///
/// Why: see the module doc.
/// What: parses the session id, delegates to
/// [`DaemonState::live_shared_tree_writers`], and folds the agent names into a
/// deduplicated count. A malformed session id is a 400; an unknown session is
/// an EMPTY answer, not a 404 — a session the daemon has no record of has no
/// delegations, and a 404 would read to the guard as an error rather than as
/// "nobody else is here".
/// Test: `shared_tree_writers_route_reports_live_unisolated_writers`,
/// `shared_tree_writers_route_excludes_the_callers_own_dispatch`,
/// `shared_tree_writers_route_rejects_a_malformed_session_id`.
pub async fn shared_tree_writers_route(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Query(q): Query<SharedTreeWritersQuery>,
) -> Result<Json<SharedTreeWritersResponse>, DaemonError> {
    let session = uuid::Uuid::parse_str(&id)
        .map(SessionId)
        .map_err(|_| DaemonError::InvalidRequest(format!("malformed session id: {id}")))?;
    let names = state.live_shared_tree_writers(session, &q.cwd, q.exclude_tool_use_id.as_deref());

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
    }))
}

#[cfg(test)]
#[path = "delegation_routes_tests.rs"]
mod tests;
