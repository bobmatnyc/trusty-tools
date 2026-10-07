//! HTTP handlers for `tm sessions rebind` (#9313).
//!
//! Why: after a tmux server replacement a record can name a pane and server
//! that no longer exist while its Claude runs on in a new pane. The automatic
//! pass rebinds only on an unambiguous match on a replaced server; the verb
//! lets an operator rebind a renamed or same-server-relaunched session.
//! What: [`router`] registers `POST …/managed/{id}/rebind` (optional
//! `?tmux=<session>`) and the fleet-wide `POST …/managed/rebind`. Both report
//! per record `rebound`, `current`, `no_match`, `ambiguous` or `error`; the
//! fleet-wide one also `skipped` for a record whose server was not replaced.
//! A rebind writes the record only; no tmux session is created, killed or
//! signalled.
//! Test: `pane_rebind_tests.rs` (`the_rebind_routes_report_each_outcome`).

use std::sync::Arc;

use axum::{
    Router,
    extract::{Path as AxumPath, Query, State},
    response::IntoResponse,
    routing::post,
};
use serde::{Deserialize, Serialize};

use super::summary::parse_id;
use crate::daemon::rpc::managed::outcome::RouteOutcome;
use crate::daemon::state::DaemonState;
use crate::session_manager::pane_rebind::RebindOutcome;
use crate::session_manager::{ManagedError, ManagedSessionState, SessionManager};

/// Build the rebind sub-router, merged into the daemon router by `api.rs`.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new()
        .route("/api/v1/sessions/managed/{id}/rebind", post(rebind_route))
        .route("/api/v1/sessions/managed/rebind", post(rebind_all_route))
}

/// `?tmux=<session>`: the live tmux session to bind to, when it is not the
/// record's own `tmux_name` (a renamed session).
#[derive(Debug, Default, Deserialize)]
pub struct RebindQuery {
    /// The live tmux session name to bind the record to.
    #[serde(default)]
    pub tmux: Option<String>,
}

/// One record's rebind result.
#[derive(Debug, Serialize)]
pub struct RebindResponse {
    /// Managed session id.
    pub id: String,
    /// The record's tmux name after the call.
    pub name: String,
    /// `rebound`, `current`, `no_match`, `ambiguous`, `skipped` or `error`.
    pub outcome: String,
    /// Why the record was left as it is; empty for `rebound` and for a
    /// `current` record that is active.
    pub detail: String,
    /// The pane the record now names, for `rebound`.
    pub pane_id: Option<String>,
    /// The tmux server the record now names, for `rebound`.
    pub tmux_server: Option<String>,
}

/// The fleet-wide body: one [`RebindResponse`] per active or stopped record.
#[derive(Debug, Serialize)]
pub struct RebindAllResponse {
    /// Per-record results, in store order.
    pub results: Vec<RebindResponse>,
}

/// Shape one record's outcome for the wire.
fn response(id: String, name: &str, outcome: &RebindOutcome) -> RebindResponse {
    let (detail, pane_id, tmux_server, name) = match outcome {
        RebindOutcome::Rebound {
            tmux_name,
            pane_id,
            tmux_server,
        } => (
            String::new(),
            Some(pane_id.clone()),
            Some(tmux_server.clone()),
            tmux_name.clone(),
        ),
        RebindOutcome::Current { stopped: false } => (String::new(), None, None, name.to_owned()),
        // #9313: a stopped record on its live pane needs a resume, not a rebind.
        RebindOutcome::Current { stopped: true } => (
            format!("its runtime is stopped; `tm sessions resume {id}` starts it"),
            None,
            None,
            name.to_owned(),
        ),
        RebindOutcome::NoMatch(why)
        | RebindOutcome::Ambiguous(why)
        | RebindOutcome::Skipped(why) => (why.clone(), None, None, name.to_owned()),
    };
    RebindResponse {
        id,
        name,
        outcome: outcome.label().to_owned(),
        detail,
        pane_id,
        tmux_server,
    }
}

/// POST /api/v1/sessions/managed/{id}/rebind — a thin wrapper over `rebind_core`.
pub async fn rebind_route(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
    Query(query): Query<RebindQuery>,
) -> impl IntoResponse {
    rebind_core(&state, &id_str, query.tmux.as_deref()).await
}

/// Re-bind one record to its live pane (#9313), served over the socket as
/// `mpm.managed.rebind`.
///
/// Why: `tm sessions rebind <id-or-name> [--tmux <session>]`.
/// What: [`SessionManager::rebind_session`]; 404 for an unknown id, 409 for
/// a record that cannot be rebound or a `tmux` name tm cannot target, 503
/// when tmux cannot be read, 500 for a store failure, and 200 with a
/// [`RebindResponse`] otherwise. Every error leaves the record unchanged.
/// Test: `the_rebind_routes_report_each_outcome`.
pub(crate) async fn rebind_core(
    state: &Arc<DaemonState>,
    id_str: &str,
    tmux: Option<&str>,
) -> RouteOutcome {
    let id = match parse_id(id_str) {
        Ok(id) => id,
        Err((code, msg)) => return RouteOutcome::text(code.as_u16(), msg),
    };
    let mgr = state.session_manager().await;
    match mgr.rebind_session(&id, tmux).await {
        Ok(outcome) => {
            let name = mgr.get(&id).await.map(|r| r.tmux_name).unwrap_or_default();
            RouteOutcome::ok(&response(id.to_string(), &name, &outcome))
        }
        Err(ManagedError::SessionNotFound(_)) => {
            RouteOutcome::text(404, format!("session {id_str} not found"))
        }
        Err(ManagedError::InvalidState(_, why)) => RouteOutcome::text(409, why),
        Err(e @ ManagedError::TmuxUnavailable(_)) => {
            RouteOutcome::text(503, format!("{e}; the record was not changed"))
        }
        Err(e) => RouteOutcome::text(500, format!("{e}; the record was not changed")),
    }
}

/// POST /api/v1/sessions/managed/rebind — a thin wrapper over `rebind_all_core`.
pub async fn rebind_all_route(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    rebind_all_core(&state).await
}

/// Re-bind every active or stopped record (#9313), served over the socket
/// as `mpm.managed.rebind_all`.
///
/// Why: `tm sessions rebind --all` after a tmux server replacement.
/// What: [`rebind_each`] over the manager; always 200.
/// Test: `the_rebind_routes_report_each_outcome`,
/// `rebind_all_leaves_a_same_server_fleet_unchanged`.
pub(crate) async fn rebind_all_core(state: &Arc<DaemonState>) -> RouteOutcome {
    let mgr = state.session_manager().await;
    RouteOutcome::ok(&RebindAllResponse {
        results: rebind_each(&mgr).await,
    })
}

/// [`SessionManager::rebind_if_server_replaced`] for each active or stopped
/// record, one at a time, so `--all` moves only records on a replaced tmux
/// server (#9313). A failure is that record's `error` row and never stops the
/// rest.
async fn rebind_each(mgr: &SessionManager) -> Vec<RebindResponse> {
    let mut results = Vec::new();
    for record in mgr.list().await {
        if !matches!(
            record.state,
            ManagedSessionState::Active | ManagedSessionState::Stopped
        ) {
            continue;
        }
        let id = record.id.to_string();
        let row = match mgr.rebind_if_server_replaced(&record.id).await {
            Ok(outcome) => response(id, &record.tmux_name, &outcome),
            Err(e) => RebindResponse {
                id,
                name: record.tmux_name.clone(),
                outcome: "error".to_owned(),
                detail: format!("{e}; the record was not changed"),
                pane_id: None,
                tmux_server: None,
            },
        };
        results.push(row);
    }
    results
}
