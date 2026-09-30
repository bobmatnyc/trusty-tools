//! The HTTP-only routes the sandboxed `tm` CLI reaches, served on the socket
//! (#6288 step 1).
//!
//! Why: a sandboxed session has no loopback TCP (Q2), and ADR-0032 makes the
//! socket the only same-host transport. `tm build-lease` posts its decision
//! log, `tm session adopt-worktree` posts an adoption, and the retired
//! builder-slot routes still answer `410` — four routes slices 2-5 left on
//! HTTP only.
//! What: one method per route, each calling the SAME body its HTTP handler
//! calls, so the two transports cannot drift.
//!
//! | Method | HTTP route |
//! |---|---|
//! | `mpm.build_lease.decision` | `POST /api/v1/build-lease/decisions` |
//! | `mpm.builder_slot.claim` | `POST /api/v1/sessions/{id}/delegations/builder-slot` |
//! | `mpm.builder_slot.list` | `GET /api/v1/builder-slots` |
//! | `mpm.managed.adopt_worktree` | `POST /api/v1/sessions/managed/adopt-worktree` |
//!
//! Auth is the socket's own: `serve_until` checks the peer uid on every
//! connection before a byte is read, which is stronger than the HTTP origin
//! guard these routes carry (see `rpc::core`'s module doc).
//! Test: `rpc_build_lease_decision_is_acknowledged`,
//! `rpc_builder_slot_methods_answer_gone`,
//! `rpc_adopt_worktree_refuses_like_http`.

use std::sync::Arc;

use serde_json::Value;
use trusty_common::uds::server::{RpcError, RpcRouter};

use crate::daemon::build_lease_routes::log_decision_op;
use crate::daemon::builder_slot_routes::RETIRED_MESSAGE;
use crate::daemon::error::CODE_GONE;
use crate::daemon::managed_routes::adopt_worktree::{AdoptWorktreeRequest, adopt_worktree_core};
use crate::daemon::state::DaemonState;

#[cfg(test)]
#[path = "cli_socket_tests.rs"]
mod tests;

/// Every method this module registers.
///
/// Test: `every_route_names_a_served_method`.
pub const METHODS: &[&str] = &[
    "mpm.build_lease.decision",
    "mpm.builder_slot.claim",
    "mpm.builder_slot.list",
    "mpm.managed.adopt_worktree",
];

/// Mount the four methods on `router`.
///
/// Why: `socket::build_router` composes families with one call each.
/// What: the decision log acknowledges with `{}` (HTTP answers `204`); both
/// builder-slot names refuse with `CODE_GONE` and [`RETIRED_MESSAGE`], the
/// text the HTTP `410` body carries; adopt-worktree projects the shared
/// `RouteOutcome` exactly as the managed family does.
/// Test: the module's tests.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let adopt_state = Arc::clone(state);
    router
        .typed("mpm.build_lease.decision", |body: Value| async move {
            log_decision_op(&body);
            Ok(serde_json::json!({}))
        })
        .typed("mpm.builder_slot.claim", |_: Value| async move {
            Err::<Value, _>(RpcError::new(CODE_GONE, RETIRED_MESSAGE))
        })
        .typed("mpm.builder_slot.list", |_: Value| async move {
            Err::<Value, _>(RpcError::new(CODE_GONE, RETIRED_MESSAGE))
        })
        .typed(
            "mpm.managed.adopt_worktree",
            move |req: AdoptWorktreeRequest| {
                let state = Arc::clone(&adopt_state);
                async move { adopt_worktree_core(&state, req).await.into_rpc() }
            },
        )
}
