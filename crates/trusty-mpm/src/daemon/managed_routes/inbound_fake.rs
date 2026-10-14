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
use serde_json::{Value, json};
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

/// RED STUB (#9873 S7): serves whatever `sandboxed` says and always answers
/// 200; the next commit replaces it.
pub fn router(sandboxed: bool) -> Router<Arc<DaemonState>> {
    let _ = (sandboxed, inbound_op, reply, Channel::Slack);
    Router::new().route(
        INBOUND_FAKE_ROUTE,
        post(|Json(req): Json<FakeInbound>| async move {
            let _ = InboundEvent {
                channel: Channel::Slack,
                sender: req.sender,
                conversation: Conversation {
                    dm_channel_id: req.dm_channel_id,
                    thread_ts: req.thread_ts,
                },
                text: req.text,
            };
            let _: Option<State<Arc<DaemonState>>> = None;
            (StatusCode::OK, Json::<Value>(json!({})))
        }),
    )
}

#[cfg(test)]
#[path = "inbound_fake_tests.rs"]
mod tests;
