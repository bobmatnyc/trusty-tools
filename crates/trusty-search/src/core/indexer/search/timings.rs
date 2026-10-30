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
//! Test: `search_reports_per_stage_timings_in_log_and_meta`,
//! `a_search_cancelled_at_the_deadline_logs_index_query_and_stage`.

use std::cell::Cell;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use super::super::CodeIndexer;

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
/// Test: `search_reports_per_stage_timings_in_log_and_meta`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SearchTimings {
    pub embed_ms: u64,
    pub vector_ms: u64,
    pub lexical_ms: u64,
    pub ripgrep_ms: u64,
    pub fusion_ms: u64,
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

    /// Close the last lap and report the totals.
    pub(crate) fn finish(mut self, promotions: u64, rehydrate_wait_ms: u64) -> SearchTimings {
        self.enter(QueryPhase::Respond);
        let ms = |i: usize| self.micros[i] / 1000;
        SearchTimings {
            embed_ms: ms(0),
            vector_ms: ms(1),
            lexical_ms: ms(2),
            ripgrep_ms: ms(3),
            fusion_ms: ms(4),
            rehydrate_wait_ms,
            rehydrate_ran: self.rehydrate_ran,
            hnsw_promoted: promotions > self.promotions_at_start,
        }
    }
}

impl CodeIndexer {
    /// How many times this index's vector store has promoted its mmap view to
    /// a heap copy; `0` with no vector store (#10195).
    pub(crate) fn hnsw_promotions(&self) -> u64 {
        self.store.as_ref().map_or(0, |s| s.promotions())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lap is charged to the stage it ran in, and a promote between start
    /// and finish is reported.
    #[test]
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

    /// Outside a query scope the phase is not tracked and `mark` is a no-op.
    #[test]
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
