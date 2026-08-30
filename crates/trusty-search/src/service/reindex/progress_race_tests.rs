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
//! A terminal event has a second, wider window of its own.
//! [`a_terminal_status_is_not_observable_before_its_event_is_in_the_replay`] and
//! [`a_stream_opened_across_the_terminal_transition_always_gets_the_terminal_frame`]
//! cover it: the terminal transition used to store the status and push its frame
//! as two unlocked steps, and on the `Complete` path an RSS poll, a git
//! subprocess, a marker write and two `RwLock` writes sat between them. Both
//! producers stop reading the live channel once the status is terminal, so an
//! open in that window ended its stream with no terminal frame at all —
//! `ReindexProgress::push_terminal` now does both under one lock hold.
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

/// Open-versus-terminal-transition races to run.
const TERMINAL_RACE_TRIALS: usize = 512;
/// Concurrent openers per terminal-transition trial, spawned either side of the
/// emitter so some are always still opening when the transition lands.
const TERMINAL_RACE_OPENERS: usize = 8;
/// The `seq` the planted terminal frame carries. Far above any ordinary one so
/// an assertion message names it unambiguously.
const TERMINAL_SEQ: usize = 9_999;
/// How long the lock-holding test watches for a leaked terminal status before
/// concluding there is none. Proving an absence needs a bound; the watch returns
/// the instant a leak appears, so only the healthy path pays this in full. A bare
/// yield loop is NOT enough — it finishes before the emitter's worker is
/// scheduled at all and passes vacuously, which is how this test was first
/// written and why it went green against a reverted fix.
const STATUS_LEAK_WATCH: std::time::Duration = std::time::Duration::from_millis(250);
/// Gap between reads inside that watch — a real park, not a spin.
const STATUS_LEAK_POLL: std::time::Duration = std::time::Duration::from_millis(1);

/// The frame a terminal transition plants, shaped like the real `complete` event.
fn terminal_event() -> serde_json::Value {
    serde_json::json!({ "event": "complete", "seq": TERMINAL_SEQ })
}

/// Whether a replayed or broadcast line is the planted terminal frame.
fn is_terminal(line: &str) -> bool {
    seq_of(line) == TERMINAL_SEQ
}

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

/// Why: both stream producers stop reading the live channel the moment the
/// status they opened with is terminal, so for such a subscriber the replay is
/// the ONLY path the terminal frame can arrive on. A terminal transition used to
/// store the status and push its frame as two unlocked steps, with an RSS poll, a
/// git subprocess, a marker write and two `RwLock` writes between them on the
/// `Complete` path. An open in that window read `Complete`, refused to read live,
/// and got a replay with no terminal frame — its stream ended silently and its
/// client waited for a completion that had already happened.
///
/// What: holds the replay-buffer lock, starts a real `push_terminal` against it,
/// and checks the terminal status never becomes visible while that lock is held —
/// which is what makes the store and the frame one step. Releasing the lock then
/// proves the emission really did run, so the check above was not vacuous.
/// Reverting `push_terminal` to `status.store` followed by `push` fails this
/// immediately.
///
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_terminal_status_is_not_observable_before_its_event_is_in_the_replay() {
    let progress = Arc::new(ReindexProgress::new());
    progress.push(serde_json::json!({ "seq": 0 })).await;

    // Hold the replay-buffer lock: every emission and every open must wait on it.
    let held = progress.events.lock().await;

    let (at_call_tx, at_call_rx) = tokio::sync::oneshot::channel();
    let emitter = {
        let progress = Arc::clone(&progress);
        tokio::spawn(async move {
            at_call_tx
                .send(())
                .expect("the test outlives its emitter task");
            progress
                .push_terminal(ReindexStatus::Complete, terminal_event())
                .await;
        })
    };
    at_call_rx
        .await
        .expect("the emitter must reach its terminal emission");

    // The emitter is inside `push_terminal`, waiting on the lock this test holds.
    // Watch the status for a bounded window: a leaked store is visible as soon as
    // the emitter's worker runs, so this returns immediately when the invariant
    // breaks and only waits out the budget on the healthy path.
    let leaked = tokio::time::timeout(STATUS_LEAK_WATCH, async {
        loop {
            let seen = progress.status.load();
            if seen != ReindexStatus::Running {
                return seen;
            }
            tokio::time::sleep(STATUS_LEAK_POLL).await;
        }
    })
    .await;
    assert!(
        leaked.is_err(),
        "the terminal status became visible ({:?}) while the replay buffer was \
         still locked, so an open in this window reads a terminal status, stops \
         reading the live channel, and never sees the terminal frame (#6386)",
        leaked.unwrap_or(ReindexStatus::Running),
    );

    drop(held);
    emitter.await.expect("the terminal emission must not panic");

    let (replay, status, _live) = progress.subscribe_with_replay().await;
    assert_eq!(
        status,
        ReindexStatus::Complete,
        "releasing the lock must let the transition through — otherwise the \
         assertion above proved nothing"
    );
    assert!(
        replay.iter().any(|line| is_terminal(line)),
        "the terminal status and its frame must become visible together"
    );
}

/// Why: the production interleaving of the case above — real opens racing a real
/// terminal transition, on the multi-threaded runtime the daemon runs on. Each
/// opener is judged by the rule its own transport applies: an open reporting
/// `Running` reads the live channel, one reporting a terminal status reads only
/// the replay. Either way the terminal frame must reach it exactly once.
///
/// What: races `TERMINAL_RACE_OPENERS` opens against one `push_terminal` over
/// `TERMINAL_RACE_TRIALS` trials, half the openers spawned before the emitter and
/// half after.
///
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_opened_across_the_terminal_transition_always_gets_the_terminal_frame() {
    for trial in 0..TERMINAL_RACE_TRIALS {
        let progress = Arc::new(ReindexProgress::new());
        progress.push(serde_json::json!({ "seq": 0 })).await;

        let open = || {
            let progress = Arc::clone(&progress);
            tokio::spawn(async move { progress.subscribe_with_replay().await })
        };

        let mut openers: Vec<_> = (0..TERMINAL_RACE_OPENERS / 2).map(|_| open()).collect();
        let emitter = {
            let progress = Arc::clone(&progress);
            tokio::spawn(async move {
                progress
                    .push_terminal(ReindexStatus::Complete, terminal_event())
                    .await;
            })
        };
        openers.extend((0..TERMINAL_RACE_OPENERS / 2).map(|_| open()));

        emitter.await.expect("the emitter must not panic");

        for (opener_n, opener) in openers.into_iter().enumerate() {
            let (replay, status, mut live) = opener.await.expect("no opener may panic");
            let in_replay = replay.iter().any(|line| is_terminal(line));

            if status != ReindexStatus::Running {
                assert!(
                    in_replay,
                    "trial {trial} opener {opener_n}: the open read status {status:?} but its \
                     {replay_len}-event replay held no terminal frame — both producers stop \
                     reading the live channel on a terminal status, so this stream ends \
                     without one (#6386)",
                    replay_len = replay.len(),
                );
            }

            let seen = delivered(&replay, &mut live);
            let arrivals = seen.iter().filter(|seq| **seq == TERMINAL_SEQ).count();
            assert_eq!(
                arrivals,
                1,
                "trial {trial} opener {opener_n}: the terminal frame arrived {arrivals} times \
                 across the replay and the live channel, not once (status was {status:?}, \
                 replay held {replay_len})",
                replay_len = replay.len(),
            );
        }
    }
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
