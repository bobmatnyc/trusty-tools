//! The HTTP routes over the `files` cores: index-file, remove-file, chunks,
//! grep and call-chain.
//!
//! Why (#9214, ruling D1): the daemon binds no TCP listener, so these axum
//! handlers serve only the HTTP tests that drive the cores through them.
//! `files.rs` keeps the cores the socket serves; PR-B deletes this file.
//! What: six thin handlers, each rendering one `*_report` core's verdict.
//! Test: `tests_corpus_read_5917.rs`, `tests_grep_cold_8266.rs` and
//! `tests_index_routing.rs` call them directly.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;

use super::files::{
    call_chain_report, global_grep_report, grep_report, index_chunks_report, index_file_report,
    remove_file_report, CallChainParams, ChunksParams,
};
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
    index_file_report(&state, &id, req)
        .await
        .map(Json)
        .map_err(|(status, body)| (status, Json(body)))
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
    remove_file_report(&state, &id, req)
        .await
        .map(Json)
        .map_err(|(status, body)| (status, Json(body)))
}

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
/// Test: `chunks_endpoint_offset_back_compat` (offset) and
/// `chunks_endpoint_cursor_pages_full_coverage` (cursor + next_cursor).
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
