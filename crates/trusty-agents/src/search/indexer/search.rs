//! The [`CodeIndexer`] read path: query embedding cache, vector search,
//! hybrid RRF fusion, and knowledge-graph expansion.
//!
//! Why: Keeping retrieval in its own file lets the write path (`index.rs`)
//! stay focused on construction + ingestion while this module owns ranking.
//! What: A second `impl CodeIndexer` block providing `embed_query_cached`,
//! `search`, `search_hybrid`, `expand_with_graph`, and `search_filtered`.
//! Test: See the `tests` submodule of the parent `indexer` module —
//! `search_returns_code_chunk_with_metadata`,
//! `search_hybrid_promotes_lexical_match`,
//! `search_hybrid_expansion_appends_related_chunks`, and the agentconfig
//! boost test.

use std::sync::Arc;

use anyhow::{Context, Result};

use crate::context::bm25::Bm25Index;
use crate::context::indexer::tokenize;
use crate::memory::Segment;
use crate::search::indexer::{CodeChunk, CodeIndexer, RRF_K};
use crate::search::query_classifier::{ClassifiedQuery, classify_query};

impl CodeIndexer {
    /// Embed `query` with an LRU cache so repeated queries skip the
    /// FastEmbedder cost (#376 D2).
    ///
    /// Why: Within a session the same query gets re-issued — by the user
    /// iterating, the LLM retrying, or the daemon serving multiple
    /// concurrent agents. Caching is correct because the embedder is
    /// deterministic and the cached value is invariant across queries.
    /// What: Looks up `query` in the LRU. On hit, returns the clone
    /// directly (sub-microsecond). On miss, runs the existing
    /// `spawn_blocking(embed_single)` path and inserts the result.
    /// Test: Indirectly via the bench (`hybrid_vs_ripgrep_benchmark`)
    /// and by `search_hybrid_promotes_lexical_match`.
    pub(crate) async fn embed_query_cached(&self, query: &str) -> Result<Vec<f32>> {
        if let Some(hit) = self.query_cache.lock().await.get(query) {
            return Ok(hit.clone());
        }
        let embedder = Arc::clone(&self.embedder);
        let q = query.to_string();
        let vec = tokio::task::spawn_blocking(move || embedder.embed_single(&q))
            .await
            .context("embed task panicked")??;
        self.query_cache
            .lock()
            .await
            .put(query.to_string(), vec.clone());
        Ok(vec)
    }

    /// Semantic search over the code index.
    ///
    /// Why: The whole point of the indexer is to answer `"where is X?"`
    /// with ranked chunks. This is the read path.
    /// What: Embeds `query`, calls `store.search(Segment::CodeIndex, ...)`,
    /// deserializes each payload into a [`CodeChunk`], and sets `score`
    /// from the raw result.
    /// Test: `search_returns_code_chunk_with_metadata`.
    pub async fn search(&self, query: &str, top_k: usize) -> Result<Vec<CodeChunk>> {
        // Warm-up gate: bumps last_access and (if the index was evicted by
        // the cool-down task) reloads it from disk before the query runs.
        // The reload itself logs a single info line; here we stay quiet so
        // a hot path doesn't spam logs.
        self.ensure_warm().await?;
        let vec = self.embed_query_cached(query).await?;
        // Ask for extra results so we can drop manifest rows without
        // shrinking the final result set below `top_k`.
        let pull = top_k.saturating_mul(2).max(top_k + 4);
        let hits = self.store.search(Segment::CodeIndex, &vec, pull).await?;
        let mut out = Vec::with_capacity(hits.len());
        for hit in hits {
            // Manifest entries share the segment but aren't real chunks;
            // skip them by id prefix.
            if hit.id.starts_with("manifest:") {
                continue;
            }
            let mut chunk: CodeChunk = serde_json::from_value(hit.payload)
                .context("failed to deserialize CodeChunk payload")?;
            chunk.score = hit.score;
            chunk.match_reason = "vector".to_string();
            // Boost `agentconfig` chunks so root-level AGENTS.md /
            // CLAUDE.md surface first for agent/task/workflow queries.
            // Cap at 1.0 to preserve the "score is a similarity in
            // [0, 1]" invariant downstream formatters rely on.
            if chunk.language == "agentconfig" {
                chunk.score = (chunk.score * 1.1).min(1.0);
            }
            out.push(chunk);
        }
        // Re-sort after boosting so promoted chunks rank ahead of
        // equal-raw-score markdown/code siblings.
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out.truncate(top_k);
        Ok(out)
    }

    /// Hybrid code search: vector recall + BM25 lexical re-ranking via RRF,
    /// optionally followed by a knowledge-graph expansion pass.
    ///
    /// Why: Vector search alone misses exact-token matches (struct names,
    /// CLI flags, error strings). BM25 alone misses paraphrases. Reciprocal
    /// Rank Fusion (RRF) combines both rankings without needing the score
    /// distributions to be commensurable — a parameter-free approach that
    /// consistently outperforms either signal alone in production search
    /// engines (Elastic, Vespa, Weaviate). The graph expansion pass (#376)
    /// pulls in callers/callees of the top-K matches so a single match on
    /// `foo` also surfaces the functions that drive or depend on it.
    /// What: Pulls `4 * top_k` candidates from the vector index, builds a
    /// fresh `Bm25Index` over their text, then computes
    /// `rrf = alpha/(k + rank_vector) + beta/(k + rank_bm25)` for each
    /// candidate (k=60). The (alpha, beta) weights come from
    /// [`classify_query`] so a Definition query leans BM25, a Conceptual
    /// query leans the embedding signal, etc. (#376 B2). When
    /// `expand_graph` is true, looks up callers/callees of each top-K
    /// chunk's function name in the SymbolGraph and appends matching
    /// chunks (scored at 70% of the triggering chunk's RRF) to the result
    /// set. Returns the top `top_k` by combined score with original RRF
    /// hits taking priority on ties.
    /// Test: `search_hybrid_promotes_lexical_match`,
    /// `search_hybrid_expansion_appends_related_chunks`.
    pub async fn search_hybrid(
        &self,
        query: &str,
        top_k: usize,
        expand_graph: bool,
    ) -> Result<Vec<CodeChunk>> {
        // 0. Classify the query so the fusion weighting matches intent
        //    (#376 B2). For Definition/BugDebt queries we want to lean
        //    on BM25; for Conceptual queries the embedding signal wins.
        let classified: ClassifiedQuery = classify_query(query);
        let alpha = classified.vector_weight;
        let beta = classified.bm25_weight;
        tracing::debug!(
            intent = ?classified.intent,
            alpha,
            beta,
            "search_hybrid: classified query"
        );

        // 1. Pull a wider vector candidate set so BM25 has room to re-rank.
        //    Without this expansion, hybrid degrades to plain vector search.
        let pull = top_k.saturating_mul(4).max(top_k + 4);
        let vector_candidates = self.search(query, pull).await?;
        if vector_candidates.is_empty() {
            return Ok(Vec::new());
        }

        // 2. Build a per-candidate BM25 index. Tokens come from the existing
        //    project tokenizer so query and corpus terms agree on normalization
        //    (lowercase, alphanumeric splits, drop tokens ≤2 chars).
        let mut bm25 = Bm25Index::new();
        let mut chunk_ids: Vec<String> = Vec::with_capacity(vector_candidates.len());
        for chunk in &vector_candidates {
            // Stable ID across the two ranked lists; combining file +
            // start_line + end_line matches how the store keys chunks
            // (#376 A4).
            let id = format!(
                "{}:{}:{}",
                chunk.file.display(),
                chunk.start_line,
                chunk.end_line
            );
            // Include function name tokens too — BM25 should reward exact
            // identifier matches in `fn foo()` even when the body is short.
            let mut text = chunk.text.clone();
            if let Some(name) = &chunk.function_name {
                text.push(' ');
                text.push_str(name);
            }
            let terms = tokenize(&text);
            bm25.add_doc(id.clone(), terms);
            chunk_ids.push(id);
        }
        let query_terms = tokenize(query);
        let bm25_scored = bm25.score(&query_terms);

        // 3. Compute rank maps (1-indexed) and keep raw BM25 scores around
        //    so we can use them as a tiebreaker. A missing entry (zero BM25
        //    score) is treated as rank `len + 1` so it still contributes a
        //    small reciprocal but ranks below any matching doc.
        use std::collections::HashMap;
        let mut bm25_rank: HashMap<String, usize> = HashMap::new();
        let mut bm25_raw: HashMap<String, f32> = HashMap::new();
        for (rank, (id, score)) in bm25_scored.iter().enumerate() {
            bm25_rank.insert(id.clone(), rank + 1);
            bm25_raw.insert(id.clone(), *score);
        }
        let absent_rank = vector_candidates.len() + 1;

        // 4. Compute RRF score per candidate and rebuild the chunk list with
        //    that score so the final ordering is RRF-driven. We track the
        //    raw BM25 score alongside as a tiebreaker for the common case
        //    where two chunks happen to flip rank between the vector and
        //    lexical lists and end up with identical RRF (e.g. ranks (1,2)
        //    vs (2,1)) — we'd rather promote the one BM25 actually scored
        //    higher than leave the result order to the prior vector rank.
        let mut scored: Vec<(f32, f32, CodeChunk)> = vector_candidates
            .into_iter()
            .enumerate()
            .map(|(idx, mut chunk)| {
                let id = &chunk_ids[idx];
                let r_vec = idx + 1; // vector candidates are already ranked
                let r_bm = *bm25_rank.get(id).unwrap_or(&absent_rank);
                let rrf = alpha / (RRF_K + r_vec as f32) + beta / (RRF_K + r_bm as f32);
                let bm = *bm25_raw.get(id).unwrap_or(&0.0);
                chunk.score = rrf;
                chunk.match_reason = "hybrid".to_string();
                (rrf, bm, chunk)
            })
            .collect();

        // 5. Sort descending by RRF, breaking ties by raw BM25 score so a
        //    lexical-strong chunk wins when fusion produces a numerical tie.
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        // Collect the *full* re-ranked candidate list (not just top_k) so
        // graph expansion can look up sibling chunks that ranked just
        // below the cutoff but match a caller/callee of a primary hit.
        let all_ranked: Vec<CodeChunk> = scored.into_iter().map(|(_, _, c)| c).collect();
        let primary: Vec<CodeChunk> = all_ranked.iter().take(top_k).cloned().collect();

        if !expand_graph || primary.is_empty() {
            return Ok(primary);
        }

        // 6. Knowledge-graph expansion (#376 B1). For each top-K chunk
        //    with a function name, look up callers + callees in the
        //    SymbolGraph derived from that chunk's source file. Append
        //    matching chunks at 70% of the triggering chunk's RRF.
        Ok(self.expand_with_graph(primary, &all_ranked, top_k, &classified))
    }

    /// Look up callers/callees of each chunk's function in a per-file
    /// SymbolGraph and append those expansions at 70% of the trigger
    /// chunk's score (#376 B1).
    ///
    /// Why: A semantic match on `process_request` is much more useful
    /// when the result also surfaces what calls it and what it calls.
    /// 70% scoring keeps the original RRF order on top while letting
    /// strong expansions outrank weaker primary hits when warranted.
    /// What: Builds a `SymbolGraph` per unique source file (cached for
    /// the duration of this call), looks up `callers_of` + `callees_of`
    /// for each function, and matches them back to existing primary
    /// chunks by `function_name + file`. De-duplicates against the
    /// primary set. Caps the expansion at `top_k` extra hits.
    /// Test: `search_hybrid_expansion_appends_related_chunks`.
    fn expand_with_graph(
        &self,
        primary: Vec<CodeChunk>,
        all_ranked: &[CodeChunk],
        top_k: usize,
        classified: &ClassifiedQuery,
    ) -> Vec<CodeChunk> {
        use std::collections::{HashMap, HashSet};
        use std::path::PathBuf;
        use trusty_common::symgraph::graph::SymbolGraph;

        // Cache per-file graphs to avoid rebuilding when multiple top-K
        // hits live in the same file.
        let mut graph_cache: HashMap<PathBuf, Option<SymbolGraph>> = HashMap::new();
        // Build a name -> chunk map across the *full* re-ranked candidate
        // set (not just top-K) so expansion can resolve neighbours that
        // ranked just below the cutoff. Index uses (file, function_name).
        let mut by_name: HashMap<(PathBuf, String), CodeChunk> = HashMap::new();
        for c in all_ranked {
            if let Some(name) = &c.function_name {
                by_name
                    .entry((c.file.clone(), name.clone()))
                    .or_insert_with(|| c.clone());
            }
        }
        let mut seen: HashSet<(PathBuf, usize, usize)> = primary
            .iter()
            .map(|c| (c.file.clone(), c.start_line, c.end_line))
            .collect();

        let mut expansions: Vec<CodeChunk> = Vec::new();
        for trigger in &primary {
            let Some(fn_name) = trigger.function_name.as_ref() else {
                continue;
            };
            let entry = graph_cache
                .entry(trigger.file.clone())
                .or_insert_with(|| SymbolGraph::build_from_file(&trigger.file).ok());
            let Some(graph) = entry.as_ref() else {
                continue;
            };

            let mut neighbours: Vec<&trusty_common::symgraph::graph::SymbolNode> = Vec::new();
            neighbours.extend(graph.callers_of(fn_name));
            neighbours.extend(graph.callees_of(fn_name));

            for node in neighbours {
                let key = (node.file.clone(), node.name.clone());
                let Some(neighbour) = by_name.get(&key) else {
                    continue;
                };
                let dedup_key = (
                    neighbour.file.clone(),
                    neighbour.start_line,
                    neighbour.end_line,
                );
                if seen.contains(&dedup_key) {
                    continue;
                }
                seen.insert(dedup_key);
                let mut hit = neighbour.clone();
                hit.score = trigger.score * 0.7;
                hit.match_reason = "hybrid+kg".to_string();
                expansions.push(hit);
            }
        }

        // Trace for observability without spamming production logs.
        if !expansions.is_empty() {
            tracing::debug!(
                intent = ?classified.intent,
                primary = primary.len(),
                expansions = expansions.len(),
                "search_hybrid: graph expansion appended hits"
            );
        }

        // Combine: primary first (preserve RRF order), then expansions
        // sorted by their (already-discounted) score. Cap at `top_k +
        // top_k` total to bound payload size as the spec calls for.
        expansions.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut out = primary;
        let cap = top_k.saturating_mul(2);
        for e in expansions {
            if out.len() >= cap {
                break;
            }
            out.push(e);
        }
        out
    }

    /// Search with an optional language filter.
    ///
    /// Why: Power users often want to scope results to one language
    /// (`"rust"`, `"python"`, ...); filtering in-memory after a larger
    /// top-k pull is simple and correct for the current index sizes.
    /// What: Calls [`search`](CodeIndexer::search) with `top_k`, then retains
    /// only chunks whose `language` equals `language` (if provided).
    /// Test: Exercised indirectly by `search_returns_code_chunk_with_metadata`.
    pub async fn search_filtered(
        &self,
        query: &str,
        top_k: usize,
        language: Option<&str>,
    ) -> Result<Vec<CodeChunk>> {
        let hits = self.search(query, top_k).await?;
        let Some(lang) = language else {
            return Ok(hits);
        };
        Ok(hits.into_iter().filter(|c| c.language == lang).collect())
    }
}
