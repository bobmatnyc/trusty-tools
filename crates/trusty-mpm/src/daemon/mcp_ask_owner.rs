//! The `ask_owner` MCP tool's daemon body (#9856, #8433 P4).
//!
//! Why: `mcp_backend.rs` sits near the SLOC cap; the caller-proof wiring
//! lives here. The asker is the process on the other end of the daemon
//! socket, as the kernel reports it (#9856 Q1); no tool argument names it.
//! What: [`ask_owner`] reads the socket dispatch's peer
//! (`trusty_common::uds::server::request_peer`; `None` over HTTP `/rpc`) and
//! calls `managed_routes::ask_owner::ask_owner_op`. A build without the
//! `channels` feature refuses.
//! Test: `mcp_ask_owner_over_http_rpc_is_refused_as_unproven`
//! (`managed_routes/ask_owner_tests.rs`).

use std::sync::Arc;

use serde_json::Value;

use super::state::DaemonState;

/// Ask the owner for the socket caller; the JSON result or the rule's text.
#[cfg(feature = "channels")]
pub async fn ask_owner(
    state: &Arc<DaemonState>,
    recipient: &str,
    question: &str,
) -> Result<Value, String> {
    use super::managed_routes::ask_owner::ask_owner_op;
    use super::services::delegation_repair_caller::RepairPeer;
    use crate::channels::ask_owner::AskOwnerRequest;
    // #9856 Q1: the kernel's peer for this socket dispatch, never an argument.
    let peer = RepairPeer::from_socket(trusty_common::uds::server::request_peer());
    let req = AskOwnerRequest {
        recipient: recipient.to_owned(),
        question: question.to_owned(),
    };
    let asked = ask_owner_op(state, peer, req)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_value(asked).map_err(|_| "ask_owner: encode failed".to_owned())
}

/// Without the `channels` feature there is no route policy to ask under.
#[cfg(not(feature = "channels"))]
pub async fn ask_owner(
    _state: &Arc<DaemonState>,
    _recipient: &str,
    _question: &str,
) -> Result<Value, String> {
    Err("ask_owner: this build has no channels feature".to_owned())
}
