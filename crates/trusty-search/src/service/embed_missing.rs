//! Resume a failed or partial semantic stage by embedding only the chunks that
//! have no vector (#10551).
//!
//! Why: an embedder fault inside the semantic stage (`embedder stdio IPC error:
//! child closed stdout`) settles `semantic` as `Failed` and leaves part of the
//! corpus without vectors. The index then reports `degraded`, and nothing
//! re-ran the stage: `PATCH config {"vector": true}` (#8148) re-arms a
//! `Pending` or `Failed` lane only on a boot-time path, the vector-gap
//! reconcile (#8726, #8863) looks at `Ready` and `Pending` stages only, and a
//! full reindex re-walks and re-chunks everything to recover 2% of the vectors.
//! What: [`resume_semantic`] measures the gap by chunk id
//! (`CodeIndexer::vector_coverage`), probes the embedder, flips the stage to
//! `InProgress` and queues the existing deferred-embed pass through
//! `reindex::spawn_deferred_embed_pass`. That pass embeds only the chunks the
//! store lacks, so no new embed loop exists here. A gap of zero is a no-op, or
//! settles a `Failed` stage `Ready` once the #4707 honesty gate agrees.
//! Chunks whose embedding the store durably refused (#8884) are not a gap.
//! `service::embed_retry` calls the same function on a backoff.
//! Test: `service::embed_missing::tests` (this module's `embed_missing_tests.rs`).

use std::sync::Arc;
use std::time::Duration;

use crate::core::indexer::VectorCoverage;
use crate::core::registry::{IndexHandle, StageStatus};

/// How long the pre-flight embedder probe may take before it counts as down.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// What a successful [`resume_semantic`] call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeAction {
    /// The missing chunks were queued on the deferred-embed pass.
    Queued,
    /// `dry_run`: chunks are missing and nothing was queued.
    WouldQueue,
    /// A pass is already running; nothing was added.
    AlreadyRunning,
    /// Nothing is missing and the stage needed no change.
    Noop,
    /// Nothing is missing; a `Failed` stage was settled `Ready`.
    Settled,
}

impl ResumeAction {
    /// The wire label of this action.
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::WouldQueue => "would_queue",
            Self::AlreadyRunning => "already_running",
            Self::Noop => "noop",
            Self::Settled => "settled",
        }
    }
}

/// The counts behind one [`resume_semantic`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeReport {
    pub action: ResumeAction,
    /// Corpus chunks, when measured (`None` for `AlreadyRunning`).
    pub chunks: Option<usize>,
    /// Chunks with no vector that a pass owes, when measured.
    pub missing: Option<usize>,
    /// Chunks the store durably refused, which are not owed (#8884).
    pub rejected: usize,
    /// The semantic stage after the call.
    pub semantic: StageStatus,
}

/// Why [`resume_semantic`] did nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeRefusal {
    /// The index has no vector lane to resume; permanent for this handle.
    NotApplicable(String),
    /// The embedder did not answer a probe; retry once it recovers.
    EmbedderDown(String),
    /// The vector store or corpus could not be read; retryable.
    Unreadable(String),
    /// A reindex or another pass holds the index; retryable.
    Busy(String),
}

/// Embed only the chunks that have no vector, and settle the stage (#10551).
///
/// Why: see the module doc. Idempotent by construction: the second call over a
/// covered index measures zero missing and changes nothing.
/// What: refuses a `skip_vector`, `lexical_only`, serve-only or embedder-less
/// handle. Returns `AlreadyRunning` for an `InProgress` stage. Otherwise
/// measures coverage by id. Zero owed chunks is `Noop`, or `Settled` for a
/// `Failed` stage over a non-empty, healthy store. For owed chunks, `dry_run`
/// stops at `WouldQueue`; a live call probes the embedder (a down embedder
/// leaves the stage untouched), sets the stage `InProgress`, persists the
/// pending marker so a restart re-arms the pass, and queues it.
/// Test: `only_the_missing_chunks_are_embedded_and_the_stage_leaves_failed`,
/// `a_repeat_run_with_nothing_missing_is_a_noop`,
/// `a_dead_embedder_is_refused_before_the_stage_is_touched`,
/// `a_failed_stage_with_every_vector_present_is_settled_ready`,
/// `a_dry_run_reports_the_gap_and_queues_nothing`,
/// `a_disabled_vector_lane_is_not_applicable`,
/// `a_skipped_stage_set_during_the_window_is_not_overwritten`,
/// `a_second_call_while_in_progress_reports_already_running`,
/// `a_held_index_is_refused_as_busy`, `a_skipped_stage_is_not_applicable`,
/// `a_refused_chunk_is_not_a_gap_to_an_embed_missing_resume`.
pub async fn resume_semantic(
    handle: &Arc<IndexHandle>,
    dry_run: bool,
) -> Result<ResumeReport, ResumeRefusal> {
    if handle.skip_vector || handle.lexical_only || handle.serve_only {
        return Err(ResumeRefusal::NotApplicable(
            "the vector lane is off for this index (skip_vector, lexical_only or serve_only)"
                .into(),
        ));
    }
    let semantic = handle.stages.read().await.semantic.status;
    match semantic {
        StageStatus::InProgress => {
            return Ok(ResumeReport {
                action: ResumeAction::AlreadyRunning,
                chunks: None,
                missing: None,
                rejected: 0,
                semantic,
            });
        }
        // A `PATCH {vector:false}` parks the lane; resuming would undo it.
        StageStatus::Skipped => {
            return Err(ResumeRefusal::NotApplicable(
                "the semantic stage is skipped for this index".into(),
            ));
        }
        _ => {}
    }
    let embedder = handle.indexer.read().await.embedder_arc();
    let Some(embedder) = embedder else {
        return Err(ResumeRefusal::NotApplicable(
            "no embedder is wired on this daemon".into(),
        ));
    };
    if crate::service::reindex::index_task_in_flight(&handle.id) {
        return Err(ResumeRefusal::Busy(
            "a reindex or embed pass holds this index".into(),
        ));
    }
    // Probe before any scan: a dead embedder must cost one embed call, not two
    // passes over every corpus id, on every retry.
    if !dry_run {
        if let Err(detail) = probe(&embedder).await {
            tracing::warn!(
                "embed_missing[{}]: embedder probe failed: {detail}",
                handle.id.0
            );
            return Err(ResumeRefusal::EmbedderDown(
                "the embedder did not answer a probe embed".into(),
            ));
        }
    }
    let coverage = handle.indexer.read().await.vector_coverage().await;
    let (chunks, missing_ids) = match coverage {
        VectorCoverage::NotApplicable => {
            return Err(ResumeRefusal::NotApplicable(
                "no vector store is wired on this index".into(),
            ));
        }
        VectorCoverage::Unreadable(why) => {
            tracing::warn!("embed_missing[{}]: coverage unreadable: {why}", handle.id.0);
            return Err(ResumeRefusal::Unreadable(
                "the index vector state could not be read".into(),
            ));
        }
        VectorCoverage::Measured { chunks, missing } => (chunks, missing),
    };
    let missing = missing_ids.len();
    // #8884: chunks the store durably refused recur on every pass; not owed.
    // Reuses the measurement above instead of scanning the corpus again.
    let rejected = if missing > 0 {
        crate::service::vector_gap::refusals_excusing_gap(handle, missing_ids)
            .await
            .unwrap_or(0)
    } else {
        0
    };
    let owed = if rejected == missing { 0 } else { missing };
    let report = |action, semantic| ResumeReport {
        action,
        chunks: Some(chunks),
        missing: Some(owed),
        rejected,
        semantic,
    };
    if owed == 0 {
        return Ok(settle_when_covered(handle, dry_run, chunks, rejected, semantic).await);
    }
    if dry_run {
        return Ok(report(ResumeAction::WouldQueue, semantic));
    }
    {
        let mut stages = handle.stages.write().await;
        // The stage may have changed during the probe and the scans: proceed
        // only from the state this call read, and only while no reindex runs.
        let now = stages.semantic.status;
        if now == StageStatus::InProgress {
            return Ok(report(ResumeAction::AlreadyRunning, now));
        }
        if now != semantic {
            return Err(ResumeRefusal::Busy(
                "the semantic stage changed while the gap was measured".into(),
            ));
        }
        if crate::service::reindex::index_task_in_flight(&handle.id) {
            return Err(ResumeRefusal::Busy(
                "a reindex or embed pass holds this index".into(),
            ));
        }
        stages.semantic = crate::core::registry::StageState {
            status: StageStatus::InProgress,
            total: Some(chunks),
            ..Default::default()
        };
    }
    tracing::warn!(
        "embed_missing[{}]: {owed} of {chunks} chunks have no vector — embedding only those \
         (#10551)",
        handle.id.0,
    );
    // The pass clears the marker when it settles; a restart before then re-arms it.
    crate::service::boot_markers::persist_deferred_embed_pending(&handle.id.0, true);
    crate::service::reindex::spawn_deferred_embed_pass(
        Arc::clone(handle),
        Arc::new(crate::service::reindex::ReindexProgress::new()),
        owed,
    );
    Ok(report(ResumeAction::Queued, StageStatus::InProgress))
}

/// The zero-owed arm of [`resume_semantic`]: settle a `Failed` stage or no-op.
async fn settle_when_covered(
    handle: &Arc<IndexHandle>,
    dry_run: bool,
    chunks: usize,
    rejected: usize,
    semantic: StageStatus,
) -> ResumeReport {
    let mut report = ResumeReport {
        action: ResumeAction::Noop,
        chunks: Some(chunks),
        missing: Some(0),
        rejected,
        semantic,
    };
    if dry_run || semantic != StageStatus::Failed || chunks == 0 {
        return report;
    }
    // #4707: the same gate every `Ready` publication passes. Gathered before
    // the stages write lock, which never nests the indexer's.
    if crate::service::reindex::semantic_health_reason(handle)
        .await
        .is_some()
    {
        return report;
    }
    let mut stages = handle.stages.write().await;
    if stages.semantic.status != StageStatus::Failed {
        report.semantic = stages.semantic.status;
        return report;
    }
    stages.semantic = crate::core::registry::StageState {
        status: StageStatus::Ready,
        completed_at: Some(chrono::Utc::now().to_rfc3339()),
        embedded: Some(chunks),
        total: Some(chunks),
        vectors_rejected: (rejected > 0).then_some(rejected),
        ..Default::default()
    };
    drop(stages);
    crate::service::boot_markers::persist_deferred_embed_pending(&handle.id.0, false);
    tracing::info!(
        "embed_missing[{}]: every one of {chunks} chunks has a vector — semantic is ready \
         (#10551)",
        handle.id.0,
    );
    report.action = ResumeAction::Settled;
    report.semantic = StageStatus::Ready;
    report
}

/// One embed call, bounded: does the embedder answer right now?
///
/// Why: the pass would otherwise flip the stage `InProgress`, fail on its first
/// wave and flip it back, once per attempt. A probe keeps a dead embedder from
/// touching the stage.
/// What: embeds one short text and checks the answer has the embedder's width.
/// Test: `a_dead_embedder_is_refused_before_the_stage_is_touched`.
async fn probe(embedder: &Arc<dyn crate::core::embed::Embedder>) -> Result<(), String> {
    let call = embedder.embed_batch(&["trusty-search embedder probe"]);
    match tokio::time::timeout(PROBE_TIMEOUT, call).await {
        Err(_) => Err(format!(
            "the embedder did not answer within {}s",
            PROBE_TIMEOUT.as_secs()
        )),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Ok(Ok(v)) if v.len() == 1 && v[0].len() == embedder.dimension() => Ok(()),
        Ok(Ok(v)) => Err(format!(
            "the embedder answered {} vectors for one text",
            v.len()
        )),
    }
}

#[cfg(test)]
#[path = "embed_missing_tests.rs"]
mod tests;
