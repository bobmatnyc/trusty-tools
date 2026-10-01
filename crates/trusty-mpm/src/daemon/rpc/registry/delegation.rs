//! The delegation verbs as RPC methods (#6288 slice 5, step 2a).
//!
//! Why registration only: `delegation_routes` had SLOC headroom, so its
//! `*_op` bodies stayed beside their handlers.
//!
//! What does NOT change: both dispatch routes answer and CLAIM in one critical
//! section (`DaemonState::claim_shared_tree_dispatch` holds one mutex across
//! both halves, #5324), and both re-derive eligibility from the payload rather
//! than trusting the caller. Neither moved, so a socket caller cannot occupy a
//! directory an HTTP caller could not have.
//!
//! Step 2a adds the read-only listing and both repair verbs. #8531: the
//! repair's caller is the kernel's peer pid for this connection
//! (`trusty_common::uds::server::request_peer`), walked to its session by
//! `delegation_repair_caller::establish_caller`. No param names the caller; a
//! `caller_session` an older client still sends is ignored.
//!
//! Test: the `parity_delegation_*` and `rpc_delegation_*` cases in
//! `super::tests`.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use trusty_common::uds::server::{RpcError, RpcRouter, request_peer};

use crate::daemon::delegation_routes as dg;
use crate::daemon::services::delegation_records::DelegationListing;
use crate::daemon::services::delegation_repair::RepairOutcome;
use crate::daemon::services::delegation_repair_caller::RepairPeer;
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

/// `mpm.delegation.list` parameters: the `?cwd=` query field.
#[derive(Debug, Deserialize)]
pub struct ListParams {
    /// The directory whose records to list.
    pub cwd: PathBuf,
}

/// `mpm.delegation.repair` parameters: the path's agent id and the body's
/// `force` flag. #8531: no caller field — the caller is the kernel's peer.
#[derive(Debug, Deserialize)]
pub struct RepairParams {
    /// The agent id from the URL path.
    pub agent_id: String,
    /// End the record even when the owning session is undeterminable.
    #[serde(default)]
    pub force: bool,
}

/// `mpm.delegation.repair_by_id` parameters: [`RepairParams`] keyed by the
/// delegation id instead of the agent id.
#[derive(Debug, Deserialize)]
pub struct RepairByIdParams {
    /// The delegation id from the URL path.
    pub delegation_id: String,
    /// End the record even when the owning session is undeterminable.
    #[serde(default)]
    pub force: bool,
}

/// Mount the delegation methods.
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
    let r = r.typed::<DispatchParams, dg::SharedTreeWritersResponse, _, _>(
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
    );
    register_step_2a(r, state)
}

/// Mount the listing and both repair verbs (#6288 step 2a).
fn register_step_2a(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let held = Arc::clone(state);
    let r = router.typed::<ListParams, DelegationListing, _, _>("mpm.delegation.list", move |p| {
        let s = Arc::clone(&held);
        async move { Ok(dg::list_delegations_op(&s, p.cwd)) }
    });

    let held = Arc::clone(state);
    let r = r.typed::<RepairParams, RepairOutcome, _, _>("mpm.delegation.repair", move |p| {
        let s = Arc::clone(&held);
        async move {
            // #8531: the peer and its accept instant, set by the server for this task.
            let peer = RepairPeer::from_socket(request_peer());
            Ok::<_, RpcError>(dg::repair_delegation_op(s, p.agent_id, p.force, peer).await)
        }
    });

    let held = Arc::clone(state);
    r.typed::<RepairByIdParams, RepairOutcome, _, _>("mpm.delegation.repair_by_id", move |p| {
        let s = Arc::clone(&held);
        async move {
            // #8531: the peer and its accept instant, set by the server for this task.
            let peer = RepairPeer::from_socket(request_peer());
            dg::repair_delegation_by_id_op(s, &p.delegation_id, p.force, peer)
                .await
                .map_err(Into::into)
        }
    })
}
