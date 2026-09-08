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
//! What: [`replay_since`] lists day files, reads every event with
//! `seq > since_seq` across them in seq order (file order already matches
//! seq order, since files rotate forward in time and seq only increases), and
//! interleaves gap markers: one BEFORE the first event if `since_seq` predates
//! `earliest_retained_seq`, and one wherever two consecutive events' seqs are
//! not adjacent (a backpressure drop). Every returned event is wrapped
//! `persisted: true` — read from the log, it always is, by construction.
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
use super::recovery::list_log_files;

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
pub(crate) async fn replay_since(
    dir: &Path,
    since_seq: u64,
    earliest_retained_seq: Option<u64>,
) -> Result<Vec<ReplayItem>, LogError> {
    let files = list_log_files(dir).await?;
    let mut items = Vec::new();
    // #6848: seed from `since_seq` itself, not `None` — the caller already
    // has everything up to and including `since_seq`, so a hole immediately
    // after it (the FIRST event this call returns) must be checked exactly
    // like every later hole is. Leaving this `None` until the leading-gap
    // branch below set it meant the first returned event skipped the
    // `prev + 1` check entirely whenever `since_seq` was already inside the
    // retained window — the normal reconnect case — silently dropping the
    // gap marker for a seq that was broadcast live but never durably
    // written.
    let mut previous_seq: Option<u64> = Some(since_seq);

    if let Some(earliest) = earliest_retained_seq
        && since_seq + 1 < earliest
    {
        items.push(ReplayItem::Gap {
            after_seq: since_seq,
            before_seq: earliest,
        });
        previous_seq = Some(earliest - 1);
    }

    for (_, path) in &files {
        for event in read_events(path).await? {
            if event.seq <= since_seq {
                continue;
            }
            if let Some(prev) = previous_seq
                && event.seq > prev + 1
            {
                items.push(ReplayItem::Gap {
                    after_seq: prev,
                    before_seq: event.seq,
                });
            }
            previous_seq = Some(event.seq);
            items.push(ReplayItem::Event(Box::new(event)));
        }
    }
    Ok(items)
}
