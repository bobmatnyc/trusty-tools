//! Commit-phase helpers for the ingestion pipeline.
//!
//! Why: the phase 3+4 commit work (BM25, HNSW, embeddings cache, corpus map,
//! redb persistence, symbol graph rebuild) is orthogonal to the parse/embed
//! phase and benefits from a dedicated module so reviewers can focus on either
//! phase in isolation.
//! What: `commit_parsed_batch`, `commit_vectors_batch`, `commit_bm25_batch`,
//! `commit_embeddings_cache`, `commit_corpus`, `commit_corpus_to_redb`,
//! `commit_entities`, `chunk_count`, `symbol_graph`, `corpus_arc`, and
//! convenience accessors.
//! Test: covered indirectly by `test_index_files_batch_*` and
//! `test_corpus_store_roundtrip` in `indexer::tests`.

use std::sync::Arc;

use anyhow::{Context, Result};

use crate::core::chunker::RawChunk;
use crate::core::entity::RawEntity;
use crate::core::symbol_graph::SymbolGraph;

use super::super::{max_chunks_per_index, CodeIndexer, CommitTimings, ParsedBatch};

impl CodeIndexer {
    /// Phase 3+4 of the bulk pipeline: commit a [`ParsedBatch`] into the index.
    ///
    /// Why: this is the **only** phase that mutates shared state (BM25 index,
    /// corpus map, chunk_embeddings cache, HNSW store, entities map). By
    /// isolating it from the parse+embed work, the write-lock window shrinks
    /// from "minutes per batch" to "milliseconds per batch", letting concurrent
    /// searches and the next batch's parse+embed phase overlap freely.
    /// What: single-pass BM25 upsert, single-call HNSW `upsert_batch`, one
    /// corpus write lock for the whole batch, one entities write lock, then
    /// the (optional) graph rebuild.
    /// Test: covered indirectly by `test_index_files_batch_*`.
    pub async fn commit_parsed_batch(
        &self,
        parsed: ParsedBatch,
        defer_graph_rebuild: bool,
    ) -> Result<CommitTimings> {
        // Rehydrate an idle-evicted map before the cap check / insert below.
        self.ensure_chunks_loaded().await;
        self.touch_activity();
        let ParsedBatch {
            chunks: mut all_chunks,
            mut embeddings,
            entities_by_file,
            parse_ms: _,
            embed_ms: _,
            vector_count: _,
        } = parsed;

        // Issue #82 (180GB RSS fix): enforce the per-index chunk cap BEFORE
        // ingesting anything into BM25, HNSW, or the embedding cache. Without
        // this pre-filter, dropped chunks leaked into every downstream structure.
        let cap = max_chunks_per_index();
        let pre_filter_dropped = {
            let corpus = self.chunks.read().await;
            let mut keep_mask: Vec<bool> = Vec::with_capacity(all_chunks.len());
            let mut new_count = corpus.len();
            let mut dropped = 0usize;
            for chunk in &all_chunks {
                let is_update = corpus.contains_key(&chunk.id);
                if is_update {
                    keep_mask.push(true);
                } else if new_count < cap {
                    new_count += 1;
                    keep_mask.push(true);
                } else {
                    dropped += 1;
                    keep_mask.push(false);
                }
            }
            drop(corpus);
            if dropped > 0 {
                let mut kept_chunks: Vec<RawChunk> = Vec::with_capacity(all_chunks.len() - dropped);
                let mut kept_embeddings: Vec<Option<Vec<f32>>> =
                    Vec::with_capacity(all_chunks.len() - dropped);
                for ((chunk, vec_opt), keep) in all_chunks
                    .drain(..)
                    .zip(embeddings.drain(..))
                    .zip(keep_mask)
                {
                    if keep {
                        kept_chunks.push(chunk);
                        kept_embeddings.push(vec_opt);
                    }
                }
                all_chunks = kept_chunks;
                embeddings = kept_embeddings;
            }
            dropped
        };
        if pre_filter_dropped > 0 {
            tracing::warn!(
                "index '{}' chunk cap ({}) reached — pre-filtered {} chunks before commit \
                 (prevents leak into BM25/HNSW/embedding cache)",
                self.index_id,
                cap,
                pre_filter_dropped
            );
        }

        let chunk_total = all_chunks.len();
        if chunk_total == 0 {
            self.commit_entities(entities_by_file).await;
            return Ok(CommitTimings {
                chunks_dropped_by_cap: pre_filter_dropped,
                ..CommitTimings::default()
            });
        }

        let vec_start = std::time::Instant::now();
        self.commit_vectors_batch(&all_chunks, &embeddings).await?;
        let vector_upsert_ms = vec_start.elapsed().as_millis() as u64;

        let bm25_start = std::time::Instant::now();
        self.commit_bm25_batch(&all_chunks).await;
        let bm25_ms = bm25_start.elapsed().as_millis() as u64;

        self.commit_embeddings_cache(&all_chunks, embeddings).await;
        if self.corpus.is_some() {
            self.commit_corpus_to_redb(&all_chunks, &entities_by_file)
                .await;
        }
        self.commit_corpus(&mut all_chunks).await;
        self.commit_entities(entities_by_file).await;

        let kg_ms = if defer_graph_rebuild {
            0
        } else {
            let kg_start = std::time::Instant::now();
            self.rebuild_symbol_graph().await;
            kg_start.elapsed().as_millis() as u64
        };

        // Issue #85: fire-and-forget incremental persistence. Issue #29:
        // throttled — only actually spawns the HNSW snapshot every
        // `HNSW_SNAPSHOT_BATCH_INTERVAL` batches.
        self.spawn_incremental_persist(false);

        Ok(CommitTimings {
            chunks: chunk_total,
            bm25_ms,
            vector_upsert_ms,
            kg_ms,
            chunks_dropped_by_cap: pre_filter_dropped,
        })
    }

    /// Single batched HNSW upsert across all chunks that have an embedding.
    ///
    /// Why: drops 3N lock acquisitions to 3 for a batch of N chunks. Also
    /// guards against NaN / zero vectors (issue #764) — inserting them silently
    /// poisons the HNSW graph so every subsequent cosine-similarity search
    /// returns 0.0 for the affected neighbours.
    /// What: filters chunks without embeddings (BM25-only mode), validates each
    /// vector for NaN/all-zero content, then delegates to `store.upsert_batch`.
    /// No-op when no store is wired or no embeddings were computed.
    /// Test: `nan_vector_rejected_loudly` and `zero_vector_rejected_loudly`
    /// in `tests.rs`; `test_index_files_batch_*` covers the healthy path.
    pub(crate) async fn commit_vectors_batch(
        &self,
        chunks: &[RawChunk],
        embeddings: &[Option<Vec<f32>>],
    ) -> Result<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let mut items: Vec<(String, Vec<f32>)> = Vec::new();
        for (chunk, vec_opt) in chunks.iter().zip(embeddings.iter()) {
            let Some(v) = vec_opt.as_ref() else {
                continue;
            };
            // Issue #764: reject NaN vectors loudly.
            if v.iter().any(|x| x.is_nan()) {
                tracing::warn!(
                    chunk_id = %chunk.id,
                    "commit_vectors_batch: NaN component in embedding vector — \
                     skipping HNSW upsert for this chunk (issue #764). \
                     This indicates a sidecar or model defect; \
                     check embedderd logs."
                );
                continue;
            }
            // Issue #764: reject all-zero vectors.
            if v.iter().all(|x| *x == 0.0_f32) {
                tracing::warn!(
                    chunk_id = %chunk.id,
                    "commit_vectors_batch: all-zero embedding vector — \
                     skipping HNSW upsert for this chunk (issue #764). \
                     This indicates a sidecar or model defect; \
                     check embedderd logs."
                );
                continue;
            }
            items.push((chunk.id.clone(), v.clone()));
        }
        if items.is_empty() {
            return Ok(());
        }
        store
            .upsert_batch(&items)
            .await
            .context("batch upsert chunk vectors")
    }

    /// Upsert every chunk's BM25 document under a single write lock.
    ///
    /// Why: doing this before moving chunks into the corpus avoids a second
    /// clone of each chunk.
    /// What: holds the BM25 write lock once and walks `chunks` to upsert
    /// `body + virtual_terms` for each.
    /// Test: BM25 search correctness is covered by every search test.
    pub(crate) async fn commit_bm25_batch(&self, chunks: &[RawChunk]) {
        let mut bm25 = self.bm25.write().await;
        for chunk in chunks {
            let text = Self::bm25_doc_text(chunk);
            bm25.upsert_document(&chunk.id, &text);
        }
    }

    /// Cache per-chunk embeddings for MMR diversity (#28).
    ///
    /// Why: MMR needs vectors for already-ranked chunks without paying a
    /// re-embed or HNSW round-trip per candidate. Skip entirely when no
    /// embedder is wired (BM25-only mode).
    /// What: walks chunks and their (consumed) embeddings, inserts each
    /// `(id, vec)` pair under one write lock.
    /// Test: covered indirectly by `test_get_embedding_returns_some_after_indexing`.
    pub(crate) async fn commit_embeddings_cache(
        &self,
        chunks: &[RawChunk],
        embeddings: Vec<Option<Vec<f32>>>,
    ) {
        if self.embedder.is_none() {
            return;
        }
        let mut emb_cache = self.chunk_embeddings.write().await;
        for (chunk, vec_opt) in chunks.iter().zip(embeddings) {
            if let Some(vec) = vec_opt {
                emb_cache.put(chunk.id.clone(), vec);
            }
        }
    }

    /// Drain `chunks` into the corpus under a single write lock.
    ///
    /// Why: single-lock insertion shrinks the write-lock window to milliseconds
    /// even for large batches.
    /// What: consumes `chunks` via `drain` so callers don't keep a stale copy.
    /// Honours `max_chunks_per_index()` (issue #75).
    /// Test: covered indirectly by every search test.
    pub(crate) async fn commit_corpus(&self, chunks: &mut Vec<RawChunk>) {
        let cap = max_chunks_per_index();
        let mut corpus = self.chunks.write().await;
        let mut dropped = 0usize;
        for chunk in chunks.drain(..) {
            if !corpus.contains_key(&chunk.id) && corpus.len() >= cap {
                dropped += 1;
                continue;
            }
            corpus.insert(chunk.id.clone(), chunk);
        }
        if dropped > 0 {
            tracing::warn!(
                "index '{}' chunk cap ({}) reached — dropped {} new chunks in batch",
                self.index_id,
                cap,
                dropped
            );
        }
    }

    /// Persist a committed batch to the durable redb corpus store (issue #28).
    ///
    /// Why: replaces the old full-rewrite `chunks.json` snapshot. Each batch is
    /// written in its own redb write transaction, so the on-disk corpus is
    /// always crash-consistent and the write cost is O(batch) rather than
    /// O(corpus). Issue #29: chunks and entities are now written via
    /// `CorpusStore::upsert_batch` in a single redb transaction.
    /// What: clones the chunks plus entities, moves them onto a blocking worker,
    /// and writes both tables in one atomic transaction. Failures are logged at
    /// `warn` and swallowed.
    /// Test: `tests::test_corpus_store_roundtrip`.
    pub(crate) async fn commit_corpus_to_redb(
        &self,
        chunks: &[RawChunk],
        entities_by_file: &[(String, Vec<RawEntity>)],
    ) {
        let Some(corpus) = self.corpus.clone() else {
            return;
        };
        let chunks = chunks.to_vec();
        let entities = entities_by_file.to_vec();
        let index_id = self.index_id.clone();
        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            corpus.upsert_batch(&chunks, &entities)
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(
                "index '{index_id}': redb corpus write failed ({e}) — \
                 in-memory commit succeeded; on-disk corpus will re-converge \
                 on the next batch or shutdown flush"
            ),
            Err(e) => tracing::warn!("index '{index_id}': redb corpus write task panicked ({e})"),
        }
    }

    /// Insert each `(file_path, entities)` tuple into the per-file entity map.
    ///
    /// Why: factored so the early-return path (empty batch) and the main commit
    /// path share one implementation.
    /// What: holds the entities write lock once and inserts every tuple.
    /// Test: covered indirectly by `test_entity_exact_match_*`.
    pub(crate) async fn commit_entities(&self, entities_by_file: Vec<(String, Vec<RawEntity>)>) {
        let mut emap = self.entities.write().await;
        for (path, ents) in entities_by_file {
            emap.insert(path, ents);
        }
    }

    /// Number of chunks currently held in the corpus.
    ///
    /// Why: used by service health endpoints and test assertions.
    /// What: non-blocking try_read on the corpus map.
    /// Test: every test that tracks chunk counts uses this.
    pub fn chunk_count(&self) -> usize {
        self.chunks.try_read().map(|g| g.len()).unwrap_or(0)
    }

    /// Snapshot the current symbol graph. Cheap (`Arc::clone`); intended for
    /// read-only KG queries from concurrent search handlers.
    ///
    /// Why: callers must not hold the symbol_graph write lock — handing out an
    /// `Arc` lets them read the graph without a lock.
    /// What: acquires the symbol_graph read lock, clones the `Arc`, releases.
    /// Test: covered indirectly by every KG search test.
    pub async fn symbol_graph(&self) -> Arc<SymbolGraph> {
        Arc::clone(&*self.symbol_graph.read().await)
    }

    /// Borrow the durable redb corpus (issue #41 phase 4).
    ///
    /// Why: exposes the `Arc<CorpusStore>` to callers (e.g. `server.rs`) that
    /// need direct access to the on-disk chunk + symbol tables without holding
    /// the indexer's internal `RwLock`.
    /// What: returns `None` for BM25-only / test indexers; `Some(Arc::clone)`
    /// otherwise.
    /// Test: covered indirectly by search integration tests.
    pub fn corpus_arc(&self) -> Option<Arc<crate::core::corpus::CorpusStore>> {
        self.corpus.as_ref().map(Arc::clone)
    }
}
