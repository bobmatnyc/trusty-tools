//! The L2 session-proxy routes as JSON-RPC methods (#6288 slice 4).
//!
//! Why: `managed_routes::proxy` is the focus/inject/summarize state machine
//! every channel binds to, and ADR-0032 moves its transport onto the socket.
//! Nothing here re-implements it — each method calls the same `*_core` body the
//! axum handler calls.
//!
//! What: five methods. The two GET routes carry their path segment as a
//! `conversation_key` field instead, since a socket request has no path.
//!
//! | Method | Route |
//! |---|---|
//! | `mpm.proxy.focus` | `POST /api/v1/sessions/proxy/focus` |
//! | `mpm.proxy.get_focus` | `GET /api/v1/sessions/proxy/focus/{conversation_key}` |
//! | `mpm.proxy.unfocus` | `POST /api/v1/sessions/proxy/unfocus` |
//! | `mpm.proxy.message` | `POST /api/v1/sessions/proxy/message` |
//! | `mpm.proxy.summary` | `GET /api/v1/sessions/proxy/summary/{conversation_key}` |
//!
//! Test: `proxy_*_parity` in `daemon::rpc::managed_tests`.

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::RpcRouter;

use crate::daemon::managed_routes::proxy;
use crate::daemon::state::DaemonState;

/// A path-only request: the conversation key the HTTP route reads from its URL.
#[derive(Debug, Deserialize)]
pub struct ConversationKeyParams {
    /// The conversation whose focus (or summary) is being read.
    pub conversation_key: String,
}

/// Register the five proxy methods on `router`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let focus_state = Arc::clone(state);
    let get_focus_state = Arc::clone(state);
    let unfocus_state = Arc::clone(state);
    let message_state = Arc::clone(state);
    let summary_state = Arc::clone(state);

    router
        .typed("mpm.proxy.focus", move |req: proxy::ProxyFocusRequest| {
            let state = Arc::clone(&focus_state);
            async move { proxy::focus_core(&state, req).await.into_rpc() }
        })
        .typed("mpm.proxy.get_focus", move |req: ConversationKeyParams| {
            let state = Arc::clone(&get_focus_state);
            async move {
                proxy::get_focus_core(&state, &req.conversation_key)
                    .await
                    .into_rpc()
            }
        })
        .typed(
            "mpm.proxy.unfocus",
            move |req: proxy::ProxyUnfocusRequest| {
                let state = Arc::clone(&unfocus_state);
                async move { proxy::unfocus_core(&state, req).await.into_rpc() }
            },
        )
        .typed(
            "mpm.proxy.message",
            move |req: proxy::ProxyMessageRequest| {
                let state = Arc::clone(&message_state);
                async move { proxy::message_core(&state, req).await.into_rpc() }
            },
        )
        .typed("mpm.proxy.summary", move |req: ConversationKeyParams| {
            let state = Arc::clone(&summary_state);
            async move {
                proxy::summary_core(&state, &req.conversation_key)
                    .await
                    .into_rpc()
            }
        })
}
