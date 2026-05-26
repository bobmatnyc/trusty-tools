//! Coordinator HTTP routes (`/api/v1/coordinator/*`).
//!
//! Why: the TUI/GUI coordinator surface needs two daemon endpoints — one that
//! returns a cross-session activity snapshot for display, and one that takes a
//! free-text message and either routes it to a named session by `@prefix:` or
//! answers it via the LLM chat assistant with the snapshot as context. Keeping
//! them in their own route module mirrors `claude_config_routes` and keeps
//! `api.rs` focused on the core session/hook/tmux surface.
//! What: the `#[utoipa::path]`-annotated `coordinator_context` and
//! `coordinator_chat` handlers plus their request/response types. They are
//! wired into the router by `api::router`.
//! Test: `cargo test -p trusty-mpm-daemon` drives these via `api_tests`.

use std::sync::Arc;

use axum::{Json, extract::State};

use crate::daemon::coordinator::{
    CoordinatorContext, build_coordinator_context, coordinator_system_prompt, parse_session_prefix,
};
use crate::daemon::error::DaemonError;
use crate::daemon::llm_overseer::ChatMessage;
use crate::daemon::services::{SessionService, TmuxService};
use crate::daemon::state::DaemonState;

/// JSON body for `POST /api/v1/coordinator/chat`.
///
/// Why: the coordinator chat is conversational; the caller owns the history so
/// the daemon stays stateless about chat sessions, exactly like `/llm/chat`.
/// What: the user's `message` plus the prior conversation `history`.
/// Test: `coordinator_chat_routes_unknown_session` exercises the shape.
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct CoordinatorChatRequest {
    /// The user's message text.
    pub message: String,
    /// Prior conversation history (oldest first); empty starts a new chat.
    #[serde(default)]
    #[schema(value_type = Vec<Object>)]
    pub history: Vec<ChatMessage>,
}

/// Response of `POST /api/v1/coordinator/chat`.
///
/// Why: a coordinator message resolves to one of two outcomes — a direct
/// command routed at a session, or an LLM answer; the response carries enough
/// for the UI to render either without a second call.
/// What: the assistant `reply` text; `routed_to_session` names the session a
/// `@prefix:` message was sent to; `command_output` carries that session's
/// captured pane output. For an LLM answer the latter two are `None`.
/// Test: `coordinator_chat_routes_unknown_session`.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct CoordinatorChatResponse {
    /// The assistant reply, or a human-readable note about the routed command.
    pub reply: String,
    /// The tmux name of the session a prefixed message was routed to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_to_session: Option<String>,
    /// Captured pane output from a routed command, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_output: Option<String>,
}

/// `GET /api/v1/coordinator/context` — a cross-session activity snapshot.
///
/// Why: the TUI/GUI display the per-session summaries (name, status, recent
/// output) the coordinator reasons over; this endpoint is that read-only view.
/// What: assembles a [`CoordinatorContext`] from current daemon state — every
/// session with a recent-output excerpt plus the last 20 global hook events.
/// Always returns `200`; an absent tmux just yields empty output excerpts.
/// Test: `coordinator_context_returns_snapshot`.
#[utoipa::path(
    get,
    path = "/api/v1/coordinator/context",
    tag = "config",
    responses((status = 200, description = "Cross-session activity snapshot"))
)]
pub async fn coordinator_context(
    State(state): State<Arc<DaemonState>>,
) -> Json<CoordinatorContext> {
    Json(build_coordinator_context(&state))
}

/// `POST /api/v1/coordinator/chat` — coordinator message; route or answer.
///
/// Why: the coordinator is the operator's one conversational surface over every
/// session. A message prefixed with `@session:` is a direct command — the LLM
/// is deliberately not involved — while a plain message is answered by the LLM
/// chat assistant handed the full session snapshot as context.
/// What: builds a [`CoordinatorContext`], then [`parse_session_prefix`] checks
/// for an `@prefix:` route. On a match it sends the remaining text to that
/// session's tmux pane (via [`TmuxService`]), captures the output, and returns
/// it as `command_output`. With no prefix it runs [`crate::daemon::llm_overseer::LlmOverseer::chat`]
/// over the client history, prepending the coordinator system prompt as the
/// first turn so the model sees every session. Requires a configured LLM
/// overseer for the non-prefix path (else `503`).
/// Test: `coordinator_chat_routes_unknown_session`, `coordinator_chat_without_overseer_is_503`.
#[utoipa::path(
    post,
    path = "/api/v1/coordinator/chat",
    tag = "config",
    request_body = CoordinatorChatRequest,
    responses(
        (status = 200, description = "Coordinator reply, or routed command output"),
        (status = 503, description = "LLM chat is not configured (non-prefixed message)"),
    )
)]
pub async fn coordinator_chat(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<CoordinatorChatRequest>,
) -> Result<Json<CoordinatorChatResponse>, DaemonError> {
    let context = build_coordinator_context(&state);

    // A `@prefix:` message is a direct command — route it straight to the
    // session's tmux pane and return the captured output, no LLM involved.
    if let Some((session_name, command)) = parse_session_prefix(&body.message, &context.sessions) {
        let session = SessionService::new(&state).command_target(&session_name)?;
        TmuxService::send_command(&session, &command);
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let output = TmuxService::capture(&session, 100);
        return Ok(Json(CoordinatorChatResponse {
            reply: format!("Sent to {session_name}: {command}"),
            routed_to_session: Some(session_name),
            command_output: Some(output),
        }));
    }

    // No prefix: answer via the LLM chat assistant, handing it the snapshot.
    let overseer = state.llm_overseer().ok_or_else(|| {
        DaemonError::ServiceUnavailable(
            "LLM chat is not configured (no OpenRouter API key)".to_string(),
        )
    })?;

    // Lead the conversation with the coordinator system prompt as a synthetic
    // first user turn so the model always sees the current session snapshot.
    let mut history = vec![ChatMessage::user(coordinator_system_prompt(&context))];
    history.push(ChatMessage::assistant(
        "Understood — I have the current session context.".to_string(),
    ));
    history.extend(body.history);

    let reply = overseer
        .chat(&mut history, &body.message)
        .await
        .map_err(|e| DaemonError::Internal(e.to_string()))?;

    Ok(Json(CoordinatorChatResponse {
        reply,
        routed_to_session: None,
        command_output: None,
    }))
}
