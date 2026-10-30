//! Spool files a start replayed, held until their events leave (#10467).
//!
//! Why: the replay used to delete each spool file as it read it, before the
//! events reached the bus, so a process killed before its first flush lost
//! the backlog it took (up to the 4096 of the #10412 incident).
//! What: [`ActionPublisher::replay_spool`] claims no more spool files than
//! the buffer holds ([`spool::Budget`]); newer files wait on disk for a later
//! start. It buffers the claimed events and keeps each file locked on disk as
//! a [`spool::Claim`]. Every flush goes through
//! [`ActionPublisher::flush_counted`], which credits the frames written and
//! retires (deletes) each claim, oldest first, once all its events are
//! written. Once the buffer has evicted any frame while claims are held, no
//! claim is retired by credit or at stop: an evicted event may be one of
//! theirs, so its file stays. The stop's settle retires what is left only
//! after those events are spooled again; otherwise the files stay.
//!
//! Delivery: at-least-once. A file is deleted only after its events are
//! written to the bus or spooled again, so no event is lost to a crash. A
//! file is retired whole, so a kill after part of it was written replays all
//! of it, and a file kept after an eviction replays in full too. Each event
//! keeps its `EventId` in the spool; trusty-events drops a repeated id only
//! while the first copy is still in its in-memory ring (8192 entries, not
//! reloaded after a trusty-events restart). A repeat that arrives later
//! reaches viewers. Exactly-once across a crash needs bus acknowledgements,
//! which are out of scope (Architect ruling on #10467).
//! Test: `spool_tests.rs` (`*_10467`), `replayed_tests.rs`.

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

/// The held claims and what has been written against them.
#[derive(Default)]
pub(super) struct Pending {
    /// Taken files, oldest first; their events lead the buffer in order.
    pub(super) claims: VecDeque<spool::Claim>,
    /// Frames written since the replay not yet matched to a whole claim.
    pub(super) credit: usize,
    /// The buffer's overflow count when the replay buffered its events.
    pub(super) dropped_at_take: u64,
    /// A flush was cut off, so `credit` may miss frames it wrote.
    pub(super) uncounted: bool,
    /// The buffer evicted a frame while claims were held (#10467).
    pub(super) evicted: bool,
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
    /// Add `sent` written frames, given the buffer's overflow count
    /// `dropped`; returns every claim now fully written.
    ///
    /// What: claims retire oldest first, each once the credit covers all its
    /// events (a zero-event claim at once). #10467: if `dropped` passed
    /// [`Self::dropped_at_take`] while claims are held, the eviction took the
    /// oldest frames, which may be replayed ones, so nothing retires from
    /// then on and the files stay for a later replay.
    /// Test: `credit_retires_whole_claims_and_never_after_an_eviction_10467`.
    pub(super) fn credit(&mut self, sent: usize, dropped: u64) -> Vec<spool::Claim> {
        if self.claims.is_empty() {
            return Vec::new();
        }
        if dropped > self.dropped_at_take {
            self.evicted = true;
        }
        if self.evicted {
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
    /// Buffer the events a previous run spooled, up to the buffer's room;
    /// returns how many.
    ///
    /// Why: events spooled at shutdown reach the bus only if a later start
    /// sends them (#10412).
    /// What: [`spool::take_within`] takes files under a lock, oldest first,
    /// within [`Self::budget`]; their events join the buffer ahead of any new
    /// hook. #10467: the files stay on disk, locked, until
    /// [`Self::flush_counted`] or the settle retires them; files past the
    /// budget stay untaken for a later start.
    /// Test: `spooled_events_replay_on_the_next_start_exactly_once_10412`,
    /// `a_taker_sigkilled_before_delivery_loses_no_spooled_event_10467`,
    /// `a_replay_larger_than_the_buffer_loses_no_event_at_stop_10467`.
    pub fn replay_spool(&self) -> usize {
        let Some(dir) = &self.spool else { return 0 };
        let replay = match spool::take_within(dir, self.budget) {
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
        if replay.held > 0 || replay.deferred > 0 {
            tracing::info!(
                held = replay.held,
                deferred = replay.deferred,
                "action events: spool files left on disk (held by a live process, or past the buffer's room)"
            );
        }
        let dropped_before = self.client.dropped();
        let count = replay.events.len();
        for event in replay.events {
            self.buffer(event);
        }
        // #10467: hold the files until their events are written.
        let empty = {
            let mut pending = self.replayed.pending.lock();
            pending.claims.extend(replay.claims);
            pending.dropped_at_take = dropped_before;
            pending.credit(0, self.client.dropped())
        };
        retire_all(empty);
        if count > 0 {
            tracing::info!(
                replayed = count,
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
    /// claim whose events are all written, unless the buffer has evicted a
    /// frame since the replay ([`Pending::credit`]). A flush cancelled
    /// mid-way credits nothing, so its claims stay, and it marks the count
    /// uncertain for [`Self::take_claims`].
    /// Test: `a_failed_bus_flush_keeps_the_replayed_file_and_a_delivery_removes_it_10467`,
    /// `hooks_that_evict_replayed_events_keep_their_file_10467`.
    pub(super) async fn flush_counted(&self) -> PushFlushOutcome {
        let _one = self.replayed.flushing.lock().await;
        let mut guard = CreditGuard {
            replayed: &self.replayed,
            credited: false,
        };
        let outcome = self.client.flush().await;
        guard.credited = true;
        let dropped = self.client.dropped();
        let done = self.replayed.pending.lock().credit(outcome.sent, dropped);
        retire_all(done);
        outcome
    }

    /// Take every claim the settle may retire, and how many of their events
    /// the buffer still holds; the settle calls it right after draining.
    ///
    /// What: the replayed events lead the buffer, so those not yet written
    /// are the unmatched claim events. #10467: after any eviction since the
    /// replay, the claims are dropped here (released, files intact) and
    /// `(empty, 0)` comes back, so the settle retires nothing and counts no
    /// replayed event as safe. After a cut-off flush the count is unknown
    /// and reads as zero, so a loss is over-counted, never under-counted.
    /// Test: `a_replay_larger_than_the_buffer_loses_no_event_at_stop_10467`,
    /// `hooks_that_evict_replayed_events_keep_their_file_10467`,
    /// `an_unwritable_spool_at_stop_keeps_the_replayed_files_10467`.
    pub(super) fn take_claims(&self) -> (Vec<spool::Claim>, usize) {
        let dropped = self.client.dropped();
        let mut pending = self.replayed.pending.lock();
        let uncounted = std::mem::take(&mut pending.uncounted);
        let evicted = std::mem::take(&mut pending.evicted)
            || (!pending.claims.is_empty() && dropped > pending.dropped_at_take);
        let outstanding = pending
            .claims
            .iter()
            .map(spool::Claim::events)
            .sum::<usize>()
            .saturating_sub(pending.credit);
        pending.credit = 0;
        let claims: Vec<spool::Claim> = pending.claims.drain(..).collect();
        drop(pending);
        if evicted {
            tracing::warn!(
                files = claims.len(),
                "action events: the buffer evicted frames while replayed spool files were held; files kept, their events may replay again"
            );
            return (Vec::new(), 0);
        }
        if uncounted {
            return (claims, 0);
        }
        (claims, outstanding)
    }
}
