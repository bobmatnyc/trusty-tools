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
//! never silent, and (2) marks the progress status `Failed` and emits the error
//! event through `ReindexProgress::push_terminal_from_drop`, which takes the
//! `events` lock with a bounded `try_lock` rather than an `.await` (#9933).
//!
//! #10738: a staged run also hands the guard its staging corpus, its staged
//! HNSW snapshot and the index permit. A run that dies before `finish`
//! resolves them leaves staging attached, so `Drop` refuses writes at once
//! and hands the rollback to a spawned task (see [`StagedRollback`]).
//!
//! Test: `reindex_guard_fires_on_early_return`,
//! `reindex_guard_uses_captured_failure_reason`,
//! `reindex_guard_failure_reason_is_none_by_default`, and
//! `reindex_guard_omits_index_id_when_never_stamped` in `tests.rs`;
//! `a_staged_run_dropped_mid_batch_refuses_or_rolls_back` in
//! `identity_10738_tests.rs`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedRwLockReadGuard, OwnedSemaphorePermit};

use super::finish_teardown::{resolve_hnsw_swap, rollback_staged_corpus_swap};
use super::hnsw_swap::{begin_staged_hnsw_swap, HnswSwapPaths};
use super::progress::{ReindexProgress, ReindexStatus};
use super::semaphore::index_cancel_flag;
use super::staging::StagingResolution;
use crate::core::registry::IndexHandle;

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
    /// `None` until [`ReindexTerminationGuard::with_index_id`] stamps it.
    ///
    /// Why: #1451 — this was a `String` defaulting to `""`, and `Drop`'s SSE
    /// payload serialized it unconditionally, so a guard that fired before the
    /// id was known sent `"index_id": ""`. A consumer routing on `index_id`
    /// then had an error frame naming an index that does not exist rather than
    /// one it could see was unattributed. `None` omits the field instead.
    /// Test: `reindex_guard_omits_index_id_when_never_stamped`.
    index_id: Option<String>,
    failure_reason: FailureReasonSlot,
    /// #10738: the stagings `finish` has not resolved yet; `None` before
    /// staging begins.
    staged: Option<StagedRollback>,
}

/// A staged run's unresolved swap, rolled back if the run dies (#10738).
///
/// Why: a run that panics or is cancelled between staging and `finish`
/// left the staging corpus attached. Puts kept landing there under the old
/// generation, and the next restart's attach discarded them.
/// What: the handle, the staging corpus path and HNSW paths not yet
/// settled, the indexer's `swap_abandoned` flag, the index permit, which
/// keeps a new run off the index until the rollback returns, and the shared
/// side of the teardown lock, which keeps a delete off it (a delete takes
/// only the exclusive side).
/// Test: `a_staged_run_dropped_mid_batch_refuses_or_rolls_back`,
/// `a_delete_during_a_dropped_runs_rollback_leaves_no_data_or_identity`.
struct StagedRollback {
    handle: Arc<IndexHandle>,
    corpus_tmp: Option<PathBuf>,
    hnsw: Option<HnswSwapPaths>,
    abandoned: Option<Arc<AtomicBool>>,
    _permit: OwnedSemaphorePermit,
    _teardown: OwnedRwLockReadGuard<()>,
}

impl StagedRollback {
    /// Refuse writes now and hand the rollback to a spawned task.
    ///
    /// Why: `Drop` cannot await, and the rollback deletes and reopens files.
    /// What: sets the `swap_abandoned` flag when a staging corpus is attached,
    /// so `refuse_if_corpus_detached` refuses every write with no lock taken,
    /// then spawns [`Self::roll_back`] on the current runtime. With no runtime
    /// the flag stays set: writes are refused until a restart, whose attach
    /// raises the generation.
    /// Test: `a_staged_run_dropped_mid_batch_refuses_or_rolls_back`.
    fn hand_off(mut self, index_id: &str) {
        if self.corpus_tmp.is_none() && self.hnsw.is_none() {
            return;
        }
        if self.corpus_tmp.is_some() {
            if self.abandoned.is_none() {
                let indexer = self.handle.indexer.try_read().ok();
                self.abandoned = indexer.map(|indexer| indexer.swap_abandoned_flag());
            }
            if let Some(flag) = &self.abandoned {
                flag.store(true, Ordering::Release);
            }
        }
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                tracing::error!(
                    "reindex[{index_id}]: rolling back the staged swap of a run that \
                     died mid-run; writes are refused until live is re-attached"
                );
                runtime.spawn(self.roll_back());
            }
            Err(e) => tracing::error!(
                "reindex[{index_id}]: no runtime to roll back the staged swap on ({e}); \
                 writes stay refused and the next attach raises the generation"
            ),
        }
    }

    /// The `finish` rollback, run for a staged run that died (#10738).
    ///
    /// Why: one rollback path, so a dead run ends exactly as a failed run does.
    /// What: rolls back the staged HNSW snapshot and the staging corpus
    /// (re-attach live or quarantine, then raise the generation), then clears
    /// `swap_abandoned`. A flag `Drop` could not read is set here first, under
    /// the read lock. A delete that signalled the index's cancel flag is
    /// waiting on the teardown lock this holds: the rollback then only takes
    /// staging out, so it neither reopens live (which would recreate the data
    /// directory the delete removes) nor bumps the identity the delete
    /// retires. A failed bump leaves `swap_abandoned` set, so writes stay
    /// refused until a restart. The permit and the teardown lock are released
    /// when this returns.
    /// Test: `a_staged_run_dropped_mid_batch_refuses_or_rolls_back`,
    /// `a_delete_during_a_dropped_runs_rollback_leaves_no_data_or_identity`,
    /// `a_dropped_runs_rollback_whose_bump_fails_keeps_refusing_puts`.
    async fn roll_back(self) {
        let handle = &self.handle;
        let abandoned = match (&self.corpus_tmp, &self.abandoned) {
            (None, _) => None,
            (Some(_), Some(flag)) => Some(Arc::clone(flag)),
            (Some(_), None) => {
                let flag = handle.indexer.read().await.swap_abandoned_flag();
                flag.store(true, Ordering::Release);
                Some(flag)
            }
        };
        // #10738: the delete signals before it waits on the teardown lock held here.
        let cancel = index_cancel_flag(&handle.id);
        let reason = "the reindex task died mid-run (#10738)";
        let resolution = StagingResolution::Rollback {
            reason: reason.into(),
        };
        if !cancel.load(Ordering::Acquire) {
            resolve_hnsw_swap(handle, &handle.id, self.hnsw.as_ref(), &resolution).await;
        }
        let settled = match &self.corpus_tmp {
            None => true,
            Some(_) if cancel.load(Ordering::Acquire) => {
                tracing::warn!(
                    "reindex[{}]: index is being deleted; the dead run's staging \
                     corpus is taken out and live is not reopened (#10738)",
                    handle.id.0
                );
                drop(handle.indexer.write().await.take_corpus_store());
                true
            }
            Some(tmp) => rollback_staged_corpus_swap(handle, &handle.id, tmp, Some(reason)).await,
        };
        match abandoned {
            Some(flag) if settled => flag.store(false, Ordering::Release),
            Some(_) => tracing::error!(
                "reindex[{}]: the rollback's generation bump failed; writes stay \
                 refused until a restart, whose attach raises the generation (#10738)",
                handle.id.0
            ),
            None => {}
        }
    }
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
            index_id: None,
            failure_reason: Arc::new(Mutex::new(None)),
            staged: None,
        }
    }

    /// Begin the staged HNSW swap and arm the rollback of both stagings.
    ///
    /// Why: #10738 — the guard is the only code that runs when the task dies,
    /// so it must own what a death leaves behind.
    /// What: records the handle, `corpus_tmp`, the index permit and the
    /// run's shared teardown guard before any await, then the indexer's
    /// `swap_abandoned` flag, then runs `begin_staged_hnsw_swap` and records
    /// its paths. Returns those paths.
    /// Test: `a_staged_run_dropped_mid_batch_refuses_or_rolls_back`.
    pub(super) async fn begin_staged(
        &mut self,
        handle: &Arc<IndexHandle>,
        corpus_tmp: &Option<PathBuf>,
        permit: OwnedSemaphorePermit,
        teardown: OwnedRwLockReadGuard<()>,
    ) -> Option<HnswSwapPaths> {
        self.staged = Some(StagedRollback {
            handle: Arc::clone(handle),
            corpus_tmp: corpus_tmp.clone(),
            hnsw: None,
            abandoned: None,
            _permit: permit,
            _teardown: teardown,
        });
        let flag = handle.indexer.read().await.swap_abandoned_flag();
        if let Some(staged) = self.staged.as_mut() {
            staged.abandoned = Some(flag);
        }
        let paths = begin_staged_hnsw_swap(handle, &handle.id).await;
        if let Some(staged) = self.staged.as_mut() {
            staged.hnsw = paths.clone();
        }
        paths
    }

    /// `finish` resolves the staged HNSW snapshot from here on (#10738).
    pub(super) fn settle_hnsw(&mut self) {
        if let Some(staged) = self.staged.as_mut() {
            staged.hnsw = None;
        }
    }

    /// `finish` resolves the staging corpus from here on (#10738).
    pub(super) fn settle_corpus(&mut self) {
        if let Some(staged) = self.staged.as_mut() {
            staged.corpus_tmp = None;
        }
    }

    /// Why: the daemon log is most actionable when the silent-failure line names
    /// the index that died. The runner knows the id; pass it in so `Drop`'s
    /// `error!` line is greppable per-index.
    /// What: builder-style setter that stamps the index id used in the `error!`
    /// log and the SSE payload. Until it is called the payload carries no
    /// `index_id` field at all (#1451).
    /// Test: `reindex_guard_uses_captured_failure_reason` asserts the id appears.
    pub fn with_index_id(mut self, index_id: impl Into<String>) -> Self {
        self.index_id = Some(index_id.into());
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
            .as_ref()
            .cloned();
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
        match self.index_id.as_deref() {
            None => tracing::error!("reindex: terminated without a completion event — {detail}"),
            Some(id) => {
                tracing::error!("reindex[{id}]: terminated without a completion event — {detail}")
            }
        }

        // #1451: omit `index_id` entirely when it was never stamped — an empty
        // string is a value a consumer routing on this field would act on.
        let mut payload = serde_json::Map::new();
        payload.insert("event".into(), "error".into());
        if let Some(id) = self.index_id.as_deref() {
            payload.insert("index_id".into(), id.into());
        }
        payload.insert("message".into(), detail.into());
        payload.insert("fatal".into(), true.into());
        let msg = serde_json::Value::Object(payload).to_string();
        // #9933: status and frame in one hold of the `events` lock when it is
        // free, so a live stream never reads `Failed` without the frame queued.
        // Non-blocking: bounded `try_lock`, no `.await`.
        self.progress
            .push_terminal_from_drop(ReindexStatus::Failed, msg);

        // #10738: a run that died before `finish` resolved its stagings left
        // staging attached; refuse writes now and roll it back.
        if let Some(staged) = self.staged.take() {
            staged.hand_off(self.index_id.as_deref().unwrap_or("?"));
        }
    }
}
