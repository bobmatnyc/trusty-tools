//! Vector-lane health, computed once and reported by two endpoints (#6699).
//!
//! Why: `GET /indexes/{id}/status` learned to flag a zero-vector index in
//! #6689, but `GET /indexes?details=true` — the list the console's Indexes view
//! reads — carried neither `stages` nor `semantic_coverage`, so a fleet of 41
//! indexes showed every one of them green. A zero-vector index reports
//! `stages.semantic: ready`, so it does not surface via `/health`'s
//! `indexes_stage_failed_ids` either. Recomputing the same verdict a second way
//! in the list arm is how the two views drift apart, so both arms read this
//! module and nothing else.
//! What: [`semantic_coverage`] is the cumulative-coverage block #4787 built,
//! lifted verbatim out of `status.rs`; [`vector_lane_health`] bundles it with
//! the three inputs a consumer needs to tell a genuine fault from a BM25-only
//! index (`stages`, `search_capabilities`, `lexical_only` / `skip_vector`).
//! Test: `list_and_status_agree_on_vector_lane_health`,
//! `list_indexes_details_reports_zero_vectors`,
//! `list_indexes_details_reports_embed_lag` (`list_vector_health_tests.rs`).

use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexStages};

/// Cumulative semantic coverage for one index (#4787), as both endpoints report it.
///
/// Why: `stages.semantic.embedded` counts what THIS boot embedded, so a
/// fully-working index whose HNSW snapshot was already current reports `0` —
/// the exact signature of a dead vector lane. `vectors_present` is the ground
/// truth read from the store that actually answers queries, and `chunk_count`
/// is the denominator that says whether the embedding lane is behind the
/// corpus.
/// What: the four fields `GET /indexes/{id}/status` has carried under
/// `semantic_coverage` since #4787. Every field serialises unconditionally
/// (`null` rather than absent) because the #4787 contract distinguishes "not
/// applicable" from "unreadable" by the value, not by the key's presence.
/// Test: `status_semantic_coverage_reports_live_vector_count`,
/// `list_and_status_agree_on_vector_lane_health`.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SemanticCoverage {
    /// Vectors the LIVE store holds, or `null` when there is none to count.
    pub vectors_present: Option<usize>,
    /// Which of the two nulls this is: `count_unreadable` (a store is attached
    /// but `len()` errored — a real fault) or `no_vector_store` (BM25-only or
    /// `skip_vector`, the correct healthy answer). `null` whenever
    /// `vectors_present` is a number.
    pub vectors_unavailable_reason: Option<&'static str>,
    /// #6822: the precision the LIVE index holds, read from the index itself
    /// rather than from `TRUSTY_VECTOR_QUANT`, which says what the NEXT index
    /// will be built with. `null` for a BM25-only / `skip_vector` index.
    pub vector_quant: Option<&'static str>,
    /// Durable corpus size — the denominator `vectors_present` is measured
    /// against. `null` when the corpus failed to open (#4333), because the
    /// in-memory fallback would understate a quarantined index catastrophically.
    pub chunk_count: Option<usize>,
    /// The same number `stages.semantic.embedded` carries, under a name that
    /// says what it measures. That field keeps its name for wire compatibility.
    pub embedded_this_boot: Option<usize>,
}

/// Everything both the per-index status body and one index-list row report
/// about the vector lane (#6699).
///
/// Why: flagging a zero-vector index needs more than the count. An index that
/// never had a vector lane (`lexical_only`, `skip_vector`, or a `skipped`
/// semantic stage) holds zero vectors correctly, and one that does not
/// advertise `vector` in `search_capabilities` is not yet claiming to answer
/// vector queries. Shipping the count without those three would turn every
/// BM25-only index in the roster red.
/// What: flattened into the list entry, so a row carries the same key names at
/// the same depth as the per-index status body and a consumer can run one
/// predicate over either. `chunk_count` is restated at the top level because
/// that is where the status body has always carried it.
/// Test: `list_and_status_agree_on_vector_lane_health`.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct VectorLaneHealth {
    pub chunk_count: Option<usize>,
    pub stages: IndexStages,
    pub search_capabilities: Vec<&'static str>,
    pub semantic_coverage: SemanticCoverage,
    pub lexical_only: bool,
    pub skip_vector: bool,
}

/// Snapshot the handle's stages with the pause flag projected in (#6524).
///
/// Why: the pause lives on the handle, not inside `stages`, so one owner keeps
/// the two in step. Both endpoints must project it the same way or a paused
/// index reads differently depending on which one you asked.
/// What: clones the stage snapshot under a read lock, then stamps
/// `semantic.paused`. Only `semantic` is pausable, so the other two stay false.
/// Test: `status_reports_the_semantic_stage_as_paused`.
pub(crate) async fn stages_snapshot(handle: &IndexHandle) -> IndexStages {
    let mut stages = handle.stages.read().await.clone();
    stages.semantic.paused = handle.embedding_pause.is_paused();
    stages
}

/// Durable chunk count, or `None` when the corpus failed to open (#4333).
///
/// Why: the in-memory chunk map returns 0 after idle eviction, so the durable
/// count is preferred. But when the corpus failed to open, the quarantine
/// invariant guarantees `corpus_arc()` is `None` and the in-memory fallback is
/// NOT a measurement of the on-disk corpus — it once reported 122 for an index
/// holding 201,206 chunks, reading as catastrophic data loss. `None` (unknown)
/// is the honest answer there.
/// What: one redb read transaction (`table.len()`, an O(1) metadata read on an
/// already-open database), falling back to the in-memory map for BM25-only and
/// test indexers that have no corpus wired.
/// Test: `list_and_status_agree_on_vector_lane_health`.
pub(crate) fn corpus_chunk_count(indexer: &CodeIndexer) -> Option<usize> {
    if indexer.corpus_open_failed {
        return None;
    }
    Some(
        indexer
            .corpus_arc()
            .and_then(|c| c.chunk_count().ok())
            .unwrap_or_else(|| indexer.chunk_count()),
    )
}

/// Read cumulative semantic coverage off the live store (#4787).
///
/// Why: this is the ONE implementation of the zero-vector verdict's inputs.
/// #6689 built the verdict on top of `vectors_present` vs `chunk_count`; the
/// list arm must feed that verdict the identical numbers or the roster and the
/// expanded panel disagree about the same index.
/// What: `Index::size()` on the wired store, plus the #4787 review's
/// two-way split of the null. Costs one O(1) store read and one O(1) redb
/// metadata read — no corpus scan.
/// Test: `status_semantic_coverage_distinguishes_absent_from_unreadable`,
/// `list_indexes_details_reports_zero_vectors`.
pub(crate) async fn semantic_coverage(
    indexer: &CodeIndexer,
    embedded_this_boot: Option<usize>,
) -> SemanticCoverage {
    let vectors_present = indexer.vector_count().await;
    let vectors_unavailable_reason = match (vectors_present, indexer.has_vector_store()) {
        (Some(_), _) => None,
        // A store is attached but `len()` errored — a real fault, not an
        // absence, and distinct from `skip_vector`.
        (None, true) => Some("count_unreadable"),
        // BM25-only, `skip_vector`, or a test indexer: there is nothing to
        // count and that is the correct, healthy answer.
        (None, false) => Some("no_vector_store"),
    };
    SemanticCoverage {
        vectors_present,
        vectors_unavailable_reason,
        vector_quant: indexer.vector_quant_label().await,
        chunk_count: corpus_chunk_count(indexer),
        embedded_this_boot,
    }
}

/// Bundle one index's vector-lane health for the list arm (#6699).
///
/// Why: the list arm has no `indexer` guard of its own, and taking the stage
/// lock and the indexer lock in two places would invite the two endpoints to
/// order them differently. One function, one order.
/// What: stage snapshot first (its own lock), then one `indexer` read guard
/// held across the two O(1) reads. Adds no directory walk — the details arm's
/// existing `index_disk_and_mtime` call already walks each index directory and
/// dominates this cost by orders of magnitude.
/// Test: `list_and_status_agree_on_vector_lane_health`.
pub(crate) async fn vector_lane_health(handle: &IndexHandle) -> VectorLaneHealth {
    let stages = stages_snapshot(handle).await;
    let search_capabilities = stages.search_capabilities();
    let indexer = handle.indexer.read().await;
    let semantic_coverage = semantic_coverage(&indexer, stages.semantic.embedded).await;
    VectorLaneHealth {
        chunk_count: semantic_coverage.chunk_count,
        stages,
        search_capabilities,
        semantic_coverage,
        lexical_only: handle.lexical_only,
        skip_vector: handle.skip_vector,
    }
}
