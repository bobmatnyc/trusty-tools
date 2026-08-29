//! Grounded conversational Q&A, served as a JSON-RPC method (#6285 slice 5.6).
//!
//! Why: slice 5.5 left `POST /chat` HTTP-only on the reading that chat serves
//! the embedded `/ui` alone. #6155 moves that UI into `trusty-console`, which
//! reaches this daemon over the socket, so the route needs a method here before
//! the retire slice deletes it — otherwise the migrated UI loses its chat panel
//! and the surface-removal gate has to wait for a method nobody scheduled.
//!
//! What: [`METHOD_CHAT`], and [`register`]. The handler is a thin adapter over
//! [`chat_report`] in `service::ui` — this file decides WHICH method exists and
//! what it decodes, never what it does.
//!
//! ## Method → route
//!
//! | Method | HTTP route | Lane |
//! |---|---|---|
//! | `search.chat` | `POST /chat` | free |
//!
//! ## This is a unary method, and that is the route's shape rather than a
//! simplification
//!
//! `POST /chat` streams from the provider INTERNALLY and collects the deltas
//! into one JSON envelope before it answers; the SSE pair slice 5 moved
//! (`search.status.stream`, `search.index.reindex.stream`) are the routes that
//! stream to the CALLER. Registering this one into the router's streaming table
//! would give the socket a shape HTTP has never had, and the two doors would
//! stop answering the same thing — which is the property the whole surface is
//! built to keep. When the route itself learns to stream deltas, both transports
//! move together.
//!
//! ## What guards this method
//!
//! Everything slice 2's `reads` module documents — the peer-uid check over a
//! `0600` socket in a `0700` directory. A caller may supply its own `api_key` in
//! the params, exactly as the HTTP body accepts one; a peer that reached this
//! socket has the daemon's own uid and could read that key from the environment
//! regardless.
//!
//! Test: `chat_tests.rs`.
//!
//! [`chat_report`]: crate::service::ui::chat_report
//! [`register`]: crate::service::rpc::chat::register
//! [`METHOD_CHAT`]: crate::service::rpc::chat::METHOD_CHAT

use std::sync::Arc;

use trusty_common::uds::server::RpcRouter;

use crate::service::server::SearchAppState;
use crate::service::ui::ChatRequest;

use super::error::rpc_error_from_http;

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;

/// `POST /chat` — answer one question about an index, grounded in a search.
pub const METHOD_CHAT: &str = "search.chat";

/// Every method this slice registers, in registration order.
///
/// The same contract the other families carry — `service::socket::METHODS`
/// splices this in by reference rather than restating the literal, so a rename
/// is a compile error there rather than a drift only a running consumer finds.
/// Test: `rpc_router_registers_every_documented_method` and
/// `every_family_method_is_spliced_into_the_socket_method_list` in
/// `socket_tests.rs`.
pub const METHODS: &[&str] = &[METHOD_CHAT];

/// Mount [`METHOD_CHAT`] onto `router`.
///
/// Why: the route-specific half of `POST /chat`, kept beside the other families
/// so each slice adds one file rather than editing one.
/// What: one `typed` registration decoding [`ChatRequest`] — byte-for-byte the
/// JSON the HTTP route's extractor takes, so `question`/`message` aliasing and
/// every serde default behave identically — and running [`chat_report`], the
/// core the axum handler wraps.
/// Test: `chat_without_a_provider_is_refused_identically_on_both_transports`
/// and its refusal siblings in `chat_tests.rs`.
///
/// [`chat_report`]: crate::service::ui::chat_report
pub fn register(router: RpcRouter, state: &Arc<SearchAppState>) -> RpcRouter {
    let held = Arc::clone(state);
    router.typed::<ChatRequest, serde_json::Value, _, _>(METHOD_CHAT, move |req| {
        let state = Arc::clone(&held);
        async move {
            crate::service::ui::chat_report(&state, req)
                .await
                .map_err(|(status, body)| rpc_error_from_http(status, &body))
        }
    })
}
