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

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::sync::RwLockReadGuard;

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

/// Vector coverage of the corpus, read by chunk id (#8884).
///
/// Why/What: see [`CodeIndexer::vector_coverage`].
/// Test: `a_rejected_embedding_is_reported_and_does_not_fail_the_stage`,
/// `an_unreadable_store_does_not_let_a_pass_settle_ready`.
#[derive(Debug)]
pub(crate) enum VectorCoverage {
    /// No embedder or no store is wired: the index is vectorless by design.
    NotApplicable,
    /// Coverage cannot be measured; the text says which read failed.
    Unreadable(String),
    /// The corpus ids the store holds no vector for, out of `chunks`.
    Measured { chunks: usize, missing: Vec<String> },
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
    /// for on [`BACKGROUND_REHYDRATE_CEILING`], again if it is reclaimed
    /// between the wait and the read; the commit errors on a read fault or at
    /// that ceiling, since neither check can be answered then.
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
        let rejected = self.commit_vectors_batch(&live, &embeddings).await?;
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
            rejected,
        })
    }

    /// Measure vector coverage by chunk id: the corpus ids the store holds no
    /// vector for (#8884).
    ///
    /// Why: a count comparison cannot tell a chunk no pass planned from one
    /// whose embedding the store refused, and an orphan vector hides a missing
    /// one. The settle of a deferred-embed pass must tell those apart.
    /// What: `NotApplicable` with no embedder or no store. Otherwise reads the
    /// durable corpus ids (the chunk map when no corpus is wired), then the
    /// store's size and membership (`contains_many`). A failed corpus id read,
    /// or a failed store size read over a non-empty corpus, is `Unreadable`.
    /// Test: `a_never_attempted_chunk_fails_the_stage_even_when_the_counts_match`,
    /// `an_unreadable_store_does_not_let_a_pass_settle_ready`.
    pub(crate) async fn vector_coverage(&self) -> VectorCoverage {
        let Some(store) = self.store.as_ref().filter(|_| self.embedder.is_some()) else {
            return VectorCoverage::NotApplicable;
        };
        let ids: Vec<String> = match self.corpus.clone() {
            Some(corpus) => {
                match tokio::task::spawn_blocking(move || corpus.list_chunk_ids()).await {
                    Ok(Ok(ids)) => ids.into_iter().collect(),
                    Ok(Err(e)) => {
                        let why = format!("the corpus chunk ids could not be read ({e:#})");
                        return VectorCoverage::Unreadable(why);
                    }
                    Err(e) => {
                        let why = format!("the corpus chunk id read did not finish ({e})");
                        return VectorCoverage::Unreadable(why);
                    }
                }
            }
            None => self.chunks.read().await.keys().cloned().collect(),
        };
        let chunks = ids.len();
        if chunks > 0 && store.len().await.is_err() {
            return VectorCoverage::Unreadable(format!(
                "the vector store's size could not be read, so the {chunks} corpus chunks \
                 cannot be reconciled against it"
            ));
        }
        let present = store.contains_many(&ids).await;
        let missing = ids
            .into_iter()
            .zip(present)
            .filter_map(|(id, has)| (!has).then_some(id))
            .collect();
        VectorCoverage::Measured { chunks, missing }
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

    /// Read-lock the chunk map once it is a view of the whole corpus (#8761).
    ///
    /// Why: a reclaim can land after [`Self::wait_for_corpus_view`] returns and
    /// before the read lock is taken; the map then reads as an empty corpus.
    /// What: waits, takes the read guard, and re-checks `chunks_evicted` under
    /// it. `clear_in_memory_chunks` sets that flag before it releases its
    /// write guard, so a clear flag here means this guard sees a whole map. A
    /// set flag drops the guard and waits again; `ceiling` bounds the loop.
    /// Test: `a_reclaim_between_the_wait_and_the_pre_upsert_read_is_waited_out`,
    /// `a_reclaim_between_the_wait_and_the_post_upsert_read_is_waited_out`.
    async fn read_corpus_view(
        &self,
        ceiling: Duration,
    ) -> Result<RwLockReadGuard<'_, HashMap<String, RawChunk>>> {
        let deadline = tokio::time::Instant::now() + ceiling;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            self.wait_for_corpus_view(remaining).await?;
            let corpus = self.chunks.read().await;
            // #8761: a reclaim that landed since the wait set the flag first.
            if !self.chunks_evicted.load(Ordering::Relaxed) {
                return Ok(corpus);
            }
        }
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
        let corpus = self
            .read_corpus_view(BACKGROUND_REHYDRATE_CEILING)
            .await
            .context("confirm the embedded chunks are still in the corpus")?;
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
    /// `drop_chunk_ids_from_memory` (`remove_file`) and `remove_chunk_ids_committed`
    /// (the file watcher) — drop the map entry before the vector, so a removal this
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
        let removed: Vec<&str> = {
            let corpus = self
                .read_corpus_view(BACKGROUND_REHYDRATE_CEILING)
                .await
                .context("find chunks removed during the vector upsert")?;
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
