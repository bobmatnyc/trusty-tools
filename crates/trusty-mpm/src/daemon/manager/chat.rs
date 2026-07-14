//! `POST /api/v1/manager/chat` — conversation-keyed portfolio chat loop, with a
//! phase-2 in-conversation propose→confirm action flow (WI-4 #2581, WI-9 #2586).
//!
//! Why: DOC-36 §3.2 gives `tm manager` a conversation-keyed chat turn against the
//! portfolio-manager persona. Phase 1 shipped it strictly read-only; DOC-36 §6
//! phase 2 EXPLICITLY SUPERSEDES that framing (coordinator review of #2586: the
//! issue's primary acceptance criterion is that the CHAT LOOP itself — not only
//! the standalone `/manager/act` endpoint — "proposes a session launch/inject
//! action in-conversation and only executes it after explicit user confirmation
//! in the same [conversation]"). This module now supports exactly that: when the
//! model's reply embeds a proposal (see [`super::proposal::extract_proposed_action`]),
//! the chat handler stores it as PENDING for that `conversation_key` and returns
//! it to the caller as advisory text — NOTHING is executed. Only when the VERY
//! NEXT turn on that same key is an explicit confirmation
//! ([`super::proposal::is_confirmation`]) does the handler execute it, via the
//! SAME [`super::act::execute_action`] dispatch and [`super::actuator::ManagerActuator`]
//! seam `/manager/act` uses — never a duplicated execution path. A pending
//! proposal NEVER survives past that immediately-following turn (next-turn-only
//! TTL, [`super::proposal::ProposalStore`]) — an operator who moves on to a
//! different topic and later says "confirm" out of context executes nothing.
//! DOC-35 §11's boundary is satisfied by the EXPLICIT CONFIRMATION TURN, not by
//! chat being structurally incapable of acting: the request still carries NO
//! `tools` (the proposal is a parsed text sentinel, never real tool-calling), and
//! a PLAIN message (no proposal, no confirmation) still never mutates anything.
//! What: [`ChatReplyBody`]/[`ChatRequestBody`], the snapshot+history prompt
//! builder [`build_chat_messages`] (now also documenting the proposal sentinel
//! format to the model), and the [`manager_chat_route`] handler, which checks the
//! pending-proposal store BEFORE any LLM call so a confirm turn executes with
//! ZERO inference calls.
//! Test: `build_chat_messages_includes_context_and_history` in `chat_tests.rs`;
//! HTTP multi-turn / degrade / no-tools coverage in `tests/manager_inference.rs`;
//! the propose→confirm suite (proposal executes nothing, confirm executes
//! exactly once, cross-conversation isolation, next-turn expiry, plain messages
//! never execute) in `tests/manager_routing.rs`.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use serde_json::json;
use trusty_common::inference::{ChatMessage, ChatRequest};

use super::act::{ActResponse, execute_action, propose_message};
use super::actuator::resolve_actuator;
use super::chat_store::{ChatTurn, TurnRole};
use super::inference::{MANAGER_MAX_TOKENS, MANAGER_TEMPERATURE};
use super::proposal::{extract_proposed_action, is_confirmation};
use super::status::{PortfolioStatusResponse, load_portfolio_status};
use crate::daemon::state::DaemonState;

/// Request body for `POST /api/v1/manager/chat`.
///
/// Why: DOC-36 §3.2 fixes the shape `{ conversation_key, message }`; the
/// conversation key mirrors the L2 proxy focus-map keying so the surface is
/// channel-agnostic. The SAME key drives the propose→confirm action flow
/// (#2586): a `message` on this key that [`is_confirmation`] returns `true` for
/// executes whatever proposal is pending for it.
/// What: the caller-supplied conversation key and the new user message.
/// Test: HTTP coverage in `tests/manager_inference.rs`, `tests/manager_routing.rs`.
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
/// a channel/CLI client to attribute and thread the conversation, and — new in
/// phase 2 — the structured [`ActResponse`] when this turn proposed OR executed
/// an action, so a client does not have to scrape the prose for it.
/// What: the echoed conversation key, the reply prose, the authoring model slug,
/// the retained turn count, and `action_result` (absent on an ordinary turn;
/// `ActResponse::Proposed` when this turn's reply embedded a new proposal; the
/// matching executed variant — `Launched`/`Injected`/`Summarized`/etc. — when
/// this turn confirmed a pending one).
/// Test: HTTP coverage in `tests/manager_routing.rs`.
#[derive(Debug, Serialize)]
pub struct ChatReplyBody {
    /// The conversation this reply belongs to.
    pub conversation_key: String,
    /// The manager's reply prose.
    pub reply: String,
    /// The model slug that authored the reply (or, on a zero-inference confirm
    /// turn, the currently-configured model slug — reported for consistency,
    /// even though no call was made this turn).
    pub model: String,
    /// Retained message count for this conversation after recording the exchange.
    pub turn_count: usize,
    /// The proposed or executed action for this turn, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_result: Option<ActResponse>,
}

/// Build the chat prompt: persona + live snapshot + recent turns.
///
/// Why: the reply must be grounded in the CURRENT deterministic portfolio state
/// (rebuilt fresh each request) and coherent across turns (recent history). The
/// persona is advisory by default — it must not CLAIM to have already acted — but
/// phase 2 (#2586) documents the `manager-action` sentinel so the model MAY
/// propose (never silently execute) exactly one action when the user explicitly
/// asks for a launch/inject/summarize. This is a parsed text convention, not real
/// tool-calling: the request still carries no `tools`, so the "no tool-calling
/// surface" invariant `tests/manager_inference.rs` pins still holds.
/// What: a system message (persona + proposal-format instructions + the
/// pretty-printed snapshot), then the prior `history` replayed as user/assistant
/// messages, then the new `user_message`.
/// Test: `build_chat_messages_includes_context_and_history`.
pub fn build_chat_messages(
    status: &PortfolioStatusResponse,
    history: &[ChatTurn],
    user_message: &str,
) -> Vec<ChatMessage> {
    let snapshot = serde_json::to_string_pretty(status)
        .unwrap_or_else(|_| "{\"error\":\"snapshot serialization failed\"}".to_string());
    let system = format!(
        "You are the portfolio manager for a software developer running many coding \
         sessions across multiple projects. Answer questions about the portfolio using \
         ONLY the deterministic status snapshot below and the conversation so far — never \
         invent projects, sessions, or counts. You are advisory: you must NOT claim to have \
         already launched, injected, killed, resumed, or changed anything — you may only \
         PROPOSE an action, which a human confirms separately before it runs.\n\n\
         If — and ONLY if — the user explicitly asks you to launch a session for a \
         project, send/inject a message into a specific session, or summarize a specific \
         session, you MAY end your reply with exactly one fenced proposal block in this \
         exact form (a bare JSON object, no extra keys):\n\
         ```manager-action\n\
         {{\"type\":\"launch\",\"project\":\"<project name>\",\"task\":\"<task text>\"}}\n\
         ```\n\
         or\n\
         ```manager-action\n\
         {{\"type\":\"inject\",\"session\":\"<session id or name>\",\"text\":\"<message text>\"}}\n\
         ```\n\
         or\n\
         ```manager-action\n\
         {{\"type\":\"summarize\",\"session\":\"<session id or name>\"}}\n\
         ```\n\
         Never include a proposal block unless the user asked for one of these three \
         actions. Never propose more than one action in a single reply. The proposal is \
         NOT executed automatically — a human must reply on this same conversation with an \
         explicit confirmation before anything runs.\n\n\
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

/// Describe an executed [`ActResponse`] as reply prose.
///
/// Why: the confirm turn skips the LLM entirely (deterministic, zero-inference
/// execution — DOC-16 D1's "one LLM call per operation" is respected by making
/// this turn ZERO calls rather than a redundant narration call), so the visible
/// `reply` text is rendered here instead of by the model.
/// What: one line per outcome variant.
/// Test: exercised via the confirm-turn HTTP coverage in `tests/manager_routing.rs`.
fn describe_action_result(result: &ActResponse) -> String {
    match result {
        ActResponse::Proposed { proposal, .. } => proposal.clone(),
        ActResponse::Launched {
            session_id, name, ..
        } => format!("Launched session '{name}' ({session_id})."),
        ActResponse::Injected { name, text, .. } => {
            format!("Sent to session '{name}': {text:?}")
        }
        ActResponse::Summarized { name, summary, .. } => {
            format!("Session '{name}': {summary}")
        }
        ActResponse::SessionNotFound { session, error } => {
            format!("Could not find session '{session}': {error}")
        }
        ActResponse::SessionVanished { session, error } => {
            format!("Session '{session}' is gone: {error}")
        }
        ActResponse::ActionFailed { session, error } => {
            format!("Action on session '{session}' failed: {error}")
        }
    }
}

/// `POST /api/v1/manager/chat` handler — conversation-keyed, with the phase-2
/// in-conversation propose→confirm action flow.
///
/// Why: the curl-first (§4) portfolio chat surface. It threads a conversation by
/// key, grounds each reply in the live deterministic snapshot plus recent turns,
/// and — per #2586 — lets the model propose (never silently execute) a session
/// action, executing it ONLY on an explicit confirming turn on the SAME key.
/// What: validates the body (400 on empty key/message); consults
/// [`super::state::ManagerState::proposals`] FIRST — if this key has a pending
/// proposal, it is unconditionally consumed (next-turn-only TTL): a confirming
/// message ([`is_confirmation`]) executes it via [`execute_action`] with ZERO
/// LLM calls; any other message discards it and falls through to the normal
/// flow. The normal flow loads the deterministic snapshot, resolves inference
/// (503 on no provider), issues ONE completion with NO `tools` attached, parses
/// the reply for an embedded proposal ([`extract_proposed_action`]) — storing it
/// as pending and stripping it from the visible text — and records the exchange
/// (in-memory window + best-effort palace dual-write). Never logs message/reply
/// text (privacy).
/// Test: HTTP coverage in `tests/manager_inference.rs`, `tests/manager_routing.rs`.
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

    let manager = state.manager_state();

    // Phase 2 (#2586): consult the pending-proposal store BEFORE any LLM call.
    // `take` unconditionally consumes (next-turn-only TTL) — a confirming
    // message executes it via the SAME actuator seam `/manager/act` uses; any
    // other message just lets it expire and falls through to the normal flow
    // (the `take` above already consumed it either way).
    if let Some(pending) = manager.proposals().take(&conversation_key)
        && is_confirmation(&body.message)
    {
        let actuator = resolve_actuator(&state);
        let action_result = match execute_action(&actuator, &conversation_key, pending).await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!("manager chat: confirmed action failed: {error}");
                ActResponse::ActionFailed {
                    session: "(launch)".to_string(),
                    error,
                }
            }
        };
        let reply = describe_action_result(&action_result);
        let model = manager.inference().model();
        let turn_count =
            manager
                .conversations()
                .record_exchange(&conversation_key, &body.message, &reply);
        manager
            .palace()
            .record_chat_turn(&conversation_key, &body.message, &reply)
            .await;
        return Json(ChatReplyBody {
            conversation_key,
            reply,
            model,
            turn_count,
            action_result: Some(action_result),
        })
        .into_response();
    }

    let status = match load_portfolio_status(&state).await {
        Ok(status) => status,
        Err((code, msg)) => return (code, msg).into_response(),
    };

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
    // No tool-calling surface: NO `tools` are attached, so the model has no API
    // surface to call a mutating verb directly — any action it wants is only ever
    // a PARSED TEXT PROPOSAL (see `extract_proposed_action`), never executed by
    // this call.
    let mut request = ChatRequest::new(
        model.clone(),
        build_chat_messages(&status, &history, &body.message),
    );
    request.max_tokens = Some(MANAGER_MAX_TOKENS);
    request.temperature = Some(MANAGER_TEMPERATURE);

    let raw_reply = match adapter.chat(&request).await {
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

    // Parse for an embedded proposal; on a hit, store it as pending (next turn
    // on this key may confirm it) and show the human-readable proposal text
    // instead of the raw fenced block.
    let (visible_text, proposed) = extract_proposed_action(&raw_reply);
    let (reply, action_result) = match proposed {
        Some(action) => {
            manager.proposals().set(&conversation_key, action.clone());
            let proposal = propose_message(&action);
            let reply = if visible_text.is_empty() {
                proposal.clone()
            } else {
                format!("{visible_text}\n\n{proposal}")
            };
            (reply, Some(ActResponse::Proposed { proposal, action }))
        }
        None => (raw_reply, None),
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
        action_result,
    })
    .into_response()
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
