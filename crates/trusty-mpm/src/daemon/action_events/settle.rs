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

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use trusty_common::control_bus::{HarnessEvent, PushFlushOutcome};

use super::{
    ActionEvents, ActionPublisher, FINAL_FLUSH_BUDGET, SIGNAL_FLUSH_BUDGET, replayed, spool,
};

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
    /// The settle is at the bus, which may hold one popped frame.
    at_bus: AtomicBool,
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
}

impl ActionEvents {
    /// Log the final late-event total once; returns it (#10412).
    ///
    /// Why: [`ActionPublisher::buffer`] logs late events only at powers of
    /// two, and the sessions a normal stop ends after the settle send hooks.
    /// What: one warn line with `late_lost` when it is above zero. Each stop
    /// path calls it last: serve after the shutdown report, the signal path
    /// before `process::exit`.
    /// Test: `the_signal_path_logs_the_final_late_total_10412`.
    pub fn report_late(&self) -> u64 {
        let late = self.publisher().map_or(0, |p| p.late_lost());
        if late > 0 {
            tracing::warn!(
                late_lost = late,
                "action events: events after the stop settled the buffer; lost in total"
            );
        }
        late
    }
}

impl ActionPublisher {
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
    /// #10412: the count is read under the lock the drain holds, so it never
    /// falls between the drain and its count, and it adds the one frame a
    /// flush at the bus may have popped. It is an upper bound.
    /// Test: `concurrent_settles_account_for_every_event_once_10412`,
    /// `a_signal_waiter_gets_the_spool_when_a_slower_caller_settles_10412`,
    /// `a_waiter_out_of_budget_counts_the_frame_at_the_bus_10412`.
    pub async fn final_flush_within(&self, budget: Duration) -> Settled {
        let once = self
            .settle
            .result
            .get_or_init(|| self.settle_within(budget.saturating_sub(SETTLE_MARGIN)));
        match tokio::time::timeout(budget, once).await {
            Ok(settled) => *settled,
            Err(_) => {
                let at_risk = {
                    let _closed = self.settle.closed.lock();
                    self.settle.in_flight.load(Ordering::Relaxed)
                        + self.client.buffered_len()
                        + usize::from(self.settle.at_bus.load(Ordering::Relaxed))
                };
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
    /// What: the bus gets half of `budget`, and never more than half of
    /// [`SIGNAL_FLUSH_BUDGET`] (a cut-off write re-queues its frame).
    /// #10412: the first caller's budget sets the settle's, and on SIGTERM
    /// that may be the serve drain's 1 s; the cap leaves room for the spool
    /// write inside the signal handler's fixed 200 ms. Under load that budget
    /// can still end first: the signal caller then returns its at-risk count
    /// as `lost`, an upper bound, while the settle finishes and spools the
    /// events for the other caller (see #10412). The buffer is then closed
    /// and drained in one step; the drained events go to the spool on the
    /// blocking pool, cut off at `budget`.
    /// Every event neither sent nor spooled is counted in [`Settled::lost`]
    /// and logged with that count.
    /// #10467: replayed spool files still held are retired only once the
    /// spool write lands (inside the blocking task, so a write that outlives
    /// the budget still retires them when it lands). If it fails, they are
    /// dropped and stay on disk, and their events count as spooled, not lost.
    /// After an eviction since the replay, no claim comes back to retire
    /// (`take_claims`), so those files stay and replay again.
    /// Test: `events_buffered_at_shutdown_with_the_bus_down_are_spooled_10412`,
    /// `a_hung_bus_and_an_unwritable_spool_stop_in_bound_and_count_the_loss_10412`,
    /// `an_unwritable_spool_at_stop_keeps_the_replayed_files_10467`.
    async fn settle_within(&self, budget: Duration) -> Settled {
        let deadline = tokio::time::Instant::now() + budget;
        let mut settled = Settled::default();
        let buffered = {
            let _closed = self.settle.closed.lock();
            self.settle.at_bus.store(true, Ordering::Relaxed);
            self.client.buffered_len()
        };
        let at_bus = budget.min(SIGNAL_FLUSH_BUDGET) / 2;
        match tokio::time::timeout(at_bus, self.flush_counted()).await {
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
        let (left, claims, on_disk) = {
            let mut closed = self.settle.closed.lock();
            *closed = true;
            self.settle.at_bus.store(false, Ordering::Relaxed);
            let left = self.client.drain_buffered();
            // #10467: replayed events still in their files are not at risk.
            let (claims, replayed) = self.take_claims();
            let on_disk = replayed.min(left.len());
            self.settle
                .in_flight
                .store(left.len() - on_disk, Ordering::Relaxed);
            (left, claims, on_disk)
        };
        let count = left.len();
        // Replayed events in their still-held files survive a failed write.
        let fresh = count - on_disk;
        settled.lost = fresh;
        if count == 0 {
            replayed::retire_all(claims);
            tracing::debug!(sent = settled.sent, "action events: final flush delivered");
        } else if let Some(dir) = self.spool.clone() {
            settled.spooled = on_disk;
            let write = tokio::task::spawn_blocking(move || {
                let written = spool::persist(&dir, &left);
                // #10467: the old files go only once the new one is down;
                // on an error the dropped claims leave them for replay.
                if written.is_ok() {
                    replayed::retire_all(claims);
                }
                written
            });
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
                    tracing::warn!(lost = fresh, kept = on_disk, error = %e, "action events: spool write failed; lost, except replayed events kept in their files");
                }
                Ok(Err(e)) => {
                    tracing::warn!(lost = fresh, kept = on_disk, error = %e, "action events: spool write did not finish; lost, except replayed events kept in their files");
                }
                Err(_) => tracing::warn!(
                    lost = fresh,
                    kept = on_disk,
                    "action events: spool write outlived the stop budget; counted lost, though the file may still land and replay"
                ),
            }
        } else {
            tracing::warn!(
                lost = fresh,
                "action events: undelivered at stop and no spool; lost"
            );
        }
        self.settle.in_flight.store(0, Ordering::Relaxed);
        settled
    }
}
