//! `POST /api/v1/channels/inbound-fake`: a sandbox-only inbound source
//! (#9873 Q1, #8433 P5).
//!
//! Why: P6 (Socket Mode) is the real inbound source and is not built. The
//! resolve and second-resolve live checks need a way to feed one event to
//! the daemon. A production route would let any local caller forge an
//! owner's reply into a resolve, a taint and a pane write, so this route
//! exists only in a daemon whose sandbox latch is set (`tm daemon --sandbox`,
//! #9121), and that latch is one-way.
//! What: [`router`] is empty unless `sandboxed`; then it serves
//! [`INBOUND_FAKE_ROUTE`], whose body is a [`FakeInbound`]. The event runs
//! through `managed_routes::inbound::inbound_op` and its status map.
//! Test: `inbound_fake_route_is_absent_outside_the_sandbox`,
//! `inbound_fake_route_status_map`, `inbound_route_has_no_production_source`.

use std::sync::Arc;

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde::Deserialize;
use serde_json::json;
use trusty_channels::policy::Channel;

use super::inbound::{inbound_op, reply};
use crate::channels::inbound::InboundEvent;
use crate::channels::question_table::Conversation;
use crate::daemon::state::DaemonState;

/// The sandbox-only inbound route.
pub const INBOUND_FAKE_ROUTE: &str = "/api/v1/channels/inbound-fake";

/// One fake inbound event, as a P6 source would build it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FakeInbound {
    /// `slack` or `telegram`.
    pub channel: String,
    /// The sender (a Slack `U…` id).
    pub sender: String,
    /// The DM channel the reply was posted in.
    pub dm_channel_id: String,
    /// The thread the reply was posted in.
    pub thread_ts: String,
    /// The reply text.
    pub text: String,
}

/// The sandbox-only sub-router; empty unless `sandboxed`.
///
/// Why: see the module doc. The latch is a parameter so a test can build
/// both routers without setting the process-wide latch under its siblings;
/// `channels_router` passes `secret_source::sandboxed()`.
/// What: a served channel name and an [`InboundEvent`] built from the body,
/// then `inbound_op` and its `reply`. An unknown channel is a 422
/// `channel_not_served`; a body with a missing or unknown field is axum's
/// JSON rejection.
/// Test: `inbound_fake_route_is_absent_outside_the_sandbox`,
/// `inbound_fake_route_status_map`.
pub fn router(sandboxed: bool) -> Router<Arc<DaemonState>> {
    if !sandboxed {
        return Router::new();
    }
    Router::new().route(
        INBOUND_FAKE_ROUTE,
        post(
            |State(state): State<Arc<DaemonState>>, Json(req): Json<FakeInbound>| async move {
                let Some(channel) = served_channel(&req.channel) else {
                    return (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        Json(json!({ "error": "channel_not_served", "resolved": false })),
                    );
                };
                let ev = InboundEvent {
                    channel,
                    sender: req.sender,
                    conversation: Conversation {
                        dm_channel_id: req.dm_channel_id,
                        thread_ts: req.thread_ts,
                    },
                    text: req.text,
                };
                reply(&inbound_op(&state, ev).await)
            },
        ),
    )
}

/// The daemon-served channel named `name`; gchat-mcp serves gchat.
fn served_channel(name: &str) -> Option<Channel> {
    [Channel::Slack, Channel::Telegram]
        .into_iter()
        .find(|c| c.as_str() == name)
}

#[cfg(test)]
#[path = "inbound_fake_tests.rs"]
mod tests;
