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

use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

use crate::core::chunker::RawChunk;

use super::super::CodeIndexer;
use super::embed::{EmbedContext, EmbedRun};
use super::EmbedCatchUp;

/// How long a deferred-embed commit waits for an evicted chunk map to
/// rehydrate (#8761).
///
/// Why: the commit runs in the background, so the ~27 s interactive query
/// budget does not apply, and giving up throws the embed pass away. The
/// slowest measured scan is 40 s for 315K chunks on NFS (`idle_evict`); at
/// that rate the largest corpus `TRUSTY_MAX_CHUNKS` allows (800K) takes about
/// 100 s. 300 s is three times that. The wait stays bounded because the
/// commit holds the background permit, this index's permit and its indexer
/// read guard, so a scan that never commits must not hold them forever.
const BACKGROUND_REHYDRATE_CEILING: Duration = Duration::from_secs(300);

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
    ///
    /// #8761: the plan is a snapshot and the embed phase holds no lock, so a
    /// chunk can be removed or edited before this runs. Only chunks the live
    /// corpus still holds with the snapshot's content are committed, and a
    /// chunk removed while the upsert ran has its vector evicted afterwards.
    /// `embedded` counts the vectors kept. An evicted chunk map is waited
    /// for on [`BACKGROUND_REHYDRATE_CEILING`]; the commit errors on a read
    /// fault or at that ceiling, since neither check can be answered then.
    /// Test: `a_paused_pass_owes_work_and_a_resumed_one_embeds_only_the_gap`,
    /// `a_stalled_wave_commits_the_waves_before_it`,
    /// `a_file_removed_or_edited_during_the_embed_phase_gets_no_vector`,
    /// `a_file_removed_during_the_upsert_has_its_vector_evicted`,
    /// `a_rehydrate_slower_than_the_query_budget_is_waited_out`.
    pub(crate) async fn commit_deferred_embed(
        &self,
        plan: DeferredEmbedPlan,
        run: EmbedRun,
    ) -> Result<EmbedCatchUp> {
        let DeferredEmbedPlan {
            mut to_embed,
            total,
            embed: _,
        } = plan;
        if to_embed.is_empty() {
            return Ok(EmbedCatchUp::finished(0, total));
        }
        let EmbedRun {
            mut embeddings,
            stalled,
        } = run;
        let done = embeddings.iter().take_while(|e| e.is_some()).count();
        let paused = done < to_embed.len();
        embeddings.truncate(done);
        to_embed.truncate(done);
        // #8761: the snapshot may name chunks removed or edited while it embedded.
        let (live, embeddings) = self.retain_live_snapshot(to_embed, embeddings).await?;
        self.commit_vectors_batch(&live, &embeddings).await?;
        self.commit_embeddings_cache(&live, embeddings).await;
        let evicted = self.evict_vectors_removed_during_commit(&live).await?;
        if let Some(stall) = stalled {
            // #8600: the completed waves are committed; make them durable
            // before the pass settles `Failed`, as the paused arm does.
            if done > 0 {
                self.force_incremental_persist();
            }
            return Err(stall);
        }
        Ok(EmbedCatchUp {
            // #8761: the vectors this commit kept, not the chunks it embedded.
            embedded: live.len() - evicted,
            total,
            paused,
        })
    }

    /// Wait, on a background budget, until the chunk map is a view of the
    /// durable corpus (#8761).
    ///
    /// Why: `ensure_corpus_view_is_current` gives up after the ~27 s an
    /// interactive query can spend. A cold scan of a 315K-chunk NFS corpus
    /// took 27–40 s (see `idle_evict`), and a commit that gives up discards
    /// the whole embed pass, which the next boot then repeats.
    /// What: rejoins the detached rehydrate until the map is current. Returns
    /// the recorded read fault as soon as one is seen, since waiting will not
    /// clear it, and errors once `ceiling` elapses.
    /// Test: `a_rehydrate_slower_than_the_query_budget_is_waited_out`,
    /// `a_read_fault_during_the_commit_wait_fails_fast`,
    /// `the_commit_wait_gives_up_at_its_ceiling`.
    pub(crate) async fn wait_for_corpus_view(&self, ceiling: Duration) -> Result<()> {
        let waited = tokio::time::timeout(ceiling, async {
            loop {
                self.ensure_chunks_loaded().await;
                if let Some(fault) = self.corpus_read_fault.error(&self.index_id) {
                    return Err(anyhow::Error::new(fault));
                }
                if !self.chunks_evicted.load(Ordering::Relaxed) {
                    return Ok(());
                }
            }
        })
        .await;
        waited.unwrap_or_else(|_| {
            Err(anyhow!(
                "index '{}': the evicted chunk map did not rehydrate within {}s",
                self.index_id,
                ceiling.as_secs_f64(),
            ))
        })
    }

    /// Drop snapshot chunks the live corpus no longer holds unchanged (#8761).
    ///
    /// Why: an embedding of a removed chunk re-inserted an orphan vector that
    /// no removal path reclaims; an embedding of an edited chunk overwrote the
    /// vector of its current content with the pre-edit one.
    /// What: keeps each chunk whose id the corpus holds with identical
    /// content, with its embedding. Errors when the map cannot be read as the
    /// whole corpus within [`BACKGROUND_REHYDRATE_CEILING`].
    /// Test: `a_file_removed_or_edited_during_the_embed_phase_gets_no_vector`,
    /// `commit_refuses_when_the_chunk_map_cannot_be_rehydrated`.
    async fn retain_live_snapshot(
        &self,
        snapshot: Vec<RawChunk>,
        embeddings: Vec<Option<Vec<f32>>>,
    ) -> Result<(Vec<RawChunk>, Vec<Option<Vec<f32>>>)> {
        self.wait_for_corpus_view(BACKGROUND_REHYDRATE_CEILING)
            .await
            .context("confirm the embedded chunks are still in the corpus")?;
        let corpus = self.chunks.read().await;
        Ok(snapshot
            .into_iter()
            .zip(embeddings)
            .filter(|(chunk, _)| {
                corpus
                    .get(&chunk.id)
                    .is_some_and(|now| now.content == chunk.content)
            })
            .unzip())
    }

    /// Evict the vectors of chunks removed while they were being upserted (#8761).
    ///
    /// Why: `retain_live_snapshot` reads the corpus before the upsert, so a
    /// removal that lands between the two finds no vector to remove and the
    /// upsert then inserts an orphan. Both removal paths —
    /// `drop_chunk_ids_from_memory` (`remove_file`) and `remove_chunk` (the
    /// file watcher) — drop the map entry before the vector, so a removal this
    /// check misses removes the vector itself.
    /// What: removes the vector and cached embedding of every committed chunk
    /// the corpus no longer holds, and returns how many it removed. Every id
    /// is attempted; the failures come back as one error. Errors too when the
    /// map cannot be read as the whole corpus.
    /// Test: `a_file_removed_during_the_upsert_has_its_vector_evicted`,
    /// `a_failed_eviction_after_the_upsert_is_an_error`,
    /// `one_failed_eviction_does_not_stop_the_others`,
    /// `post_upsert_check_refuses_when_the_chunk_map_cannot_be_rehydrated`.
    async fn evict_vectors_removed_during_commit(&self, committed: &[RawChunk]) -> Result<usize> {
        let Some(store) = &self.store else {
            return Ok(0);
        };
        self.wait_for_corpus_view(BACKGROUND_REHYDRATE_CEILING)
            .await
            .context("find chunks removed during the vector upsert")?;
        let removed: Vec<&str> = {
            let corpus = self.chunks.read().await;
            committed
                .iter()
                .filter(|chunk| !corpus.contains_key(&chunk.id))
                .map(|chunk| chunk.id.as_str())
                .collect()
        };
        // #8761: one failed removal must not leave the rest as orphans.
        let mut failures = Vec::new();
        for id in &removed {
            self.chunk_embeddings.write().await.pop(*id);
            if let Err(e) = store.remove(id).await {
                failures.push(format!("evict the vector of removed chunk {id}: {e:#}"));
            }
        }
        if !failures.is_empty() {
            bail!(
                "{} of {} removed chunks kept their vectors: {}",
                failures.len(),
                removed.len(),
                failures.join("; "),
            );
        }
        Ok(removed.len())
    }
}
