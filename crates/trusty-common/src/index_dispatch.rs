//! Bounded dispatch for the crate's best-effort background indexing work
//! (issue #2798).
//!
//! Why: [`crate::search_index::index_files_best_effort`] used to call
//! `std::thread::spawn` once per write/edit tool call with nothing capping how
//! many of those threads could be alive at once. Against a *degraded but
//! reachable* trusty-search daemon each thread lives far longer than usual —
//! #2785's bounded retry means up to ~6.2s per file (3 attempts × a 2s client
//! timeout, plus backoff) — so a burst of writes spawns threads faster than
//! they drain and the process accumulates them without limit. Nothing in the
//! old path pushed back, which is exactly why a slow daemon turned into
//! unbounded resource growth in the client.
//!
//! What: a fixed-size worker pool fed by a BOUNDED queue. At most
//! [`MAX_INDEX_WORKERS`] jobs run concurrently and at most
//! [`INDEX_QUEUE_CAPACITY`] more wait behind them. Submission never blocks the
//! caller (a tool executor mid-turn), so the saturation behaviour is the third
//! option: **the batch is REJECTED**. Rejection is counted
//! ([`BoundedDispatcher::rejected`]) and the caller logs at `warn` naming what
//! it dropped — see [`crate::search_index::index_files_best_effort`]. Blocking
//! was rejected as a design because it would convert a slow daemon into a
//! stalled agent task, the precise failure the whole fail-open module exists to
//! avoid; an unbounded queue was rejected because it only moves unbounded
//! growth from threads to memory.
//!
//! Queued work is not stale work: each job reads the file's content from disk
//! at EXECUTION time, so a batch that waits in the queue indexes whatever the
//! file says when it finally runs.
//!
//! Test: `dispatcher_rejects_submissions_once_workers_and_queue_are_full`,
//! `no_more_jobs_run_concurrently_than_the_worker_count`,
//! `a_panicking_job_does_not_kill_its_worker` (in the `tests` module below),
//! and the end-to-end
//! `index_files_best_effort_drops_the_batch_when_the_shared_pool_is_saturated`
//! in `search_index_tests.rs`.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};

/// A unit of background indexing work.
pub(crate) type IndexJob = Box<dyn FnOnce() + Send + 'static>;

/// How many indexing jobs may run at once, process-wide.
///
/// Why: four concurrent per-file POSTs is more than enough to keep a healthy
/// local daemon busy, and it is a hard ceiling on the OS threads a degraded one
/// can tie up. Test: `no_more_jobs_run_concurrently_than_the_worker_count`.
pub(crate) const MAX_INDEX_WORKERS: usize = 4;

/// How many batches may wait behind the busy workers before submissions are
/// rejected.
///
/// Why: deep enough that a realistic write burst (the #2798 bake-off did ~29
/// files over 13.5 minutes) never reaches it, shallow enough that a wedged
/// daemon cannot grow the backlog without limit. Test:
/// `dispatcher_rejects_submissions_once_workers_and_queue_are_full`.
pub(crate) const INDEX_QUEUE_CAPACITY: usize = 64;

/// A fixed worker pool with a bounded queue, rejecting work when both are full.
///
/// Why / What / design rationale: see the module doc.
/// Test: the module's `tests` submodule constructs small instances
/// (`BoundedDispatcher::new(1, 1)`) so the saturation boundary is exercised
/// deterministically rather than by racing the shared pool.
pub(crate) struct BoundedDispatcher {
    tx: SyncSender<IndexJob>,
    rejected: AtomicU64,
}

impl BoundedDispatcher {
    /// Spawn `workers` worker threads draining a queue of `queue_capacity`.
    ///
    /// Why: the sizes are parameters rather than constants so tests can pin the
    /// saturation boundary with a 1-worker/1-slot pool instead of submitting 69
    /// jobs to reach the production one.
    /// What: creates a bounded `sync_channel` and starts `workers` threads (at
    /// least one — a zero-worker pool would silently reject everything) that
    /// loop on the shared receiver until the sender is dropped. The threads are
    /// detached and live for the life of the pool.
    /// Test: `no_more_jobs_run_concurrently_than_the_worker_count`.
    pub(crate) fn new(workers: usize, queue_capacity: usize) -> Self {
        let (tx, rx) = sync_channel::<IndexJob>(queue_capacity);
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..workers.max(1) {
            let rx = Arc::clone(&rx);
            std::thread::spawn(move || worker_loop(&rx));
        }
        Self {
            tx,
            rejected: AtomicU64::new(0),
        }
    }

    /// Queue `job` if there is room, otherwise reject it — never blocks.
    ///
    /// Why: the caller is a tool executor mid-turn whose contract is to return
    /// immediately; blocking here would stall an agent task behind a slow
    /// daemon. See the module doc for why rejection (not blocking, not an
    /// unbounded queue) is the saturation behaviour.
    /// What: returns `true` when the job was accepted. Returns `false` — and
    /// increments [`BoundedDispatcher::rejected`] — when the queue is full or,
    /// pathologically, no worker is alive to receive. `#[must_use]` because a
    /// dropped indexing batch must be reported by whoever knows what it
    /// contained, never swallowed.
    /// Test: `dispatcher_rejects_submissions_once_workers_and_queue_are_full`.
    #[must_use]
    pub(crate) fn try_submit(&self, job: IndexJob) -> bool {
        match self.tx.try_send(job) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    "background index dispatch pool has no live workers; work is being \
                     dropped (issue #2798)"
                );
                false
            }
        }
    }

    /// How many jobs this pool has rejected since process start.
    pub(crate) fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }
}

/// Drain jobs until the sender is gone, surviving a panicking job.
///
/// Why: a job that panics must not silently retire a worker — losing all
/// [`MAX_INDEX_WORKERS`] that way would turn every later submission into a
/// rejection, converting the bound into a permanent outage.
/// What: locks the shared receiver only for the `recv` itself (so the other
/// workers can take the next job while this one runs), then executes the job
/// inside `catch_unwind`, logging a panic at `warn`. A poisoned mutex is
/// recovered rather than propagated. Returns when the channel disconnects.
/// Test: `a_panicking_job_does_not_kill_its_worker`.
fn worker_loop(rx: &Mutex<Receiver<IndexJob>>) {
    loop {
        let received = {
            let guard = match rx.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.recv()
        };
        let Ok(job) = received else {
            return;
        };
        if std::panic::catch_unwind(AssertUnwindSafe(job)).is_err() {
            tracing::warn!("background index job panicked; its worker continues (issue #2798)");
        }
    }
}

/// The process-wide pool every incremental index update flows through.
///
/// Why: the bound only means anything if it is SHARED — a per-call pool would
/// reproduce the unbounded spawn it replaces. Lazily initialised so a process
/// that never indexes pays for no threads.
/// What: a `OnceLock`-backed [`BoundedDispatcher`] sized
/// [`MAX_INDEX_WORKERS`] × [`INDEX_QUEUE_CAPACITY`].
/// Test: `index_files_best_effort_drops_the_batch_when_the_shared_pool_is_saturated`.
pub(crate) fn global() -> &'static BoundedDispatcher {
    static DISPATCHER: OnceLock<BoundedDispatcher> = OnceLock::new();
    DISPATCHER.get_or_init(|| BoundedDispatcher::new(MAX_INDEX_WORKERS, INDEX_QUEUE_CAPACITY))
}

// Tests live in a sibling file so this module stays well under the 500-SLOC
// production cap; as a child module it still reaches private items via `super::`.
#[cfg(test)]
#[path = "index_dispatch_tests.rs"]
mod tests;
