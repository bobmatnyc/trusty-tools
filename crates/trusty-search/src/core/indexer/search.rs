//! Hybrid search pipeline for [`CodeIndexer`].
//!
//! Why: the search hot path (query classify → embed → HNSW + BM25 → RRF → KG
//! expand → MMR diversity → materialize) is the most-read code in this file
//! and benefits from living in a dedicated module away from ingest/persist.
//! What: holds `search`, the per-lane helpers (`embed_query`, `bm25_search`,
//! `vector_search`), the KG expansion logic (`kg_expand`, `edge_kinds_for_intent`),
//! and the post-fusion stages (`apply_mmr_rerank`, `apply_score_adjustments`,
//! `inject_entity_exact_match`, `expand_with_kg`, `materialize_search_results`).
//! Test: covered by every `test_search_*`, `test_kg_*`, and intent-routing test
//! in `indexer::tests`.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};

use crate::core::classifier::{QueryClassifier, QueryIntent};
use crate::core::entity::EdgeKind;
use crate::core::git::{normalize_path, resolve_branch_files};
use crate::core::mmr::cosine_similarity;
use crate::core::search::rrf::{rrf_fuse, RRF_K};

use super::archive::{self, MarkerCache};
use super::docs_penalty;
use super::{
    build_compact_snippet, compute_match_reason, definition_boost_query_tokens,
    file_type_score_multiplier, hash_query, is_function_definition_chunk_type,
    is_struct_definition_chunk_type, raw_to_code_chunk, CodeChunk, CodeIndexer, SearchQuery,
    HNSW_OVERSAMPLE, KG_EXPAND_HOPS, STRUCT_DEFINITION_BOOST,
};

/// Score assigned to grep-fallback hits (issue #75). Intentionally tiny so
/// fallback rows never out-rank a real BM25/vector hit — they only surface
/// when the primary lanes returned nothing.
const GREP_FALLBACK_SCORE: f32 = 0.001;

/// Minimum cosine similarity a KG-expanded neighbour must have against the
/// `refine_query` embedding before it is kept (issue #147).
///
/// Why: 0.4 is empirically safe — embeddings for unrelated code concepts
/// typically score below 0.3, while semantically related code clusters score
/// 0.5–1.0.  The threshold is applied as `>= KG_REFINE_THRESHOLD` so
/// exactly-equal values (boundary case) are kept.
pub(crate) const KG_REFINE_THRESHOLD: f32 = 0.4;

/// Merge a grep lane into an already-RRF-fused list (issue #75).
///
/// Why: `rrf_fuse` is hard-coded to two lanes (HNSW + BM25). Rather than
/// reshape its signature, we fold the grep lane in via the same reciprocal
/// rank formula and re-truncate to `top_k`. Grep hits that already appeared
/// in the fused list get their score bumped; brand-new ids show up at the
/// bottom of the ranking.
/// What: walks `grep_lane` (ordered best-first), adds `weight * 1/(k+rank)`
/// to each id's score, re-sorts by score desc with id as tie-break, then
/// truncates.
/// Test: `test_merge_grep_lane_appends_new_ids` in `indexer::tests`.
pub(crate) fn merge_grep_lane(
    fused: Vec<(String, f32)>,
    grep_lane: &[(String, f32)],
    weight: f32,
    top_k: usize,
) -> Vec<(String, f32)> {
    if grep_lane.is_empty() {
        return fused;
    }
    let mut accum: HashMap<String, f32> = fused.into_iter().collect();
    for (rank0, (id, _)) in grep_lane.iter().enumerate() {
        let rank = (rank0 + 1) as f32;
        *accum.entry(id.clone()).or_insert(0.0) += weight * (1.0 / (RRF_K + rank));
    }
    let mut out: Vec<(String, f32)> = accum.into_iter().collect();
    out.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    out.truncate(top_k);
    out
}

/// Lower bound for the branch-modified file score multiplier (issue #122).
/// `1.0` disables boosting and is the floor.
pub(crate) const BRANCH_BOOST_MIN: f32 = 1.0;
/// Upper bound for the branch-modified file score multiplier (issue #122).
/// `3.0` keeps the boost gentle enough that strong off-branch matches still
/// outrank weak on-branch ones.
pub(crate) const BRANCH_BOOST_MAX: f32 = 3.0;

/// Resolve the effective branch-modified file set + clamped multiplier for a
/// search query (issue #122).
///
/// Why: extracted from `search` so the resolution rules — explicit
/// `branch_files` wins over `branch`, missing both → no boost, clamp the
/// multiplier — are easy to read and to unit-test.
/// What: returns `(Some(set), boost)` when there is a non-empty file list and
/// the resolved boost is strictly greater than `1.0`; `(None, _)` otherwise.
/// Test: covered by `test_branch_boost_clamped_to_3x`,
/// `test_no_boost_when_branch_files_absent`, and the integration tests.
pub(crate) fn resolve_branch_set(
    query: &SearchQuery,
    root_path: &std::path::Path,
) -> (Option<HashSet<String>>, f32) {
    let boost = query.branch_boost.clamp(BRANCH_BOOST_MIN, BRANCH_BOOST_MAX);

    let files: Option<Vec<String>> = match &query.branch_files {
        Some(v) if !v.is_empty() => Some(v.clone()),
        _ => match &query.branch {
            Some(name) => resolve_branch_files(root_path, name),
            None => None,
        },
    };

    let set = files.and_then(|v| {
        let s: HashSet<String> = v.iter().map(|p| normalize_path(p).to_owned()).collect();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    });

    // If the resolved multiplier is the no-op floor, drop the set so we skip
    // the per-chunk lookup work entirely in the hot path.
    if (boost - 1.0).abs() < f32::EPSILON {
        (None, boost)
    } else {
        (set, boost)
    }
}

impl CodeIndexer {
    /// Batch-fetch the `RawChunk`s for a set of chunk ids, reading from the
    /// durable redb corpus when one is wired and falling back to the in-memory
    /// `chunks` HashMap otherwise.
    ///
    /// Why: issue #28 deferred item — the query hot path used to join fused
    /// `(id, score)` pairs against `chunks: Arc<RwLock<HashMap<..>>>`, which
    /// kept every chunk's text resident in the process heap permanently
    /// (~45 GB RSS on a large monorepo). Reading the top-k chunk text straight
    /// from redb at materialization time lets the daemon serve those bytes
    /// from the OS page cache (redb values are mmap-backed) instead of the
    /// heap, dropping steady-state RSS to <10 GB. A `top_k=20` query does ~20
    /// point reads in a single read transaction — fast enough for the sub-10 ms
    /// budget.
    /// What: when `self.corpus` is `Some`, runs `CorpusStore::get_chunks` on a
    /// blocking worker (redb's API is sync) and returns the result keyed by id
    /// for O(1) join in the caller. When `self.corpus` is `None` (BM25-only /
    /// test indexers built without a data dir), falls back to cloning the
    /// requested entries out of the in-memory HashMap so those indexers behave
    /// exactly as before. Either way the result is a `HashMap<id → RawChunk>`;
    /// ids with no row (a benign race against a concurrent removal, a corrupt
    /// row, or a chunk never persisted) are simply absent — the caller skips
    /// them with a `trace`.
    /// Test: covered by every `test_search_*` integration test (the durable
    /// path) and by `core::corpus::tests::get_chunks_batch_reads_subset` (the
    /// redb batch read itself).
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
        // entries out of the in-memory HashMap. Rehydrate first in case the map
        // was evicted while idle (no-op unless `chunks_evicted` is set).
        self.ensure_chunks_loaded().await;
        let chunks = self.chunks.read().await;
        ids.iter()
            .filter_map(|id| chunks.get(id).map(|c| (id.clone(), c.clone())))
            .collect()
    }

    /// Retrieve a cached chunk embedding by `chunk_id`.
    ///
    /// Why: code-to-code similarity search (issue #31) needs the seed chunk's
    /// embedding to query the HNSW lane without re-embedding its source. We
    /// already populate `chunk_embeddings` on `add_chunk`, so this is an O(1)
    /// lookup. Returns `None` when the chunk doesn't exist or was indexed in
    /// BM25-only mode (no embedder wired).
    pub fn get_embedding(&self, chunk_id: &str) -> Option<Vec<f32>> {
        // `peek` doesn't promote the entry — we read through an `&RwLockReadGuard`
        // (immutable), and we don't want background reads to disturb LRU order
        // (only the write paths in `add_chunk` / batch commit promote on insert).
        self.chunk_embeddings
            .try_read()
            .ok()
            .and_then(|g| g.peek(chunk_id).cloned())
    }

    /// Embed an arbitrary text using the wired embedder, bypassing the
    /// query-LRU cache.
    ///
    /// Why: callers outside the search hot path (e.g. context-embedding
    /// generation in `service::context_inference`, fan-out routing in
    /// `service::server`) need to produce embeddings without polluting the
    /// query cache and without going through `embed_query`'s `pub(super)`
    /// gate. Returns `None` when no embedder is wired (BM25-only mode).
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
    pub(super) async fn embed_query(&self, query: &str) -> Result<Option<Vec<f32>>> {
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
    /// every search. On a 115k-chunk index that single line cost ~9.5s and
    /// caused all results to rank by BM25 alone (the HNSW lane completed
    /// fast but the latency budget was already gone). The index is now
    /// maintained incrementally by `add_chunk` / `index_files_batch` /
    /// `remove_*`, so the search hot path is just a read lock + posting walk.
    async fn bm25_search(&self, query: &str, want: usize) -> Result<Vec<(String, f32)>> {
        let bm25 = self.bm25.read().await;
        if bm25.is_empty() {
            return Ok(Vec::new());
        }
        Ok(bm25.score_query_all(query, want))
    }

    /// Grep-fallback lane: scan in-memory chunk contents for a literal match
    /// of `query` (issue #75).
    ///
    /// Why: when the primary BM25 + vector lanes both return no rows (rare
    /// but real on small / unusual indexes, or when the query tokenises to
    /// nothing useful) we want at least an exact-substring fallback before
    /// telling the caller "no results". This is the same role ripgrep would
    /// play if shelled out to, but it runs against the already-loaded chunk
    /// corpus so it costs nothing extra to maintain.
    /// What: builds a regex from `regex::escape(query)` (so user input is
    /// treated as a literal, never as a pattern), then walks
    /// `self.chunks.read()` and collects up to `want` hits scored at
    /// [`GREP_FALLBACK_SCORE`]. Empty / regex-build failure short-circuits
    /// to `Vec::new()`.
    /// Test: `test_grep_fallback_returns_substring_hits` in `indexer::tests`.
    pub(super) async fn grep_fallback_search(
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
        // Rehydrate the in-memory corpus if it was evicted while idle — the
        // grep fallback scans chunk *content*, which only the in-memory map
        // carries (redb point-reads are keyed by id, not content).
        self.ensure_chunks_loaded().await;
        let chunks = self.chunks.read().await;
        let mut out: Vec<(String, f32)> = Vec::new();
        for raw in chunks.values() {
            if re.is_match(&raw.content) {
                out.push((raw.id.clone(), GREP_FALLBACK_SCORE));
                if out.len() >= want {
                    break;
                }
            }
        }
        out
    }

    /// Run the HNSW lane. Returns `(chunk_id, distance)` style — we treat the
    /// `VectorStore`'s `score` as opaque since RRF only consumes rank.
    pub(super) async fn vector_search(
        &self,
        embedding: &[f32],
        want: usize,
    ) -> Result<Vec<(String, f32)>> {
        let Some(store) = &self.store else {
            return Ok(Vec::new());
        };
        let hits = store.search(embedding, want).await?;
        // VectorStore returns "higher = better" already (1 - cos_dist); we keep
        // that convention so callers can sort or display directly. RRF ignores
        // the magnitude.
        Ok(hits.into_iter().map(|h| (h.chunk_id, h.score)).collect())
    }

    /// Edge-kinds traversed for each query intent (issue #18).
    ///
    /// Each intent picks a small set of `EdgeKind`s most likely to surface
    /// adjacent code that's actually relevant to the question being asked.
    /// Score for each neighbour = `seed_score * edge_kind.score_multiplier()`.
    fn edge_kinds_for_intent(intent: QueryIntent) -> Vec<EdgeKind> {
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

    /// Intent-gated KG expansion (issue #18). For each seed
    /// `(chunk_id, score)`:
    /// 1. Look up the defining symbol of the seed chunk.
    /// 2. BFS its `EdgeKind`-filtered neighbourhood (intent-specific edges).
    /// 3. Score each neighbour as `seed_score * edge_kind.score_multiplier()`.
    ///
    /// Deduplicates: a chunk already in the seed set is never re-emitted; a
    /// chunk reachable through multiple seed/edge paths keeps its best score.
    async fn kg_expand(&self, seeds: &[(String, f32)], intent: QueryIntent) -> Vec<(String, f32)> {
        let graph = self.symbol_graph().await;
        if graph.node_count() == 0 || seeds.is_empty() {
            return Vec::new();
        }

        let edge_kinds = Self::edge_kinds_for_intent(intent);
        let seed_ids: std::collections::HashSet<&String> = seeds.iter().map(|(id, _)| id).collect();
        let mut best: HashMap<String, f32> = HashMap::new();

        for (seed_id, seed_score) in seeds {
            let Some(symbol) = graph.symbol_for_chunk(seed_id) else {
                continue;
            };
            for (_, neighbour_id, edge_kind) in
                graph.neighbors_by_edge(symbol, &edge_kinds, KG_EXPAND_HOPS)
            {
                if seed_ids.contains(&neighbour_id) {
                    continue;
                }
                let derived = seed_score * edge_kind.score_multiplier();
                best.entry(neighbour_id)
                    .and_modify(|s| {
                        if derived > *s {
                            *s = derived;
                        }
                    })
                    .or_insert(derived);
            }
        }

        let mut out: Vec<(String, f32)> = best.into_iter().collect();
        // Stable order: score desc, then id asc.
        out.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        out
    }

    /// Hybrid search: classify intent → route weights → HNSW + BM25 → RRF → KG.
    ///
    /// Steps:
    /// 1. Classify intent (regex-based, sub-ms) and pick `(alpha, beta, use_kg_first)`.
    /// 2. Embed the query (LRU-cached).
    /// 3. Run HNSW (`top_k * 4` candidates) and BM25 in parallel.
    /// 4. Fuse with RRF (`k=60`).
    /// 5. KG-expand (stub) when intent says so.
    /// 6. Materialise the top `top_k` chunk IDs into `CodeChunk`s with the
    ///    fused score and per-result `match_reason`.
    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<CodeChunk>> {
        // Mark the index live so the idle-eviction ticker won't reclaim its
        // in-memory chunk map out from under an actively-queried session.
        self.touch_activity();
        // Use the domain-aware classifier so per-index vocabulary from
        // `trusty-search.yaml` (`domain_terms:`) nudges otherwise-`Unknown`
        // queries to `Definition` intent. Falls back to plain `classify` when
        // `domain_terms` is empty (the common single-index case).
        let intent = QueryClassifier::classify_with_domain(&query.text, &self.domain_terms);
        let (alpha, beta, use_kg_first) = intent.weights();

        // Issue #73: intent-aware effective mode. The mode field defaults to
        // `SearchMode::Code` for backward compatibility (issue #77), which
        // hard-filters out every `.md` chunk in `apply_archive_downrank`.
        // Conceptual and Definition intents both need docs to answer
        // correctly (READMEs, architecture guides, hook docs, YAML
        // frontmatter), so when the caller did not override the default,
        // upgrade the effective mode to `SearchMode::All`. Explicit
        // `SearchMode::Code` from the caller still wins — only the implicit
        // default gets nudged by intent.
        //
        // Why: without this, queries classified as Conceptual or Definition
        // return 0 results on doc-heavy questions because the post-filter
        // drops every chunk before materialisation.
        // What: pattern-match the (intent, query.mode) tuple; promote
        // `Code` → `All` for Conceptual/Definition; pass through every other
        // combination unchanged.
        // Test: `test_conceptual_does_not_demote_docs` (now exercises default
        // Code mode), `test_mode_filter_code_excludes_markdown` (still
        // asserts explicit Code excludes `.md`).
        let effective_mode = match (&intent, query.mode) {
            (QueryIntent::Conceptual, super::SearchMode::Code) => super::SearchMode::All,
            (QueryIntent::Definition, super::SearchMode::Code) => super::SearchMode::All,
            _ => query.mode,
        };

        tracing::debug!(
            "search index={} query={:?} intent={:?} alpha={} beta={} mode={:?} effective_mode={:?}",
            self.index_id,
            query.text,
            intent,
            alpha,
            beta,
            query.mode,
            effective_mode
        );

        // Staged-pipeline lane selector (issue #109, Phase 1; extended #138
        // for per-lane MCP tools). Each `SearchStage` pins the dispatcher to
        // a fixed lane combination regardless of what's ready on the index:
        //   * `Lexical`  → BM25 + grep only (no HNSW, no KG).
        //   * `Semantic` → BM25 + HNSW via RRF; KG expansion skipped.
        //   * `Graph`    → BM25 + HNSW + KG expansion forced ON.
        //   * `None`     → legacy adaptive routing (every ready lane runs).
        let lexical_only = matches!(query.stage, Some(super::SearchStage::Lexical));
        let semantic_lane = matches!(query.stage, Some(super::SearchStage::Semantic));
        let graph_lane = matches!(query.stage, Some(super::SearchStage::Graph));
        // KG expansion is force-enabled by `Graph`, force-disabled by
        // `Lexical` or `Semantic`, and otherwise honours the caller's
        // `expand_graph` flag combined with the intent's `use_kg_first`.
        let force_kg = graph_lane;
        let skip_kg = lexical_only || semantic_lane;

        // 1) Embed (cache-first) — None when no embedder is wired OR the
        //    caller has opted out of the semantic lane via `?stage=lexical`.
        let embedding = if lexical_only {
            None
        } else {
            self.embed_query(&query.text).await?
        };

        // 2) Run lanes (HNSW + BM25), then inject entity-exact-match if applicable.
        let want = query.top_k.saturating_mul(HNSW_OVERSAMPLE).max(query.top_k);
        let bm25_fut = self.bm25_search(&query.text, want);
        let hnsw_results = match &embedding {
            Some(v) => self.vector_search(v, want).await?,
            None => Vec::new(),
        };
        let mut bm25_results = bm25_fut.await?;
        self.inject_entity_exact_match(&intent, &query.text, beta, &mut bm25_results)
            .await;

        // 2a) Issue #75: for Definition intent, run grep in parallel as a
        //     third RRF lane. Exact identifier matches give a strong signal
        //     for "where is this symbol declared" queries that the regex
        //     tokeniser may otherwise miss (underscore-heavy names, embedded
        //     in larger words, etc.). We re-use the BM25 weight `beta` for
        //     the grep lane because the signal it carries is lexical, not
        //     semantic.
        let grep_lane: Vec<(String, f32)> = if matches!(intent, QueryIntent::Definition) {
            self.grep_fallback_search(&query.text, want).await
        } else {
            Vec::new()
        };

        // 3) RRF fuse, then MMR diversity.
        //
        // Issue #72: fuse / merge / rerank over the oversampled `want`
        // candidate budget, NOT the caller's `top_k`. `rrf_fuse` truncates
        // its output, so truncating to `top_k` here would discard
        // source-file candidates *before* the mode-aware `doc_score_penalty`
        // in `apply_score_adjustments` ever runs. A long high-TF prose chunk
        // (e.g. CHANGELOG.md) would then occupy the only surviving slots, the
        // genuine `.rs` match would be gone, and the post-RRF hard file-type
        // filter would drop the prose — yielding zero results for a
        // code-navigation query. Keeping `want` candidates alive through the
        // penalty pass lets the docs sink and the source rows claim the final
        // `top_k` slots, which are cut once at materialization.
        let fused_raw = rrf_fuse(&hnsw_results, &bm25_results, alpha, beta, RRF_K, want);
        let fused_raw = merge_grep_lane(fused_raw, &grep_lane, beta, want);

        // 3a) Issue #75: empty-result fallback. When both primary lanes (and
        //     the optional Definition grep lane) produced nothing, scan the
        //     in-memory chunk corpus for a literal substring match and use
        //     those as the result set. They are scored at
        //     [`GREP_FALLBACK_SCORE`] (≈ 0.001) so they are clearly weaker
        //     than any real BM25/vector hit, and they materialise with
        //     `match_reason = "fallback:ripgrep"` because they appear in
        //     none of the (in_hnsw, in_bm25, in_kg) sets the materializer
        //     consults.
        let fused_raw = if fused_raw.is_empty() {
            self.grep_fallback_search(&query.text, want).await
        } else {
            fused_raw
        };
        let fused = self.apply_mmr_rerank(fused_raw, want).await;

        // 4) KG expand (conditional). Track which IDs came **only** from KG
        //    so the materialization step can label them "hybrid+kg".
        //    Lane gating:
        //      * `Lexical` / `Semantic`  → skip KG entirely (issue #138).
        //      * `Graph`                 → force-enable KG even when
        //        `intent.use_kg_first` is false; the LLM explicitly asked
        //        for graph expansion by picking `search_kg`.
        //      * `None` (default)        → existing behaviour: KG runs iff
        //        `use_kg_first && query.expand_graph`.
        //
        // Issue #147: embed `refine_query` (when present) for use in
        // KG-neighbourhood reranking. This is done here (not inside
        // `expand_with_kg`) so the await point is outside the expand helper
        // and any error is surfaced at the `search` boundary.
        let refine_embedding: Option<Vec<f32>> = if skip_kg {
            None
        } else {
            match &query.refine_query {
                Some(rq) if !rq.is_empty() => self.embed_query(rq).await?,
                _ => None,
            }
        };
        let (all, kg_ids) = if skip_kg {
            (fused, std::collections::HashSet::new())
        } else {
            let effective_use_kg = use_kg_first || force_kg;
            let effective_expand = query.expand_graph || force_kg;
            self.expand_with_kg(
                fused,
                &intent,
                effective_use_kg,
                effective_expand,
                refine_embedding.as_deref(),
            )
            .await
        };

        // 4a) Re-rank by score after KG expansion (issue #94): KG-expanded
        //     neighbours are appended after the fused list, so a naïve
        //     `take(top_k)` would silently discard them. Sort the merged
        //     `(id, score)` list so well-scored KG hits survive truncation
        //     and `match_reason: "hybrid+kg"` actually surfaces in results.
        // 4b) Apply a file-type multiplier for Definition intent (issue #92):
        //     when the user is looking for a symbol definition, prefer source
        //     files over docs/configs whose BM25 TF can spuriously rank them
        //     above the canonical .rs/.py/.go declaration.
        // 4c) Apply a branch-modified file multiplier (issue #122): chunks
        //     whose file is part of the current branch's diff against its
        //     merge-base are nudged upward so feature-branch work is surfaced
        //     ahead of equivalent off-branch matches.
        let (branch_set, branch_boost) = resolve_branch_set(query, &self.root_path);
        let all = self
            .apply_score_adjustments(
                all,
                &intent,
                &query.text,
                branch_set.as_ref(),
                branch_boost,
                effective_mode,
            )
            .await;

        // 5) Materialise the top-k IDs into `CodeChunk`s.
        let mut result = self
            .materialize_search_results(
                all,
                &hnsw_results,
                &bm25_results,
                &kg_ids,
                branch_set.as_ref(),
                query,
            )
            .await;

        // 6) Issue #77 (final design) + #75: mode-based hard file-type
        //    filter + archive downrank. First drop chunks whose file is
        //    not in the allowed set for the requested `SearchMode`
        //    (replaces the prior penalty matrix — see `docs_penalty`).
        //    Then apply a multiplicative score penalty to chunks that
        //    look like archive / deprecated / legacy code and stamp the
        //    `archive_reason` label. Re-sort by score so demoted rows
        //    sink within the filtered set.
        self.apply_archive_downrank(&mut result, effective_mode, query.exclude_archived);
        Ok(result)
    }

    /// Apply the mode-based hard file-type filter (issue #77, final
    /// design) and then the archive score penalty (issue #75) to the
    /// materialized result list.
    ///
    /// Why: two distinct concerns share the post-materialisation step.
    /// First, the unified search tool's `SearchMode` says which file
    /// types are valid for this query — code, text, data, or all — and
    /// chunks outside the allowed set are dropped entirely (no score
    /// distortion, no cross-contamination). Second, even within the
    /// allowed set, archived/legacy/deprecated code should sink so live
    /// code surfaces first (path keywords like `deprecated/`,
    /// `#[deprecated]` annotations, `.archived` marker files, stale
    /// `git_mtime`). The archive penalty stamps `archive_reason` so the
    /// UI can explain why a row sank.
    /// What: walks `results` in two passes. (1) retain only chunks for
    /// which [`docs_penalty::is_allowed_for_mode`] returns `true` for
    /// the requested mode (skipped entirely when mode is `All`). (2)
    /// for each surviving chunk, run [`archive::classify`] and multiply
    /// `score` by the returned multiplier when an archive reason fires;
    /// stamp `archive_reason`. Finally re-sort by score descending with
    /// id as the stable tie-breaker. The marker-file cache is shared
    /// across the whole pass so K chunks under the same directory pay
    /// at most one filesystem hit.
    /// Issue #74: when `exclude_archived` is `true`, chunks whose archive
    /// classifier fires (path keyword, `#[deprecated]` annotation, or a
    /// `.archived` / `DEPRECATED` marker file) are dropped from the result
    /// list entirely rather than score-penalised. The lighter mtime-only
    /// "stale" signal does NOT trigger exclusion — staleness alone is too
    /// weak to justify hiding a result; only the strong archive signals do.
    ///
    /// Test: `test_archive_downrank_demotes_deprecated_chunks`,
    /// `test_exclude_archived_drops_archive_chunks`,
    /// `test_mode_filter_*` integration tests, and the per-mode unit
    /// tests in [`archive::tests`] / [`docs_penalty::tests`].
    fn apply_archive_downrank(
        &self,
        results: &mut Vec<CodeChunk>,
        mode: super::SearchMode,
        exclude_archived: bool,
    ) {
        if results.is_empty() {
            return;
        }
        // Issue #77: hard file-type filter. Each `SearchMode` declares an
        // allowed set of file extensions / named-doc prefixes; chunks
        // outside that set are dropped from the result list entirely.
        // `SearchMode::All` short-circuits to "everything allowed" so the
        // raw RRF ranking surfaces unchanged.
        //
        // Issue #78: in `SearchMode::Code` we additionally drop chunks whose
        // `chunk_type` is `Docstring`. The docs_penalty file-type filter
        // already rejects .md / prose extensions, but a `Docstring` chunk
        // attached to a `.rs` file (extracted by the chunker from a `///`
        // doc-comment block) still carries prose tokens that BM25 weights
        // highly for symbol-name queries. Filtering by chunk_type at this
        // stage keeps code-mode results to actual executable Rust.
        if matches!(mode, super::SearchMode::Code) {
            use crate::core::chunker::ChunkType;
            results.retain(|chunk| !matches!(chunk.chunk_type, ChunkType::Docstring));
        }
        results.retain(|chunk| docs_penalty::is_allowed_for_mode(&chunk.file, mode));

        // Issue #75: archive downranking still applies after the file-type
        // filter — an `archived/` source file is still source code, but it
        // should sink relative to live source.
        //
        // Issue #77 (final design): on top of the hard filter we also apply
        // the [`docs_penalty::doc_score_penalty`] matrix so any chunk that
        // survived the filter but is still off-target for the requested
        // mode (currently a no-op because the filter already keeps only
        // on-mode files, but defensive when the filter relaxes or new
        // file-type classifications land) sinks with its penalty
        // multiplier and gets a `text:` / `data:` / `source:` reason
        // stamp.
        let mut markers = MarkerCache::new();
        // Issue #74: collect the chunk ids that fired a *strong* archive
        // signal (path / annotation / marker — anything but the lighter
        // `stale:` mtime signal) so we can drop them after the loop when
        // `exclude_archived` is set. We can't `retain` mid-iteration because
        // we hold `&mut` borrows of each chunk; defer the removal.
        let mut archived_ids: HashSet<String> = HashSet::new();
        for chunk in results.iter_mut() {
            let (archive_mult, archive_reason_opt) =
                archive::classify(&self.root_path, &chunk.file, &chunk.content, &mut markers);
            // Issue #72: the docs_penalty multiplier is now applied in
            // `apply_score_adjustments` (pre-truncation) so that prose
            // chunks can't crowd source matches out of top_k before the
            // penalty fires. We still call `doc_score_penalty` here purely
            // to recover the `archive_reason`-style label (e.g.
            // `text:CHANGELOG.md`) for the UI — the multiplier is NOT
            // re-applied to the score.
            let (_docs_mult, docs_reason_opt) = docs_penalty::doc_score_penalty(&chunk.file, mode);
            if archive_reason_opt.is_some() {
                chunk.score *= archive_mult;
            }
            // Issue #74: only the strong archive signals (not `stale:`)
            // qualify for hard exclusion. `archive::classify` returns a
            // `stale:`-prefixed reason only when no strong signal fired, so
            // we can distinguish on the prefix.
            if let Some(reason) = &archive_reason_opt {
                if exclude_archived && !reason.starts_with("stale:") {
                    archived_ids.insert(chunk.id.clone());
                }
            }
            if archive_reason_opt.is_some() || docs_reason_opt.is_some() {
                // Archive reason wins — issue #75 tests assert on its
                // prefixes (`path:`/`annotation:`/`marker:`/`stale:`).
                chunk.archive_reason = archive_reason_opt.or(docs_reason_opt);
            }
        }
        if exclude_archived && !archived_ids.is_empty() {
            results.retain(|chunk| !archived_ids.contains(&chunk.id));
        }
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
    }

    /// Re-rank merged direct+KG candidates and apply file-type weighting.
    ///
    /// Why: KG-expanded neighbours are appended after the RRF-fused list, so
    /// the naïve `take(top_k)` in `materialize_search_results` used to drop
    /// them (issue #94). At the same time, Definition-intent queries used to
    /// rank `.md` docs above source files because they had high BM25 TF for
    /// symbol names (issue #92). We solve both by adjusting every candidate's
    /// score in a single pass and re-sorting before truncation.
    ///
    /// Issue #72: the mode-aware `doc_score_penalty` matrix used to fire
    /// *after* the `take(top_k)` truncation in `apply_archive_downrank`. That
    /// meant prose / config files with high BM25 TF could fill the top-k
    /// slots and crowd out genuine source-file matches before the penalty
    /// ever got a chance to demote them. We now apply the matrix here, in
    /// the pre-truncation pass, so the docs sink in ranking and the source
    /// chunks they would have displaced get to claim top-k slots. The
    /// post-truncation pass in `apply_archive_downrank` still runs (idempotent
    /// — multiplier 1.0 leaves the score unchanged) so the `archive_reason`
    /// label stays attached for the UI.
    ///
    /// What: for `Definition` intent, multiplies the score of each candidate
    /// by `0.5` if its file extension is in `DOC_EXTENSIONS`. Then multiplies
    /// by the mode-aware `doc_score_penalty` matrix (e.g. 0.1× for prose
    /// chunks under Code mode). Issue #117 additionally multiplies by
    /// [`STRUCT_DEFINITION_BOOST`] (2.0×) when the chunk is a
    /// Struct/Enum/Class/Trait/TypeAlias declaration whose `function_name`
    /// literally matches a query token — this surfaces canonical
    /// declarations (`hnsw_store.rs::HnswStore`) above usage chunks
    /// (`retrieval.rs`) for queries like `HNSW vector similarity search`.
    /// Issue #122 extends the same boost to `Function`/`Method` chunks so
    /// function-name queries (`BRUSILOV_EPOCH`, `get_call_chain`) surface
    /// the canonical declaration ahead of usage sites and string-literal
    /// occurrences (e.g. a JSON tool descriptor containing the function
    /// name as a string). Finally re-sorts by score descending with id as
    /// a stable tie-breaker.
    /// Test: covered by `test_definition_demotes_markdown_below_source`,
    /// `test_kg_results_survive_top_k_truncation`,
    /// `test_code_mode_source_outranks_changelog_pre_truncation` (issue #72),
    /// `test_struct_definition_boost_surfaces_struct_over_usage` (#117),
    /// and `test_function_definition_boost_surfaces_function_over_string_literal_usage`
    /// (#122).
    async fn apply_score_adjustments(
        &self,
        candidates: Vec<(String, f32)>,
        intent: &QueryIntent,
        query_text: &str,
        branch_files: Option<&HashSet<String>>,
        branch_boost: f32,
        effective_mode: super::SearchMode,
    ) -> Vec<(String, f32)> {
        let demote_docs = matches!(intent, QueryIntent::Definition);
        // Issue #117: for Definition-intent queries, boost chunks that are
        // *the* declaration of a type (Struct / Enum / Class / Trait /
        // TypeAlias) whose `function_name` matches a literal query token.
        // Pre-compute the lowercased token set once so the per-candidate
        // loop is O(tokens) per chunk; for typical 4-word queries this is
        // ~4 ASCII string compares.
        let struct_boost_tokens: Vec<String> = if matches!(intent, QueryIntent::Definition) {
            definition_boost_query_tokens(query_text)
        } else {
            Vec::new()
        };
        // Issue #28 deferred item: read the candidate chunks from the durable
        // redb corpus (mmap-backed, OS-page-cached) instead of the in-memory
        // HashMap so the heap-resident corpus can be dropped from the query
        // hot path. Only the file path of each candidate is needed here.
        let candidate_ids: Vec<String> = candidates.iter().map(|(id, _)| id.clone()).collect();
        let chunks = self.fetch_chunks_for_ids(&candidate_ids).await;
        let mut adjusted: Vec<(String, f32)> = candidates
            .into_iter()
            .map(|(id, score)| {
                let mut multiplier = 1.0_f32;
                let raw = chunks.get(&id);
                if demote_docs {
                    if let Some(r) = raw {
                        multiplier *= file_type_score_multiplier(&r.file);
                    }
                }
                // Issue #72: apply the mode-aware doc/source penalty here,
                // BEFORE the top_k truncation, so high-TF prose can't
                // crowd source-file matches out of the result list. The
                // post-truncation pass in `apply_archive_downrank` is now
                // a no-op for the score (the matrix is idempotent under
                // repeat application — second multiplication is by the
                // same value, but we deliberately don't double-apply: see
                // `apply_archive_downrank` which inspects the stamped
                // `archive_reason` to skip the second multiply).
                if let Some(r) = raw {
                    let (docs_mult, _) = docs_penalty::doc_score_penalty(&r.file, effective_mode);
                    multiplier *= docs_mult;
                }
                // Issue #117 / #122: Definition-intent structural boost —
                // multiply by [`STRUCT_DEFINITION_BOOST`] when the chunk is
                // the declaration of a Struct/Enum/Class/Trait/TypeAlias
                // (#117) OR a Function/Method (#122) whose `function_name`
                // contains (case-insensitive) at least one query token.
                // Substring rather than exact match so the canonical
                // type-declaration case — query
                // `HNSW vector similarity search` vs declaration
                // `HnswStore` — fires: lowercased, the function-name
                // `hnswstore` contains the token `hnsw`.
                //
                // String-literal false-positive defense (#122): the
                // chunk_type filter naturally excludes string-literal-only
                // matches because the JSON-descriptor case is chunked as
                // Constant/Statement, not Function. False positives are
                // possible if a Function chunk contains ONLY a string
                // literal of the query — a known edge case to revisit if
                // it emerges in production.
                //
                // Skipped when:
                //   * intent != Definition (struct_boost_tokens is empty)
                //   * chunk_type is not a struct- or function-like
                //     declaration (e.g. Constant, Statement, Docstring)
                //   * the chunk has no `function_name` (anonymous code)
                //   * no query token is a substring of the function name
                if !struct_boost_tokens.is_empty() {
                    if let Some(r) = raw {
                        let eligible = is_struct_definition_chunk_type(&r.chunk_type)
                            || is_function_definition_chunk_type(&r.chunk_type);
                        if eligible {
                            if let Some(name) = r.function_name.as_deref() {
                                let name_lower = name.to_ascii_lowercase();
                                if struct_boost_tokens
                                    .iter()
                                    .any(|t| name_lower.contains(t.as_str()))
                                {
                                    multiplier *= STRUCT_DEFINITION_BOOST;
                                }
                            }
                        }
                    }
                }
                // Branch-modified file boost (issue #122). Apply after the
                // file-type multiplier so doc-on-branch never out-ranks
                // source-on-branch when both files are in the diff.
                if let (Some(set), Some(r)) = (branch_files, raw) {
                    if set.contains(normalize_path(&r.file)) {
                        multiplier *= branch_boost;
                    }
                }
                (id, score * multiplier)
            })
            .collect();
        adjusted.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        adjusted
    }

    /// Issue #20: when intent is Definition or Unknown (a likely symbol
    /// lookup), inject the exact-name entity hit as the rank-1 BM25 result.
    ///
    /// Why: keeps the RRF lane seeing a strong signal even when the literal
    /// token didn't tokenize (e.g. underscore-heavy names). Lifting this out
    /// of `search` shrinks the latter's cyclomatic complexity.
    /// What: scoped to two intents; when an entity match is found, dedupes
    /// any prior occurrence and prepends a synthetic `(id, beta * 1.5)` pair.
    /// Test: covered by `test_entity_exact_match_struct_ranks_first`.
    async fn inject_entity_exact_match(
        &self,
        intent: &QueryIntent,
        query_text: &str,
        beta: f32,
        bm25_results: &mut Vec<(String, f32)>,
    ) {
        if !matches!(intent, QueryIntent::Definition | QueryIntent::Unknown) {
            return;
        }
        let Some(hit) = self.entity_exact_match(query_text).await else {
            return;
        };
        let injected_score = beta * 1.5;
        bm25_results.retain(|(id, _)| id != &hit);
        bm25_results.insert(0, (hit, injected_score));
    }

    /// MMR diversity pass (#28) over the RRF-fused candidate list.
    ///
    /// Why: re-ranks so adjacent near-duplicates don't crowd the top-k.
    /// λ=`DEFAULT_LAMBDA` (=0.5) balances relevance vs diversity.
    /// What: snapshots the embedding cache; if empty (BM25-only mode) falls
    /// back to the input order gracefully.
    /// Test: covered indirectly by every search integration test.
    async fn apply_mmr_rerank(
        &self,
        fused_raw: Vec<(String, f32)>,
        top_k: usize,
    ) -> Vec<(String, f32)> {
        // Snapshot only the candidate embeddings out of the LRU into a
        // transient `HashMap` for MMR. `peek` avoids promoting entries on
        // read (we only want the embed pipeline / batch commit to reorder
        // the LRU). Missing entries are handled gracefully by MMR — it
        // simply contributes zero diversity for that candidate.
        let emb_map = self.chunk_embeddings.read().await;
        if emb_map.is_empty() {
            return fused_raw;
        }
        let snapshot: HashMap<String, Vec<f32>> = fused_raw
            .iter()
            .filter_map(|(id, _)| emb_map.peek(id).map(|v| (id.clone(), v.clone())))
            .collect();
        drop(emb_map);
        crate::core::mmr::mmr_rerank(
            fused_raw,
            &snapshot,
            crate::core::mmr::DEFAULT_LAMBDA,
            top_k,
        )
    }

    /// KG expand the fused list when `use_kg_first` is on and the caller
    /// hasn't disabled `expand_graph`.
    ///
    /// Why: lifts the conditional and the "which-ids-came-only-from-KG"
    /// bookkeeping out of `search`.
    /// What: returns `(all_candidates, kg_only_ids)`. `all_candidates`
    /// starts as `fused` and is extended with KG-derived `(id, score)` pairs.
    ///
    /// When `refine_embedding` is `Some`, each KG-expanded neighbour is scored
    /// by cosine similarity against the refine query embedding (issue #147).
    /// Neighbours below [`KG_REFINE_THRESHOLD`] are dropped; the rest are
    /// reranked by their cosine score so the strongest semantic match floats to
    /// the top.  Seeds from the primary fused list are never filtered — only
    /// the newly-expanded KG neighbours are subject to the threshold.
    ///
    /// Test: covered by `test_kg_expansion_marks_neighbours_with_hybrid_kg`,
    /// `test_kg_expansion_disabled_by_expand_graph_false`,
    /// `test_kg_refine_query_filters_irrelevant_neighbours`, and
    /// `test_kg_refine_query_none_preserves_all_neighbours`.
    /// Why: exposed for direct unit-testing (issue #147) so tests can verify
    /// the refine-filter in isolation without going through the full search
    /// pipeline where HNSW may independently surface neighbours.
    /// What: thin visibility lift — same implementation, called from tests.
    /// Test: `test_kg_refine_query_filters_irrelevant_neighbours`.
    #[cfg(test)]
    pub(super) async fn expand_with_kg_for_test(
        &self,
        fused: Vec<(String, f32)>,
        intent: &QueryIntent,
        use_kg_first: bool,
        expand_graph: bool,
        refine_embedding: Option<&[f32]>,
    ) -> (Vec<(String, f32)>, std::collections::HashSet<String>) {
        self.expand_with_kg(fused, intent, use_kg_first, expand_graph, refine_embedding)
            .await
    }

    async fn expand_with_kg(
        &self,
        fused: Vec<(String, f32)>,
        intent: &QueryIntent,
        use_kg_first: bool,
        expand_graph: bool,
        refine_embedding: Option<&[f32]>,
    ) -> (Vec<(String, f32)>, std::collections::HashSet<String>) {
        let mut all = fused.clone();
        if !(use_kg_first && expand_graph) {
            return (all, std::collections::HashSet::new());
        }
        let mut expanded = self.kg_expand(&fused, intent.clone()).await;

        // Issue #147: when a refine embedding is provided, score each expanded
        // neighbour by cosine similarity to the refine query, drop those below
        // the threshold, and reorder by cosine score so the best semantic match
        // is nearest the front. The seed set (`fused`) is intentionally left
        // unfiltered — we don't want to drop a correct seed just because its
        // chunk text is not the most fluent match for the natural-language query.
        if let Some(refine_emb) = refine_embedding {
            let mut scored: Vec<(String, f32)> = Vec::with_capacity(expanded.len());
            for (id, _kg_score) in &expanded {
                // Look up the pre-computed embedding for this neighbour chunk.
                // Chunks that have no stored embedding (BM25-only indexes) get
                // a zero cosine score and are dropped at the threshold check.
                let cos = self
                    .get_embedding(id)
                    .map(|emb| cosine_similarity(refine_emb, &emb))
                    .unwrap_or(0.0);
                if cos >= KG_REFINE_THRESHOLD {
                    scored.push((id.clone(), cos));
                }
            }
            // Sort by cosine score descending (stable tie-break by id).
            scored.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
            expanded = scored;
        }

        let kg_ids: std::collections::HashSet<String> =
            expanded.iter().map(|(id, _)| id.clone()).collect();
        all.extend(expanded);
        (all, kg_ids)
    }

    /// Materialize the top-k `(id, score)` pairs into `CodeChunk`s with the
    /// correct `match_reason` derived from the source lanes.
    ///
    /// Why: isolates the final per-result loop (lookup table joins, snippet
    /// construction, RawChunk → CodeChunk) so `search` stays focused on
    /// orchestration.
    /// What: builds lookup sets for HNSW and BM25 hit IDs, then for each of
    /// the top-k `(id, score)` pairs picks a `match_reason` and emits a
    /// `CodeChunk` via `raw_to_code_chunk`.
    /// Test: covered by every search integration test.
    async fn materialize_search_results(
        &self,
        all: Vec<(String, f32)>,
        hnsw_results: &[(String, f32)],
        bm25_results: &[(String, f32)],
        kg_ids: &HashSet<String>,
        branch_files: Option<&HashSet<String>>,
        query: &SearchQuery,
    ) -> Vec<CodeChunk> {
        let in_hnsw: HashSet<&String> = hnsw_results.iter().map(|(id, _)| id).collect();
        let in_bm25: HashSet<&String> = bm25_results.iter().map(|(id, _)| id).collect();

        // Issue #28 deferred item: materialize the top-k results by batch
        // point-reading their text from the durable redb corpus rather than
        // the heap-resident `chunks` HashMap. Only the top-k ids are read —
        // a `top_k=20` query does ~20 mmap-backed point reads, served from the
        // OS page cache, so the in-memory corpus is no longer on the query
        // hot path.
        let top_k: Vec<(String, f32)> = all.into_iter().take(query.top_k).collect();
        let top_k_ids: Vec<String> = top_k.iter().map(|(id, _)| id.clone()).collect();
        let chunks = self.fetch_chunks_for_ids(&top_k_ids).await;
        let mut out = Vec::with_capacity(top_k.len());
        for (id, score) in top_k {
            let Some(raw) = chunks.get(&id) else {
                tracing::trace!("fused id {id} not in corpus — likely race; skipping");
                continue;
            };
            let match_reason = compute_match_reason(
                in_hnsw.contains(&id),
                in_bm25.contains(&id),
                kg_ids.contains(&id),
            );
            let snippet = if query.compact {
                Some(build_compact_snippet(&raw.content))
            } else {
                None
            };
            let mut chunk = raw_to_code_chunk(raw, score, match_reason, snippet, &self.root_path);
            if let Some(set) = branch_files {
                chunk.on_branch = set.contains(normalize_path(&raw.file));
            }
            out.push(chunk);
        }
        out
    }
}
