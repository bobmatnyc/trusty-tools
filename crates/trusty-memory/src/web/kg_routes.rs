//! Knowledge Graph REST handlers.
//!
//! Why: The KG Explorer UI and MCP tool surface both rely on these endpoints
//! to browse, assert, and retract triples. Keeping them in one file makes
//! the KG REST surface easy to audit and extend.
//! What: All `/api/v1/palaces/{id}/kg/*` endpoints plus the dream-cycle
//! status/run endpoints, and the opaque triple-id encode/decode helpers.
//! Test: `kg_list_subjects_*`, `kg_list_all_*`, `kg_graph_*`,
//! `decode_triple_id_*`, `dream_status_*`, `dream_run_*` in `web::tests`.

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
// #4670: ExpandDirection backs the `direction` param on `kg/graph/neighbors`.
use trusty_common::memory_core::store::kg::{ExpandDirection, Triple};

use crate::AppState;
// #4776: the page-size bounds moved to the service layer so the MCP
// `kg_list_subjects` tool reads the same two literals these routes do; `tools`
// is compiled without the `axum-server` feature that gates this module.
use crate::service::core_kg::{DEFAULT_KG_LIST_LIMIT, MAX_KG_LIST_LIMIT};

use super::error::ApiError;

// ---------------------------------------------------------------------------
// KG query + assert
// ---------------------------------------------------------------------------

/// Query parameters for `GET /api/v1/palaces/{id}/kg`.
///
/// Why: Requires a `subject` filter so the handler does not accidentally
/// return the full graph, which can be unbounded.
/// What: Single required `subject` string.
/// Test: Covered by integration.
#[derive(Deserialize)]
pub(super) struct KgQueryParams {
    subject: String,
}

/// `GET /api/v1/palaces/{id}/kg?subject=<s>` — query active triples for a subject.
///
/// Why: The KG Explorer detail view and external tooling need a fast subject
/// lookup without fetching the whole graph.
/// What: Delegates to `MemoryService::kg_query`.
/// Test: Covered by integration.
pub(super) async fn kg_query(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<KgQueryParams>,
) -> Result<Json<Vec<Triple>>, ApiError> {
    Ok(Json(
        crate::service::MemoryService::new(state)
            .kg_query(&id, &q.subject)
            .await?,
    ))
}

pub(crate) use crate::service::KgAssertBody;

/// `POST /api/v1/palaces/{id}/kg` — assert a new triple.
///
/// Why: HTTP counterpart to the MCP `kg_assert` tool.
/// What: Delegates to `MemoryService::kg_assert`; returns `204 No Content`.
/// Test: Covered via `http_create_drawer_runs_auto_kg_extraction`.
pub(super) async fn kg_assert(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<KgAssertBody>,
) -> Result<StatusCode, ApiError> {
    crate::service::MemoryService::new(state)
        .kg_assert(&id, body)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// KG list helpers
// ---------------------------------------------------------------------------

fn default_kg_list_limit() -> usize {
    DEFAULT_KG_LIST_LIMIT
}

/// Query parameters for `GET /api/v1/palaces/{id}/kg/subjects`.
///
/// Why: The KG Explorer's left panel asks for a bounded subject list; `limit`
/// is clamped server-side so the SPA cannot accidentally pull the whole graph.
/// What: `limit` defaults to [`DEFAULT_KG_LIST_LIMIT`] and is clamped to
/// `[1, MAX_KG_LIST_LIMIT]` in the handler.
/// Test: `kg_list_subjects_returns_distinct`.
#[derive(Deserialize)]
pub(super) struct KgListSubjectsParams {
    #[serde(default = "default_kg_list_limit")]
    limit: usize,
}

/// `GET /api/v1/palaces/{id}/kg/subjects?limit=N` — list distinct active subjects.
///
/// Why: The KG Explorer needs to browse subjects without a prior query (the
/// existing `kg_query` endpoint requires one). Surfacing this read on the
/// daemon avoids the SPA having to know how to issue SQL.
/// What: clamps `limit` to `[1, MAX_KG_LIST_LIMIT]` and delegates to
/// `KnowledgeGraph::list_subjects`. Returns a JSON array of strings.
/// Test: `kg_list_subjects_returns_distinct`.
pub(super) async fn kg_list_subjects(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<KgListSubjectsParams>,
) -> Result<Json<Vec<String>>, ApiError> {
    let limit = q.limit.clamp(1, MAX_KG_LIST_LIMIT);
    Ok(Json(
        crate::service::MemoryService::new(state)
            .kg_list_subjects(&id, limit)
            .await?,
    ))
}

/// `GET /api/v1/palaces/{id}/kg/subjects_with_counts?limit=N` — list distinct
/// active subjects with their active-triple counts.
///
/// Why: The KG Explorer's subject list shows a count badge per subject and
/// supports sort-by-count. Returning the grouped counts in a single SQL pass
/// is cheaper than issuing one query per subject from the SPA.
/// What: clamps `limit` to `[1, MAX_KG_LIST_LIMIT]` and delegates to
/// `KnowledgeGraph::list_subjects_with_counts`. Returns a JSON array of
/// `{subject, count}` objects ordered alphabetically.
/// Test: indirectly via the KG Explorer UI.
pub(super) async fn kg_list_subjects_with_counts(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<KgListSubjectsParams>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let limit = q.limit.clamp(1, MAX_KG_LIST_LIMIT);
    let rows = crate::service::MemoryService::new(state)
        .kg_list_subjects_with_counts(&id, limit)
        .await?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|(subject, count)| json!({ "subject": subject, "count": count }))
        .collect();
    Ok(Json(out))
}

/// Query parameters for `GET /api/v1/palaces/{id}/kg/all`.
///
/// Why: The KG Explorer's "All" mode pages through every active triple;
/// `limit`+`offset` give the SPA stable prev/next controls.
/// What: defaults match `kg_list_subjects` for limit; `offset` defaults to 0.
/// Test: `kg_list_all_returns_paginated_triples`.
#[derive(Deserialize)]
pub(super) struct KgListAllParams {
    #[serde(default = "default_kg_list_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

/// `GET /api/v1/palaces/{id}/kg/all?limit=N&offset=N` — list all active triples.
///
/// Why: The KG Explorer's "All" mode wants a paged view across every active
/// triple regardless of subject. The existing `kg_query` requires a subject.
/// What: clamps `limit` to `[1, MAX_KG_LIST_LIMIT]` and delegates to
/// `KnowledgeGraph::list_active`. Returns a JSON array of `Triple` objects.
/// Test: `kg_list_all_returns_paginated_triples`.
pub(super) async fn kg_list_all(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<KgListAllParams>,
) -> Result<Json<Vec<Triple>>, ApiError> {
    let limit = q.limit.clamp(1, MAX_KG_LIST_LIMIT);
    Ok(Json(
        crate::service::MemoryService::new(state)
            .kg_list_all(&id, limit, q.offset)
            .await?,
    ))
}

/// `GET /api/v1/palaces/{id}/kg/count` — count of currently-active triples.
///
/// Why: The KG Explorer header shows a quick "N triples" badge; computing the
/// count server-side avoids fetching every triple to count them.
/// What: returns `{ "active": N }` where N is `count_active_triples()` on the
/// palace's KG.
/// Test: indirectly via the same palace counts surfaced on `/api/v1/status`.
pub(super) async fn kg_count(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    let active = crate::service::MemoryService::new(state)
        .kg_count(&id)
        .await?;
    Ok(Json(json!({ "active": active })))
}

// ---------------------------------------------------------------------------
// Triple encode/decode + delete
// ---------------------------------------------------------------------------

/// Separator byte sequence used inside a URL-safe base64 triple ID.
///
/// Why: The triple primary key is `(subject, predicate)`. Encoding them as a
/// single opaque ID lets the REST path look like `/kg/triples/<id>` (a
/// resource identifier) rather than carrying both parts in the URL path, which
/// would require double-escaping arbitrary strings. A `\0` separator is safe
/// because neither subjects nor predicates ever contain null bytes.
/// What: Used by [`encode_triple_id`] and [`decode_triple_id`].
/// Test: `decode_triple_id_round_trips`.
const TRIPLE_ID_SEPARATOR: u8 = 0x00;

/// Encode a `(subject, predicate)` pair as a URL-safe base64 triple ID.
///
/// Why: Produces a single opaque string that can travel as a URL path segment
/// without percent-encoding. The null-byte separator ensures the encoding is
/// injective (no two distinct pairs can produce the same encoded string) —
/// but only when neither component itself contains a null byte. Prior to
/// issue #1102 the function silently produced an ambiguous (corrupt) id when
/// the subject or predicate contained `\0`; now it returns an error so
/// callers cannot persist an undecodable id.
/// What: Validates that neither `subject` nor `predicate` contains
/// `TRIPLE_ID_SEPARATOR` (`\0`). On success returns
/// `Ok(base64url(subject_bytes + "\0" + predicate_bytes))`, no padding.
/// On validation failure returns `Err` with a descriptive message.
/// Test: `decode_triple_id_round_trips`, `encode_triple_id_rejects_null_byte`.
// Only called from tests (round-trip + null-byte rejection); suppress the
// dead_code lint that fires in non-test builds.
#[allow(dead_code)]
pub(crate) fn encode_triple_id(subject: &str, predicate: &str) -> Result<String, String> {
    use base64::Engine as _;
    if subject.as_bytes().contains(&TRIPLE_ID_SEPARATOR) {
        return Err(format!(
            "subject must not contain the null-byte separator (\\0); got {subject:?}"
        ));
    }
    if predicate.as_bytes().contains(&TRIPLE_ID_SEPARATOR) {
        return Err(format!(
            "predicate must not contain the null-byte separator (\\0); got {predicate:?}"
        ));
    }
    let mut buf = Vec::with_capacity(subject.len() + 1 + predicate.len());
    buf.extend_from_slice(subject.as_bytes());
    buf.push(TRIPLE_ID_SEPARATOR);
    buf.extend_from_slice(predicate.as_bytes());
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf))
}

/// Decode a URL-safe base64 triple ID back to `(subject, predicate)`.
///
/// Why: The handler for `DELETE /kg/triples/<id>` needs to recover the
/// `(subject, predicate)` pair from the opaque path segment to call the
/// service layer.
/// What: Decodes base64url, splits on the first null byte. Returns `None`
/// when the input is not valid base64url or contains no null separator.
/// Test: `decode_triple_id_round_trips`.
pub(crate) fn decode_triple_id(id: &str) -> Option<(String, String)> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(id)
        .ok()?;
    let sep_pos = bytes.iter().position(|&b| b == TRIPLE_ID_SEPARATOR)?;
    let subject = String::from_utf8(bytes[..sep_pos].to_vec()).ok()?;
    let predicate = String::from_utf8(bytes[sep_pos + 1..].to_vec()).ok()?;
    Some((subject, predicate))
}

/// `DELETE /api/v1/palaces/{id}/kg/triples/{triple_id}` — surgically remove
/// one active triple by its opaque base64url-encoded `(subject, predicate)` ID.
///
/// Why: Issue #278 — the existing `(subject, predicate)` retract via
/// `/kg/prompt-facts` is scope-wide (retract across all palaces). This
/// endpoint targets exactly one triple in exactly one palace, giving callers
/// a surgical way to delete a specific edge without affecting other palaces
/// or other predicates for the same subject.
/// What: Decodes `triple_id` (base64url of `subject\0predicate`) back into
/// `(subject, predicate)`, retracts the active interval via
/// `MemoryService::kg_retract_triple`, and returns:
///   - `204 No Content` on success
///   - `404 Not Found` when the triple_id is malformed or no active triple
///     matched
///
/// Test: `kg_delete_triple_returns_204_on_success` and
/// `kg_delete_triple_returns_404_for_missing`.
pub(super) async fn kg_delete_triple(
    State(state): State<AppState>,
    AxumPath((id, triple_id)): AxumPath<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let (subject, predicate) = decode_triple_id(&triple_id).ok_or_else(|| {
        ApiError::not_found("invalid triple id — expected base64url(subject\\0predicate)")
    })?;
    let found = crate::service::MemoryService::new(state)
        .kg_retract_triple(&id, &subject, &predicate)
        .await?;
    if found {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!(
            "no active triple with subject={subject:?} predicate={predicate:?} in palace {id:?}"
        )))
    }
}

pub(crate) use crate::service::KgGraphPayload;

/// `GET /api/v1/palaces/{id}/kg/graph` — full graph for visualisation.
///
/// Why: The KG Explorer graph-view's explicit "load everything" mode needs the
/// full active triple set. As of issue #4670 this is no longer the view's
/// default load — see [`kg_graph_seed`] — because the payload is capped at
/// `KG_GRAPH_MAX_TRIPLES` and the response now says so.
/// What: Delegates to `MemoryService::kg_graph`; returns `KgGraphPayload`,
/// which carries `returned_triple_count` / `active_triple_count` / `truncated`.
/// Test: `kg_graph_returns_active_triples`, `kg_graph_signals_truncation`,
/// `kg_graph_meets_perf_budget_for_500_triples`.
pub(super) async fn kg_graph(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<KgGraphPayload>, ApiError> {
    Ok(Json(
        crate::service::MemoryService::new(state)
            .kg_graph(&id)
            .await?,
    ))
}

// ---------------------------------------------------------------------------
// Progressive graph exploration (issue #4670)
// ---------------------------------------------------------------------------

pub(crate) use crate::service::{KgNeighborsPayload, KgSeedPayload};

/// Default seed size for `GET /kg/graph/seed` when the caller omits `limit`.
///
/// Why (issue #4670): the graph view's layout is an O(n²) force simulation run
/// for ~200 ticks. At 75 nodes that is ~2,775 pair computations per tick
/// (~555K total) — imperceptible; at the palace's full 9,311 nodes it is ~43M
/// per tick (~8.7B total), which is the freeze this endpoint exists to avoid.
/// 75 sits deliberately above the sibling list endpoints' 50: measured degree
/// distribution on the live palace is 90.2% degree-1 leaves with top degrees of
/// 48/46/45/44/32, so a 50-node seed stops right where the mid-tier hubs
/// (degree 5–10, ~7% of nodes) begin. 75 reaches into that tier while leaving
/// ~2.5× headroom under the max before layout cost becomes noticeable, so a
/// few click-expansions do not need a page reload to stay smooth.
const DEFAULT_KG_SEED_LIMIT: usize = 75;

/// Hard ceiling on the seed size.
///
/// Why: mirrors `MAX_KG_LIST_LIMIT` so every bounded KG read in this file
/// shares one ceiling. 200 nodes is ~4M layout operations — still interactive,
/// and past it the hairball is unreadable regardless of performance.
const MAX_KG_SEED_LIMIT: usize = 200;

fn default_kg_seed_limit() -> usize {
    DEFAULT_KG_SEED_LIMIT
}

/// Query parameters for `GET /api/v1/palaces/{id}/kg/graph/seed`.
#[derive(Deserialize)]
pub(super) struct KgSeedParams {
    #[serde(default = "default_kg_seed_limit")]
    limit: usize,
}

/// `GET /api/v1/palaces/{id}/kg/graph/seed?limit=N` — top-N nodes by degree.
///
/// Why (issue #4670): first paint of the graph view. Returns the structurally
/// important slice of the graph plus the palace-wide totals, so the header can
/// state "75 of 9,311 nodes shown" instead of implying it rendered everything.
/// What: clamps `limit` to `[1, MAX_KG_SEED_LIMIT]` and delegates to
/// `MemoryService::kg_graph_seed`. The returned `triples` use the same wire
/// shape as `/kg/graph`, so the client merges seed and expansion results with
/// one code path.
/// Test: `kg_graph_seed_ranks_by_degree`, `kg_graph_seed_clamps_limit`.
pub(super) async fn kg_graph_seed(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<KgSeedParams>,
) -> Result<Json<KgSeedPayload>, ApiError> {
    let limit = q.limit.clamp(1, MAX_KG_SEED_LIMIT);
    Ok(Json(
        crate::service::MemoryService::new(state)
            .kg_graph_seed(&id, limit)
            .await?,
    ))
}

/// Maximum BFS depth for `GET /kg/graph/neighbors`.
///
/// Why: matches `trusty-search`'s `graph_neighbors_handler`, which clamps
/// `max_hops` to `1..=4` — keeping the two crates' traversal contracts
/// identical means an operator who learned one already knows the other.
const MAX_KG_NEIGHBOR_HOPS: usize = 4;

fn default_kg_neighbor_hops() -> usize {
    1
}

/// Query parameters for `GET /api/v1/palaces/{id}/kg/graph/neighbors`.
///
/// Why: parameter names (`node`, `direction`, `max_hops`) deliberately mirror
/// `trusty-search`'s contributed-graph neighbors endpoint.
/// What: `node` is required; `direction` defaults to `both`; `max_hops`
/// defaults to 1 and is clamped to `[1, MAX_KG_NEIGHBOR_HOPS]`.
/// Test: `kg_neighbors_returns_incoming_edges`, `kg_neighbors_clamps_max_hops`,
/// `kg_neighbors_rejects_bad_direction`.
#[derive(Deserialize)]
pub(super) struct KgNeighborsParams {
    node: String,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default = "default_kg_neighbor_hops")]
    max_hops: usize,
}

/// `GET /api/v1/palaces/{id}/kg/graph/neighbors?node=X&direction=…&max_hops=N`
///
/// Why (issue #4670): click-to-expand. Crucially this is the first endpoint
/// that can answer "what points AT this node" — `kg_query` is a subject prefix
/// scan (`store/kg_redb/read_ops.rs:29-67`) and never reads the object side,
/// so the incoming half of every palace graph was unreachable over HTTP.
/// What: parses `direction` (`in` | `out` | `both`, 400 on anything else),
/// clamps `max_hops`, and delegates to `MemoryService::kg_neighbors`, which
/// BFSes the resident adjacency — no disk I/O.
/// Test: `kg_neighbors_returns_incoming_edges`, `kg_neighbors_clamps_max_hops`,
/// `kg_neighbors_rejects_bad_direction`.
pub(super) async fn kg_graph_neighbors(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<KgNeighborsParams>,
) -> Result<Json<KgNeighborsPayload>, ApiError> {
    let direction = match q.direction.as_deref().unwrap_or("both") {
        "in" | "inbound" => ExpandDirection::In,
        "out" | "outbound" => ExpandDirection::Out,
        "both" => ExpandDirection::Both,
        other => {
            return Err(ApiError::bad_request(format!(
                "direction must be in|out|both, got {other:?}"
            )))
        }
    };
    let max_hops = q.max_hops.clamp(1, MAX_KG_NEIGHBOR_HOPS);
    Ok(Json(
        crate::service::MemoryService::new(state)
            .kg_neighbors(&id, &q.node, direction, max_hops)
            .await?,
    ))
}

// ---------------------------------------------------------------------------
// Dream cycle status + on-demand run
// ---------------------------------------------------------------------------

pub(crate) use crate::service::DreamStatusPayload;

/// `GET /api/v1/dream/status` — aggregate dream cycle status across all palaces.
///
/// Why: The admin UI dashboard shows whether the last dream cycle succeeded.
/// What: Delegates to `MemoryService::dream_status_aggregate`.
/// Test: `dream_status_empty_returns_nulls`, `dream_status_aggregates_across_palaces`.
pub(super) async fn dream_status(State(state): State<AppState>) -> Json<DreamStatusPayload> {
    Json(
        crate::service::MemoryService::new(state)
            .dream_status_aggregate()
            .await,
    )
}

/// `GET /api/v1/palaces/{id}/dream/status` — dream cycle status for one palace.
///
/// Why: Per-palace dream status lets the UI show which palace is stale.
/// What: Delegates to `MemoryService::dream_status_for_palace`.
/// Test: Covered implicitly by `dream_status_aggregates_across_palaces`.
pub(super) async fn palace_dream_status(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<DreamStatusPayload>, ApiError> {
    Ok(Json(
        crate::service::MemoryService::new(state)
            .dream_status_for_palace(&id)
            .await?,
    ))
}

/// `POST /api/v1/dream/run` — trigger an on-demand dream cycle.
///
/// Why: Operators and tests need a way to trigger consolidation without
/// waiting for the scheduled background timer.
/// What: Delegates to `MemoryService::dream_run`; returns the aggregate
/// status after the run completes.
/// Test: `dream_run_aggregates_stats`.
pub(super) async fn dream_run(
    State(state): State<AppState>,
) -> Result<Json<DreamStatusPayload>, ApiError> {
    Ok(Json(
        crate::service::MemoryService::new(state)
            .dream_run()
            .await?,
    ))
}
