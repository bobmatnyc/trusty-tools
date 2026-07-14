//! Daemon-side implementation of the four session-manager PROXY MCP tools
//! (#2550, a #1440 follow-up).
//!
//! Why: the MCP `StateBackend` (in `mcp_backend.rs`) must service the proxy
//! tools, but inlining their bodies there risks the 500-SLOC cap. More
//! importantly these tools must be THIN WRAPPERS over the SAME
//! [`crate::client::proxy::SessionProxy`] state machine the daemon's local HTTP
//! proxy routes already drive — not a parallel reimplementation — so the MCP,
//! HTTP, and Telegram surfaces behave identically. This module builds the proxy
//! through the shared [`crate::daemon::managed_routes::proxy::local_proxy`]
//! constructor (a [`crate::client::proxy::SessionProxy`] over the in-process
//! [`crate::daemon::managed_routes::DirectManagedBackend`] and the daemon's ONE
//! shared [`crate::daemon::state::DaemonState::proxy_focus_store`]), so a focus
//! set over MCP is visible on the HTTP/Telegram surfaces under the same key and
//! vice versa.
//! What: four free async functions — `session_proxy_focus`,
//! `session_proxy_unfocus`, `session_proxy_message`, `session_proxy_summary` —
//! each taking `&Arc<DaemonState>` plus parsed arguments, calling the matching
//! [`SessionProxy`] verb, and serialising the outcome via the SAME wire response
//! types the HTTP routes return (so the JSON shape is identical across
//! transports). Every proxy outcome is a valid state (never a transport error),
//! so these always return `Ok(_)`, exactly as the HTTP routes always answer 200.
//! `mcp_backend.rs`'s single `impl OrchestratorBackend for StateBackend` block
//! wires these four functions to the trait as one-liner delegates — the SAME
//! pattern `session_new`/`config_read`/`project_list` already use for their
//! sibling modules (a trait CANNOT be split across two `impl` blocks for one
//! type — Rust rejects that as E0119 — so this module holds only the bodies,
//! not a second impl).
//! Test: `tests` below drives all four through a hermetic
//! `DaemonState::with_root_isolated_managed` (no real tmux), asserting the tagged
//! `outcome` — proving each tool reaches the real `SessionProxy` methods.

use std::sync::Arc;

use serde_json::Value;

use crate::daemon::managed_routes::proxy::local_proxy;
use crate::daemon::managed_routes::{
    ProxyFocusResponse, ProxyMessageResponse, ProxySummaryResponse, ProxyUnfocusResponse,
};
use crate::daemon::state::DaemonState;

/// Serialise a proxy wire response into a tool-result [`Value`].
///
/// Why: every proxy tool renders its outcome through the SAME `Serialize` wire
/// type the HTTP routes use; centralising the `to_value` + error mapping keeps
/// each wrapper a one-liner and the JSON identical across transports.
/// What: serialises `resp`, mapping the (practically-impossible) serialisation
/// failure to a flat error string rather than a panic.
/// Test: exercised by every `tests` case below via the four wrappers.
fn to_result<T: serde::Serialize>(resp: T) -> Result<Value, String> {
    serde_json::to_value(resp).map_err(|e| format!("failed to serialise proxy response: {e}"))
}

/// Focus (or query) a session for a conversation (`session_proxy_focus` tool).
///
/// Why: bind a conversation to ONE managed session so later
/// [`session_proxy_message`] calls route to it without a per-turn id.
/// What: with a non-empty `session_id` validates + records the focus; with an
/// empty `session_id` queries the current focus without touching the backend.
/// Returns the tagged [`ProxyFocusResponse`] as JSON.
/// Test: `tests::mcp_proxy_focus_message_summary_unfocus_round_trip`,
/// `tests::mcp_proxy_focus_empty_queries_current`.
pub async fn session_proxy_focus(
    state: &Arc<DaemonState>,
    conversation_key: &str,
    session_id: &str,
) -> Result<Value, String> {
    let proxy = local_proxy(state);
    let outcome = proxy.focus(conversation_key, session_id).await;
    to_result(ProxyFocusResponse::from(outcome))
}

/// Clear a conversation's focus (`session_proxy_unfocus` tool).
///
/// Why: the "back to fleet chat" action — drop the binding so free text no
/// longer injects.
/// What: clears the focus and reports the cleared session (or `null`); never an
/// error — unfocusing an unfocused conversation is a harmless no-op.
/// Test: `tests::mcp_proxy_focus_message_summary_unfocus_round_trip`.
pub async fn session_proxy_unfocus(
    state: &Arc<DaemonState>,
    conversation_key: &str,
) -> Result<Value, String> {
    let proxy = local_proxy(state);
    let cleared = proxy.unfocus(conversation_key).map(Into::into);
    to_result(ProxyUnfocusResponse { cleared })
}

/// INJECT free text into the conversation's focused session
/// (`session_proxy_message` tool).
///
/// Why: the payoff of focus — a plain message becomes a `send` at the focused
/// session, addressed by focus instead of an explicit id.
/// What: injects `text` and returns the tagged [`ProxyMessageResponse`]
/// (`sent` / `auto_unfocused` / `failed` / `no_focus`).
/// Test: `tests::mcp_proxy_focus_message_summary_unfocus_round_trip`,
/// `tests::mcp_proxy_message_no_focus`.
pub async fn session_proxy_message(
    state: &Arc<DaemonState>,
    conversation_key: &str,
    text: &str,
) -> Result<Value, String> {
    let proxy = local_proxy(state);
    let outcome = proxy.inject(conversation_key, text).await;
    to_result(ProxyMessageResponse::from(outcome))
}

/// SUMMARIZE the conversation's focused session (`session_proxy_summary` tool).
///
/// Why: the second proxy direction — "what is my focused session doing?" —
/// without attaching to tmux.
/// What: returns a lightweight activity digest as the tagged
/// [`ProxySummaryResponse`] (`summary` / `auto_unfocused` / `failed` /
/// `no_focus`).
/// Test: `tests::mcp_proxy_focus_message_summary_unfocus_round_trip`.
pub async fn session_proxy_summary(
    state: &Arc<DaemonState>,
    conversation_key: &str,
) -> Result<Value, String> {
    let proxy = local_proxy(state);
    let outcome = proxy.summarize(conversation_key).await;
    to_result(ProxySummaryResponse::from(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::runtime::RuntimeKind;
    use crate::session_manager::record::ManagedSessionId;

    /// Build a hermetic `DaemonState` with one seeded managed session.
    ///
    /// Why: every proxy test needs a resolvable session to focus; mirrors
    /// `managed_routes::proxy::tests::seeded_state` (a separate module) so the
    /// MCP-layer wrappers are exercised over the SAME fake-tmux, isolated store.
    /// What: returns the state and the seeded session's friendly `tmux_name`.
    async fn seeded_state() -> (Arc<DaemonState>, String) {
        let root = tempfile::tempdir().unwrap().keep();
        let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);
        let mgr = state.session_manager().await;
        let record = mgr
            .create_with_id(
                ManagedSessionId::new(),
                "mcp proxy task".to_string(),
                None,
                Some("mcpproxy".to_string()),
                None,
                None,
                None,
                RuntimeKind::default(),
                false,
                false,
            )
            .await
            .expect("seed session");
        (state, record.tmux_name)
    }

    #[tokio::test]
    async fn mcp_proxy_focus_message_summary_unfocus_round_trip() {
        // Proves each MCP tool reaches the REAL SessionProxy over the direct
        // backend + shared focus store: focus by name → inject → summarize →
        // unfocus, asserting the tagged outcome at every step.
        let (state, name) = seeded_state().await;

        let focus = session_proxy_focus(&state, "conv-1", &name).await.unwrap();
        assert_eq!(focus["outcome"], "focused");
        assert_eq!(focus["name"], name);

        let msg = session_proxy_message(&state, "conv-1", "run the tests")
            .await
            .unwrap();
        assert_eq!(msg["outcome"], "sent");
        assert_eq!(msg["target"]["name"], name);
        assert_eq!(msg["text"], "run the tests");

        let summary = session_proxy_summary(&state, "conv-1").await.unwrap();
        assert_eq!(summary["outcome"], "summary");
        assert_eq!(summary["target"]["name"], name);
        assert!(
            summary["summary"]
                .as_str()
                .unwrap()
                .contains("mcp proxy task")
        );

        let cleared = session_proxy_unfocus(&state, "conv-1").await.unwrap();
        assert_eq!(cleared["cleared"]["name"], name);

        // Now unfocused: message reports no_focus, never an error.
        let after = session_proxy_message(&state, "conv-1", "still there?")
            .await
            .unwrap();
        assert_eq!(after["outcome"], "no_focus");
    }

    #[tokio::test]
    async fn mcp_proxy_focus_empty_queries_current() {
        // An empty session_id is a read-only query, not a set.
        let (state, _name) = seeded_state().await;
        let body = session_proxy_focus(&state, "conv-2", "").await.unwrap();
        assert_eq!(body["outcome"], "current");
        assert!(body["target"].is_null());
    }

    #[tokio::test]
    async fn mcp_proxy_focus_shares_store_with_http_surface() {
        // The MCP focus is keyed into DaemonState::proxy_focus_store — the SAME
        // store the HTTP/Telegram surfaces use. Confirm a focus set via the MCP
        // wrapper is observable through the shared store under the same key.
        let (state, name) = seeded_state().await;
        session_proxy_focus(&state, "conv-shared", &name)
            .await
            .unwrap();
        let store = state.proxy_focus_store();
        let guard = store.lock().expect("focus store lock");
        let focused = guard.get("conv-shared").expect("focus present in store");
        assert_eq!(focused.name, name);
    }

    #[tokio::test]
    async fn mcp_proxy_message_no_focus() {
        let (state, _name) = seeded_state().await;
        let body = session_proxy_message(&state, "conv-never", "hello")
            .await
            .unwrap();
        assert_eq!(body["outcome"], "no_focus");
    }
}
