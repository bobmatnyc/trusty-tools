//! The once-only settle of the push buffer at stop (#10412).
//!
//! Why: on SIGTERM the signal handler (200 ms) and the serve drain (1 s) both
//! settle one publisher, and the socket keeps taking hooks until
//! `process::exit`. Two drains split the buffer: the handler could exit while
//! the other caller's spool write was in flight, and a hook buffered after a
//! drain sat in a buffer nothing settled again. Both were uncounted losses.
//! What: [`SettleState`] runs the settle at most once. A second caller waits
//! for that result within its own budget; if the budget ends first, it counts
//! every event still at risk as lost. The drain closes the buffer under the
//! lock every sender takes, so a later event is counted and logged as late.
//! Test: `spool_tests.rs`.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use trusty_common::control_bus::{HarnessEvent, PushFlushOutcome};

use super::{ActionPublisher, FINAL_FLUSH_BUDGET, spool};

/// Kept back from a settle's own budget so its result lands before the
/// caller's outer timeout does.
const SETTLE_MARGIN: Duration = Duration::from_millis(10);

/// Where the final flush left the buffered events (#10412).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Settled {
    /// Written to the bus.
    pub sent: usize,
    /// Written to the spool, for replay on the next start.
    pub spooled: usize,
    /// Neither; each loss is also logged with this count.
    pub lost: usize,
}

/// A publisher's stop state: closed flag, settle result, at-risk counts.
#[derive(Default)]
pub(super) struct SettleState {
    /// Set by the settle's drain; every sender checks it under this lock.
    closed: parking_lot::Mutex<bool>,
    /// The one settle's result, shared by every caller.
    result: tokio::sync::OnceCell<Settled>,
    /// Events drained and not yet sent or spooled.
    in_flight: AtomicUsize,
    /// Events offered after the drain.
    late: AtomicU64,
}

impl ActionPublisher {
    /// Buffer `event`, or count it as late once the stop has drained.
    ///
    /// What: the closed check and the buffer write share one lock with the
    /// drain, so an event is either drained or counted, never stranded. A
    /// late event is logged at counts 1, 2, 4, 8, ... so a flood stays a few
    /// lines.
    /// Test: `an_event_after_the_settle_is_counted_lost_10412`.
    pub(super) fn buffer(&self, event: HarnessEvent) {
        let closed = self.settle.closed.lock();
        if !*closed {
            self.client.send(event);
            return;
        }
        drop(closed);
        let late = self.settle.late.fetch_add(1, Ordering::Relaxed) + 1;
        if late.is_power_of_two() {
            tracing::warn!(
                late_lost = late,
                "action events: event after the stop settled the buffer; lost"
            );
        }
    }

    /// Events offered after the stop drained the buffer.
    pub fn late_lost(&self) -> u64 {
        self.settle.late.load(Ordering::Relaxed)
    }

    /// The exit-path flush, bounded by [`FINAL_FLUSH_BUDGET`].
    ///
    /// Test: `final_flush_is_bounded_when_the_bus_stalls_6849`,
    /// `shutdown_runs_the_final_flush_once_6849`.
    pub async fn final_flush(&self) -> Settled {
        self.final_flush_within(FINAL_FLUSH_BUDGET).await
    }

    /// Settle the buffer once; every caller gets that result within `budget`.
    ///
    /// Why: two exit paths settle concurrently on SIGTERM (#10412).
    /// What: the first caller runs the settle; a later one waits on it. A
    /// caller whose `budget` ends first returns the drained-but-unwritten
    /// plus still-buffered count as `lost`, and logs it, since its process
    /// may exit before the settle ends.
    /// Test: `concurrent_settles_account_for_every_event_once_10412`,
    /// `a_settle_waiter_out_of_budget_counts_what_is_at_risk_10412`.
    pub async fn final_flush_within(&self, budget: Duration) -> Settled {
        let once = self
            .settle
            .result
            .get_or_init(|| self.settle_within(budget.saturating_sub(SETTLE_MARGIN)));
        match tokio::time::timeout(budget, once).await {
            Ok(settled) => *settled,
            Err(_) => {
                let at_risk =
                    self.settle.in_flight.load(Ordering::Relaxed) + self.client.buffered_len();
                tracing::warn!(
                    lost = at_risk,
                    "action events: stop budget ended before the settle did; counted lost"
                );
                Settled {
                    lost: at_risk,
                    ..Settled::default()
                }
            }
        }
    }

    /// The settle itself: deliver, then spool the rest.
    ///
    /// What: the bus gets half of `budget` (a cut-off write re-queues its
    /// frame). The buffer is then closed and drained in one step; the drained
    /// events go to the spool on the blocking pool, cut off at `budget`.
    /// Every event neither sent nor spooled is counted in [`Settled::lost`]
    /// and logged with that count.
    /// Test: `events_buffered_at_shutdown_with_the_bus_down_are_spooled_10412`,
    /// `a_hung_bus_and_an_unwritable_spool_stop_in_bound_and_count_the_loss_10412`.
    async fn settle_within(&self, budget: Duration) -> Settled {
        let deadline = tokio::time::Instant::now() + budget;
        let mut settled = Settled::default();
        let buffered = self.client.buffered_len();
        match tokio::time::timeout(budget / 2, self.client.flush()).await {
            Ok(PushFlushOutcome { error: None, sent }) => settled.sent = sent,
            Ok(PushFlushOutcome {
                error: Some(e),
                sent,
            }) => {
                tracing::info!(sent, error = %e, "action events: final flush stopped; spooling the rest");
                settled.sent = sent;
            }
            Err(_) => {
                // The cut-off flush reports nothing; the buffer shows what it wrote.
                settled.sent = buffered.saturating_sub(self.client.buffered_len());
                tracing::info!(
                    sent = settled.sent,
                    "action events: final flush outlived its budget; spooling the rest"
                );
            }
        }
        let left = {
            let mut closed = self.settle.closed.lock();
            *closed = true;
            let left = self.client.drain_buffered();
            self.settle.in_flight.store(left.len(), Ordering::Relaxed);
            left
        };
        let count = left.len();
        settled.lost = count;
        if count == 0 {
            tracing::debug!(sent = settled.sent, "action events: final flush delivered");
        } else if let Some(dir) = self.spool.clone() {
            let write = tokio::task::spawn_blocking(move || spool::persist(&dir, &left));
            match tokio::time::timeout_at(deadline, write).await {
                Ok(Ok(Ok(written))) => {
                    tracing::info!(
                        spooled = written,
                        "action events: undelivered at stop; spooled for the next start"
                    );
                    settled.spooled = written;
                    settled.lost = count.saturating_sub(written);
                }
                Ok(Ok(Err(e))) => {
                    tracing::warn!(lost = count, error = %e, "action events: spool write failed; lost");
                }
                Ok(Err(e)) => {
                    tracing::warn!(lost = count, error = %e, "action events: spool write did not finish; lost");
                }
                Err(_) => tracing::warn!(
                    lost = count,
                    "action events: spool write outlived the stop budget; counted lost, though the file may still land and replay"
                ),
            }
        } else {
            tracing::warn!(
                lost = count,
                "action events: undelivered at stop and no spool; lost"
            );
        }
        self.settle.in_flight.store(0, Ordering::Relaxed);
        settled
    }
}
