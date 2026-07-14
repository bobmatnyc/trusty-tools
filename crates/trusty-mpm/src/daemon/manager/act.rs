//! `POST /api/v1/manager/act` — session launch/inject/summarize proposal-and-
//! confirm flow (WI-9, #2586, epic #2109, DOC-36 phase 2).
//!
//! Why: after `route-task` (#2585) resolves WHICH project a task belongs to, the
//! manager may want to ACT — launch a session, or inject/summarize a specific
//! session. DOC-35 §11 forbids acting without an explicit call and bans any
//! silent/background mutation, so this endpoint models an explicit PROPOSE→CONFIRM
//! protocol: a first call with `confirm` unset returns a human-readable PROPOSAL
//! and does nothing; only a second call with `confirm: true` executes exactly that
//! action, as one deliberate, traceable call. Execution never reimplements the
//! composed subsystems — a launch calls #2108's launch verb ([`spawn_managed`] via
//! the actuator), and a session-directed message routes through L2's real
//! [`crate::client::proxy::SessionProxy`] — never a direct tmux mutation. The
//! execution seam ([`ManagerActuator`], `actuator.rs`) is overridable on
//! [`ManagerState`] so the hermetic suite drives the whole flow over a test-double
//! launcher + a `SessionProxy` over a mock backend, with no live session/channel.
//! What: [`ActRequest`]/[`ProposedAction`]/[`ActResponse`], the pure
//! [`propose_message`] renderer, and the [`manager_act_route`] handler.
//! Test: `propose_message_*` in `act_tests.rs`; the propose→confirm HTTP flow
//! (launch + inject, over doubles) in `tests/manager_routing.rs`.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::actuator::{InjectOutcome, ManagerActuator, SummarizeOutcome, resolve_actuator};
use crate::daemon::state::DaemonState;

/// The action a caller proposes (and, on confirm, the manager executes).
///
/// Why: DOC-36 §3.2's manager acts in exactly three ways on a resolved route —
/// launch a project session, inject a message into a session, or summarize a
/// session. A `type`-tagged enum keeps the wire shape explicit and the handler
/// exhaustive. `Serialize` too so the proposal response can echo the action back
/// verbatim for the confirming call.
/// What: [`Self::Launch`] (project + task), [`Self::Inject`] (session + text),
/// [`Self::Summarize`] (session).
/// Test: `propose_message_launch`, `propose_message_inject` in `act_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProposedAction {
    /// Launch a new session for a project (the #2108 launch verb).
    Launch {
        /// The project to launch a session for (as resolved by `route-task`).
        project: String,
        /// The task to seed the session with.
        task: String,
    },
    /// Inject a message into a specific session (via `SessionProxy`).
    Inject {
        /// Managed session id, name, or unambiguous prefix.
        session: String,
        /// The message text to inject.
        text: String,
    },
    /// Summarize a specific session's recent activity (via `SessionProxy`).
    Summarize {
        /// Managed session id, name, or unambiguous prefix.
        session: String,
    },
}

/// Request body for `POST /api/v1/manager/act`.
///
/// Why: the propose→confirm protocol needs the conversation key (so an inject
/// routes through the same conversation-keyed focus map every channel uses), the
/// action, and the explicit confirmation flag that gates execution.
/// What: the conversation key, the [`ProposedAction`], and `confirm` (absent/false
/// = propose only; true = execute).
/// Test: HTTP coverage in `tests/manager_routing.rs`.
#[derive(Debug, Deserialize)]
pub struct ActRequest {
    /// Conversation key (same keying shape as `SessionProxy`'s focus map).
    ///
    /// Why REQUIRED uniformly across every [`ProposedAction`] variant — including
    /// `Launch`, whose execution path (`execute_action`) does not read it at all:
    /// this is a DELIBERATE consistency/audit-trail choice (coordinator review
    /// finding 3), not an oversight. Every manager action — proposed via this
    /// endpoint or via the chat loop's in-conversation propose/confirm (#2586,
    /// `chat.rs`) — is attributable to the conversation that proposed it, and a
    /// single uniform validation rule (`400` on empty, for every action type) is
    /// simpler to reason about and test than a per-variant carve-out. A future
    /// caller that wants a conversation-less launch should mint a synthetic key
    /// (e.g. `"api:<uuid>"`) rather than have the server special-case `Launch`.
    /// Test: `act_empty_conversation_key_is_400` (Summarize),
    /// `act_launch_also_requires_conversation_key` (Launch) in
    /// `tests/manager_routing.rs`.
    pub conversation_key: String,
    /// The proposed action.
    pub action: ProposedAction,
    /// Explicit confirmation. Absent/false returns a proposal WITHOUT executing.
    #[serde(default)]
    pub confirm: bool,
}

/// Response body for `POST /api/v1/manager/act`.
///
/// Why: a `status`-tagged enum lets a caller branch on the outcome without probing
/// for null fields. The propose case and every execution outcome (including the
/// advisory "session not found"/"vanished"/"failed" states, which are valid
/// results a caller must render, not transport errors — mirroring the proxy
/// routes' always-200 convention) are distinct variants.
/// What: `Proposed` (nothing executed) plus one variant per executed outcome.
/// Test: HTTP coverage in `tests/manager_routing.rs`.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ActResponse {
    /// The action was PROPOSED, not executed — re-send with `confirm: true`.
    Proposed {
        /// A human-readable description of what confirming would do.
        proposal: String,
        /// The exact action to confirm (echo back on the confirming call).
        action: ProposedAction,
    },
    /// A session was launched (the #2108 launch verb ran).
    Launched {
        /// Canonical session id.
        session_id: String,
        /// Friendly session name.
        name: String,
        /// Lifecycle state immediately after launch.
        state: String,
    },
    /// A message was injected into the resolved session.
    Injected {
        /// Canonical session id it was sent to.
        session_id: String,
        /// Friendly session name.
        name: String,
        /// The injected text.
        text: String,
    },
    /// A session's activity was summarized.
    Summarized {
        /// Canonical session id.
        session_id: String,
        /// Friendly session name.
        name: String,
        /// Lifecycle state.
        state: String,
        /// The activity summary text.
        summary: String,
        /// Any decision the session is blocked on.
        pending_decision: Option<String>,
    },
    /// The target session could not be resolved (inject/summarize).
    SessionNotFound {
        /// The unresolved target.
        session: String,
        /// The resolution error.
        error: String,
    },
    /// The target session vanished mid-action; focus was auto-cleared.
    SessionVanished {
        /// The session that was targeted.
        session: String,
        /// The "not found" error.
        error: String,
    },
    /// A transient failure acting on the session (state preserved).
    ActionFailed {
        /// The session that was targeted.
        session: String,
        /// The transport/daemon error.
        error: String,
    },
}

impl From<InjectOutcome> for ActResponse {
    fn from(o: InjectOutcome) -> Self {
        match o {
            InjectOutcome::Sent { target, text } => Self::Injected {
                session_id: target.id,
                name: target.name,
                text,
            },
            InjectOutcome::NotFound { session, error } => Self::SessionNotFound { session, error },
            InjectOutcome::Vanished { target, error } => Self::SessionVanished {
                session: target.name,
                error,
            },
            InjectOutcome::Failed { target, error } => Self::ActionFailed {
                session: target.name,
                error,
            },
        }
    }
}

impl From<SummarizeOutcome> for ActResponse {
    fn from(o: SummarizeOutcome) -> Self {
        match o {
            SummarizeOutcome::Summary {
                target,
                state,
                summary,
                pending_decision,
            } => Self::Summarized {
                session_id: target.id,
                name: target.name,
                state,
                summary,
                pending_decision,
            },
            SummarizeOutcome::NotFound { session, error } => {
                Self::SessionNotFound { session, error }
            }
            SummarizeOutcome::Vanished { target, error } => Self::SessionVanished {
                session: target.name,
                error,
            },
            SummarizeOutcome::Failed { target, error } => Self::ActionFailed {
                session: target.name,
                error,
            },
        }
    }
}

/// Render the human-readable proposal string for an action.
///
/// Why: the propose step must tell the operator EXACTLY what confirming will do,
/// so the confirmation is informed — DOC-35 §11's "explicit call" made
/// conversational. Pure so it is unit-testable without HTTP.
/// What: a one-line description naming the action, its target, and the
/// confirm-to-execute instruction.
/// Test: `propose_message_launch`, `propose_message_inject`, `propose_message_summarize`.
pub fn propose_message(action: &ProposedAction) -> String {
    let body = match action {
        ProposedAction::Launch { project, task } => {
            format!("launch a new session for project '{project}' with task: {task:?}")
        }
        ProposedAction::Inject { session, text } => {
            format!("inject into session '{session}' the message: {text:?}")
        }
        ProposedAction::Summarize { session } => {
            format!("summarize the recent activity of session '{session}'")
        }
    };
    format!("Proposed: {body}. Re-send this request with \"confirm\": true to execute it.")
}

/// Execute a confirmed [`ProposedAction`] through the [`ManagerActuator`] seam.
///
/// Why: BOTH `/manager/act`'s confirm branch and the chat loop's in-conversation
/// confirm turn (#2586, coordinator review finding 1) must execute an action the
/// IDENTICAL way — through the same actuator instance and the same
/// [`ProposedAction`]/[`ActResponse`] types — so extracting the dispatch here is
/// what makes the chat wiring a genuine reuse rather than a parallel copy.
/// What: dispatches by variant — `Launch` calls [`ManagerActuator::launch`] and
/// maps `Err` to a `String` (the caller decides how to render a launch failure,
/// since `/manager/act` renders it as a 502 while chat renders it inline in the
/// reply); `Inject`/`Summarize` call the matching actuator verb and always
/// succeed at the HTTP layer (their failure modes are valid [`ActResponse`]
/// variants, never a hard error).
/// Test: HTTP coverage in `tests/manager_routing.rs` (both the `/manager/act`
/// confirm path and the chat propose-confirm suite exercise this).
pub async fn execute_action(
    actuator: &Arc<dyn ManagerActuator>,
    conversation_key: &str,
    action: ProposedAction,
) -> Result<ActResponse, String> {
    match action {
        ProposedAction::Launch { project, task } => {
            actuator
                .launch(&project, &task)
                .await
                .map(|outcome| ActResponse::Launched {
                    session_id: outcome.session_id,
                    name: outcome.name,
                    state: outcome.state,
                })
        }
        ProposedAction::Inject { session, text } => Ok(ActResponse::from(
            actuator.inject(conversation_key, &session, &text).await,
        )),
        ProposedAction::Summarize { session } => Ok(ActResponse::from(
            actuator.summarize(conversation_key, &session).await,
        )),
    }
}

/// `POST /api/v1/manager/act` handler (propose → confirm).
///
/// Why: the curl-first (§4) surface for acting on a resolved route WITHOUT silent
/// mutation. A call with `confirm` unset returns a proposal and touches nothing; a
/// call with `confirm: true` executes exactly one action through the
/// [`ManagerActuator`] seam — a launch via #2108's launch verb, an inject/summarize
/// via L2's `SessionProxy`. This is ALSO the seam the chat loop's confirm turn
/// drives (`chat.rs`, #2586) via the shared [`execute_action`] helper — the API
/// route and the conversational confirm turn execute identically.
/// What: validates the conversation key (400 on empty — required uniformly across
/// every action, including `Launch`, which does not read it operationally; see
/// [`ActRequest::conversation_key`]'s doc for why); on `confirm == false` returns
/// [`ActResponse::Proposed`]; on `confirm == true` resolves the actuator (a test
/// override on [`ManagerState`], else a fresh production `ProxyActuator`, via
/// [`resolve_actuator`]) and calls [`execute_action`]. A launch spawn error is a
/// 502 (the one genuinely failing side effect); inject/summarize
/// resolution/vanish/transient outcomes are valid advisory states returned as 200.
/// Never logs task/message text (privacy).
/// Test: HTTP coverage in `tests/manager_routing.rs`.
pub async fn manager_act_route(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<ActRequest>,
) -> impl IntoResponse {
    let conversation_key = body.conversation_key.trim().to_string();
    if conversation_key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_request",
                         "message": "conversation_key must not be empty" })),
        )
            .into_response();
    }

    // Propose-only: return what confirming would do; execute NOTHING.
    if !body.confirm {
        return Json(ActResponse::Proposed {
            proposal: propose_message(&body.action),
            action: body.action,
        })
        .into_response();
    }

    // Confirmed: resolve the execution seam (test override, else production) and
    // execute through the SAME dispatch the chat confirm turn uses.
    let actuator = resolve_actuator(&state);
    match execute_action(&actuator, &conversation_key, body.action).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => {
            tracing::warn!("manager act: action execution failed: {error}");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": "launch_failed", "message": error })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
#[path = "act_tests.rs"]
mod tests;
