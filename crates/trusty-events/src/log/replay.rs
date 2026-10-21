//! Replay-on-reconnect: everything persisted after `since_seq`, in order.
//!
//! Why: DOC-73 §4.3 — "a viewer that opens mid-session reads the log to build
//! the tree it missed, then switches to the live ring at the log's last
//! `seq`." A gap must never read as silence: retention can have discarded
//! everything before `since_seq` asked for, and the writer's bounded channel
//! (`super::config::WRITE_CHANNEL_CAPACITY`) can have dropped an event under
//! backpressure, leaving a hole inside what IS retained. Both cases surface
//! as an explicit [`ReplayItem::Gap`] rather than the caller silently getting
//! fewer events than it expected.
//! What: `replay_since` lists day files, reads every event with
//! `seq > since_seq` across them in seq order (file order already matches
//! seq order, since files rotate forward in time and seq only increases), and
//! interleaves gap markers: one BEFORE the first event if `since_seq` predates
//! `earliest_retained_seq`, and one wherever two consecutive events' seqs are
//! not adjacent (a backpressure drop). Every returned event is wrapped
//! `persisted: true` — read from the log, it always is, by construction.
//! #10071 review: [`ReplayFiles`] yields the same items one day file at a
//! time, so a subscription can beat between reads and stop when its reader
//! leaves instead of holding the whole log in memory first.
//! Test: `super::tests::replay_since_zero_returns_everything_in_order`,
//! `super::tests::replay_since_a_mid_seq_returns_only_the_remainder`,
//! `super::tests::replay_since_before_retention_yields_a_leading_gap`,
//! `super::tests::replay_since_across_a_backpressure_drop_yields_an_interior_gap`,
//! `super::tests::replay_since_a_drop_on_the_first_replayed_seq_yields_a_gap`,
//! `super::tests::replay_since_caught_up_returns_no_events_and_no_gap`,
//! `super::tests::replay_since_on_an_empty_log_returns_nothing`.

use std::path::Path;

use trusty_common::control_bus::HarnessEvent;

use super::error::LogError;
use super::format::read_events;
use super::recovery::{DayFile, list_log_files};

/// One item of a replay: either a persisted event, or an explicit marker that
/// events between `after_seq` and `before_seq` (both exclusive) are gone and
/// will never be replayed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ReplayItem {
    /// A durably-written event, boxed so this arm does not force every
    /// [`ReplayItem::Gap`] to carry `HarnessEvent`'s full size too (clippy
    /// `large_enum_variant`). Always `persisted: true` on the
    /// [`super::super::bus::BusFrame`] a caller wraps this in — read from the
    /// log, it already is.
    Event(Box<HarnessEvent>),
    /// No event with a seq in `(after_seq, before_seq)` will ever be
    /// replayed — either retention discarded it, or the writer dropped it
    /// under backpressure and it was never durably written at all.
    Gap { after_seq: u64, before_seq: u64 },
}

/// Replay everything persisted after `since_seq`, in seq order.
///
/// `earliest_retained_seq` is [`super::DurableLog`]'s current lower bound —
/// `None` when nothing has ever been retained. When `since_seq` predates it,
/// the first item returned is a [`ReplayItem::Gap`] covering
/// `(since_seq, earliest_retained_seq)` before any event.
///
/// # Errors
///
/// [`LogError::Io`] if a day file cannot be listed or read — a single file
/// disappearing between listing and reading degrades to "no events in that
/// file" (see [`read_events`]), not a hard failure of the whole replay.
///
/// Test: see module docs.
// #10071 review: the whole-log read is the tests' oracle; subscriptions read
// through `ReplayFiles`.
#[cfg(test)]
pub(crate) async fn replay_since(
    dir: &Path,
    since_seq: u64,
    earliest_retained_seq: Option<u64>,
) -> Result<Vec<ReplayItem>, LogError> {
    let mut files = ReplayFiles::open(dir, since_seq, earliest_retained_seq).await?;
    let mut items = Vec::new();
    while let Some(file) = files.next_file().await {
        items.extend(file?);
    }
    Ok(items)
}

/// A replay read one day file per step (#10071 review).
///
/// Why: a subscription must keep beating while the log is read, and stop
/// reading once its reader has gone; one whole-log read allows neither.
/// What: each [`ReplayFiles::next_file`] returns the next day file's items,
/// in the order `replay_since` returns them; the leading gap, if any, comes
/// with the first step. `None` once every file listed at open has been read.
/// Test: `crate::serve_tests::a_slow_replay_read_still_beats`,
/// `crate::serve_tests::a_pump_whose_reader_left_stops_reading_the_log`.
pub(crate) struct ReplayFiles {
    files: std::vec::IntoIter<DayFile>,
    since_seq: u64,
    previous_seq: Option<u64>,
    leading: Option<ReplayItem>,
}

impl ReplayFiles {
    /// List the day files a replay after `since_seq` reads.
    ///
    /// # Errors
    ///
    /// [`LogError::Io`] if the log directory cannot be listed.
    pub(crate) async fn open(
        dir: &Path,
        since_seq: u64,
        earliest_retained_seq: Option<u64>,
    ) -> Result<Self, LogError> {
        let files = list_log_files(dir).await?;
        // #6848: seed from `since_seq` itself, not `None` — the caller already
        // has everything up to and including `since_seq`, so a hole immediately
        // after it (the FIRST event this call returns) must be checked exactly
        // like every later hole is. Leaving this `None` until the leading-gap
        // branch below set it meant the first returned event skipped the
        // `prev + 1` check entirely whenever `since_seq` was already inside the
        // retained window — the normal reconnect case — silently dropping the
        // gap marker for a seq that was broadcast live but never durably
        // written.
        let mut previous_seq = Some(since_seq);
        let mut leading = None;
        if let Some(earliest) = earliest_retained_seq
            && since_seq + 1 < earliest
        {
            leading = Some(ReplayItem::Gap {
                after_seq: since_seq,
                before_seq: earliest,
            });
            previous_seq = Some(earliest - 1);
        }
        Ok(Self {
            files: files.into_iter(),
            since_seq,
            previous_seq,
            leading,
        })
    }

    /// The next day file's items; `None` once every file has been read.
    ///
    /// # Errors
    ///
    /// `Some(Err(_))` if a day file cannot be read; one rotated away since
    /// the listing reads as empty (see [`read_events`]).
    pub(crate) async fn next_file(&mut self) -> Option<Result<Vec<ReplayItem>, LogError>> {
        let mut items: Vec<ReplayItem> = Vec::new();
        let Some((_, path)) = self.files.next() else {
            // No file left: a leading gap still has to be reported.
            return self.leading.take().map(|gap| Ok(vec![gap]));
        };
        items.extend(self.leading.take());
        let events = match read_events(&path).await {
            Ok(events) => events,
            Err(e) => return Some(Err(e)),
        };
        for event in events {
            if event.seq <= self.since_seq {
                continue;
            }
            if let Some(prev) = self.previous_seq
                && event.seq > prev + 1
            {
                items.push(ReplayItem::Gap {
                    after_seq: prev,
                    before_seq: event.seq,
                });
            }
            self.previous_seq = Some(event.seq);
            items.push(ReplayItem::Event(Box::new(event)));
        }
        Some(Ok(items))
    }
}
