//! Adopting an interrupted run's staging corpus, and stamping a run's resume
//! checkpoint (#3979).
//!
//! Why: split out of `corpus_swap.rs` to keep it under the line cap (#10738).
//! What: [`adopt_staged_corpus`] and [`write_checkpoint`], unchanged apart
//! from the #10738 live-corpus marker in the adoption.
//! Test: `super::super::resume_tests::interrupted_reindex_resumes_to_identical_index`.

use crate::core::registry::{IndexHandle, IndexId};
use std::path::PathBuf;

use super::super::checkpoint::{ReindexCheckpoint, ResumeState};

/// Adopt an existing, already-validated staging corpus for a resumed reindex
/// (issue #3979).
///
/// Why: the whole point of resume. `begin_staged_corpus_swap`'s normal path
/// calls `CorpusStore::open_fresh`, which DELETES the file at the staging path
/// before opening it — that single call is what discarded every committed batch
/// after a crash. Adoption opens the same file without deleting it and without
/// re-seeding from live, because the interrupted run already copied the live
/// rows in (#839) and then added its own committed batches on top.
///
/// What: installs the staging corpus the probe already opened and validated
/// (#4721 — this used to re-open the same redb file, which is both wasted work
/// on a multi-GB corpus and a window in which the exclusive file lock is held by
/// nobody; an `Err` from that second open silently abandoned the resume) via
/// `swap_corpus_store` — the same sink the fresh path uses, deliberately NOT
/// `set_corpus_store`, so the #4122 write quarantine is neither lifted nor
/// bypassed — and then drops the in-memory
/// chunk/BM25/entity caches. That last step is required for correctness, not
/// memory: those caches were rehydrated at warm boot from the LIVE corpus, so
/// for any file the interrupted run had already re-indexed they hold the OLD
/// chunks while the adopted corpus holds the NEW ones. Clearing them makes every
/// downstream reader (search lanes, the KG rebuild, `remove_file_no_kg_rebuild`)
/// lazily rehydrate from the adopted corpus through the existing
/// `ensure_corpus_rehydrated` guards, so the served state matches the corpus
/// that will be promoted.
///
/// Returns `Ok(Some(tmp_path))` — since #4721 the handle is already open, so
/// there is no adoption failure mode left to fall back from. #10738: except
/// a failed identity marker write on the live corpus, which is an `Err`; the
/// staging file stays on disk for a later resume.
/// Test: `super::resume_tests::interrupted_reindex_resumes_to_identical_index`,
/// `super::resume_tests::probe_hands_the_open_staging_corpus_to_the_adoption`,
/// `super::resume_tests::a_resumed_first_walk_embeds_the_chunks_it_adopted`.
pub(super) async fn adopt_staged_corpus(
    handle: &IndexHandle,
    index_id: &IndexId,
    state: ResumeState,
) -> Result<Option<PathBuf>, anyhow::Error> {
    let ResumeState {
        tmp_path,
        staged,
        staged_chunks,
        ..
    } = state;
    {
        let mut indexer = handle.indexer.write().await;
        // #10738: puts after a restart land in the live corpus this swap
        // retires; mark it so the next attach bumps, or refuse the resume.
        crate::service::index_identity::mark_live_before_swap(&indexer)?;
        let _prev = indexer.swap_corpus_store(std::sync::Arc::new(staged));
    }
    // Drop caches built from the LIVE corpus — see the doc comment above.
    // #8884: mark them evicted even when empty (a first walk killed before its
    // promotion), or no reader rehydrates the adopted rows.
    let reclaimed = handle.indexer.read().await.invalidate_corpus_caches().await;
    tracing::info!(
        "reindex[{}]: adopted staging corpus {} ({} staged chunk(s)); dropped {} \
         in-memory cache entr(ies) built from the pre-crash live corpus so reads \
         rehydrate from the adopted corpus (issue #3979)",
        index_id.0,
        tmp_path.display(),
        staged_chunks,
        reclaimed,
    );
    Ok(Some(tmp_path))
}

/// Persist `checkpoint` into the currently-installed (staging) corpus (#3979).
///
/// Why: the record has to live in the same redb file as the partial chunks so a
/// crash cannot leave the two disagreeing — redb's ACID commit means the blob is
/// either fully there or not there at all, which is strictly stronger than a
/// write-then-rename sidecar.
/// What: serializes to JSON and calls `write_reindex_checkpoint_sync` on a
/// blocking worker. A failure is logged at `warn` and ignored — the only
/// consequence is that this run's staging corpus is not adoptable, i.e. exactly
/// the pre-#3979 behaviour.
/// Test: `super::resume_tests::interrupted_reindex_resumes_to_identical_index`.
pub(super) async fn write_checkpoint(
    handle: &IndexHandle,
    index_id: &IndexId,
    checkpoint: &ReindexCheckpoint,
) {
    let corpus = {
        let indexer = handle.indexer.read().await;
        indexer.corpus_store()
    };
    let Some(corpus) = corpus else {
        return;
    };
    let bytes = match serde_json::to_vec(checkpoint) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                "reindex[{}]: could not serialize the resume checkpoint ({e}) — \
                 an interruption will fall back to a full reindex (issue #3979)",
                index_id.0
            );
            return;
        }
    };
    let res =
        tokio::task::spawn_blocking(move || corpus.write_reindex_checkpoint_sync(&bytes)).await;
    match res {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(
            "reindex[{}]: could not write the resume checkpoint ({e}) — an \
             interruption will fall back to a full reindex (issue #3979)",
            index_id.0
        ),
        Err(e) => tracing::warn!(
            "reindex[{}]: resume-checkpoint write task panicked ({e}) — an \
             interruption will fall back to a full reindex (issue #3979)",
            index_id.0
        ),
    }
}
