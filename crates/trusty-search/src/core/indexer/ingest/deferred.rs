//! The three phases of a deferred-embed catch-up pass (#8600).
//!
//! Why: the pass used to run under one indexer read guard from snapshot to
//! commit. Embedding can take minutes to hours, and tokio's `RwLock` is fair: a
//! writer queued behind that guard blocked every later reader, so
//! `GET /indexes/{id}/status` hung for the rest of the pass.
//! What: [`CodeIndexer::plan_deferred_embed`] snapshots the owed chunks and the
//! embed dependencies under the guard; [`DeferredEmbedPlan::embed`] runs with no
//! guard; [`CodeIndexer::commit_deferred_embed`] commits the embedded prefix
//! under a fresh guard. The caller drops the guard between the phases.
//! Test: `status_answers_during_an_embed_pass_with_a_writer_queued`, plus the
//! #6524 pause tests through `embed_deferred_chunks_gated`.

use anyhow::Result;

use crate::core::chunker::RawChunk;

use super::super::CodeIndexer;
use super::embed::{EmbedContext, EmbedRun};
use super::EmbedCatchUp;

/// The chunks one catch-up pass owes, snapshotted under the indexer lock.
///
/// Why/What: see the module docs. `to_embed` is empty when the corpus is
/// empty, the index has no embedder or store, or every chunk already has a
/// vector.
/// Test: `status_answers_during_an_embed_pass_with_a_writer_queued`.
pub(crate) struct DeferredEmbedPlan {
    to_embed: Vec<RawChunk>,
    total: usize,
    embed: EmbedContext,
}

impl DeferredEmbedPlan {
    /// Embed the owed chunks. Holds no indexer lock (#8600).
    ///
    /// What: the embeddings are 1:1 with the owed chunks; a pause, drain or
    /// no-progress abort leaves the tail `None` (#6524). #8600: an abort is
    /// carried in [`EmbedRun::stalled`], not an `Err`, so the completed waves
    /// before it still reach [`CodeIndexer::commit_deferred_embed`].
    /// Test: `a_drain_abandons_an_in_flight_embed_wave_and_releases_the_corpus`,
    /// `a_stalled_wave_commits_the_waves_before_it`.
    pub(crate) async fn embed(
        &self,
        progress_tx: Option<&tokio::sync::mpsc::UnboundedSender<(usize, u64)>>,
        pause: Option<&crate::core::embed_pause::EmbeddingPause>,
    ) -> Result<EmbedRun> {
        if self.to_embed.is_empty() {
            return Ok(EmbedRun {
                embeddings: Vec::new(),
                stalled: None,
            });
        }
        self.embed
            .embed_chunks_keeping_prefix(&self.to_embed, progress_tx, pause)
            .await
    }
}

impl CodeIndexer {
    /// Snapshot the chunks the vector store does not have yet (#8600).
    ///
    /// Why: issue #2984 Phase 1 HIGH finding 3 — catch-up is incremental, so
    /// chunks that already carry a vector are skipped (`contains_many`).
    /// What: the read-only first phase; see [`DeferredEmbedPlan`].
    /// Test: `a_paused_pass_owes_work_and_a_resumed_one_embeds_only_the_gap`.
    pub(crate) async fn plan_deferred_embed(&self) -> DeferredEmbedPlan {
        let chunks: Vec<RawChunk> = {
            self.ensure_chunks_loaded().await;
            let map = self.chunks.read().await;
            map.values().cloned().collect()
        };
        let total = chunks.len();
        let embed = self.embed_context().await;
        let to_embed = match (&self.store, self.embedder.is_some()) {
            (Some(store), true) if total > 0 => {
                let ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();
                let already_embedded = store.contains_many(&ids).await;
                chunks
                    .into_iter()
                    .zip(already_embedded)
                    .filter_map(|(chunk, embedded)| (!embedded).then_some(chunk))
                    .collect()
            }
            _ => Vec::new(),
        };
        DeferredEmbedPlan {
            to_embed,
            total,
            embed,
        }
    }

    /// Commit the embedded prefix of `plan` (#6524, #8600).
    ///
    /// Why: a pause or drain leaves the tail `None`. `commit_vectors_batch`
    /// reads a `None` slot as a stale-embedding eviction, so committing the
    /// un-embedded remainder would undo work instead of deferring it.
    /// What: commits only the leading `Some` run; `paused` is true when that
    /// run is shorter than the plan. A stalled run (#8600) commits and
    /// snapshots its prefix too, then returns the stall as the `Err` that
    /// settles the pass `Failed`. A DURABLE WRITE — the caller must hold the
    /// per-index teardown read-guard (#3049).
    /// Test: `a_paused_pass_owes_work_and_a_resumed_one_embeds_only_the_gap`,
    /// `a_stalled_wave_commits_the_waves_before_it`.
    pub(crate) async fn commit_deferred_embed(
        &self,
        plan: DeferredEmbedPlan,
        run: EmbedRun,
    ) -> Result<EmbedCatchUp> {
        if plan.to_embed.is_empty() {
            return Ok(EmbedCatchUp::finished(0, plan.total));
        }
        let EmbedRun {
            mut embeddings,
            stalled,
        } = run;
        let done = embeddings.iter().take_while(|e| e.is_some()).count();
        let paused = done < plan.to_embed.len();
        embeddings.truncate(done);
        self.commit_vectors_batch(&plan.to_embed[..done], &embeddings)
            .await?;
        self.commit_embeddings_cache(&plan.to_embed[..done], embeddings)
            .await;
        if let Some(stall) = stalled {
            // #8600: the completed waves are committed; make them durable
            // before the pass settles `Failed`, as the paused arm does.
            if done > 0 {
                self.force_incremental_persist();
            }
            return Err(stall);
        }
        Ok(EmbedCatchUp {
            embedded: done,
            total: plan.total,
            paused,
        })
    }
}
