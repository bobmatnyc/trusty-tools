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

use std::sync::atomic::Ordering;

use anyhow::{Context, Result};

use crate::core::classifier::QueryIntent;
use crate::core::entity::EdgeKind;

use super::super::{hash_query, CodeIndexer, SearchQuery};
use super::path_filter;

/// Bound on the ensure-then-read retry loop in [`CodeIndexer::bm25_search`] /
/// [`CodeIndexer::grep_fallback_search`] (issue #2846 review).
///
/// Why: the memory-pressure ticker's high-water cadence is bounded well below
/// 1 Hz (`TRUSTY_MEMORY_ENFORCE_SECS`, default 30s), so a query lane racing
/// against a reclaim on every one of 3 consecutive attempts is not a real
/// steady-state scenario — this cap exists purely to convert a
/// theoretically-possible adversarial cadence into a graceful empty-result
/// degradation instead of an unbounded retry loop.
const REHYDRATE_RACE_RETRIES: u32 = 3;

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
    ///
    /// Why the retry loop (issue #2846 review — MEDIUM): `ensure_bm25_entities_loaded`
    /// and the `bm25.read()` below are two SEPARATE lock acquisitions, so a
    /// memory-pressure reclaim (`reclaim_memory_now`, which fires under active
    /// load by design — unlike idle-evict, which only ever raced this window
    /// after 60s of quiet) can land in between: the ensure-check observes BM25
    /// populated, then the reclaim clears it before this function's own read
    /// lock is acquired, and the query would silently lose its entire lexical
    /// lane. `clear_bm25_entities` only sets `bm25_entities_evicted` when it
    /// actually cleared a non-empty index, so observing `bm25.is_empty()` AND
    /// the flag set (under the SAME read guard that saw the empty state) is an
    /// unambiguous signal that a reclaim raced us since `ensure()` returned —
    /// as opposed to a genuinely-empty corpus, where the flag stays false and
    /// we return immediately. That case rehydrates and retries rather than
    /// silently degrading recall.
    /// What: rehydrates an idle-evicted or race-evicted BM25 corpus (issue
    /// #2162 / #2846), then acquires the BM25 read lock and runs
    /// `score_query_all` (or, when `filter` is set, `score_query_all_with_filter`
    /// — issue #3401: the path/repo scope predicate MUST be evaluated by BM25
    /// itself before its internal `top_k` truncation, not applied by the
    /// caller afterward, or a genuinely in-scope, lexically-matching document
    /// ranked outside the unscoped top `want` would already be gone by the
    /// time this function returns). Retries up to [`REHYDRATE_RACE_RETRIES`]
    /// times only when the race is detected; a genuinely empty BM25 index
    /// returns `Ok(vec![])` on the first pass with no retry.
    /// Test: BM25 results are covered by every search integration test;
    /// `bm25_search_survives_reclaim_race_between_ensure_and_read` in
    /// `indexer::tests_idle_evict` pins the race-detection retry itself;
    /// `test_path_prefix_filter_recovers_bm25_match_beyond_want` in
    /// `indexer::tests::path_filter_search` pins the pre-truncation filter.
    pub(super) async fn bm25_search(
        &self,
        query: &str,
        want: usize,
        filter: Option<&(dyn Fn(&str) -> bool + Send + Sync)>,
    ) -> Result<Vec<(String, f32)>> {
        for _ in 0..REHYDRATE_RACE_RETRIES {
            self.ensure_bm25_entities_loaded().await;
            let bm25 = self.bm25.read().await;
            if !bm25.is_empty() {
                return Ok(match filter {
                    Some(f) => bm25.score_query_all_with_filter(query, want, f),
                    None => bm25.score_query_all(query, want),
                });
            }
            if !self.bm25_entities_evicted.load(Ordering::Relaxed) {
                return Ok(Vec::new());
            }
            // A reclaim raced us since `ensure()` returned — drop the read
            // guard and loop back to rehydrate again.
        }
        // Exhausted retries: a reclaim landed on every attempt, which is not
        // reachable at the configured (>=30s) enforcement cadence. Degrade to
        // an empty lexical lane rather than loop forever; the next query
        // succeeds once the sweep settles.
        tracing::warn!(
            "index '{}': BM25 rehydrate raced by memory-pressure reclaim {REHYDRATE_RACE_RETRIES} \
             times in a row — returning empty lexical lane for this query",
            self.index_id
        );
        Ok(Vec::new())
    }

    /// Grep-fallback lane: scan in-memory chunk contents for a literal match
    /// of `query` (issue #75).
    ///
    /// Why: when the primary BM25 + vector lanes both return no rows (rare but
    /// real on small / unusual indexes), we want at least an exact-substring
    /// fallback before telling the caller "no results".
    ///
    /// Why the retry loop (issue #2846 review — MEDIUM): same race as
    /// [`Self::bm25_search`], applied to the `chunks` map / `chunks_evicted`
    /// flag instead of BM25 — `ensure_chunks_loaded` and `chunks.read()` below
    /// are separate lock acquisitions, so a memory-pressure reclaim can clear
    /// the map in between and this lane would silently scan zero chunks.
    /// What: builds a `regex::escape(query)` pattern, rehydrates an
    /// idle-evicted or race-evicted in-memory chunk map, and walks it
    /// collecting up to `want` hits scored at `GREP_FALLBACK_SCORE`. Empty
    /// query / `want` / regex-build failure short-circuits to `Vec::new()`.
    /// Retries up to [`REHYDRATE_RACE_RETRIES`] times only when the race is
    /// detected (mirrors `bm25_search`'s flag-under-guard check); a
    /// genuinely-empty map returns immediately.
    ///
    /// Issue #3401: when `filter` is set, a chunk must ALSO pass it to count
    /// toward the `out.len() >= want` early-exit. Counting an out-of-scope
    /// match toward that cap would cut the scan short and silently drop
    /// later in-scope matches — the same truncate-before-filter bug class as
    /// `bm25_search`, just expressed as an early `break` instead of a
    /// `Vec::truncate`.
    /// Test: `test_grep_fallback_returns_substring_hits` in `indexer::tests`;
    /// `grep_fallback_survives_reclaim_race_between_ensure_and_read` in
    /// `indexer::tests_idle_evict` pins the race-detection retry.
    // pub(crate): also called from tests.rs (a sibling of `search/` in `indexer`).
    pub(crate) async fn grep_fallback_search(
        &self,
        query: &str,
        want: usize,
        filter: Option<&(dyn Fn(&str) -> bool + Send + Sync)>,
    ) -> Vec<(String, f32)> {
        if query.is_empty() || want == 0 {
            return Vec::new();
        }
        let Ok(re) = regex::Regex::new(&regex::escape(query)) else {
            return Vec::new();
        };
        for _ in 0..REHYDRATE_RACE_RETRIES {
            self.ensure_chunks_loaded().await;
            let chunks = self.chunks.read().await;
            if chunks.is_empty() && self.chunks_evicted.load(Ordering::Relaxed) {
                // A reclaim raced us since `ensure()` returned — drop the read
                // guard and loop back to rehydrate again.
                continue;
            }
            let mut out: Vec<(String, f32)> = Vec::new();
            for raw in chunks.values() {
                if !re.is_match(&raw.content) {
                    continue;
                }
                if let Some(f) = filter {
                    if !f(&raw.id) {
                        continue;
                    }
                }
                out.push((raw.id.clone(), super::GREP_FALLBACK_SCORE));
                if out.len() >= want {
                    break;
                }
            }
            return out;
        }
        // Exhausted retries: see `bm25_search`'s identical tail comment — not
        // reachable at the configured enforcement cadence.
        tracing::warn!(
            "index '{}': chunk rehydrate raced by memory-pressure reclaim {REHYDRATE_RACE_RETRIES} \
             times in a row — returning empty grep-fallback lane for this query",
            self.index_id
        );
        Vec::new()
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

    /// Path/repo-scoped HNSW lane (issue #3401).
    ///
    /// Why: a naive over-fetch-then-filter cannot guarantee recall for an
    /// approximate-nearest-neighbour index — a chunk that genuinely matches
    /// the path/repo filter can rank arbitrarily far outside the raw-cosine-
    /// similarity `want` window HNSW would otherwise explore, and no amount
    /// of `want` inflation removes that risk in general. Instead, when a
    /// filter is active, this pushes the predicate INTO the HNSW traversal
    /// itself via `VectorStore::search_filtered` (usearch's
    /// `Index::filtered_search`, which evaluates the predicate during graph
    /// exploration and keeps expanding until `want` matching candidates are
    /// found or the graph is exhausted) — no recall loss. This is NOT free:
    /// a highly selective filter forces the traversal to visit far more of
    /// the graph than an unfiltered `top_k` search would (verified against
    /// vendored usearch 2.25.2's `filtered_search` C++ implementation,
    /// `index.hpp`, which only stops on `top_limit` predicate-passing
    /// candidates or frontier exhaustion) — real added latency under a very
    /// narrow scope, traded deliberately for correctness.
    /// What: delegates to plain `store.search` when no filter is active
    /// (identical behaviour/cost to today); otherwise resolves the
    /// root-relative `path_prefix` (`path_filter::normalized_path_prefix` —
    /// callers may pass either a root-relative or an absolute-under-root
    /// prefix, since `CodeChunk::file` in results is always absolute) and
    /// calls `store.search_filtered` with a predicate over the chunk id (see
    /// `path_filter` module docs for why testing the id is safe here).
    /// Test: `test_path_prefix_filter_survives_top_k_truncation` in
    /// `indexer::tests::path_filter_search`; `UsearchStore` predicate-pushdown
    /// itself is covered by
    /// `store::tests::test_filtered_search_finds_match_ranked_below_top_k`.
    pub(crate) async fn vector_search_scoped(
        &self,
        embedding: &[f32],
        want: usize,
        query: &SearchQuery,
    ) -> Result<Vec<(String, f32)>> {
        let Some(store) = &self.store else {
            return Ok(Vec::new());
        };
        if !path_filter::is_active(query) {
            let hits = store.search(embedding, want).await?;
            return Ok(hits.into_iter().map(|h| (h.chunk_id, h.score)).collect());
        }
        let normalized_prefix = path_filter::normalized_path_prefix(query, &self.root_path);
        let hits = store
            .search_filtered(embedding, want, normalized_prefix.as_deref(), &query.repos)
            .await?;
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
