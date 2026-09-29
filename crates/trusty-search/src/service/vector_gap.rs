//! Reconcile the semantic stage against the live vector count (#8726, #8863).
//!
//! Why: warm boot derives `semantic` from the HNSW snapshot's existence alone,
//! and nothing re-checks it against the store afterwards. Any pass that leaves
//! chunks without a vector therefore left the stage `ready` with
//! `vectors_present < chunk_count` and no backfill queued. M005's re-chunk
//! (#6581) is one such pass: it hands a stored vector only to text the corpus
//! already held and leaves the rest to an embed catch-up that nothing queued.
//! The mirror case (#8863): a restore that discards a torn snapshot derives
//! `pending` over a full corpus and an empty store, and nothing ever queued the
//! embed for it either — the stage sat at `pending` until a manual reindex.
//! What: [`semantic_vector_gap`] is the pure rule; [`reconcile_semantic_vector_gap`]
//! applies it to a live handle and queues the catch-up through the serialized
//! deferred-embed queue. Called after a restore registers a handle and after
//! the boot migration chain succeeds. [`gap_after_embed_pass`] measures the gap
//! by chunk id when a deferred-embed pass settles, so the pass cannot publish
//! `Ready` over a gap it did not close (#8884).
//! Test: `tests` below, and through the boot migration path
//! `m005_vector_gap_is_not_ready_and_is_backfilled` in
//! `core::migration::m005::tests`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use crate::core::indexer::VectorCoverage;
use crate::core::registry::{IndexHandle, StageState, StageStatus};

/// How many chunks a `Ready` or `Pending` semantic stage is short of a vector
/// (#8726, #8863).
///
/// Why: `Ready` publishes the vector lane to search as complete. Over a store
/// that holds fewer vectors than the corpus holds chunks, the missing chunks
/// are unreachable by every semantic query while the index reports healthy.
/// `Pending` at restore means no pass owns the work: warm boot and lazy restore
/// derive it when the snapshot is absent or was discarded, and nothing else
/// queues the embed (#8863).
/// What: `Some(chunk_count - vectors)` only when `status` is `Ready` or
/// `Pending`, a store is wired (`vector_count` is `Some`) and it holds fewer
/// vectors than `chunk_count`. `None` otherwise: `InProgress` already has a
/// pass, `Failed`/`Skipped` are terminal, and an unwired store is the BM25-only
/// steady state.
/// Test: `gap_is_reported_for_a_ready_or_pending_stage_short_of_vectors`.
pub fn semantic_vector_gap(
    status: StageStatus,
    chunk_count: usize,
    vector_count: Option<usize>,
) -> Option<usize> {
    // #8863: `Pending` owes the same backfill `Ready` does; nothing else queues it.
    if !matches!(status, StageStatus::Ready | StageStatus::Pending) {
        return None;
    }
    let vectors = vector_count?;
    (vectors < chunk_count).then(|| chunk_count - vectors)
}

/// The durable corpus chunk count, falling back to the in-memory map.
fn corpus_chunk_count(indexer: &crate::core::indexer::CodeIndexer) -> usize {
    indexer
        .corpus_arc()
        .and_then(|c| c.chunk_count().ok())
        .unwrap_or_else(|| indexer.chunk_count())
}

/// How long a settle waits for in-flight index writers before its re-read
/// (#8884). A pending write lock stalls new readers, so this stays short.
const WRITER_SETTLE_CEILING: Duration = Duration::from_secs(5);

/// What a finished deferred-embed pass may publish (#8884).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PassCoverage {
    /// Every corpus chunk has a vector except `rejected`, whose embedding the
    /// store refused as NaN or all-zero (#764). Settles `Ready`.
    Covered { rejected: usize },
    /// Chunks no pass attempted still lack a vector, or coverage cannot be
    /// measured. Settles `Failed` with this reason and keeps the marker.
    Short(String),
}

/// Judge whether a finished deferred-embed pass may publish `Ready` (#8884).
///
/// Why: the pass plans from the in-memory chunk map, so a map that has lost
/// sight of corpus rows — the resumed-first-walk adoption of #8884 — finishes
/// without error while those rows still have no vector. Publishing `Ready`
/// then hides the gap until a restart. A count comparison over-reports: a
/// refused NaN/zero embedding (#764) recurs on every pass, and `remove_file`
/// drops a vector before its redb row, so a read between the two sees a gap
/// that the removal is about to close.
/// What: measures coverage by chunk id (`CodeIndexer::vector_coverage`) and
/// subtracts `rejected`, the ids this pass's commit refused. What remains is
/// the unattempted gap. Before judging a gap or an unreadable store `Short`,
/// waits up to [`WRITER_SETTLE_CEILING`] for the indexer write lock, which
/// every in-flight `remove_file` holds a read guard against, and re-reads
/// once. No embedder or no store wired is `Covered` (#4707's vectorless
/// index).
/// Test: `a_deferred_pass_that_leaves_chunks_unembedded_is_not_ready`,
/// `a_rejected_embedding_is_reported_and_does_not_fail_the_stage`,
/// `a_never_attempted_chunk_fails_the_stage_even_when_the_counts_match`,
/// `a_removal_racing_the_settle_does_not_fail_the_stage`,
/// `an_unreadable_store_does_not_let_a_pass_settle_ready`.
pub(crate) async fn gap_after_embed_pass(
    handle: &IndexHandle,
    rejected: &[String],
) -> PassCoverage {
    let rejected: HashSet<&str> = rejected.iter().map(String::as_str).collect();
    let first = judge_coverage(
        handle.indexer.read().await.vector_coverage().await,
        &rejected,
    );
    if matches!(first, PassCoverage::Covered { .. }) {
        return first;
    }
    // #8884: let an in-flight removal delete its redb row, then re-read once.
    let _ = tokio::time::timeout(WRITER_SETTLE_CEILING, handle.indexer.write()).await;
    judge_coverage(
        handle.indexer.read().await.vector_coverage().await,
        &rejected,
    )
}

/// The verdict for one coverage read; see [`gap_after_embed_pass`].
fn judge_coverage(coverage: VectorCoverage, rejected: &HashSet<&str>) -> PassCoverage {
    const RE_ARM: &str = "The deferred-embed marker is kept, so the next boot re-arms the pass; \
                          `PATCH /indexes/{id}/config {\"vector\": true}` re-arms it now (#8884)";
    match coverage {
        VectorCoverage::NotApplicable => PassCoverage::Covered { rejected: 0 },
        VectorCoverage::Unreadable(why) => PassCoverage::Short(format!(
            "the semantic embed pass cannot confirm its coverage: {why}. {RE_ARM}"
        )),
        VectorCoverage::Measured { chunks, missing } => {
            let refused = missing
                .iter()
                .filter(|id| rejected.contains(id.as_str()))
                .count();
            let gap = missing.len() - refused;
            if gap == 0 {
                return PassCoverage::Covered { rejected: refused };
            }
            PassCoverage::Short(format!(
                "the semantic embed pass finished with {gap} of {chunks} chunks still without a \
                 vector, so the stage is not reported ready. {RE_ARM}"
            ))
        }
    }
}

/// Settle the semantic stage of a restored handle whose store is short of the
/// corpus, and queue the backfill that closes the gap (#8726, #8863).
///
/// Why: see the module doc. Reporting the gap without queueing the work would
/// leave the index degraded until an unrelated reindex; queueing it without
/// demoting the stage would keep search routing through a lane that cannot
/// reach the missing chunks.
/// What: a no-op for a `skip_vector` or `lexical_only` handle. Otherwise reads
/// the durable chunk count, the live vector count and whether an embedder is
/// wired. When [`semantic_vector_gap`] reports a gap and an embedder is wired,
/// sets `semantic` to `InProgress` with `total` = the chunk count, then enqueues
/// the C2 pass through `reindex::spawn_deferred_embed_pass` — the queue that
/// takes the background permit and then the per-index permit, so it never
/// overlaps a reindex or a migration. That pass embeds only the chunks the store
/// lacks and settles the stage `Ready` or `Failed`. A gap that nothing can close
/// is a terminal `Failed` naming the reason, never a `Pending` nothing will
/// start (#8863): no embedder wired, or a `Pending` or `Ready` stage over a
/// store whose size cannot be read. A gap made only of chunks the durable
/// refusal record excuses at their current content leaves the stage as it is
/// and records the count in `vectors_rejected` (#8884); an unreadable record
/// excuses nothing. Returns `true` only when a pass was queued.
/// Test: `a_gap_demotes_the_stage_and_queues_a_backfill`,
/// `a_restore_after_a_refused_embedding_does_not_demote_the_stage`,
/// `an_unreadable_refusal_record_is_treated_as_a_real_gap`,
/// `a_refused_chunk_whose_content_changed_is_backfilled`,
/// `a_new_chunk_beside_a_refused_one_is_backfilled`,
/// `no_gap_leaves_a_ready_stage_alone`,
/// `a_pending_stage_left_by_a_discarded_snapshot_is_backfilled`,
/// `an_unreadable_store_fails_a_pending_stage_closed`,
/// `an_unreadable_store_fails_a_ready_stage_closed`,
/// `a_gap_with_no_embedder_fails_the_stage_with_a_reason`,
/// `m005_vector_gap_is_not_ready_and_is_backfilled`,
/// `m005_vector_gap_backfill_failure_is_not_reported_ready`.
pub async fn reconcile_semantic_vector_gap(handle: &Arc<IndexHandle>) -> bool {
    if handle.skip_vector || handle.lexical_only {
        return false;
    }
    let (chunk_count, vectors, has_store, has_embedder) = {
        let indexer = handle.indexer.read().await;
        (
            corpus_chunk_count(&indexer),
            indexer.vector_count().await,
            indexer.has_vector_store(),
            indexer.has_embedder(),
        )
    };
    let index_id = &handle.id.0;
    // #8884: gathered before the stages write lock, which never nests the indexer's.
    let refused = if vectors.is_some_and(|v| v < chunk_count) && has_embedder {
        refusals_behind_gap(handle).await
    } else {
        None
    };
    let gap = {
        let mut stages = handle.stages.write().await;
        let status = stages.semantic.status;
        // #8863: a wired store whose size cannot be read cannot be reconciled.
        // Fail closed rather than leave owed work at a `Pending` nothing starts,
        // or report `Ready` over a store whose coverage is unknown.
        let owed = matches!(status, StageStatus::Ready | StageStatus::Pending);
        if has_store && vectors.is_none() && owed && chunk_count > 0 {
            let reason = format!(
                "semantic embed was not scheduled: the vector store's size could not be read, \
                 so the {chunk_count} corpus chunks cannot be reconciled against it (#8863)"
            );
            tracing::error!("vector_gap[{index_id}]: {reason}");
            stages.semantic = StageState::failed(reason);
            return false;
        }
        let Some(gap) = semantic_vector_gap(status, chunk_count, vectors) else {
            return false;
        };
        if let Some(refused) = refused {
            // #8884: every missing vector is a recorded refusal of unchanged
            // content; a backfill would only refuse it again.
            tracing::info!(
                "vector_gap[{index_id}]: {refused} of {chunk_count} chunks have no vector because \
                 the store refused their embedding — semantic stays {status:?} (#8884)"
            );
            stages.semantic.vectors_rejected = Some(refused);
            return false;
        }
        if !has_embedder {
            // #8863: no embedder means no pass can close the gap — a terminal,
            // named state, not a `Pending` that waits forever.
            let reason = format!(
                "semantic embed was not scheduled: {gap} of {chunk_count} chunks have no vector \
                 and this daemon has no embedder wired to backfill them (#8863)"
            );
            tracing::error!("vector_gap[{index_id}]: {reason}");
            stages.semantic = StageState::failed(reason);
            return false;
        }
        stages.semantic = StageState {
            status: StageStatus::InProgress,
            total: Some(chunk_count),
            ..Default::default()
        };
        gap
    };
    tracing::warn!(
        "vector_gap[{index_id}]: {gap} of {chunk_count} chunks have no vector — semantic is \
         in progress and an embed backfill is queued (#8726, #8863)"
    );
    crate::service::reindex::spawn_deferred_embed_pass(
        Arc::clone(handle),
        Arc::new(crate::service::reindex::ReindexProgress::new()),
        gap,
    );
    true
}

/// How many refused chunks explain the whole gap, failing closed (#8884).
///
/// What: `CodeIndexer::refusals_behind_vector_gap`, with a read or parse
/// fault logged and answered `None`, so the gap is reconciled as real.
/// Test: `an_unreadable_refusal_record_is_treated_as_a_real_gap`.
async fn refusals_behind_gap(handle: &IndexHandle) -> Option<usize> {
    let outcome = handle
        .indexer
        .read()
        .await
        .refusals_behind_vector_gap()
        .await;
    outcome.unwrap_or_else(|e| {
        tracing::warn!(
            "vector_gap[{}]: the vector-refusal record could not be read ({e:#}), so the gap \
             is treated as real (#8884)",
            handle.id.0,
        );
        None
    })
}

#[cfg(test)]
#[path = "vector_gap_tests.rs"]
mod tests;
