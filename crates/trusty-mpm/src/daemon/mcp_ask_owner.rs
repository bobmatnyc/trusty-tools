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
    options: Vec<String>,
) -> Result<Value, String> {
    use super::managed_routes::ask_owner::ask_owner_op;
    use super::services::delegation_repair_caller::RepairPeer;
    use crate::channels::ask_owner::AskOwnerRequest;
    // #9856 Q1: the kernel's peer for this socket dispatch, never an argument.
    let peer = RepairPeer::from_socket(trusty_common::uds::server::request_peer());
    let req = AskOwnerRequest {
        recipient: recipient.to_owned(),
        question: question.to_owned(),
        // #10127: the option labels; `ask_owner` validates them.
        options,
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
    _options: Vec<String>,
) -> Result<Value, String> {
    Err("ask_owner: this build has no channels feature".to_owned())
}

/// Stage a `channel_send` (#10423); the JSON result or the rule's text.
///
/// What: `managed_routes::channel_send::send_op`; the error text is the
/// rule code and its message, never the message text.
/// Test: `dispatch_channel_send_tool` (mock); the body in
/// `managed_routes/channel_send_tests.rs`.
#[cfg(feature = "channels")]
pub async fn channel_send(
    state: &Arc<DaemonState>,
    user: &str,
    text: &str,
    via_org: Option<String>,
) -> Result<Value, String> {
    use super::managed_routes::channel_send::{SendRequest, send_op};
    let req = SendRequest {
        user: user.to_owned(),
        text: text.to_owned(),
        via_org,
    };
    let staged = send_op(state, req).await.map_err(|e| e.to_string())?;
    serde_json::to_value(staged).map_err(|_| "channel_send: encode failed".to_owned())
}

/// Without the `channels` feature there is no org routing to send through.
#[cfg(not(feature = "channels"))]
pub async fn channel_send(
    _state: &Arc<DaemonState>,
    _user: &str,
    _text: &str,
    _via_org: Option<String>,
) -> Result<Value, String> {
    Err("channel_send: this build has no channels feature".to_owned())
}
