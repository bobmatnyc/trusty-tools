//! Reindex progress state: status enum, progress snapshot, and replay buffer.
//!
//! Why: the `ReindexProgress` struct is the central coordination point between
//! the background reindex task and the SSE stream handler — it is also the
//! type stored on `SearchAppState::reindex_progress`. Extracting it here keeps
//! the orchestrator focused on control flow and makes progress types easy to
//! import without pulling in the full orchestrator.
//!
//! What: `ReindexStatus` (terminal state enum) and `ReindexProgress` (live
//! counters + broadcast channel + replay buffer).
//!
//! Test: see `crates/trusty-search/src/service/reindex/tests.rs`.

use crossbeam_utils::atomic::AtomicCell;
use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

/// Capacity of the per-reindex broadcast channel. Lagged subscribers will
/// drop events older than this — the SSE handler also replays from the buffer
/// stored in `events`, so late subscribers still see the full history.
pub(super) const BROADCAST_CAPACITY: usize = 256;

/// Max replay events buffered on a `ReindexProgress`. A full reindex emits
/// ~100 events for a 14k-file repo (one per batch + start/complete), but
/// pathological cases (per-file errors) could otherwise grow the vector
/// without bound. Late SSE subscribers still see the most recent 500 events,
/// which is more than enough to replay context.
pub(super) const MAX_REPLAY_EVENTS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReindexStatus {
    Running,
    Complete,
    /// Issue #120: the reindex aborted because the soft RSS ceiling
    /// (`TRUSTY_MEMORY_LIMIT_MB`) was breached. Distinguished from `Complete`
    /// so external callers can apply a cooldown before retrying — re-running
    /// immediately would just hit the limit again, producing an infinite
    /// reindex loop.
    AbortedMemory,
    Failed,
}

/// Live state of a reindex. Wrapped in `Arc` and stored on
/// `SearchAppState::reindex_progress` so concurrent SSE subscribers can read
/// the same snapshot without coordinating.
pub struct ReindexProgress {
    pub status: AtomicCell<ReindexStatus>,
    pub total_files: AtomicUsize,
    pub indexed: AtomicUsize,
    pub total_chunks: AtomicUsize,
    pub errors: AtomicUsize,
    /// Files skipped because their content hash matched the previous reindex.
    pub skipped: AtomicUsize,
    /// Issue #100: number of chunks dropped during the most recent reindex
    /// because the per-index `TRUSTY_MAX_CHUNKS` cap was reached. Non-zero ⇒
    /// the index is incomplete and downstream search results may miss code
    /// from the tail of the walk. Surfaced via `GET /indexes/:id/status` as
    /// `walk_truncated_by_budget` (boolean) and `chunks_dropped_by_cap`
    /// (count) so operators can distinguish a clean index from one that
    /// silently lost source.
    pub chunks_dropped_by_cap: AtomicUsize,
    /// Append-only log of JSON-encoded events. Replayed to late SSE
    /// subscribers so they don't miss earlier `start` / `progress` events.
    pub events: Arc<Mutex<Vec<String>>>,
    /// Live event broadcaster. Subscribers receive new events as they're sent.
    pub sender: broadcast::Sender<String>,
}

impl ReindexProgress {
    /// Why: constructs an armed, running progress tracker with an empty replay
    /// buffer and a fresh broadcast channel.
    /// What: initialises all counters to zero, status to `Running`, and
    /// creates a broadcast channel with `BROADCAST_CAPACITY`.
    /// Test: covered by reindex integration tests via `reindex_handler`.
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            status: AtomicCell::new(ReindexStatus::Running),
            total_files: Default::default(),
            indexed: Default::default(),
            total_chunks: Default::default(),
            errors: Default::default(),
            skipped: Default::default(),
            chunks_dropped_by_cap: Default::default(),
            events: Arc::new(Mutex::new(Vec::new())),
            sender,
        }
    }

    /// Push an event onto the replay buffer and broadcast it to live subscribers.
    /// Caps the replay buffer at `MAX_REPLAY_EVENTS` to bound memory under
    /// pathological reindexes (e.g. one error event per file).
    ///
    /// Why: central emission point for every non-terminal event — the replay
    /// buffer and the live broadcast channel stay in sync because the append and
    /// the broadcast happen under ONE hold of the `events` lock, which is what
    /// makes an emission atomic with respect to
    /// [`subscribe_with_replay`](Self::subscribe_with_replay) — see #6386 and the
    /// exactly-once argument on that method. A TERMINAL event goes through
    /// [`push_terminal`](Self::push_terminal) instead, which stores the status
    /// under that same hold.
    /// What: appends to the in-memory replay buffer (dropping the oldest event
    /// when over-cap) and sends on the broadcast channel (errors are benign —
    /// replay buffer retains the event for late subscribers).
    /// Test: `reindex_walks_directory_and_emits_events`;
    /// `no_event_is_lost_or_duplicated_when_a_stream_opens_against_live_pushes`
    /// in `progress_race_tests.rs` for the atomicity this ordering buys.
    pub async fn push(&self, event: serde_json::Value) {
        let line = event.to_string();
        let mut buf = self.events.lock().await;
        self.emit_locked(&mut buf, line);
    }

    /// Store the terminal status and emit its event as ONE atomic step.
    ///
    /// Why: a terminal transition used to be two unlocked steps — `status.store`
    /// followed by [`push`](Self::push) — and on the `Complete` path an RSS poll,
    /// a git subprocess, a marker-file write and two `RwLock` writes sat between
    /// them. A [`subscribe_with_replay`](Self::subscribe_with_replay) landing in
    /// that window read a terminal status while the replay buffer still lacked
    /// the terminal event. Both transports stop reading the live channel once the
    /// status is terminal, so that subscriber's stream ended with no terminal
    /// frame at all and its client waited on one that would never come. See
    /// #6386.
    /// What: takes the `events` lock once and, under that single hold, stores the
    /// status, appends the event to the replay buffer, and broadcasts it — so no
    /// open can observe the status and the buffer disagreeing. Identical to
    /// `push` in every respect but the status store.
    /// Test: `a_terminal_status_is_not_observable_before_its_event_is_in_the_replay`
    /// and `a_stream_opened_across_the_terminal_transition_always_gets_the_terminal_frame`
    /// in `progress_race_tests.rs`.
    pub async fn push_terminal(&self, status: ReindexStatus, event: serde_json::Value) {
        let line = event.to_string();
        let mut buf = self.events.lock().await;
        // #6386: inside the hold, so an open either predates the whole
        // transition or sees both halves of it.
        self.status.store(status);
        self.emit_locked(&mut buf, line);
    }

    /// Append to the replay buffer and broadcast, under a lock the caller holds.
    ///
    /// Why: `push` and `push_terminal` differ only in whether they also store the
    /// terminal status, and both must append and broadcast under ONE hold of the
    /// `events` lock (#6386). Sharing the body keeps the two emission paths from
    /// drifting apart.
    /// What: evicts the oldest entry once at `MAX_REPLAY_EVENTS`, appends, then
    /// sends on the broadcast channel. `broadcast::Sender::send` is synchronous
    /// and never blocks on a receiver, so the caller's critical section stays
    /// short and cannot deadlock.
    /// Test: every test that calls `push` or `push_terminal` exercises it.
    fn emit_locked(&self, buf: &mut Vec<String>, line: String) {
        if buf.len() >= MAX_REPLAY_EVENTS {
            // Drop the oldest event. `remove(0)` is O(n) but n ≤ 500.
            buf.remove(0);
        }
        buf.push(line.clone());
        // Broadcast errors (no receivers) are fine — replay buffer still has it.
        let _ = self.sender.send(line);
    }

    /// Open a stream: the replay buffer, the status, and a live subscription,
    /// taken as one atomic step.
    ///
    /// Why: this is the ONE place a reindex stream may open, and it exists so the
    /// snapshot and the subscription cannot straddle a [`push`](Self::push).
    /// Both transports — the `GET /indexes/{id}/reindex/stream` SSE route and the
    /// `search.index.reindex.stream` socket method — call it, so neither can
    /// re-introduce the #6386 race by hand-ordering the two steps again.
    ///
    /// What: acquires the `events` lock, clones the replay buffer, reads the
    /// status, and subscribes to the broadcast channel, releasing the lock only
    /// once all three are in hand.
    ///
    /// The exactly-once argument, for any event emitted through
    /// [`push`](Self::push) or [`push_terminal`](Self::push_terminal), each of
    /// which appends AND broadcasts under this same lock: the two critical
    /// sections are ordered, so for any event either the emission ran first — its
    /// line is in the returned buffer, and its broadcast preceded the
    /// subscription, so the receiver never sees it — or this ran first, and the
    /// line is absent from the buffer while the subscription predates the
    /// broadcast. Every such event lands in exactly one of the two paths. Before
    /// #6386 the broadcast sat outside `push`'s lock and the subscribe outside the
    /// snapshot's, so an event could land in both (a duplicate) or in neither (a
    /// LOST event).
    ///
    /// A terminal event carries the status with it, so the same argument covers
    /// the `status` returned beside the buffer: an open never reads a terminal
    /// status from a buffer that does not yet hold the terminal frame. That
    /// mattered because both transports stop reading the live channel on a
    /// terminal status, so such an open ended its stream with no terminal frame.
    ///
    /// The terminal frame `ReindexTerminationGuard::drop` emits is the sole
    /// remaining exception — `Drop` cannot await the lock, so it broadcasts and
    /// stores the status without one. A subscriber that opens after that frame
    /// reads the `Failed` status rather than the frame; that predates #6386 and is
    /// unchanged.
    ///
    /// Test: `no_event_is_lost_or_duplicated_when_a_stream_opens_against_live_pushes`
    /// and `an_event_pushed_either_side_of_an_open_arrives_on_exactly_one_path` in
    /// `progress_race_tests.rs`;
    /// `an_event_either_side_of_the_stream_opening_is_delivered_exactly_once` in
    /// `service::rpc::streams_tests` drives it end to end over both transports.
    pub async fn subscribe_with_replay(
        &self,
    ) -> (Vec<String>, ReindexStatus, broadcast::Receiver<String>) {
        let buffered = self.events.lock().await;
        (
            buffered.as_slice().to_vec(),
            self.status.load(),
            self.sender.subscribe(),
        )
    }

    /// Load the current `indexed` counter with `Acquire` ordering.
    ///
    /// Why: convenience accessor used by batch helpers that need the file
    /// position without holding the full progress reference mutably.
    /// What: `AtomicUsize::load` with `Acquire`.
    /// Test: covered indirectly by all batch-processing tests.
    pub fn indexed_count(&self) -> usize {
        self.indexed.load(Ordering::Acquire)
    }
}

impl Default for ReindexProgress {
    fn default() -> Self {
        Self::new()
    }
}
