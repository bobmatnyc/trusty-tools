//! The last HTTP-only routes an in-repo client reaches, served on the socket
//! (#6288 client cutover, PR 1).
//!
//! Why: the client cutover moves every in-repo client to the daemon socket
//! with no TCP fallback, so a route a client calls must have a socket method.
//! Two had none: the egress guard's policy verdict (`tm hook --egress-guard`)
//! and a managed session's file-change view.
//! What: one method per route, each calling the SAME body its HTTP handler
//! calls, so the two transports cannot drift.
//!
//! | Method | HTTP route |
//! |---|---|
//! | `mpm.channels.egress_verdict` | `POST /api/v1/channels/egress-verdict` |
//! | `mpm.managed.files` | `GET /api/v1/sessions/managed/{id}/files` |
//!
//! A build without the `channels` feature serves no verdict route over HTTP
//! (404); its socket method refuses with the matching not-found code, so the
//! hook reads both the same way.
//! Test: `client_cutover_tests.rs` — `egress_verdict_parity`,
//! `managed_files_parity`, `managed_files_unknown_session_parity`.

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::RpcRouter;

use crate::core::egress_verdict::VerdictRequest;
use crate::daemon::state::DaemonState;

#[cfg(test)]
#[path = "client_cutover_tests.rs"]
mod tests;

/// Every method this module registers.
///
/// Test: `every_route_names_a_served_method`.
pub const METHODS: &[&str] = &["mpm.channels.egress_verdict", "mpm.managed.files"];

/// `mpm.managed.files` parameters: the path id, plus the `?since=` bound.
#[derive(Debug, Deserialize)]
pub struct FilesParams {
    /// The managed session id (the `{id}` path segment).
    pub id: String,
    /// Unix-seconds lower bound; absent means every record.
    #[serde(default)]
    pub since: Option<i64>,
}

/// Mount the two methods on `router`.
///
/// Why: `socket::build_router` composes families with one call each.
/// What: the verdict answers with the HTTP handler's `answer` over the same
/// snapshot read; the files view projects `files_core`'s `RouteOutcome`.
/// Test: the module's tests.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let verdict_state = Arc::clone(state);
    let files_state = Arc::clone(state);
    router
        .typed("mpm.channels.egress_verdict", move |req: VerdictRequest| {
            let state = Arc::clone(&verdict_state);
            async move { egress_verdict(&state, &req) }
        })
        .typed("mpm.managed.files", move |req: FilesParams| {
            let state = Arc::clone(&files_state);
            async move {
                crate::daemon::managed_routes::files::files_core(&state, &req.id, req.since)
                    .await
                    .into_rpc()
            }
        })
}

/// The verdict body the HTTP route serves, for the socket.
#[cfg(feature = "channels")]
fn egress_verdict(
    state: &Arc<DaemonState>,
    req: &VerdictRequest,
) -> Result<serde_json::Value, trusty_common::uds::server::RpcError> {
    let home = state.user_home();
    let read = || state.channel_policy().current_aged();
    let reply =
        crate::daemon::managed_routes::egress_verdict::answer(read, home.as_deref(), &req.home);
    serde_json::to_value(reply).map_err(|e| {
        trusty_common::uds::server::RpcError::new(
            trusty_common::uds::server::CODE_INTERNAL_ERROR,
            format!("encode the verdict: {e}"),
        )
    })
}

/// No `channels` feature: no policy snapshot, and no HTTP route either (404).
#[cfg(not(feature = "channels"))]
fn egress_verdict(
    _state: &Arc<DaemonState>,
    _req: &VerdictRequest,
) -> Result<serde_json::Value, trusty_common::uds::server::RpcError> {
    Err(trusty_common::uds::server::RpcError::new(
        crate::daemon::error::CODE_NOT_FOUND,
        "this daemon is built without the channels feature; no egress verdict is served",
    ))
}
