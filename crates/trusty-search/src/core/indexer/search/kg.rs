//! Knowledge-graph expansion helpers for the search pipeline.
//!
//! Why: KG expansion is a distinct algorithmic concern from lane search and
//! from the top-level orchestration in `search()`. Extracting it here keeps
//! the hot-path code reviewable and lets us unit-test the expansion in
//! isolation without going through the full search pipeline.
//! What: `kg_expand`, `expand_with_kg`, and the `#[cfg(test)]`-gated
//! `expand_with_kg_for_test` visibility shim.
//! Test: covered by `test_kg_expansion_marks_neighbours_with_hybrid_kg`,
//! `test_kg_expansion_disabled_by_expand_graph_false`,
//! `test_kg_refine_query_filters_irrelevant_neighbours`, and
//! `test_kg_refine_query_none_preserves_all_neighbours`.

use std::collections::{HashMap, HashSet};

use crate::core::classifier::QueryIntent;
use crate::core::mmr::cosine_similarity;

use super::super::{CodeIndexer, KG_EXPAND_HOPS};
use super::KG_REFINE_THRESHOLD;

impl CodeIndexer {
    /// Intent-gated KG expansion (issue #18).
    ///
    /// Why: for each seed `(chunk_id, score)`, the symbol graph exposes
    /// semantically adjacent code that the primary BM25 + HNSW lanes may not
    /// surface.
    /// What:
    ///   1. Look up the defining symbol of the seed chunk.
    ///   2. BFS its `EdgeKind`-filtered neighbourhood (intent-specific edges).
    ///   3. Score each neighbour as `seed_score * edge_kind.score_multiplier()`.
    ///
    /// Deduplicates: a chunk already in the seed set is never re-emitted; a
    /// chunk reachable through multiple seed/edge paths keeps its best score.
    /// Test: covered by `test_kg_expansion_marks_neighbours_with_hybrid_kg`.
    pub(super) async fn kg_expand(
        &self,
        seeds: &[(String, f32)],
        intent: QueryIntent,
    ) -> Vec<(String, f32)> {
        let graph = self.symbol_graph().await;
        if graph.node_count() == 0 || seeds.is_empty() {
            return Vec::new();
        }

        let edge_kinds = Self::edge_kinds_for_intent(intent);
        let seed_ids: HashSet<&String> = seeds.iter().map(|(id, _)| id).collect();
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

    /// KG expand the fused list when `use_kg_first` is on and the caller
    /// hasn't disabled `expand_graph`.
    ///
    /// Why: lifts the conditional and the "which-ids-came-only-from-KG"
    /// bookkeeping out of `search`.
    /// What: returns `(all_candidates, kg_only_ids)`. `all_candidates` starts
    /// as `fused` and is extended with KG-derived `(id, score)` pairs.
    /// When `refine_embedding` is `Some`, each KG-expanded neighbour is scored
    /// by cosine similarity against the refine query embedding (issue #147).
    /// Neighbours below [`KG_REFINE_THRESHOLD`] are dropped; seeds from the
    /// primary fused list are never filtered.
    /// Test: covered by `test_kg_expansion_marks_neighbours_with_hybrid_kg`,
    /// `test_kg_expansion_disabled_by_expand_graph_false`,
    /// `test_kg_refine_query_filters_irrelevant_neighbours`, and
    /// `test_kg_refine_query_none_preserves_all_neighbours`.
    pub(super) async fn expand_with_kg(
        &self,
        fused: Vec<(String, f32)>,
        intent: &QueryIntent,
        use_kg_first: bool,
        expand_graph: bool,
        refine_embedding: Option<&[f32]>,
    ) -> (Vec<(String, f32)>, HashSet<String>) {
        let mut all = fused.clone();
        if !(use_kg_first && expand_graph) {
            return (all, HashSet::new());
        }
        let mut expanded = self.kg_expand(&fused, intent.clone()).await;

        // Issue #147: when a refine embedding is provided, score each expanded
        // neighbour by cosine similarity to the refine query, drop those below
        // the threshold, and reorder by cosine score so the best semantic match
        // floats to the top. The seed set (`fused`) is intentionally left
        // unfiltered.
        if let Some(refine_emb) = refine_embedding {
            let mut scored: Vec<(String, f32)> = Vec::with_capacity(expanded.len());
            for (id, _kg_score) in &expanded {
                // Chunks with no stored embedding (BM25-only indexes) get a
                // zero cosine score and are dropped at the threshold check.
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

        let kg_ids: HashSet<String> = expanded.iter().map(|(id, _)| id.clone()).collect();
        all.extend(expanded);
        (all, kg_ids)
    }

    /// Visibility shim for direct unit-testing of `expand_with_kg` (issue #147).
    ///
    /// Why: `expand_with_kg` is `pub(super)` so tests outside the `search`
    /// submodule can't call it directly. This shim exposes it at `pub(super)`
    /// under a test-only gate.
    /// What: thin delegation — same implementation, different visibility.
    /// Test: `test_kg_refine_query_filters_irrelevant_neighbours`.
    #[cfg(test)]
    pub(crate) async fn expand_with_kg_for_test(
        &self,
        fused: Vec<(String, f32)>,
        intent: &QueryIntent,
        use_kg_first: bool,
        expand_graph: bool,
        refine_embedding: Option<&[f32]>,
    ) -> (Vec<(String, f32)>, HashSet<String>) {
        self.expand_with_kg(fused, intent, use_kg_first, expand_graph, refine_embedding)
            .await
    }
}
