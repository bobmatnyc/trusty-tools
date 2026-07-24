//! Tests for `defer_embed_queue` (issue #3748 slice A).
//!
//! Why: split from `defer_embed_queue.rs` into a sibling `_tests.rs` file
//! (`#[path = "defer_embed_queue_tests.rs"] mod tests;`) to keep the
//! production file under the 500-SLOC cap — this file gets the 1500-SLOC
//! test-file budget instead. `use super::*` reaches every private item of
//! `defer_embed_queue` exactly as it would from a nested `mod tests { .. }`
//! block, since `#[path]` makes this file's content the body of that module.
//! What: heap-ordering, tiebreak, and anti-starvation unit tests plus two
//! end-to-end tests exercising the full `enqueue` -> `wait_for_turn` ->
//! `run_embed_catch_up` pipeline.
//! Test: this file IS the test suite for `defer_embed_queue`.

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

/// Issue #3748 anti-starvation: an old, large job must be identified as
/// best NEXT once a DIFFERENT job arrives a full [`MAX_WAIT`] LATER than
/// it did — a genuinely later wave overtaking it — even though that
/// newcomer is strictly smaller (which pure size ordering would
/// otherwise always prefer).
///
/// Why: a pure size-priority queue has no fairness bound — this pins the
/// wall-clock guarantee that breaks it, using the CORRECTED "later wave"
/// semantics (not "have I simply waited a while") — see the module docs'
/// "Anti-starvation" section for why the naive version reintroduces the
/// bug this slice fixes.
/// What: pushes a "giant" job with `enqueued_at` backdated by
/// `MAX_WAIT + 1ms` (no real `sleep` needed — `Instant` arithmetic), then
/// pushes a fresh "tiny" job (whose arrival is therefore >= `MAX_WAIT`
/// after the giant's), and asserts `best_pending_seq` returns the giant.
/// Test: this IS the test.
#[test]
fn pop_next_force_promotes_a_job_once_a_later_wave_arrives() {
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
        "a job overtaken by an arrival a full MAX_WAIT later must be force-promoted \
         even though the newcomer is strictly smaller and would otherwise win on \
         pure size ordering"
    );
}

/// Issue #3748: with no later-wave arrival, ordering is unaffected — the
/// smallest job still wins even when an older, larger job is also
/// pending. Pins the "no later wave, no promotion" side of the same gate
/// the previous test exercises, so the two together cover both branches
/// of `best_pending_seq`.
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

/// Issue #3748 slice A review finding 1 (the BLOCK-ing bug): a single
/// same-burst arrival of ~30 jobs — the exact warm-boot shape this slice
/// exists to fix — must NEVER revert to arrival order, no matter how
/// long the burst has collectively been sitting in the queue, EVEN once
/// that elapsed time exceeds `MAX_WAIT` many times over.
///
/// Why: the original (BLOCKed) version of this gate promoted the OLDEST
/// pending job purely because it had waited `>= MAX_WAIT`, full stop.
/// Since an entire warm-boot burst enqueues within milliseconds, once
/// the whole burst collectively aged past `MAX_WAIT` (guaranteed for any
/// burst that takes a while to drain by size — exactly what a burst
/// containing a genuine giant does), `best_pending_seq` would select
/// min-by-seq among ALL of them — i.e. raw arrival/directory-walk order,
/// with the giant back near the front. This test is the fix's proof:
/// even with `enqueued_at` backdated so the ENTIRE burst is already
/// `10 * MAX_WAIT` old (no need to actually sleep that long), size order
/// must still hold, because no burst member's arrival is ever a later
/// WAVE relative to any other burst member's arrival — they're all the
/// same wave.
/// What: builds a 30-job heap (1 giant + 29 small, giant among the
/// early `seq`s to match "the giant finished its fast pass early"),
/// all sharing one backdated `enqueued_at` far past `MAX_WAIT`, and
/// pops the ENTIRE heap via repeated `best_pending_seq` + removal,
/// asserting the giant is popped strictly LAST.
/// Test: this IS the test.
#[test]
fn same_burst_never_reverts_to_arrival_order_even_once_max_wait_has_elapsed() {
    let burst_arrival = Instant::now()
        .checked_sub(MAX_WAIT * 10)
        .expect("test process has been running longer than 10x MAX_WAIT");

    let mut heap = BinaryHeap::new();
    let giant_seq = SEQ.fetch_add(1, Ordering::Relaxed);
    heap.push(QueuedEmbedJob {
        chunk_count: 94_000,
        seq: giant_seq,
        enqueued_at: burst_arrival,
        handle: bare_handle("giant-early-seq"),
        progress: Arc::new(ReindexProgress::new()),
    });
    // The giant is seq 0 (arrived first in the burst); the other 29
    // small jobs arrive right after it, same burst, same backdated
    // timestamp — mirroring "the giant's fast pass happened to finish
    // first" from the original incident report.
    for i in 0..29 {
        heap.push(QueuedEmbedJob {
            chunk_count: 10 + i,
            seq: SEQ.fetch_add(1, Ordering::Relaxed),
            enqueued_at: burst_arrival,
            handle: bare_handle(&format!("small-{i}")),
            progress: Arc::new(ReindexProgress::new()),
        });
    }
    assert_eq!(heap.len(), 30, "precondition: 1 giant + 29 small jobs");

    let mut pop_order = Vec::new();
    while let Some(seq) = best_pending_seq(&heap) {
        let items = std::mem::take(&mut heap).into_vec();
        let mut items = items;
        let pos = items
            .iter()
            .position(|j| j.seq == seq)
            .expect("best_pending_seq returned a seq present in this heap");
        pop_order.push(items.remove(pos).seq);
        heap = BinaryHeap::from(items);
    }

    assert_eq!(
        pop_order.len(),
        30,
        "every job must eventually be popped exactly once"
    );
    assert_eq!(
        *pop_order.last().expect("non-empty"),
        giant_seq,
        "the giant must still be dispatched LAST — a same-burst arrival must never \
         revert to raw arrival order just because the whole burst has aged past \
         MAX_WAIT (issue #3748 slice A review finding 1)"
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
        let indexer = CodeIndexer::new(id, format!("/tmp/{id}")).with_components(embedder, store);
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

/// Issue #3748 slice A review finding 1: the full async dispatch
/// pipeline (not just `best_pending_seq` in isolation) must still
/// dispatch a giant LAST out of a ~30-job same-burst arrival with
/// NON-INSTANT simulated embed durations — the exact shape of the
/// original warm-boot incident (27 repos: 26 small + 1 giant, `repo-
/// duetto`, 94k chunks), reproduced here as 1 giant + 26 small jobs
/// enqueued back-to-back with the giant FIRST (matching "the giant's
/// fast pass happened to finish early").
///
/// Why: `same_burst_never_reverts_to_arrival_order_even_once_max_wait_has_elapsed`
/// proves the pure selection function; this proves `enqueue` ->
/// `wait_for_turn` -> `run_embed_catch_up` end to end, under real
/// (if short) per-job embed latency, so the burst's total drain time is
/// non-trivial wall-clock — the exact condition that broke the original
/// (BLOCKed) "have I simply waited MAX_WAIT" gate. This test does NOT
/// need to literally wait for the production `MAX_WAIT` (5 minutes) to
/// elapse: the corrected gate only ever promotes on a genuinely LATER
/// wave (see the module docs), and no new job is enqueued after the
/// initial burst here, so `MAX_WAIT`'s value is irrelevant to this
/// test's outcome — which is itself part of the proof: burst correctness
/// no longer depends on how long the burst takes to drain.
/// What: one shared `OrderRecordingEmbedder` (adds a short `sleep` before
/// recording, so each job's embed call has real non-zero latency) across
/// 1 giant (94000 priority key, 1 real committed chunk) + 26 small jobs
/// (priority keys `1..=26`, 1 real committed chunk each). Enqueues the
/// giant FIRST, then the 26 small jobs in ASCENDING priority-key order,
/// waits for every handle to leave Pending/InProgress, and asserts the
/// giant is the LAST id in the recorded order.
/// Test: this IS the test.
#[tokio::test]
async fn burst_of_many_jobs_still_dispatches_the_giant_last_end_to_end() {
    use crate::core::chunker::{ChunkType, RawChunk};
    use crate::core::embed::Embedder;
    use crate::core::indexer::ParsedBatch;
    use crate::core::store::{UsearchStore, VectorStore};
    use std::sync::Mutex as StdMutex;

    struct SlowOrderRecordingEmbedder {
        id: String,
        order: Arc<StdMutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl Embedder for SlowOrderRecordingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            tokio::time::sleep(Duration::from_millis(5)).await;
            self.order.lock().expect("lock").push(self.id.clone());
            Ok(vec![0.1; 8])
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            tokio::time::sleep(Duration::from_millis(5)).await;
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

    async fn committed_handle(id: &str, order: &Arc<StdMutex<Vec<String>>>) -> StdArc<IndexHandle> {
        let embedder: Arc<dyn Embedder> = Arc::new(SlowOrderRecordingEmbedder {
            id: id.to_string(),
            order: Arc::clone(order),
        });
        let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(8).expect("usearch new"));
        let indexer = CodeIndexer::new(id, format!("/tmp/{id}")).with_components(embedder, store);
        let parsed = ParsedBatch {
            chunks: vec![raw_chunk(id)],
            embeddings: vec![None],
            entities_by_file: vec![],
            parse_ms: 0,
            embed_ms: 0,
            vector_count: 0,
        };
        indexer.commit_parsed_batch(parsed, false).await.ok();
        StdArc::new(IndexHandle::bare(
            IndexId::new(id),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            format!("/tmp/{id}").into(),
        ))
    }

    let order: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));

    let giant = committed_handle("burst-giant-3748", &order).await;
    let mut small = Vec::new();
    for i in 0..26 {
        small.push(committed_handle(&format!("burst-small-{i}-3748"), &order).await);
    }

    // Giant FIRST (mirrors "the giant's fast pass finished early" from
    // the original incident), then the 26 small jobs — same burst, no
    // `.await` yielding a full turn between any of these `enqueue` calls
    // beyond what `committed_handle`'s own commit already did above (all
    // handles are fully built before any `enqueue` call runs).
    enqueue(
        StdArc::clone(&giant),
        Arc::new(ReindexProgress::new()),
        94_000,
    );
    for (i, handle) in small.iter().enumerate() {
        enqueue(
            StdArc::clone(handle),
            Arc::new(ReindexProgress::new()),
            i + 1,
        );
    }

    let mut all_handles = small.clone();
    all_handles.push(StdArc::clone(&giant));
    for handle in &all_handles {
        for _ in 0..500 {
            let status = handle.stages.read().await.semantic.status;
            if status != crate::core::registry::StageStatus::Pending
                && status != crate::core::registry::StageStatus::InProgress
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    let recorded = order.lock().expect("lock").clone();
    assert_eq!(
        recorded.len(),
        27,
        "all 27 jobs (1 giant + 26 small) must have run their embed call; recorded={recorded:?}"
    );
    assert_eq!(
        recorded.last().map(String::as_str),
        Some("burst-giant-3748"),
        "the giant must be dispatched LAST out of the full ~30-job same-burst \
         arrival, even under non-instant simulated embed durations — recorded \
         order={recorded:?}"
    );
}
