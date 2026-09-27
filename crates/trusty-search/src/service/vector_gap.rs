//! Reconcile the semantic stage against the live vector count (#8726).
//!
//! Why: warm boot derives `semantic` from the HNSW snapshot's existence alone,
//! and nothing re-checks it against the store afterwards. Any pass that leaves
//! chunks without a vector therefore left the stage `ready` with
//! `vectors_present < chunk_count` and no backfill queued. M005's re-chunk
//! (#6581) is one such pass: it hands a stored vector only to text the corpus
//! already held and leaves the rest to an embed catch-up that nothing queued.
//! What: [`semantic_vector_gap`] is the pure rule; [`reconcile_semantic_vector_gap`]
//! applies it to a live handle and queues the catch-up through the serialized
//! deferred-embed queue. Called after a restore registers a handle and after
//! the boot migration chain succeeds.
//! Test: `tests` below, and through the boot migration path
//! `m005_vector_gap_is_not_ready_and_is_backfilled` in
//! `core::migration::m005::tests`.

use std::sync::Arc;

use crate::core::registry::{IndexHandle, StageState, StageStatus};

/// How many chunks a `Ready` semantic stage is short of a vector (#8726).
///
/// Why: `Ready` publishes the vector lane to search as complete. Over a store
/// that holds fewer vectors than the corpus holds chunks, the missing chunks
/// are unreachable by every semantic query while the index reports healthy.
/// What: `Some(chunk_count - vectors)` only when `status` is `Ready`, a store is
/// wired (`vector_count` is `Some`) and it holds fewer vectors than
/// `chunk_count`. `None` otherwise: a stage that is not `Ready` already says it
/// owes work, and an unwired store is the BM25-only steady state.
/// Test: `gap_is_reported_only_for_a_ready_stage_short_of_vectors`.
pub fn semantic_vector_gap(
    status: StageStatus,
    chunk_count: usize,
    vector_count: Option<usize>,
) -> Option<usize> {
    if status != StageStatus::Ready {
        return None;
    }
    let vectors = vector_count?;
    (vectors < chunk_count).then(|| chunk_count - vectors)
}

/// Demote a `Ready` semantic stage that is short of vectors, and queue the
/// backfill that closes the gap (#8726).
///
/// Why: see the module doc. Reporting the gap without queueing the work would
/// leave the index degraded until an unrelated reindex; queueing it without
/// demoting the stage would keep search routing through a lane that cannot
/// reach the missing chunks.
/// What: a no-op for a `skip_vector` or `lexical_only` handle. Otherwise reads
/// the durable chunk count, the live vector count and whether an embedder is
/// wired; when [`semantic_vector_gap`] reports a gap, sets `semantic` to
/// `InProgress` with `total` = the chunk count, then enqueues the C2 pass through
/// `reindex::spawn_deferred_embed_pass` — the queue that takes the background
/// permit and then the per-index permit, so it never overlaps a reindex or a
/// migration. That pass embeds only the chunks the store lacks and settles the
/// stage `Ready` or `Failed`. With no embedder wired the stage is set `Pending`
/// and nothing is queued; the next boot re-derives the gap. Returns `true` only
/// when a pass was queued.
/// Test: `a_gap_demotes_the_stage_and_queues_a_backfill`,
/// `no_gap_leaves_a_ready_stage_alone`, `m005_vector_gap_is_not_ready_and_is_backfilled`,
/// `m005_vector_gap_backfill_failure_is_not_reported_ready`.
pub async fn reconcile_semantic_vector_gap(handle: &Arc<IndexHandle>) -> bool {
    if handle.skip_vector || handle.lexical_only {
        return false;
    }
    let (chunk_count, vectors, has_embedder) = {
        let indexer = handle.indexer.read().await;
        let chunks = indexer
            .corpus_arc()
            .and_then(|c| c.chunk_count().ok())
            .unwrap_or_else(|| indexer.chunk_count());
        (chunks, indexer.vector_count().await, indexer.has_embedder())
    };
    let gap = {
        let mut stages = handle.stages.write().await;
        let Some(gap) = semantic_vector_gap(stages.semantic.status, chunk_count, vectors) else {
            return false;
        };
        // #8148's claim shape: `InProgress` once a pass is queued, `Pending`
        // when the work is owed but nothing can run it.
        stages.semantic = StageState {
            status: if has_embedder {
                StageStatus::InProgress
            } else {
                StageStatus::Pending
            },
            total: Some(chunk_count),
            ..Default::default()
        };
        gap
    };
    let index_id = &handle.id.0;
    if !has_embedder {
        tracing::warn!(
            "vector_gap[{index_id}]: {gap} of {chunk_count} chunks have no vector — semantic \
             is pending, but this daemon has no embedder wired to backfill them (#8726)"
        );
        return false;
    }
    tracing::warn!(
        "vector_gap[{index_id}]: {gap} of {chunk_count} chunks have no vector — semantic is \
         in progress and an embed backfill is queued (#8726)"
    );
    crate::service::reindex::spawn_deferred_embed_pass(
        Arc::clone(handle),
        Arc::new(crate::service::reindex::ReindexProgress::new()),
        gap,
    );
    true
}

#[cfg(test)]
#[path = "vector_gap_tests.rs"]
mod tests;
