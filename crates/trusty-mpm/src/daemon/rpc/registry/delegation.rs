//! The two delegation-query verbs as RPC methods (#6288 slice 5).
//!
//! Why registration only: `delegation_routes` had SLOC headroom, so its two
//! `*_op` bodies stayed beside their handlers.
//!
//! What does NOT change: both routes answer and CLAIM in one critical section
//! (`DaemonState::claim_shared_tree_dispatch` holds one mutex across both
//! halves, #5324), and both re-derive eligibility from the payload rather than
//! trusting the caller. Neither moved, so a socket caller cannot occupy a
//! directory an HTTP caller could not have.
//!
//! Test: the `parity_delegation_*` cases in `super::tests`.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use trusty_common::uds::server::RpcRouter;

use crate::daemon::delegation_routes as dg;
use crate::daemon::state::DaemonState;

/// Parameters for both delegation methods: the session id the HTTP route
/// carries in its path, plus the forwarded hook payload.
#[derive(Debug, Deserialize)]
pub struct DispatchParams {
    /// The session id from the URL path.
    pub id: String,
    /// The `PreToolUse` hook payload, in the daemon's own forwarded shape.
    pub payload: Value,
}

/// Mount the two delegation methods.
///
/// Test: `rpc_router_registers_every_documented_method`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let held = Arc::clone(state);
    let r = router.typed::<DispatchParams, dg::SharedTreeWritersResponse, _, _>(
        "mpm.delegation.shared_tree_dispatch",
        move |p| {
            let s = Arc::clone(&held);
            async move {
                dg::shared_tree_dispatch_op(
                    &s,
                    &p.id,
                    dg::SharedTreeDispatchRequest { payload: p.payload },
                )
                .map_err(Into::into)
            }
        },
    );

    let held = Arc::clone(state);
    r.typed::<DispatchParams, dg::SharedTreeWritersResponse, _, _>(
        "mpm.delegation.granted_worktree",
        move |p| {
            let s = Arc::clone(&held);
            async move {
                dg::granted_worktree_op(
                    &s,
                    &p.id,
                    dg::SharedTreeDispatchRequest { payload: p.payload },
                )
                .map_err(Into::into)
            }
        },
    )
}
