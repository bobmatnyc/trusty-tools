//! File-level index operations: index-file, remove-file, chunk listing,
//! grep, and call-chain traversal.
//!
//! Why: Groups the handlers that operate on individual files or the chunk
//! corpus (`POST /index-file`, `POST /remove-file`, `GET /chunks`,
//! `POST /grep`, `GET /call_chain`) into one focused module.
//! What: `index_file_handler`, `remove_file_handler`,
//! `get_index_chunks_handler`, `grep_one_index`, `grep_handler`,
//! `global_grep_handler`, `call_chain_handler` and their param types.
//! Test: `grep_endpoint_returns_matches` and related.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::core::registry::{IndexHandle, IndexId};

use super::helpers::file_is_within_root;
use super::router::{IndexFileRequest, RemoveFileRequest};
use super::state::SearchAppState;

pub(super) async fn index_file_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<IndexFileRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let index_id = IndexId::new(id);
    let handle = state.registry.get(&index_id).ok_or(StatusCode::NOT_FOUND)?;
    let indexer = handle.indexer.read().await;
    indexer
        .index_file(&req.path, &req.content)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "index_id": index_id.0,
        "path": req.path,
        "indexed": true,
    })))
}

pub(super) async fn remove_file_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<RemoveFileRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let index_id = IndexId::new(id);
    let handle = state.registry.get(&index_id).ok_or(StatusCode::NOT_FOUND)?;
    let indexer = handle.indexer.read().await;
    let removed = indexer
        .remove_file(&req.path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "index_id": index_id.0,
        "path": req.path,
        "removed_chunks": removed,
    })))
}

/// Query params for `GET /indexes/:id/chunks` (issue #54).
#[derive(Deserialize)]
pub struct ChunksParams {
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_chunks_limit")]
    pub limit: usize,
}

fn default_chunks_limit() -> usize {
    100
}

/// Hard ceiling on a single `chunks` page so a misconfigured client can't pull
/// the entire corpus into one response. Mirrored in the `list_chunks` MCP tool.
const MAX_CHUNKS_LIMIT: usize = 1_000;

/// `GET /indexes/:id/chunks?offset=&limit=` — paginated enumeration of an index.
///
/// Why: trusty-analyzer (sidecar daemon) and external tooling need to page
/// through every chunk in batches without loading the whole corpus at once.
/// Issue #54 introduces stable-order pagination on top of the existing bulk
/// export.
/// What: Returns
/// `{ index_id, total, offset, limit, chunks: [...] }`. `chunks` is the slice
/// `[offset .. offset+limit]` of the corpus sorted by `(file, start_line)`.
/// `limit` is clamped to `MAX_CHUNKS_LIMIT` (1000); the value echoed back in
/// the response is the post-clamp value so clients can detect the clamp.
/// Test: `test_get_index_chunks_paginates` registers an index, indexes a few
/// files, asserts page1 + page2 cover all chunks without overlap.
pub(super) async fn get_index_chunks_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Query(params): Query<ChunksParams>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let index_id = IndexId::new(id);
    let handle = state.registry.get(&index_id).ok_or(StatusCode::NOT_FOUND)?;
    let limit = params.limit.min(MAX_CHUNKS_LIMIT);
    let indexer = handle.indexer.read().await;
    let (total, chunks) = indexer.enumerate_chunks(params.offset, limit).await;
    Ok(Json(serde_json::json!({
        "index_id": index_id.0,
        "total": total,
        "offset": params.offset,
        "limit": limit,
        "chunks": chunks,
    })))
}

/// Grep a single index's files and append hits into `out`, honouring the
/// remaining `max_results` budget.
///
/// Why: both the per-index (`POST /indexes/:id/grep`) and the global
/// (`POST /grep`) handlers need the identical "for every file the index knows
/// about, read it from disk and run the matcher" loop. Factoring it out keeps
/// the two handlers thin and guarantees they behave identically.
/// What: snapshots the index's `RawChunk` corpus to discover the distinct set
/// of files (deduped, since one file produces many chunks), then for each file
/// that passes the glob filter and lives within the index root, reads the file
/// fresh from disk under `root_path` and runs [`grep::grep_file_content`]. Files
/// that fail the glob, escape the root, or can't be read are skipped silently
/// (a read failure is logged at debug — the file may have been deleted since it
/// was indexed). Greps the real on-disk bytes, so no embedding is required and
/// line numbers are exact. Stops once `out.len()` reaches `max_results`.
/// Test: `grep::tests` covers the matcher; `grep_endpoint_*` server integration
/// tests cover the file-walking + glob + root-confinement behaviour.
async fn grep_one_index(
    handle: &IndexHandle,
    compiled: &crate::service::grep::CompiledGrep,
    out: &mut Vec<crate::service::grep::GrepMatch>,
    max_results: usize,
) {
    if out.len() >= max_results {
        return;
    }
    let chunks = {
        let indexer = handle.indexer.read().await;
        indexer.raw_chunks_snapshot().await
    };
    // One file produces many chunks; dedupe to a sorted, distinct file set so
    // each file is read and scanned exactly once in a deterministic order.
    let mut files: Vec<String> = chunks.into_iter().map(|c| c.file).collect();
    files.sort();
    files.dedup();

    for rel in files {
        if out.len() >= max_results {
            return;
        }
        // Glob filter (cheap) before defense-in-depth root confinement.
        if !compiled.path_matches(&rel) {
            continue;
        }
        if !file_is_within_root(&rel, &handle.root_path) {
            continue;
        }
        let abs = if std::path::Path::new(&rel).is_absolute() {
            std::path::PathBuf::from(&rel)
        } else {
            handle.root_path.join(&rel)
        };
        match tokio::fs::read_to_string(&abs).await {
            Ok(content) => {
                crate::service::grep::grep_file_content(&rel, &content, compiled, out, max_results);
            }
            Err(e) => {
                tracing::debug!(
                    file = %rel,
                    error = %e,
                    "grep: skipping unreadable file (deleted or non-UTF-8 since index time)"
                );
            }
        }
    }
}

/// `POST /indexes/:id/grep` — grep-parity regex search over one index's files.
///
/// Why: complements `POST /indexes/:id/search` (fuzzy hybrid recall) with exact,
/// deterministic, line-accurate matching for callers who need `grep`/`ripgrep`
/// semantics (regex, `-i`, `-A`/`-B`/`-C`, `--include` glob, multiline) against
/// a known project — without re-embedding.
/// What: compiles the [`grep::GrepRequest`] (400 on bad regex/glob), resolves
/// the index (404 if unknown), runs [`grep_one_index`], and returns a
/// [`grep::GrepResponse`]. `truncated` is set when the `max_results` cap is hit.
/// Test: `grep_endpoint_returns_matches`, `grep_endpoint_bad_regex_is_400`,
/// `grep_endpoint_unknown_index_is_404`.
pub(super) async fn grep_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<crate::service::grep::GrepRequest>,
) -> Result<Json<crate::service::grep::GrepResponse>, (StatusCode, Json<serde_json::Value>)> {
    // Issue #882: empty / whitespace-only patterns match every line in every
    // file, producing a meaningless dump of the entire corpus.
    if req.pattern.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "pattern must not be empty" })),
        ));
    }
    let compiled = crate::service::grep::CompiledGrep::compile(&req).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
    })?;
    let index_id = IndexId::new(id);
    let handle = state.registry.get(&index_id).ok_or((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": format!("unknown index: {}", index_id.0) })),
    ))?;

    let started = std::time::Instant::now();
    let mut matches = Vec::new();
    grep_one_index(&handle, &compiled, &mut matches, req.max_results).await;
    let truncated = matches.len() >= req.max_results;
    tracing::info!(
        index_id = %index_id,
        matches = matches.len(),
        truncated = truncated,
        latency_ms = started.elapsed().as_millis() as u64,
        "grep"
    );
    let total = matches.len();
    Ok(Json(crate::service::grep::GrepResponse {
        matches,
        total,
        truncated,
    }))
}

/// `POST /grep` — grep-parity regex search fanned out across indexes.
///
/// Why: callers that don't know which project a literal lives in want one grep
/// over every (or a chosen) index, mirroring the global `POST /search` fan-out.
/// What: compiles the request (400 on bad regex/glob), then iterates the
/// registered indexes (restricted to `index_id` when supplied — unknown id ⇒
/// empty result set, not 404, matching the global search's tolerant behaviour),
/// running [`grep_one_index`] against each until the shared `max_results` budget
/// is exhausted. Returns a [`grep::GrepResponse`].
/// Test: `grep_global_fans_out`, `grep_global_respects_index_filter`.
pub(super) async fn global_grep_handler(
    State(state): State<Arc<SearchAppState>>,
    Json(req): Json<crate::service::grep::GrepRequest>,
) -> Result<Json<crate::service::grep::GrepResponse>, (StatusCode, Json<serde_json::Value>)> {
    // Issue #882: same guard as grep_handler — an empty pattern matches every line.
    if req.pattern.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "pattern must not be empty" })),
        ));
    }
    let compiled = crate::service::grep::CompiledGrep::compile(&req).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
    })?;

    let ids: Vec<IndexId> = match req.index_id.as_deref() {
        Some(only) => state
            .registry
            .list()
            .into_iter()
            .filter(|id| id.0 == only)
            .collect(),
        None => state.registry.list(),
    };

    let started = std::time::Instant::now();
    let mut matches = Vec::new();
    for id in ids {
        if matches.len() >= req.max_results {
            break;
        }
        if let Some(handle) = state.registry.get(&id) {
            grep_one_index(&handle, &compiled, &mut matches, req.max_results).await;
        }
    }
    let truncated = matches.len() >= req.max_results;
    tracing::info!(
        matches = matches.len(),
        truncated = truncated,
        latency_ms = started.elapsed().as_millis() as u64,
        "grep_global"
    );
    let total = matches.len();
    Ok(Json(crate::service::grep::GrepResponse {
        matches,
        total,
        truncated,
    }))
}

/// Query params for `GET /indexes/{id}/call_chain` (issue #76).
///
/// Why: HTTP callers (and the MCP `get_call_chain` tool that proxies through
/// the daemon) need to specify an entry point and traversal options without
/// posting a JSON body.
/// What: mirrors the `get_call_chain` MCP tool args.
/// Test: integration test `test_call_chain_handler_*`.
#[derive(Debug, Deserialize)]
pub(super) struct CallChainParams {
    entry_point: String,
    direction: Option<String>,
    max_depth: Option<u32>,
    include_source: Option<bool>,
}

/// `GET /indexes/{id}/call_chain?entry_point=...&direction=...&...` —
/// return an annotated call-tree report for a function (issue #76).
///
/// Why: LLM clients consume the response directly as plain text context, so
/// the body is `text/plain` (not JSON). The MCP `get_call_chain` tool calls
/// this endpoint and wraps the result in the standard `content[]` envelope.
/// What: snapshots the indexer's symbol graph + raw chunk corpus, hands them
/// to [`crate::service::call_chain::render_call_chain`], and returns the
/// resulting `String`. Returns 400 for invalid params, 404 for unknown
/// indexes or unresolvable entry points.
/// Test: covered by `service::call_chain::tests` (renderer) and the MCP
/// dispatch tests (transport contract).
pub(super) async fn call_chain_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Query(params): Query<CallChainParams>,
) -> Result<Response, (StatusCode, axum::Json<serde_json::Value>)> {
    use crate::service::call_chain::{render_call_chain, CallChainRequest};

    let req = CallChainRequest {
        index_id: id.clone(),
        entry_point: params.entry_point,
        direction: params.direction,
        max_depth: params.max_depth,
        include_source: params.include_source,
    };
    let validated = req.validate().map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        )
    })?;

    let index_id = IndexId::new(id);
    let handle = state.registry.get(&index_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": format!("unknown index: {}", index_id.0) })),
        )
    })?;

    // Issue #313: skip_kg indexes have no symbol graph — return a structured
    // 503 so callers can distinguish "KG disabled" from "no symbols found".
    if handle.skip_kg {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "error": "kg_unavailable",
                "reason": "skipped_by_config",
                "index": index_id.0,
            })),
        ));
    }

    let (graph, chunks) = {
        let indexer = handle.indexer.read().await;
        let graph = indexer.snapshot_symbol_graph().await;
        let chunks = indexer.raw_chunks_snapshot().await;
        (graph, chunks)
    };

    let text = render_call_chain(&validated, graph.as_ref(), &chunks).map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": e })),
        )
    })?;
    Ok((
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        text,
    )
        .into_response())
}
