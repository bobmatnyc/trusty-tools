//! One deadline and one query embed for the all-index fan-out (#9027).
//!
//! Why: `POST /search` waited on every index it fanned out to, with only the
//! 30 s request timeout above it. One idle-evicted index rehydrating its corpus
//! held the whole answer for ~26 s, and the query text was embedded once for
//! routing and again inside every per-index search.
//! What: [`resolve_index_deadline`], [`embed_query_once`],
//! [`kick_evicted_rehydrates`] and [`search_one_index`], which runs one index's
//! search under the deadline and classifies every way it can contribute
//! nothing ([`LaneOutcome`]).
//! Test: `global_search_skips_an_index_that_misses_the_deadline`,
//! `global_search_embeds_the_query_once_for_every_index`, `parse_deadline_env`.

use std::sync::Arc;
use std::time::Duration;

use crate::core::indexer::{
    CodeChunk, CorpusReadUnavailable, IndexMigrationInProgress, PrecomputedQueryVector, SearchQuery,
};
use crate::core::registry::{IndexId, IndexRegistry};

/// Env var that sets the per-index fan-out deadline, in milliseconds.
pub(super) const FANOUT_INDEX_DEADLINE_ENV: &str = "TRUSTY_SEARCH_FANOUT_INDEX_DEADLINE_MS";

/// Default per-index fan-out deadline: 3 s.
///
/// Why: a warm per-index search answers in well under a second (0.06–2.5 s
/// measured on #9027); a cold one waits on an O(corpus) rehydrate of 10 s or
/// more. 3 s keeps every warm index and bounds a cold fan-out under the 5 s
/// target the issue set.
pub(super) const DEFAULT_FANOUT_INDEX_DEADLINE_MS: u64 = 3_000;

/// Parse the raw env value; unset, unparseable or zero gives the default.
///
/// Test: `parse_deadline_env`.
pub(super) fn parse_deadline_env(raw: Option<String>) -> Duration {
    let ms = raw
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .unwrap_or(DEFAULT_FANOUT_INDEX_DEADLINE_MS);
    Duration::from_millis(ms)
}

/// The deadline one fan-out applies: the request's `per_index_deadline_ms`
/// (clamped to at least 1 ms), else the env var, else the default.
///
/// Test: `parse_deadline_env`, `request_deadline_wins_over_env`.
pub(super) fn resolve_index_deadline(request_ms: Option<u64>) -> Duration {
    match request_ms {
        Some(ms) => Duration::from_millis(ms.max(1)),
        None => parse_deadline_env(std::env::var(FANOUT_INDEX_DEADLINE_ENV).ok()),
    }
}

/// The fan-out's one query embed.
///
/// Why: every index shares the daemon's embedder, so one vector serves them
/// all. A failure is carried as `Failed` so the per-index searches degrade to
/// lexical at once instead of each retrying the same embedder (#8348).
/// What: embeds through the first index with an embedder wired whose lock is
/// free right now, via that index's query-embed cache — `try_read`, never a wait, because an index held by a
/// reindex would otherwise stall the request before any deadline applies.
/// `None` when no index could embed at all (BM25-only fleet, or every lock
/// held); each per-index search then embeds for itself as before.
/// Test: `global_search_embeds_the_query_once_for_every_index`.
pub(super) async fn embed_query_once(
    registry: &IndexRegistry,
    ids: &[IndexId],
    text: &str,
) -> Option<Result<Arc<Vec<f32>>, String>> {
    for id in ids {
        let Some(handle) = registry.get(id) else {
            continue;
        };
        let Ok(indexer) = handle.indexer.try_read() else {
            continue;
        };
        // #5024: `embed_query` reads the query-embed cache, so a repeated
        // all-index query costs no embedder call (`embed_text` bypassed it).
        match indexer.embed_query(text).await {
            Ok(Some(vector)) => return Some(Ok(Arc::new(vector))),
            Ok(None) => continue,
            Err(e) => return Some(Err(format!("{e:#}"))),
        }
    }
    None
}

/// Start the detached corpus rehydrate of every evicted index in `ids`.
///
/// Why: with a bounded fan-out, an index queued behind the concurrency cap
/// would otherwise start rehydrating only when its turn came — possibly after
/// the deadline. Kicking them all at request start rehydrates in parallel, so
/// the next search finds them warm (#9027).
/// What: non-blocking; skips an index whose lock is held. Never waits.
/// Test: `global_search_skips_an_index_that_misses_the_deadline`.
pub(super) fn kick_evicted_rehydrates(registry: &IndexRegistry, ids: &[IndexId]) {
    for id in ids {
        if let Some(handle) = registry.get(id) {
            if let Ok(indexer) = handle.indexer.try_read() {
                let _ = indexer.begin_corpus_rehydrate();
            }
        }
    }
}

/// Why one index contributed no lane to the fan-out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SkipReason {
    /// #4087: the durable corpus failed to open.
    CorpusFailed,
    /// #5917: the corpus opened and then failed a read.
    CorpusReadFailed,
    /// #6581: a schema migration is rebuilding the corpus.
    MigrationInProgress,
    /// #9027: the search missed the per-index deadline.
    Deadline,
    /// Any other search error; logged, not counted (pre-#9027 behaviour).
    Errored,
}

/// What one index's search produced.
pub(super) enum LaneOutcome {
    /// The index answered within the deadline.
    Searched(IndexId, Vec<CodeChunk>),
    /// The index contributed nothing, for `SkipReason`.
    Skipped(IndexId, SkipReason),
    /// The index left the registry between listing and searching.
    Gone,
}

/// Run one index's search under the fan-out deadline.
///
/// Why: see the module doc. The deadline is one instant shared by the whole
/// fan-out, so the request is bounded by it however many waves the concurrency
/// cap splits the fan-out into.
/// What: the corpus-failed check, the lock wait and the search all run inside
/// `timeout_at(deadline_at)`. A miss is [`SkipReason::Deadline`]; any detached
/// rehydrate the search started keeps running. `query_vector` is the fan-out's
/// one embed, passed to the search unchanged.
/// Test: `global_search_skips_an_index_that_misses_the_deadline`.
pub(super) async fn search_one_index(
    registry: IndexRegistry,
    id: IndexId,
    query: SearchQuery,
    query_vector: Option<Arc<Result<Arc<Vec<f32>>, String>>>,
    deadline_at: tokio::time::Instant,
) -> LaneOutcome {
    let work = async {
        let Some(handle) = registry.get(&id) else {
            return LaneOutcome::Gone;
        };
        if super::degraded::is_corpus_failed(&handle).await {
            return LaneOutcome::Skipped(id.clone(), SkipReason::CorpusFailed);
        }
        let pre = query_vector.as_deref().map(|r| match r {
            Ok(v) => PrecomputedQueryVector::Embedded(v.as_slice()),
            Err(e) => PrecomputedQueryVector::Failed(e.as_str()),
        });
        let indexer = handle.indexer.read().await;
        match indexer.search_with_query_vector(&query, pre).await {
            Ok(results) => LaneOutcome::Searched(id.clone(), results),
            Err(e) if e.downcast_ref::<CorpusReadUnavailable>().is_some() => {
                tracing::warn!(index_id = %id, "global search: corpus unreadable ({e:#}) (#5917)");
                LaneOutcome::Skipped(id.clone(), SkipReason::CorpusReadFailed)
            }
            Err(e) if e.downcast_ref::<IndexMigrationInProgress>().is_some() => {
                tracing::warn!(index_id = %id, "global search: migration in progress (#6581)");
                LaneOutcome::Skipped(id.clone(), SkipReason::MigrationInProgress)
            }
            Err(e) => {
                tracing::warn!("global search: index {id} errored: {e}");
                LaneOutcome::Skipped(id.clone(), SkipReason::Errored)
            }
        }
    };
    match tokio::time::timeout_at(deadline_at, work).await {
        Ok(outcome) => outcome,
        Err(_) => {
            tracing::warn!(
                index_id = %id,
                "global search: index '{id}' missed the per-index deadline; skipped and \
                 reported in rehydrating_indexes_skipped (#9027)"
            );
            LaneOutcome::Skipped(id, SkipReason::Deadline)
        }
    }
}

/// Per-reason skip counts plus the ids that missed the deadline.
#[derive(Debug, Default)]
pub(super) struct SkipTally {
    pub(super) corpus_failed: usize,
    pub(super) corpus_read_failed: usize,
    pub(super) migration_in_progress: usize,
    /// Sorted ids of the indexes that missed the deadline.
    pub(super) deadline_ids: Vec<String>,
}

/// Split fan-out outcomes into the searched lanes and the skip tally.
///
/// Test: `global_search_skips_an_index_that_misses_the_deadline`.
pub(super) fn split_outcomes(
    outcomes: Vec<LaneOutcome>,
) -> (Vec<(IndexId, Vec<CodeChunk>)>, SkipTally) {
    let mut searched = Vec::with_capacity(outcomes.len());
    let mut tally = SkipTally::default();
    for outcome in outcomes {
        match outcome {
            LaneOutcome::Searched(id, results) => searched.push((id, results)),
            LaneOutcome::Skipped(_, SkipReason::CorpusFailed) => tally.corpus_failed += 1,
            LaneOutcome::Skipped(_, SkipReason::CorpusReadFailed) => tally.corpus_read_failed += 1,
            LaneOutcome::Skipped(_, SkipReason::MigrationInProgress) => {
                tally.migration_in_progress += 1
            }
            LaneOutcome::Skipped(id, SkipReason::Deadline) => tally.deadline_ids.push(id.0),
            LaneOutcome::Skipped(_, SkipReason::Errored) | LaneOutcome::Gone => {}
        }
    }
    tally.deadline_ids.sort();
    (searched, tally)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_deadline_env() {
        let default = Duration::from_millis(DEFAULT_FANOUT_INDEX_DEADLINE_MS);
        assert_eq!(super::parse_deadline_env(None), default);
        assert_eq!(super::parse_deadline_env(Some("0".into())), default);
        assert_eq!(super::parse_deadline_env(Some("junk".into())), default);
        assert_eq!(
            super::parse_deadline_env(Some(" 750 ".into())),
            Duration::from_millis(750)
        );
    }

    #[test]
    fn request_deadline_wins_over_env() {
        assert_eq!(resolve_index_deadline(Some(40)), Duration::from_millis(40));
        assert_eq!(resolve_index_deadline(Some(0)), Duration::from_millis(1));
    }
}
