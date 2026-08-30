//! #6386: opening a reindex stream against a running reindex must deliver every
//! event exactly once.
//!
//! Why: a reindex stream opens by taking the replay buffer and then subscribing
//! to the live broadcast. Until #6386 those were two separate steps, and
//! `ReindexProgress::push` was two separate steps as well — it appended under the
//! `events` lock and broadcast after releasing it. An event whose append landed
//! after the snapshot and whose broadcast landed before the subscribe reached
//! NEITHER path, so a dashboard silently missed a progress event; the mirror
//! interleaving delivered one twice. Both arms are invisible to a consumer: a
//! reindex that emits 100 events and streams 99 looks exactly like a reindex that
//! emitted 99.
//!
//! What: [`no_event_is_lost_or_duplicated_when_a_stream_opens_against_live_pushes`]
//! is the regression test — it races real opens against real pushes on a
//! multi-threaded runtime and checks the one invariant the fix buys: the replay
//! an open returns, unioned with everything its subscription then delivers, is
//! every pushed event, each appearing once. The remaining cases pin the
//! deterministic composition either side of an open plus the two overflow paths
//! the single lock hold must not have changed.
//!
//! The race is only reachable across threads — before #6386 the window between
//! the snapshot and the subscribe had no await point in it, so a single-threaded
//! runtime could never schedule a push into it. That is why the regression test
//! is a multi-threaded race rather than a barrier-sequenced pair, and why it
//! carries enough trials to make the pre-fix loss reproducible.
//!
//! Test: this file IS the test module.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::broadcast::error::TryRecvError;

use super::progress::{BROADCAST_CAPACITY, MAX_REPLAY_EVENTS};
use super::{ReindexProgress, ReindexStatus};

/// How many independent open-versus-push races to run.
///
/// Why this many: one trial is one scheduling coincidence, and the pre-fix window
/// was a few instructions wide. At 4 openers per trial this is ~2 000 chances for
/// a push to land inside an open. 128 trials reproduced the pre-fix defect on
/// only about half of isolated runs; this count failed every reverted run
/// measured, and the whole loop still finishes in well under a second because it
/// touches nothing but memory.
const RACE_TRIALS: usize = 512;
/// Concurrent writers per trial — enough that some are always mid-emission.
const RACE_PUSHERS: usize = 8;
/// Concurrent readers per trial, each checked independently.
const RACE_OPENERS: usize = 4;
/// Events per trial. Deliberately below both channel bounds so a lagged
/// subscriber or a truncated replay is a broken fixture rather than a finding.
const RACE_EVENTS: usize = 200;

/// The `seq` an event line carries.
fn seq_of(line: &str) -> usize {
    let event: serde_json::Value =
        serde_json::from_str(line).expect("every planted event is one JSON document");
    usize::try_from(
        event["seq"]
            .as_u64()
            .expect("every planted event carries a numeric seq"),
    )
    .expect("seq fits a usize")
}

/// Every `seq` on the replay, then every `seq` the subscription still holds.
///
/// The order is the order a stream producer would emit them: replay first, live
/// after. Duplicates are preserved rather than folded, because a repeat is one of
/// the two failures under test.
fn delivered(replay: &[String], live: &mut tokio::sync::broadcast::Receiver<String>) -> Vec<usize> {
    let mut seen: Vec<usize> = replay.iter().map(|line| seq_of(line)).collect();
    loop {
        match live.try_recv() {
            Ok(line) => seen.push(seq_of(&line)),
            Err(TryRecvError::Empty | TryRecvError::Closed) => return seen,
            Err(TryRecvError::Lagged(skipped)) => panic!(
                "the subscriber lagged by {skipped}: this fixture pushes {RACE_EVENTS} events \
                 into a {BROADCAST_CAPACITY}-slot channel, so a lag means the fixture broke, \
                 not that the open lost anything"
            ),
        }
    }
}

/// Why: this is the #6386 regression. Before the fix, an event pushed while a
/// stream was opening could be appended after the open's snapshot and broadcast
/// before its subscribe, reaching neither path — a progress event silently
/// missing from a dashboard, with no error anywhere. The mirror interleaving
/// delivered one twice.
///
/// What: races `RACE_OPENERS` real opens against `RACE_PUSHERS` real writers over
/// `RACE_TRIALS` trials on a multi-threaded runtime. Each opener is then checked
/// alone: its replay plus everything its subscription delivered must be the full
/// pushed set, with no repeats. Reverting either half of the fix — the broadcast
/// back outside `push`'s lock, or the subscribe back outside the snapshot's —
/// fails this within a few trials.
///
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_event_is_lost_or_duplicated_when_a_stream_opens_against_live_pushes() {
    // The fixture must not be able to lag or truncate on its own: a lag or a
    // dropped replay entry would read as a lost event and accuse the code wrongly.
    const { assert!(RACE_EVENTS < BROADCAST_CAPACITY && RACE_EVENTS <= MAX_REPLAY_EVENTS) };
    let expected: BTreeSet<usize> = (0..RACE_EVENTS).collect();

    for trial in 0..RACE_TRIALS {
        let progress = Arc::new(ReindexProgress::new());
        let next = Arc::new(AtomicUsize::new(0));

        let pushers: Vec<_> = (0..RACE_PUSHERS)
            .map(|_| {
                let progress = Arc::clone(&progress);
                let next = Arc::clone(&next);
                tokio::spawn(async move {
                    loop {
                        let seq = next.fetch_add(1, Ordering::Relaxed);
                        if seq >= RACE_EVENTS {
                            return;
                        }
                        progress.push(serde_json::json!({ "seq": seq })).await;
                    }
                })
            })
            .collect();

        // Spawned without yielding first: the writers are already running, so an
        // open lands mid-emission rather than after the last one.
        let openers: Vec<_> = (0..RACE_OPENERS)
            .map(|_| {
                let progress = Arc::clone(&progress);
                tokio::spawn(async move { progress.subscribe_with_replay().await })
            })
            .collect();

        for pusher in pushers {
            pusher.await.expect("no writer may panic");
        }

        for (opener_n, opener) in openers.into_iter().enumerate() {
            let (replay, _status, mut live) = opener.await.expect("no opener may panic");
            let seen = delivered(&replay, &mut live);
            let unique: BTreeSet<usize> = seen.iter().copied().collect();

            assert_eq!(
                seen.len(),
                unique.len(),
                "trial {trial} opener {opener_n}: an event was delivered twice — \
                 it was in the {replay_len}-event replay AND arrived live",
                replay_len = replay.len(),
            );
            let lost: Vec<usize> = expected.difference(&unique).copied().collect();
            assert!(
                lost.is_empty(),
                "trial {trial} opener {opener_n}: {count} event(s) reached neither the replay \
                 nor the live subscription — seq {lost:?} (replay held {replay_len})",
                count = lost.len(),
                replay_len = replay.len(),
            );
        }
    }
}

/// Why: the two orderings either side of an open are what a consumer actually
/// depends on — an event emitted before it connected must arrive from the replay
/// and not again live, and one emitted after must arrive live and not already sit
/// in the replay. A rewrite that replayed twice, subscribed before snapshotting,
/// or dropped the replay changes one of these counts.
///
/// What: pushes one event, opens, then pushes a second, checking each path's
/// contents at every step; then re-opens to prove the buffer itself still holds
/// each event once.
///
/// Test: this function IS the test.
#[tokio::test]
async fn an_event_pushed_either_side_of_an_open_arrives_on_exactly_one_path() {
    let progress = ReindexProgress::new();
    progress.push(serde_json::json!({ "seq": 0 })).await;

    let (replay, status, mut live) = progress.subscribe_with_replay().await;
    assert_eq!(
        replay.iter().map(|line| seq_of(line)).collect::<Vec<_>>(),
        vec![0],
        "the event pushed before the open must be replayed"
    );
    assert_eq!(
        status,
        ReindexStatus::Running,
        "the open reports the status it read under the same lock"
    );
    assert!(
        matches!(live.try_recv(), Err(TryRecvError::Empty)),
        "the replayed event must not also arrive live"
    );

    progress.push(serde_json::json!({ "seq": 1 })).await;
    let live_line = live
        .try_recv()
        .expect("the event pushed after the open must arrive live");
    assert_eq!(seq_of(&live_line), 1);
    assert!(
        matches!(live.try_recv(), Err(TryRecvError::Empty)),
        "nothing beyond that one event may arrive"
    );

    let (replay, _status, _live) = progress.subscribe_with_replay().await;
    assert_eq!(
        replay.iter().map(|line| seq_of(line)).collect::<Vec<_>>(),
        vec![0, 1],
        "a later open replays both events, once each"
    );
}

/// Why: a subscriber that falls behind must still be TOLD, because both stream
/// producers turn `Lagged` into the `{"type":"lag","skipped":N}` frame a consumer
/// reads as "you missed some". Broadcasting under `push`'s lock must not have
/// turned an overrun into silence or into an end-of-stream.
///
/// What: opens, pushes past the channel's capacity without reading, and checks
/// the overrun is reported and that reception then resumes at the oldest event
/// the channel still holds.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_subscriber_that_outruns_the_channel_still_reports_its_lag() {
    let progress = ReindexProgress::new();
    let (_replay, _status, mut live) = progress.subscribe_with_replay().await;

    for seq in 0..BROADCAST_CAPACITY + 16 {
        progress.push(serde_json::json!({ "seq": seq })).await;
    }

    let skipped = match live.try_recv() {
        Err(TryRecvError::Lagged(skipped)) => skipped,
        other => panic!("an overrun subscriber must report Lagged, got {other:?}"),
    };
    assert!(skipped > 0, "a reported lag must name a non-zero count");

    let resumed = live
        .try_recv()
        .expect("a lagged subscriber resumes rather than ending");
    assert_eq!(
        u64::try_from(seq_of(&resumed)).expect("seq fits a u64"),
        skipped,
        "reception resumes at the oldest event the channel still holds"
    );
}

/// Why: `push` now holds the `events` lock across the broadcast, and the
/// over-cap eviction sits inside that same hold. A subscriber that opens after a
/// pathological reindex must still get the most recent `MAX_REPLAY_EVENTS` and
/// not an unbounded buffer.
///
/// What: pushes past the cap and checks the replay's length and both ends.
///
/// Test: this function IS the test.
#[tokio::test]
async fn the_replay_buffer_still_drops_its_oldest_at_the_cap() {
    let progress = ReindexProgress::new();
    let overshoot = 5;
    for seq in 0..MAX_REPLAY_EVENTS + overshoot {
        progress.push(serde_json::json!({ "seq": seq })).await;
    }

    let (replay, _status, _live) = progress.subscribe_with_replay().await;
    assert_eq!(replay.len(), MAX_REPLAY_EVENTS, "the cap still bounds it");
    assert_eq!(
        seq_of(&replay[0]),
        overshoot,
        "the oldest {overshoot} events were evicted"
    );
    assert_eq!(
        seq_of(replay.last().expect("a capped buffer is never empty")),
        MAX_REPLAY_EVENTS + overshoot - 1,
        "the newest event survives"
    );
}

/// Why: the status decides whether a producer subscribes at all — a stream that
/// read `Running` for a finished reindex would park on a broadcast nothing will
/// send to again. Reading it inside the open's lock hold is what keeps it
/// consistent with the replay handed back beside it.
///
/// What: pushes a terminal event, flips the status, and checks the open reports
/// both together.
///
/// Test: this function IS the test.
#[tokio::test]
async fn an_open_reports_the_status_beside_the_replay_it_snapshotted() {
    let progress = ReindexProgress::new();
    progress.push(serde_json::json!({ "seq": 0 })).await;
    progress.status.store(ReindexStatus::Complete);

    let (replay, status, _live) = progress.subscribe_with_replay().await;
    assert_eq!(status, ReindexStatus::Complete);
    assert_eq!(replay.len(), 1, "the terminal event is in the replay");
}
