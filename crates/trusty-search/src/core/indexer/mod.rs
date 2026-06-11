//! `CodeIndexer`: hybrid HNSW + BM25 + RRF search pipeline.
//!
//! Why: this is the central orchestrator that ties embeddings, vector search,
//! lexical search, and intent-based weight routing into a single `search()` call.
//! What: holds an `Embedder`, a `VectorStore`, and an in-memory chunk corpus;
//! `search()` runs both lanes in parallel, fuses with RRF, and returns the
//! top-k chunks with their fused score and per-result `match_reason`.
//! Test: see the `tests` submodule — RRF unit coverage lives in `search::rrf`,
//! and the integration test `test_search_integration` indexes 3 chunks and
//! verifies the most-relevant one ranks first.
//!
//! Module layout (issue #96 — god-object split):
//!   * `mod.rs` (this file): types, free helpers, struct definition, constructors.
//!   * `helpers`: env readers, codec helpers, and score-adjustment free functions.
//!   * `ingest`: add/index/batch parse+embed/commit pipeline.
//!   * `persist`: snapshot/restore + background incremental persist.
//!   * `files`: remove + lookup + entity-exact-match helpers.
//!   * `search`: hybrid query pipeline (HNSW + BM25 + RRF + KG + MMR).
//!   * `tests`: every test in one place so private fields stay accessible.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lru::LruCache;
use tokio::sync::RwLock;

use crate::core::bm25::Bm25Index;
use crate::core::chunker::RawChunk;
use crate::core::embed::Embedder;
use crate::core::entity::RawEntity;
use crate::core::store::VectorStore;
use crate::core::symbol_graph::SymbolGraph;

pub(crate) mod archive;
pub(crate) mod docs_penalty;
mod files;
pub(crate) mod helpers;
mod ingest;
pub(crate) mod migrations;
mod persist;
mod persist_hnsw;
mod search;
mod types;

/// Re-export for the reindex orchestrator's progress-interval gate.
pub(crate) use ingest::PROGRESS_CHUNK_INTERVAL;
#[cfg(test)]
pub(crate) use search::KG_REFINE_THRESHOLD;
#[cfg(test)]
mod tests;

// Re-export helpers so sibling modules can use the crate-internal API.
pub(crate) use helpers::{
    build_compact_snippet, definition_boost_query_tokens, embed_batch_size, embedding_cache_cap,
    file_type_score_multiplier, hash_query, idle_evict_secs, is_function_definition_chunk_type,
    is_struct_definition_chunk_type, max_chunks_per_index, populate_virtual_terms,
    raw_to_code_chunk, STRUCT_DEFINITION_BOOST,
};
// Exposed for unit tests in `tests.rs`.
#[cfg(test)]
pub(crate) use helpers::{
    compute_match_reason, resolve_chunk_file, DEFAULT_CHUNKS_IDLE_EVICT_SECS,
};

// Re-export types so callers outside this module see the same paths.
pub(crate) use types::ChunkSnapshot;
pub use types::{CodeChunk, CommitTimings, ParsedBatch, SearchMode, SearchQuery, SearchStage};

/// LRU capacity (entries) for the per-indexer query embedding cache.
const QUERY_CACHE_CAPACITY: usize = 256;
/// Oversample factor for the HNSW lane before RRF fusion.
pub(crate) const HNSW_OVERSAMPLE: usize = 4;

/// Legacy KG-expand score multiplier (doc only — pipeline now uses `EdgeKind::score_multiplier`).
/// Tests still reference this when validating the `CallsFunction` baseline (issue #18).
#[allow(dead_code)]
pub(crate) const KG_EXPAND_SCORE_FACTOR: f32 = 0.7;
/// Default BFS depth for KG expansion (1 hop = direct callers/callees only).
pub(crate) const KG_EXPAND_HOPS: usize = 1;

/// How many committed batches must elapse between background HNSW snapshots
/// (issue #29).
///
/// Why: `spawn_incremental_persist` used to fire after *every* committed
/// batch. The chunk corpus is already persisted transactionally per batch by
/// `commit_corpus_to_redb`, so the per-batch work that actually mattered for
/// crash-safety was the redb write — the HNSW `Index::save` is a pure backup
/// that takes hundreds of ms on a large graph. On a 14k-file reindex (128
/// files/batch → ~110 batches) that was ~110 full graph saves; throttling to
/// one every 16 batches cuts that to ~7 (plus one forced save at reindex
/// completion), reclaiming ~15+ seconds of redundant I/O without weakening
/// durability.
/// What: the batch-count modulus used by `spawn_incremental_persist`.
/// Test: `tests::test_incremental_persist_throttles_to_interval`.
pub(crate) const HNSW_SNAPSHOT_BATCH_INTERVAL: u32 = 16;

/// `CodeIndexer`: hybrid search engine for one named index.
///
/// Why: central orchestrator for the hybrid HNSW + BM25 + RRF pipeline.
/// Fields are crate-visible so the submodule `impl` blocks (`ingest`,
/// `persist`, `files`, `search`) can mutate state without going through
/// accessors.
/// What: holds all shared state (embedder, vector store, BM25, symbol graph,
/// corpus) and delegates to focused submodules for each concern.
/// Test: see `tests` submodule; integration tests in `tests/integration_tests.rs`.
pub struct CodeIndexer {
    pub index_id: String,
    pub root_path: std::path::PathBuf,

    pub(super) embedder: Option<Arc<dyn Embedder>>,
    pub(super) store: Option<Arc<dyn VectorStore>>,

    /// In-memory chunk corpus. Write-through cache of the redb `CHUNKS_TABLE`.
    pub(super) chunks: Arc<RwLock<HashMap<String, RawChunk>>>,

    /// Durable redb-backed chunk corpus (issue #28). `None` for BM25-only or
    /// test indexers built without a data dir.
    pub(super) corpus: Option<Arc<crate::core::corpus::CorpusStore>>,

    /// Per-file entities extracted by `chunk_ast`.
    pub(super) entities: Arc<RwLock<HashMap<String, Vec<RawEntity>>>>,

    /// Cached chunk embeddings, keyed by `chunk_id`. Bounded by
    /// `embedding_cache_cap()`.
    pub(super) chunk_embeddings: Arc<RwLock<LruCache<String, Vec<f32>>>>,

    /// Persistent BM25 index kept hot alongside the HNSW index.
    pub(super) bm25: Arc<RwLock<Bm25Index>>,

    /// LRU cache of query → embedding, keyed by `hash_query`.
    pub(super) query_cache: Arc<Mutex<LruCache<u64, Vec<f32>>>>,

    /// Call graph derived from the chunk corpus.
    pub(super) symbol_graph: Arc<RwLock<Arc<SymbolGraph>>>,

    /// Optional ONNX NER for `NaturalLanguagePhrase` extraction.
    pub(super) ner: crate::core::ner::NerExtractor,

    /// Coalescing state for `spawn_incremental_persist`.
    pub(super) persist_state: Arc<PersistState>,

    /// Per-index domain vocabulary used by `QueryClassifier::classify_with_domain`.
    pub(super) domain_terms: Vec<String>,

    /// Process-relative clock base for [`Self::last_activity_ms`].
    pub(super) created_at: Instant,

    /// Milliseconds (relative to [`Self::created_at`]) of the most recent
    /// query or ingest activity.
    pub(super) last_activity_ms: Arc<AtomicU64>,

    /// `true` once the in-memory `chunks` map has been evicted.
    pub(super) chunks_evicted: Arc<AtomicBool>,
}

/// Coalescing state for `spawn_incremental_persist`.
///
/// Why: prior to this guard, every call to `commit_parsed_batch` spawned a
/// fire-and-forget tokio task that cloned the **entire** chunk corpus into a
/// `Vec<RawChunk>` and serialized it to JSON. On a 200k-chunk corpus that's
/// ~400 MB of `Vec<RawChunk>` plus another ~800 MB of serialized `Vec<u8>`
/// per task. A reindex emits one commit per 128 files, so a 76 800-file repo
/// would stack ~600 of these tasks.
/// What: `in_flight` guarantees only one persist task is alive at a time;
/// `dirty` lets later commits coalesce.
/// Test: `tests::test_persist_coalesces_concurrent_calls`.
#[derive(Debug, Default)]
pub(crate) struct PersistState {
    pub(crate) in_flight: AtomicBool,
    pub(crate) dirty: AtomicBool,
    /// Monotonic count of committed batches that have requested a persist
    /// (issue #29). Only every [`HNSW_SNAPSHOT_BATCH_INTERVAL`] batches is
    /// the HNSW snapshot actually spawned.
    pub(crate) batch_counter: AtomicU32,
}

impl CodeIndexer {
    /// Construct a bare indexer without an embedder/store. Call
    /// [`Self::with_components`] before invoking [`Self::search`] — otherwise
    /// search returns `Ok(vec![])` (BM25-only fallback uses the same path).
    ///
    /// Why: many call sites (tests, warm-boot, staging) build an indexer
    /// incrementally before attaching all components.
    /// What: initialises all fields to their zero/empty states.
    /// Test: every test that constructs a `CodeIndexer` exercises this.
    pub fn new(index_id: impl Into<String>, root_path: impl Into<std::path::PathBuf>) -> Self {
        let cap =
            NonZeroUsize::new(QUERY_CACHE_CAPACITY).expect("QUERY_CACHE_CAPACITY must be non-zero");
        let emb_cap = NonZeroUsize::new(embedding_cache_cap())
            .expect("embedding_cache_cap must be non-zero (env var filtered)");
        Self {
            index_id: index_id.into(),
            root_path: root_path.into(),
            embedder: None,
            store: None,
            corpus: None,
            chunks: Arc::new(RwLock::new(HashMap::new())),
            entities: Arc::new(RwLock::new(HashMap::new())),
            chunk_embeddings: Arc::new(RwLock::new(LruCache::new(emb_cap))),
            bm25: Arc::new(RwLock::new(Bm25Index::new())),
            query_cache: Arc::new(Mutex::new(LruCache::new(cap))),
            symbol_graph: Arc::new(RwLock::new(Arc::new(SymbolGraph::new()))),
            ner: crate::core::ner::NerExtractor::try_load(),
            persist_state: Arc::new(PersistState::default()),
            domain_terms: Vec::new(),
            created_at: Instant::now(),
            last_activity_ms: Arc::new(AtomicU64::new(0)),
            chunks_evicted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Record that this index was just queried or ingested.
    ///
    /// Why: the idle-eviction ticker needs a cheap, lock-free "when was this
    /// index last touched?" signal.
    /// What: stores the milliseconds elapsed since [`Self::created_at`] into
    /// [`Self::last_activity_ms`] with `Relaxed` ordering.
    /// Test: `idle_eviction_drops_and_lazily_rehydrates_chunks` touches then
    /// asserts eviction is skipped within the window.
    pub(super) fn touch_activity(&self) {
        let ms = self.created_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
        self.last_activity_ms.store(ms, Ordering::Relaxed);
    }

    /// Milliseconds since the last recorded activity (query/ingest).
    ///
    /// Why: lets the eviction logic compare elapsed idle time against the
    /// configured window without exposing the raw atomic.
    /// What: `created_at.elapsed() - last_activity_ms`, floored at 0.
    /// Test: covered via `evict_chunks_if_idle`'s behaviour tests.
    fn idle_duration(&self) -> std::time::Duration {
        let now_ms = self.created_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let last = self.last_activity_ms.load(Ordering::Relaxed);
        std::time::Duration::from_millis(now_ms.saturating_sub(last))
    }

    /// Number of chunks currently resident in the in-memory `chunks` map.
    ///
    /// Why: tests and the eviction ticker want a direct read of the in-memory
    /// footprint.
    /// What: returns `self.chunks.read().len()`.
    /// Test: `idle_eviction_drops_and_lazily_rehydrates_chunks`.
    pub async fn in_memory_chunk_count(&self) -> usize {
        self.chunks.read().await.len()
    }

    /// Drop the in-memory `chunks` map when the index has been idle longer
    /// than `idle_threshold` and a durable corpus can repopulate it.
    ///
    /// Why: see `DEFAULT_CHUNKS_IDLE_EVICT_SECS` — the raw chunk-text map is
    /// the single largest idle-heap contributor per index and is unused on the
    /// query hot path once a redb corpus is wired.
    /// What: a no-op when idle_threshold is zero, no durable corpus is wired,
    /// the map is already empty, or the index was recently active. Otherwise
    /// clears the map, marks `chunks_evicted`, and logs an `info` with the
    /// reclaimed count. BM25 and the symbol graph are intentionally left hot.
    /// Returns the number of chunks evicted (0 when skipped).
    /// Test: `idle_eviction_drops_and_lazily_rehydrates_chunks`.
    pub async fn evict_chunks_if_idle(&self, idle_threshold: std::time::Duration) -> usize {
        if idle_threshold.is_zero() {
            return 0;
        }
        if self.corpus.is_none() {
            return 0;
        }
        if self.idle_duration() < idle_threshold {
            return 0;
        }
        let mut chunks = self.chunks.write().await;
        if chunks.is_empty() {
            return 0;
        }
        let evicted = chunks.len();
        chunks.clear();
        chunks.shrink_to_fit();
        drop(chunks);
        self.chunks_evicted.store(true, Ordering::Relaxed);
        tracing::info!(
            "index '{}': evicted {} in-memory chunks after {}s idle \
             (durable corpus retained; lazily rehydrates on next access)",
            self.index_id,
            evicted,
            idle_threshold.as_secs(),
        );
        evicted
    }

    /// Repopulate the in-memory `chunks` map from the durable corpus if it was
    /// previously evicted while idle.
    ///
    /// Why: the in-memory readers must observe a populated map; after an idle
    /// eviction the map is empty and `chunks_evicted` is set.
    /// What: a fast no-op (single relaxed atomic load) when the map was never
    /// evicted. When evicted, reloads every chunk from `CorpusStore` on a
    /// blocking worker and refills the map, then clears the flag.
    /// Test: `idle_eviction_drops_and_lazily_rehydrates_chunks`.
    pub(super) async fn ensure_chunks_loaded(&self) {
        if !self.chunks_evicted.load(Ordering::Relaxed) {
            return;
        }
        let Some(corpus) = self.corpus.clone() else {
            self.chunks_evicted.store(false, Ordering::Relaxed);
            return;
        };
        let index_id = self.index_id.clone();
        let loaded = tokio::task::spawn_blocking(move || corpus.load_all_chunks()).await;
        match loaded {
            Ok(Ok(chunks)) => {
                let n = chunks.len();
                let mut map = self.chunks.write().await;
                for chunk in chunks {
                    map.insert(chunk.id.clone(), chunk);
                }
                drop(map);
                self.chunks_evicted.store(false, Ordering::Relaxed);
                tracing::info!(
                    "index '{index_id}': rehydrated {n} chunks from redb after idle eviction"
                );
            }
            Ok(Err(e)) => tracing::warn!(
                "index '{index_id}': failed to rehydrate chunks from redb ({e}); \
                 will retry on next access"
            ),
            Err(e) => tracing::warn!(
                "index '{index_id}': chunk rehydration task panicked ({e}); \
                 will retry on next access"
            ),
        }
    }

    /// Builder-style setter for the per-index domain vocabulary.
    ///
    /// Why: lets the daemon attach `trusty-search.yaml`'s `domain_terms:`
    /// without leaking the field into every constructor call site.
    /// What: stores the vector verbatim.
    /// Test: see `tests::search_uses_domain_terms_when_provided`.
    pub fn with_domain_terms(mut self, terms: Vec<String>) -> Self {
        self.domain_terms = terms;
        self
    }

    /// Replace the per-index domain vocabulary in place.
    pub fn set_domain_terms(&mut self, terms: Vec<String>) {
        self.domain_terms = terms;
    }

    /// Returns a cheap `Arc` snapshot of the current symbol graph.
    ///
    /// Why: the `GET /indexes/{id}/graph` endpoint needs to read the whole
    /// symbol graph without holding a lock.
    /// What: clones the inner `Arc<SymbolGraph>` while holding the read lock.
    /// Test: covered by the `graph_handler` integration path.
    pub async fn snapshot_symbol_graph(&self) -> Arc<SymbolGraph> {
        Arc::clone(&*self.symbol_graph.read().await)
    }

    /// Borrow the (optional) durable corpus store.
    ///
    /// Why: the reindex orchestrator and symbol-graph paths need the
    /// `CorpusStore` without exposing every internal field.
    /// What: returns `Some(Arc::clone)` when the corpus is wired, `None` for
    /// BM25-only / test indexers.
    /// Test: indirectly via `service::reindex` graph rebuild trigger.
    pub fn corpus_store(&self) -> Option<Arc<crate::core::corpus::CorpusStore>> {
        self.corpus.as_ref().map(Arc::clone)
    }

    /// Attach the embedder and vector store so the full hybrid pipeline can run.
    /// Builder-style; returns `self` for chaining.
    pub fn with_components(
        mut self,
        embedder: Arc<dyn Embedder>,
        store: Arc<dyn VectorStore>,
    ) -> Self {
        self.embedder = Some(embedder);
        self.store = Some(store);
        self
    }

    /// Attach a durable redb-backed [`crate::core::corpus::CorpusStore`]
    /// (issue #28).
    ///
    /// Why: the daemon resolves one `index.redb` per index and wires it in
    /// before warm-boot.
    /// What: stores the `Arc` so both the ingest commit path and the
    /// fire-and-forget persist task can reach it.
    /// Test: `tests::test_corpus_store_roundtrip`.
    pub fn set_corpus_store(&mut self, corpus: Arc<crate::core::corpus::CorpusStore>) {
        self.corpus = Some(corpus);
    }

    /// Swap in a new durable corpus store, returning the one it replaced
    /// (issue #28, Phase 4).
    ///
    /// Why: a `--force` reindex stages its rebuilt corpus in a temp file.
    /// What: replaces `self.corpus` with `Some(corpus)` and returns the prior
    /// value.
    /// Test: `tests::test_force_reindex_atomic_corpus_swap`.
    pub fn swap_corpus_store(
        &mut self,
        corpus: Arc<crate::core::corpus::CorpusStore>,
    ) -> Option<Arc<crate::core::corpus::CorpusStore>> {
        self.corpus.replace(corpus)
    }

    /// Take the durable corpus store out of the indexer, leaving `None`
    /// (issue #28, Phase 4).
    ///
    /// Why: to atomically rename the staging corpus file over the live one,
    /// every `Arc<CorpusStore>` clone pointing at either file must first be
    /// dropped.
    /// What: `Option::take` on `self.corpus`.
    /// Test: `tests::test_force_reindex_atomic_corpus_swap`.
    pub fn take_corpus_store(&mut self) -> Option<Arc<crate::core::corpus::CorpusStore>> {
        self.corpus.take()
    }

    /// True iff a durable corpus store is currently wired (issue #28).
    ///
    /// Why: the `--force` reindex orchestrator only performs the atomic
    /// staging-file swap when the index actually has a durable corpus.
    /// What: `self.corpus.is_some()`.
    /// Test: covered by `tests::test_force_reindex_atomic_corpus_swap`.
    pub fn has_corpus_store(&self) -> bool {
        self.corpus.is_some()
    }

    /// Whether this indexer has an embedder wired (issue #601).
    ///
    /// Why: the reindex non-empty gate must distinguish "no embedder configured"
    /// from "embedder present but produced zero vectors".
    /// What: `self.embedder.is_some()`.
    /// Test: `validate::reindex_outcome` unit tests model both branches.
    pub fn has_embedder(&self) -> bool {
        self.embedder.is_some()
    }
}
