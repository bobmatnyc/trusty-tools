//! The `sessions_chat` MCP tool's daemon body (#9979, sub-issue of #6288).
//!
//! Why: `mcp_backend.rs` sits near the SLOC cap. The tool must answer exactly
//! as `mpm.sessions.chat` does, so it calls the same body.
//! What: [`sessions_chat`] decodes the tool arguments as the chat request and
//! runs `coordinator_chat_op`, the body `POST /api/v1/sessions/chat` and
//! `mpm.sessions.chat` share.
//! Test: `mcp_dispatch_sessions_chat_matches_mpm_sessions_chat` and
//! `mcp_dispatch_sessions_chat_without_a_model_is_a_503_tool_error`
//! (`rpc/mcp_tests.rs`).

use std::sync::Arc;

use serde_json::Value;

use super::api::coordinator_routes::{CoordinatorChatRequest, coordinator_chat_op};
use super::state::DaemonState;

/// Run one chat turn; the response as JSON, or an error text with its status.
///
/// What: an error reads `sessions_chat failed (<status>): <error>` — `503`
/// when no model is configured — so a caller can tell "not configured" from
/// a failed turn.
pub async fn sessions_chat(state: &Arc<DaemonState>, turn: Value) -> Result<Value, String> {
    let body: CoordinatorChatRequest = serde_json::from_value(turn)
        .map_err(|e| format!("sessions_chat: invalid arguments: {e}"))?;
    let reply = coordinator_chat_op(state, body)
        .await
        .map_err(|e| format!("sessions_chat failed ({}): {e}", e.status().as_u16()))?;
    serde_json::to_value(reply).map_err(|_| "sessions_chat: encode failed".to_owned())
}
