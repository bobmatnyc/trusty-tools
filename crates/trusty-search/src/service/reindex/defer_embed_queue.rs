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
//! SAME calling context that produced it (matching the pre-#3748 shape), so
//! one test's early return can never strand another test's job.
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
//! `burst_of_many_jobs_still_dispatches_the_giant_last_end_to_end` in this
//! module's tests.

use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::defer_embed::run_embed_catch_up;
use super::progress::ReindexProgress;
use super::semaphore::{background_reindex_semaphore, index_semaphore};
use crate::core::registry::IndexHandle;

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
/// What: incremented on enqueue, decremented once a job's embed pass
/// completes (success or failure).
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
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
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

    tokio::spawn(wait_for_turn(seq));
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
async fn wait_for_turn(my_seq: u64) {
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
            let remaining = QUEUE_DEPTH.fetch_sub(1, Ordering::AcqRel) - 1;
            note_if_drained(remaining);
            return;
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

    run_embed_catch_up(job.handle, job.progress).await;

    let remaining = QUEUE_DEPTH.fetch_sub(1, Ordering::AcqRel) - 1;
    note_if_drained(remaining);
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
