//! Recall (vector + BM25 hybrid search) HTTP handlers.
//!
//! Why: Recall is the primary read path — agents and the admin UI both query
//! this endpoint for semantic memory retrieval. Both per-palace and
//! cross-palace fan-out variants are served here.
//! What: `GET /api/v1/palaces/{id}/recall` and `GET /api/v1/recall` handlers,
//! `RecallQuery` params struct, and the `recall_entry_json` re-export.
//! Test: `recall_all_handler_*` tests in `web::tests`.

use axum::{
    extract::{Path as AxumPath, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::Value;

use crate::AppState;

use super::error::ApiError;

/// Query parameters shared by the per-palace and cross-palace recall endpoints.
///
/// Why: both `GET /api/v1/palaces/{id}/recall` and `GET /api/v1/recall` accept
/// the same `q` / `top_k` / `deep` triple. Keeping one struct avoids drift
/// between the two handler signatures.
/// What: `q` is required; `top_k` and `deep` are optional with handler-side
/// defaults (10 and false respectively).
/// Test: `recall_all_handler_*` tests in this module.
#[derive(Deserialize)]
pub(super) struct RecallQuery {
    q: String,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    deep: Option<bool>,
    /// Issue #465: optional palace filter on the flat `GET /api/v1/recall`
    /// endpoint. When supplied, recall is scoped to that palace instead of
    /// fanning out across all palaces. Absent → cross-palace fan-out.
    #[serde(default)]
    palace: Option<String>,
}

/// `GET /api/v1/palaces/{id}/recall` — recall from a single palace.
///
/// Why: Palace-scoped recall lets the admin UI and per-project agents query
/// just one project's memory without merging across palaces.
/// What: Delegates to `MemoryService::recall` with the given `id`.
/// Test: Covered by integration; `recall_all_handler_honors_palace_filter`
/// exercises the scoped path via the flat endpoint with `?palace=`.
pub(super) async fn recall_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<RecallQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        crate::service::MemoryService::new(state)
            .recall(&id, &q.q, q.top_k.unwrap_or(10), q.deep.unwrap_or(false))
            .await?,
    ))
}

#[allow(unused_imports)]
pub(crate) use crate::service::recall_entry_json;

/// Extracts a human-readable error message from a partial-failure envelope.
///
/// Why: `recall_all` may return an object with a non-empty `"errors"` array
/// when one or more palaces fail during fan-out while others succeed. Without
/// this guard, that envelope passes through as 200 OK and callers receive
/// silent data loss. Extracting the detection into a pure function lets unit
/// tests verify every branch directly, without spinning up the HTTP stack.
/// What: Inspects `value` for a non-empty `"errors"` array. If found, formats
/// a message from the first entry — plain string, `{"message":"..."}` object,
/// or a generic fallback — and returns `Some(message)`. Returns `None` when
/// the array is absent or empty (i.e. no partial failure detected).
/// Test: `extract_partial_error_*` unit tests in `web::tests::recall_tests`.
pub(super) fn extract_partial_error(value: &Value) -> Option<String> {
    let errors = value.get("errors")?.as_array()?;
    if errors.is_empty() {
        return None;
    }
    let first = errors
        .first()
        .and_then(|e| {
            e.as_str()
                .map(str::to_owned)
                .or_else(|| e.get("message").and_then(|m| m.as_str()).map(str::to_owned))
        })
        .unwrap_or_else(|| "partial recall failure".to_owned());
    Some(format!(
        "recall_all partial failure ({} error(s)): {first}",
        errors.len()
    ))
}

/// `GET /api/v1/recall?q=<query>&top_k=<n>&deep=<bool>[&palace=<id>]` — recall
/// with optional palace scoping.
///
/// Why: Agents and dashboard widgets often need the most relevant memories
/// regardless of palace boundary; forcing the caller to issue one request per
/// palace and merge client-side is both slower (no fan-out) and wrong (no
/// dedup/rerank). Serving the merged top-k from the daemon collapses the
/// round-trip and reuses the shared embedder singleton.
/// Issue #465: the `palace=` query param was silently ignored — this endpoint
/// always queried the default palace regardless of the supplied filter, causing
/// callers to receive results from the wrong palace. Fix: when `palace=` is
/// present and non-empty, route the recall to that specific palace (matching
/// the behaviour of `GET /api/v1/palaces/{id}/recall`). When absent, fall back
/// to the cross-palace fan-out.
/// Issue #1102: hardened error detection on the cross-palace fan-out path.
/// `recall_all` now returns either a JSON array (success) or an object
/// carrying an `"error"` string (full failure). Both are detected; a
/// non-empty `"errors"` array in any future partial-success envelope is also
/// surfaced as an internal error rather than silently passed through as 200 OK
/// (see `extract_partial_error` for the branch logic, which is unit-tested
/// independently).
/// What: If `palace` query param is set, delegates to `MemoryService::recall`
/// for that palace. Otherwise delegates to `MemoryService::recall_all`.
/// Returns a JSON array of `{ palace_id, drawer_id, content, score, layer }`
/// entries sorted by score descending, or a 500 on failure.
/// Test: `recall_all_handler_honors_palace_filter`,
/// `recall_all_handler_fans_out_without_palace_param`,
/// `recall_all_handler_surfaces_error_envelope`,
/// `extract_partial_error_*` (unit tests for the branch logic).
pub(super) async fn recall_all_handler(
    State(state): State<AppState>,
    Query(q): Query<RecallQuery>,
) -> Result<Json<Value>, ApiError> {
    // Issue #465: honour the `palace=` query param when present.
    if let Some(ref palace_id) = q.palace.filter(|s| !s.is_empty()) {
        let value = crate::service::MemoryService::new(state)
            .recall(
                palace_id,
                &q.q,
                q.top_k.unwrap_or(10),
                q.deep.unwrap_or(false),
            )
            .await?;
        return Ok(Json(value));
    }
    let value = crate::service::MemoryService::new(state)
        .recall_all(&q.q, q.top_k.unwrap_or(10), q.deep.unwrap_or(false))
        .await;

    // Issue #1102: surface all failure envelopes rather than only checking the
    // top-level "error" key. `recall_all` currently returns `{"error":"..."}` on
    // full failure and a JSON array on success; the `extract_partial_error`
    // helper guards against a future partial-success envelope carrying a
    // non-empty "errors" array so that partial failure is never silently
    // passed through as 200 OK.
    if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
        return Err(ApiError::internal(err.to_string()));
    }
    if let Some(msg) = extract_partial_error(&value) {
        return Err(ApiError::internal(msg));
    }
    Ok(Json(value))
}
