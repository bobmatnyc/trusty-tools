//! Ingestion + indexing pipeline for [`CodeIndexer`].
//!
//! Why: parsing files, embedding chunks, and committing batched results into
//! the corpus / BM25 / HNSW / KG is the single largest cluster of behaviour on
//! `CodeIndexer`. Lifting it into a dedicated module keeps the search hot path
//! and the persistence module focused.
//! What: `add_chunk`, `index_file`, the NLP enrichment helper,
//! `index_files_batch[_no_rebuild]`, `parse_and_embed_files`, the parallel
//! parse + batched embed helpers, and `rebuild_symbol_graph[_now]`.
//! `commit_parsed_batch` and every `commit_*` helper live in the sibling
//! `ingest::commit` sub-module (`ingest/commit.rs`).
//! Test: covered by `test_index_files_batch_*`,
//! `test_virtual_terms_populated_from_entities`, and every ingest-flavoured
//! test in `indexer::tests`.

pub(crate) mod commit;
pub(crate) mod embed;

use anyhow::{Context, Result};

use crate::core::chunker::{chunk_ast, RawChunk};
use crate::core::entity::RawEntity;
use crate::core::symbol_graph::{ChunkTuple, SymbolGraph};

use super::{populate_virtual_terms, CodeIndexer, ParsedBatch};

/// Minimum chunks embedded before a progress notification is fired.
///
/// Why: the caller (reindex orchestrator) needs fine-grained progress so the
/// CLI Embed bar advances continuously rather than in coarse per-file-batch
/// jumps.
/// What: `embed_chunks_in_batches` fires the optional `progress_tx` callback
/// at most once per wave but not more often than every
/// `PROGRESS_CHUNK_INTERVAL` chunks.
/// Test: `progress_interval_constant_is_32` below.
pub(crate) const PROGRESS_CHUNK_INTERVAL: usize = 32;

impl CodeIndexer {
    /// Rebuild the symbol graph from the current corpus.
    ///
    /// Why: called after any mutation (`add_chunk`, `remove_chunk`,
    /// `index_file`). Rebuilding is O(N + E) over chunks/calls and the
    /// corpus is small + in-memory, so we favour simplicity over incremental
    /// maintenance.
    /// What: snapshots chunk tuples and entity lists under read locks, builds
    /// a new `SymbolGraph`, persists it to the corpus if wired, and installs it.
    /// Test: every test that calls `add_chunk` or `index_file` exercises the
    /// rebuild path indirectly.
    pub(super) async fn rebuild_symbol_graph(&self) {
        // Issue #2162 follow-up: this function reads `self.chunks` and
        // `self.entities` directly below, but several call paths
        // (`remove_file`, `remove_chunk` from the FSEvents watcher,
        // `rebuild_symbol_graph_for_reindex` on a prune-only reindex,
        // the contributed-graph ingest endpoint) reach this function without
        // having rehydrated either structure first. An idle-evicted map read
        // here would silently rebuild the graph from an EMPTY chunk/entity
        // snapshot, dropping every entity-derived KG edge for the WHOLE
        // corpus — and that impoverished graph then gets persisted to redb,
        // so the loss survives a restart. Guarding at this single choke
        // point (every caller funnels through here) is a fast no-op when
        // nothing was evicted.
        self.ensure_chunks_loaded().await;
        self.ensure_bm25_entities_loaded().await;

        // Issue (180GB RSS fix): the temporary `Vec<ChunkTuple>` snapshot clones
        // every chunk's strings (id, file, function_name, calls, inherits_from)
        // and can hit 1-2 GB on a 1M-chunk corpus. We can't avoid the snapshot
        // entirely (build_from_chunks needs a slice, and we don't want to hold
        // the chunks read lock across `add_node`), but we cap snapshot size to
        // the same KG node cap so we don't allocate more than we'll actually
        // use. Chunks past the cap can't contribute new symbols anyway.
        let kg_cap = crate::core::symbol_graph::max_kg_nodes();
        let chunks = self.chunks.read().await;
        // Pre-size for the worst case. When `kg_cap == 0` (unlimited) fall back
        // to corpus size. Multiplied by 2 because the cap is on unique symbols
        // and a single function might be defined across a handful of duplicates.
        let snapshot_cap = if kg_cap == 0 {
            chunks.len()
        } else {
            // Heuristic: most chunks have a function name; cap snapshot at
            // 2× the KG node cap to leave headroom for duplicates while still
            // bounding peak allocation.
            (kg_cap.saturating_mul(2)).min(chunks.len())
        };
        // Issue #824: iterate in deterministic (file, id) order before
        // truncating so the same symbols are always included across restarts.
        // Without sorting, HashMap/DashMap iteration order is arbitrary —
        // on a large repo the N chunks that fall into the dropped half change
        // between daemon restarts, making call-chain results non-reproducible
        // and confusing to diagnose. Sorting by (file, id) is stable and
        // cheap relative to the string clone cost.
        let mut all_tuples: Vec<ChunkTuple> = chunks
            .values()
            .map(|c| {
                (
                    c.id.clone(),
                    c.file.clone(),
                    c.function_name.clone(),
                    c.calls.clone(),
                    c.inherits_from.clone(),
                    c.chunk_type.clone(),
                )
            })
            .collect();
        drop(chunks);

        // Sort by (file, chunk_id) for deterministic truncation. The sort key
        // is (field 1 = file, field 0 = id) — both are the first-class identity
        // fields we want to be stable across runs.
        all_tuples.sort_unstable_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

        if snapshot_cap < all_tuples.len() {
            tracing::warn!(
                index_id = %self.index_id,
                total_chunks = all_tuples.len(),
                snapshot_cap,
                "kg: snapshot truncated to {} chunks (2×MAX_KG_NODES={}); \
                 symbols in the dropped portion will have no KG edges this boot. \
                 Raise TRUSTY_MAX_KG_NODES or run --force reindex to rebuild the \
                 graph at full size. (issue #824)",
                snapshot_cap,
                snapshot_cap / 2,
            );
        }
        let tuples: Vec<ChunkTuple> = all_tuples.into_iter().take(snapshot_cap).collect();

        // Issue #41 phase 2: include per-file entity lists so Phase B/C edges
        // (`TestedBy`, `CoOccursInTest`, `Documents`, `ReferencesConcept`)
        // are wired into the graph. The clones are cheap relative to the
        // chunk snapshot above.
        let entities_snapshot: Vec<(String, Vec<crate::core::entity::RawEntity>)> = {
            let ents = self.entities.read().await;
            ents.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };

        let new_graph = std::sync::Arc::new(SymbolGraph::build_from_chunks_with_entities(
            &tuples,
            &entities_snapshot,
        ));
        // Free the snapshots immediately — they are the second-largest
        // allocations in this function and we don't need them past
        // `build_from_chunks_with_entities`.
        drop(tuples);
        drop(entities_snapshot);

        // Issue #41 phase 2 + ADR-0009: persist the freshly rebuilt *derived*
        // graph (best-effort, pre-merge so the derived kg_* tables never
        // absorb contributed rows), then fold the stored contributed overlay
        // back in — a reindex must not evict contributed edges from the
        // serving graph. Both redb-bound steps run on one blocking worker;
        // failures degrade with warnings (see `save_then_merge_contrib`).
        let new_graph = crate::core::symbol_graph::save_then_merge_contrib(
            new_graph,
            self.corpus.clone(),
            self.index_id.clone(),
        )
        .await;

        *self.symbol_graph.write().await = new_graph;
    }

    /// Add (or replace) a chunk in the corpus. If an embedder + store are
    /// attached, the chunk is also embedded and upserted into the HNSW index.
    pub async fn add_chunk(&self, chunk: RawChunk) -> Result<()> {
        self.add_chunk_inner(chunk).await?;
        self.rebuild_symbol_graph().await;
        Ok(())
    }

    /// Internal helper: do every side effect of `add_chunk` **except** the
    /// trailing symbol graph rebuild.
    ///
    /// Why: `index_file` ingests N chunks from one file via this code path.
    /// Calling the public `add_chunk` in that loop triggers N symbol graph
    /// rebuilds (each `O(corpus)`).
    /// What: embeds + HNSW-upserts (when wired), maintains BM25, applies the
    /// per-index chunk cap, and inserts into the corpus map.
    /// Test: covered transitively by every test that calls `add_chunk` or
    /// `index_file`.
    pub(super) async fn add_chunk_inner(&self, chunk: RawChunk) -> Result<()> {
        self.ensure_chunks_loaded().await;
        // Issue #2162: rehydrate BM25/entities before the direct bm25 upsert
        // below, mirroring `commit_parsed_batch`.
        self.ensure_bm25_entities_loaded().await;
        self.touch_activity();
        let id = chunk.id.clone();

        {
            let chunks = self.chunks.read().await;
            let cap = super::max_chunks_per_index();
            if !chunks.contains_key(&id) && chunks.len() >= cap {
                tracing::warn!(
                    "index '{}' chunk cap ({}) reached — skipping chunk {}",
                    self.index_id,
                    cap,
                    id
                );
                return Ok(());
            }
        }

        if let (Some(embedder), Some(store)) = (&self.embedder, &self.store) {
            let vec = embedder
                .embed(&chunk.content)
                .await
                .context("embed chunk content")?;
            store
                .upsert(&id, vec.clone())
                .await
                .context("upsert chunk vector")?;
            self.chunk_embeddings.write().await.put(id.clone(), vec);
        }

        let bm25_text = Self::bm25_doc_text(&chunk);
        self.bm25.write().await.upsert_document(&id, &bm25_text);

        self.chunks.write().await.insert(id, chunk);
        Ok(())
    }

    /// Parse a file with `chunk_ast`, store every chunk in the corpus, and
    /// retain the per-file entity list for later KG/entity-search phases.
    ///
    /// Why: this routine collects every chunk first, embeds them in one
    /// batched ONNX call, then commits BM25, HNSW, the embeddings cache, and
    /// the corpus under the same lock-window-minimizing path used by the bulk
    /// reindex.
    /// What: chunk the file, batch-embed all chunks, commit vectors / BM25 /
    /// corpus, then enrich entities via the NLP helper and rebuild the
    /// symbol graph once.
    /// Test: covered by every `index_file`-based test in `indexer::tests`.
    pub async fn index_file(&self, file_path: &str, content: &str) -> Result<()> {
        let (mut chunks, entities) = chunk_ast(file_path, content);

        populate_virtual_terms(&mut chunks, &entities);

        let chunk_contents: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();

        if !chunks.is_empty() {
            let embeddings = self.embed_chunks_in_batches(&chunks, None).await?;
            let parsed = ParsedBatch {
                chunks,
                embeddings,
                entities_by_file: Vec::new(),
                parse_ms: 0,
                embed_ms: 0,
                vector_count: 0,
            };
            self.commit_parsed_batch(parsed, true).await?;
        }

        let all_entities = self
            .enrich_with_nlp_entities(file_path, content, &chunk_contents, entities)
            .await;

        self.entities
            .write()
            .await
            .insert(file_path.to_string(), all_entities);
        self.rebuild_symbol_graph().await;
        Ok(())
    }

    /// Run NER + ConceptCluster passes and merge their entities with the
    /// AST-derived base list.
    ///
    /// Why: keeps `index_file` focused on chunk persistence; isolates the two
    /// gated NLP passes behind a single helper.
    /// What: extracts doc-comment NER entities, runs ConceptCluster when an
    /// embedder is wired, returns the combined entity list.
    /// Test: covered indirectly by every `index_file` integration test.
    async fn enrich_with_nlp_entities(
        &self,
        file_path: &str,
        content: &str,
        #[cfg_attr(not(feature = "clustering"), allow(unused_variables))]
        chunk_contents: &[String],
        base_entities: Vec<RawEntity>,
    ) -> Vec<RawEntity> {
        let doc_text = crate::core::ner::extract_doc_comments(content);
        let ner_entities = self.ner.extract(&doc_text, file_path);
        if !ner_entities.is_empty() {
            tracing::debug!(
                "ner: {} NaturalLanguagePhrase entities for {}",
                ner_entities.len(),
                file_path
            );
        }

        let mut all_entities = base_entities;
        all_entities.extend(ner_entities);

        #[cfg(feature = "clustering")]
        if let Some(embedder) = &self.embedder {
            let refs: Vec<&str> = chunk_contents.iter().map(|s| s.as_str()).collect();
            let cluster_entities = crate::core::concept_cluster::cluster_concepts_from_contents(
                &refs,
                embedder.as_ref(),
                file_path,
            )
            .await;
            if !cluster_entities.is_empty() {
                tracing::debug!(
                    "concept_cluster: {} ConceptCluster entities for {}",
                    cluster_entities.len(),
                    file_path
                );
                all_entities.extend(cluster_entities);
            }
        }

        all_entities
    }

    /// Bulk-index many files in one shot.
    ///
    /// Why: per-file `index_file` issues one ONNX `embed` call per chunk and
    /// rebuilds the symbol graph after every chunk. On a 13k-file Java
    /// monorepo that translates to ~80k serial ONNX calls.
    /// What: parse → batch embed → commit → rebuild symbol graph once.
    /// Returns the total number of chunks added across the batch.
    pub async fn index_files_batch(&self, files: &[(String, String)]) -> Result<usize> {
        self.index_files_batch_inner(files, false).await
    }

    /// Bulk-index variant that skips the trailing symbol graph rebuild.
    ///
    /// Why: a full reindex calls `index_files_batch` many times. Each call
    /// previously rebuilt the symbol graph, which is `O(N + E)` over the
    /// entire corpus. The reindex orchestrator now calls this per batch and
    /// rebuilds the graph **once** at the very end.
    pub async fn index_files_batch_no_rebuild(&self, files: &[(String, String)]) -> Result<usize> {
        self.index_files_batch_inner(files, true).await
    }

    /// Public hook for the bulk reindex orchestrator: rebuild the symbol graph
    /// once after a series of `index_files_batch_no_rebuild` calls.
    pub async fn rebuild_symbol_graph_now(&self) {
        self.rebuild_symbol_graph().await;
    }

    async fn index_files_batch_inner(
        &self,
        files: &[(String, String)],
        defer_graph_rebuild: bool,
    ) -> Result<usize> {
        if files.is_empty() {
            return Ok(0);
        }
        let parsed = self.parse_and_embed_files(files.to_vec()).await?;
        let timings = self
            .commit_parsed_batch(parsed, defer_graph_rebuild)
            .await?;
        Ok(timings.chunks)
    }

    /// Phase 1+2 of the bulk pipeline: parse files into chunks and embed them.
    ///
    /// Why: this phase does the heavy CPU/ONNX work but mutates **no shared
    /// state**. Lifting it out of the corpus write lock lets the reindex
    /// orchestrator overlap a batch's parse+embed with the previous batch's
    /// commit phase.
    /// What: parallel parse via rayon (with virtual_terms population from
    /// entities), then batched ONNX embed. Returns a [`ParsedBatch`] ready for
    /// [`Self::commit_parsed_batch`].
    /// Test: covered indirectly by every `index_files_batch*` test.
    pub async fn parse_and_embed_files(&self, files: Vec<(String, String)>) -> Result<ParsedBatch> {
        self.parse_files_inner(files, true, None).await
    }

    /// Progress-tracked variant of [`parse_and_embed_files`].
    ///
    /// Why: the reindex orchestrator needs per-wave chunk counts to emit
    /// fine-grained `chunk_progress` SSE events.
    /// What: same as `parse_and_embed_files` but passes `progress_tx` into
    /// `embed_chunks_in_batches`.
    /// Test: covered by `service::reindex::tests::progress_granularity_*`.
    pub async fn parse_and_embed_files_tracked(
        &self,
        files: Vec<(String, String)>,
        progress_tx: tokio::sync::mpsc::UnboundedSender<(usize, u64)>,
    ) -> Result<ParsedBatch> {
        self.parse_files_inner(files, true, Some(progress_tx)).await
    }

    /// Parse-only variant for the staged-pipeline `lexical_only` opt-in
    /// (issue #109, Phase 1).
    ///
    /// Why: callers who explicitly want a daemonized ripgrep set
    /// `lexical_only: true` at index-create time; they must skip the embedder
    /// entirely.
    /// What: same as `parse_and_embed_files` but skips the embed step.
    /// Test: `service::reindex::tests::lexical_only_index_never_runs_stage_2`.
    pub async fn parse_files_only(&self, files: Vec<(String, String)>) -> Result<ParsedBatch> {
        self.parse_files_inner(files, false, None).await
    }

    /// Shared implementation for `parse_and_embed_files*` and
    /// `parse_files_only`. `embed` selects the ONNX step; `progress_tx` is
    /// forwarded to `embed_chunks_in_batches`.
    async fn parse_files_inner(
        &self,
        files: Vec<(String, String)>,
        embed: bool,
        progress_tx: Option<tokio::sync::mpsc::UnboundedSender<(usize, u64)>>,
    ) -> Result<ParsedBatch> {
        if files.is_empty() {
            return Ok(ParsedBatch::default());
        }

        let parse_start = std::time::Instant::now();
        let parsed = Self::parse_files_parallel(files).await?;

        let mut all_chunks: Vec<RawChunk> = Vec::new();
        let mut entities_by_file: Vec<(String, Vec<RawEntity>)> = Vec::with_capacity(parsed.len());
        for (path, chunks, entities) in parsed {
            all_chunks.extend(chunks);
            entities_by_file.push((path, entities));
        }
        let parse_ms = parse_start.elapsed().as_millis() as u64;

        let (embeddings, embed_ms, vector_count) = if embed {
            let embed_start = std::time::Instant::now();
            let embeddings = self
                .embed_chunks_in_batches(&all_chunks, progress_tx.as_ref())
                .await?;
            let embed_ms = embed_start.elapsed().as_millis() as u64;
            let vector_count = embeddings.iter().filter(|e| e.is_some()).count();
            (embeddings, embed_ms, vector_count)
        } else {
            let embeddings: Vec<Option<Vec<f32>>> = vec![None; all_chunks.len()];
            (embeddings, 0, 0)
        };

        Ok(ParsedBatch {
            chunks: all_chunks,
            embeddings,
            entities_by_file,
            parse_ms,
            embed_ms,
            vector_count,
        })
    }

    /// Parse every file in parallel via rayon and populate `virtual_terms`
    /// from the AST-derived entity list.
    ///
    /// Why: `chunk_ast` is sync + CPU-bound, so rayon's worker pool is a
    /// better fit than tokio tasks.
    /// What: spawns a single blocking task that parallel-maps `chunk_ast`
    /// across every input.
    /// Test: covered indirectly by every `index_files_batch_*` test.
    async fn parse_files_parallel(
        files: Vec<(String, String)>,
    ) -> Result<Vec<(String, Vec<RawChunk>, Vec<RawEntity>)>> {
        use rayon::prelude::*;
        tokio::task::spawn_blocking(move || {
            files
                .par_iter()
                .map(|(path, content)| {
                    let (mut chunks, entities) = chunk_ast(path, content);
                    populate_virtual_terms(&mut chunks, &entities);
                    (path.clone(), chunks, entities)
                })
                .collect()
        })
        .await
        .context("batch parse task panicked")
    }

    /// Pre-warm the embedder by sending a trivial single-text batch.
    ///
    /// Why: Issue #744. The sidecar (`trusty-embedderd`) is lazy-spawned on
    /// the first embedding request. On Apple Silicon / ONNX the cold spawn +
    /// CoreML session init takes 30 to 60 seconds. Calling `warm_embedder`
    /// concurrently with the file walk lets model-load overlap with chunking.
    /// What: if `self.embedder` is wired, calls `embed_batch(&["warm"])` to
    /// trigger the lazy spawn and ONNX session init. The result is discarded.
    /// Test: `warm_embedder_noop_without_embedder`.
    pub async fn warm_embedder(&self) {
        let Some(embedder) = &self.embedder else {
            return;
        };
        match embedder.embed_batch(&["warm"]).await {
            Ok(_) => {
                tracing::debug!(
                    "warm_embedder[{}]: embedder pre-warm succeeded",
                    self.index_id
                );
            }
            Err(e) => {
                tracing::debug!(
                    "warm_embedder[{}]: embedder pre-warm failed ({e}) — \
                     will retry on first batch",
                    self.index_id
                );
            }
        }
    }

    /// Embed corpus chunks that don't already have a stored vector and upsert
    /// them into HNSW (issue #923 C2 pass; issue #2984 Phase 1 HIGH finding 3).
    ///
    /// Why: fast pass (C1) stored chunks without embedding; this catch-up job
    /// fills the semantic lane. `progress_tx` is forwarded to
    /// `embed_chunks_in_batches` so callers can update `stages.semantic.embedded`
    /// per wave for live N/total progress on `/indexes/:id/status` (issue #929).
    /// Issue #2984 Phase 1: this is also the runtime vector-component
    /// re-enable catch-up (`components::spawn_component_catch_up` via
    /// `service::reindex::run_embed_catch_up`) — the locked design decision is
    /// "incremental catch-up, never a forced full rebuild", so re-embedding
    /// every chunk unconditionally (the pre-fix behaviour) directly
    /// contradicted it and re-paid the full embed cost every time vector was
    /// re-enabled, even for content that was embedded before it was disabled.
    /// What: snapshots chunks, then filters to only those the vector store
    /// does NOT already have (`VectorStore::contains_many`, a single bulk
    /// membership check) before embedding/committing. On a fresh corpus (the
    /// ordinary post-reindex C1/C2 fast pass) no id is present yet, so the
    /// filter is a no-op there — behaviour is unchanged for that path.
    /// Returns `(newly_embedded, total_corpus_chunks)`.
    /// Test: `deferred_embed_pass_marks_semantic_ready_and_is_idempotent`,
    /// `embed_deferred_chunks_skips_already_embedded_chunks`.
    pub async fn embed_deferred_chunks(
        &self,
        progress_tx: Option<&tokio::sync::mpsc::UnboundedSender<(usize, u64)>>,
    ) -> anyhow::Result<(usize, usize)> {
        let chunks: Vec<RawChunk> = {
            self.ensure_chunks_loaded().await;
            let map = self.chunks.read().await;
            map.values().cloned().collect()
        };
        let total = chunks.len();
        if total == 0 || self.embedder.is_none() || self.store.is_none() {
            return Ok((0, total));
        }
        // Issue #2984 Phase 1 HIGH finding 3: incremental catch-up — skip
        // chunks that already have a stored vector rather than blindly
        // re-embedding the whole corpus.
        let store = self.store.as_ref().expect("store presence checked above");
        let ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();
        let already_embedded = store.contains_many(&ids).await;
        let to_embed: Vec<RawChunk> = chunks
            .into_iter()
            .zip(already_embedded)
            .filter_map(|(chunk, embedded)| (!embedded).then_some(chunk))
            .collect();
        if to_embed.is_empty() {
            return Ok((0, total));
        }
        let embeddings = self.embed_chunks_in_batches(&to_embed, progress_tx).await?;
        self.commit_vectors_batch(&to_embed, &embeddings).await?;
        self.commit_embeddings_cache(&to_embed, embeddings).await;
        Ok((to_embed.len(), total))
    }

    /// Count corpus chunks NOT yet embedded (issue #3748 slice A, review
    /// finding 2) — the real cost [`embed_deferred_chunks`] will pay, as
    /// opposed to `chunk_count()`'s TOTAL corpus size.
    ///
    /// Why: the deferred-embed catch-up queue (`service::reindex::defer_embed_queue`)
    /// originally keyed its size-ordered priority on `chunk_count()`,
    /// captured once by `finish_reindex` after every reindex — including
    /// INCREMENTAL ones. But `embed_deferred_chunks` only ever embeds the
    /// chunks the vector store does not already have (its own
    /// `contains_many` filter, see that method's docs) — so an incremental
    /// reindex of a 94k-chunk repo with a single changed chunk sorted as
    /// "huge" in the queue even though its real embed work is near-instant
    /// (one chunk). Mirroring that exact filter here — without embedding
    /// anything — gives the queue an accurate priority key for BOTH a cold
    /// index (nothing embedded yet, so this returns the full corpus size,
    /// identical to the old `chunk_count()` behaviour) and an incremental
    /// one (a handful of changed chunks).
    /// What: snapshots chunk ids (reusing `ensure_chunks_loaded`, a no-op
    /// when chunks are already resident — always true immediately after a
    /// reindex, the only caller of this today), runs the SAME single bulk
    /// `VectorStore::contains_many` check `embed_deferred_chunks` runs, and
    /// counts the `false` (not-yet-embedded) entries. Falls back to the
    /// unfiltered chunk count when there's no embedder/store configured —
    /// `embed_deferred_chunks` would short-circuit to `Ok((0, total))`
    /// immediately in that case too, so the queue key is moot there; no
    /// point paying for a membership check that can't apply.
    /// Test: `pending_embed_count_matches_delta_not_total_chunk_count`,
    /// `pending_embed_count_falls_back_to_full_count_without_embedder`.
    pub async fn pending_embed_count(&self) -> usize {
        self.ensure_chunks_loaded().await;
        let chunks: Vec<RawChunk> = {
            let map = self.chunks.read().await;
            map.values().cloned().collect()
        };
        let total = chunks.len();
        let Some(store) = self.store.as_ref() else {
            return total;
        };
        if self.embedder.is_none() {
            return total;
        }
        let ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();
        let already_embedded = store.contains_many(&ids).await;
        already_embedded.into_iter().filter(|&e| !e).count()
    }
}

#[cfg(test)]
mod warm_embedder_tests {
    use super::super::CodeIndexer;

    /// `warm_embedder` on an indexer with no embedder must be a no-op.
    ///
    /// Why: Issue #744 — `warm_embedder` is called as a concurrent background
    /// task for every non-lexical reindex; on test indexers it must return
    /// immediately.
    /// What: calls `warm_embedder` on a bare `CodeIndexer`.
    /// Test: this test.
    #[tokio::test]
    async fn warm_embedder_noop_without_embedder() {
        let indexer = CodeIndexer::new("warm-test", "/tmp");
        indexer.warm_embedder().await;
    }
}

#[cfg(test)]
mod progress_interval_tests {
    use super::PROGRESS_CHUNK_INTERVAL;

    /// `PROGRESS_CHUNK_INTERVAL` must equal 32.
    ///
    /// Why: the constant is the contract between `embed_chunks_in_batches` and
    /// the reindex orchestrator.
    /// What: asserts the value is exactly 32.
    /// Test: this test.
    #[test]
    fn progress_interval_constant_is_32() {
        assert_eq!(PROGRESS_CHUNK_INTERVAL, 32);
    }
}
