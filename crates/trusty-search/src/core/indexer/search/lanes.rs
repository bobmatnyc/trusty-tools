//! Per-lane fetch and query helpers for [`CodeIndexer`].
//!
//! Why: extracted from `search/mod.rs` (issue #607) so the parent file stays
//! under the 500-SLOC hard cap. These stateless helpers — embedding, BM25,
//! HNSW, grep-fallback, and chunk-fetch — are cleanly separable from the
//! orchestration and KG expansion logic.
//! What: `fetch_chunks_for_ids`, `get_embedding`, `embed_text`, `embed_query`,
//! `bm25_search`, `grep_fallback_search`, `vector_search`, and
//! `edge_kinds_for_intent`.
//! Test: covered by every `test_search_*` and `test_kg_*` integration test
//! in `indexer::tests`.

use anyhow::{Context, Result};

use crate::core::classifier::QueryIntent;
use crate::core::entity::EdgeKind;

use super::super::{hash_query, CodeIndexer};

impl CodeIndexer {
    /// Batch-fetch the `RawChunk`s for a set of chunk ids, reading from the
    /// durable redb corpus when one is wired and falling back to the in-memory
    /// `chunks` HashMap otherwise.
    ///
    /// Why: the query hot path used to join fused `(id, score)` pairs against
    /// the in-memory `chunks` HashMap, keeping every chunk's text resident
    /// (~45 GB RSS on a large monorepo). Reading top-k chunk text from redb at
    /// materialisation time serves bytes from the OS page cache, dropping
    /// steady-state RSS to <10 GB.
    /// What: when `self.corpus` is `Some`, runs `CorpusStore::get_chunks` on a
    /// blocking worker and returns the result keyed by id. When `self.corpus` is
    /// `None`, falls back to cloning the requested entries from the in-memory
    /// HashMap. Ids with no row are simply absent — the caller skips them with
    /// a `trace`.
    /// Test: covered by every `test_search_*` integration test.
    pub(super) async fn fetch_chunks_for_ids(
        &self,
        ids: &[String],
    ) -> std::collections::HashMap<String, crate::core::chunker::RawChunk> {
        if ids.is_empty() {
            return std::collections::HashMap::new();
        }
        if let Some(corpus) = self.corpus.clone() {
            let owned_ids = ids.to_vec();
            let index_id = self.index_id.clone();
            let read = tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = owned_ids.iter().map(String::as_str).collect();
                corpus.get_chunks(&refs)
            })
            .await;
            match read {
                Ok(Ok(chunks)) => {
                    return chunks.into_iter().map(|c| (c.id.clone(), c)).collect();
                }
                Ok(Err(e)) => tracing::warn!(
                    "index '{index_id}': redb point-read failed ({e}) — \
                     falling back to in-memory corpus for this query"
                ),
                Err(e) => tracing::warn!(
                    "index '{index_id}': redb point-read task panicked ({e}) — \
                     falling back to in-memory corpus for this query"
                ),
            }
        }
        // BM25-only / test indexer, or a redb read error: clone the requested
        // entries out of the in-memory HashMap.
        self.ensure_chunks_loaded().await;
        let chunks = self.chunks.read().await;
        ids.iter()
            .filter_map(|id| chunks.get(id).map(|c| (id.clone(), c.clone())))
            .collect()
    }

    /// Retrieve a cached chunk embedding by `chunk_id`.
    ///
    /// Why: code-to-code similarity search (issue #31) needs the seed chunk's
    /// embedding without re-embedding. We already populate `chunk_embeddings`
    /// on `add_chunk`, so this is an O(1) lookup.
    /// What: `peek` doesn't promote the entry — returns `None` when the chunk
    /// doesn't exist or was indexed in BM25-only mode.
    /// Test: covered by `test_get_embedding_returns_some_after_indexing`.
    pub fn get_embedding(&self, chunk_id: &str) -> Option<Vec<f32>> {
        self.chunk_embeddings
            .try_read()
            .ok()
            .and_then(|g| g.peek(chunk_id).cloned())
    }

    /// Embed an arbitrary text using the wired embedder, bypassing the
    /// query-LRU cache.
    ///
    /// Why: callers outside the search hot path (e.g. context-embedding
    /// generation in `service::context_inference`) need embeddings without
    /// polluting the query cache. Returns `None` when no embedder is wired.
    /// What: thin wrapper around `embedder.embed(text)`.
    /// Test: covered indirectly via the context-embedding integration test.
    pub async fn embed_text(&self, text: &str) -> Result<Option<Vec<f32>>> {
        let Some(embedder) = self.embedder.clone() else {
            return Ok(None);
        };
        let vec = embedder.embed(text).await.context("embed text")?;
        Ok(Some(vec))
    }

    /// Resolve a query → embedding, using the LRU cache to skip repeats.
    ///
    /// Why: search queries repeat across sessions; caching avoids repeated
    /// ONNX calls for the same text.
    /// What: hash the query, check the LRU, return cached vector if hit; else
    /// embed and store. Returns `None` when no embedder is wired.
    /// Test: covered indirectly by every search integration test.
    // pub(crate): also called from tests.rs (a sibling of `search/` in `indexer`).
    pub(crate) async fn embed_query(&self, query: &str) -> Result<Option<Vec<f32>>> {
        let Some(embedder) = self.embedder.clone() else {
            return Ok(None);
        };
        let key = hash_query(query);

        // Fast path: cache hit.
        if let Some(v) = self
            .query_cache
            .lock()
            .expect("query_cache mutex poisoned")
            .get(&key)
        {
            return Ok(Some(v.clone()));
        }

        let vec = embedder.embed(query).await.context("embed query")?;

        self.query_cache
            .lock()
            .expect("query_cache mutex poisoned")
            .put(key, vec.clone());

        Ok(Some(vec))
    }

    /// Run `query` against the hot, persistent BM25 index.
    ///
    /// Why: the previous implementation rebuilt the entire posting list on
    /// every search (~9.5s on a 115k-chunk index). The index is now maintained
    /// incrementally so the search hot path is just a read lock + posting walk.
    /// What: acquires the BM25 read lock, runs `score_query_all`.
    /// Test: BM25 results are covered by every search integration test.
    pub(super) async fn bm25_search(&self, query: &str, want: usize) -> Result<Vec<(String, f32)>> {
        let bm25 = self.bm25.read().await;
        if bm25.is_empty() {
            return Ok(Vec::new());
        }
        Ok(bm25.score_query_all(query, want))
    }

    /// Grep-fallback lane: scan in-memory chunk contents for a literal match
    /// of `query` (issue #75).
    ///
    /// Why: when the primary BM25 + vector lanes both return no rows (rare but
    /// real on small / unusual indexes), we want at least an exact-substring
    /// fallback before telling the caller "no results".
    /// What: builds a `regex::escape(query)` pattern, walks the in-memory chunk
    /// corpus, and collects up to `want` hits scored at `GREP_FALLBACK_SCORE`.
    /// Empty / regex-build failure short-circuits to `Vec::new()`.
    /// Test: `test_grep_fallback_returns_substring_hits` in `indexer::tests`.
    // pub(crate): also called from tests.rs (a sibling of `search/` in `indexer`).
    pub(crate) async fn grep_fallback_search(
        &self,
        query: &str,
        want: usize,
    ) -> Vec<(String, f32)> {
        if query.is_empty() || want == 0 {
            return Vec::new();
        }
        let Ok(re) = regex::Regex::new(&regex::escape(query)) else {
            return Vec::new();
        };
        // Rehydrate the in-memory corpus if it was evicted while idle.
        self.ensure_chunks_loaded().await;
        let chunks = self.chunks.read().await;
        let mut out: Vec<(String, f32)> = Vec::new();
        for raw in chunks.values() {
            if re.is_match(&raw.content) {
                out.push((raw.id.clone(), super::GREP_FALLBACK_SCORE));
                if out.len() >= want {
                    break;
                }
            }
        }
        out
    }

    /// Run the HNSW lane. Returns `(chunk_id, score)` in "higher = better"
    /// convention (the `VectorStore`'s score is `1 − cos_dist`).
    ///
    /// Why: RRF consumes only rank order, so the magnitude is informational;
    /// we preserve it so callers can display raw vector similarity if needed.
    /// What: delegates to `store.search`; returns empty when no store is wired.
    /// Test: covered by every vector-lane search integration test.
    pub(crate) async fn vector_search(
        &self,
        embedding: &[f32],
        want: usize,
    ) -> Result<Vec<(String, f32)>> {
        let Some(store) = &self.store else {
            return Ok(Vec::new());
        };
        let hits = store.search(embedding, want).await?;
        Ok(hits.into_iter().map(|h| (h.chunk_id, h.score)).collect())
    }

    /// Edge-kinds traversed for each query intent (issue #18).
    ///
    /// Why: each intent picks a small set of `EdgeKind`s most likely to surface
    /// adjacent code that is actually relevant to the question being asked.
    /// What: pattern-matches intent to a fixed `Vec<EdgeKind>`.
    /// Test: covered indirectly by every KG expansion test.
    pub(super) fn edge_kinds_for_intent(intent: QueryIntent) -> Vec<EdgeKind> {
        match intent {
            QueryIntent::Definition => {
                vec![EdgeKind::Implements, EdgeKind::Aliases, EdgeKind::UsesType]
            }
            QueryIntent::Usage => vec![
                EdgeKind::CallsFunction,
                EdgeKind::CalledByFunction,
                EdgeKind::TestedBy,
                EdgeKind::CoOccursInTest,
            ],
            QueryIntent::Conceptual => {
                vec![EdgeKind::ReferencesConcept, EdgeKind::Documents]
            }
            QueryIntent::BugDebt => vec![
                EdgeKind::RaisesError,
                EdgeKind::ErrorDescribes,
                EdgeKind::Configures,
            ],
            QueryIntent::Unknown => vec![EdgeKind::CallsFunction, EdgeKind::CalledByFunction],
        }
    }
}
