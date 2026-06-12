//! Global (cross-project) fan-out search handler (`POST /search`).
//!
//! Why: extracted from `search.rs` to keep both files under the 500-line cap
//! (issue #993 added ~82 lines to `search.rs`; the global handler is an
//! independent concern with no shared state with the per-index path).
//! What: `GlobalSearchRequest` and `global_search_handler`. Per-index search
//! lives in `search.rs`; routing helpers live in `routing.rs`.
//! Test: `test_global_search_fans_out_and_merges` in `server.rs#tests`.
use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use std::sync::Arc;

use crate::core::{classifier::QueryClassifier, indexer::SearchQuery, registry::IndexId};

use super::routing::{compute_context_weights, RoutingMode};
use super::state::SearchAppState;

/// Body for the global `POST /search` endpoint (issue #10 — cross-project
/// search fan-out).
///
/// Why: callers (LLM agents, the UI search bar) often don't know which
/// project an answer lives in. A single fan-out search across every
/// registered index, with results re-ranked via Reciprocal Rank Fusion, lets
/// them ask one question and get one merged answer.
#[derive(Deserialize)]
pub struct GlobalSearchRequest {
    pub query: String,
    #[serde(default = "default_global_top_k")]
    pub top_k: usize,
    /// When true, response chunks include the full `content` field. When
    /// false (default), the daemon still returns chunks with content — clients
    /// that want compact responses can read `compact_snippet`.
    #[serde(default)]
    pub full_content: bool,
    /// Optional allow-list of index ids to fan out to (issue #110). When
    /// present, only the named indexes are searched; unknown ids are
    /// silently skipped (logged at debug). When absent / empty, the daemon
    /// fans out to every registered index (legacy behaviour).
    #[serde(default)]
    pub indexes: Option<Vec<String>>,

    /// Fan-out routing strategy (issue #112). Controls how the daemon
    /// weights or filters the per-index lanes by cosine similarity between
    /// the query embedding and each index's stored `context_embedding`.
    ///
    /// - `"all"` (default): every index is searched; each index's RRF lane
    ///   is multiplied by its cosine similarity weight (indexes with no
    ///   context embedding use the neutral 1.0).
    /// - `"top_n"`: only the top-N indexes (by cosine similarity) are
    ///   searched; `routing_n` controls N (default 3).
    /// - `"threshold"`: indexes with cosine similarity below
    ///   `routing_threshold` (default 0.3) are skipped.
    #[serde(default)]
    pub routing: Option<String>,
    /// Number of indexes to keep for `routing = "top_n"`. Default 3.
    #[serde(default)]
    pub routing_n: Option<usize>,
    /// Cosine-similarity cutoff for `routing = "threshold"`. Default 0.3.
    #[serde(default)]
    pub routing_threshold: Option<f32>,
}

fn default_global_top_k() -> usize {
    10
}

/// `POST /search` — fan-out hybrid search across every registered index.
///
/// Why: see [`GlobalSearchRequest`] doc. This is distinct from
/// `POST /indexes/:id/search`, which targets a single index.
/// What: runs per-index search concurrently, tags each result with its
/// `index_id`, then re-runs RRF (k=60) over the per-index ranked lists
/// (each index treated as an equally-weighted lane) and returns the top-k
/// merged results. Indexes that error during search are skipped (logged) so
/// one bad index doesn't take down the whole fan-out.
///
/// PR #1103 correctness: with selective warm-boot, `registry.list()` returns
/// only HOT (eagerly loaded) indexes. Cold indexes parked in `ColdIndexStore`
/// are NOT searched, and the response now carries `cold_indexes_skipped` so
/// callers know the fan-out is incomplete. Lazy-loading all cold indexes during
/// fan-out is deliberately avoided (too expensive); callers that need full
/// coverage should trigger per-index loads first or wait for selective warm-boot
/// to complete. A follow-up issue should track "opt-in load-all on global
/// search" as a deeper semantic decision.
///
/// Test: `test_global_search_fans_out_and_merges` registers two indexes,
/// indexes a file into each, and asserts both contribute results tagged with
/// the right `index_id`. `test_global_search_surfaces_cold_indexes_skipped`
/// asserts the count is > 0 when cold indexes exist.
pub(super) async fn global_search_handler(
    State(state): State<Arc<SearchAppState>>,
    Json(req): Json<GlobalSearchRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Issue #882: reject empty / whitespace-only queries before fan-out.
    if req.query.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "query must not be empty" })),
        ));
    }

    use crate::core::search::rrf::{rrf_fuse, RRF_K};

    // PR #1103: `registry.list()` returns only HOT indexes. Cold indexes parked
    // in `cold_store` are NOT searched. Count them so callers know the fan-out
    // may be incomplete when `cold_indexes_skipped > 0`.
    let cold_indexes_skipped: usize = if let Some(requested) = req.indexes.as_ref() {
        // Restricted fan-out: count cold entries whose id was requested.
        state
            .cold_store
            .count_matching(requested.iter().map(|s| s.as_str()))
    } else {
        // Global fan-out: every cold entry is implicitly requested but skipped.
        state.cold_store.len()
    };

    let all_ids = state.registry.list();
    // Issue #110: when caller supplies `indexes`, restrict fan-out to that
    // set. Unknown ids are dropped here (the per-index branch below would
    // emit a 404; we'd rather silently skip so a stale caller doesn't
    // poison an otherwise-good fan-out).
    let index_ids: Vec<IndexId> = if let Some(requested) = req.indexes.as_ref() {
        let allow: std::collections::HashSet<&str> = requested.iter().map(|s| s.as_str()).collect();
        all_ids
            .into_iter()
            .filter(|id| allow.contains(id.0.as_str()))
            .collect()
    } else {
        all_ids
    };
    let total_indexes = index_ids.len();
    if index_ids.is_empty() {
        return Ok(Json(serde_json::json!({
            "results": Vec::<crate::core::indexer::CodeChunk>::new(),
            "indexes_searched": Vec::<String>::new(),
            "total_indexes": 0_usize,
            "cold_indexes_skipped": cold_indexes_skipped,
            "latency_ms": 0_u64,
            "intent": format!("{:?}", QueryClassifier::classify(&req.query)),
        })));
    }

    let started = std::time::Instant::now();
    let intent = QueryClassifier::classify(&req.query);

    // Issue #112: compute per-index context weights, then apply the routing
    // strategy to decide which indexes participate in the fan-out.
    let routing_mode = RoutingMode::from_request(&req);
    let weights = compute_context_weights(&state.registry, &index_ids, &req.query).await;
    let (mut active_ids, mut weight_map) = routing_mode.apply(&index_ids, &weights);

    // Issue #404 — nested-index fan-out (MVP):
    // 1. Derive the index hierarchy from root_path prefix containment.
    // 2. For `threshold` routing: include any sub-index whose parent is active,
    //    even if the sub-index's own cosine similarity falls below the threshold.
    //    This prevents small-subtree indexes from being silently excluded when
    //    the parent is clearly relevant.
    //
    // Note: when the caller supplies an explicit `indexes: [...]` restriction,
    // the set is treated as flat peers (no hierarchy applied) to preserve the
    // existing precision-override semantics.
    let hierarchy = if req.indexes.is_none() {
        let h = crate::core::search::hierarchy::IndexHierarchy::from_registry(
            &state.registry,
            &index_ids,
        );
        if matches!(routing_mode, RoutingMode::Threshold(_)) && !h.parent_of.is_empty() {
            let inactive_ids: Vec<IndexId> = index_ids
                .iter()
                .filter(|id| !weight_map.contains_key(id))
                .cloned()
                .collect();
            crate::core::search::hierarchy::apply_threshold_child_inclusion(
                &inactive_ids,
                &mut active_ids,
                &mut weight_map,
                &h,
            );
        }
        h
    } else {
        crate::core::search::hierarchy::IndexHierarchy::default()
    };

    let routing_label = routing_mode.label().to_string();
    let routing_decisions: Vec<serde_json::Value> = index_ids
        .iter()
        .map(|id| {
            let w = weights.get(id).copied().unwrap_or(1.0);
            let included = weight_map.contains_key(id);
            serde_json::json!({
                "index_id": id.0,
                "cosine_similarity": w,
                "included": included,
            })
        })
        .collect();

    // Build the same SearchQuery shape every per-index search uses. We
    // oversample per-index by passing the user's top_k unchanged: each lane
    // contributes up to top_k candidates, then RRF picks the best top_k
    // overall.
    let per_index_query = SearchQuery {
        text: req.query.clone(),
        top_k: req.top_k,
        expand_graph: true,
        compact: !req.full_content,
        branch_files: None,
        branch_boost: SearchQuery::default_branch_boost(),
        branch: None,
        // Cross-project fan-out is code-shaped by convention; per-tool
        // search_text / search_data callers use the per-index endpoint
        // directly and carry their own `mode` in the request body.
        mode: crate::core::indexer::SearchMode::default(),
        // Cross-project fan-out keeps the downrank default (issue #74); a
        // caller that wants archived chunks gone uses the per-index endpoint
        // with `exclude_archived: true`.
        exclude_archived: false,
        // Cross-project fan-out leaves stage selection up to each index's
        // own capability surface — the per-index loop below downshifts to
        // lexical when the semantic lane isn't ready (issue #109 Phase 1).
        stage: None,
        refine_query: None,
    };

    // Run all per-index searches concurrently. Any index that errors is
    // skipped with a log line so a single broken index doesn't 500 the
    // whole fan-out.
    let registry = state.registry.clone();
    let futures = active_ids.into_iter().map(|id| {
        let registry = registry.clone();
        let query = per_index_query.clone();
        async move {
            let handle = registry.get(&id)?;
            let indexer = handle.indexer.read().await;
            match indexer.search(&query).await {
                Ok(results) => Some((id, results)),
                Err(e) => {
                    tracing::warn!("global search: index {} errored: {e}", id);
                    None
                }
            }
        }
    });
    let per_index_results: Vec<(IndexId, Vec<crate::core::indexer::CodeChunk>)> =
        futures::future::join_all(futures)
            .await
            .into_iter()
            .flatten()
            .collect();

    // Build a flat lookup table from "namespaced" chunk_id
    // ({index_id}::{chunk.id}) back to the tagged CodeChunk, plus per-index
    // ranked id lists for RRF. Namespacing is required because different
    // indexes can produce colliding chunk_ids (same relative file path in
    // two projects).
    let mut chunk_lookup: std::collections::HashMap<String, crate::core::indexer::CodeChunk> =
        std::collections::HashMap::new();
    let mut lanes: Vec<Vec<(String, f32)>> = Vec::with_capacity(per_index_results.len());
    let mut indexes_searched: Vec<String> = Vec::with_capacity(per_index_results.len());
    for (id, results) in per_index_results {
        indexes_searched.push(id.0.clone());
        // Issue #112: in `"all"` mode, multiply each lane's scores by the
        // index's cosine-similarity weight; in `"top_n"` / `"threshold"`
        // modes the weight is always 1.0 (selection has already happened).
        // Issue #404: also apply the sub-index priority boost so sub-index
        // hits rank above the parent's duplicate coverage after RRF fusion.
        let cosine_weight = weight_map.get(&id).copied().unwrap_or(1.0);
        let weight = crate::core::search::hierarchy::effective_weight_for_index(
            &id,
            cosine_weight,
            &hierarchy,
        );
        let mut lane: Vec<(String, f32)> = Vec::with_capacity(results.len());
        for mut chunk in results {
            let namespaced = format!("{}::{}", id.0, chunk.id);
            // Tag the chunk with its origin index before storing it so the
            // returned CodeChunks know where they came from.
            chunk.index_id = Some(id.0.clone());
            let weighted_score = chunk.score * weight;
            lane.push((namespaced.clone(), weighted_score));
            chunk_lookup.insert(namespaced, chunk);
        }
        lanes.push(lane);
    }

    // RRF fuse across lanes. `rrf_fuse` takes exactly two lanes, so we fold
    // pairwise: start with empty + lane0, then merge each subsequent lane.
    // Each fold step uses alpha=1, beta=1 — every index lane contributes
    // equally. The output is sorted by fused score desc.
    let mut fused: Vec<(String, f32)> = Vec::new();
    let oversample = req.top_k.saturating_mul(4).max(req.top_k).max(10);
    for lane in lanes {
        fused = rrf_fuse(&fused, &lane, 1.0, 1.0, RRF_K, oversample);
    }

    // Issue #404: post-RRF dedup for nested indexes.
    // When a parent index and one of its sub-indexes both contain a chunk for
    // the same `(canonical_absolute_path, start_line, end_line)`, drop the
    // parent's copy (lower-scored after boost) and keep the sub-index's copy.
    // Flat peers that merely share files are NOT deduped.
    let (fused, hierarchy_dedup_count) = crate::core::search::hierarchy::dedup_nested_results(
        fused,
        &chunk_lookup,
        &state.registry,
        &hierarchy,
    );

    let mut fused = fused;
    fused.truncate(req.top_k);

    let results: Vec<crate::core::indexer::CodeChunk> = fused
        .into_iter()
        .filter_map(|(id, fused_score)| {
            let mut chunk = chunk_lookup.remove(&id)?;
            chunk.score = fused_score;
            Some(chunk)
        })
        .collect();

    let latency_ms = started.elapsed().as_millis() as u64;
    Ok(Json(serde_json::json!({
        "results": results,
        "indexes_searched": indexes_searched,
        "total_indexes": total_indexes,
        // PR #1103: callers must check this field — when > 0, the fan-out is
        // incomplete because selective warm-boot has not yet loaded all indexes.
        // Cold indexes are NOT lazy-loaded during fan-out (too expensive). Use
        // per-index `POST /indexes/:id/search` to trigger a cold-index load, or
        // wait until the index appears in `registry.list()`.
        "cold_indexes_skipped": cold_indexes_skipped,
        "latency_ms": latency_ms,
        "intent": format!("{:?}", intent),
        "routing": routing_label,
        "routing_decisions": routing_decisions,
        "hierarchy_dedup_count": hierarchy_dedup_count,
    })))
}
