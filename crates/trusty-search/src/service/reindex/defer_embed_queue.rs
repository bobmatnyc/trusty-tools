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
//! pending job (smallest, or aged past [`MAX_WAIT`])?" — if yes, it removes
//! itself and proceeds to acquire `background_reindex_semaphore` + the
//! per-index semaphore exactly as `run_embed_catch_up`'s callers always have;
//! if no, it sleeps [`POLL_INTERVAL`] and asks again. This changes
//! SUBMISSION ORDER only — concurrency is unchanged (still one background
//! embed pass in flight at a time; no dedicated worker, no
//! embedder-concurrency change — that is issue #3748 slice B).
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
//! Anti-starvation: a pure size-priority queue has an unbounded-wait failure
//! mode — a large job can be pushed behind an endless stream of newly
//! arriving smaller ones forever. At warm-boot this is unlikely (the catch-up
//! cohort is essentially fixed once boot's fast passes finish), but it is a
//! real risk for a long-lived daemon taking a steady trickle of new small
//! `POST /indexes` registrations. [`MAX_WAIT`] bounds it: on every poll tick,
//! if the OLDEST pending job has been waiting at least that long, IT is
//! treated as "the best" regardless of size — a WALL-CLOCK bound (not a
//! dispatch-count bound), so the guarantee holds however fast or slow other
//! jobs happen to be draining.
//!
//! Test: `queued_jobs_pop_in_ascending_chunk_count_order`,
//! `equal_chunk_counts_tiebreak_fifo_by_enqueue_order`,
//! `pop_next_force_promotes_a_job_once_it_exceeds_max_wait`,
//! `pop_next_still_prefers_smallest_when_nothing_has_aged_out`,
//! `enqueue_drains_smallest_first_end_to_end` in this module's tests.

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

/// A pending job waiting at least this long is treated as "the best"
/// regardless of size on the next poll tick — see the module docs'
/// "Anti-starvation" section. Long enough that it never meaningfully
/// undercuts the "small drains before giant" goal (warm-boot catch-up passes
/// are the common case and finish well under this), short enough to bound
/// worst-case wait to something an operator would never notice.
const MAX_WAIT: Duration = Duration::from_millis(750);

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

/// Identify (without removing) the best currently-pending job: the oldest if
/// it has waited at least [`MAX_WAIT`], otherwise the smallest — see the
/// module docs' "Anti-starvation" section.
///
/// Why a separate function: keeps the fairness decision testable in
/// isolation (`pop_next_force_promotes_a_job_once_it_exceeds_max_wait`,
/// `pop_next_still_prefers_smallest_when_nothing_has_aged_out` exercise this
/// directly on a plain `BinaryHeap`, no tokio required) and shared between
/// `wait_for_turn`'s "is it my turn" poll and (indirectly) the logging path.
/// What: returns the `seq` of the best candidate without mutating the heap.
/// `O(n)` (`enqueued_at.elapsed()` must be checked fresh on every call, so
/// even the "no one is starving" path scans once) — `n` is bounded by the
/// number of indexes still awaiting catch-up, hundreds at most.
fn best_pending_seq(heap: &BinaryHeap<QueuedEmbedJob>) -> Option<u64> {
    if heap.is_empty() {
        return None;
    }
    let starving = heap
        .iter()
        .filter(|j| j.enqueued_at.elapsed() >= MAX_WAIT)
        .min_by_key(|j| j.seq);
    if let Some(job) = starving {
        return Some(job.seq);
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
mod tests {
    use super::*;
    use crate::core::{indexer::CodeIndexer, registry::IndexId};
    use std::sync::Arc as StdArc;

    fn bare_handle(id: &str) -> StdArc<IndexHandle> {
        let indexer = CodeIndexer::new(id, format!("/tmp/{id}"));
        StdArc::new(IndexHandle::bare(
            IndexId::new(id),
            StdArc::new(tokio::sync::RwLock::new(indexer)),
            format!("/tmp/{id}").into(),
        ))
    }

    /// Issue #3748: [`best_pending_seq`] must identify the job with the
    /// SMALLEST `chunk_count` (not the natural max-heap order).
    ///
    /// Why: this is the entire point of the slice — small repos must drain
    /// before a giant one, regardless of push order.
    /// What: pushes jobs with out-of-order sizes, asserts `best_pending_seq`
    /// picks the smallest one.
    /// Test: this IS the test.
    #[test]
    fn queued_jobs_pop_in_ascending_chunk_count_order() {
        let mut heap = BinaryHeap::new();
        let mut tiny_seq = 0;
        for (size, id) in [(500usize, "mid"), (94_000, "giant"), (12, "tiny")] {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            if size == 12 {
                tiny_seq = seq;
            }
            heap.push(QueuedEmbedJob {
                chunk_count: size,
                seq,
                enqueued_at: Instant::now(),
                handle: bare_handle(id),
                progress: Arc::new(ReindexProgress::new()),
            });
        }
        assert_eq!(
            best_pending_seq(&heap),
            Some(tiny_seq),
            "the smallest chunk_count job must be identified as best"
        );
    }

    /// Issue #3748: two jobs with the SAME `chunk_count` must resolve to the
    /// one enqueued FIRST (FIFO tiebreak), not an arbitrary one.
    ///
    /// Why: the spec explicitly requires "keep FIFO as tiebreak for equal
    /// sizes" — without a `seq` tiebreak, `BinaryHeap`'s max is unspecified
    /// among equal-priority elements.
    /// What: pushes three same-size jobs in a known order, asserts
    /// `best_pending_seq` picks the first.
    /// Test: this IS the test.
    #[test]
    fn equal_chunk_counts_tiebreak_fifo_by_enqueue_order() {
        let mut heap = BinaryHeap::new();
        let ids = ["first", "second", "third"];
        let mut first_seq = 0;
        for (i, id) in ids.iter().enumerate() {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            if i == 0 {
                first_seq = seq;
            }
            heap.push(QueuedEmbedJob {
                chunk_count: 1_000,
                seq,
                enqueued_at: Instant::now(),
                handle: bare_handle(id),
                progress: Arc::new(ReindexProgress::new()),
            });
        }
        assert_eq!(
            best_pending_seq(&heap),
            Some(first_seq),
            "equal-size jobs must resolve to the FIFO-first one"
        );
    }

    /// Issue #3748 anti-starvation: an old, large job that has waited at
    /// least [`MAX_WAIT`] must be identified as best NEXT, even with a
    /// strictly smaller, strictly newer job also pending (which pure size
    /// ordering would otherwise always prefer).
    ///
    /// Why: a pure size-priority queue has no fairness bound — this pins the
    /// wall-clock guarantee that breaks it.
    /// What: pushes a "giant" job with `enqueued_at` backdated past
    /// `MAX_WAIT` (no real `sleep` needed — `Instant` arithmetic), then
    /// pushes a fresh, much smaller "tiny" job, and asserts
    /// `best_pending_seq` returns the giant.
    /// Test: this IS the test.
    #[test]
    fn pop_next_force_promotes_a_job_once_it_exceeds_max_wait() {
        let mut heap = BinaryHeap::new();
        let giant_seq = SEQ.fetch_add(1, Ordering::Relaxed);
        heap.push(QueuedEmbedJob {
            chunk_count: 94_000,
            seq: giant_seq,
            enqueued_at: Instant::now()
                .checked_sub(MAX_WAIT + Duration::from_millis(1))
                .expect("test process has been running longer than MAX_WAIT"),
            handle: bare_handle("giant-old"),
            progress: Arc::new(ReindexProgress::new()),
        });
        heap.push(QueuedEmbedJob {
            chunk_count: 1,
            seq: SEQ.fetch_add(1, Ordering::Relaxed),
            enqueued_at: Instant::now(),
            handle: bare_handle("tiny-newcomer"),
            progress: Arc::new(ReindexProgress::new()),
        });

        assert_eq!(
            best_pending_seq(&heap),
            Some(giant_seq),
            "a job waiting past MAX_WAIT must be force-promoted even though a \
             strictly smaller, strictly newer job is available and would otherwise \
             win on pure size ordering"
        );
    }

    /// Issue #3748: below `MAX_WAIT`, ordering is unaffected — the smallest
    /// job still wins even when an older, larger job is also pending. Pins
    /// the "no wait, no promotion" side of the same gate the previous test
    /// exercises, so the two together cover both branches of
    /// `best_pending_seq`.
    /// Test: this IS the test.
    #[test]
    fn pop_next_still_prefers_smallest_when_nothing_has_aged_out() {
        let mut heap = BinaryHeap::new();
        heap.push(QueuedEmbedJob {
            chunk_count: 94_000,
            seq: SEQ.fetch_add(1, Ordering::Relaxed),
            enqueued_at: Instant::now(),
            handle: bare_handle("giant-fresh"),
            progress: Arc::new(ReindexProgress::new()),
        });
        let tiny_seq = SEQ.fetch_add(1, Ordering::Relaxed);
        heap.push(QueuedEmbedJob {
            chunk_count: 1,
            seq: tiny_seq,
            enqueued_at: Instant::now(),
            handle: bare_handle("tiny-fresh"),
            progress: Arc::new(ReindexProgress::new()),
        });

        assert_eq!(
            best_pending_seq(&heap),
            Some(tiny_seq),
            "with nothing aged past MAX_WAIT, the smallest job must still win"
        );
    }

    /// Issue #3748 end-to-end: `enqueue`-ing a giant job FIRST and a tiny job
    /// SECOND must still run the tiny job's embed pass first (proving the
    /// per-index tasks — not just `best_pending_seq` in isolation — respect
    /// size order).
    ///
    /// Why: the unit tests above only prove the selection logic; this proves
    /// the full `enqueue` -> `wait_for_turn` -> `run_embed_catch_up` pipeline
    /// actually uses it.
    /// What: both handles carry ONE real committed chunk and a shared
    /// `OrderRecordingEmbedder` that appends its index id to a common
    /// `Arc<Mutex<Vec<String>>>` the INSTANT `embed_batch` is called —
    /// recording order directly at the moment of dispatch, not via a
    /// wall-clock poll (a poll-based "did it finish yet" race is
    /// indistinguishable from a real ordering bug once both jobs' embed
    /// passes are cheap enough to complete within the same poll tick).
    /// Enqueues big (9000 "chunks", the priority key — the real committed
    /// chunk count is 1) then small (3), waits for both to reach a terminal
    /// stage, and asserts the RECORDED order is `[small, big]`.
    /// Test: this IS the test.
    #[tokio::test]
    async fn enqueue_drains_smallest_first_end_to_end() {
        use crate::core::chunker::{ChunkType, RawChunk};
        use crate::core::embed::Embedder;
        use crate::core::indexer::ParsedBatch;
        use crate::core::store::{UsearchStore, VectorStore};
        use std::sync::Mutex as StdMutex;

        struct OrderRecordingEmbedder {
            id: String,
            order: Arc<StdMutex<Vec<String>>>,
        }
        #[async_trait::async_trait]
        impl Embedder for OrderRecordingEmbedder {
            async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
                self.order.lock().expect("lock").push(self.id.clone());
                Ok(vec![0.1; 8])
            }
            async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
                self.order.lock().expect("lock").push(self.id.clone());
                Ok(texts.iter().map(|_| vec![0.1; 8]).collect())
            }
            fn dimension(&self) -> usize {
                8
            }
        }

        fn raw_chunk(id: &str) -> RawChunk {
            RawChunk {
                id: format!("{id}:1:1"),
                file: "test.rs".into(),
                start_line: 1,
                end_line: 1,
                content: "fn f() {}".into(),
                function_name: None,
                language: Some("rust".into()),
                chunk_type: ChunkType::Code,
                calls: vec![],
                inherits_from: vec![],
                chunk_depth: 0,
                parent_chunk_id: None,
                child_chunk_ids: vec![],
                nlp_keywords: vec![],
                nlp_code_refs: vec![],
                virtual_terms: vec![],
            }
        }

        fn committed_handle(id: &str, order: &Arc<StdMutex<Vec<String>>>) -> StdArc<IndexHandle> {
            let embedder: Arc<dyn Embedder> = Arc::new(OrderRecordingEmbedder {
                id: id.to_string(),
                order: Arc::clone(order),
            });
            let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(8).expect("usearch new"));
            let indexer =
                CodeIndexer::new(id, format!("/tmp/{id}")).with_components(embedder, store);
            StdArc::new(IndexHandle::bare(
                IndexId::new(id),
                Arc::new(tokio::sync::RwLock::new(indexer)),
                format!("/tmp/{id}").into(),
            ))
        }

        let order: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let big = committed_handle("e2e-big-3748", &order);
        let small = committed_handle("e2e-small-3748", &order);

        // Commit one synthetic chunk into each indexer so `has_embedder() &&
        // chunk_count > 0`, otherwise `embed_deferred_chunks` short-circuits
        // without ever calling the embedder and there is nothing to order.
        for handle in [&big, &small] {
            let parsed = ParsedBatch {
                chunks: vec![raw_chunk(&handle.id.0)],
                embeddings: vec![None],
                entities_by_file: vec![],
                parse_ms: 0,
                embed_ms: 0,
                vector_count: 0,
            };
            handle
                .indexer
                .read()
                .await
                .commit_parsed_batch(parsed, false)
                .await
                .ok();
        }

        // Enqueue big FIRST (arrival order) but with the LARGER priority
        // key, then small SECOND with the smaller key — reversed vs. size
        // order, so a passing test proves size (not arrival) wins.
        enqueue(StdArc::clone(&big), Arc::new(ReindexProgress::new()), 9_000);
        enqueue(StdArc::clone(&small), Arc::new(ReindexProgress::new()), 3);

        for handle in [&big, &small] {
            for _ in 0..200 {
                let status = handle.stages.read().await.semantic.status;
                if status != crate::core::registry::StageStatus::Pending
                    && status != crate::core::registry::StageStatus::InProgress
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }

        let recorded = order.lock().expect("lock").clone();
        assert_eq!(
            recorded,
            vec!["e2e-small-3748".to_string(), "e2e-big-3748".to_string()],
            "the smaller job (enqueued SECOND, smaller priority key) must still run its \
             embed call BEFORE the bigger job (enqueued FIRST) — proves dispatch \
             honours size order, not arrival order"
        );
    }
}
