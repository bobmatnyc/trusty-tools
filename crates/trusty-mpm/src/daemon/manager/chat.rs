//! `POST /api/v1/manager/chat` — read-only portfolio chat loop (WI-4, #2581).
//!
//! Why: DOC-36 §3.2 gives `tm manager` a conversation-keyed chat turn against the
//! portfolio-manager persona. In phase 1 it is strictly READ-ONLY: a plain
//! completion over context (the deterministic portfolio snapshot + recent turns),
//! with NO tool-calling surface — so it structurally cannot launch/inject/kill a
//! session or mutate a Deliverable/Milestone (DOC-35 §11 / DOC-36 §2.1 boundary;
//! zero writes to #2108 records). Conversations are keyed exactly like L2's
//! SessionProxy focus map (`client/proxy.rs`, a `conversation_key` string). Turns
//! are dual-written to the portfolio palace when available and degrade silently to
//! no-persistence otherwise (§3.4). LLM calls go through the unified
//! `trusty_common::inference` adapter (§3.3); no channel/bot token is required (§4).
//! What: [`ChatRequestBody`]/[`ChatReplyBody`], the snapshot+history prompt builder
//! [`build_chat_messages`], and the [`manager_chat_route`] handler.
//! Test: `build_chat_messages_includes_context_and_history` in `chat_tests.rs`;
//! HTTP multi-turn / degrade / read-only coverage in `tests/manager_inference.rs`.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use serde_json::json;
use trusty_common::inference::{ChatMessage, ChatRequest};

use super::chat_store::{ChatTurn, TurnRole};
use super::inference::{MANAGER_MAX_TOKENS, MANAGER_TEMPERATURE};
use super::status::{PortfolioStatusResponse, load_portfolio_status};
use crate::daemon::state::DaemonState;

/// Request body for `POST /api/v1/manager/chat`.
///
/// Why: DOC-36 §3.2 fixes the shape `{ conversation_key, message }`; the
/// conversation key mirrors the L2 proxy focus-map keying so the surface is
/// channel-agnostic.
/// What: the caller-supplied conversation key and the new user message.
/// Test: HTTP coverage in `tests/manager_inference.rs`.
#[derive(Debug, Deserialize)]
pub struct ChatRequestBody {
    /// Conversation key (same keying shape as `SessionProxy`'s focus map).
    pub conversation_key: String,
    /// The new user message for this turn.
    pub message: String,
}

/// Success reply body for `POST /api/v1/manager/chat`.
///
/// Why: returns the assistant's reply plus enough metadata (model, turn count) for
/// a channel/CLI client to attribute and thread the conversation.
/// What: the echoed conversation key, the reply prose, the authoring model slug,
/// and the retained turn count for that conversation.
/// Test: HTTP coverage in `tests/manager_inference.rs`.
#[derive(Debug, Serialize)]
pub struct ChatReplyBody {
    /// The conversation this reply belongs to.
    pub conversation_key: String,
    /// The manager's reply prose.
    pub reply: String,
    /// The model slug that authored the reply.
    pub model: String,
    /// Retained message count for this conversation after recording the exchange.
    pub turn_count: usize,
}

/// Build the chat prompt: read-only persona + live snapshot + recent turns.
///
/// Why: the reply must be grounded in the CURRENT deterministic portfolio state
/// (rebuilt fresh each request) and coherent across turns (recent history). The
/// persona is pinned read-only — advisory only, never proposing a mutating action
/// — which, together with the absence of any tools on the request, makes the loop
/// structurally incapable of mutating a record.
/// What: a system message (persona + the pretty-printed snapshot), then the prior
/// `history` replayed as user/assistant messages, then the new `user_message`.
/// Test: `build_chat_messages_includes_context_and_history`.
pub fn build_chat_messages(
    status: &PortfolioStatusResponse,
    history: &[ChatTurn],
    user_message: &str,
) -> Vec<ChatMessage> {
    let snapshot = serde_json::to_string_pretty(status)
        .unwrap_or_else(|_| "{\"error\":\"snapshot serialization failed\"}".to_string());
    let system = format!(
        "You are the read-only portfolio manager for a software developer running many \
         coding sessions across multiple projects. Answer questions about the portfolio \
         using ONLY the deterministic status snapshot below and the conversation so far — \
         never invent projects, sessions, or counts. You are advisory and read-only: you \
         may explain, summarize, and prioritize, but you must NOT claim to have launched, \
         injected, killed, resumed, or changed anything, and you must not offer to do so.\n\n\
         Current portfolio snapshot (JSON):\n{snapshot}"
    );
    let mut messages = Vec::with_capacity(history.len() + 2);
    messages.push(ChatMessage::system(system));
    for turn in history {
        match turn.role {
            TurnRole::User => messages.push(ChatMessage::user(turn.content.clone())),
            TurnRole::Assistant => messages.push(ChatMessage::assistant(turn.content.clone())),
        }
    }
    messages.push(ChatMessage::user(user_message.to_string()));
    messages
}

/// Render a typed chat degrade response (`error` + actionable `message`).
///
/// Why: unlike the digest, a chat reply has no deterministic templated
/// substitute — when inference is unavailable or fails there is nothing to
/// synthesize, so the surface returns a typed error the caller can act on.
/// What: a `(status, Json{ error, message })` response.
/// Test: HTTP coverage in `tests/manager_inference.rs`.
fn chat_error(status: StatusCode, error: &str, message: String) -> axum::response::Response {
    (status, Json(json!({ "error": error, "message": message }))).into_response()
}

/// `POST /api/v1/manager/chat` handler (read-only, conversation-keyed).
///
/// Why: the curl-first (§4) portfolio chat surface. It threads a conversation by
/// key, grounds each reply in the live deterministic snapshot plus recent turns,
/// and issues exactly one read-only completion — never a tool call, never a
/// mutating endpoint — then persists the turn in-memory and (best-effort) to the
/// portfolio palace.
/// What: validates the body (400 on empty key/message), loads the deterministic
/// snapshot, resolves the inference seam (503 on no provider), builds the prompt
/// from [`build_chat_messages`], issues ONE
/// [`trusty_common::inference::InferenceAdapter::chat`] with NO tools, and on
/// success records the exchange (in-memory window + silent palace dual-write) and
/// returns the reply. An empty/failed call is a 502. Never logs message/reply text
/// (privacy).
/// Test: HTTP coverage in `tests/manager_inference.rs`.
pub async fn manager_chat_route(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<ChatRequestBody>,
) -> impl IntoResponse {
    let conversation_key = body.conversation_key.trim().to_string();
    if conversation_key.is_empty() {
        return chat_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "conversation_key must not be empty".to_string(),
        );
    }
    if body.message.trim().is_empty() {
        return chat_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "message must not be empty".to_string(),
        );
    }

    let status = match load_portfolio_status(&state).await {
        Ok(status) => status,
        Err((code, msg)) => return (code, msg).into_response(),
    };

    let manager = state.manager_state();
    let (model, adapter) = match manager.inference().resolve() {
        Ok(pair) => pair,
        Err(unavailable) => {
            return chat_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "inference_unavailable",
                unavailable.to_string(),
            );
        }
    };

    let history = manager.conversations().history(&conversation_key);
    // Read-only by construction: NO `tools` are attached, so the model has no
    // surface to call a mutating verb; the handler itself only reads state.
    let mut request = ChatRequest::new(
        model.clone(),
        build_chat_messages(&status, &history, &body.message),
    );
    request.max_tokens = Some(MANAGER_MAX_TOKENS);
    request.temperature = Some(MANAGER_TEMPERATURE);

    let reply = match adapter.chat(&request).await {
        Ok(response) => match response.first_text().filter(|t| !t.trim().is_empty()) {
            Some(text) => text,
            None => {
                return chat_error(
                    StatusCode::BAD_GATEWAY,
                    "inference_failed",
                    "provider returned an empty reply".to_string(),
                );
            }
        },
        Err(e) => {
            tracing::warn!("manager chat inference call failed: {e}");
            return chat_error(
                StatusCode::BAD_GATEWAY,
                "inference_failed",
                "inference call failed".to_string(),
            );
        }
    };

    // Record the completed exchange: in-memory window (always) + portfolio palace
    // dual-write (best-effort; silent no-op when the palace is unavailable).
    let turn_count =
        manager
            .conversations()
            .record_exchange(&conversation_key, &body.message, &reply);
    manager
        .palace()
        .record_chat_turn(&conversation_key, &body.message, &reply)
        .await;

    Json(ChatReplyBody {
        conversation_key,
        reply,
        model,
        turn_count,
    })
    .into_response()
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
