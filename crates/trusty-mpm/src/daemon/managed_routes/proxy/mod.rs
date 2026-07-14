//! Local HTTP surface for the session-manager PROXY (TELUI-6, #1440).
//!
//! Why: before this module, the focus/inject/summarize state machine
//! ([`crate::client::proxy::SessionProxy`]) could only be exercised by
//! constructing a Telegram (or Slack) bot process and talking to it. That made
//! the proxy layer un-testable without a live bot token — an operator (or CI)
//! had no way to drive "focus a session, then send it a message" without
//! standing up a real channel. This module exposes the SAME
//! [`crate::client::proxy::SessionProxy`] state machine as a plain daemon HTTP
//! surface (`/api/v1/sessions/proxy/*`) so it is `curl`-testable locally BEFORE
//! any channel is connected — and, critically, once Telegram IS connected, its
//! `focus`/`inject`/`summarize` calls run through this exact same state machine
//! (just reached over HTTP via [`crate::client::executor::CommandExecutor`]
//! instead of in-process), so a local API test genuinely exercises what
//! Telegram will exercise.
//! What: [`DirectManagedBackend`] implements
//! [`crate::client::proxy::ManagedBackend`] directly over this daemon's OWN
//! `SessionManager` (no network hop, no self-referential HTTP call) — resolving
//! a fuzzy target via the shared [`crate::client::resolve_target`] resolver,
//! injecting text via `SessionManager::send_input`, and building a lightweight
//! [`crate::client::ActivityDigest`] straight from the session record (state,
//! task, pending decision — deliberately NOT the heavier LLM-classified
//! `GET .../activity` digest, so this surface stays hermetic: no tmux capture,
//! no LLM key required, safe and fast for local/CI testing). Five handlers wire
//! this backend to a per-request [`crate::client::SessionProxy`] built with
//! [`crate::client::SessionProxy::with_focus_store`] over
//! [`DaemonState::proxy_focus_store`], and render each proxy outcome as a
//! tagged JSON body — HTTP 200 even for a "no session focused" or "session
//! vanished" outcome, since those are valid states a caller must branch on, not
//! transport errors (a caller can therefore fall back to its own coordinator on
//! a 200 without treating the call as failed).
//! Test: `tests.rs` (in-crate, mirrors the `proxy/tests.rs` client-side suite)
//! plus `tests/proxy_routes.rs` (real HTTP, the curl-facing contract).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::client::proxy::{
    FocusOutcome, FocusTarget, InjectOutcome, ManagedBackend, SessionProxy, SummarizeOutcome,
};
use crate::daemon::state::DaemonState;

mod backend;
pub use backend::DirectManagedBackend;

/// Build a per-request [`SessionProxy`] over this daemon's direct backend,
/// sharing `state`'s persistent focus store.
///
/// Why: every handler below needs the identical construction; centralising it
/// keeps each handler a one-line call. Since #2550 the MCP proxy tools
/// ([`crate::daemon::mcp_proxy`]) build their [`SessionProxy`] through this SAME
/// helper, so the HTTP and MCP surfaces share one construction site over the one
/// shared focus store — a focus set on either surface is visible on the other.
/// What: wraps a fresh [`DirectManagedBackend`] and the daemon's shared focus
/// map into a [`SessionProxy`].
pub(crate) fn local_proxy(state: &Arc<DaemonState>) -> SessionProxy {
    let backend: Arc<dyn ManagedBackend> = Arc::new(DirectManagedBackend::new(Arc::clone(state)));
    SessionProxy::with_focus_store(backend, state.proxy_focus_store())
}

/// Request body for `POST /api/v1/sessions/proxy/focus`.
#[derive(Debug, Deserialize)]
pub struct ProxyFocusRequest {
    /// Opaque caller-supplied conversation key (Telegram's `chat_id`, Slack's
    /// channel id, or any caller-chosen identifier for local testing).
    pub conversation_key: String,
    /// Managed session id, friendly name, or unambiguous prefix to focus.
    /// Empty (or absent) queries the CURRENT focus instead of setting one.
    #[serde(default)]
    pub session_id: String,
}

/// Request body for `POST /api/v1/sessions/proxy/unfocus`.
#[derive(Debug, Deserialize)]
pub struct ProxyUnfocusRequest {
    /// The conversation whose focus should be cleared.
    pub conversation_key: String,
}

/// Request body for `POST /api/v1/sessions/proxy/message`.
#[derive(Debug, Deserialize)]
pub struct ProxyMessageRequest {
    /// The conversation the message arrived on.
    pub conversation_key: String,
    /// The free-text message body.
    pub text: String,
}

/// A resolved focus target, wire-shaped for the proxy JSON responses.
#[derive(Debug, Serialize)]
pub struct ProxyTargetWire {
    /// Canonical managed-session id.
    pub session_id: String,
    /// Friendly session name.
    pub name: String,
}

impl From<FocusTarget> for ProxyTargetWire {
    fn from(f: FocusTarget) -> Self {
        Self {
            session_id: f.id,
            name: f.name,
        }
    }
}

/// Response body for the focus-query/-set endpoints
/// (`POST /api/v1/sessions/proxy/focus`, `GET .../focus/{conversation_key}`).
///
/// Why: a tagged enum lets a caller branch on `outcome` without probing for
/// `null` fields; every variant is a valid, non-error state (always HTTP 200).
/// What: `focused` on a successful resolve-and-set; `current` for a read-only
/// query (target may be `null` when nothing is focused); `not_found` when the
/// target could not be resolved (focus is left unchanged).
#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProxyFocusResponse {
    /// The session was validated and is now focused.
    Focused(ProxyTargetWire),
    /// A read-only query of the current focus (may be unset).
    Current {
        /// The currently focused session, or `None` if unfocused.
        target: Option<ProxyTargetWire>,
    },
    /// The requested target could not be resolved.
    NotFound {
        /// The unresolved target string.
        target: String,
        /// Why resolution failed.
        error: String,
    },
}

impl From<FocusOutcome> for ProxyFocusResponse {
    fn from(o: FocusOutcome) -> Self {
        match o {
            FocusOutcome::Focused(t) => Self::Focused(t.into()),
            FocusOutcome::Current(t) => Self::Current {
                target: t.map(Into::into),
            },
            FocusOutcome::NotFound { target, error } => Self::NotFound { target, error },
        }
    }
}

/// Response body for `POST /api/v1/sessions/proxy/unfocus`.
#[derive(Debug, Serialize)]
pub struct ProxyUnfocusResponse {
    /// The session that WAS focused, now cleared (`None` if nothing was).
    pub cleared: Option<ProxyTargetWire>,
}

/// Response body for `POST /api/v1/sessions/proxy/message` — mirrors exactly
/// how Telegram free text routes: focused → inject outcome; unfocused →
/// `no_focus` so the caller can fall back to its own coordinator.
///
/// Why: this is the acceptance surface for "free text routes exactly like
/// Telegram" — the SAME [`InjectOutcome`] the Telegram binding renders to HTML
/// is rendered to JSON here, over the SAME [`SessionProxy::inject`] call.
/// What: `sent` on success; `auto_unfocused` when the focused session had
/// vanished (focus is already cleared by the time this is returned); `failed`
/// on a transient backend error (focus preserved); `no_focus` when nothing was
/// focused for this conversation.
#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProxyMessageResponse {
    /// The text was injected into the focused session.
    Sent {
        /// The session it was sent to.
        target: ProxyTargetWire,
        /// The text that was injected.
        text: String,
    },
    /// The focused session had vanished; focus was auto-cleared.
    AutoUnfocused {
        /// The session that was focused (now cleared).
        target: ProxyTargetWire,
        /// The "not found" error that triggered the auto-unfocus.
        error: String,
    },
    /// A transient failure; focus is preserved.
    Failed {
        /// The still-focused session.
        target: ProxyTargetWire,
        /// The backend error.
        error: String,
    },
    /// Nothing was focused for this conversation — the caller should fall back
    /// to its own coordinator/default handling.
    NoFocus,
}

impl From<InjectOutcome> for ProxyMessageResponse {
    fn from(o: InjectOutcome) -> Self {
        match o {
            InjectOutcome::Sent { target, text } => Self::Sent {
                target: target.into(),
                text,
            },
            InjectOutcome::AutoUnfocused { target, error } => Self::AutoUnfocused {
                target: target.into(),
                error,
            },
            InjectOutcome::Failed { target, error } => Self::Failed {
                target: target.into(),
                error,
            },
            InjectOutcome::NoFocus => Self::NoFocus,
        }
    }
}

/// Response body for `GET /api/v1/sessions/proxy/summary/{conversation_key}`.
///
/// Why: mirrors [`ProxyMessageResponse`] for the SUMMARIZE direction — the same
/// [`SummarizeOutcome`] the Telegram `/summary` command renders is rendered here.
#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProxySummaryResponse {
    /// A digest of the focused session's recent activity.
    Summary {
        /// The focused session summarized.
        target: ProxyTargetWire,
        /// Current lifecycle state.
        state: String,
        /// Short activity summary.
        summary: String,
        /// Any decision the session is blocked on.
        pending_decision: Option<String>,
    },
    /// The focused session had vanished; focus was auto-cleared.
    AutoUnfocused {
        /// The session that was focused (now cleared).
        target: ProxyTargetWire,
        /// The "not found" error that triggered the auto-unfocus.
        error: String,
    },
    /// A transient failure; focus is preserved.
    Failed {
        /// The still-focused session.
        target: ProxyTargetWire,
        /// The backend error.
        error: String,
    },
    /// Nothing was focused for this conversation.
    NoFocus,
}

impl From<SummarizeOutcome> for ProxySummaryResponse {
    fn from(o: SummarizeOutcome) -> Self {
        match o {
            SummarizeOutcome::Summary {
                target,
                state,
                summary,
                pending_decision,
            } => Self::Summary {
                target: target.into(),
                state,
                summary,
                pending_decision,
            },
            SummarizeOutcome::AutoUnfocused { target, error } => Self::AutoUnfocused {
                target: target.into(),
                error,
            },
            SummarizeOutcome::Failed { target, error } => Self::Failed {
                target: target.into(),
                error,
            },
            SummarizeOutcome::NoFocus => Self::NoFocus,
        }
    }
}

/// `POST /api/v1/sessions/proxy/focus` — focus (or query) a session for a
/// conversation.
///
/// Why: the "session click" of TELUI-6 translated to a plain HTTP verb — set
/// (or, with an empty `session_id`, query) the focused session for
/// `conversation_key`.
/// What: builds a per-request [`SessionProxy`] over the shared focus store and
/// calls [`SessionProxy::focus`]; always 200, the JSON `outcome` tag carries the
/// result.
/// Test: `proxy_focus_route_*` in `tests.rs` / `tests/proxy_routes.rs`.
pub async fn proxy_focus(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<ProxyFocusRequest>,
) -> impl IntoResponse {
    let proxy = local_proxy(&state);
    let outcome = proxy.focus(&req.conversation_key, &req.session_id).await;
    Json(ProxyFocusResponse::from(outcome)).into_response()
}

/// `GET /api/v1/sessions/proxy/focus/{conversation_key}` — query the current
/// focus without setting one.
///
/// Why: a read-only companion to `POST .../focus` for a caller that just wants
/// to know "what's focused right now?" via a plain GET.
/// What: delegates to [`SessionProxy::focus`] with an empty target, which never
/// touches the backend and always yields [`FocusOutcome::Current`].
/// Test: `proxy_get_focus_route_*` in `tests.rs` / `tests/proxy_routes.rs`.
pub async fn proxy_get_focus(
    State(state): State<Arc<DaemonState>>,
    AxumPath(conversation_key): AxumPath<String>,
) -> impl IntoResponse {
    let proxy = local_proxy(&state);
    let outcome = proxy.focus(&conversation_key, "").await;
    Json(ProxyFocusResponse::from(outcome)).into_response()
}

/// `POST /api/v1/sessions/proxy/unfocus` — clear a conversation's focus.
///
/// Why: the "back button" of TELUI-6 as a plain HTTP verb.
/// What: calls [`SessionProxy::unfocus`] and reports the cleared session (or
/// `None`) — never an error; unfocusing an already-unfocused conversation is a
/// harmless no-op.
/// Test: `proxy_unfocus_route_*` in `tests.rs` / `tests/proxy_routes.rs`.
pub async fn proxy_unfocus(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<ProxyUnfocusRequest>,
) -> impl IntoResponse {
    let proxy = local_proxy(&state);
    let cleared = proxy.unfocus(&req.conversation_key).map(Into::into);
    Json(ProxyUnfocusResponse { cleared }).into_response()
}

/// `POST /api/v1/sessions/proxy/message` — route free text exactly like a
/// channel's non-command message.
///
/// Why: THE acceptance endpoint for "the telegram model should be API testable
/// locally" — this reproduces the Telegram `on_message` free-text branch
/// (focused → inject; unfocused → the caller's own fallback) as a single HTTP
/// call, over the identical [`SessionProxy::inject`] the Telegram binding calls.
/// What: when `req.conversation_key` has a focus, injects `req.text` into it and
/// returns the resulting [`InjectOutcome`]; otherwise returns
/// [`ProxyMessageResponse::NoFocus`] — always HTTP 200, so a caller can branch
/// on `outcome` to decide whether to fall back to ITS OWN coordinator/default
/// handling rather than treating the call as failed.
/// Test: `proxy_message_route_*` in `tests.rs` / `tests/proxy_routes.rs`.
pub async fn proxy_message(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<ProxyMessageRequest>,
) -> impl IntoResponse {
    let proxy = local_proxy(&state);
    let outcome = proxy.inject(&req.conversation_key, &req.text).await;
    Json(ProxyMessageResponse::from(outcome)).into_response()
}

/// `GET /api/v1/sessions/proxy/summary/{conversation_key}` — digest the focused
/// session's activity.
///
/// Why: the SUMMARIZE proxy direction as a plain HTTP GET, over the identical
/// [`SessionProxy::summarize`] the Telegram `/summary` command calls.
/// What: returns the [`SummarizeOutcome`] as JSON; always HTTP 200.
/// Test: `proxy_summary_route_*` in `tests.rs` / `tests/proxy_routes.rs`.
pub async fn proxy_summary(
    State(state): State<Arc<DaemonState>>,
    AxumPath(conversation_key): AxumPath<String>,
) -> impl IntoResponse {
    let proxy = local_proxy(&state);
    let outcome = proxy.summarize(&conversation_key).await;
    Json(ProxySummaryResponse::from(outcome)).into_response()
}

#[cfg(test)]
mod tests;
