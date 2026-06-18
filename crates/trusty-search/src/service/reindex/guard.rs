//! RAII termination guard for the reindex task.
//!
//! Why: a Rust panic inside a `tokio::spawn` task unwinds that task silently
//! — the `broadcast::Sender` in `ReindexProgress` drops, live SSE subscribers
//! never receive a terminal frame, and the CLI reports "stream ended without
//! completion event" indefinitely. Worse (issue #1428): on a CUDA build a
//! mid-batch GPU stall could cancel or panic the runner future, the guard set
//! `status=Failed` and broadcast a generic SSE error, but it logged **nothing**
//! to stderr — so the daemon log showed no error at all ("silent" `status=error`
//! with `duration=None`). Placing this guard at the start of the spawned future
//! and calling `disarm()` only after `emit_complete_event` completes ensures
//! that ANY early exit (panic, early return, or `.await` cancellation) emits
//! `{"event":"error","message":"…"}` **and** logs the underlying cause at
//! `error!` on stderr.
//!
//! What: holds `Arc<ReindexProgress>`, an `armed: bool` flag, and a shared
//! `failure_reason` slot. The runner records a specific cause via
//! `set_failure_reason` whenever it knows one (e.g. a captured producer-task
//! panic). `Drop` checks the flag — if still armed, it (1) logs the captured
//! reason (or a generic fallback) at `error!` to stderr so the daemon log is
//! never silent, (2) pushes a blocking-channel send of the error event directly
//! onto the broadcast sender (no `.await` in `Drop`; use the non-blocking
//! `send`), and (3) marks the progress status as `Failed`.
//!
//! Test: `reindex_guard_fires_on_early_return`,
//! `reindex_guard_uses_captured_failure_reason`, and
//! `reindex_guard_failure_reason_is_none_by_default` in `tests.rs`.

use std::sync::{Arc, Mutex};

use super::progress::{ReindexProgress, ReindexStatus};

/// Shared, writable slot carrying the most specific failure reason known to the
/// reindex task at the moment it exits early.
///
/// Why: the guard's `Drop` runs after the runner future has unwound, so it
/// cannot inspect local error values directly. The runner writes the cause it
/// observed (e.g. a producer-task `JoinError`, or an aborting carryover-copy
/// failure) into this slot **before** the early return; `Drop` then reads it so
/// the operator sees the real cause instead of "exited unexpectedly".
/// What: an `Arc<Mutex<Option<String>>>` cloneable handle. `None` means no
/// specific cause was recorded and `Drop` falls back to the generic message.
/// Test: `reindex_guard_uses_captured_failure_reason`.
pub(crate) type FailureReasonSlot = Arc<Mutex<Option<String>>>;

/// RAII guard that emits a terminal SSE error event AND an `error!` stderr log
/// if the reindex task exits without having emitted one via the normal path.
///
/// Why: ensures that ANY early exit from the spawned reindex task (panic,
/// early return, `.await` cancellation) is non-silent — it emits
/// `{"event":"error","message":"…"}` for live SSE subscribers and logs the
/// underlying cause at `error!` so the daemon log always records WHY a reindex
/// ended in `status=error` (issue #1428).
/// What: armed on construction; `Drop` fires the error event + stderr log
/// synchronously when still armed. Call `disarm()` after the normal terminal
/// event is emitted. Call `set_failure_reason()` to record a specific cause.
/// Test: `reindex_guard_fires_on_early_return`,
/// `reindex_guard_uses_captured_failure_reason`.
pub(crate) struct ReindexTerminationGuard {
    progress: Arc<ReindexProgress>,
    armed: bool,
    index_id: String,
    failure_reason: FailureReasonSlot,
}

impl ReindexTerminationGuard {
    /// Why: construct a guard that will fire an error event + stderr log on drop
    /// unless `disarm()` is called.
    /// What: sets `armed = true`; stores the `Arc<ReindexProgress>` with an
    /// unlabelled index id and an empty failure-reason slot.
    /// Test: `reindex_guard_fires_on_early_return`.
    pub fn new(progress: Arc<ReindexProgress>) -> Self {
        Self {
            progress,
            armed: true,
            index_id: String::new(),
            failure_reason: Arc::new(Mutex::new(None)),
        }
    }

    /// Why: the daemon log is most actionable when the silent-failure line names
    /// the index that died. The runner knows the id; pass it in so `Drop`'s
    /// `error!` line is greppable per-index.
    /// What: builder-style setter that stamps the index id used in the `error!`
    /// log and the SSE payload.
    /// Test: `reindex_guard_uses_captured_failure_reason` asserts the id appears.
    pub fn with_index_id(mut self, index_id: impl Into<String>) -> Self {
        self.index_id = index_id.into();
        self
    }

    /// Clone the shared failure-reason slot so the runner can record a specific
    /// cause out-of-band (e.g. a captured producer-task panic) before exiting.
    ///
    /// Why: `Drop` cannot see the runner's local error values; the slot is the
    /// hand-off. Handing back a clone (rather than a `&mut`) lets the runner
    /// write to it even after the guard has been moved into `FinishCtx`.
    /// What: returns an `Arc` clone of the inner `Mutex<Option<String>>`.
    /// Test: `reindex_guard_uses_captured_failure_reason`.
    pub fn failure_reason_slot(&self) -> FailureReasonSlot {
        Arc::clone(&self.failure_reason)
    }

    /// Record the most specific failure reason known at the call site.
    ///
    /// Why: lets a code path that detects a concrete fault (producer panic,
    /// abort) replace the generic "exited unexpectedly" message with the real
    /// cause, which `Drop` then both logs and broadcasts.
    /// What: writes `Some(reason)` into the shared slot (last writer wins).
    /// Recovers a poisoned mutex via `into_inner` so the cause is never lost —
    /// this whole path exists to handle panics, the exact thing that poisons the
    /// lock, so silently no-op'ing on poison would defeat its purpose (issue
    /// #1428 review follow-up).
    /// Test: `reindex_guard_uses_captured_failure_reason`.
    pub fn set_failure_reason(slot: &FailureReasonSlot, reason: impl Into<String>) {
        let mut guard = slot.lock().unwrap_or_else(|poisoned| {
            tracing::warn!(
                "reindex: failure-reason mutex was poisoned — recovering inner \
                 value so the captured cause is not lost"
            );
            poisoned.into_inner()
        });
        *guard = Some(reason.into());
    }

    /// Disarm the guard — call this after successfully emitting the terminal
    /// `complete` or `aborted_memory` event so `Drop` does not double-emit.
    ///
    /// Why: the orchestrator must commit to exactly one terminal SSE frame.
    /// `disarm()` marks that commitment — the guard's `Drop` is now a no-op.
    /// What: sets `self.armed = false`.
    /// Test: verified by the absence of a double-event in `reindex_guard_does_not_fire_after_disarm`.
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ReindexTerminationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // The task is exiting without having emitted a terminal event. Pull the
        // most specific cause the runner recorded; fall back to a generic hint.
        // Recover a poisoned mutex via `into_inner` rather than silently treating
        // it as "no captured reason": this `Drop` runs precisely on the panic
        // paths that poison the lock, so swallowing the reason would discard the
        // real cause the runner recorded just before unwinding (issue #1428
        // review follow-up).
        let captured = self
            .failure_reason
            .lock()
            .unwrap_or_else(|poisoned| {
                tracing::warn!(
                    "reindex: failure-reason mutex was poisoned in Drop — \
                     recovering inner value so the captured cause is not lost"
                );
                poisoned.into_inner()
            })
            .clone();
        let detail = captured.unwrap_or_else(|| {
            "reindex task exited unexpectedly (panic or cancellation) — \
             check daemon logs for details"
                .to_string()
        });

        // Issue #1428: ALWAYS log at `error!` to stderr so a `status=error`
        // reindex can never again be silent in the daemon log. Logs go to
        // stderr only (daemon stdout is reserved for MCP JSON-RPC framing).
        // Index id is carried in the human-readable `reindex[{}]:` prefix only
        // (no duplicate structured `index_id` field) so JSON log backends don't
        // double-emit it; `reindex[...]: terminated` greps still match (issue
        // #1428 review follow-up).
        if self.index_id.is_empty() {
            tracing::error!("reindex: terminated without a completion event — {detail}");
        } else {
            tracing::error!(
                "reindex[{}]: terminated without a completion event — {detail}",
                self.index_id,
            );
        }

        // Set the status and push the terminal error frame. `broadcast::send`
        // is non-blocking, so this is `Drop`-safe (no `.await`).
        self.progress.status.store(ReindexStatus::Failed);
        let msg = serde_json::json!({
            "event": "error",
            "index_id": self.index_id,
            "message": detail,
            "fatal": true,
        })
        .to_string();
        // `send` returns Err when there are no receivers; ignore — the replay
        // buffer is updated async by `push`, but Drop cannot be async. The
        // broadcast path alone is sufficient for live subscribers; late
        // subscribers reading the replay buffer will see the status as
        // `Failed` (and now an `error!` line in the daemon log).
        let _ = self.progress.sender.send(msg);
    }
}
