//! The daemon's Slack Socket Mode inbound source (#9923, #8433 P6).
//!
//! Why: only the daemon may hold the trusty app's socket and turn an owner's
//! DM reply into `inbound_op` (P5). Slack redelivers an envelope that was
//! not acked, so each event id is claimed durably before the ack and before
//! any effect, and a claimed id is acked and dropped (Q4). This is a
//! Fail-Open Check site: a step that cannot claim acks nothing and acts on
//! nothing; no arm logs and goes on.
//! What: [`run`] holds no socket while [`SocketEnv::ready`] refuses (Q8),
//! else connects through a [`transport::SocketTransport`] and reads frames;
//! a dropped socket reconnects after a doubling [`Backoff`]. Each
//! `events_api` frame goes through [`handle_event`] in the fixed order:
//! ready, claim, ack, then, for a fresh claim, the effect and its settle.
//! Nothing here touches the question table, so a lost socket closes and
//! denies nothing (plan item 7).
//! Test: `socket_mode_tests.rs`.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::channels::inbound::InboundEvent;
use crate::channels::question_table::claims::Claim;

// #9923 Q7: the daemon's own frame parser, not the `tm slack` product's.
pub mod envelope;
// #9923 plan item 2: the app token through `app_ref` only.
pub mod token;
// #9923 Q6: the transport seam; the production one reaches Slack only.
pub mod transport;

use envelope::{EventEnvelope, Frame, parse_frame};
use transport::{SocketConn, SocketTransport};

/// What the loop reads and writes outside itself.
///
/// Why: each step is a Fail-Open Check arm a test must force.
/// What: the daemon's is `DaemonSocketEnv`
/// (`daemon::managed_routes::socket_mode`).
#[async_trait::async_trait]
pub trait SocketEnv: Send + Sync {
    /// `Ok` when the daemon may hold the socket and claim: the policy
    /// snapshot serves, Slack names an `app_ref`, the claim store opens.
    async fn ready(&self) -> Result<(), String>;
    /// Claim `event_id` durably; see `ClaimStore::claim`.
    async fn claim(&self, event_id: &str) -> Result<Claim, String>;
    /// Record that `event_id`'s effect ended with `outcome`.
    async fn settle(&self, event_id: &str, outcome: &str) -> Result<(), String>;
    /// The effect: one owner reply through `inbound_op`; its outcome code.
    async fn act(&self, ev: InboundEvent) -> String;
}

/// The loop's waits.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Between two readiness checks while not ready.
    pub poll: Duration,
    /// The first reconnect wait.
    pub backoff_min: Duration,
    /// The longest reconnect wait.
    pub backoff_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(15),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
        }
    }
}

/// A doubling reconnect wait, reset by a `hello`.
#[derive(Debug, Clone)]
pub struct Backoff {
    timing: Timing,
    next: Duration,
}

impl Backoff {
    /// A backoff starting at `timing.backoff_min`.
    pub fn new(timing: Timing) -> Self {
        Self {
            timing,
            next: timing.backoff_min,
        }
    }

    /// The wait to use now; doubles the next, up to `backoff_max`.
    pub fn next_wait(&mut self) -> Duration {
        let wait = self.next;
        self.next = (self.next * 2).min(self.timing.backoff_max);
        wait
    }

    /// Back to `backoff_min`: a connection came up.
    pub fn reset(&mut self) {
        self.next = self.timing.backoff_min;
    }
}

/// Hold the socket while ready; reconnect with backoff; stop on `cancel`.
///
/// Why: see the module doc. A not-ready daemon opens no socket (Q8).
/// What: each round checks [`SocketEnv::ready`] (refused: wait
/// `timing.poll`), connects (failed: wait the backoff), then serves the
/// connection until it closes, errs, or Slack sends `disconnect`, and waits
/// the backoff before the next round.
/// Test: `socket_mode_source_absent_when_policy_not_ready`,
/// `socket_loss_backs_off_and_denies_nothing_open`.
pub async fn run(
    env: Arc<dyn SocketEnv>,
    transport: Arc<dyn SocketTransport>,
    cancel: CancellationToken,
    timing: Timing,
) {
    let mut backoff = Backoff::new(timing);
    let mut last_refusal: Option<String> = None;
    loop {
        if cancel.is_cancelled() {
            return;
        }
        if let Err(why) = env.ready().await {
            if last_refusal.as_deref() != Some(why.as_str()) {
                tracing::info!(%why, "socket mode: not ready; no socket held");
                last_refusal = Some(why);
            }
            if !pause(&cancel, timing.poll).await {
                return;
            }
            continue;
        }
        last_refusal = None;
        let connected = tokio::select! {
            () = cancel.cancelled() => return,
            conn = transport.connect() => conn,
        };
        match connected {
            Ok(mut conn) => {
                let Some(why) = serve(&*env, &mut *conn, &cancel, &mut backoff).await else {
                    return;
                };
                tracing::warn!(%why, "socket mode: connection ended; reconnecting");
            }
            Err(why) => tracing::warn!(%why, "socket mode: connect failed"),
        }
        if !pause(&cancel, backoff.next_wait()).await {
            return;
        }
    }
}

/// Read `conn` until it ends; `None` when cancelled, else why it ended.
async fn serve(
    env: &dyn SocketEnv,
    conn: &mut dyn SocketConn,
    cancel: &CancellationToken,
    backoff: &mut Backoff,
) -> Option<String> {
    loop {
        let next = tokio::select! {
            () = cancel.cancelled() => return None,
            next = conn.recv() => next,
        };
        let raw = match next {
            None => return Some("socket closed".to_owned()),
            Some(Err(why)) => return Some(why),
            Some(Ok(raw)) => raw,
        };
        match parse_frame(&raw) {
            Frame::Hello => backoff.reset(),
            Frame::Disconnect { reason } => return Some(format!("disconnect: {reason}")),
            // #9923: never claimed, so never acked (plan item 3).
            Frame::Other { kind } => tracing::debug!(kind, "socket mode: frame not acted on"),
            Frame::Event(event) => handle_event(env, conn, event).await,
        }
    }
}

/// One `events_api` envelope, in the fixed order: ready, claim, ack,
/// effect, settle.
///
/// Why: plan items 3-6. The claim is the dedup and must be durable before
/// Slack is told the envelope arrived; only a fresh claim acts.
/// What: no `event_id`, a not-ready env (Q8) or a claim error: no ack, no
/// effect (Slack redelivers). Else the ack; a failed ack is logged and the
/// claim stands, so the redelivery is acked and dropped. A `Duplicate`
/// stops there. A `Fresh` claim runs the effect (an owner DM message
/// through [`SocketEnv::act`], anything else `ignored`), then the settle;
/// a failed settle is logged, and `tm doctor` lists the id as claimed and
/// never done.
/// Test: `claim_precedes_ack_in_event_order`,
/// `retried_envelope_is_acked_and_not_redelivered`,
/// `event_id_claimed_by_daemon_before_any_effect`,
/// `claim_store_write_failure_acks_nothing_and_acts_on_nothing`.
pub async fn handle_event(env: &dyn SocketEnv, conn: &mut dyn SocketConn, event: EventEnvelope) {
    let Some(event_id) = event.event_id.as_deref() else {
        tracing::warn!("socket mode: events_api envelope without an event_id; not acked");
        return;
    };
    // #9923 Q8: not ready: no claim, no ack; Slack retries.
    if let Err(why) = env.ready().await {
        tracing::warn!(%why, "socket mode: not ready; event neither claimed nor acked");
        return;
    }
    // #9923 (plan item 6): a claim that is not durable acks and acts on nothing.
    let claim = match env.claim(event_id).await {
        Ok(claim) => claim,
        Err(why) => {
            tracing::error!(%why, "socket mode: claim not written; event not acked");
            return;
        }
    };
    if let Err(why) = conn.ack(&event.envelope_id).await {
        tracing::warn!(%why, "socket mode: ack failed; the claim stands");
    }
    if claim == Claim::Duplicate {
        tracing::info!(
            retry_attempt = event.retry_attempt,
            "socket mode: event already claimed; acked, nothing done"
        );
        return;
    }
    let outcome = match event.message {
        Some(ev) => env.act(ev).await,
        None => "ignored".to_owned(),
    };
    if let Err(why) = env.settle(event_id, &outcome).await {
        tracing::error!(%why, "socket mode: settle not written; doctor lists the claim");
    }
}

/// Wait `wait` unless cancelled first; `false` when cancelled.
async fn pause(cancel: &CancellationToken, wait: Duration) -> bool {
    tokio::select! {
        () = cancel.cancelled() => false,
        () = tokio::time::sleep(wait) => true,
    }
}

#[cfg(test)]
#[path = "socket_mode_tests.rs"]
mod tests;
