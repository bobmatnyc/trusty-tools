//! The daemon side of the Socket Mode inbound source (#9923, #8433 P6).
//!
//! Why: the loop in `channels::socket_mode` is the only production caller
//! of [`inbound_op`] (plan item 8); this file gives it the daemon's policy
//! snapshot, claim log and `inbound_op`. No HTTP route is added.
//! What: [`spawn`] starts `channels::socket_mode::run` over
//! [`DaemonSocketEnv`] and the transport `transport_for` picks (the
//! loopback fake only behind the sandbox latch, Q6). The env's `ready` is
//! the snapshot serving, the host naming a Slack `app_ref`, and the claim
//! log opening (its open prunes, Q5); claims and settles run on the
//! blocking pool; the effect is `inbound_op`, settled with its code.
//! Test: `daemon_socket_env_is_not_ready_without_policy`,
//! `daemon_socket_env_claims_once_across_a_reopen`,
//! `inbound_route_has_no_production_source`.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::inbound::{InboundReply, inbound_op};
use crate::channels::inbound::{Dropped, InboundEvent, snapshot_code};
use crate::channels::question_table::claims::Claim;
use crate::channels::socket_mode::transport::transport_for;
use crate::channels::socket_mode::{SocketEnv, Timing, run, token};
use crate::daemon::state::DaemonState;

/// The daemon's [`SocketEnv`].
pub struct DaemonSocketEnv {
    state: Arc<DaemonState>,
}

impl DaemonSocketEnv {
    /// The env over `state`.
    pub fn new(state: Arc<DaemonState>) -> Self {
        Self { state }
    }
}

#[async_trait::async_trait]
impl SocketEnv for DaemonSocketEnv {
    async fn ready(&self) -> Result<(), String> {
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || {
            let home = state.user_home();
            // #9923 plan item 2: the host names the socket's token by role.
            token::app_ref(home.as_deref())?;
            // #9923 Q8: a snapshot that cannot serve claims nothing.
            state
                .channel_policy()
                .current()
                .map_err(|deny| snapshot_code(deny).to_owned())?;
            state.inbound_claims().map(drop).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("the readiness task did not finish: {e}"))?
    }

    async fn claim(&self, event_id: &str) -> Result<Claim, String> {
        let (state, id) = (Arc::clone(&self.state), event_id.to_owned());
        tokio::task::spawn_blocking(move || state.inbound_claims()?.claim(&id))
            .await
            .map_err(|e| format!("the claim task did not finish: {e}"))?
            .map_err(|e| e.to_string())
    }

    async fn settle(&self, event_id: &str, outcome: &str) -> Result<(), String> {
        let (state, id, outcome) = (
            Arc::clone(&self.state),
            event_id.to_owned(),
            outcome.to_owned(),
        );
        tokio::task::spawn_blocking(move || state.inbound_claims()?.settle(&id, &outcome))
            .await
            .map_err(|e| format!("the settle task did not finish: {e}"))?
            .map_err(|e| e.to_string())
    }

    async fn act(&self, ev: InboundEvent) -> String {
        outcome_code(&inbound_op(&self.state, ev).await).to_owned()
    }
}

/// The claim log's outcome code for one inbound reply.
fn outcome_code(reply: &InboundReply) -> &'static str {
    match reply {
        InboundReply::Delivered { .. } => "delivered",
        InboundReply::Dropped(Dropped { code, .. }) => code,
        InboundReply::DeliveryFailed { code, .. } => code,
        InboundReply::Aborted(_) => "inbound_aborted",
    }
}

/// Start the Socket Mode loop for `state` until `cancel`.
///
/// Why: plan item 1: the daemon alone holds the trusty app's socket.
/// What: one task; it holds no socket while the env is not ready.
/// Test: `socket_mode_source_absent_when_policy_not_ready`.
pub fn spawn(state: Arc<DaemonState>, cancel: CancellationToken) {
    let transport = transport_for(crate::secret_source::sandboxed(), state.user_home());
    let env = Arc::new(DaemonSocketEnv::new(state));
    tokio::spawn(run(env, transport, cancel, Timing::default()));
}

#[cfg(test)]
#[path = "socket_mode_tests.rs"]
mod tests;
