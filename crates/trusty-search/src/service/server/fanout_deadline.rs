//! One deadline and one query embed for the all-index fan-out (#9027).
//!
//! Why: `POST /search` waited on every index it fanned out to, with only the
//! 30 s request timeout above it. One idle-evicted index rehydrating its corpus
//! held the whole answer for ~26 s, and the query text was embedded once for
//! routing and again inside every per-index search.
//! What: [`resolve_index_deadline`], [`resolve_embed_timeout`],
//! [`embed_query_once`], [`kick_evicted_rehydrates`] and [`search_one_index`],
//! which runs one index's search under the deadline and classifies every way
//! it can contribute nothing ([`LaneOutcome`]).
//! Test: `global_search_skips_an_index_that_misses_the_deadline`,
//! `global_search_embeds_the_query_once_for_every_index`,
//! `a_slow_embed_degrades_to_lexical_and_still_searches_every_index`,
//! `the_fan_out_deadline_starts_after_the_query_embed`, `parse_deadline_env`.

use std::sync::Arc;
use std::time::Duration;

use crate::core::indexer::{
    CodeChunk, CorpusReadUnavailable, IndexMigrationInProgress, PrecomputedQueryVector, SearchQuery,
};
use crate::core::registry::{IndexId, IndexRegistry};

/// Env var that sets the fan-out deadline, in milliseconds. The name says
/// "index" for compatibility; the deadline is one instant for the whole fan-out.
pub(super) const FANOUT_INDEX_DEADLINE_ENV: &str = "TRUSTY_SEARCH_FANOUT_INDEX_DEADLINE_MS";

/// Default fan-out deadline: 3 s, measured from the start of the fan-out.
///
/// Why: a warm per-index search answers in well under a second (0.06–2.5 s
/// measured on #9027); a cold one waits on an O(corpus) rehydrate of 10 s or
/// more. 3 s keeps every warm index; with the embed timeout before it, a cold
/// fan-out stays under the 5 s target the issue set.
pub(super) const DEFAULT_FANOUT_INDEX_DEADLINE_MS: u64 = 3_000;

/// Env var that bounds the fan-out's one query embed, in milliseconds.
pub(super) const FANOUT_EMBED_TIMEOUT_ENV: &str = "TRUSTY_SEARCH_FANOUT_EMBED_TIMEOUT_MS";

/// Default query-embed timeout: 1 s.
///
/// Why: a warm embed answers in tens of milliseconds; a cold embedder respawn
/// takes 2–15 s (`TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS`). Waiting for the respawn
/// spent the whole fan-out deadline before any index was searched (#9027).
pub(super) const DEFAULT_FANOUT_EMBED_TIMEOUT_MS: u64 = 1_000;

/// Parse a raw millisecond env value; unset, unparseable or zero gives `default_ms`.
fn parse_ms_env(raw: Option<String>, default_ms: u64) -> Duration {
    let ms = raw
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .unwrap_or(default_ms);
    Duration::from_millis(ms)
}

/// Parse the raw deadline env value; unset, unparseable or zero gives the default.
///
/// Test: `parse_deadline_env`.
pub(super) fn parse_deadline_env(raw: Option<String>) -> Duration {
    parse_ms_env(raw, DEFAULT_FANOUT_INDEX_DEADLINE_MS)
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

/// The fan-out's query-embed timeout: the env var, else the default.
///
/// Test: `parse_deadline_env`.
pub(super) fn resolve_embed_timeout() -> Duration {
    parse_ms_env(
        std::env::var(FANOUT_EMBED_TIMEOUT_ENV).ok(),
        DEFAULT_FANOUT_EMBED_TIMEOUT_MS,
    )
}

/// How the fan-out's one query embed ended (#9027).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EmbedStatus {
    /// One vector serves every index.
    Embedded,
    /// The embedder answered with an error; every index searches lexically.
    Failed,
    /// The embed missed its timeout; every index searches lexically.
    TimedOut,
    /// No index could embed (BM25-only fleet, or every lock held); each
    /// per-index search embeds for itself.
    PerIndex,
}

impl EmbedStatus {
    /// The `query_embed` value the response carries.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::PerIndex => "per_index",
        }
    }

    /// `true` when the fan-out answered without its vector lane.
    pub(super) fn degraded(self) -> bool {
        matches!(self, Self::Failed | Self::TimedOut)
    }
}

/// The fan-out's one query embed: the vector or failure every index reuses,
/// and how it ended.
pub(super) struct FanoutEmbed {
    pub(super) vector: Option<Arc<Result<Arc<Vec<f32>>, String>>>,
    pub(super) status: EmbedStatus,
}

/// The fan-out's one query embed, bounded by `timeout`.
///
/// Why: every index shares the daemon's embedder, so one vector serves them
/// all. A failure is carried as `Failed` so the per-index searches degrade to
/// lexical at once instead of each retrying the same embedder (#8348). A cold
/// embedder respawn outlasted the whole fan-out deadline, so the embed has its
/// own timeout, and a timeout degrades the same way (#9027).
/// What: the embed runs on a detached task, so a timeout never cancels a
/// sidecar spawn half-way; the task finishes, warms the embedder and fills the
/// query-embed cache for the next request. A timeout or a failure answers
/// [`PrecomputedQueryVector::Failed`] text; [`FanoutEmbed::status`] says which.
/// Test: `global_search_embeds_the_query_once_for_every_index`,
/// `a_slow_embed_degrades_to_lexical_and_still_searches_every_index`.
pub(super) async fn embed_query_once(
    registry: &IndexRegistry,
    ids: &[IndexId],
    text: &str,
    timeout: Duration,
) -> FanoutEmbed {
    let (registry, ids, text) = (registry.clone(), ids.to_vec(), text.to_string());
    let task = tokio::spawn(async move { embed_first_available(&registry, &ids, &text).await });
    let (vector, status) = match tokio::time::timeout(timeout, task).await {
        Ok(Ok(Some(Ok(v)))) => (Some(Ok(Arc::new(v))), EmbedStatus::Embedded),
        Ok(Ok(Some(Err(e)))) => (Some(Err(e)), EmbedStatus::Failed),
        Ok(Ok(None)) => (None, EmbedStatus::PerIndex),
        Ok(Err(e)) => (
            Some(Err(format!("query embed task failed: {e}"))),
            EmbedStatus::Failed,
        ),
        Err(_) => {
            let ms = timeout.as_millis();
            tracing::warn!(
                "global search: query embed missed its {ms} ms timeout; every index answers \
                 from the lexical lane (#9027)"
            );
            let msg = format!("query embed timed out after {ms} ms ({FANOUT_EMBED_TIMEOUT_ENV})");
            (Some(Err(msg)), EmbedStatus::TimedOut)
        }
    };
    FanoutEmbed {
        vector: vector.map(Arc::new),
        status,
    }
}

/// Embed `text` through the first index with an embedder wired whose lock is
/// free right now, via that index's query-embed cache.
///
/// `try_read`, never a wait: an index held by a reindex would otherwise stall
/// the request. `None` when no index could embed at all.
async fn embed_first_available(
    registry: &IndexRegistry,
    ids: &[IndexId],
    text: &str,
) -> Option<Result<Vec<f32>, String>> {
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
            Ok(Some(vector)) => return Some(Ok(vector)),
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
/// the deadline. Kicking them all before the fan-out rehydrates in parallel,
/// so the next search finds them warm (#9027). The caller passes only the
/// indexes routing kept; an index the search will not touch stays evicted.
/// What: non-blocking; skips an index whose lock is held. Never waits.
/// Test: `global_search_skips_an_index_that_misses_the_deadline`,
/// `global_search_kicks_rehydrates_only_for_routed_indexes`.
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
    /// #9027: the search missed the fan-out deadline.
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
/// fan-out, taken just before the fan-out starts, so the request is bounded by
/// it however many waves the concurrency cap splits the fan-out into.
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
                "global search: index '{id}' missed the fan-out deadline; skipped and \
                 reported in deadline_indexes_skipped (#9027)"
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
    fn embed_status_labels_and_degradation() {
        assert_eq!(EmbedStatus::TimedOut.label(), "timed_out");
        assert!(EmbedStatus::TimedOut.degraded() && EmbedStatus::Failed.degraded());
        assert!(!EmbedStatus::Embedded.degraded() && !EmbedStatus::PerIndex.degraded());
    }

    #[test]
    fn request_deadline_wins_over_env() {
        assert_eq!(resolve_index_deadline(Some(40)), Duration::from_millis(40));
        assert_eq!(resolve_index_deadline(Some(0)), Duration::from_millis(1));
    }
}
