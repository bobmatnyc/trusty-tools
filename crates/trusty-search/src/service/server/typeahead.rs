//! HTTP handler for `GET /indexes/{id}/typeahead`.
//!
//! Why: typeahead/autocomplete needs a dedicated endpoint that is (1) fast
//! enough for per-keystroke use in lexical mode (<30 ms, no ONNX call) and
//! (2) richer in blended mode for debounced invocations (semantic + KG).
//! Having a separate handler keeps the main `search_handler` uncomplicated and
//! lets this endpoint use query params (GET) rather than a POST body, which
//! maps naturally to browser `<input>` completion.
//! What: `typeahead_handler` extracts `?q=`, `?limit=`, `?mode=` query
//! params, runs `CodeIndexer::search` with the appropriate `SearchStage`,
//! and maps the results to `TypeaheadHit` slices via the shared
//! `TypeaheadHit::from_chunk` constructor.
//! Test: `tests/typeahead.rs`.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::core::indexer::{
    typeahead::{TypeaheadHit, TypeaheadMode, TypeaheadResponse},
    SearchQuery, SearchStage,
};
use crate::core::registry::IndexId;

use super::state::SearchAppState;

/// Maximum number of typeahead hits a caller may request.
///
/// Why: autocomplete UIs rarely need more than 10 items; clamping at 25
/// prevents callers from accidentally running a full search through the
/// typeahead endpoint.
/// What: hard upper bound applied after parsing `?limit=`.
/// Test: `typeahead_limit_clamped_to_max` in `tests/typeahead.rs`.
pub(super) const MAX_TYPEAHEAD_LIMIT: usize = 25;

/// Default number of typeahead hits returned when `?limit=` is absent.
const DEFAULT_TYPEAHEAD_LIMIT: usize = 6;

/// Query parameters for the typeahead endpoint.
///
/// Why: GET params rather than a JSON body so the endpoint is directly
/// addressable from `<input oninput="fetch('/indexes/x/typeahead?q=...')">`.
/// What: `q` (required prefix), `limit` (clamped to 1–25, default 6), `mode`
/// (`lexical` default or `blended`).
/// Test: `typeahead_query_params_parsed` in `tests/typeahead.rs`.
#[derive(Debug, Deserialize)]
pub struct TypeaheadParams {
    /// The prefix to search for. Empty or whitespace → empty response.
    #[serde(default)]
    pub q: String,
    /// Maximum hits to return. Clamped to `[1, MAX_TYPEAHEAD_LIMIT]`.
    pub limit: Option<usize>,
    /// Retrieval mode: `"lexical"` (default) or `"blended"`.
    pub mode: Option<TypeaheadMode>,
}

/// `GET /indexes/{id}/typeahead` — blended typeahead/autocomplete.
///
/// Why: per-keystroke autocomplete needs a sub-30 ms endpoint in lexical mode
/// (no embedding call). Blended mode adds semantic + KG for richer
/// suggestions when the client debounces input.
/// What: validates params, short-circuits on empty/whitespace `q`, calls
/// `CodeIndexer::search` with the correct `SearchStage`, maps `CodeChunk`s
/// to `TypeaheadHit`s, and returns a `TypeaheadResponse` envelope.
/// Test: `typeahead_lexical_returns_hits`, `typeahead_empty_query_returns_empty`
/// in `tests/typeahead.rs`.
pub async fn typeahead_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Query(params): Query<TypeaheadParams>,
) -> Result<Json<TypeaheadResponse>, (StatusCode, Json<serde_json::Value>)> {
    let q = params.q.trim();
    if q.is_empty() {
        return Ok(Json(TypeaheadResponse {
            hits: vec![],
            mode: "lexical".to_string(),
            latency_ms: 0,
        }));
    }

    let limit = params
        .limit
        .unwrap_or(DEFAULT_TYPEAHEAD_LIMIT)
        .clamp(1, MAX_TYPEAHEAD_LIMIT);

    let mode = params.mode.unwrap_or_default();

    let index_id = IndexId::new(id.clone());
    let handle = match state.registry.get(&index_id) {
        Some(h) => h,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": format!("unknown index: {id}") })),
            ))
        }
    };

    // Build the SearchQuery. Lexical mode skips the ONNX embedding call
    // entirely; blended enables semantic + KG expansion.
    let (stage, expand_graph) = match mode {
        TypeaheadMode::Lexical => (SearchStage::Lexical, false),
        TypeaheadMode::Blended => (SearchStage::Semantic, true),
    };

    let query = SearchQuery {
        text: q.to_owned(),
        top_k: limit,
        expand_graph,
        compact: true,
        stage: Some(stage),
        ..SearchQuery::default()
    };

    // Acquire the read lock first, then start the latency timer so `latency_ms`
    // measures search execution time rather than search + lock-acquisition wait
    // (issue #1560 nit 2). Under contention the lock-wait could dominate and
    // mislead callers who use `latency_ms` to budget ONNX / BM25 time.
    let indexer = handle.indexer.read().await;
    let started = std::time::Instant::now();
    let chunks = indexer.search(&query).await.map_err(|e| {
        tracing::warn!(index_id = %id, err = %e, "typeahead search error");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "internal search error" })),
        )
    })?;
    drop(indexer);

    let latency_ms = started.elapsed().as_millis() as u64;
    let mode_str = match mode {
        TypeaheadMode::Lexical => "lexical",
        TypeaheadMode::Blended => "blended",
    };

    let hits: Vec<TypeaheadHit> = chunks
        .iter()
        .take(limit)
        .map(|chunk| {
            let source = TypeaheadHit::classify_source(&chunk.match_reason);
            TypeaheadHit::from_chunk(chunk, source)
        })
        .collect();

    tracing::debug!(
        index_id = %id,
        q = %q,
        mode = %mode_str,
        hits = hits.len(),
        latency_ms = latency_ms,
        "typeahead"
    );

    Ok(Json(TypeaheadResponse {
        hits,
        mode: mode_str.to_owned(),
        latency_ms,
    }))
}
