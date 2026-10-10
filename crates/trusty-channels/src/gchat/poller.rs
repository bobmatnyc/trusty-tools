//! The Pub/Sub poller: the one consumer of a project's subscription.
//!
//! Why: one consumer per subscription (#9448, Architect ruling). The running
//! `gchat-mcp` server holds the project's single [`GchatChannel`] and only
//! it polls, so a reply is bound by the process that holds the ledger.
//! What: [`Poller::tick`] runs one [`GchatChannel::poll_once`] and records
//! the outcome in a shared [`PollStatus`] that `gchat_doctor` reads.
//! [`Poller::run`] ticks once per message on a tick channel, so a test
//! drives ticks by hand and production feeds it from [`interval_ticks`].
//! A failure is logged (to stderr, via tracing) when its text changes, so a
//! standing fault is reported once rather than every tick.
//! Test: `ask_then_one_poller_tick_resolves_and_answer_returns_it`,
//! `add_on_batch_is_a_loud_poller_error_and_shows_in_gchat_doctor`,
//! `interval_ticks_sends_one_tick_per_period`.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::mpsc;

use crate::gchat::channel::GchatChannel;
use crate::gchat::error::InboundError;
use crate::gchat::inbound::{BatchReport, InboundOutcome};
use crate::gchat::state::now_rfc3339;

/// The default tick period of the `gchat-mcp` poller.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Messages pulled per tick.
pub const PULL_BATCH: u32 = 10;

/// What the poller has done so far, for `gchat_doctor`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PollStatus {
    /// Ticks run.
    pub ticks: u64,
    /// When the last tick succeeded (RFC 3339).
    pub last_ok_at: Option<String>,
    /// The current failure, cleared by the next successful tick.
    pub last_error: Option<String>,
    /// Failed ticks since the last success.
    pub consecutive_failures: u64,
    /// Questions resolved by this process.
    pub answered: u64,
    /// Pulled messages left unacknowledged because their answer or audit
    /// line could not be written. A tick that withholds any is a failure.
    pub withheld: u64,
    /// Pulled messages dropped over a rate limit (#8454). A limited drop is
    /// acked, so it never fails a tick.
    pub rate_limited: u64,
}

impl PollStatus {
    /// True while the last tick succeeded (or none has run yet).
    pub fn is_healthy(&self) -> bool {
        self.last_error.is_none()
    }
}

/// A [`PollStatus`] shared between the poller and the MCP server.
pub type SharedPollStatus = Arc<Mutex<PollStatus>>;

/// Lock a shared status; a poisoned lock still holds whole values.
pub(crate) fn read_status(status: &SharedPollStatus) -> MutexGuard<'_, PollStatus> {
    status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Polls one channel's subscription.
///
/// Why: the server's background task; split from the tick source so the
/// tick interval is injectable.
/// What: holds the shared channel, the shared status and the batch size.
/// Test: `ask_then_one_poller_tick_resolves_and_answer_returns_it`.
#[derive(Debug, Clone)]
pub struct Poller {
    channel: Arc<GchatChannel>,
    status: SharedPollStatus,
    max: u32,
}

impl Poller {
    /// A poller over `channel` pulling up to `max` messages a tick.
    pub fn new(channel: Arc<GchatChannel>, max: u32) -> Self {
        Self {
            channel,
            status: SharedPollStatus::default(),
            max,
        }
    }

    /// The status this poller writes.
    pub fn status(&self) -> SharedPollStatus {
        Arc::clone(&self.status)
    }

    /// Run one pull–process–acknowledge step and record its outcome.
    ///
    /// Why: the unit a test drives without a timer.
    /// What: calls [`GchatChannel::poll_once`] and counts answered
    /// questions and rate-limited drops. A batch with no withheld message
    /// clears the error. A batch that withheld any message, or a failed
    /// step, stores an error
    /// text and logs it when it differs from the previous tick's: an
    /// add-on-format batch at `error` level naming the "Workspace add-on"
    /// setting, a missing configuration at `warn`, anything else at `error`.
    /// Test: `ask_then_one_poller_tick_resolves_and_answer_returns_it`,
    /// `add_on_batch_is_a_loud_poller_error_and_shows_in_gchat_doctor`,
    /// `withheld_reply_makes_the_poller_unhealthy_in_gchat_doctor`,
    /// `poll_status_counts_rate_limited_and_stays_healthy`.
    pub async fn tick(&self) -> Result<BatchReport, InboundError> {
        let result = self.channel.poll_once(self.max).await;
        let mut status = read_status(&self.status);
        status.ticks += 1;
        match &result {
            Ok(report) => {
                let answered = report
                    .outcomes
                    .iter()
                    .filter(|o| matches!(o, InboundOutcome::Answered { .. }))
                    .count();
                status.answered += answered as u64;
                // #8454: counted, never a failure on its own.
                status.rate_limited += report.rate_limited;
                if report.withheld.is_empty() {
                    if status.last_error.take().is_some() {
                        tracing::info!("gchat poller recovered");
                    }
                    status.consecutive_failures = 0;
                    status.last_ok_at = Some(now_rfc3339());
                } else {
                    // #9448 review: a withheld message is a failed tick, not
                    // a healthy one; Pub/Sub redelivers it.
                    status.withheld += report.withheld.len() as u64;
                    let text = format!(
                        "{} pulled message(s) withheld unacknowledged: their answer or audit \
                         line could not be written to the state directory",
                        report.withheld.len()
                    );
                    record_failure(&mut status, text, |t| {
                        tracing::error!("gchat poller: {t}");
                    });
                }
            }
            Err(e) => record_failure(&mut status, e.to_string(), |t| log_failure(e, t)),
        }
        result
    }

    /// Tick once per message received on `ticks`, until its senders drop.
    ///
    /// Why: production ticks on a timer; a test ticks by sending `()`.
    /// What: awaits each tick in turn, so ticks never overlap.
    /// Test: `ask_then_one_poller_tick_resolves_and_answer_returns_it`.
    pub async fn run(&self, mut ticks: mpsc::Receiver<()>) {
        while ticks.recv().await.is_some() {
            // The outcome is recorded in the status and logged by `tick`.
            let _ = self.tick().await;
        }
    }
}

/// Store a failed tick, logging `text` only when it changed.
fn record_failure(status: &mut PollStatus, text: String, log: impl FnOnce(&str)) {
    if status.last_error.as_deref() != Some(text.as_str()) {
        log(&text);
    }
    status.consecutive_failures += 1;
    status.last_error = Some(text);
}

fn log_failure(e: &InboundError, text: &str) {
    match e {
        InboundError::AddOnEventFormat { .. } => {
            tracing::error!("gchat poller: Workspace add-on event format: {text}");
        }
        InboundError::NotConfigured { .. } => {
            tracing::warn!("gchat poller idle: {text}");
        }
        _ => tracing::error!("gchat poller tick failed: {text}"),
    }
}

/// A tick channel fed every `period`, first tick at once.
///
/// Why: the production tick source for [`Poller::run`].
/// What: spawns a task on the current runtime that sends `()` each period.
/// A tick is dropped, not queued, while the poller is still busy with the
/// last one. The task ends when the receiver drops.
/// Test: `interval_ticks_sends_one_tick_per_period`.
pub fn interval_ticks(period: Duration) -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if let Err(mpsc::error::TrySendError::Closed(())) = tx.try_send(()) {
                return;
            }
        }
    });
    rx
}
