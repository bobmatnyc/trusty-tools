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
    /// Why: central event emission point — all SSE events funnel through here
    /// so the replay buffer and live broadcast channel stay in sync.
    /// What: appends to the in-memory replay buffer (dropping the oldest event
    /// when over-cap) and sends on the broadcast channel (errors are benign —
    /// replay buffer retains the event for late subscribers).
    /// Test: covered by `reindex_walks_directory_and_emits_events`.
    pub async fn push(&self, event: serde_json::Value) {
        let line = event.to_string();
        {
            let mut buf = self.events.lock().await;
            if buf.len() >= MAX_REPLAY_EVENTS {
                // Drop the oldest event. `remove(0)` is O(n) but n ≤ 500.
                buf.remove(0);
            }
            buf.push(line.clone());
        }
        // Broadcast errors (no receivers) are fine — replay buffer still has it.
        let _ = self.sender.send(line);
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
