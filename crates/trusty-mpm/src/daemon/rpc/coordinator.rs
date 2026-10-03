//! The coordinator's context and chat routes as JSON-RPC methods (#6288
//! step 2a).
//!
//! Why: the TUI coordinator polls the context snapshot and posts chat turns,
//! and slices 2-5 left both on HTTP only. Step 2c removes the TCP bind, so
//! both need a socket method before any client moves (step 2b).
//!
//! What: two methods, each calling the SAME body its HTTP handler calls.
//!
//! | Method | HTTP route |
//! |---|---|
//! | `mpm.sessions.context` | `GET /api/v1/sessions/context`, alias `GET /api/v1/session-manager/context` |
//! | `mpm.sessions.chat` | `POST /api/v1/sessions/chat`, alias `POST /api/v1/session-manager/chat` |
//!
//! The chat route's origin guard does not travel. It is browser-CSRF defence
//! for a listener a page can reach; the socket admits only a same-uid peer,
//! checked before a byte is read, which is the stronger guard (`daemon::socket`).
//! Errors cross as `DaemonError`'s own RPC code, derived from the status HTTP
//! answers: `503` becomes `CODE_UNAVAILABLE`, `404` `CODE_NOT_FOUND`.
//!
//! Test: `coordinator_tests.rs`.

use std::sync::Arc;

use trusty_common::uds::server::{RpcError, RpcRouter};

use crate::daemon::api::coordinator_routes::{
    CoordinatorChatRequest, CoordinatorChatResponse, coordinator_chat_op,
};
use crate::daemon::coordinator::{CoordinatorContext, build_coordinator_context};
use crate::daemon::rpc::registry::NoParams;
use crate::daemon::state::DaemonState;

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;

/// Every method this module registers.
///
/// Test: `coordinator_methods_are_registered`.
pub const METHODS: &[&str] = &["mpm.sessions.chat", "mpm.sessions.context"];

/// Mount both coordinator methods on `router`.
///
/// Test: `coordinator_methods_are_registered`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let context_state = Arc::clone(state);
    let chat_state = Arc::clone(state);
    router
        .typed::<NoParams, CoordinatorContext, _, _>("mpm.sessions.context", move |_| {
            let state = Arc::clone(&context_state);
            async move { Ok(build_coordinator_context(&state)) }
        })
        .typed::<CoordinatorChatRequest, CoordinatorChatResponse, _, _>(
            "mpm.sessions.chat",
            move |body| {
                let state = Arc::clone(&chat_state);
                async move {
                    coordinator_chat_op(&state, body)
                        .await
                        .map_err(RpcError::from)
                }
            },
        )
}
