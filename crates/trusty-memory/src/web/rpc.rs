//! JSON-RPC 2.0 dispatch endpoint and shared HTTP helpers.
//!
//! Why: The multi-transport refactor needs a single HTTP route that accepts
//! the same JSON-RPC envelopes the UDS transport speaks, without forcing
//! every caller to learn the per-tool REST vocabulary.
//! What: `POST /rpc` handler, `creator_info_from_http` attribution extractor,
//! and `parse_iso_or_bad_request` timestamp parser shared with other modules.
//! Test: `drawer_creator_attribution_http_*` and `http_rpc_endpoint_roundtrip`
//! tests in `web::tests`.

use axum::{extract::State, http::HeaderMap, Json};

use crate::attribution::{
    CreatorInfo, CreatorSource, HTTP_DEFAULT_CLIENT, X_TRUSTY_CLIENT_CWD, X_TRUSTY_CLIENT_NAME,
};
use crate::AppState;

use super::error::ApiError;

/// `POST /rpc` — JSON-RPC 2.0 dispatch endpoint.
///
/// Why: the multi-transport refactor needs a single HTTP route that
/// accepts the same envelopes the UDS transport speaks. Browser
/// clients that want the new tool surface (or third-party scripts
/// that prefer JSON-RPC to REST) can POST a request envelope here
/// and get a response back without learning the per-tool REST
/// vocabulary. The existing `/api/v1/*` REST routes continue to work
/// unchanged — this is purely additive.
/// What: deserialises a [`JsonRpcRequest`] from the request body,
/// calls [`crate::transport::rpc::dispatch`], and returns the
/// [`JsonRpcResponse`] as JSON. Always returns HTTP 200 with the
/// envelope inside (JSON-RPC errors are carried in the `error`
/// field, not the HTTP status). Returns HTTP 400 only on JSON
/// deserialisation failure of the outer envelope.
/// Test: `http_rpc_endpoint_roundtrip` in `web::tests`.
pub(super) async fn rpc_handler(
    State(state): State<AppState>,
    Json(req): Json<crate::transport::rpc::JsonRpcRequest>,
) -> Json<crate::transport::rpc::JsonRpcResponse> {
    let resp = crate::transport::rpc::dispatch(&state, req).await;
    Json(resp)
}

/// Extract a [`CreatorInfo`] for an HTTP write request.
///
/// Why: every HTTP write path (drawers, messages) must attach
/// attribution tags so operators can trace which client wrote which
/// drawer. Centralising the extraction here keeps the `X-Trusty-Client-*`
/// header contract in one place.
/// What: pulls `X-Trusty-Client-Name` (default
/// [`HTTP_DEFAULT_CLIENT`]) and the optional `X-Trusty-Client-Cwd`
/// header off the request, then builds a `CreatorInfo` with
/// `source = Http` and the current daemon crate version.
/// Test: `drawer_creator_attribution_http_default`,
/// `drawer_creator_attribution_http_header`.
pub(crate) fn creator_info_from_http(headers: &HeaderMap) -> CreatorInfo {
    let client = headers
        .get(X_TRUSTY_CLIENT_NAME)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(HTTP_DEFAULT_CLIENT)
        .to_string();
    let cwd = headers
        .get(X_TRUSTY_CLIENT_CWD)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    CreatorInfo {
        client,
        version: env!("CARGO_PKG_VERSION").to_string(),
        source: CreatorSource::Http,
        cwd,
    }
}

/// Parse an optional ISO-8601 timestamp string for the activity filter.
///
/// Why: the `since` / `until` query params are user-supplied; a bad value
/// should reject the request with a clear 400 rather than be silently
/// dropped (which would return seemingly-correct but mis-filtered data).
/// What: returns `Ok(None)` when the input is `None` or empty;
/// `Ok(Some(_))` on a parseable RFC 3339 timestamp; `Err(ApiError::bad_request)`
/// otherwise.
/// Test: `activity_endpoint_lists_recent_emits` exercises the happy path
/// (no timestamps); a bad timestamp returns 400 — see manual curl.
pub(crate) fn parse_iso_or_bad_request(
    s: Option<&str>,
    field: &str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    match s {
        None | Some("") => Ok(None),
        Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
            .map(|dt| Some(dt.with_timezone(&chrono::Utc)))
            .map_err(|e| ApiError::bad_request(format!("invalid {field} (RFC 3339): {e}"))),
    }
}
