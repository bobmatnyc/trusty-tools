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
//! In [`run`] the reader does ready, claim and ack, and one effect task
//! runs the effects in arrival order, each bounded by [`ACT_TIMEOUT`], so a
//! hung effect never stops the reader (#9923). Nothing here touches the
//! question table, so a lost socket closes and denies nothing (plan item 7).
//! Test: `socket_mode_tests.rs`.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::channels::inbound::InboundEvent;
use crate::channels::question_table::claims::{ACT_TIMED_OUT, Claim};

// #9923 Q7: the daemon's own frame parser, not the `tm slack` product's.
pub mod envelope;
// #9923 plan item 2: the app token through `app_ref` only.
pub mod token;
// #9923 Q6: the transport seam; the production one reaches Slack only.
pub mod transport;

use envelope::{EventEnvelope, Frame, parse_frame};
use transport::{SocketConn, SocketTransport};

/// The longest one effect may run before its claim is settled
/// [`ACT_TIMED_OUT`] (#9923).
///
/// Why: an `inbound_op` that never returns must not hold its claim open for
/// ever, and the effect task must move on to the next answer. Slack's
/// 3-second ack deadline does not bound it: the ack is sent after the claim
/// and before the effect. A retry Slack sends anyway (about 1 and 5 minutes
/// after the first try) finds the claim and is acked and dropped, in flight
/// or timed out, so the value only trades a slow effect against a stuck
/// queue. A normal effect (one table fsync, one pane write and its
/// confirm) takes seconds. 60 s is below `tm doctor`'s 2-minute
/// claimed-never-done age, so an effect still inside its bound is never
/// listed as lost.
/// What: [`run_effect`] wraps [`SocketEnv::act`] in `tokio::time::timeout`
/// with this bound; on expiry the effect future is dropped and the claim is
/// settled [`ACT_TIMED_OUT`]. A decision already on the blocking pool runs
/// to its end, so the question may be resolved with no delivery, which
/// `tm doctor` reports (Q4a).
/// Test: `hung_effect_does_not_stop_the_reader_and_settles_as_timed_out`,
/// `retry_of_a_timed_out_event_is_acked_and_not_acted_again`.
pub const ACT_TIMEOUT: Duration = Duration::from_secs(60);

/// How many claimed events may wait for the effect task (#9923).
///
/// What: a full queue claims and acks nothing, so Slack redelivers the
/// event; owner replies arrive far slower than one per [`ACT_TIMEOUT`].
pub const EFFECT_QUEUE: usize = 16;

/// A claimed and acked event whose effect has not run yet.
#[derive(Debug)]
struct Effect {
    event_id: String,
    message: Option<InboundEvent>,
}

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
/// the backoff before the next round. The effects run on one task of their
/// own, which outlives a reconnect and stops on `cancel` (a queued effect
/// is then never settled, as after a crash, and `tm doctor` lists it).
/// Test: `socket_mode_source_absent_when_policy_not_ready`,
/// `socket_loss_backs_off_and_denies_nothing_open`,
/// `hung_effect_does_not_stop_the_reader_and_settles_as_timed_out`.
pub async fn run(
    env: Arc<dyn SocketEnv>,
    transport: Arc<dyn SocketTransport>,
    cancel: CancellationToken,
    timing: Timing,
) {
    // #9923: the reader claims and acks; the effects run off it, in order.
    let (effects, queue) = mpsc::channel(EFFECT_QUEUE);
    tokio::spawn(effect_task(Arc::clone(&env), queue, cancel.clone()));
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
                let served = serve(&*env, &mut *conn, &effects, &cancel, &mut backoff).await;
                let Some(why) = served else {
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

/// Run queued effects in arrival order until the queue closes or `cancel`.
async fn effect_task(
    env: Arc<dyn SocketEnv>,
    mut queue: mpsc::Receiver<Effect>,
    cancel: CancellationToken,
) {
    loop {
        let effect = tokio::select! {
            () = cancel.cancelled() => return,
            next = queue.recv() => match next {
                Some(effect) => effect,
                None => return,
            },
        };
        run_effect(&*env, effect).await;
    }
}

/// Read `conn` until it ends; `None` when cancelled, else why it ended.
///
/// What: an event is admitted (ready, claim, ack) here and its effect is
/// queued on `effects`. A full queue claims and acks nothing (#9923).
async fn serve(
    env: &dyn SocketEnv,
    conn: &mut dyn SocketConn,
    effects: &mpsc::Sender<Effect>,
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
            Frame::Event(event) => {
                // #9923: no queue slot: not claimed, not acked; Slack retries.
                let Ok(slot) = effects.try_reserve() else {
                    tracing::warn!(
                        "socket mode: no effect slot (queue full or effect task gone); \
                         event neither claimed nor acked"
                    );
                    continue;
                };
                if let Some(effect) = admit(env, conn, event).await {
                    slot.send(effect);
                }
            }
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
/// through [`SocketEnv::act`] within [`ACT_TIMEOUT`], anything else
/// `ignored`), then the settle; a failed settle is logged, and `tm doctor`
/// lists the id as claimed and never done. [`run`] does the same two halves
/// on two tasks.
/// Test: `claim_precedes_ack_in_event_order`,
/// `retried_envelope_is_acked_and_not_redelivered`,
/// `event_id_claimed_by_daemon_before_any_effect`,
/// `claim_store_write_failure_acks_nothing_and_acts_on_nothing`,
/// `retry_of_a_timed_out_event_is_acked_and_not_acted_again`.
pub async fn handle_event(env: &dyn SocketEnv, conn: &mut dyn SocketConn, event: EventEnvelope) {
    if let Some(effect) = admit(env, conn, event).await {
        run_effect(env, effect).await;
    }
}

/// Ready, claim, ack; the effect still to run for a `Fresh` claim.
async fn admit(
    env: &dyn SocketEnv,
    conn: &mut dyn SocketConn,
    event: EventEnvelope,
) -> Option<Effect> {
    let Some(event_id) = event.event_id else {
        tracing::warn!("socket mode: events_api envelope without an event_id; not acked");
        return None;
    };
    // #9923 Q8: not ready: no claim, no ack; Slack retries.
    if let Err(why) = env.ready().await {
        tracing::warn!(%why, "socket mode: not ready; event neither claimed nor acked");
        return None;
    }
    // #9923 (plan item 6): a claim that is not durable acks and acts on nothing.
    let claim = match env.claim(&event_id).await {
        Ok(claim) => claim,
        Err(why) => {
            tracing::error!(%why, "socket mode: claim not written; event not acked");
            return None;
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
        return None;
    }
    Some(Effect {
        event_id,
        message: event.message,
    })
}

/// The effect of one fresh claim, bounded by [`ACT_TIMEOUT`], then its
/// settle.
async fn run_effect(env: &dyn SocketEnv, effect: Effect) {
    let outcome = match effect.message {
        // #9923: a hung `inbound_op` is dropped and its claim settled as lost.
        Some(ev) => match tokio::time::timeout(ACT_TIMEOUT, env.act(ev)).await {
            Ok(outcome) => outcome,
            Err(_) => {
                tracing::error!("socket mode: effect did not finish in time; settled as timed out");
                ACT_TIMED_OUT.to_owned()
            }
        },
        None => "ignored".to_owned(),
    };
    if let Err(why) = env.settle(&effect.event_id, &outcome).await {
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
