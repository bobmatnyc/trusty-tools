//! Axum router and HTTP request handlers for the search daemon.
//!
//! Why: Splitting the routing surface from the daemon lifecycle keeps both
//! files under the 500-line cap (#365) and lets the integration test
//! exercise the handlers without going through `tokio::signal` or pid-file
//! IO.
//! What: Defines the request body types, the five `/search/*` handlers, the
//! compact-mode truncation helper, and [`build_router`].
//! Test: See the parent module's `router_serves_health_with_mock_indexer`
//! integration test.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::Value;

use super::{SearchState, default_extensions};
use crate::search::indexer::CodeChunk;

/// Number of lines to keep per chunk in compact mode (#400).
const COMPACT_LINES: usize = 7;

#[derive(Deserialize)]
struct QueryBody {
    query: String,
    #[serde(default = "default_top_k")]
    top_k: usize,
    /// When true, run KG expansion on top-K results (#376 B1).
    #[serde(default = "default_expand_graph")]
    expand_graph: bool,
    /// When true, truncate each chunk's `text` to 7 lines (compact mode).
    ///
    /// Why: Full chunk payloads can be 40-120 lines. Compact mode cuts
    /// ~5-10x token cost for callers that only need to locate a function,
    /// not read its entire body (#400).
    #[serde(default)]
    compact: bool,
}

fn default_top_k() -> usize {
    5
}

fn default_expand_graph() -> bool {
    true
}

#[derive(Deserialize)]
struct PathBody {
    path: String,
}

/// `GET /search/health` — liveness probe.
async fn health_handler(State(s): State<SearchState>) -> impl IntoResponse {
    // Best-effort: count of CodeIndex chunks isn't directly exposed by the
    // store trait, so we report a sentinel `-1` when unavailable. The
    // important contract is the 200 status + `status: ok`.
    let chunks: i64 = -1;
    let _ = &s; // silence unused for now; placeholder for future stats
    Json(serde_json::json!({
        "status": "ok",
        "indexed_chunks": chunks,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Truncate a chunk's text to `COMPACT_LINES` lines when compact mode is on.
///
/// Why: Full chunks can be 40-120 lines; callers that only need to locate a
/// function can request compact mode for ~5-10x token savings (#400).
fn apply_compact(mut hits: Vec<CodeChunk>) -> Vec<CodeChunk> {
    for chunk in &mut hits {
        let truncated: String = chunk
            .text
            .lines()
            .take(COMPACT_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        chunk.text = truncated;
    }
    hits
}

/// `POST /search/query` — semantic + lexical hybrid search.
async fn query_handler(
    State(s): State<SearchState>,
    Json(body): Json<QueryBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if body.query.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "query must be non-empty".into()));
    }
    // Helper: serialize hits, return a 500 instead of swallowing the error
    // into `Value::Null` like the previous version did (#376 A4).
    let to_json = |hits: Vec<CodeChunk>| {
        let hits = if body.compact {
            apply_compact(hits)
        } else {
            hits
        };
        serde_json::to_value(&hits).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("encoding hits to JSON failed: {e}"),
            )
        })
    };
    match s
        .indexer
        .search_hybrid(&body.query, body.top_k, body.expand_graph)
        .await
    {
        Ok(hits) => Ok(Json(to_json(hits)?)),
        Err(e) => {
            tracing::warn!(error = %e, "search_hybrid failed; falling back to vector-only");
            match s.indexer.search(&body.query, body.top_k).await {
                Ok(hits) => Ok(Json(to_json(hits)?)),
                Err(e2) => Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("search failed: hybrid={e}; vector={e2}"),
                )),
            }
        }
    }
}

/// `POST /search/index-file` — re-index a single file by absolute path.
async fn index_file_handler(
    State(s): State<SearchState>,
    Json(body): Json<PathBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let path = PathBuf::from(&body.path);
    match s.indexer.index_file(&path, Some(&s.project_root)).await {
        Ok(n) => Ok(Json(serde_json::json!({ "chunks": n }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("index_file failed: {e}"),
        )),
    }
}

/// `POST /search/remove-file` — drop all chunks for a path.
async fn remove_file_handler(
    State(s): State<SearchState>,
    Json(body): Json<PathBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let path = PathBuf::from(&body.path);
    match s.indexer.remove_file(&path).await {
        Ok(n) => Ok(Json(serde_json::json!({ "removed": n }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("remove_file failed: {e}"),
        )),
    }
}

/// `POST /search/reindex` — fire-and-forget full directory reindex.
async fn reindex_handler(State(s): State<SearchState>) -> Json<Value> {
    {
        let mut flag = s.reindex_in_flight.lock().await;
        if *flag {
            return Json(serde_json::json!({ "status": "already-running" }));
        }
        *flag = true;
    }
    let indexer = Arc::clone(&s.indexer);
    let root = s.project_root.clone();
    let flag = Arc::clone(&s.reindex_in_flight);
    tokio::spawn(async move {
        let exts = default_extensions();
        let ext_refs: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();
        match indexer.index_directory(&root, &ext_refs).await {
            Ok(n) => tracing::info!(chunks = n, "background reindex complete"),
            Err(e) => tracing::warn!(error = %e, "background reindex failed"),
        }
        *flag.lock().await = false;
    });
    Json(serde_json::json!({ "status": "started" }))
}

/// Build the axum router with the shared state attached.
///
/// Why: Splitting the router from `run_search_service` keeps the
/// integration test simple — it can construct a `SearchState` over a
/// mock store and exercise all five handlers without going through
/// `tokio::signal` or pid-file IO.
/// What: Wires the five `/search/*` routes to their handlers and attaches
/// `state`.
/// Test: `router_serves_health_with_mock_indexer`.
pub fn build_router(state: SearchState) -> Router {
    Router::new()
        .route("/search/health", get(health_handler))
        .route("/search/query", post(query_handler))
        .route("/search/index-file", post(index_file_handler))
        .route("/search/remove-file", post(remove_file_handler))
        .route("/search/reindex", post(reindex_handler))
        .with_state(state)
}
