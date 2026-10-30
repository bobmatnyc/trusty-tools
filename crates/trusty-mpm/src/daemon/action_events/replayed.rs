//! Spool files a start replayed, held until their events leave (#10467).
//!
//! Why: the replay used to delete each spool file as it read it, before the
//! events reached the bus, so a process killed before its first flush lost
//! the backlog it took (up to the 4096 of the #10412 incident).
//! What: [`ActionPublisher::replay_spool`] buffers the events and keeps every
//! taken file locked on disk as a [`spool::Claim`]. Every flush goes through
//! [`ActionPublisher::flush_counted`], which credits the frames written and
//! retires (deletes) each claim, oldest first, once all its events are
//! written. The stop's settle retires what is left only after those events
//! are spooled again; otherwise it drops the claims and the files stay.
//!
//! Delivery: at-least-once on disk, never lost. A file is deleted after its
//! events are written, so a kill between the write and the delete replays
//! that file's events once more. Each event keeps its `EventId` in the
//! spool, and trusty-events drops a repeated id still in its ring
//! (`EventBus::ingest`), so the bus shows each event once. The window is one
//! drain long; a replay that comes after the ring evicted the first copy is
//! the one case a duplicate can reach a viewer.
//! Test: `spool_tests.rs` (`*_10467`).

use std::collections::VecDeque;

use trusty_common::control_bus::PushFlushOutcome;

use super::{ActionPublisher, spool};

/// The claims a replay holds, plus the writes credited against them.
#[derive(Default)]
pub(super) struct Replayed {
    pending: parking_lot::Mutex<Pending>,
    /// One flush at a time, so a credit counts only finished writes.
    flushing: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Pending {
    /// Taken files, oldest first; their events lead the buffer in order.
    claims: VecDeque<spool::Claim>,
    /// Frames written since the replay not yet matched to a whole claim.
    credit: usize,
    /// The buffer's overflow count when the replay buffered its events.
    dropped_at_take: u64,
    /// A flush was cut off, so `credit` may miss frames it wrote.
    uncounted: bool,
}

/// Marks [`Pending::uncounted`] when a flush is dropped before its credit.
struct CreditGuard<'a> {
    replayed: &'a Replayed,
    credited: bool,
}

impl Drop for CreditGuard<'_> {
    fn drop(&mut self) {
        if !self.credited {
            self.replayed.pending.lock().uncounted = true;
        }
    }
}

impl Pending {
    /// Add `sent` written frames; returns every claim now fully written.
    fn credit(&mut self, sent: usize) -> Vec<spool::Claim> {
        if self.claims.is_empty() {
            return Vec::new();
        }
        self.credit += sent;
        let mut done = Vec::new();
        while self
            .claims
            .front()
            .is_some_and(|c| c.events() <= self.credit)
        {
            if let Some(claim) = self.claims.pop_front() {
                self.credit -= claim.events();
                done.push(claim);
            }
        }
        if self.claims.is_empty() {
            self.credit = 0;
        }
        done
    }
}

/// [`spool::retire`] each claim; a failure is logged, since its events were
/// delivered and can at worst replay again.
pub(super) fn retire_all(claims: Vec<spool::Claim>) {
    for claim in claims {
        if let Err(e) = spool::retire(claim) {
            tracing::warn!(error = %e, "action events: delivered spool file kept; its events may replay again");
        }
    }
}

impl ActionPublisher {
    /// Buffer every event a previous run spooled; returns how many.
    ///
    /// Why: events spooled at shutdown reach the bus only if a later start
    /// sends them (#10412).
    /// What: [`spool::take_all`] takes each file under a lock; its events
    /// join the buffer ahead of any new hook. #10467: the files stay on disk,
    /// locked, until [`Self::flush_counted`] or the settle retires them.
    /// Overflow is counted in [`Self::dropped`], and logged here too.
    /// Test: `spooled_events_replay_on_the_next_start_exactly_once_10412`,
    /// `a_taker_sigkilled_before_delivery_loses_no_spooled_event_10467`.
    pub fn replay_spool(&self) -> usize {
        let Some(dir) = &self.spool else { return 0 };
        let replay = match spool::take_all(dir) {
            Ok(replay) => replay,
            Err(e) => {
                tracing::warn!(error = %e, dir = %dir.display(), "action events: spool unreadable; nothing replayed");
                return 0;
            }
        };
        if replay.skipped > 0 || replay.failed_files > 0 {
            tracing::warn!(
                skipped = replay.skipped,
                failed_files = replay.failed_files,
                "action events: part of the spool was unreadable"
            );
        }
        if replay.held > 0 {
            tracing::info!(
                held = replay.held,
                "action events: spool files held by a live process; left to it"
            );
        }
        let dropped_before = self.client.dropped();
        let count = replay.events.len();
        for event in replay.events {
            self.buffer(event);
        }
        let overflowed = self.client.dropped().saturating_sub(dropped_before);
        // #10467: hold the files until their events are written.
        let empty = {
            let mut pending = self.replayed.pending.lock();
            pending.claims.extend(replay.claims);
            pending.dropped_at_take = dropped_before;
            pending.credit(0)
        };
        retire_all(empty);
        if count > 0 {
            tracing::info!(
                replayed = count,
                overflowed,
                "action events: replayed the spool from the last stop"
            );
        }
        count
    }

    /// Flush the buffer once and retire every claim it fully wrote.
    ///
    /// Why: #10467: a replayed file may go only once its events are on the
    /// bus; a failed flush must leave it.
    /// What: one flush at a time; credits `sent` (frames written, never a
    /// frame re-queued by a failed or cancelled write) and deletes each
    /// claim whose events are all written. A flush cancelled mid-way credits
    /// nothing, so its claims stay: a later replay may repeat, never lose.
    /// It marks the count uncertain for [`Self::take_claims`].
    /// Test: `a_failed_bus_flush_keeps_the_replayed_file_and_a_delivery_removes_it_10467`.
    pub(super) async fn flush_counted(&self) -> PushFlushOutcome {
        let _one = self.replayed.flushing.lock().await;
        let mut guard = CreditGuard {
            replayed: &self.replayed,
            credited: false,
        };
        let outcome = self.client.flush().await;
        guard.credited = true;
        let done = self.replayed.pending.lock().credit(outcome.sent);
        retire_all(done);
        outcome
    }

    /// Take every claim still held, and how many of its events the buffer
    /// still holds; the settle calls it right after draining the buffer.
    ///
    /// What: the replayed events lead the buffer, so those not yet written
    /// are the unmatched claim events less any the overflow evicted (oldest
    /// first, so replayed first). After a cut-off flush the count is unknown
    /// and reads as zero, so a loss is over-counted, never under-counted.
    pub(super) fn take_claims(&self) -> (Vec<spool::Claim>, usize) {
        let mut pending = self.replayed.pending.lock();
        let uncounted = std::mem::take(&mut pending.uncounted);
        let outstanding = pending
            .claims
            .iter()
            .map(spool::Claim::events)
            .sum::<usize>()
            .saturating_sub(pending.credit);
        let evicted = self
            .client
            .dropped()
            .saturating_sub(pending.dropped_at_take);
        let evicted = usize::try_from(evicted).unwrap_or(usize::MAX);
        pending.credit = 0;
        let claims = pending.claims.drain(..).collect();
        if uncounted {
            return (claims, 0);
        }
        (claims, outstanding.saturating_sub(evicted))
    }
}
