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
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::core::classifier::QueryIntent;
use crate::core::embed::Embedder;
use crate::core::entity::EdgeKind;
use crate::service::embed_pool::RequestPriority;

use super::super::{hash_query, CodeIndexer, SearchQuery};
use super::path_filter;

/// Bound on the ensure-then-read retry loop in [`CodeIndexer::bm25_search`] /
/// [`CodeIndexer::grep_fallback_search`] (issue #2846 review; extended to the
/// cold-start case by issue #3683 slice 1).
///
/// Why: the memory-pressure ticker's high-water cadence is bounded well below
/// 1 Hz (`TRUSTY_MEMORY_ENFORCE_SECS`, default 30s), so a query lane racing
/// against a reclaim on every one of 3 consecutive attempts is not a real
/// steady-state scenario — this cap exists purely to convert a
/// theoretically-possible adversarial cadence into a graceful empty-result
/// degradation instead of an unbounded retry loop.
///
/// Issue #3683 slice 1: this same loop now also bounds the genuine
/// **cold-start** case — the first query after an idle eviction of a large
/// corpus, where `ensure_*_loaded()`'s detached rehydrate (see
/// `idle_evict::ensure_corpus_rehydrated`) may still be running when its own
/// bounded wait (`idle_evict::rehydrate_wait_budget`, default 9s) elapses.
/// Each loop iteration rejoins the SAME still-in-flight detached task (it is
/// deduplicated, not re-triggered) rather than starting a second scan, so the
/// worst-case total wait before this lane degrades is roughly
/// `REHYDRATE_RACE_RETRIES * rehydrate_wait_budget()` (~27s at the defaults) —
/// just under the default 30s query timeout, and the task itself keeps
/// running to completion in the background regardless of how many of these
/// retries (or the whole request) time out.
///
/// Code-critic review finding 3 (issue #3683, HIGH): the value that first
/// shipped here (~12s total) was deliberately LESS than the 27-40s cold-scan
/// latency this issue's own RCA measured on the production 315K-chunk
/// corpus, so on that corpus every first cold query was *guaranteed* to
/// exhaust these retries and degrade — see `idle_evict::DEFAULT_REHYDRATE_WAIT_MS`'s
/// doc comment for why ~27s (still an intentional trade-off — an interactive
/// query must never block on an O(corpus) scan — but no longer guaranteed to
/// lose on the low end of the measured window) replaced it, and for why this
/// remains a probabilistic improvement, not a guarantee, given the hard 30s
/// request-timeout ceiling. PROVIDED the degrade is observable: see
/// [`CodeIndexer::lane_degraded`] and the `trusty_bm25_lane_degraded` gauge
/// it drives (surfaced in the search HTTP response as
/// `meta.bm25_lane_degraded` — `service::server::search::search_handler`),
/// plus the `trusty_bm25_lane_degraded_total` / `trusty_grep_fallback_lane_degraded_total`
/// counters, at the bottom of `bm25_search` / `grep_fallback_search`. These
/// are the load-bearing signals distinguishing "degraded, rehydrate still
/// running" from a genuine empty result (an `Ok(vec![])` alone cannot). The
/// intended steady state is: the first cold query degrades (loudly, via the
/// gauge/counter/response-bit) while the detached task keeps converging in
/// the background, and the NEXT query — independent of any single request's
/// deadline — is warm, which is also the instant the gauge/flag reset
/// (finding 5 — see `idle_evict::spawn_detached_rehydrate`'s commit-success
/// branch). `TRUSTY_REHYDRATE_WAIT_MS` is the per-deployment knob for trading
/// first-query latency against fewer degraded responses.
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

    /// Embed one text via the pool's Interactive lane when a pool is
    /// installed, falling back to a direct `embedder.embed()` call otherwise
    /// (issue #3748 slice B PR 1).
    ///
    /// Why: `embed_text` and `embed_query` are the two query-side embed call
    /// sites (design research: `search/lanes.rs:154,182` on pre-#3748 main)
    /// and need identical pool-vs-direct routing; factoring it out gives the
    /// fallback branch (no pool resolvable — exercised by every existing
    /// test that doesn't call `set_embed_pool`/`set_embed_pool_source`) a
    /// single implementation and keeps each call site a one-liner.
    /// What: resolves the pool via [`CodeIndexer::resolve_embed_pool`]
    /// (self-healing boot-race fix, PR #3784 review) and, when one is
    /// available, submits a single-text batch to
    /// [`crate::service::embed_pool::EmbedPool::embed`] with
    /// `RequestPriority::Interactive` and unwraps the one resulting vector;
    /// otherwise calls `embedder.embed(text)` directly. Errors surface as the
    /// same `anyhow::Error` shape either path already used the underlying
    /// `EmbedderError`.
    /// Test: `embed_pool_routing::query_embeds_route_through_interactive_lane_when_pool_installed`,
    /// `embed_pool_routing::query_embeds_fall_back_to_direct_embedder_without_pool`
    /// in `core::indexer::tests`.
    async fn embed_interactive(
        &self,
        embedder: &Arc<dyn Embedder>,
        text: &str,
    ) -> Result<Vec<f32>> {
        if let Some(pool) = self.resolve_embed_pool().await {
            let mut out = pool
                .embed(vec![text.to_string()], RequestPriority::Interactive)
                .await?;
            return out
                .pop()
                .context("embed pool returned no vector for interactive request");
        }
        embedder.embed(text).await
    }

    /// Embed an arbitrary text using the wired embedder, bypassing the
    /// query-LRU cache.
    ///
    /// Why: callers outside the search hot path (e.g. context-embedding
    /// generation in `service::context_inference`) need embeddings without
    /// polluting the query cache. Returns `None` when no embedder is wired.
    /// What: thin wrapper around [`Self::embed_interactive`] (Interactive
    /// lane when a pool is installed; direct `embedder.embed()` otherwise).
    /// Test: covered indirectly via the context-embedding integration test;
    /// pool routing covered by `embed_pool_routing` (see
    /// [`Self::embed_interactive`]).
    pub async fn embed_text(&self, text: &str) -> Result<Option<Vec<f32>>> {
        let Some(embedder) = self.embedder.clone() else {
            return Ok(None);
        };
        let vec = self
            .embed_interactive(&embedder, text)
            .await
            .context("embed text")?;
        Ok(Some(vec))
    }

    /// Resolve a query → embedding, using the LRU cache to skip repeats.
    ///
    /// Why: search queries repeat across sessions; caching avoids repeated
    /// ONNX calls for the same text.
    /// What: hash the query, check the LRU, return cached vector if hit; else
    /// embed via [`Self::embed_interactive`] (Interactive lane when a pool is
    /// installed; direct `embedder.embed()` otherwise) and store. Returns
    /// `None` when no embedder is wired.
    /// Test: covered indirectly by every search integration test; pool
    /// routing covered by `embed_pool_routing` (see [`Self::embed_interactive`]).
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

        let vec = self
            .embed_interactive(&embedder, query)
            .await
            .context("embed query")?;

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
    /// Why the retry loop (issue #2846 review — MEDIUM; extended to cold
    /// start by issue #3683 slice 1): `ensure_bm25_entities_loaded`
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
    /// silently degrading recall. The identical flag-still-set-after-ensure
    /// signal also now covers a genuine **cold start**: `ensure_bm25_entities_loaded`
    /// is bounded (issue #3683 slice 1 — it never blocks past
    /// `idle_evict::rehydrate_wait_budget()`), so on a large/slow corpus the
    /// detached rehydrate task may simply not have finished yet when this
    /// function's own wait returns; the loop rejoins the SAME in-flight task
    /// on the next iteration rather than re-scanning.
    /// What: rehydrates an idle-evicted or race-evicted BM25 corpus (issue
    /// #2162 / #2846 / #3683), then acquires the BM25 read lock and runs
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
    // pub(crate): also called directly from `tests_idle_evict.rs` (a sibling
    // of `search`, not a descendant of it) to exercise the BM25 lane without
    // `grep_fallback_search`'s Definition-intent third-lane contamination —
    // mirrors `grep_fallback_search`'s own `pub(crate)` visibility below.
    pub(crate) async fn bm25_search(
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
        // Exhausted retries: either a reclaim landed on every attempt (not
        // reachable at the configured >=30s enforcement cadence), or — issue
        // #3683 slice 1 — the detached rehydrate task is still genuinely
        // running past `REHYDRATE_RACE_RETRIES * rehydrate_wait_budget()`
        // (a very large/slow cold-start scan). Either way, degrade to an
        // empty lexical lane rather than loop forever or burn the whole
        // request deadline; the task itself keeps running in the background
        // and the next query succeeds once it completes.
        //
        // Code-critic review finding 3 (issue #3683, HIGH): this degrade is
        // OBSERVABLE, not silent. `REHYDRATE_RACE_RETRIES * rehydrate_wait_budget()`
        // defaults to ~27s (see the const's doc comment above and
        // `idle_evict::DEFAULT_REHYDRATE_WAIT_MS` for why), at the low end of
        // the 27-40s cold-scan latency this PR's own RCA cites for the
        // production 315K-chunk corpus — narrowing, not eliminating, how
        // often a cold query on that corpus hits this path. An `Ok(vec![])`
        // here is otherwise bit-for-bit indistinguishable from a
        // genuinely-empty corpus at the HTTP layer. Finding 3(a): flip the
        // sticky per-index flag AND set the gauge —
        // together these are `meta.bm25_lane_degraded` in the search HTTP
        // response (`service::server::search::search_handler`) and the
        // `trusty_bm25_lane_degraded` Prometheus signal, so both a single
        // caller and a dashboard can tell this apart from a true empty
        // result without tailing logs. Reset (finding 5) happens in
        // `idle_evict::spawn_detached_rehydrate`'s commit-success branch,
        // the actual recovery instant, not on some later query noticing.
        self.lane_degraded.store(true, Ordering::Relaxed);
        metrics::gauge!("trusty_bm25_lane_degraded", "index" => self.index_id.clone()).set(1.0);
        metrics::counter!("trusty_bm25_lane_degraded_total", "index" => self.index_id.clone())
            .increment(1);
        tracing::warn!(
            "index '{}': BM25 rehydrate still not ready after {REHYDRATE_RACE_RETRIES} bounded \
             waits — returning empty lexical lane for this query (degraded, not a true empty \
             result; issue #3683)",
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
    /// Why the retry loop (issue #2846 review — MEDIUM; extended to cold
    /// start by issue #3683 slice 1): same race as
    /// [`Self::bm25_search`], applied to the `chunks` map / `chunks_evicted`
    /// flag instead of BM25 — `ensure_chunks_loaded` and `chunks.read()` below
    /// are separate lock acquisitions, so a memory-pressure reclaim can clear
    /// the map in between and this lane would silently scan zero chunks. The
    /// same flag-still-set check also covers a genuine cold start whose
    /// detached rehydrate hasn't finished within `ensure_chunks_loaded`'s own
    /// bounded wait — see `bm25_search`'s identical note.
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
        // Exhausted retries: see `bm25_search`'s identical tail comment
        // (issue #3683 finding 3) — either a reclaim race or a still-running
        // cold-start rehydrate. Same observability requirement: this degrade
        // must be distinguishable from a genuine empty result.
        // Finding 3(a): same sticky flag/gauge as `bm25_search` — grep
        // fallback shares the identical cold-start root cause (the detached
        // rehydrate hasn't converged yet), so it's folded into the same
        // signal rather than adding a second gauge for what is, from an
        // operator's point of view, the same "this index's lexical results
        // are degraded" condition.
        self.lane_degraded.store(true, Ordering::Relaxed);
        metrics::gauge!("trusty_bm25_lane_degraded", "index" => self.index_id.clone()).set(1.0);
        metrics::counter!("trusty_grep_fallback_lane_degraded_total", "index" => self.index_id.clone())
            .increment(1);
        tracing::warn!(
            "index '{}': chunk rehydrate still not ready after {REHYDRATE_RACE_RETRIES} bounded \
             waits — returning empty grep-fallback lane for this query (degraded, not a true \
             empty result; issue #3683)",
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
