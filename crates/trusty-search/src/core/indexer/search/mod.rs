//! Hybrid search pipeline for [`CodeIndexer`].
//!
//! Why: the search hot path (query classify → embed → HNSW + BM25 → RRF → KG
//! expand → MMR diversity → materialize) is the most-read code in this crate
//! and benefits from living in a dedicated module away from ingest/persist.
//! What: `search` (the main orchestrator), `apply_archive_downrank`,
//! `apply_score_adjustments`, `inject_entity_exact_match`, `apply_mmr_rerank`,
//! and the free-function helpers `merge_grep_lane` / `resolve_branch_set`.
//! Per-lane fetch/embed helpers live in `lanes`; KG expansion in `kg`;
//! result materialisation in `materialize`.
//! Test: covered by every `test_search_*`, `test_kg_*`, and intent-routing
//! test in `indexer::tests`.

pub(crate) mod kg;
pub(crate) mod lanes;
pub(crate) mod materialize;

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::core::classifier::{QueryClassifier, QueryIntent};
use crate::core::git::{normalize_path, resolve_branch_files};
use crate::core::search::rrf::{rrf_fuse, RRF_K};

use super::archive::{self, MarkerCache};
use super::docs_penalty;
use super::{
    definition_boost_query_tokens, file_type_score_multiplier, is_function_definition_chunk_type,
    is_struct_definition_chunk_type, CodeChunk, CodeIndexer, SearchQuery, HNSW_OVERSAMPLE,
    STRUCT_DEFINITION_BOOST,
};

/// Score assigned to grep-fallback hits (issue #75). Intentionally tiny so
/// fallback rows never out-rank a real BM25/vector hit — they only surface
/// when the primary lanes returned nothing.
///
/// Why: calibrated so fallback rows are visible but never displace real hits.
/// What: a tiny positive constant well below any realistic RRF score.
/// Test: `test_grep_fallback_returns_substring_hits`.
pub(crate) const GREP_FALLBACK_SCORE: f32 = 0.001;

/// Minimum cosine similarity a KG-expanded neighbour must have against the
/// `refine_query` embedding before it is kept (issue #147).
///
/// Why: 0.4 is empirically safe — unrelated concepts typically score < 0.3;
/// semantically related code clusters score 0.5–1.0.
/// What: applied as `>= KG_REFINE_THRESHOLD` so boundary values are kept.
/// Test: `test_kg_refine_query_filters_irrelevant_neighbours`.
pub(crate) const KG_REFINE_THRESHOLD: f32 = 0.4;

/// Merge a grep lane into an already-RRF-fused list (issue #75).
///
/// Why: `rrf_fuse` is hard-coded to two lanes (HNSW + BM25). Rather than
/// reshape its signature, we fold the grep lane in via the same reciprocal
/// rank formula and re-truncate to `top_k`.
/// What: walks `grep_lane` (ordered best-first), adds `weight * 1/(k+rank)`
/// to each id's score, re-sorts by score desc with id as tie-break, truncates.
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

/// Resolve the effective branch-modified file set + clamped multiplier.
///
/// Why: extracted from `search` so the resolution rules are easy to read
/// and unit-test independently.
/// What: returns `(Some(set), boost)` when there is a non-empty file list and
/// the resolved boost is strictly greater than `1.0`; `(None, _)` otherwise.
/// Test: `test_branch_boost_clamped_to_3x` and
/// `test_no_boost_when_branch_files_absent`.
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
    /// Hybrid search: classify intent → route weights → HNSW + BM25 → RRF → KG.
    ///
    /// Why: the entry point for all search callers; orchestrates the full
    /// pipeline without doing any leaf-level data structure work itself.
    /// What: intent classification → lane selection → embed → BM25/HNSW
    /// dispatch → RRF fusion → MMR → KG expansion → score adjustments →
    /// materialisation → archive filter. Returns up to `query.top_k`
    /// `CodeChunk`s ranked by fused score.
    /// Test: covered by every `test_search_*` integration test.
    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<CodeChunk>> {
        self.touch_activity();
        let intent = QueryClassifier::classify_with_domain(&query.text, &self.domain_terms);
        let (alpha, beta, use_kg_first) = intent.weights();

        // Issue #73: intent-aware effective mode. Conceptual and Definition
        // intents both need docs, so when the caller used the default Code mode,
        // upgrade to All. Explicit Code from the caller still wins.
        let effective_mode = match (&intent, query.mode) {
            (QueryIntent::Conceptual, super::SearchMode::Code) => super::SearchMode::All,
            (QueryIntent::Definition, super::SearchMode::Code) => super::SearchMode::All,
            _ => query.mode,
        };

        tracing::debug!(
            "search index={} query={:?} intent={:?} alpha={} beta={} \
             mode={:?} effective_mode={:?}",
            self.index_id,
            query.text,
            intent,
            alpha,
            beta,
            query.mode,
            effective_mode
        );

        // Staged-pipeline lane selector (issue #109, Phase 1; extended #138).
        let lexical_only = matches!(query.stage, Some(super::SearchStage::Lexical));
        let semantic_lane = matches!(query.stage, Some(super::SearchStage::Semantic));
        let graph_lane = matches!(query.stage, Some(super::SearchStage::Graph));
        let force_kg = graph_lane;
        let skip_kg = lexical_only || semantic_lane;

        // 1) Embed (cache-first).
        let embedding = if lexical_only {
            None
        } else {
            self.embed_query(&query.text).await?
        };

        // 2) Run lanes (HNSW + BM25), then inject entity-exact-match.
        let want = query.top_k.saturating_mul(HNSW_OVERSAMPLE).max(query.top_k);
        let bm25_fut = self.bm25_search(&query.text, want);
        let hnsw_results = match &embedding {
            Some(v) => self.vector_search(v, want).await?,
            None => Vec::new(),
        };
        let mut bm25_results = bm25_fut.await?;
        self.inject_entity_exact_match(&intent, &query.text, beta, &mut bm25_results)
            .await;

        // 2a) Issue #75: for Definition intent, run grep as a third RRF lane.
        let grep_lane: Vec<(String, f32)> = if matches!(intent, QueryIntent::Definition) {
            self.grep_fallback_search(&query.text, want).await
        } else {
            Vec::new()
        };

        // 3) RRF fuse, then MMR diversity pass.
        let fused_raw = rrf_fuse(&hnsw_results, &bm25_results, alpha, beta, RRF_K, want);
        let fused_raw = merge_grep_lane(fused_raw, &grep_lane, beta, want);

        // 3a) Issue #75: empty-result fallback — scan chunk corpus for literal
        // substring match.
        let fused_raw = if fused_raw.is_empty() {
            self.grep_fallback_search(&query.text, want).await
        } else {
            fused_raw
        };
        let fused = self.apply_mmr_rerank(fused_raw, want).await;

        // 4) KG expand (conditional). Issue #147: embed `refine_query` for
        // KG-neighbourhood reranking.
        let refine_embedding: Option<Vec<f32>> = if skip_kg {
            None
        } else {
            match &query.refine_query {
                Some(rq) if !rq.is_empty() => self.embed_query(rq).await?,
                _ => None,
            }
        };
        let (all, kg_ids) = if skip_kg {
            (fused, HashSet::new())
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

        // 4a–c) Score adjustments: file-type, docs-penalty, struct boost,
        // branch boost.
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

        // 6) Mode-based hard file-type filter + archive downrank.
        self.apply_archive_downrank(&mut result, effective_mode, query.exclude_archived);
        Ok(result)
    }

    /// Apply the mode-based hard file-type filter and the archive score
    /// penalty.
    ///
    /// Why: two distinct concerns share the post-materialisation step: (1) the
    /// `SearchMode` filter drops chunks outside the allowed file-type set; (2)
    /// the archive penalty demotes archived/legacy/deprecated code. Combining
    /// them in one method keeps post-processing logic consolidated.
    /// What: (1) retains only chunks for which
    /// `docs_penalty::is_allowed_for_mode` returns `true` (skipped for `All`).
    /// (2) runs `archive::classify` per chunk, multiplies score by the
    /// penalty, stamps `archive_reason`. When `exclude_archived` is `true`,
    /// chunks with strong archive signals are dropped. Re-sorts by score desc.
    /// Test: `test_archive_downrank_demotes_deprecated_chunks`,
    /// `test_exclude_archived_drops_archive_chunks`, `test_mode_filter_*`.
    fn apply_archive_downrank(
        &self,
        results: &mut Vec<CodeChunk>,
        mode: super::SearchMode,
        exclude_archived: bool,
    ) {
        if results.is_empty() {
            return;
        }
        if matches!(mode, super::SearchMode::Code) {
            use crate::core::chunker::ChunkType;
            results.retain(|chunk| !matches!(chunk.chunk_type, ChunkType::Docstring));
        }
        results.retain(|chunk| docs_penalty::is_allowed_for_mode(&chunk.file, mode));

        let mut markers = MarkerCache::new();
        let mut archived_ids: HashSet<String> = HashSet::new();
        for chunk in results.iter_mut() {
            let (archive_mult, archive_reason_opt) =
                archive::classify(&self.root_path, &chunk.file, &chunk.content, &mut markers);
            let (_docs_mult, docs_reason_opt) = docs_penalty::doc_score_penalty(&chunk.file, mode);
            if archive_reason_opt.is_some() {
                chunk.score *= archive_mult;
            }
            if let Some(reason) = &archive_reason_opt {
                if exclude_archived && !reason.starts_with("stale:") {
                    archived_ids.insert(chunk.id.clone());
                }
            }
            if archive_reason_opt.is_some() || docs_reason_opt.is_some() {
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
    /// Why: (1) KG-expanded neighbours are appended after the RRF-fused list,
    /// so naïve `take(top_k)` in materialisation discards them (issue #94).
    /// (2) Issue #72: the mode-aware `doc_score_penalty` matrix must fire
    /// BEFORE `take(top_k)` so prose chunks can't crowd source matches out of
    /// the result list. (3) Issues #117/#122: struct/function definition boost.
    /// What: adjusts every candidate's score in a single pass and re-sorts
    /// before truncation.
    /// Test: `test_definition_demotes_markdown_below_source`,
    /// `test_kg_results_survive_top_k_truncation`,
    /// `test_struct_definition_boost_surfaces_struct_over_usage`.
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
        // Issue #117: for Definition-intent queries, boost chunks that are the
        // declaration of a type whose `function_name` matches a literal query
        // token. Pre-compute the lowercased token set once.
        let struct_boost_tokens: Vec<String> = if matches!(intent, QueryIntent::Definition) {
            definition_boost_query_tokens(query_text)
        } else {
            Vec::new()
        };
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
                // Issue #72: apply the mode-aware doc/source penalty BEFORE
                // top_k truncation.
                if let Some(r) = raw {
                    let (docs_mult, _) = docs_penalty::doc_score_penalty(&r.file, effective_mode);
                    multiplier *= docs_mult;
                }
                // Issues #117 / #122: Definition-intent structural/function boost.
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
                // Branch-modified file boost (issue #122).
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

    /// Issue #20: when intent is Definition or Unknown, inject the exact-name
    /// entity hit as the rank-1 BM25 result.
    ///
    /// Why: keeps the RRF lane seeing a strong signal even when the literal
    /// token didn't tokenize (e.g. underscore-heavy names).
    /// What: scoped to two intents; when an entity match is found, dedupes any
    /// prior occurrence and prepends a synthetic `(id, beta * 1.5)` pair.
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

    /// MMR diversity pass over the RRF-fused candidate list.
    ///
    /// Why: re-ranks so adjacent near-duplicates don't crowd the top-k.
    /// λ = `DEFAULT_LAMBDA` (0.5) balances relevance vs diversity.
    /// What: snapshots the embedding cache; falls back to input order when
    /// the cache is empty (BM25-only mode).
    /// Test: covered indirectly by every search integration test.
    async fn apply_mmr_rerank(
        &self,
        fused_raw: Vec<(String, f32)>,
        top_k: usize,
    ) -> Vec<(String, f32)> {
        let emb_map = self.chunk_embeddings.read().await;
        if emb_map.is_empty() {
            return fused_raw;
        }
        // Snapshot only the candidate embeddings out of the LRU. `peek` avoids
        // promoting entries on read.
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
}
