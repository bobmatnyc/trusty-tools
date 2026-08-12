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
//! The bound loses work in TWO places, and this module counts both because a
//! health consumer must be able to tell them apart. A SUBMISSION the pool
//! refuses is a rejection ([`BoundedDispatcher::rejected`]) — the pool was full,
//! and nothing of that batch ran. A batch the pool ACCEPTED and started, then
//! cut short at [`crate::search_index::BATCH_INDEX_BUDGET`], is a truncation
//! ([`BoundedDispatcher::truncated`]) — part of it landed and the rest was
//! abandoned. The two have different causes and different fixes (a full pool
//! vs. a slow daemon making one batch overrun), so they are never summed into
//! one number.
//!
//! Test: `dispatcher_rejects_submissions_once_workers_and_queue_are_full`,
//! `no_more_jobs_run_concurrently_than_the_worker_count`,
//! `a_panicking_job_does_not_kill_its_worker`, `a_fresh_pool_reports_no_drop_ever`,
//! `a_rejection_records_when_it_happened`,
//! `a_truncation_is_counted_apart_from_a_rejection` (in the `tests` module
//! below), and the end-to-end
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

/// How many times one kind of work loss has happened, and when the last one was.
///
/// Why: both loss modes in the module doc need exactly this pair, and a health
/// consumer needs both halves — the total says whether it has EVER happened,
/// the stamp says whether it is happening NOW. One type so the two halves
/// cannot drift apart, and so a later loss mode inherits the shape instead of
/// growing a third near-copy of `fetch_add` + `SystemTime::now`.
/// What: `count` is monotonic for the life of the process. `last_unix_secs`
/// stores `0` until the first [`LossCounter::record`], which is why
/// [`LossCounter::last_unix_secs`] hands back an `Option` rather than leaking
/// that sentinel to callers.
/// Test: `a_fresh_pool_reports_no_drop_ever`,
/// `a_rejection_records_when_it_happened`,
/// `a_truncation_is_counted_apart_from_a_rejection`.
#[derive(Default)]
pub(crate) struct LossCounter {
    count: AtomicU64,
    last_unix_secs: AtomicU64,
}

impl LossCounter {
    /// Count one loss and stamp when it happened.
    fn record(&self) {
        self.count.fetch_add(1, Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.last_unix_secs.store(now, Ordering::Relaxed);
    }

    /// How many losses of this kind since process start.
    fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Unix seconds of the most recent loss, or `None` if there has never been
    /// one.
    fn last_unix_secs(&self) -> Option<u64> {
        match self.last_unix_secs.load(Ordering::Relaxed) {
            0 => None,
            secs => Some(secs),
        }
    }
}

/// A fixed worker pool with a bounded queue, rejecting work when both are full.
///
/// Why / What / design rationale: see the module doc, including why the two
/// loss counters below stay separate numbers.
/// Test: the module's `tests` submodule constructs small instances
/// (`BoundedDispatcher::new(1, 1)`) so the saturation boundary is exercised
/// deterministically rather than by racing the shared pool.
pub(crate) struct BoundedDispatcher {
    tx: SyncSender<IndexJob>,
    /// Batches refused at SUBMISSION because workers and queue were both full.
    rejected: LossCounter,
    /// Batches accepted and started, then cut short by the per-batch budget.
    truncated: LossCounter,
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
            rejected: LossCounter::default(),
            truncated: LossCounter::default(),
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
                self.rejected.record();
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                self.rejected.record();
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
        self.rejected.count()
    }

    /// Unix seconds of the most recent rejection, or `None` if there has never
    /// been one.
    ///
    /// Why: lets a health consumer separate "no drops ever" from "drops right
    /// now" — see [`crate::search_index::index_drop_stats`].
    /// Test: `a_fresh_pool_reports_no_drop_ever`,
    /// `a_rejection_records_when_it_happened`.
    pub(crate) fn last_drop_unix_secs(&self) -> Option<u64> {
        self.rejected.last_unix_secs()
    }

    /// Count a batch that ran but was cut short by its per-batch budget.
    ///
    /// Why: the budget's `break` used to be a `warn!` and nothing else, which
    /// is the same single-reader blind spot the rejection counter exists to
    /// close. A saturation episode where every batch is ACCEPTED but repeatedly
    /// truncated leaves files unindexed batch after batch while
    /// `dropped_batches` stays `0` forever, so the loss needs its own readable
    /// number. Recorded here rather than in a second global because this pool
    /// is already the one process-wide object the health surface reads.
    /// What: called by [`crate::search_index`]'s budget guard, from a worker
    /// thread — hence the same relaxed atomics as the rejection path.
    /// Test: `a_truncation_is_counted_apart_from_a_rejection`,
    /// and `a_truncated_batch_is_counted_separately_from_a_dropped_one` in
    /// `search_index_tests.rs` for the call site.
    pub(crate) fn record_truncation(&self) {
        self.truncated.record();
    }

    /// How many batches this pool has started and then cut short, since process
    /// start.
    pub(crate) fn truncated(&self) -> u64 {
        self.truncated.count()
    }

    /// Unix seconds of the most recent truncation, or `None` if there has never
    /// been one.
    pub(crate) fn last_truncation_unix_secs(&self) -> Option<u64> {
        self.truncated.last_unix_secs()
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
