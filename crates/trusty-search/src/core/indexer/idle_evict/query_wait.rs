//! The query path's bounded wait for an in-flight corpus rehydrate (#10193).
//!
//! Why: a query lane used to wait up to `TRUSTY_REHYDRATE_WAIT_MS` (9 s) per
//! retry for the rehydrate, and each lane — BM25, entity match, exact match,
//! grep fallback — waited on its own, so a cold query blocked for as long as
//! the rehydrate ran. A query now waits a short budget ONCE, shared by every
//! lane, and then answers without the lexical lanes, flagged
//! `meta.bm25_lane_degraded`.
//! What: [`CodeIndexer::with_query_rehydrate_deadline`] scopes a per-query
//! deadline; the search starts (or joins) the rehydrate before its embed, so
//! the two overlap. [`CodeIndexer::await_corpus_for_query`] waits for the
//! rehydrate up to that deadline. Writers keep the full
//! `ensure_corpus_rehydrated` wait.
//! Test: `a_query_during_a_slow_bm25_build_answers_degraded_within_the_wait`,
//! `lane_degraded_flag_sets_on_exhausted_retries_and_clears_on_rehydrate`.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use super::super::CodeIndexer;

/// Longest a query waits for an in-flight rehydrate before its lexical lanes
/// degrade (#10193).
///
/// Why: long enough for a small index's whole rehydrate, short enough that a
/// 140k-chunk rebuild (seconds) never sets the query's latency.
/// What: 500 ms, capped further by `TRUSTY_REHYDRATE_WAIT_MS` when that is
/// set lower, so the one existing knob still bounds every rehydrate wait.
pub(super) const QUERY_REHYDRATE_WAIT_MS: u64 = 500;

/// Poll slice for [`CodeIndexer::await_corpus_for_query`]: a wakeup the
/// waiter missed (see `ensure_corpus_rehydrated`'s accepted race) costs at
/// most this long, not the whole budget.
const QUERY_WAIT_SLICE: Duration = Duration::from_millis(50);

tokio::task_local! {
    /// The instant this query's rehydrate wait ends, shared by every lane.
    static QUERY_REHYDRATE_DEADLINE: Instant;
}

/// The per-query rehydrate budget: [`QUERY_REHYDRATE_WAIT_MS`], or the
/// general `TRUSTY_REHYDRATE_WAIT_MS` budget when that is shorter.
fn query_rehydrate_wait() -> Duration {
    Duration::from_millis(QUERY_REHYDRATE_WAIT_MS).min(super::rehydrate_wait_budget())
}

impl CodeIndexer {
    /// Run one search as a query with a single shared rehydrate deadline.
    ///
    /// Why: #10193 — the lanes of one query must share one short wait, not
    /// stack a wait each.
    /// What: runs `fut` with a task-local deadline [`query_rehydrate_wait`]
    /// from now. `fut` starts the rehydrate itself, after its own refusal
    /// checks, so a deleted or migrating index scans nothing.
    /// Test: `a_query_during_a_slow_bm25_build_answers_degraded_within_the_wait`.
    pub(crate) async fn with_query_rehydrate_deadline<F: Future>(&self, fut: F) -> F::Output {
        let deadline = Instant::now() + query_rehydrate_wait();
        QUERY_REHYDRATE_DEADLINE.scope(deadline, fut).await
    }

    /// Wait for an evicted corpus to rehydrate, up to the query's deadline.
    ///
    /// Why: #10193 — a query lane must not wait out the rehydrate; it answers
    /// degraded instead, and the detached rehydrate keeps running.
    /// What: `true` at once when nothing is evicted. Otherwise starts or joins
    /// the rehydrate and waits until it commits (`true`) or the deadline passes
    /// (`false`). The deadline is the enclosing query's
    /// ([`Self::with_query_rehydrate_deadline`]); a caller outside a query
    /// gets a fresh [`query_rehydrate_wait`]. An attempt that ends without
    /// committing starts a new one, unless it recorded a corpus read fault,
    /// which returns `false` at once rather than rescanning a failing corpus.
    /// Test: `a_query_during_a_slow_bm25_build_answers_degraded_within_the_wait`,
    /// `bm25_search_survives_reclaim_race_between_ensure_and_read`.
    pub(crate) async fn await_corpus_for_query(&self) -> bool {
        let deadline = QUERY_REHYDRATE_DEADLINE
            .try_with(|d| *d)
            .unwrap_or_else(|_| Instant::now() + query_rehydrate_wait());
        loop {
            let Some(notify) = self.begin_corpus_rehydrate() else {
                return true;
            };
            loop {
                if !self.corpus_evicted() {
                    return true;
                }
                if !self.rehydrate_attempt_running(&notify) {
                    break;
                }
                let now = Instant::now();
                if now >= deadline {
                    return false;
                }
                let slice = (deadline - now).min(QUERY_WAIT_SLICE);
                let _ = tokio::time::timeout(slice, notify.notified()).await;
            }
            // The attempt ended without clearing the flags: a failed read, or
            // an evict that raced its commit.
            if self.corpus_read_fault.detail().is_some() || Instant::now() >= deadline {
                return !self.corpus_evicted();
            }
        }
    }

    /// `true` while `notify`'s rehydrate attempt still holds the gate.
    fn rehydrate_attempt_running(&self, notify: &Arc<Notify>) -> bool {
        self.rehydrate_inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|g| Arc::ptr_eq(g, notify))
    }
}
