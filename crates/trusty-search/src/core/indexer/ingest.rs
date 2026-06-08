//! Ingestion + indexing pipeline for [`CodeIndexer`].
//!
//! Why: parsing files, embedding chunks, and committing batched results into
//! the corpus / BM25 / HNSW / KG is the single largest cluster of behaviour on
//! `CodeIndexer`. Lifting it into a dedicated module keeps the search hot path
//! and the persistence module focused.
//! What: `add_chunk`, `index_file`, the NLP enrichment helper,
//! `index_files_batch[_no_rebuild]`, `parse_and_embed_files`, the parallel
//! parse + batched embed helpers, `commit_parsed_batch`, every `commit_*`
//! helper, and `rebuild_symbol_graph[_now]`.
//! Test: covered by `test_index_files_batch_*`, `test_virtual_terms_populated_from_entities`,
//! and every ingest-flavoured test in `indexer::tests`.

use std::sync::Arc;

use anyhow::{Context, Result};

use crate::core::chunker::{chunk_ast, RawChunk};
use crate::core::entity::RawEntity;
use crate::core::symbol_graph::{ChunkTuple, SymbolGraph};

use super::{
    embed_batch_size, max_chunks_per_index, populate_virtual_terms, CodeIndexer, CommitTimings,
    ParsedBatch,
};

/// (chunk_start_index, expected_count, embed_result) — alias to satisfy clippy.
type WaveResult = (usize, usize, Result<Vec<Vec<f32>>>);

/// Minimum chunks embedded before a progress notification is fired.
///
/// Why: the caller (reindex orchestrator) needs fine-grained progress so the
/// CLI Embed bar advances continuously rather than in coarse per-file-batch
/// jumps. 32 chunks ≈ 32 × ~50-token snippets — negligible overhead (~2000
/// events for a 65k-chunk index) while giving the operator visible movement
/// every second or two at typical embedding throughput.
/// What: `embed_chunks_in_batches` fires the optional `progress_tx` callback at
/// most once per wave but not more often than every `PROGRESS_CHUNK_INTERVAL`
/// chunks (a wave is `inflight × batch_size` chunks; with defaults 2 × 64 = 128
/// this means one notification per wave, which is finer than the previous single
/// notification per 128-file file-batch).
/// Test: `progress_interval_constant_is_32` below.
pub(crate) const PROGRESS_CHUNK_INTERVAL: usize = 32;

/// Concurrent in-flight sub-batches (issue #753). Reads `TRUSTY_EMBED_INFLIGHT`,
/// clamps to [1, 4], defaults to 2. Test: `embed_chunks_in_batches` (indirect).
fn resolve_embed_inflight() -> usize {
    static CACHED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("TRUSTY_EMBED_INFLIGHT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|n| n.clamp(1, 4))
            .unwrap_or(2)
    })
}

/// Resident-set-size of the current process, in megabytes.
///
/// Why: used by the CoreML memory tripwire to measure the RSS delta a single
/// `embed_batch` call produced. CoreML on Apple Silicon can spike RSS by tens
/// of GB within one call (the per-batch buffer is not released until the call
/// returns), so the inter-batch RSS poller fires too late to prevent the
/// spike — the tripwire instead measures the damage after the fact and halves
/// the batch size for subsequent calls.
/// What: reads resident set size for `std::process::id()`. On macOS, shells
/// out to `ps -o rss= -p <pid>` (KiB). On Linux, parses `VmRSS` from
/// `/proc/self/status` (KiB). Returns 0 on any error — the tripwire then
/// degrades gracefully (a 0 reading just means the tripwire never fires).
/// Test: `tripwire_tests::test_current_rss_mb_is_plausible` asserts the value
/// is in a plausible range (the probe is intentionally non-fatal — a 0 reading
/// just disables the tripwire — so it is only checked against a sanity ceiling).
fn current_rss_mb() -> usize {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let pid = std::process::id();
        Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<usize>().ok())
            .map(|kb| kb / 1024)
            .unwrap_or(0)
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("VmRSS:"))
                    .and_then(|rest| rest.split_whitespace().next().map(str::to_string))
            })
            .and_then(|kb| kb.parse::<usize>().ok())
            .map(|kb| kb / 1024)
            .unwrap_or(0)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

impl CodeIndexer {
    /// Rebuild the symbol graph from the current corpus. Called after any
    /// mutation (`add_chunk`, `remove_chunk`, `index_file`). Rebuilding is
    /// O(N + E) over chunks/calls and the corpus is small + in-memory, so we
    /// favour simplicity over incremental maintenance.
    pub(super) async fn rebuild_symbol_graph(&self) {
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
        let mut tuples: Vec<ChunkTuple> = Vec::with_capacity(snapshot_cap);
        for c in chunks.values() {
            if tuples.len() >= snapshot_cap {
                break;
            }
            tuples.push((
                c.id.clone(),
                c.file.clone(),
                c.function_name.clone(),
                c.calls.clone(),
                c.inherits_from.clone(),
                c.chunk_type.clone(),
            ));
        }
        drop(chunks);

        // Issue #41 phase 2: include per-file entity lists so Phase B/C edges
        // (`TestedBy`, `CoOccursInTest`, `Documents`, `ReferencesConcept`)
        // are wired into the graph. The clones are cheap relative to the
        // chunk snapshot above.
        let entities_snapshot: Vec<(String, Vec<crate::core::entity::RawEntity>)> = {
            let ents = self.entities.read().await;
            ents.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };

        let new_graph = Arc::new(SymbolGraph::build_from_chunks_with_entities(
            &tuples,
            &entities_snapshot,
        ));
        // Free the snapshots immediately — they are the second-largest
        // allocations in this function and we don't need them past
        // `build_from_chunks_with_entities`.
        drop(tuples);
        drop(entities_snapshot);

        // Issue #41 phase 2: persist the freshly rebuilt graph alongside the
        // chunk corpus so warm-boot can skip the rebuild on restart. Best
        // effort — a persistence failure is logged at `warn` and never aborts
        // the in-memory swap (search keeps working with a stale on-disk graph
        // until the next successful save).
        if let Some(corpus) = &self.corpus {
            let corpus = Arc::clone(corpus);
            let graph_for_save = Arc::clone(&new_graph);
            let index_id = self.index_id.clone();
            let join =
                tokio::task::spawn_blocking(move || graph_for_save.save_to_corpus(&corpus)).await;
            match join {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!(
                    "index '{index_id}': kg persist failed ({e}) — graph stays in memory"
                ),
                Err(e) => tracing::warn!("index '{index_id}': kg persist task panicked ({e})"),
            }
        }

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
    /// rebuilds (each `O(corpus)`), producing an `O(N · corpus)` regression on
    /// any non-trivial file — a single 10-chunk file forced 11 rebuilds. By
    /// splitting the corpus-mutation work from the graph rebuild, `index_file`
    /// can run the rebuild **once** at the end of the file, restoring the
    /// intended `O(corpus)` cost per file.
    /// What: embeds + HNSW-upserts (when wired), maintains BM25, applies the
    /// per-index chunk cap (#75), and inserts into the corpus map. Does **not**
    /// touch the symbol graph — callers are responsible for rebuilding it
    /// after their batch of inserts.
    /// Test: covered transitively by every test that calls `add_chunk` or
    /// `index_file` in `indexer::tests`; the public `add_chunk` path still
    /// guarantees a fresh symbol graph on return.
    pub(super) async fn add_chunk_inner(&self, chunk: RawChunk) -> Result<()> {
        // An idle-evicted in-memory map must be rehydrated before we read its
        // length for the cap check or insert into it — otherwise the cap check
        // sees 0 and the map would diverge from the durable corpus.
        self.ensure_chunks_loaded().await;
        self.touch_activity();
        let id = chunk.id.clone();

        // Issue #75: hard cap per-index chunk count to bound RAM growth.
        // Upserts (existing id) are always allowed; only brand-new ids hit
        // the cap. Failing fast here keeps HNSW / BM25 / corpus in sync.
        {
            let chunks = self.chunks.read().await;
            let cap = max_chunks_per_index();
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
            // Cache for MMR diversity (#28). Cheap O(1) write under the corpus
            // mutation path so the search hot loop never has to re-embed.
            // LRU `put` evicts the oldest entry when at capacity.
            self.chunk_embeddings.write().await.put(id.clone(), vec);
        }

        // Maintain the persistent BM25 index. Doing this on every write keeps
        // the search path O(query_terms · postings) instead of O(corpus).
        let bm25_text = Self::bm25_doc_text(&chunk);
        self.bm25.write().await.upsert_document(&id, &bm25_text);

        self.chunks.write().await.insert(id, chunk);
        Ok(())
    }

    /// Parse a file with `chunk_ast`, store every chunk in the corpus, and
    /// retain the per-file entity list for later KG/entity-search phases.
    ///
    /// Why: previously this routine called `add_chunk_inner` once per chunk,
    /// which issues a single-chunk ONNX `embed` call apiece. For a 10-chunk
    /// file that's 10 sequential ONNX dispatches vs. the bulk path's single
    /// batched call. This rewrite collects every chunk first, embeds them
    /// in one batched ONNX call (matching `index_files_batch`), then commits
    /// BM25, HNSW, the embeddings cache, and the corpus under the same
    /// lock-window-minimizing path used by the bulk reindex.
    /// What: chunk the file, batch-embed all chunks, commit vectors / BM25 /
    /// corpus, then enrich entities via the NLP helper and rebuild the
    /// symbol graph once.
    /// Test: covered by every `index_file`-based test in `indexer::tests`
    /// plus the live indexing path exercised by integration tests.
    pub async fn index_file(&self, file_path: &str, content: &str) -> Result<()> {
        let (mut chunks, entities) = chunk_ast(file_path, content);

        // Issue #19: virtual_terms from entities so BM25 sees symbolic tokens
        // that don't appear literally in the chunk body.
        populate_virtual_terms(&mut chunks, &entities);

        // Snapshot chunk contents before move so we can run the ConceptCluster
        // pass below. Borrowing into the commit pipeline would hold the slice
        // across `await`.
        let chunk_contents: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();

        if !chunks.is_empty() {
            // Batch embed every chunk in one ONNX call (instead of N sequential
            // single-chunk calls). Mirrors the bulk-path `parse_and_embed_files`
            // → `commit_parsed_batch` flow.
            let embeddings = self.embed_chunks_in_batches(&chunks, None).await?;
            let parsed = ParsedBatch {
                chunks,
                embeddings,
                // index_file owns its own entity write below, so don't double-insert
                // via commit_parsed_batch.
                entities_by_file: Vec::new(),
                parse_ms: 0,
                embed_ms: 0,
                vector_count: 0,
            };
            // `defer_graph_rebuild = true` — we rebuild the graph once at the
            // tail of this function after entity enrichment, matching the
            // previous behaviour.
            self.commit_parsed_batch(parsed, true).await?;
        }

        let all_entities = self
            .enrich_with_nlp_entities(file_path, content, &chunk_contents, entities)
            .await;

        self.entities
            .write()
            .await
            .insert(file_path.to_string(), all_entities);
        // Single symbol graph rebuild for the entire file.
        self.rebuild_symbol_graph().await;
        Ok(())
    }

    /// Run NER + ConceptCluster passes and merge their entities with the
    /// AST-derived base list.
    ///
    /// Why: keeps `index_file` focused on chunk persistence; isolates the two
    /// gated NLP passes (both no-ops when their respective preconditions
    /// aren't met) behind a single helper.
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
        // Phase D: ONNX NER over doc comments (issue #23). Gated — no-op when
        // the model file is absent.
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

        // Phase C: ConceptCluster entities (issue #22). Only runs when an
        // embedder is wired and the file has enough doc comments to cluster.
        // Feature-gated behind `clustering` (issue #108) to keep linfa/ndarray
        // out of default builds.
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
    /// monorepo that translates to ~80k serial ONNX calls and ~80k graph
    /// rebuilds — the dominant cost of a cold reindex.
    ///
    /// What:
    /// 1. Parse every file into chunks + entities in parallel via rayon.
    /// 2. Collect all chunk texts and embed them in batches of
    ///    [`EMBED_BATCH_SIZE`] — one ONNX call per batch instead of per chunk.
    /// 3. Upsert vectors + insert chunks under a single corpus write lock.
    /// 4. Rebuild the symbol graph **once** at the end.
    ///
    /// Returns the total number of chunks added across the batch. Files whose
    /// chunker returned no chunks contribute zero; per-file embed/upsert
    /// failures are surfaced as `Err` and abort the batch (the caller should
    /// fall back to per-file `index_file` for diagnostics).
    pub async fn index_files_batch(&self, files: &[(String, String)]) -> Result<usize> {
        self.index_files_batch_inner(files, false).await
    }

    /// Bulk-index variant that skips the trailing symbol graph rebuild.
    ///
    /// Why: a full reindex calls `index_files_batch` many times. Each call
    /// previously rebuilt the symbol graph (`O(N + E)` over the entire corpus
    /// with a per-edge suffix scan). On 14k files / 115k chunks that adds up
    /// to the dominant non-embedding cost. The reindex orchestrator now calls
    /// `index_files_batch_no_rebuild` per batch and rebuilds the graph **once**
    /// at the very end.
    ///
    /// Single-file paths (`add_chunk`, `index_file`, file watcher) keep the
    /// per-call rebuild for correctness — they're not in the bulk-cold-start
    /// hot path.
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
    /// Why: This phase does the heavy CPU/ONNX work but mutates **no shared
    /// state**. Lifting it out of the corpus write lock lets the reindex
    /// orchestrator overlap a batch's parse+embed with the previous batch's
    /// commit phase, and ensures concurrent search readers are never blocked
    /// by ONNX inference.
    /// What: parallel parse via rayon (with virtual_terms population from
    /// entities), then batched ONNX embed (`EMBED_BATCH_SIZE` chunks per
    /// `embed_batch` call). Returns a [`ParsedBatch`] ready for
    /// [`Self::commit_parsed_batch`].
    /// Test: covered indirectly by every `index_files_batch*` test.
    pub async fn parse_and_embed_files(&self, files: Vec<(String, String)>) -> Result<ParsedBatch> {
        self.parse_files_inner(files, true, None).await
    }

    /// Progress-tracked variant of [`parse_and_embed_files`].
    ///
    /// Why: the reindex orchestrator needs per-wave chunk counts to emit
    /// fine-grained `chunk_progress` SSE events (every ~32 chunks) so the CLI
    /// Embed bar advances continuously rather than in coarse per-file-batch jumps.
    /// What: same as `parse_and_embed_files` but passes `progress_tx` into
    /// `embed_chunks_in_batches`; each completed wave sends `(chunks, ms)` to
    /// the caller via the channel.
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
    /// `lexical_only: true` at index-create time. The reindex pipeline must
    /// skip the embedder entirely on these indexes — otherwise warm-boot
    /// daemons would still pay the ONNX session arena cost on every batch.
    /// What: same as `parse_and_embed_files` but skips the embed step and
    /// returns a `ParsedBatch` whose `embeddings` slot is all `None`.
    /// Downstream `commit_parsed_batch` already handles all-`None`
    /// embeddings (BM25-only mode) so no commit-side changes are needed.
    /// Test: `service::reindex::tests::lexical_only_index_never_runs_stage_2`.
    pub async fn parse_files_only(&self, files: Vec<(String, String)>) -> Result<ParsedBatch> {
        self.parse_files_inner(files, false, None).await
    }

    /// Shared implementation for `parse_and_embed_files*` and `parse_files_only`.
    /// `embed` selects the ONNX step; `progress_tx` is forwarded to
    /// `embed_chunks_in_batches` for per-wave progress notifications.
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
            // Lexical-only: skip the embed step entirely. The commit phase
            // accepts an all-`None` embedding vector and stores chunks in
            // BM25 + redb without touching HNSW.
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
    /// better fit than tokio tasks. Returning `(path, chunks, entities)`
    /// keeps file boundaries intact for downstream entity-map insertion.
    /// What: spawns a single blocking task that parallel-maps `chunk_ast`
    /// across every input, then populates virtual_terms per chunk.
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

    /// Batched ONNX embed — multi-flight pipelined (issue #753). Serial loop left
    /// ANE ~78% idle; `TRUSTY_EMBED_INFLIGHT` (default 2) sub-batches now run
    /// concurrently via ordered `buffered`, filling the ANE queue. CoreML tripwire
    /// fires between waves. `Vec<Option<Vec<f32>>>` 1:1 with `chunks` (None=BM25).
    /// `progress_tx`: when `Some`, a `(chunks_in_wave, wave_embed_ms)` pair is
    /// sent after each wave (≥ `PROGRESS_CHUNK_INTERVAL` chunks) so callers can
    /// emit fine-grained progress events without polling.
    /// Test: `test_index_files_batch_*`. Order: `tests/multiflight.rs`.
    async fn embed_chunks_in_batches(
        &self,
        chunks: &[RawChunk],
        progress_tx: Option<&tokio::sync::mpsc::UnboundedSender<(usize, u64)>>,
    ) -> Result<Vec<Option<Vec<f32>>>> {
        use futures::StreamExt as _;

        let mut embeddings: Vec<Option<Vec<f32>>> = vec![None; chunks.len()];
        let (Some(embedder), Some(_store)) = (&self.embedder, &self.store) else {
            return Ok(embeddings);
        };
        let chunk_total = chunks.len();
        // CoreML pre-allocates ANE buffers; oversized batches stack until jetsam
        // kills the daemon. Use TRUSTY_COREML_BATCH_SIZE/TRUSTY_MAX_BATCH_SIZE.
        let is_coreml = matches!(
            embedder.provider(),
            trusty_common::embedder::ExecutionProvider::CoreML
                | trusty_common::embedder::ExecutionProvider::CoreMLAne
        );
        let mut batch_size = if is_coreml {
            let bs = crate::core::resolve_coreml_batch_size();
            tracing::debug!(
                "embed_chunks_in_batches: CoreML ({:?}) — TRUSTY_COREML_BATCH_SIZE={bs}",
                embedder.provider()
            );
            bs
        } else {
            embed_batch_size()
        };

        let tripwire_mb = if is_coreml {
            crate::core::resolve_coreml_tripwire_mb()
        } else {
            0
        };
        let mut tripwire_fired = false;
        let inflight = resolve_embed_inflight();
        tracing::debug!(chunk_total, batch_size, inflight, "embed_chunks_in_batches");
        let mut batch_start = 0usize;
        while batch_start < chunk_total {
            let wave_start = batch_start;
            let mut wave_sub_batches: Vec<(usize, Vec<String>)> = Vec::with_capacity(inflight);
            let mut wave_pos = batch_start;
            while wave_sub_batches.len() < inflight && wave_pos < chunk_total {
                let end = (wave_pos + batch_size).min(chunk_total);
                let sub: Vec<String> = chunks[wave_pos..end]
                    .iter()
                    .map(|c| c.content.clone())
                    .collect();
                wave_sub_batches.push((wave_pos, sub));
                wave_pos = end;
            }

            let rss_before = if is_coreml { current_rss_mb() } else { 0 };
            let wave_embed_start = std::time::Instant::now();
            // Dispatch concurrently — `buffered` preserves order.
            let wave_results: Vec<WaveResult> = {
                let iter = wave_sub_batches.into_iter().map(|(start_pos, texts)| {
                    let emb = Arc::clone(embedder);
                    let n = texts.len();
                    async move {
                        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                        (start_pos, n, emb.embed_batch(&refs).await)
                    }
                });
                futures::stream::iter(iter)
                    .buffered(inflight)
                    .collect()
                    .await
            };

            for (start_pos, expected_n, vecs) in wave_results {
                let batch_vecs = vecs.context("batch embed_batch failed")?;
                if batch_vecs.len() != expected_n {
                    anyhow::bail!(
                        "embed_batch returned {} vectors, expected {}",
                        batch_vecs.len(),
                        expected_n
                    );
                }
                for (offset, vec) in batch_vecs.into_iter().enumerate() {
                    embeddings[start_pos + offset] = Some(vec);
                }
            }

            // Fine-grained progress notification: fire once per wave when
            // ≥ PROGRESS_CHUNK_INTERVAL chunks were embedded. Lets the reindex
            // orchestrator emit chunk_progress SSE events at ~32-chunk granularity
            // rather than once per 128-file batch.
            let chunks_in_wave = wave_pos - wave_start;
            if let Some(tx) = progress_tx {
                if chunks_in_wave >= PROGRESS_CHUNK_INTERVAL {
                    let wave_ms = wave_embed_start.elapsed().as_millis() as u64;
                    let _ = tx.send((chunks_in_wave, wave_ms));
                }
            }

            // CoreML RSS tripwire: halve batch_size if spike detected.
            if is_coreml && !tripwire_fired && rss_before > 0 {
                let rss_after = current_rss_mb();
                let delta_mb = rss_after.saturating_sub(rss_before);
                if delta_mb > tripwire_mb {
                    let new_size =
                        (batch_size / 2).max(crate::core::memory_policy::COREML_BATCH_SIZE_MIN);
                    tracing::warn!(
                        "embed_chunks_in_batches: CoreML RSS delta {}MB exceeds tripwire \
                         {}MB after wave of {} chunks (inflight={}) — halving batch size \
                         {} → {} for remaining waves (non-fatal, reindex continues)",
                        delta_mb,
                        tripwire_mb,
                        wave_pos - wave_start,
                        inflight,
                        batch_size,
                        new_size,
                    );
                    batch_size = new_size;
                    tripwire_fired = true;
                }
            }

            batch_start = wave_pos;
        }
        Ok(embeddings)
    }

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
        // Rehydrate an idle-evicted map before the cap check / insert below so
        // the in-memory corpus stays consistent with the durable redb store,
        // and mark the index active so eviction won't race this commit.
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

        // Issue #N (180GB RSS fix): enforce the per-index chunk cap BEFORE
        // ingesting anything into BM25, HNSW, or the embedding cache.
        //
        // Why: previously `commit_corpus` was the only place that honoured the
        // cap. Chunks that were dropped from the corpus map still leaked into:
        //   - the HNSW vector store (via `commit_vectors_batch`)
        //   - the BM25 posting list (via `commit_bm25_batch`)
        //   - the chunk_embeddings LRU (via `commit_embeddings_cache`)
        // So on an over-cap repo, three structures grew unbounded while the
        // corpus map looked "capped". Pre-filtering here keeps every in-memory
        // structure consistent with the configured cap. Brand-new ids past the
        // cap are dropped; updates to existing ids are always allowed (they
        // don't grow the corpus).
        //
        // This is the structural fix for issue #82 — chunks dropped here never
        // allocate downstream, so RSS stays bounded by `TRUSTY_MAX_CHUNKS`.
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
                // Rebuild chunks/embeddings in place, dropping over-cap entries
                // so they never reach the downstream structures.
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
        // Snapshot the chunks for the durable redb write BEFORE `commit_corpus`
        // drains `all_chunks` into the in-memory map. `commit_corpus` consumes
        // its input via `drain`, so we clone here once; the redb write is then
        // independent of the corpus map. Skipped entirely when no `CorpusStore`
        // is wired (test / BM25-only indexers).
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

        // Issue #85 — fire-and-forget incremental persistence. Issue #29:
        // throttled — `spawn_incremental_persist(false)` only actually spawns
        // the HNSW snapshot every `HNSW_SNAPSHOT_BATCH_INTERVAL` batches. The
        // chunk corpus is already persisted transactionally per batch by
        // `commit_corpus_to_redb`, so a crash between snapshots loses only the
        // last ≤15 batches' HNSW vectors (re-embedded by the next reindex),
        // never committed chunks. The reindex orchestrator calls
        // `force_incremental_persist` after its batch loop to guarantee the
        // final HNSW state is durable.
        //
        // Why background: `Index::save` can take 100s of ms on a large
        // corpus and we don't want the commit path (which is on the hot
        // reindex loop) to wait on filesystem I/O. We don't hold any locks
        // while spawning — the clones are cheap (Arc bumps + a path string).
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
    /// Why: drops 3N lock acquisitions to 3 for a batch of N chunks (key
    /// alloc, key rev-map, HNSW write). Also guards against NaN / zero vectors
    /// (issue #764) — inserting them silently poisons the HNSW graph so every
    /// subsequent cosine-similarity search returns 0.0 for the affected
    /// neighbours, making the whole semantic lane appear dead.
    /// What: filters chunks without embeddings (BM25-only mode), validates
    /// each vector for NaN/all-zero content, then delegates to
    /// `store.upsert_batch`. No-op when no store is wired or no embeddings
    /// were computed.
    /// Test: `nan_vector_rejected_loudly` and `zero_vector_rejected_loudly`
    /// in `tests.rs`; `test_index_files_batch_*` covers the healthy path.
    async fn commit_vectors_batch(
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
            // Issue #764: reject NaN vectors loudly. A NaN in any component
            // propagates through HNSW distance computations and can silently
            // corrupt the graph. Logging a warning per-chunk is acceptable
            // (these should never occur from a healthy embedder; if they do,
            // the operator needs to see them). Skipping rather than aborting
            // the whole batch preserves BM25 coverage for the other chunks.
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
            // Issue #764: reject all-zero vectors. A zero vector has undefined
            // cosine similarity and produces misleading 0.0 distances for
            // everything. Legitimate embeddings are never all-zero; this is a
            // sign of a failed batch that slipped through the zero-count gate
            // (e.g. partial NaN-to-zero coercion in the sidecar).
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
    /// Why: doing this **before** moving chunks into the corpus avoids a
    /// second clone of each chunk.
    /// What: holds the BM25 write lock once and walks `chunks` to upsert
    /// `body + virtual_terms` for each.
    /// Test: BM25 search correctness is covered by every search test.
    async fn commit_bm25_batch(&self, chunks: &[RawChunk]) {
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
    async fn commit_embeddings_cache(
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
                // LRU `put` evicts the oldest entry when over capacity. Cache
                // eviction here is harmless: MMR rerank treats a missing entry
                // as zero diversity contribution.
                emb_cache.put(chunk.id.clone(), vec);
            }
        }
    }

    /// Drain `chunks` into the corpus under a single write lock.
    ///
    /// Why: single-lock insertion shrinks the write-lock window to
    /// milliseconds even for large batches.
    /// What: consumes `chunks` via `drain` so callers don't keep a stale
    /// copy after the corpus owns each one. Honours `max_chunks_per_index()`
    /// (issue #75): once the cap is reached new chunk ids are dropped (warned)
    /// while existing ids continue to be upserted.
    /// Test: covered indirectly by every search test.
    async fn commit_corpus(&self, chunks: &mut Vec<RawChunk>) {
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
    /// O(corpus). The redb write runs on `spawn_blocking` because redb's
    /// transaction API is synchronous and a large batch's `serde_json` encode
    /// plus fsync would otherwise pin a tokio worker thread. Issue #29: chunks
    /// and entities are now written via `CorpusStore::upsert_batch` in a
    /// **single** redb transaction, so a crash between the two never leaves the
    /// chunk corpus and the entity table inconsistent.
    /// What: clones the chunks plus entities (cheap relative to the JSON
    /// encode), moves them onto a blocking worker, and writes both tables in
    /// one atomic transaction. Failures are logged at `warn` and swallowed —
    /// persistence is a durability backup, so a transient I/O error must not
    /// abort the in-memory commit (the next batch's write, or shutdown flush,
    /// will re-converge the on-disk state).
    /// Test: `tests::test_corpus_store_roundtrip`.
    async fn commit_corpus_to_redb(
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
    /// Why: factored so the early-return path (empty batch) and the main
    /// commit path share one implementation.
    /// What: holds the entities write lock once and inserts every tuple.
    /// Test: covered indirectly by `test_entity_exact_match_*`.
    async fn commit_entities(&self, entities_by_file: Vec<(String, Vec<RawEntity>)>) {
        let mut emap = self.entities.write().await;
        for (path, ents) in entities_by_file {
            emap.insert(path, ents);
        }
    }

    /// Number of chunks currently held in the corpus.
    pub fn chunk_count(&self) -> usize {
        // blocking_read is fine on a tokio worker thread for a quick stat probe;
        // we never await across this call.
        self.chunks.try_read().map(|g| g.len()).unwrap_or(0)
    }

    /// Snapshot the current symbol graph. Cheap (`Arc::clone`); intended for
    /// read-only KG queries from concurrent search handlers.
    pub async fn symbol_graph(&self) -> Arc<SymbolGraph> {
        Arc::clone(&*self.symbol_graph.read().await)
    }

    /// Borrow the durable redb corpus (issue #41 phase 4).
    ///
    /// Why: Exposes the `Arc<CorpusStore>` to callers (e.g. `server.rs`) that
    /// need direct access to the on-disk chunk + symbol tables without holding
    /// the indexer's internal `RwLock`.
    /// What: Returns `None` for BM25-only / test indexers that have no
    /// on-disk corpus wired; `Some(Arc::clone)` otherwise.
    /// Test: covered indirectly by the search integration tests and the
    /// `get_call_chain` / `search_kg` provenance-navigation paths.
    pub fn corpus_arc(&self) -> Option<Arc<crate::core::corpus::CorpusStore>> {
        self.corpus.as_ref().map(Arc::clone)
    }

    /// Pre-warm the embedder by sending a trivial single-text batch.
    ///
    /// Why: Issue #744. The sidecar (`trusty-embedderd`) is lazy-spawned on the
    /// first embedding request (issue #315). On Apple Silicon / ONNX the cold
    /// spawn + CoreML session init takes 30 to 60 seconds, stalling the first
    /// real batch. Calling `warm_embedder` concurrently with the file walk lets
    /// model-load overlap with chunking instead of serialising against it.
    ///
    /// What: if `self.embedder` is wired, calls `embed_batch(&["warm"])` to
    /// trigger the lazy spawn and ONNX session init. The result is discarded.
    /// On error the failure is logged at debug level and ignored; the first
    /// real batch will retry via the normal path. A `None` embedder is a no-op.
    ///
    /// Test: the live ONNX path is `#[ignore]`-gated; unit-level correctness
    /// is covered by `warm_embedder_noop_without_embedder`.
    pub async fn warm_embedder(&self) {
        let Some(embedder) = &self.embedder else {
            return;
        };
        // A single short string is the minimal valid batch. We only care about
        // triggering the ONNX session init (lazy-spawn + CoreML/CUDA compile),
        // not the returned embedding — it is immediately discarded.
        match embedder.embed_batch(&["warm"]).await {
            Ok(_) => {
                tracing::debug!(
                    "warm_embedder[{}]: embedder pre-warm succeeded",
                    self.index_id
                );
            }
            Err(e) => {
                // Non-fatal: the first real batch will retry.
                tracing::debug!(
                    "warm_embedder[{}]: embedder pre-warm failed ({e}) — \
                     will retry on first batch",
                    self.index_id
                );
            }
        }
    }

    /// Embed all corpus chunks and upsert vectors into HNSW (issue #923 C2 pass).
    ///
    /// Why: fast pass (C1) stored chunks without embedding; this catch-up job
    /// fills the semantic lane. `progress_tx` is forwarded to
    /// `embed_chunks_in_batches` so callers can update `stages.semantic.embedded`
    /// per wave for live N/total progress on `/indexes/:id/status` (issue #929).
    /// What: snapshots chunks, embeds in batches, commits vectors + cache. Idempotent.
    /// Test: `deferred_embed_pass_marks_semantic_ready_and_is_idempotent`.
    pub async fn embed_deferred_chunks(
        &self,
        progress_tx: Option<&tokio::sync::mpsc::UnboundedSender<(usize, u64)>>,
    ) -> Result<(usize, usize)> {
        let chunks: Vec<RawChunk> = {
            self.ensure_chunks_loaded().await;
            let map = self.chunks.read().await;
            map.values().cloned().collect()
        };
        let total = chunks.len();
        if total == 0 || self.embedder.is_none() || self.store.is_none() {
            return Ok((0, total));
        }
        let embeddings = self.embed_chunks_in_batches(&chunks, progress_tx).await?;
        self.commit_vectors_batch(&chunks, &embeddings).await?;
        // In-memory embedding cache: subsequent MMR re-rank skips HNSW.
        self.commit_embeddings_cache(&chunks, embeddings).await;
        Ok((chunks.len(), total))
    }
}

#[cfg(test)]
mod warm_embedder_tests {
    use super::super::CodeIndexer;

    /// `warm_embedder` on an indexer with no embedder must be a no-op (no
    /// panic, no hang).
    ///
    /// Why: Issue #744 — `warm_embedder` is called as a concurrent background
    /// task for every non-lexical reindex. On test indexers (no embedder wired)
    /// it must return immediately without error.
    /// What: calls `warm_embedder` on a bare `CodeIndexer`; asserts the call
    /// completes without panic.
    /// Test: this test.
    #[tokio::test]
    async fn warm_embedder_noop_without_embedder() {
        let indexer = CodeIndexer::new("warm-test", "/tmp");
        // Must return immediately — no embedder is wired, so warm_embedder is a no-op.
        indexer.warm_embedder().await;
        // If we reach here, the no-op path worked correctly.
    }
}

#[cfg(test)]
mod tripwire_tests {
    use super::current_rss_mb;

    /// `current_rss_mb` must never panic and must return a plausible value.
    ///
    /// Why: the CoreML tripwire depends on this probe. It is intentionally
    /// non-fatal — a failed probe returns 0 and the tripwire simply never
    /// fires — so the only hard guarantee is "no panic, plausible range".
    /// What: on macOS/Linux the live test process has a non-zero RSS, so the
    /// reading should be > 0 and below an implausible 1 TB ceiling. On other
    /// platforms the function returns 0 by design.
    #[test]
    fn test_current_rss_mb_is_plausible() {
        let rss = current_rss_mb();
        // Sanity ceiling: no test host has a 1 TB resident set.
        assert!(rss < 1024 * 1024, "current_rss_mb implausibly large: {rss}");
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            // The running test process must occupy some resident memory.
            assert!(rss > 0, "current_rss_mb should be > 0 on macOS/Linux");
        }
    }
}

#[cfg(test)]
mod progress_interval_tests {
    use super::PROGRESS_CHUNK_INTERVAL;

    /// `PROGRESS_CHUNK_INTERVAL` must equal 32 so the embed bar advances at the
    /// documented granularity.
    ///
    /// Why: the constant is the contract between `embed_chunks_in_batches` and
    /// the reindex orchestrator — if it drifts the bar coarsens again silently.
    /// What: asserts the value is exactly 32.
    /// Test: this test.
    #[test]
    fn progress_interval_constant_is_32() {
        assert_eq!(PROGRESS_CHUNK_INTERVAL, 32);
    }
}
