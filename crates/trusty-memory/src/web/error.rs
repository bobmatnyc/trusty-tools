//! HTTP handler error type and palace handle helper.
//!
//! Why: Centralises the `ApiError` newtype and `open_handle` helper so every
//! handler submodule can import from one path without duplicating the
//! lightweight error-wrapping logic.
//! What: `ApiError` wraps an HTTP status code + message and converts to an
//! axum `Response`; `open_handle` opens a palace by id and maps open
//! failures to 404.
//! Test: `ApiError` conversions are exercised implicitly by every handler
//! test; `delete_palace_refuses_when_drawers_present` and
//! `remember_async_rejects_short_content` exercise `conflict` and
//! `unprocessable` specifically.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use trusty_common::memory_core::palace::PalaceId;

use crate::AppState;

/// Open a palace handle by id, converting open failures to 404.
///
/// Why: Every handler that references a palace by id has to perform the same
/// registry lookup and map the not-found path to an `ApiError`. Centralising
/// the conversion avoids boilerplate and keeps the 404 message consistent.
/// What: Calls `PalaceRegistry::open_palace`; maps `Err` to
/// `ApiError::not_found(...)`.
/// Test: `delete_palace_returns_not_found_for_missing_id`,
/// `kg_list_subjects_returns_distinct`.
pub(crate) fn open_handle(
    state: &AppState,
    id: &str,
) -> Result<std::sync::Arc<trusty_common::memory_core::PalaceHandle>, ApiError> {
    state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(id))
        .map_err(|e| ApiError::not_found(format!("palace not found: {id} ({e:#})")))
}

/// Lightweight error type for HTTP handlers.
///
/// Why: axum requires handler errors to implement `IntoResponse`. Rather than
/// scattering `(StatusCode, Json)` tuples across every handler, one struct
/// centralises the serialisation and keeps call sites readable.
/// What: Wraps a `StatusCode` and a plain error message; serialises as
/// `{ "error": "<message>" }` with the matching HTTP status.
/// Test: Error variants are exercised by most handler tests; `unprocessable`
/// is specifically tested by `remember_async_rejects_short_content`.
pub(crate) struct ApiError {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
}

impl ApiError {
    pub(crate) fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    pub(crate) fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }
    /// Build a 409 Conflict response.
    ///
    /// Why: `DELETE /palaces/{id}` (issue #180) returns 409 when the
    /// palace still has drawers and `force=true` is not set. A 400 would
    /// be misleading (the request is well-formed) and 404 would lie about
    /// existence.
    /// What: wraps the message with `StatusCode::CONFLICT`.
    /// Test: `delete_palace_refuses_when_drawers_present`.
    #[allow(dead_code)]
    pub(crate) fn conflict(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: msg.into(),
        }
    }
    pub(crate) fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
    /// Build a 422 Unprocessable Entity response.
    ///
    /// Why (issue #466): content that is structurally valid JSON but fails
    /// semantic validation (e.g. too few words to be worth storing) should
    /// return 422 rather than 400 (which implies malformed input) or 200/202
    /// (which would imply success). 422 is the standard HTTP status for
    /// "request understood but semantically unacceptable".
    /// What: wraps the message with `StatusCode::UNPROCESSABLE_ENTITY`.
    /// Test: `remember_async_rejects_short_content`.
    pub(crate) fn unprocessable(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<crate::service::ServiceError> for ApiError {
    fn from(e: crate::service::ServiceError) -> Self {
        match e {
            crate::service::ServiceError::BadRequest(m) => ApiError::bad_request(m),
            crate::service::ServiceError::NotFound(m) => ApiError::not_found(m),
            crate::service::ServiceError::Conflict(m) => ApiError::conflict(m),
            crate::service::ServiceError::Internal(m) => ApiError::internal(m),
        }
    }
}
