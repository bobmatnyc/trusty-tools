//! Durable-corpus reads for a deferred-embed pass (#12064).
//!
//! Why: the pass planned its owed ids from the in-memory chunk map, while the
//! settle measures coverage from the redb corpus. A corpus row the map lacks
//! (a write that landed in the #10738 carryover window) was owed by the
//! settle and never planned, so every retry embedded nothing and the semantic
//! stage stayed `failed` until a restart rebuilt the map.
//! What: the owed ids come from the corpus, the same source
//! [`CodeIndexer::vector_coverage`] reads. An owed id the map does not hold is
//! read from redb, one wave at a time, and the commit confirms such a chunk
//! against its redb row instead of the map. Every read failure is an error, so
//! a pass that could not load or confirm a row never settles `ready`.
//! Test: `a_corpus_row_missing_from_the_chunk_map_is_embedded_on_one_retry_12064`,
//! `a_failed_row_load_fails_the_wave_12064`,
//! `a_corpus_row_missing_from_the_chunk_map_gets_a_vector_12064`.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, Context, Result};

use crate::core::chunker::RawChunk;

use super::super::CodeIndexer;

impl CodeIndexer {
    /// The chunk ids a deferred-embed pass may owe, from the durable corpus.
    ///
    /// Why: #12064 — the settle reads the corpus, so the plan must too.
    /// What: every corpus key when a corpus is wired; the chunk map's keys
    /// otherwise, as [`CodeIndexer::vector_coverage`] does. Keys only: no row
    /// is read. A failed read is an error.
    /// Test: `a_corpus_row_missing_from_the_chunk_map_gets_a_vector_12064`.
    pub(super) async fn deferred_candidate_ids(&self) -> Result<Vec<String>> {
        let Some(corpus) = self.corpus.clone() else {
            self.ensure_chunks_loaded().await;
            return Ok(self.chunks.read().await.keys().cloned().collect());
        };
        let ids = tokio::task::spawn_blocking(move || corpus.list_chunk_ids())
            .await
            .map_err(|e| anyhow!("the corpus chunk id read did not finish ({e})"))?
            .context("read the corpus chunk ids a deferred-embed pass owes")?;
        Ok(ids.into_iter().collect())
    }

    /// Read the owed chunks the chunk map does not hold from redb (#12064).
    ///
    /// Why: such a chunk is owed by the settle and has no in-memory copy.
    /// What: one read transaction for the wave's absent ids only. A row the
    /// corpus no longer holds, or a corrupt one, is skipped; the settle then
    /// still counts it as missing. A failed read is an error.
    /// Test: `a_failed_row_load_fails_the_wave_12064`.
    pub(super) async fn load_owed_rows(&self, ids: Vec<String>) -> Result<Vec<RawChunk>> {
        let Some(corpus) = self.corpus.clone().filter(|_| !ids.is_empty()) else {
            return Ok(Vec::new());
        };
        tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            corpus.get_chunks(&refs)
        })
        .await
        .map_err(|e| anyhow!("the owed-row read did not finish ({e})"))?
        .context("load owed chunks the chunk map lacks from the corpus")
    }

    /// Of `chunks`, keep those whose redb row still holds the same content.
    ///
    /// Why: #8761 — a chunk removed or edited while it embedded must not get
    /// its snapshot vector. For a chunk read from redb (#12064) the row, not
    /// the map, says whether it is still live.
    /// What: one read of the rows; returns the 1:1 kept pairs. A failed read
    /// is an error.
    /// Test: `a_corpus_row_removed_during_the_embed_phase_gets_no_vector_12064`.
    pub(super) async fn retain_live_corpus_rows(
        &self,
        pairs: Vec<(RawChunk, Option<Vec<f32>>)>,
    ) -> Result<Vec<(RawChunk, Option<Vec<f32>>)>> {
        let ids: Vec<String> = pairs.iter().map(|(c, _)| c.id.clone()).collect();
        let rows: HashMap<String, String> = self
            .load_owed_rows(ids)
            .await?
            .into_iter()
            .map(|row| (row.id, row.content))
            .collect();
        Ok(pairs
            .into_iter()
            .filter(|(chunk, _)| rows.get(&chunk.id).is_some_and(|now| *now == chunk.content))
            .collect())
    }

    /// Of `ids`, those with no redb row: removed while their vector upserted.
    ///
    /// Why: #8761's post-upsert check, for chunks read from redb (#12064).
    /// What: one key lookup per id. A failed read is an error.
    /// Test: `a_corpus_row_missing_from_the_chunk_map_gets_a_vector_12064`.
    pub(super) async fn corpus_rows_gone(&self, ids: Vec<String>) -> Result<HashSet<String>> {
        let Some(corpus) = self.corpus.clone().filter(|_| !ids.is_empty()) else {
            return Ok(ids.into_iter().collect());
        };
        tokio::task::spawn_blocking(move || {
            let present = corpus.existing_chunk_ids(&ids)?;
            Ok(ids.into_iter().filter(|id| !present.contains(id)).collect())
        })
        .await
        .map_err(|e| anyhow!("the corpus row check did not finish ({e})"))?
    }
}
