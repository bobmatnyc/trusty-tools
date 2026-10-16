//! `POST /api/console/sessions/chat` — the mpm chat turn behind the console's
//! chat page (#9979, owner ruling D96: "Move /web to a console route").
//!
//! Why: the daemon's `/web` page posts to the daemon's own HTTP port, which
//! #6288 retires. The console serves the page instead and reaches the daemon
//! the way every other session route does, over the stdio MCP bridge.
//! What: [`MpmChat`] owns a DEDICATED `McpServiceHandle` spawned as
//! `tm serve --stdio --allow-chat`; [`chat_handler`] sends the page's body to
//! the `sessions_chat` tool on it and returns the tool's JSON unchanged. Any
//! failure is a [`ChatTurnError`] and a `503` with a text body.
//! Test: the `tests` module below.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::Value;

use crate::mcp_handle::{McpHandleError, McpServiceHandle, unwrap_mcp_content};
use crate::server::AppState;

/// The route the chat page posts to; the router mounts this same constant.
pub const MPM_CHAT_API_PATH: &str = "/api/console/sessions/chat";

/// The mpm MCP tool one chat turn calls (S1, #9979).
pub const SESSIONS_CHAT_TOOL: &str = "sessions_chat";

/// The binary the dedicated chat handle spawns.
const MPM_CHAT_BINARY: &str = "tm";

/// The arguments the dedicated chat handle spawns the binary with.
///
/// Why: `--allow-chat` is what makes `tm serve --stdio` serve `sessions_chat`
/// at all; the shared `"trusty-mpm"` handle omits it, so the tool cannot be
/// called there by mistake.
const MPM_CHAT_ARGS: [&str; 3] = ["serve", "--stdio", "--allow-chat"];

/// The console's bound on one chat turn.
///
/// Why: the bridge forwards `sessions_chat` under a 20-minute cap
/// (`CHAT_REQUEST_TIMEOUT` in `tm serve --stdio`). 30 s on top lets the
/// bridge's own timeout reply arrive first, so the page shows the bridge's
/// error instead of a bare console timeout.
pub const CHAT_CALL_TIMEOUT: Duration = Duration::from_secs(20 * 60 + 30);

/// The dedicated MCP handle for chat turns, and the bound each turn gets.
///
/// Why: #9979 — a chat turn can run for minutes, and `McpServiceHandle` holds
/// its per-client lock for a whole call. On the shared `"trusty-mpm"` handle a
/// turn would stall the Sessions, Disk and Config routes and the metrics
/// poller for that long. A handle of its own keeps them apart.
/// What: built lazily (no spawn until the first turn); kept out of
/// `AppState::mcp_handles`, so neither the services overlay nor the poller
/// touches it.
/// Test: `chat_uses_the_dedicated_handle_not_the_shared_one`.
#[derive(Clone)]
pub struct MpmChat {
    handle: Arc<McpServiceHandle>,
    call_timeout: Duration,
}

impl Default for MpmChat {
    fn default() -> Self {
        let args = MPM_CHAT_ARGS.iter().map(|a| a.to_string()).collect();
        Self {
            handle: Arc::new(McpServiceHandle::new(MPM_CHAT_BINARY, args)),
            call_timeout: CHAT_CALL_TIMEOUT,
        }
    }
}

impl MpmChat {
    /// The dedicated handle (tests prime it; nothing else should call it).
    pub fn handle(&self) -> &Arc<McpServiceHandle> {
        &self.handle
    }

    /// A chat with `handle` and a test-sized bound.
    #[cfg(test)]
    pub(crate) fn with_timeout(handle: Arc<McpServiceHandle>, call_timeout: Duration) -> Self {
        Self {
            handle,
            call_timeout,
        }
    }

    /// Run one chat turn.
    ///
    /// Why: the page needs the tool's reply or a reason it failed; a turn is
    /// never retried, because an action-capable turn is not idempotent and a
    /// timed-out call is not cancelled on the daemon.
    /// What: one capability-gated `sessions_chat` call under `call_timeout`;
    /// the envelope goes through [`interpret_envelope`].
    /// Test: `a_timed_out_turn_is_a_503_naming_the_timeout`,
    /// `chat_without_mpm_is_a_503_with_a_text_body`.
    pub async fn turn(&self, body: Value) -> Result<Value, ChatTurnError> {
        let raw = self
            .handle
            .call_tool_enveloped(SESSIONS_CHAT_TOOL, body, Some(self.call_timeout))
            .await
            .map_err(ChatTurnError::Unreachable)?;
        interpret_envelope(raw)
    }
}

/// Why a chat turn produced no reply.
///
/// Why named: #9979 Fail-Open check — the dedicated handle's failure branches
/// (spawn fails, tool missing, timeout fires, tool reports an error) must
/// reach the page as an error, never as a default reply.
/// What: `Unreachable` wraps the handle's error; `ToolFailed` carries the
/// tool's own error text. Both answer `503` with a text body.
/// Test: `a_tool_error_envelope_is_a_tool_failure`,
/// `a_timed_out_turn_is_a_503_naming_the_timeout`.
#[derive(Debug)]
pub enum ChatTurnError {
    /// The handle could not complete the call.
    Unreachable(McpHandleError),
    /// `sessions_chat` answered with `isError: true`.
    ToolFailed(String),
}

impl std::fmt::Display for ChatTurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(McpHandleError::ToolUnavailable { hint, .. }) => {
                write!(f, "mpm chat is unavailable: {hint}")
            }
            Self::Unreachable(e) => write!(f, "mpm chat is unavailable: {e}"),
            Self::ToolFailed(text) => write!(f, "mpm chat failed: {text}"),
        }
    }
}

impl IntoResponse for ChatTurnError {
    fn into_response(self) -> Response {
        tracing::warn!("mpm chat turn failed: {self}");
        (StatusCode::SERVICE_UNAVAILABLE, self.to_string()).into_response()
    }
}

/// Turn a raw `tools/call` result into the reply JSON or a tool failure.
///
/// What: `isError: true` → [`ChatTurnError::ToolFailed`] with the envelope's
/// text; otherwise the unwrapped payload.
/// Test: `a_tool_error_envelope_is_a_tool_failure`,
/// `a_success_envelope_unwraps_to_the_reply`.
fn interpret_envelope(raw: Value) -> Result<Value, ChatTurnError> {
    let is_error = raw.get("isError").and_then(Value::as_bool) == Some(true);
    let payload = unwrap_mcp_content(raw);
    if is_error {
        let text = match payload {
            Value::String(s) => s,
            other => other.to_string(),
        };
        return Err(ChatTurnError::ToolFailed(text));
    }
    Ok(payload)
}

/// `POST /api/console/sessions/chat` — one chat turn for the console chat page.
///
/// Why: D96 moves the daemon's `/web` chat page to the console; this is the
/// one endpoint that page calls.
/// What: forwards the JSON body (`{message, actions, conv_id?}`) to
/// `sessions_chat` on the dedicated chat handle; `200` with the tool's JSON
/// (`reply`, `conv_id`, `actions_taken`, `cost`), or `503` with a text body.
/// Loopback-only via `tools_ui_mpm::mpm_chat_guard`; cross-origin writes are
/// refused by the router-wide origin guard.
/// Test: `chat_route_is_not_shadowed_by_the_id_capture`,
/// `chat_without_mpm_is_a_503_with_a_text_body`.
pub async fn chat_handler(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    match state.mpm_chat.turn(body).await {
        Ok(reply) => axum::Json(reply).into_response(),
        Err(e) => e.into_response(),
    }
}

/// The tool set a primed test handle advertises: `console_metrics` (the
/// connect sentinel) plus `extra`.
#[cfg(test)]
pub(crate) fn test_tool_set(extra: &[&str]) -> std::collections::HashSet<String> {
    std::iter::once("console_metrics")
        .chain(extra.iter().copied())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
