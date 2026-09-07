//! The five operations the search daemon performs, independent of transport.
//!
//! Why (#6433, ADR-0032): these bodies used to be axum handlers — they took
//! `State<SearchState>` plus `Json<Body>` and answered with a `StatusCode`, so
//! the work and the HTTP framing were the same function. ADR-0032 moves the
//! daemon onto a Unix socket speaking JSON-RPC, and a handler that names
//! `StatusCode` cannot move with it. Each function here takes `&SearchState`
//! and its own request type and returns `Result<T, RpcError>`, so [`super::rpc`]
//! owns the wire and this module owns what the daemon does.
//! What: `health`, `query`, `index_file`, `remove_file`, `reindex`, plus the
//! compact-mode truncation helper.
//! Test: `rpc_*` in `tests.rs` drive every one of these over a real socket.

use std::path::PathBuf;
use std::sync::Arc;

use trusty_common::uds::server::RpcError;

use super::rpc::{HealthResponse, IndexFileResponse, PathRequest, QueryRequest, ReindexResponse};
use super::{SearchState, default_extensions};
use crate::search::indexer::CodeChunk;

/// Number of lines to keep per chunk in compact mode (#400).
const COMPACT_LINES: usize = 7;

/// `search.health` — liveness and this daemon's version.
///
/// Why: the method `SearchDaemonClient::connect_if_running` dials before it
/// decides a daemon is usable, and the one an operator can call by hand.
/// What: a constant answer. It reports no chunk count: the store trait exposes
/// none, and the field that used to carry one always held the sentinel `-1`.
/// Test: `rpc_health_answers_over_a_real_socket`.
pub(super) fn health() -> HealthResponse {
    HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Truncate each chunk's text to [`COMPACT_LINES`] lines.
///
/// Why: full chunks run 40-120 lines; a caller that only needs to locate a
/// function pays ~5-10x the tokens for the rest (#400).
fn apply_compact(mut hits: Vec<CodeChunk>) -> Vec<CodeChunk> {
    for chunk in &mut hits {
        chunk.text = chunk
            .text
            .lines()
            .take(COMPACT_LINES)
            .collect::<Vec<_>>()
            .join("\n");
    }
    hits
}

/// `search.query` — hybrid semantic + lexical search, with a vector-only
/// fallback.
///
/// Why: the hot path. A hybrid search that fails for a reason specific to the
/// lexical or KG half still has a usable vector answer, so the fallback keeps a
/// degraded index answering rather than refusing.
/// What: rejects an empty query, runs `search_hybrid`, falls back to `search`,
/// and applies compact truncation when the caller asked for it.
///
/// # Errors
///
/// `invalid_params` for an empty query; `internal` when both searches failed,
/// naming each failure — the two are different faults and a caller that sees
/// only one cannot tell which half broke.
///
/// Test: `rpc_query_refuses_an_empty_query`, `rpc_query_answers_with_hits`.
pub(super) async fn query(
    state: &SearchState,
    req: QueryRequest,
) -> Result<Vec<CodeChunk>, RpcError> {
    if req.query.trim().is_empty() {
        return Err(RpcError::invalid_params("query must be non-empty"));
    }
    let finish = |hits: Vec<CodeChunk>| {
        if req.compact {
            apply_compact(hits)
        } else {
            hits
        }
    };
    match state
        .indexer
        .search_hybrid(&req.query, req.top_k, req.expand_graph)
        .await
    {
        Ok(hits) => Ok(finish(hits)),
        Err(hybrid) => {
            tracing::warn!(error = %hybrid, "search_hybrid failed; falling back to vector-only");
            match state.indexer.search(&req.query, req.top_k).await {
                Ok(hits) => Ok(finish(hits)),
                Err(vector) => Err(RpcError::internal(format!(
                    "search failed: hybrid={hybrid}; vector={vector}"
                ))),
            }
        }
    }
}

/// `search.index_file` — re-index one file by path.
///
/// # Errors
///
/// `internal` when the indexer could not read, chunk or embed the file.
///
/// Test: `rpc_index_file_reports_its_chunk_count`.
pub(super) async fn index_file(
    state: &SearchState,
    req: PathRequest,
) -> Result<IndexFileResponse, RpcError> {
    let path = PathBuf::from(&req.path);
    state
        .indexer
        .index_file(&path, Some(&state.project_root))
        .await
        .map(|chunks| IndexFileResponse { chunks })
        .map_err(|e| RpcError::internal(format!("index_file failed: {e}")))
}

/// `search.remove_file` — drop every chunk recorded for a path.
///
/// # Errors
///
/// `internal` when the store rejected the delete.
///
/// Test: `rpc_remove_file_reports_its_removed_count`.
pub(super) async fn remove_file(
    state: &SearchState,
    req: PathRequest,
) -> Result<IndexFileResponse, RpcError> {
    let path = PathBuf::from(&req.path);
    state
        .indexer
        .remove_file(&path)
        .await
        .map(|chunks| IndexFileResponse { chunks })
        .map_err(|e| RpcError::internal(format!("remove_file failed: {e}")))
}

/// `search.reindex` — start a full directory reindex and answer immediately.
///
/// Why fire-and-forget: a whole-repository reindex runs for minutes, and a
/// caller that waited would hold a connection open for the duration while the
/// daemon is still answering queries off the old index.
/// What: takes the in-flight flag, spawns the walk, and reports `started` or
/// `already-running` so a second caller learns its request was a no-op rather
/// than queueing a duplicate walk.
/// Test: `rpc_reindex_starts_and_refuses_a_concurrent_second`.
pub(super) async fn reindex(state: &SearchState) -> ReindexResponse {
    {
        let mut flag = state.reindex_in_flight.lock().await;
        if *flag {
            return ReindexResponse {
                status: "already-running".to_string(),
            };
        }
        *flag = true;
    }
    let indexer = Arc::clone(&state.indexer);
    let root = state.project_root.clone();
    let flag = Arc::clone(&state.reindex_in_flight);
    tokio::spawn(async move {
        let exts = default_extensions();
        let ext_refs: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();
        match indexer.index_directory(&root, &ext_refs).await {
            Ok(n) => tracing::info!(chunks = n, "background reindex complete"),
            Err(e) => tracing::warn!(error = %e, "background reindex failed"),
        }
        *flag.lock().await = false;
    });
    ReindexResponse {
        status: "started".to_string(),
    }
}
