//! Fan-out routing strategy and code-similarity search handler.
//!
//! Why: Routing-mode logic (All / TopN / Threshold) determines which indexes
//! receive a `POST /search` fan-out; separating it from the handler body
//! keeps `search.rs` focused on per-request dispatch.
//! What: `RoutingMode` enum + `compute_context_weights` helper +
//! `SearchSimilarRequest` + `search_similar_handler`.
//! Test: `routing_mode_all_preserves_every_index_with_weights` and siblings.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::core::registry::IndexId;

use super::search::GlobalSearchRequest;
use super::state::SearchAppState;

#[derive(Debug, Clone, Copy)]
pub(super) enum RoutingMode {
    /// Search every index; multiply each lane's RRF scores by the index's
    /// context cosine similarity (indexes with no context use 1.0).
    All,
    /// Search only the top-N indexes by cosine similarity. Weights are not
    /// applied to lane scores (selection already encodes relevance).
    TopN(usize),
    /// Search only indexes whose cosine similarity ≥ threshold. Weights are
    /// not applied to lane scores.
    Threshold(f32),
}

impl RoutingMode {
    pub(super) const DEFAULT_TOP_N: usize = 3;
    const DEFAULT_THRESHOLD: f32 = 0.3;

    pub(super) fn from_request(req: &GlobalSearchRequest) -> Self {
        match req.routing.as_deref() {
            Some("top_n") => Self::TopN(req.routing_n.unwrap_or(Self::DEFAULT_TOP_N).max(1)),
            Some("threshold") => {
                Self::Threshold(req.routing_threshold.unwrap_or(Self::DEFAULT_THRESHOLD))
            }
            // "all" or anything else (or absent) defaults to All.
            _ => Self::All,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::TopN(_) => "top_n",
            Self::Threshold(_) => "threshold",
        }
    }

    /// Filter `index_ids` according to the strategy and return the active
    /// id list plus the per-id weight map the lane builder will consult.
    ///
    /// - `All`: every id is active; weight = its cosine similarity.
    /// - `TopN`: top N by cosine similarity; weight = 1.0 for selected ids.
    /// - `Threshold`: cosine ≥ threshold; weight = 1.0 for selected ids.
    pub(super) fn apply(
        self,
        index_ids: &[IndexId],
        weights: &std::collections::HashMap<IndexId, f32>,
    ) -> (Vec<IndexId>, std::collections::HashMap<IndexId, f32>) {
        match self {
            Self::All => {
                let active: Vec<IndexId> = index_ids.to_vec();
                let map: std::collections::HashMap<IndexId, f32> = index_ids
                    .iter()
                    .map(|id| (id.clone(), weights.get(id).copied().unwrap_or(1.0)))
                    .collect();
                (active, map)
            }
            Self::TopN(n) => {
                let mut ranked: Vec<(&IndexId, f32)> = index_ids
                    .iter()
                    .map(|id| (id, weights.get(id).copied().unwrap_or(1.0)))
                    .collect();
                ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                let active: Vec<IndexId> =
                    ranked.iter().take(n).map(|(id, _)| (*id).clone()).collect();
                let map: std::collections::HashMap<IndexId, f32> =
                    active.iter().map(|id| (id.clone(), 1.0)).collect();
                (active, map)
            }
            Self::Threshold(t) => {
                let active: Vec<IndexId> = index_ids
                    .iter()
                    .filter(|id| weights.get(id).copied().unwrap_or(1.0) >= t)
                    .cloned()
                    .collect();
                let map: std::collections::HashMap<IndexId, f32> =
                    active.iter().map(|id| (id.clone(), 1.0)).collect();
                (active, map)
            }
        }
    }
}

/// Embed the query once and compute cosine similarity against every index's
/// stored `context_embedding` (issue #112).
///
/// Why: the fan-out router needs a single relevance score per index. Indexes
/// without a context embedding (no recognised metadata, embedder unavailable
/// during last reindex) default to a neutral 1.0 so they participate
/// normally — the absence of a fingerprint is not a relevance signal.
/// What: returns a `HashMap<IndexId, f32>` where every id in `index_ids` has
/// an entry; the value is either `cosine_similarity(query, context)` or
/// `1.0` for indexes with no context. Failures embedding the query (e.g.
/// embedder not wired) also fall back to 1.0 across the board so the global
/// search keeps working as a plain fan-out.
pub(super) async fn compute_context_weights(
    registry: &crate::core::registry::IndexRegistry,
    index_ids: &[IndexId],
    query: &str,
) -> std::collections::HashMap<IndexId, f32> {
    use crate::core::mmr::cosine_similarity;

    // Try to obtain a query embedding from any index that has an embedder
    // wired. Every index in the registry shares the same machine-wide
    // FastEmbedder, so the first successful embed is reused for all.
    let mut query_embedding: Option<Vec<f32>> = None;
    for id in index_ids {
        let Some(handle) = registry.get(id) else {
            continue;
        };
        let indexer = handle.indexer.read().await;
        match indexer.embed_text(query).await {
            Ok(Some(vec)) => {
                query_embedding = Some(vec);
                break;
            }
            Ok(None) => continue,
            Err(e) => {
                tracing::debug!("context_routing: embed_text failed on {}: {e}", id.0);
                continue;
            }
        }
    }

    let mut out = std::collections::HashMap::with_capacity(index_ids.len());
    let Some(q) = query_embedding else {
        // Couldn't embed at all — fall back to neutral weights everywhere.
        for id in index_ids {
            out.insert(id.clone(), 1.0);
        }
        return out;
    };

    for id in index_ids {
        let Some(handle) = registry.get(id) else {
            out.insert(id.clone(), 1.0);
            continue;
        };
        let ctx_guard = handle.context_embedding.read().await;
        let weight = match ctx_guard.as_ref() {
            Some(ctx) if ctx.len() == q.len() => cosine_similarity(&q, ctx).max(0.0),
            _ => 1.0,
        };
        out.insert(id.clone(), weight);
    }
    out
}

/// Body for `POST /indexes/:id/search_similar`.
///
/// Why: code-to-code similarity (issue #31). The caller knows the *file +
/// optional function name* of the chunk they want to find neighbours of, not
/// its synthetic chunk id.
#[derive(Deserialize)]
pub struct SearchSimilarRequest {
    pub file: String,
    #[serde(default)]
    pub function: Option<String>,
    #[serde(default = "default_similar_top_k")]
    pub top_k: usize,
}

fn default_similar_top_k() -> usize {
    10
}

/// Handle `POST /indexes/:id/search_similar`.
///
/// Why (issue #484): the original implementation returned 404 whenever the
/// embedding-LRU cache missed, which always happens for `skip_kg=true` indexes
/// (the cache is populated only at commit time; entries age out and are never
/// restored). Re-embedding the seed chunk's text via `embed_text` when the
/// cache misses lets `search_similar` work on any index regardless of KG mode.
/// What: looks up the seed chunk's embedding from the LRU cache; on miss,
/// fetches the chunk's raw content and re-embeds it; falls through to 404 only
/// when neither path can produce an embedding (BM25-only index or unknown
/// chunk).
/// Test: `search_similar_fallback_reembeds_when_cache_misses` in the server
/// integration tests.
pub(super) async fn search_similar_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<SearchSimilarRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let index_id = IndexId::new(id);
    let handle = state.registry.get(&index_id).ok_or(StatusCode::NOT_FOUND)?;
    let started = std::time::Instant::now();
    let indexer = handle.indexer.read().await;
    let chunk_id = indexer
        .find_chunk_id(&req.file, req.function.as_deref())
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    // Issue #484: the LRU embedding cache misses for skip_kg=true indexes
    // (entries are only written at reindex time and are evicted under memory
    // pressure).  When the cache misses, fetch the chunk's text and re-embed
    // it so search_similar works on any index regardless of KG mode.
    let embedding = if let Some(cached) = indexer.get_embedding(&chunk_id) {
        cached
    } else {
        let content = indexer
            .chunk_content_by_id(&chunk_id)
            .await
            .ok_or(StatusCode::NOT_FOUND)?;
        indexer
            .embed_text(&content)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::NOT_FOUND)? // BM25-only: no embedder wired
    };
    let results = indexer
        .similar_by_embedding(&embedding, req.top_k, Some(&chunk_id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let latency_ms = started.elapsed().as_millis() as u64;
    Ok(Json(serde_json::json!({
        "results": results,
        "seed_chunk_id": chunk_id,
        "latency_ms": latency_ms,
    })))
}
