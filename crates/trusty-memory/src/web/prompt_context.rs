//! Knowledge gaps, prompt-context, aliases, and prompt-facts handlers.
//!
//! Why: These endpoints surface the community-detection output and hot-predicate
//! triple surface that feed the prompt-injection workflow. They are grouped
//! together because they all operate on the shared `prompt_context_cache` and
//! KG hot-predicate layer.
//! What: `GET /api/v1/kg/gaps`, `GET /api/v1/kg/prompt-context`,
//! `POST /api/v1/kg/aliases`, and `GET`/`DELETE` `/api/v1/kg/prompt-facts`.
//! Test: `kg_gaps_*`, `prompt_context_endpoint_*`, `add_alias_endpoint_*`,
//! `list_prompt_facts_*`, `remove_prompt_fact_*` in `web::tests`.

use axum::{
    extract::{Query, State},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use trusty_common::memory_core::community::KnowledgeGap;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::store::kg::Triple;

use crate::AppState;

use super::error::{open_handle, ApiError};

#[allow(unused_imports)]
pub(crate) use crate::service::refresh_gaps_cache;

// ---------------------------------------------------------------------------
// Knowledge gaps — community detection cache (issue #53)
// ---------------------------------------------------------------------------

/// Wire shape for a single knowledge gap returned by `/api/v1/kg/gaps`.
///
/// Why: `KnowledgeGap` (in `trusty-common`) does not derive `Serialize`
/// because that would force serde into the memory-core feature surface; the
/// HTTP layer instead owns a narrow response struct mirroring its fields.
/// What: One-for-one wire representation of `KnowledgeGap` — entities, the
/// internal-density score, the cross-community bridge count, and the
/// LLM/template exploration hint.
/// Test: `kg_gaps_endpoint_returns_cached_gaps`.
#[derive(Serialize, Debug, Clone)]
pub struct KnowledgeGapResponse {
    pub entities: Vec<String>,
    pub internal_density: f32,
    pub external_bridges: usize,
    pub suggested_exploration: String,
}

impl From<KnowledgeGap> for KnowledgeGapResponse {
    fn from(g: KnowledgeGap) -> Self {
        Self {
            entities: g.entities,
            internal_density: g.internal_density,
            external_bridges: g.external_bridges,
            suggested_exploration: g.suggested_exploration,
        }
    }
}

#[derive(Deserialize)]
pub(super) struct KgGapsQuery {
    #[serde(default)]
    palace: Option<String>,
}

/// `GET /api/v1/kg/gaps?palace=<name>` — return the cached knowledge gaps.
///
/// Why: Issue #53 — surfaces the community-detection output computed by the
/// dream cycle so callers (dashboard, MCP tool, external tooling) can list
/// the sparse-cluster targets the model should explore next. Reading from
/// the in-memory cache means a `/kg/gaps` request never triggers a Louvain
/// run; it just clones the latest snapshot.
/// What: Resolves the palace from the optional `palace` query arg (falling
/// back to the daemon's `default_palace`, then erroring with 400 if neither
/// is set). Returns `[]` when the cache has no entry yet — the dream cycle
/// simply hasn't populated it. Returns 404 only when the palace name is
/// unknown to the registry (handle.open failed).
/// Test: `kg_gaps_endpoint_returns_cached_gaps`,
/// `kg_gaps_endpoint_returns_empty_when_uncached`.
pub(super) async fn kg_gaps_handler(
    State(state): State<AppState>,
    Query(q): Query<KgGapsQuery>,
) -> Result<Json<Vec<KnowledgeGapResponse>>, ApiError> {
    let palace_name = q
        .palace
        .clone()
        .or_else(|| state.default_palace.clone())
        .ok_or_else(|| {
            ApiError::bad_request("missing 'palace' query parameter (no default palace configured)")
        })?;

    // Validate the palace exists; we don't strictly need the handle for the
    // cache lookup but we want a 404 rather than an empty-array masking a
    // typo in the palace name.
    let _handle = open_handle(&state, &palace_name)?;

    let pid = PalaceId::new(&palace_name);
    let gaps = state.registry.get_gaps(&pid).unwrap_or_default();
    let body: Vec<KnowledgeGapResponse> =
        gaps.into_iter().map(KnowledgeGapResponse::from).collect();
    Ok(Json(body))
}

// ---------------------------------------------------------------------------
// Prompt-facts surface (issue #42)
// ---------------------------------------------------------------------------

/// Query parameters shared by the prompt-context / prompt-facts read endpoints.
///
/// Why: Both `GET /api/v1/kg/prompt-context` and `GET /api/v1/kg/prompt-facts`
/// optionally accept a `palace` filter so callers can scope reads to a single
/// project namespace. A shared struct keeps the wire shape consistent.
/// What: A single optional `palace` query parameter. When omitted, handlers
/// span every palace in the registry (matching the MCP tool behaviour).
/// Test: `prompt_context_endpoint_returns_formatted_block`,
/// `list_prompt_facts_endpoint_returns_hot_triples`.
#[derive(Deserialize)]
pub(super) struct PromptFactsQuery {
    // Accepted for forward-compat with the MCP tool surface, but ignored:
    // the prompt cache is registry-wide, so reads always span every palace.
    // We keep the field rather than ignoring `?palace=...` silently so a
    // future per-palace filter is a non-breaking schema addition.
    #[serde(default)]
    #[allow(dead_code)]
    palace: Option<String>,
}

/// Wire shape for `POST /api/v1/kg/aliases`.
///
/// Why: Mirrors the `add_alias` MCP tool: a short → full mapping with an
/// optional palace target. Keeping the field names identical between the
/// HTTP and MCP surfaces makes documentation and client code reuse trivial.
/// What: Required `short` and `full`; optional `palace` (falls back to the
/// daemon default).
/// Test: `add_alias_endpoint_asserts_triple_and_refreshes_cache`.
#[derive(Deserialize)]
pub(super) struct AddAliasRequest {
    short: String,
    full: String,
    #[serde(default)]
    palace: Option<String>,
}

/// Wire shape for a single hot-predicate triple in JSON responses.
///
/// Why: `list_prompt_facts` returns a structured array rather than the
/// pre-formatted Markdown so dashboards and tooling can render their own
/// views over the raw data.
/// What: subject/predicate/object string trio matching the underlying KG row.
/// Test: `list_prompt_facts_endpoint_returns_hot_triples`.
#[derive(Serialize)]
pub(super) struct PromptFactRow {
    subject: String,
    predicate: String,
    object: String,
}

/// Query parameters for `DELETE /api/v1/kg/prompt-facts`.
///
/// Why: The MCP tool retracts the active interval for a `(subject, predicate)`
/// pair across every palace; the HTTP endpoint matches that contract so a
/// dashboard "Remove" button doesn't need to know which palace owns the fact.
/// What: Required `subject` and `predicate`; the issue spec mentions an
/// optional `object` filter but the underlying `KnowledgeGraph::retract` API
/// closes the entire `(subject, predicate)` interval — we accept `object`
/// for forward-compat but currently ignore it, mirroring the MCP tool.
/// Test: `remove_prompt_fact_endpoint_soft_deletes_and_refreshes_cache`.
#[derive(Deserialize)]
pub(super) struct RemovePromptFactQuery {
    subject: String,
    predicate: String,
    #[serde(default)]
    #[allow(dead_code)]
    object: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    palace: Option<String>,
}

/// `GET /api/v1/kg/prompt-context` — return the formatted prompt-context block.
///
/// Why: Lets non-MCP callers (the admin UI, curl, integration tests) fetch
/// the same Markdown block the `get_prompt_context` tool returns, without
/// needing to speak JSON-RPC. The body is a plain text response so it can
/// be piped straight into a model prompt.
/// What: Reads the in-memory `prompt_context_cache` (already kept fresh by
/// any write that touches a hot predicate), returns the formatted string,
/// or a placeholder message when nothing has been stored yet.
/// Test: `prompt_context_endpoint_returns_formatted_block`.
pub(super) async fn prompt_context_handler(
    State(state): State<AppState>,
    Query(_q): Query<PromptFactsQuery>,
) -> Result<Response, ApiError> {
    let cache_snapshot = {
        let guard = state.prompt_context_cache.read().await;
        guard.clone()
    };
    let body = if cache_snapshot.formatted.is_empty() {
        "No prompt facts stored yet.".to_string()
    } else {
        cache_snapshot.formatted
    };
    let mut resp = body.into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok(resp)
}

/// `POST /api/v1/kg/aliases` — assert a `(short, is_alias_for, full)` triple.
///
/// Why: HTTP counterpart to the `add_alias` MCP tool — lets the admin UI
/// (or an external automation) register aliases without speaking JSON-RPC.
/// What: Resolves the target palace (request body → daemon default), opens
/// the palace handle, asserts the alias triple, and rebuilds the prompt
/// cache so subsequent `GET /api/v1/kg/prompt-context` calls reflect the
/// write immediately.
/// Test: `add_alias_endpoint_asserts_triple_and_refreshes_cache`.
pub(super) async fn add_alias_handler(
    State(state): State<AppState>,
    Json(req): Json<AddAliasRequest>,
) -> Result<Json<Value>, ApiError> {
    if req.short.is_empty() || req.full.is_empty() {
        return Err(ApiError::bad_request("short and full are required"));
    }
    let palace_name = req
        .palace
        .clone()
        .or_else(|| state.default_palace.clone())
        .ok_or_else(|| ApiError::bad_request("missing 'palace' (no default palace configured)"))?;
    let handle = open_handle(&state, &palace_name)?;
    let triple = Triple {
        subject: req.short.clone(),
        predicate: "is_alias_for".to_string(),
        object: req.full.clone(),
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: Some("add_alias_http".to_string()),
    };
    handle
        .kg
        .assert(triple)
        .await
        .map_err(|e| ApiError::internal(format!("kg.assert failed: {e:#}")))?;
    if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(&state).await {
        tracing::warn!("rebuild_prompt_cache after HTTP add_alias failed: {e:#}");
    }
    Ok(Json(json!({
        "subject": req.short,
        "predicate": "is_alias_for",
        "object": req.full,
        "palace": palace_name,
    })))
}

/// `GET /api/v1/kg/prompt-facts` — list every active hot-predicate triple.
///
/// Why: Mirrors the `list_prompt_facts` MCP tool. Returning the raw triples
/// (rather than the formatted block) lets dashboards group, search, and
/// edit them with their own UI.
/// What: Calls `gather_hot_triples` over the live registry and serialises
/// each row as `{subject, predicate, object}`.
/// Test: `list_prompt_facts_endpoint_returns_hot_triples`.
pub(super) async fn list_prompt_facts_handler(
    State(state): State<AppState>,
    Query(_q): Query<PromptFactsQuery>,
) -> Result<Json<Vec<PromptFactRow>>, ApiError> {
    let triples = crate::prompt_facts::gather_hot_triples(&state)
        .await
        .map_err(|e| ApiError::internal(format!("gather_hot_triples: {e:#}")))?;
    let rows: Vec<PromptFactRow> = triples
        .into_iter()
        .map(|(subject, predicate, object)| PromptFactRow {
            subject,
            predicate,
            object,
        })
        .collect();
    Ok(Json(rows))
}

/// `DELETE /api/v1/kg/prompt-facts?subject=...&predicate=...` — soft-delete
/// the active triple matching the given `(subject, predicate)` pair.
///
/// Why: HTTP counterpart to the `remove_prompt_fact` MCP tool. Mirrors the
/// retract-across-palaces semantics so a single call cleans up the fact
/// regardless of which palace stored it.
/// What: Iterates every palace, calls `kg.retract(subject, predicate)`, and
/// reports the total number of intervals closed. Rebuilds the prompt cache
/// when at least one retraction occurred.
/// Test: `remove_prompt_fact_endpoint_soft_deletes_and_refreshes_cache`.
pub(super) async fn remove_prompt_fact_handler(
    State(state): State<AppState>,
    Query(q): Query<RemovePromptFactQuery>,
) -> Result<Json<Value>, ApiError> {
    if q.subject.is_empty() || q.predicate.is_empty() {
        return Err(ApiError::bad_request("subject and predicate are required"));
    }
    let mut closed_total: usize = 0;
    for palace_id in state.registry.list() {
        if let Some(handle) = state.registry.get(&palace_id) {
            match handle.kg.retract(&q.subject, &q.predicate).await {
                Ok(n) => closed_total += n,
                Err(e) => tracing::warn!(
                    palace = %palace_id.as_str(),
                    "HTTP retract failed: {e:#}",
                ),
            }
        }
    }
    if closed_total > 0 {
        if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(&state).await {
            tracing::warn!("rebuild_prompt_cache after HTTP remove_prompt_fact failed: {e:#}");
        }
        Ok(Json(json!({"removed": true, "closed": closed_total})))
    } else {
        Ok(Json(json!({"removed": false, "reason": "not found"})))
    }
}
