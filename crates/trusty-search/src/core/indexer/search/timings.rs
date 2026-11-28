//! Per-stage search timings and the phase a query is in (#10195).
//!
//! Why: the search log line and the reply carried one latency number, so the
//! embed, lane, fusion and rehydrate cost of a slow query was inferred rather
//! than measured. A query cancelled at the deadline logged neither its index
//! nor the stage that ran out of time.
//! What: [`SearchTimings`], the per-stage milliseconds one search spent;
//! [`StageClock`], the lap timer the search pipeline drives; and the
//! task-local query phase ([`scoped`], [`mark`], [`scope_state`]) the
//! interactive deadline sets up and a cancelled search reads. A lap reads the
//! monotonic clock once. Nothing here takes a lock, allocates or does I/O.
//! #11964: [`fetch_scoped`] and [`FetchLap`] time the chunk fetches inside
//! `fusion_ms`; the timed KG and MMR wrappers charge `kg_ms` and `rerank_ms`.
//! Test: `search_reports_per_stage_timings_in_log_and_meta`,
//! `a_search_cancelled_at_the_deadline_logs_index_query_and_stage`,
//! `search_log_splits_the_fusion_stage`.

use std::cell::Cell;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::super::CodeIndexer;
use crate::core::store::search_split::{self, VectorSplit};

/// The stage a query is in; logged when the query is cancelled (#10195).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryPhase {
    /// Before any stage below started.
    Start,
    /// Resolving the index id, including a cold lazy load.
    Resolve,
    /// Waiting for the index's read lock.
    Lock,
    /// Embedding the query text and the `refine_query` text.
    Embed,
    /// The HNSW vector lane.
    Vector,
    /// The BM25 lane and the entity exact match, including a rehydrate wait.
    Lexical,
    /// The ripgrep content-scan lane, the empty-result grep fallback and the
    /// exact-match lane, including a rehydrate wait.
    Ripgrep,
    /// RRF fusion, MMR, KG expansion, score adjustments and materialisation.
    Fusion,
    /// Building the reply after the search answered.
    Respond,
    /// The all-index fan-out.
    Fanout,
}

impl QueryPhase {
    /// The stable name logged in the `stage` field.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Resolve => "resolve",
            Self::Lock => "lock",
            Self::Embed => "embed",
            Self::Vector => "vector",
            Self::Lexical => "lexical",
            Self::Ripgrep => "ripgrep",
            Self::Fusion => "fusion",
            Self::Respond => "respond",
            Self::Fanout => "fanout",
        }
    }
}

/// The deadline and current phase of one interactive query.
struct QueryScope {
    deadline_at: Instant,
    phase: Cell<QueryPhase>,
    /// #10195: set once a guard logged this query's timeout, so the deadline
    /// wrapper does not log it a second time.
    logged: Arc<AtomicBool>,
}

tokio::task_local! {
    static QUERY_SCOPE: QueryScope;
}

/// Run `fut` as one interactive query whose deadline is `deadline_at`.
///
/// Why: a cancelled search must name the stage that ran out of time, and only
/// the search itself knows its stage. tokio drops a scoped future inside its
/// scope, so a guard dropped by the cancellation still reads [`scope_state`].
/// What: a task-local holding `deadline_at`, the phase and `logged`, stored
/// inline in the returned future. [`mark_timeout_logged`] sets `logged`; the
/// caller keeps a clone to read it after the future is dropped.
/// Test: `a_search_cancelled_at_the_deadline_logs_index_query_and_stage`.
pub async fn scoped<F: Future>(deadline_at: Instant, logged: Arc<AtomicBool>, fut: F) -> F::Output {
    let scope = QueryScope {
        deadline_at,
        phase: Cell::new(QueryPhase::Start),
        logged,
    };
    QUERY_SCOPE.scope(scope, fut).await
}

/// Record that the enclosing query entered `phase`; a no-op outside [`scoped`].
pub fn mark(phase: QueryPhase) {
    let _ = QUERY_SCOPE.try_with(|s| s.phase.set(phase));
}

/// Record that the enclosing query's timeout was logged; a no-op outside
/// [`scoped`] (#10195).
pub fn mark_timeout_logged() {
    let _ = QUERY_SCOPE.try_with(|s| s.logged.store(true, Ordering::Relaxed));
}

/// The enclosing query's phase and whether its deadline has passed; `None`
/// outside [`scoped`].
pub fn scope_state() -> Option<(QueryPhase, bool)> {
    QUERY_SCOPE
        .try_with(|s| (s.phase.get(), Instant::now() >= s.deadline_at))
        .ok()
}

/// Milliseconds one search spent per stage (#10195).
///
/// Why: published in the search log line and `meta.timings`, so a slow query
/// says where its time went.
/// What: wall time per [`QueryPhase`] the indexer drives. `rehydrate_wait_ms`
/// is the time the lexical and exact-match lanes waited for a corpus
/// rehydrate, already counted inside `lexical_ms` and `ripgrep_ms`. `rehydrate_ran` is `true`
/// when the corpus was evicted and this query started or joined its
/// rehydrate; `hnsw_promoted` when the vector store promoted its mmap view to
/// a heap copy while the query ran.
/// #12108: `vector_lock_ms`, `vector_traverse_ms` and `vector_map_ms` split
/// `vector_ms` into the graph lock wait (a load, promotion or rebuild holds
/// the write lock), the HNSW traversal (page fault-in on an mmap view lands
/// here) and the key-to-chunk-id mapping; `hnsw_view` is `true` when the
/// traversal ran on the mmap view. The remainder of `vector_ms` is lane
/// overhead outside the store.
/// #11964: `chunk_fetch_ms`, `kg_ms` and `rerank_ms` sit inside `fusion_ms`.
/// `chunk_fetch_ms` is every corpus chunk fetch (score adjustments and
/// materialisation), from submit to the blocking pool to the result,
/// `chunk_fetch_queue_ms` the part of it spent before the blocking closure
/// started. `kg_ms` is KG expansion and `rerank_ms` the MMR pass. The rest of
/// `fusion_ms` is RRF, score adjustment and materialisation compute.
/// Test: `search_reports_per_stage_timings_in_log_and_meta`,
/// `search_log_splits_the_vector_lane`,
/// `stage_clock_reports_the_vector_split`,
/// `search_log_splits_the_fusion_stage`,
/// `stage_clock_reports_the_fusion_split`,
/// `a_scoped_chunk_fetch_records_its_time`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SearchTimings {
    pub embed_ms: u64,
    pub vector_ms: u64,
    pub vector_lock_ms: u64,
    pub vector_traverse_ms: u64,
    pub vector_map_ms: u64,
    pub hnsw_view: bool,
    pub lexical_ms: u64,
    pub ripgrep_ms: u64,
    pub fusion_ms: u64,
    pub chunk_fetch_ms: u64,
    pub chunk_fetch_queue_ms: u64,
    pub kg_ms: u64,
    pub rerank_ms: u64,
    pub rehydrate_wait_ms: u64,
    pub rehydrate_ran: bool,
    pub hnsw_promoted: bool,
}

/// The lap timer `run_search` drives: one clock read per stage change.
pub(crate) struct StageClock {
    lap: Instant,
    phase: QueryPhase,
    /// Microseconds per stage: embed, vector, lexical, ripgrep, fusion.
    micros: [u64; 5],
    rehydrate_ran: bool,
    promotions_at_start: u64,
    /// #12108: the store's split of the vector lap.
    vector: VectorSplit,
    /// #11964: KG expansion and the MMR pass, inside the fusion lap.
    kg: Duration,
    rerank: Duration,
}

impl StageClock {
    /// Start timing in `phase`.
    pub(crate) fn start(phase: QueryPhase, rehydrate_ran: bool, promotions: u64) -> Self {
        mark(phase);
        Self {
            lap: Instant::now(),
            phase,
            micros: [0; 5],
            rehydrate_ran,
            promotions_at_start: promotions,
            vector: VectorSplit::default(),
            kg: Duration::ZERO,
            rerank: Duration::ZERO,
        }
    }

    /// Close the current stage's lap and enter `next`.
    pub(crate) fn enter(&mut self, next: QueryPhase) {
        let now = Instant::now();
        let slot = match self.phase {
            QueryPhase::Embed => Some(0),
            QueryPhase::Vector => Some(1),
            QueryPhase::Lexical => Some(2),
            QueryPhase::Ripgrep => Some(3),
            QueryPhase::Fusion => Some(4),
            _ => None,
        };
        if let Some(i) = slot {
            let us = now.saturating_duration_since(self.lap).as_micros();
            self.micros[i] = self.micros[i].saturating_add(u64::try_from(us).unwrap_or(u64::MAX));
        }
        self.lap = now;
        self.phase = next;
        mark(next);
    }

    /// Add the vector store's split of a vector-lane lap (#12108).
    pub(crate) fn add_vector_split(&mut self, split: VectorSplit) {
        self.vector.absorb(split);
    }

    /// Close the last lap and report the totals.
    pub(crate) fn finish(mut self, promotions: u64, rehydrate_wait_ms: u64) -> SearchTimings {
        self.enter(QueryPhase::Respond);
        let ms = |i: usize| self.micros[i] / 1000;
        let whole_ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        let fetch = fetch_split(); // #11964: zero outside `fetch_scoped`
        SearchTimings {
            embed_ms: ms(0),
            vector_ms: ms(1),
            vector_lock_ms: whole_ms(self.vector.lock_wait),
            vector_traverse_ms: whole_ms(self.vector.traverse),
            vector_map_ms: whole_ms(self.vector.map),
            hnsw_view: self.vector.on_view,
            lexical_ms: ms(2),
            ripgrep_ms: ms(3),
            fusion_ms: ms(4),
            chunk_fetch_ms: whole_ms(fetch.total),
            chunk_fetch_queue_ms: whole_ms(fetch.queue),
            kg_ms: whole_ms(self.kg),
            rerank_ms: whole_ms(self.rerank),
            rehydrate_wait_ms,
            rehydrate_ran: self.rehydrate_ran,
            hnsw_promoted: promotions > self.promotions_at_start,
        }
    }
}

/// The chunk fetches one query ran, summed (#11964).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FetchSplit {
    /// Submit to result of every fetch, the queue wait included.
    pub total: Duration,
    /// Submit to the blocking closure starting: the blocking-pool queue wait.
    pub queue: Duration,
}

tokio::task_local! {
    static FETCH_SPLIT: Cell<FetchSplit>;
}

/// Run `fut` with a chunk-fetch recorder that [`StageClock::finish`] reads.
///
/// Why: #11964 — `fusion_ms` spiked to 1 s with no sub-timer, and the redb
/// chunk read on the blocking pool is one suspect.
/// What: a task-local [`FetchSplit`] every [`FetchLap`] inside `fut` adds to.
/// Test: `a_scoped_chunk_fetch_records_its_time`.
pub(crate) async fn fetch_scoped<F: Future>(fut: F) -> F::Output {
    FETCH_SPLIT
        .scope(Cell::new(FetchSplit::default()), fut)
        .await
}

/// The enclosing [`fetch_scoped`]'s sum; zero outside one.
fn fetch_split() -> FetchSplit {
    FETCH_SPLIT.try_with(Cell::get).unwrap_or_default()
}

/// One chunk fetch's lap, added to the enclosing [`fetch_scoped`] on drop, so
/// every return path of the fetch is counted (#11964).
pub(crate) struct FetchLap {
    submitted: Instant,
    queue: Duration,
}

impl FetchLap {
    /// Start the lap just before the fetch is submitted.
    pub(crate) fn start() -> Self {
        Self {
            submitted: Instant::now(),
            queue: Duration::ZERO,
        }
    }

    /// The blocking closure started at `at`; the time before it is queue wait.
    pub(crate) fn closure_started(&mut self, at: Instant) {
        self.queue = at.saturating_duration_since(self.submitted);
    }
}

impl Drop for FetchLap {
    fn drop(&mut self) {
        let lap = FetchSplit {
            total: self.submitted.elapsed(),
            queue: self.queue,
        };
        let _ = FETCH_SPLIT.try_with(|c| {
            let sum = c.get();
            c.set(FetchSplit {
                total: sum.total.saturating_add(lap.total),
                queue: sum.queue.saturating_add(lap.queue),
            });
        });
    }
}

impl CodeIndexer {
    /// How many times this index's vector store has promoted its mmap view to
    /// a heap copy; `0` with no vector store (#10195).
    pub(crate) fn hnsw_promotions(&self) -> u64 {
        self.store.as_ref().map_or(0, |s| s.promotions())
    }

    /// The vector lane, with the store's sub-step split charged to `clock`.
    ///
    /// Why: #12108 — a cold query's `vector_ms` must say whether it waited
    /// for the graph, traversed it or mapped the hits.
    /// What: runs [`Self::vector_search_scoped`] inside
    /// [`search_split::measured`] and adds the recorded split to `clock`.
    /// Ranking and the hits are unchanged.
    /// Test: `search_log_splits_the_vector_lane`.
    pub(crate) async fn vector_search_timed(
        &self,
        embedding: &[f32],
        want: usize,
        query: &crate::core::indexer::SearchQuery,
        clock: &mut StageClock,
    ) -> anyhow::Result<Vec<(String, f32)>> {
        let (hits, split) =
            search_split::measured(self.vector_search_scoped(embedding, want, query)).await;
        clock.add_vector_split(split);
        hits
    }

    /// The MMR pass, its wall time charged to `clock` as `rerank_ms` (#11964).
    pub(super) async fn mmr_rerank_timed(
        &self,
        fused_raw: Vec<(String, f32)>,
        want: usize,
        clock: &mut StageClock,
    ) -> Vec<(String, f32)> {
        let started = Instant::now();
        let out = self.apply_mmr_rerank(fused_raw, want).await;
        clock.rerank = clock.rerank.saturating_add(started.elapsed());
        out
    }

    /// KG expansion, its wall time charged to `clock` as `kg_ms` (#11964).
    pub(super) async fn expand_with_kg_timed(
        &self,
        clock: &mut StageClock,
        fused: Vec<(String, f32)>,
        intent: &crate::core::classifier::QueryIntent,
        use_kg_first: bool,
        expand_graph: bool,
        refine_embedding: Option<&[f32]>,
    ) -> (Vec<(String, f32)>, std::collections::HashSet<String>) {
        let started = Instant::now();
        let out = self
            .expand_with_kg(fused, intent, use_kg_first, expand_graph, refine_embedding)
            .await;
        clock.kg = clock.kg.saturating_add(started.elapsed());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lap is charged to the stage it ran in, and a promote between start
    /// and finish is reported.
    #[test]
    // #11445: reaches the index-identity file; see IDENTITY_REACHING.
    #[serial_test::parallel]
    fn stage_clock_charges_each_lap_to_its_stage() {
        let mut clock = StageClock::start(QueryPhase::Embed, true, 3);
        clock.lap -= std::time::Duration::from_millis(7);
        clock.enter(QueryPhase::Fusion);
        clock.lap -= std::time::Duration::from_millis(4);
        let t = clock.finish(4, 2);
        assert_eq!(t.embed_ms, 7);
        assert_eq!(t.fusion_ms, 4);
        assert_eq!(t.vector_ms, 0);
        assert_eq!(t.rehydrate_wait_ms, 2);
        assert!(t.rehydrate_ran && t.hnsw_promoted);
    }

    /// The store's split reaches the reported timings, summed per sub-step.
    #[test]
    #[serial_test::parallel]
    fn stage_clock_reports_the_vector_split() {
        let mut clock = StageClock::start(QueryPhase::Vector, false, 0);
        let ms = std::time::Duration::from_millis;
        clock.add_vector_split(VectorSplit {
            lock_wait: ms(5),
            traverse: ms(20),
            map: ms(1),
            on_view: true,
        });
        clock.add_vector_split(VectorSplit {
            traverse: ms(2),
            ..Default::default()
        });
        let t = clock.finish(0, 0);
        assert_eq!(
            (t.vector_lock_ms, t.vector_traverse_ms, t.vector_map_ms),
            (5, 22, 1)
        );
        assert!(t.hnsw_view);
    }

    /// #11964: the fetch sum recorded in the scope, and the KG and MMR laps,
    /// reach the reported timings; outside a scope the fetch fields read zero.
    #[tokio::test]
    #[serial_test::parallel]
    async fn stage_clock_reports_the_fusion_split() {
        let ms = Duration::from_millis;
        let t = fetch_scoped(async {
            let mut clock = StageClock::start(QueryPhase::Fusion, false, 0);
            clock.kg = ms(3);
            clock.rerank = ms(2);
            let _ = FETCH_SPLIT.try_with(|c| {
                c.set(FetchSplit {
                    total: ms(9),
                    queue: ms(4),
                })
            });
            clock.finish(0, 0)
        })
        .await;
        assert_eq!(
            (
                t.chunk_fetch_ms,
                t.chunk_fetch_queue_ms,
                t.kg_ms,
                t.rerank_ms
            ),
            (9, 4, 3, 2)
        );
        let bare = StageClock::start(QueryPhase::Fusion, false, 0).finish(0, 0);
        assert_eq!((bare.chunk_fetch_ms, bare.chunk_fetch_queue_ms), (0, 0));
    }

    /// #11964: a corpus fetch inside `fetch_scoped` records a lap whose queue
    /// wait is part of its total; two fetches sum.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_scoped_chunk_fetch_records_its_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let idx = corpus_indexer(dir.path()).await;
        let ids = vec!["src/auth.rs:1:1".to_string()];
        let (one, two) = fetch_scoped(async {
            idx.fetch_chunks_for_ids(&ids).await.expect("fetch");
            let one = fetch_split();
            idx.fetch_chunks_for_ids(&ids).await.expect("fetch");
            (one, fetch_split())
        })
        .await;
        assert!(one.total > Duration::ZERO, "{one:?}");
        assert!(one.queue <= one.total, "{one:?}");
        assert!(two.total > one.total, "two fetches sum: {one:?} {two:?}");
    }

    /// Outside a query scope the phase is not tracked and `mark` is a no-op.
    #[test]
    #[serial_test::parallel]
    fn mark_outside_a_scope_is_a_no_op() {
        mark(QueryPhase::Embed);
        assert!(scope_state().is_none());
    }

    /// An indexer with a durable corpus and one indexed file (BM25 only).
    async fn corpus_indexer(dir: &std::path::Path) -> CodeIndexer {
        let mut idx = CodeIndexer::new("timings-10195", "/tmp/timings-10195");
        let corpus = crate::core::corpus::CorpusStore::open(&dir.join("index.redb"))
            .expect("open corpus store");
        idx.set_corpus_store(std::sync::Arc::new(corpus));
        idx.index_files_batch(&[("src/auth.rs".into(), "fn authenticate() {}".into())])
            .await
            .expect("index batch");
        idx
    }

    fn query() -> crate::core::indexer::SearchQuery {
        crate::core::indexer::SearchQuery {
            text: "authenticate".to_string(),
            top_k: 5,
            ..Default::default()
        }
    }

    /// A search against an evicted corpus reports that it started the
    /// rehydrate; a search of a resident corpus reports none and no wait.
    #[tokio::test]
    #[serial_test::serial]
    async fn search_timings_report_whether_a_rehydrate_ran() {
        let dir = tempfile::tempdir().expect("tempdir");
        let idx = corpus_indexer(dir.path()).await;
        let warm = idx.search_with_outcome(&query()).await.expect("search");
        assert!(!warm.timings.rehydrate_ran && !warm.timings.hnsw_promoted);
        assert_eq!(warm.timings.rehydrate_wait_ms, 0, "{:?}", warm.timings);

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let idle = std::time::Duration::from_nanos(1);
        idx.evict_bm25_entities_if_idle(idle).await;
        idx.evict_chunks_if_idle(idle).await;
        let cold = idx.search_with_outcome(&query()).await.expect("search");
        assert!(cold.timings.rehydrate_ran, "{:?}", cold.timings);
    }

    /// Inside a scope the last marked phase and the deadline are readable.
    #[tokio::test]
    #[serial_test::parallel]
    async fn scope_state_reports_the_marked_phase_and_deadline() {
        let past = Instant::now();
        let state = scoped(past, Arc::default(), async {
            mark(QueryPhase::Lexical);
            scope_state()
        })
        .await;
        assert_eq!(state, Some((QueryPhase::Lexical, true)));
    }
}
