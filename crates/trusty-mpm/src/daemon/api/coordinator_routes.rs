//! Coordinator HTTP routes (`/api/v1/sessions/context` + `/api/v1/sessions/chat`).
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

/// JSON body for `POST /api/v1/sessions/chat`.
///
/// Why: the coordinator chat is conversational; the caller owns the history so
/// the daemon stays stateless about chat sessions, exactly like `/llm/chat`.
/// The SM path (SM-7) maintains its OWN rolling context keyed by `conv_id`, so
/// the request gains an OPTIONAL `conv_id` — additive, defaulting to absent, so
/// the existing DOC-13 TUI request shape still deserializes unchanged.
/// What: the user's `message`, the prior conversation `history` (legacy
/// `LlmOverseer` path), and an optional `conv_id` (SM path).
/// Test: `coordinator_chat_routes_unknown_session`, `sm_chat_request_back_compat`.
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct CoordinatorChatRequest {
    /// The user's message text.
    pub message: String,
    /// Prior conversation history (oldest first); empty starts a new chat.
    ///
    /// Used by the legacy `LlmOverseer` fallback path (caller-owned history). The
    /// SM path keys its own rolling context off `conv_id` instead.
    #[serde(default)]
    #[schema(value_type = Vec<Object>)]
    pub history: Vec<ChatMessage>,
    /// Conversation id for the SM rolling-context engine (SM path only).
    ///
    /// Optional and additive: absent on the existing TUI requests (a fresh id is
    /// minted per turn). Echoed back in the response so a follow-up can continue
    /// the same SM conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conv_id: Option<String>,
    /// Opt in to ACTION-CAPABLE chat (Phase 2, #1283): the SM may invoke managed-
    /// session verbs INLINE within this turn via the in-process `SessionControl`.
    ///
    /// Optional and additive: absent/`false` preserves the exact text-only SM
    /// `chat` path (and the legacy/@prefix paths). Only `Some(true)` routes the SM
    /// branch through the inline-action loop.
    #[serde(default)]
    pub actions: Option<bool>,
}

/// Response of `POST /api/v1/sessions/chat`.
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
    /// Estimated USD cost of the SM provider call for the DOC-13 TUI status bar.
    ///
    /// Additive (SM-7): present only on the SM path; absent (skipped) on the
    /// legacy `LlmOverseer` and prefix-routing paths, so the existing TUI
    /// response shape is preserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// The SM conversation id this turn used, echoed so a follow-up can continue
    /// the same rolling context. Additive (SM-7); present only on the SM path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conv_id: Option<String>,
    /// The verbs the SM invoked inline this turn (Phase 2, #1283), in order, for
    /// the operator's audit trail. Additive: `None` (skipped) on the text-only,
    /// legacy, and prefix paths AND on the action path when NO verb ran — so its
    /// presence means "verbs ran" and `[]` is never serialized. `Some([...])` only
    /// when `actions: true` routed the SM branch and at least one verb executed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions_taken: Option<Vec<String>>,
}

/// `GET /api/v1/sessions/context` — a cross-session activity snapshot.
///
/// Why: the TUI/GUI display the per-session summaries (name, status, recent
/// output) the coordinator reasons over; this endpoint is that read-only view.
/// What: assembles a [`CoordinatorContext`] from current daemon state — every
/// session with a recent-output excerpt plus the last 20 global hook events.
/// Always returns `200`; an absent tmux just yields empty output excerpts.
/// Test: `coordinator_context_returns_snapshot`.
#[utoipa::path(
    get,
    path = "/api/v1/sessions/context",
    tag = "config",
    responses((status = 200, description = "Cross-session activity snapshot"))
)]
pub async fn coordinator_context(
    State(state): State<Arc<DaemonState>>,
) -> Json<CoordinatorContext> {
    Json(build_coordinator_context(&state))
}

/// `POST /api/v1/sessions/chat` — coordinator message; route or answer.
///
/// Also serves the DOC-14 D0.1 alias `POST /api/v1/session-manager/chat` (same
/// handler).
///
/// Why: the coordinator is the operator's one conversational surface over every
/// session. A message prefixed with `@session:` is a direct command — no model
/// is involved — while a plain message is answered by the Session Manager (SM-7)
/// when it is enabled, else by the legacy LLM chat assistant.
/// What: builds a [`CoordinatorContext`], then [`parse_session_prefix`] checks
/// for an `@prefix:` route. On a match it sends the remaining text to that
/// session's tmux pane (via [`TmuxService`]) and returns the captured output as
/// `command_output`. With no prefix: when `[session_manager].enabled = true` AND
/// a provider is wired, the turn routes through
/// [`SessionManagerAgent::chat`](crate::core::sm::SessionManagerAgent::chat) —
/// composing the §7.5 working prompt and returning `reply` + the additive `cost`
/// and `conv_id` (a graceful degraded result maps to `503`). Otherwise it falls
/// back to the legacy [`LlmOverseer::chat`](crate::daemon::llm_overseer::LlmOverseer::chat)
/// over the client history, which requires a configured overseer (else `503`).
/// The DOC-13 TUI request/response contract is preserved: existing fields are
/// unchanged and `cost`/`conv_id` are additive (absent on the legacy paths).
/// Test: `coordinator_chat_routes_unknown_session`,
/// `coordinator_chat_without_overseer_is_503`, and the SM-path suite in
/// `coordinator_sm_tests.rs`.
#[utoipa::path(
    post,
    path = "/api/v1/sessions/chat",
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
    // session's tmux pane and return the captured output, no LLM involved. This
    // path is identical whether or not the SM is enabled (deterministic routing
    // is part of the SM's degraded surface too, §5.3).
    if let Some((session_name, command)) = parse_session_prefix(&body.message, &context.sessions) {
        let session = SessionService::new(&state).command_target(&session_name)?;
        TmuxService::send_command(&session, &command);
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let output = TmuxService::capture(&session, 100);
        return Ok(Json(CoordinatorChatResponse {
            reply: format!("Sent to {session_name}: {command}"),
            routed_to_session: Some(session_name),
            command_output: Some(output),
            cost: None,
            conv_id: None,
            actions_taken: None,
        }));
    }

    // SM path (SM-7): when the Session Manager is enabled AND a provider is
    // wired, route the free-text turn through the SM agent — superseding the
    // legacy LlmOverseer. The SM composes the §7.5 working prompt (system prompt
    // + compressed context + recall + recent rounds + message) and returns the
    // reply, per-call cost, and conversation id. A graceful degraded result
    // (`enabled = true` but no provider credentials) maps to the documented 503
    // notice; any other SM error is a genuine 500.
    let sm = state.session_manager_agent();
    if sm.is_enabled() && sm.has_runtime() {
        // Phase 2 (#1283): when the caller opts in with `actions: true`, route the
        // SM turn through the ACTION-CAPABLE loop — the SM may invoke managed-
        // session verbs inline via the IN-PROCESS `DaemonSessionControl` (never
        // over HTTP back to this daemon, so no loopback). Absent/`false` preserves
        // the exact text-only `sm.chat` path below.
        if body.actions == Some(true) {
            return route_through_action_loop(&sm, &state, &body).await;
        }
        return route_through_session_manager(&sm, &body).await;
    }

    // Fallback (SM disabled): answer via the legacy LLM chat assistant, handing
    // it the snapshot. This is byte-for-byte today's behavior.
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
        cost: None,
        conv_id: None,
        actions_taken: None,
    }))
}

/// Drive one Session Manager chat turn and map it onto the wire response (SM-7).
///
/// Why: isolating the SM branch keeps [`coordinator_chat`] readable and gives the
/// error mapping (graceful degraded → 503; real failure → 500) one home. The SM
/// owns its rolling context, so the legacy caller-owned `history` is irrelevant
/// here — `conv_id` continues the conversation instead.
/// What: calls [`SessionManagerAgent::chat`](crate::core::sm::SessionManagerAgent::chat)
/// with the message + optional `conv_id`; on success fills `reply`, the additive
/// `cost` and `conv_id` fields (leaving the routed/output fields `None` so the
/// TUI contract holds); on [`SmAgentError::Degraded`] returns
/// [`DaemonError::ServiceUnavailable`] (503); on any other error returns
/// [`DaemonError::Internal`] (500).
/// Test: `sm_chat_returns_reply_and_cost`, `sm_chat_degraded_is_503`,
/// `coordinator_and_session_manager_aliases_match` in `api_tests`.
async fn route_through_session_manager(
    sm: &crate::core::sm::SessionManagerAgent,
    body: &CoordinatorChatRequest,
) -> Result<Json<CoordinatorChatResponse>, DaemonError> {
    use crate::core::sm::SmAgentError;
    match sm.chat(&body.message, body.conv_id.as_deref()).await {
        Ok(outcome) => Ok(Json(CoordinatorChatResponse {
            reply: outcome.reply,
            routed_to_session: None,
            command_output: None,
            cost: Some(outcome.cost_usd),
            conv_id: Some(outcome.conv_id),
            actions_taken: None,
        })),
        Err(SmAgentError::Degraded(notice)) => Err(DaemonError::ServiceUnavailable(notice)),
        Err(e) => Err(DaemonError::Internal(e.to_string())),
    }
}

/// Drive one ACTION-CAPABLE Session Manager chat turn and map it to the wire
/// response (Phase 2, #1283).
///
/// Why: the `actions: true` path lets the SM invoke managed-session verbs INLINE.
/// Execution goes through the IN-PROCESS [`DaemonSessionControl`] (the same
/// helpers the HTTP `…/managed/*` routes use), NOT chat-core's HTTP-backed
/// executor — so the daemon never loops back over HTTP onto itself. Isolating
/// this branch keeps [`coordinator_chat`] readable and gives the error mapping
/// (graceful degraded → 503; real failure → 500) one home, mirroring
/// [`route_through_session_manager`].
/// What: constructs `DaemonSessionControl::new(state.clone())` as
/// `Arc<dyn SessionControl>`, calls
/// [`SessionManagerAgent::chat_with_actions`](crate::core::sm::SessionManagerAgent::chat_with_actions)
/// with the message + optional `conv_id`; on success fills `reply`, the additive
/// `cost`, `conv_id`, and the audit `actions_taken` — `Some([...])` only when at
/// least one verb ran, else `None` so an empty `[]` is never serialized (leaving
/// the routed/output fields `None`); on [`SmAgentError::Degraded`] returns a 503;
/// on any other error a 500.
/// Test: `coordinator_chat_action_path_returns_actions_taken`,
/// `coordinator_chat_actions_absent_is_text_only` in the SM-path suite.
async fn route_through_action_loop(
    sm: &crate::core::sm::SessionManagerAgent,
    state: &Arc<DaemonState>,
    body: &CoordinatorChatRequest,
) -> Result<Json<CoordinatorChatResponse>, DaemonError> {
    use crate::core::sm::{SessionControl, SmAgentError};
    use crate::daemon::sm_stdio::DaemonSessionControl;

    let control: Arc<dyn SessionControl> = Arc::new(DaemonSessionControl::new(state.clone()));
    match sm
        .chat_with_actions(&body.message, body.conv_id.as_deref(), &control)
        .await
    {
        Ok(outcome) => Ok(Json(CoordinatorChatResponse {
            reply: outcome.reply,
            routed_to_session: None,
            command_output: None,
            cost: Some(outcome.cost_usd),
            conv_id: Some(outcome.conv_id),
            // Presence means "verbs ran": omit the field (None) when none ran so
            // `[]` is never serialized; `skip_serializing_if` then drops it.
            actions_taken: Some(outcome.actions_taken).filter(|v| !v.is_empty()),
        })),
        Err(SmAgentError::Degraded(notice)) => Err(DaemonError::ServiceUnavailable(notice)),
        Err(e) => Err(DaemonError::Internal(e.to_string())),
    }
}
