//! RAII termination guard for the reindex task.
//!
//! Why: a Rust panic inside a `tokio::spawn` task unwinds that task silently
//! — the `broadcast::Sender` in `ReindexProgress` drops, live SSE subscribers
//! never receive a terminal frame, and the CLI reports "stream ended without
//! completion event" indefinitely. Placing this guard at the start of the
//! spawned future and calling `disarm()` only after `emit_complete_event`
//! completes ensures that ANY early exit (panic, early return, or `.await`
//! cancellation) emits `{"event":"error","message":"…"}` before the sender
//! drops.
//!
//! What: holds `Arc<ReindexProgress>` and an `armed: bool` flag. `Drop`
//! checks the flag — if still armed, it pushes a blocking-channel send of
//! the error event directly onto the broadcast sender (no `.await` in `Drop`;
//! use `try_send`) and marks the progress status as `Failed`.
//!
//! Test: `reindex_guard_fires_on_early_return` in `tests.rs`.

use std::sync::Arc;

use super::progress::{ReindexProgress, ReindexStatus};

/// RAII guard that emits a terminal SSE error event if the reindex task exits
/// without having emitted one via the normal path.
///
/// Why: ensures that ANY early exit from the spawned reindex task (panic,
/// early return, `.await` cancellation) emits `{"event":"error","message":"…"}`
/// so live SSE subscribers are never left hanging.
/// What: armed on construction; `Drop` fires the error event synchronously
/// when still armed. Call `disarm()` after the normal terminal event is emitted.
/// Test: `reindex_guard_fires_on_early_return`.
pub(crate) struct ReindexTerminationGuard {
    progress: Arc<ReindexProgress>,
    armed: bool,
}

impl ReindexTerminationGuard {
    /// Why: construct a guard that will fire an error event on drop unless
    /// `disarm()` is called.
    /// What: sets `armed = true`; stores the `Arc<ReindexProgress>`.
    /// Test: `reindex_guard_fires_on_early_return`.
    pub fn new(progress: Arc<ReindexProgress>) -> Self {
        Self {
            progress,
            armed: true,
        }
    }

    /// Disarm the guard — call this after successfully emitting the terminal
    /// `complete` or `aborted_memory` event so `Drop` does not double-emit.
    ///
    /// Why: the orchestrator must commit to exactly one terminal SSE frame.
    /// `disarm()` marks that commitment — the guard's `Drop` is now a no-op.
    /// What: sets `self.armed = false`.
    /// Test: verified by the absence of a double-event in `reindex_guard_fires_on_early_return`.
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ReindexTerminationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // The task is exiting without having emitted a terminal event.
        // Set the status to Failed and push an error event synchronously
        // (broadcast::Sender::send is non-blocking).
        self.progress.status.store(ReindexStatus::Failed);
        let msg = serde_json::json!({
            "event": "error",
            "message": "reindex task exited unexpectedly — check daemon logs for details"
        })
        .to_string();
        // `send` returns Err when there are no receivers; ignore — the replay
        // buffer is updated async by `push`, but Drop cannot be async. The
        // broadcast path alone is sufficient for live subscribers; replay is
        // not updated here because we cannot `.await` in Drop. Live
        // subscribers connected at the time of the crash will see the frame;
        // late subscribers reading the replay buffer will see the status as
        // `Failed` and can surface that to the user via the /status endpoint.
        let _ = self.progress.sender.send(msg);
    }
}
