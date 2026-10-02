//! Size-ordered dispatch queue for the deferred-embed (C2) catch-up pass
//! (issue #3748 slice A).
//!
//! Why: before this module, `defer_embed::spawn_deferred_embed_pass` spawned
//! one `tokio::task` per index that raced every other pending index's task
//! for `background_reindex_semaphore()`'s single permit. Tokio's `Semaphore`
//! grants that permit FIFO by wait order, so the effective catch-up order was
//! whatever order each repo's C1 fast-pass happened to finish in during
//! warm-boot (directory-walk / discovery order) — size-blind. One oversized
//! repo (94k chunks) queued behind small ones was fine, but a giant repo that
//! finished its fast pass EARLY (or the only giant repo in the set) would
//! grind for hours while dozens of small repos queued up BEHIND it, all
//! candidates that could have drained in seconds had they gone first.
//!
//! What: a process-global min-heap of pending catch-up jobs ordered ascending
//! by `chunk_count` (FIFO — insertion sequence — as the tiebreak for equal
//! sizes). Each index's `enqueue` call spawns ITS OWN task (mirroring the old
//! one-task-per-index shape — see "Design note" below) that cooperatively
//! polls the shared heap: on each tick it asks "am I currently the best
//! pending job (smallest, or overtaken by a later wave — see "Anti-
//! starvation" below)?" — if yes, it removes itself and proceeds to acquire
//! `background_reindex_semaphore` + the per-index semaphore exactly as
//! `run_embed_catch_up`'s callers always have; if no, it sleeps
//! [`POLL_INTERVAL`] and asks again. This changes SUBMISSION ORDER only —
//! concurrency is unchanged (still one background embed pass in flight at a
//! time; no dedicated worker, no embedder-concurrency change — that is issue
//! #3748 slice B).
//!
//! Design note — why per-job tasks, not one shared dispatcher: an earlier
//! version of this module used a single global dispatcher task, spawned by
//! whichever `enqueue` call happened to find the queue empty. That task's
//! lifetime was then owned by WHATEVER caller's async context spawned it —
//! fine for the daemon's one long-lived `#[tokio::main]` runtime, but fatal
//! under `#[tokio::test]`'s per-test throwaway runtimes: if the enqueuing
//! test's own async fn returned before the shared dispatcher had drained
//! jobs belonging to OTHER, unrelated tests, tokio drops that runtime and
//! silently cancels the dispatcher mid-flight — orphaning every other
//! pending job forever (the `dispatcher_active` flag it never got to reset
//! stays stuck `true`). Per-job tasks tie each job's task lifetime to the
//! SAME calling context that produced it (matching the pre-#3748 shape).
//!
//! Dropping a job (#8770): a per-job task can itself be dropped — its runtime
//! shuts down, or it is aborted — before it claims, while it waits, or after
//! it claimed. A job left on the heap with no task stays the best pending
//! entry, so every later job would poll for it forever. So each job is owned
//! by a [`LiveJob`] guard from [`push_job`] on, moved into its task; dropping
//! the guard at any point removes the job from the heap, and the other
//! waiters see a new best on their next [`POLL_INTERVAL`] tick.
//!
//! Anti-starvation (issue #3748 slice A review finding 1): a pure
//! size-priority queue has an unbounded-wait failure mode — a large job can
//! be pushed behind an endless stream of newly arriving smaller ones
//! forever, most plausibly on a long-lived daemon taking a steady trickle of
//! new small `POST /indexes` registrations.
//!
//! A first version of this gate promoted the OLDEST pending job once it had
//! simply been WAITING for [`MAX_WAIT`], full stop. That is wrong: the
//! warm-boot boot-burst this slice exists to fix enqueues its entire cohort
//! (dozens of repos) within milliseconds of each other, and — now that
//! finding 2 keys the queue on real embed-pass cost rather than raw corpus
//! size — a burst that includes several genuinely large jobs can legitimately
//! take LONGER than a short `MAX_WAIT` to fully drain by size. A pure
//! "have I waited long enough" trigger would fire mid-burst and collapse
//! straight back to arrival (directory-walk) order — reintroducing the exact
//! bug this slice fixes, just delayed by `MAX_WAIT`.
//!
//! The gate instead asks a different question: "am I still waiting because
//! genuinely NEW arrivals keep queue-jumping me, or merely because this
//! burst has a lot of real work in it?" [`best_pending_seq`] only promotes
//! the oldest pending job when some OTHER pending job's `enqueued_at` is at
//! least [`MAX_WAIT`] LATER than the oldest job's own `enqueued_at` — i.e. a
//! job that arrived in a distinctly later wave, not merely a job that's been
//! sitting in the SAME wave a while. Every job in a single burst shares
//! (near-)identical `enqueued_at` timestamps, so no burst member's arrival
//! can ever satisfy "at least `MAX_WAIT` after the oldest arrival" relative
//! to another burst member — the whole burst always drains by size,
//! regardless of how long that takes. Only a genuinely later wave (a new
//! `POST /indexes` arriving `MAX_WAIT` after the oldest still-pending job)
//! can trigger a promotion, which is exactly the steady-trickle scenario the
//! gate exists to bound.
//!
//! Test: `queued_jobs_pop_in_ascending_chunk_count_order`,
//! `equal_chunk_counts_tiebreak_fifo_by_enqueue_order`,
//! `pop_next_force_promotes_a_job_once_a_later_wave_arrives`,
//! `pop_next_still_prefers_smallest_when_nothing_has_aged_out`,
//! `same_burst_never_reverts_to_arrival_order_even_once_max_wait_has_elapsed`,
//! `enqueue_drains_smallest_first_end_to_end`,
//! `burst_of_many_jobs_still_dispatches_the_giant_last_end_to_end`,
//! `a_job_dropped_before_its_turn_does_not_strand_the_next_job` in this
//! module's tests.

use std::cmp::Ordering as CmpOrdering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use super::defer_embed::run_embed_catch_up;
use super::progress::ReindexProgress;
use super::semaphore::{
    acquire_index_teardown_read, background_reindex_semaphore, index_semaphore,
};
use crate::core::indexer::IndexDeleted;
use crate::core::registry::{IndexHandle, IndexId};

/// One pending catch-up job: an index waiting for its C2 embed pass.
///
/// `Ord` is implemented so a `BinaryHeap<QueuedEmbedJob>` (a max-heap) pops
/// the SMALLEST `chunk_count` first — smaller-is-greater under this `Ord`,
/// so the heap's "max" is the smallest real job. Equal sizes tiebreak on
/// `seq` the same way (smaller/earlier `seq` pops first), preserving FIFO
/// arrival order among same-size repos. `enqueued_at` is deliberately EXCLUDED
/// from `Ord` — it only feeds the anti-starvation wall-clock check in
/// [`best_pending_seq`], which acts as a gate BEFORE any `Ord`-based
/// comparison, so baking it into `Ord` (where it would silently change over
/// time and violate
/// `BinaryHeap`'s invariant that an element's relative order stays fixed once
/// inserted) is never needed.
struct QueuedEmbedJob {
    chunk_count: usize,
    seq: u64,
    enqueued_at: Instant,
    handle: Arc<IndexHandle>,
    progress: Arc<ReindexProgress>,
}

impl PartialEq for QueuedEmbedJob {
    fn eq(&self, other: &Self) -> bool {
        self.chunk_count == other.chunk_count && self.seq == other.seq
    }
}
impl Eq for QueuedEmbedJob {}

impl PartialOrd for QueuedEmbedJob {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueuedEmbedJob {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // Reversed on both keys: BinaryHeap pops the greatest element, and we
        // want the SMALLEST chunk_count (then smallest/earliest seq) to pop
        // first, so smaller must compare as "greater".
        other
            .chunk_count
            .cmp(&self.chunk_count)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

fn queue_heap() -> &'static Mutex<BinaryHeap<QueuedEmbedJob>> {
    static HEAP: OnceLock<Mutex<BinaryHeap<QueuedEmbedJob>>> = OnceLock::new();
    HEAP.get_or_init(|| Mutex::new(BinaryHeap::new()))
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Every job not yet finished — queued on the heap, or claimed and waiting on
/// a pause or a permit — keyed by `seq`.
///
/// Why (#8664): a claimed job has left the heap but still holds its own
/// `Arc<IndexHandle>`, and a cold-parked index has no registry handle at all,
/// so a DELETE could not find the handle keeping `index.redb` and the HNSW
/// mapping open. `Weak` so this map never extends a handle's life.
/// What: written by [`push_job`], cleared by [`LiveJob`]'s `Drop`.
/// Test: `delete_closes_the_files_a_queued_embed_job_holds_for_a_cold_index`.
fn live_jobs() -> &'static Mutex<HashMap<u64, Weak<IndexHandle>>> {
    static LIVE: OnceLock<Mutex<HashMap<u64, Weak<IndexHandle>>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The handles every unfinished deferred-embed job for `id` holds (#8664).
///
/// Why: the delete path closes each one's files, so no job keeps the deleted
/// index's files open. See [`live_jobs`].
/// What: upgrades each live entry for `id`, one handle per distinct indexer —
/// several jobs on one index share it, and it is closed once. A job that
/// already dropped its handle is skipped.
/// Test: `delete_closes_the_files_a_queued_embed_job_holds_for_a_cold_index`,
/// `job_handles_for_yields_each_indexer_once_across_concurrent_jobs`.
pub(crate) fn job_handles_for(id: &IndexId) -> Vec<Arc<IndexHandle>> {
    let live = live_jobs()
        .lock()
        .expect("defer-embed live-job lock poisoned");
    let mut handles: Vec<Arc<IndexHandle>> = Vec::new();
    for handle in live.values().filter_map(Weak::upgrade) {
        let seen = handles
            .iter()
            .any(|h| Arc::ptr_eq(&h.indexer, &handle.indexer));
        if &handle.id == id && !seen {
            handles.push(handle);
        }
    }
    handles
}

/// Owns one pushed job's bookkeeping from [`push_job`] until it is dropped
/// (#8664, #8770).
///
/// Why: a job ends in many ways — drained pause, closed semaphore, deleted
/// index, a completed pass, or its task dropped at any await, even before
/// the first poll. Each must settle the job exactly once.
/// What: on drop, removes the job's `seq` from the heap if it is still there,
/// then its [`live_jobs`] entry, then its [`QUEUE_DEPTH`] count. Removing the
/// heap entry is the wake: every waiter re-reads [`best_pending_seq`] each
/// [`POLL_INTERVAL`], so the next job claims on its next tick.
/// Test: `a_job_dropped_before_its_turn_does_not_strand_the_next_job`,
/// `a_job_dropped_at_any_point_leaves_the_heap`,
/// `every_job_exit_drops_its_live_entry`.
pub(crate) struct LiveJob(u64);

impl LiveJob {
    /// The job's heap sequence number.
    pub(crate) fn seq(&self) -> u64 {
        self.0
    }
}

impl Drop for LiveJob {
    fn drop(&mut self) {
        // #8770: a job dropped before it claimed left its `seq` as the best
        // pending entry, so every later waiter polled for it forever.
        queue_heap()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|j| j.seq != self.0);
        live_jobs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
        let remaining = QUEUE_DEPTH.fetch_sub(1, Ordering::AcqRel) - 1;
        note_if_drained(remaining);
    }
}

/// `Err(IndexDeleted)` once a DELETE has closed this handle's files (#8664).
///
/// Why: fail closed. A job that ran its embed pass against closed files would
/// read a released mapping (#8232's SIGBUS) or write into a deleted index.
async fn refuse_if_deleted(handle: &IndexHandle) -> Result<(), IndexDeleted> {
    if handle.indexer.read().await.is_deleted() {
        return Err(IndexDeleted {
            index_id: handle.id.0.clone(),
        });
    }
    Ok(())
}

/// The minimum gap, between the OLDEST pending job's arrival and any OTHER
/// pending job's arrival, that counts as "a distinctly later wave" rather
/// than "the same burst" — see the module docs' "Anti-starvation" section.
///
/// Sized in MINUTES, not milliseconds: a warm-boot catch-up burst can
/// legitimately take minutes to fully enqueue (each repo's C1 fast pass is
/// itself serialised through the SAME 1-permit `background_reindex_semaphore`
/// this queue's jobs share, so on a large fleet — hundreds of colocated
/// indexes — successive C2 arrivals are naturally staggered well past any
/// sub-second threshold even with zero contention). A short threshold would
/// misclassify that normal staggering as "a later wave" and collapse the
/// queue back toward arrival order — the exact bug this slice fixes. Five
/// minutes comfortably exceeds normal per-repo fast-pass latency while still
/// bounding a genuinely pathological indefinite trickle of new small
/// `POST /indexes` registrations to a wait an operator would notice, not one
/// that runs for hours.
const MAX_WAIT: Duration = Duration::from_secs(300);

/// How often an index's own waiting task re-checks whether it's now the best
/// pending job. Cheap (one mutex lock over a heap of at most a few hundred
/// entries) relative to the embed passes it's waiting to run, so a short
/// interval costs nothing measurable while keeping dispatch latency low.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Number of catch-up jobs currently pending or in-flight (issue #3748).
///
/// Why: exposed on `/health` (mirrors `background_reindex_queue_depth`) so
/// operators can watch the size-ordered catch-up backlog drain.
/// What: incremented on enqueue, decremented when the job's [`LiveJob`]
/// guard drops — its pass ended, or its task was dropped (#8770).
static QUEUE_DEPTH: AtomicUsize = AtomicUsize::new(0);

/// Bumped every time [`QUEUE_DEPTH`] transitions from non-zero to zero, i.e.
/// every time a full catch-up cycle drains (issue #3748).
///
/// Why: `server::health` uses this as an edge-detector — comparing the
/// current epoch against the last epoch it observed — to recompute
/// `warm_boot_degraded` exactly once per drain rather than leaving it sticky
/// until a daemon restart. See `server::health::recompute_warm_boot_degraded`.
static COMPLETION_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Current pending+in-flight catch-up queue depth. See [`QUEUE_DEPTH`].
pub fn deferred_embed_queue_depth() -> usize {
    QUEUE_DEPTH.load(Ordering::Acquire)
}

/// Current catch-up completion epoch. See [`COMPLETION_EPOCH`].
pub fn deferred_embed_completion_epoch() -> u64 {
    COMPLETION_EPOCH.load(Ordering::Acquire)
}

/// Truncate the logged plan to this many entries; anything beyond that is
/// summarised as "+N more" so a 200+ index warm-boot doesn't spam one
/// enormous log line for every single enqueue.
const LOGGED_PLAN_ENTRIES: usize = 10;

/// Enqueue an index's deferred-embed catch-up pass (issue #3748 slice A).
///
/// Why: replaces the old "spawn a task that immediately races the semaphore"
/// scheme with explicit size-ascending submission order. See the module
/// docs, including the "Design note" on why this spawns one task PER index
/// (like the pre-#3748 code) rather than funnelling through a single shared
/// dispatcher.
/// What: pushes a job onto the shared min-heap, logs the current ordered
/// plan, and spawns this index's own `wait_for_turn` task. `chunk_count`
/// should be the index's chunk count at enqueue time (the cheapest accurate
/// proxy for embed-pass cost available before embedding starts; callers read
/// it from the same indexer read-lock they already hold right before calling
/// this, so it costs nothing extra).
/// Test: `enqueue_drains_smallest_first_end_to_end`.
pub(super) fn enqueue(
    handle: Arc<IndexHandle>,
    progress: Arc<ReindexProgress>,
    chunk_count: usize,
) {
    // #8770: the guard moves into the task, so a task dropped before its
    // first poll still takes its job off the heap.
    let live = push_job(handle, progress, chunk_count);
    tokio::spawn(async move {
        // #8664: a job whose index was deleted ends here, with its handle
        // released and nothing embedded.
        if let Err(deleted) = wait_for_turn(live).await {
            tracing::warn!("deferred_embed: job abandoned — {deleted}");
        }
    });
}

/// Push one job onto the heap and record it in [`live_jobs`]; returns the
/// [`LiveJob`] guard that owns it. The half of [`enqueue`] that does not
/// spawn, so a test can hold a job queued across a DELETE. Dropping the guard
/// without running it withdraws the job (#8770).
pub(crate) fn push_job(
    handle: Arc<IndexHandle>,
    progress: Arc<ReindexProgress>,
    chunk_count: usize,
) -> LiveJob {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    live_jobs()
        .lock()
        .expect("defer-embed live-job lock poisoned")
        .insert(seq, Arc::downgrade(&handle));
    let job = QueuedEmbedJob {
        chunk_count,
        seq,
        enqueued_at: Instant::now(),
        handle,
        progress,
    };

    let plan = {
        let mut heap = queue_heap()
            .lock()
            .expect("defer-embed queue lock poisoned");
        heap.push(job);
        QUEUE_DEPTH.fetch_add(1, Ordering::AcqRel);
        format_ordered_plan(&heap)
    };

    tracing::info!(
        "deferred_embed queue: {} pending, order=[{}]",
        plan.0,
        plan.1
    );
    LiveJob(seq)
}

/// Render the heap's current ascending-size order for logging, truncated to
/// [`LOGGED_PLAN_ENTRIES`] entries.
fn format_ordered_plan(heap: &BinaryHeap<QueuedEmbedJob>) -> (usize, String) {
    let mut items: Vec<&QueuedEmbedJob> = heap.iter().collect();
    items.sort_by(|a, b| a.chunk_count.cmp(&b.chunk_count).then(a.seq.cmp(&b.seq)));
    let total = items.len();
    let mut shown: Vec<String> = items
        .iter()
        .take(LOGGED_PLAN_ENTRIES)
        .map(|j| format!("{}({})", j.handle.id.0, j.chunk_count))
        .collect();
    if total > LOGGED_PLAN_ENTRIES {
        shown.push(format!("+{} more", total - LOGGED_PLAN_ENTRIES));
    }
    (total, shown.join(", "))
}

/// Identify (without removing) the best currently-pending job: the OLDEST
/// pending job if some OTHER pending job arrived at least [`MAX_WAIT`] LATER
/// than it did (a genuinely later wave overtaking it), otherwise the
/// SMALLEST — see the module docs' "Anti-starvation" section for why this is
/// deliberately NOT "has the oldest job simply been waiting a while".
///
/// Why a separate function: keeps the fairness decision testable in
/// isolation (`pop_next_force_promotes_a_job_once_a_later_wave_arrives`,
/// `pop_next_still_prefers_smallest_when_nothing_has_aged_out`,
/// `same_burst_never_reverts_to_arrival_order_even_once_max_wait_has_elapsed`
/// exercise this directly on a plain `BinaryHeap`, no tokio required) and
/// shared between `wait_for_turn`'s "is it my turn" poll and (indirectly)
/// the logging path.
/// What: returns the `seq` of the best candidate without mutating the heap.
/// `O(n)` twice in the worst case (find the oldest arrival, then scan for a
/// later-wave arrival relative to it) — `n` is bounded by the number of
/// indexes still awaiting catch-up, hundreds at most.
fn best_pending_seq(heap: &BinaryHeap<QueuedEmbedJob>) -> Option<u64> {
    if heap.is_empty() {
        return None;
    }
    if let Some(oldest) = heap.iter().min_by_key(|j| j.enqueued_at) {
        let later_wave_exists = heap
            .iter()
            .any(|j| j.enqueued_at >= oldest.enqueued_at + MAX_WAIT);
        if later_wave_exists {
            return Some(oldest.seq);
        }
    }
    heap.iter().max().map(|j| j.seq)
}

/// One index's wait-for-my-turn task (issue #3748 slice A). Spawned once per
/// `enqueue` call, tied to the SAME caller context as the enqueue itself
/// (see the module docs' "Design note").
///
/// Why: cooperative polling — rather than a single shared dispatcher —
/// avoids coupling every pending job's fate to whichever caller happened to
/// spawn a central dispatcher (see the module docs).
/// What: every [`POLL_INTERVAL`], checks whether `my_seq` is currently
/// [`best_pending_seq`]; if so, removes itself from the heap and proceeds to
/// acquire `background_reindex_semaphore` + the per-index semaphore exactly
/// as the pre-#3748 code did, then runs [`run_embed_catch_up`]. If not yet
/// its turn, sleeps and re-checks.
///
/// #8664: returns `Err(IndexDeleted)` without embedding anything when a DELETE
/// closed the job's handle while it waited.
/// #8770: owns `live` for its whole run, so dropping this future at any
/// await withdraws the job from the heap.
/// Test: `delete_closes_the_files_a_queued_embed_job_holds_for_a_cold_index`,
/// `a_job_dropped_at_any_point_leaves_the_heap`.
pub(crate) async fn wait_for_turn(live: LiveJob) -> Result<(), IndexDeleted> {
    let my_seq = live.seq();
    let job = loop {
        let claimed = {
            let mut heap = queue_heap()
                .lock()
                .expect("defer-embed queue lock poisoned");
            if best_pending_seq(&heap) == Some(my_seq) {
                // Extract exactly this job. `into_vec` + rebuild is O(n) but
                // only pays that cost on the tick this task actually wins.
                let items = std::mem::take(&mut *heap).into_vec();
                let mut items = items;
                let pos = items
                    .iter()
                    .position(|j| j.seq == my_seq)
                    .expect("best_pending_seq just found my_seq in this same heap");
                let mine = items.remove(pos);
                *heap = BinaryHeap::from(items);
                Some(mine)
            } else {
                None
            }
        };
        match claimed {
            Some(job) => break job,
            None => tokio::time::sleep(POLL_INTERVAL).await,
        }
    };

    // #6524: park here while this index's embedding is paused — BEFORE any
    // permit is taken. The job has already left the heap, so every other index
    // keeps draining, and a paused index holds neither the single background
    // permit nor its own teardown read-guard, so a DELETE on it is not blocked
    // either. Daemon shutdown releases the park through `EmbeddingPause::drain`;
    // the job is then abandoned with its durable pending marker still set, so
    // the next boot re-arms it.
    if job.handle.embedding_pause.wait_while_paused().await
        == crate::core::embed_pause::PauseWait::Drained
    {
        tracing::info!(
            "deferred_embed[{}]: abandoning a paused embed job — daemon is draining",
            job.handle.id.0,
        );
        return Ok(());
    }
    // #8664: a DELETE may have closed this handle while the job was queued;
    // refuse before taking the one background permit.
    refuse_if_deleted(&job.handle).await?;

    // Same concurrency guards `spawn_deferred_embed_pass` always used: the
    // process-wide background-reindex permit, then this index's own
    // mutual-exclusion permit. Only the SUBMISSION ORDER changed.
    let _permit = match background_reindex_semaphore().acquire().await {
        Ok(p) => p,
        Err(_) => {
            tracing::warn!(
                "deferred_embed[{}]: background semaphore closed — skipping embed pass",
                job.handle.id.0,
            );
            return Ok(());
        }
    };
    // Issue #2984 Phase 1 CRITICAL finding 2: also hold this index's
    // per-index mutual-exclusion permit for the whole pass — the SAME
    // semaphore the component-toggle handler and `run_reindex` acquire for
    // this index, so this pass can never race a runtime component catch-up
    // or a reindex on the SAME index.
    let _index_permit = index_semaphore(&job.handle.id)
        .acquire_owned()
        .await
        .expect(
        "per-index semaphore is never closed — it is a fresh Semaphore per IndexId, never dropped",
    );

    // #3049: hold the teardown lock's shared side across the embed pass.
    // `embed_deferred_chunks` has no interior cancel checkpoint, so a DELETE
    // landing here waits the full pass (or times out and refuses the data
    // removal) rather than deleting underneath it.
    let _teardown_guard = acquire_index_teardown_read(&job.handle.id).await;
    // #8664: the authoritative re-check. A DELETE closes files under the
    // exclusive side of this lock, so none can close them past this point.
    refuse_if_deleted(&job.handle).await?;

    run_embed_catch_up(job.handle, job.progress).await;
    Ok(())
}

/// When the queue depth reaches zero, bump the completion epoch and log the
/// drain (issue #3748 — the signal `server::health` polls to recompute
/// `warm_boot_degraded`).
fn note_if_drained(remaining: usize) {
    if remaining == 0 {
        COMPLETION_EPOCH.fetch_add(1, Ordering::AcqRel);
        tracing::info!("deferred_embed queue: drained — catch-up cycle complete");
    }
}

#[cfg(test)]
#[path = "defer_embed_queue_tests.rs"]
mod tests;
