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

/// `POST /indexes/:id/index-file` — add or replace one file in an index.
///
/// Why: this is the supported incremental-indexing path for network-mounted
/// roots (#3408), where the OS watcher cannot fire — so a caller driving it
/// from CI or a post-merge hook is the ONLY thing keeping the index current.
/// A bare hot-registry miss reported a cold-parked index as unknown, and #4715
/// establishes that a 404 from an index-scoped endpoint means "no such index
/// anywhere". Told "unknown index" for an index that exists, such a caller
/// deregisters it or gives up, and the writes stop arriving — silently, since
/// nothing else drives that index.
/// What: resolves the handle through
/// [`super::index_resolve::resolve_or_load_index`], the same function the read
/// path uses, so a cold-parked index is LOADED and the write applied (#5349)
/// rather than refused with a hint to go issue a search first. A load that
/// genuinely fails propagates as the 503/404 residency verdict — never as a
/// successful write.
/// Test: `cold_parked_index_accepts_a_write_by_driving_the_load`,
/// `write_against_an_unloadable_cold_index_fails_loudly`.
pub(super) async fn index_file_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<IndexFileRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let index_id = IndexId::new(id);
    // #5349: the write path drives the same lazy load the read path drives —
    // a cold-parked index is reloaded here, not reported as unwritable.
    let handle = super::index_resolve::resolve_or_load_index(&state, &index_id).await?;
    // #3049: hold the teardown lock's shared side across the write so a
    // concurrent DELETE cannot remove_dir_all this index's data mid-write.
    let _teardown_guard = crate::service::reindex::acquire_index_teardown_read(&index_id).await;
    let indexer = handle.indexer.read().await;
    indexer
        .index_file(&req.path, &req.content)
        .await
        .map_err(|e| {
            // #5061: a write that failed must say so — the caller cannot infer
            // it from a bare 500, and it has no other signal that the file it
            // just pushed never landed.
            tracing::warn!(
                index_id = %index_id,
                path = %req.path,
                error = %e,
                "index-file failed"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "index_file_failed",
                    "index_id": index_id.0,
                    "path": req.path,
                    "message": e.to_string(),
                })),
            )
        })?;
    Ok(Json(serde_json::json!({
        "index_id": index_id.0,
        "path": req.path,
        "indexed": true,
    })))
}

/// `POST /indexes/:id/remove-file` — drop one file's chunks from an index.
///
/// Why and What: the delete half of [`index_file_handler`]'s contract — same
/// callers, same network-mount motivation, same lazy load, same residency
/// verdict when that load fails.
/// Test: `cold_parked_index_accepts_a_delete_by_driving_the_load`.
pub(super) async fn remove_file_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<RemoveFileRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let index_id = IndexId::new(id);
    // #5349: see the sibling handler — a delete drives the load too, so the two
    // halves of the incremental contract cannot disagree about reachability.
    let handle = super::index_resolve::resolve_or_load_index(&state, &index_id).await?;
    // #3049: see the sibling handler — same guard, same reason.
    let _teardown_guard = crate::service::reindex::acquire_index_teardown_read(&index_id).await;
    let indexer = handle.indexer.read().await;
    let removed = indexer.remove_file(&req.path).await.map_err(|e| {
        // #5061: see the sibling handler — a silent 500 leaves the caller
        // believing a deletion landed when it did not.
        tracing::warn!(
            index_id = %index_id,
            path = %req.path,
            error = %e,
            "remove-file failed"
        );
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "remove_file_failed",
                "index_id": index_id.0,
                "path": req.path,
                "message": e.to_string(),
            })),
        )
    })?;
    Ok(Json(serde_json::json!({
        "index_id": index_id.0,
        "path": req.path,
        "removed_chunks": removed,
    })))
}

/// Render a chunk enumeration's corpus-read failure through the shared builder
/// (#6043; field shape unified by #5917).
///
/// Why: this file used to build the `index_corpus_unavailable` body itself,
/// sending only `retryable` while `degraded::corpus_failure_response` sent only
/// `failure_kind` + `transient` — two bodies under one error code that a caller
/// could not branch on uniformly. One builder is the fix; this wrapper keeps
/// the enumeration handlers' `warn` at the site that has the enumeration
/// context.
/// Test: the offset path's refusal reaches this builder via
/// `core::indexer::tests_cursor::enumerate_chunks_errors_when_rehydrate_did_not_commit`.
/// The cursor path's redb arm has no direct test — see
/// `CodeIndexer::enumerate_chunks_after` for why fixture-injecting a redb read
/// fault is not reachable.
fn corpus_read_failure_report(
    index_id: &str,
    err: &anyhow::Error,
) -> (StatusCode, serde_json::Value) {
    tracing::warn!("index '{index_id}': chunk enumeration failed: {err:#}");
    let (status, body) =
        super::degraded::corpus_read_failure_response(index_id, &format!("{err:#}"));
    (status, body.0)
}

/// Query params for `GET /indexes/:id/chunks` (issue #54, #1325).
///
/// Why: the original `offset`/`limit` pair forced an O(N log N) full-corpus
/// scan-and-sort per page, which 502'd at deep offsets (`offset=304000`) on
/// large indexes (issue #1325). `after` adds opt-in cursor pagination keyed on
/// the chunk's stable `id`, served by an indexed redb B-tree seek — O(page)
/// per call. `offset` is retained verbatim for back-compat.
/// What: `offset` (default 0), `limit` (default 100, clamped to 1000), and the
/// optional `after` cursor. When `after` is present it takes precedence and
/// `offset` is ignored.
/// Test: `test_get_index_chunks_paginates` (offset) and
/// `chunks_endpoint_cursor_*` server tests (cursor).
#[derive(Deserialize)]
pub struct ChunksParams {
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_chunks_limit")]
    pub limit: usize,
    /// Opaque forward cursor (a chunk `id`). When set, the page is the rows
    /// strictly after this id in ascending key order; `offset` is ignored.
    #[serde(default)]
    pub after: Option<String>,
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
/// `{ index_id, total, offset, limit, chunks: [...], next_cursor }`.
///
/// Two pagination modes, selected by the presence of the `after` query param:
/// - **Cursor (issue #1325, preferred for deep / bulk pagination):** when
///   `after` is present (including an empty string, meaning "from the start"),
///   the page is the rows strictly after that chunk `id` in ascending key
///   order, served by an indexed redb B-tree seek — O(page) regardless of
///   depth. `next_cursor` carries the id to pass as the next `after`, or is
///   `null` once the corpus is exhausted. `offset` is ignored in this mode.
/// - **Offset (issue #54, back-compat):** when `after` is absent, `chunks` is
///   the slice `[offset .. offset+limit]` of the corpus ordered by
///   `(file, start_line)`. `next_cursor` is always `null` in this mode (offset
///   order differs from cursor order, so a cursor walk must not be seeded from
///   an offset page). Offset pagination still scans/sorts the whole corpus per
///   page and can be slow at deep offsets on large indexes — prefer `after` for
///   bulk enumeration.
///
/// `limit` is clamped to `MAX_CHUNKS_LIMIT` (1000); the echoed value is the
/// post-clamp value so clients can detect the clamp.
/// Test: `test_get_index_chunks_paginates` (offset) and the
/// `chunks_endpoint_cursor_*` server tests (cursor + next_cursor).
pub(super) async fn get_index_chunks_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Query(params): Query<ChunksParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    index_chunks_report(&state, &id, &params)
        .await
        .map(Json)
        .map_err(|(status, body)| (status, Json(body)))
}

/// The body `GET /indexes/{id}/chunks` serves, without the transport
/// (#6285 slice 2).
///
/// Why: `search.chunks.list` pages the same corpus over the socket. The two
/// pagination modes disagree on ordering by design, so a second implementation
/// would be the one place a caller could silently lose or duplicate rows.
/// What: [`get_index_chunks_handler`]'s whole former body, including the
/// `MAX_CHUNKS_LIMIT` clamp and the mode selection.
/// Test: `chunks_over_the_socket_matches_the_http_body`.
pub(crate) async fn index_chunks_report(
    state: &Arc<SearchAppState>,
    id: &str,
    params: &ChunksParams,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let index_id = IndexId::new(id);
    // #4715: 404 must mean "no such index anywhere", not "not resident" —
    // see `index_status_handler` for why the MCP layer depends on that.
    // #5061: the miss now carries a body, via the builder all three
    // non-reloading endpoints share.
    let handle = match state.registry.get(&index_id) {
        Some(h) => h,
        None => {
            let (status, body) =
                super::degraded::residency_miss_response(&state.cold_store, &index_id);
            return Err((status, body.0));
        }
    };
    let limit = params.limit.min(MAX_CHUNKS_LIMIT);
    let indexer = handle.indexer.read().await;

    // Cursor mode (issue #1325): opt-in by sending the `after` param at all —
    // even an empty string, which means "start from the first chunk". The
    // cursor path orders strictly by `chunk_id` (the redb B-tree key) and does
    // an indexed seek, so it is O(page) at any depth. An empty cursor is
    // treated as "no cursor" (first page); a non-empty cursor resumes strictly
    // after that id. NOTE: cursor order (by id) differs from offset order (by
    // file, start_line), so a client must pick ONE mode and page consistently;
    // do not seed a cursor walk from an offset response.
    if let Some(cursor) = params.after.as_deref() {
        let after = if cursor.is_empty() {
            None
        } else {
            Some(cursor)
        };
        let (total, chunks, next_cursor) = indexer
            .enumerate_chunks_after(after, limit)
            .await
            .map_err(|e| corpus_read_failure_report(&index_id.0, &e))?;
        return Ok(serde_json::json!({
            "index_id": index_id.0,
            "total": total,
            "offset": params.offset,
            "limit": limit,
            "chunks": chunks,
            "next_cursor": next_cursor,
        }));
    }

    // Offset mode (issue #54): retained verbatim for back-compat. `next_cursor`
    // is always null here — offset and cursor use different orderings, so we do
    // not imply that a cursor walk can continue from an offset page (that would
    // drop or duplicate rows). Clients wanting fast deep pagination should use
    // the cursor mode from the start (`after=`).
    let (total, chunks) = indexer
        .enumerate_chunks(params.offset, limit)
        .await
        .map_err(|e| corpus_read_failure_report(&index_id.0, &e))?;
    Ok(serde_json::json!({
        "index_id": index_id.0,
        "total": total,
        "offset": params.offset,
        "limit": limit,
        "chunks": chunks,
        "next_cursor": serde_json::Value::Null,
    }))
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
///
/// Errors when the index's corpus cannot be read (#5917): the file set comes
/// from the chunk corpus, so an unreadable one greps nothing and reports
/// `{matches: [], total: 0}` — "this literal is nowhere in your code" for a
/// corpus that was never scanned.
/// Test: `grep::tests` covers the matcher; `grep_endpoint_*` server integration
/// tests cover the file-walking + glob + root-confinement behaviour;
/// `grep_over_an_unreadable_corpus_returns_503_naming_the_index` covers the
/// refusal.
async fn grep_one_index(
    handle: &IndexHandle,
    compiled: &crate::service::grep::CompiledGrep,
    out: &mut Vec<crate::service::grep::GrepMatch>,
    max_results: usize,
) -> anyhow::Result<()> {
    if out.len() >= max_results {
        return Ok(());
    }
    let chunks = {
        let indexer = handle.indexer.read().await;
        indexer.raw_chunks_snapshot().await?
    };
    // One file produces many chunks; dedupe to a sorted, distinct file set so
    // each file is read and scanned exactly once in a deterministic order.
    let mut files: Vec<String> = chunks.into_iter().map(|c| c.file).collect();
    files.sort();
    files.dedup();

    for rel in files {
        if out.len() >= max_results {
            return Ok(());
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
    Ok(())
}

/// Render a grep or call-chain corpus-read failure as the shared 503 (#5917).
///
/// Why: both grep handlers and `call_chain_handler` need the identical mapping,
/// and BOTH sub-cases belong under one shape. `ensure_corpus_view_is_current`
/// raises a typed `CorpusReadUnavailable` for a read that FAILED and an untyped
/// bail for a rehydrate still in flight past the retry budget — but a caller
/// cannot act on that distinction any differently, and the MCP layer classifies
/// only a 503, so answering the untyped arm with a bespoke 500 would reach an
/// MCP caller as an unstructured transport error. The sibling `/chunks` wrapper
/// already treats both as `index_corpus_unavailable`; this matches it, so all
/// four corpus-backed surfaces agree on both sub-cases.
/// Test: `grep_over_an_unreadable_corpus_returns_503_naming_the_index`,
/// `global_grep_over_an_unreadable_corpus_returns_503`,
/// `call_chain_over_an_unreadable_corpus_is_503_not_404`.
fn corpus_backed_read_error(
    index_id: &str,
    err: &anyhow::Error,
) -> (StatusCode, Json<serde_json::Value>) {
    tracing::warn!("index '{index_id}': corpus-backed read failed: {err:#}");
    super::degraded::corpus_read_failure_response(index_id, &format!("{err:#}"))
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
    grep_report(&state, &id, req)
        .await
        .map(Json)
        .map_err(|(status, body)| (status, Json(body)))
}

/// The body `POST /indexes/{id}/grep` serves, without the transport
/// (#6285 slice 3).
///
/// Why: `search.grep` answers the same question over the socket. One body is
/// what stops the two transports compiling the same pattern into different
/// matchers, or one reporting an unreadable corpus as zero matches.
/// What: [`grep_handler`]'s whole former body. A refusal keeps its HTTP status
/// beside its body, because that status is what
/// [`crate::service::rpc::error::rpc_error_from_http`] turns into the JSON-RPC
/// code — so a cold-parked index is the retryable 503 class on both.
/// Test: `grep_over_the_socket_matches_the_http_body`,
/// `a_bad_regex_reports_invalid_params_on_both_transports`.
pub(crate) async fn grep_report(
    state: &Arc<SearchAppState>,
    id: &str,
    req: crate::service::grep::GrepRequest,
) -> Result<crate::service::grep::GrepResponse, (StatusCode, serde_json::Value)> {
    // Issue #882: empty / whitespace-only patterns match every line in every
    // file, producing a meaningless dump of the entire corpus.
    if req.pattern.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "pattern must not be empty" }),
        ));
    }
    let compiled = crate::service::grep::CompiledGrep::compile(&req).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": e.to_string() }),
        )
    })?;
    let index_id = IndexId::new(id.to_string());
    // #4715: same rule as `index_status_handler` — a cold-parked index exists.
    // #5061: the three-way verdict (and the `restore_via` hint that tells a
    // caller a plain `search` reloads a cold-parked index) now lives in one
    // builder, so this handler cannot drift from its two neighbours.
    let handle = match state.registry.get(&index_id) {
        Some(h) => h,
        None => {
            let (status, body) =
                super::degraded::residency_miss_response(&state.cold_store, &index_id);
            return Err((status, body.0));
        }
    };

    let started = std::time::Instant::now();
    let mut matches = Vec::new();
    // #5917: an unreadable corpus greps no files at all. Reporting that as
    // zero matches tells the caller its literal is nowhere in the code.
    grep_one_index(&handle, &compiled, &mut matches, req.max_results)
        .await
        .map_err(|e| {
            let (status, body) = corpus_backed_read_error(&index_id.0, &e);
            (status, body.0)
        })?;
    let truncated = matches.len() >= req.max_results;
    tracing::info!(
        index_id = %index_id,
        matches = matches.len(),
        truncated = truncated,
        latency_ms = started.elapsed().as_millis() as u64,
        "grep"
    );
    let total = matches.len();
    Ok(crate::service::grep::GrepResponse {
        matches,
        total,
        truncated,
    })
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
    global_grep_report(&state, req)
        .await
        .map(Json)
        .map_err(|(status, body)| (status, Json(body)))
}

/// The body `POST /grep` serves, without the transport (#6285 slice 3).
///
/// Why: `search.grep.all` answers the same question over the socket. The
/// fan-out's tolerance rules — an unknown `index_id` narrows to nothing rather
/// than 404-ing, one unreadable corpus refuses the whole sweep — are exactly the
/// kind a second implementation would get subtly wrong.
/// What: [`global_grep_handler`]'s whole former body.
/// Test: `global_grep_over_the_socket_matches_the_http_body`.
pub(crate) async fn global_grep_report(
    state: &Arc<SearchAppState>,
    req: crate::service::grep::GrepRequest,
) -> Result<crate::service::grep::GrepResponse, (StatusCode, serde_json::Value)> {
    // Issue #882: same guard as grep_report — an empty pattern matches every line.
    if req.pattern.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "pattern must not be empty" }),
        ));
    }
    let compiled = crate::service::grep::CompiledGrep::compile(&req).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": e.to_string() }),
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
            // #5917: one unreadable corpus makes the whole fan-out incomplete,
            // and a global grep has no per-index field to say so. Refuse rather
            // than return the other indexes' matches as the complete answer.
            grep_one_index(&handle, &compiled, &mut matches, req.max_results)
                .await
                .map_err(|e| {
                    let (status, body) = corpus_backed_read_error(&id.0, &e);
                    (status, body.0)
                })?;
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
    Ok(crate::service::grep::GrepResponse {
        matches,
        total,
        truncated,
    })
}

/// Query params for `GET /indexes/{id}/call_chain` (issue #76).
///
/// Why: HTTP callers (and the MCP `get_call_chain` tool that proxies through
/// the daemon) need to specify an entry point and traversal options without
/// posting a JSON body.
/// What: mirrors the `get_call_chain` MCP tool args.
/// Test: integration test `test_call_chain_handler_*`.
#[derive(Debug, Deserialize)]
pub(crate) struct CallChainParams {
    pub(crate) entry_point: String,
    pub(crate) direction: Option<String>,
    pub(crate) max_depth: Option<u32>,
    pub(crate) include_source: Option<bool>,
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
    let text = call_chain_report(&state, &id, params)
        .await
        .map_err(|(status, body)| (status, axum::Json(body)))?;
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

/// The report `GET /indexes/{id}/call_chain` renders, without the transport
/// (#6285 slice 2).
///
/// Why: `search.call_chain` answers the same question over the socket. The
/// route's value is the rendered text, and the three refusal classes it
/// distinguishes — invalid params, unknown index, KG disabled — are what a
/// caller branches on, so both transports must derive them from one body.
/// What: [`call_chain_handler`]'s whole former body up to the response. The
/// result is the plain-text report; the socket returns it as a JSON string and
/// the handler above sets the `text/plain` content type.
/// Test: `call_chain_over_the_socket_matches_the_http_body`,
/// `call_chain_over_the_socket_reports_a_kg_disabled_index_as_unavailable`.
pub(crate) async fn call_chain_report(
    state: &Arc<SearchAppState>,
    id: &str,
    params: CallChainParams,
) -> Result<String, (StatusCode, serde_json::Value)> {
    use crate::service::call_chain::{render_call_chain, CallChainRequest};

    let req = CallChainRequest {
        index_id: id.to_string(),
        entry_point: params.entry_point,
        direction: params.direction,
        max_depth: params.max_depth,
        include_source: params.include_source,
    };
    let validated = req.validate().map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": e.to_string() }),
        )
    })?;

    let index_id = IndexId::new(id);
    let handle = state.registry.get(&index_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": format!("unknown index: {}", index_id.0) }),
        )
    })?;

    // Issue #313: skip_kg indexes have no symbol graph — return a structured
    // 503 so callers can distinguish "KG disabled" from "no symbols found".
    if handle.skip_kg {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({
                "error": "kg_unavailable",
                "reason": "skipped_by_config",
                "index": index_id.0,
            }),
        ));
    }

    let (graph, chunks) = {
        let indexer = handle.indexer.read().await;
        let graph = indexer.snapshot_symbol_graph().await;
        // #5917: the entry point is resolved against this snapshot, so an
        // unreadable corpus rendered as `404 entry point not found` — a real
        // symbol reported nonexistent, which reads as an answer about the code.
        let chunks = indexer.raw_chunks_snapshot().await.map_err(|e| {
            let (status, body) = corpus_backed_read_error(&index_id.0, &e);
            (status, body.0)
        })?;
        (graph, chunks)
    };

    render_call_chain(&validated, graph.as_ref(), &chunks)
        .map_err(|e| (StatusCode::NOT_FOUND, serde_json::json!({ "error": e })))
}
