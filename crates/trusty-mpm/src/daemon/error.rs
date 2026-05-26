//! Daemon domain error type.
//!
//! Why: the HTTP handlers previously returned bare `StatusCode`s, which buries
//! the *reason* a request failed at the call site and forces every handler to
//! repeat the `.ok_or(StatusCode::NOT_FOUND)` boilerplate. A single domain
//! error enum lets the domain services (in `services/`) speak in terms of what
//! went wrong — a missing session, a blocked tool call — while one
//! `IntoResponse` impl maps each variant to the right HTTP status. Business
//! logic stays HTTP-agnostic; the transport mapping lives in one place.
//! What: [`DaemonError`] enumerates every way a daemon request can fail, with a
//! `thiserror`-derived `Display` and an `axum::IntoResponse` that picks the
//! status code and renders a `{ "error": <message> }` body.
//! Test: `error_status_codes_map` asserts the variant → status mapping.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

/// A failure surfaced by a daemon domain service or request handler.
///
/// Why: domain services must report *why* an operation failed without knowing
/// they are behind HTTP; a typed enum keeps the failure self-describing and
/// lets `?` propagate it cleanly from a service into a handler.
/// What: one variant per failure mode the daemon can produce; each carries the
/// context needed to render an operator-facing message.
/// Test: `error_status_codes_map`, `error_messages_include_context`.
#[derive(Debug, Error)]
pub enum DaemonError {
    /// No session matched the supplied id or friendly name.
    #[error("session not found: {id}")]
    SessionNotFound {
        /// The id or name that failed to resolve.
        id: String,
    },

    /// The session exists but is in a state that forbids the operation.
    #[error("session not active: {id} (status: {status})")]
    SessionNotActive {
        /// The session id or name.
        id: String,
        /// The session's current status, lowercased.
        status: String,
    },

    /// The overseer halted the request before it could proceed.
    #[error("overseer blocked: {reason}")]
    OverseerBlocked {
        /// The overseer's stated reason for the block.
        reason: String,
    },

    /// A tmux operation could not be completed.
    #[error("tmux unavailable: {0}")]
    TmuxUnavailable(String),

    /// The request body or parameters were malformed.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// No checkpoint matched the supplied id.
    #[error("checkpoint not found: {id}")]
    CheckpointNotFound {
        /// The checkpoint id that failed to resolve.
        id: String,
    },

    /// A bot pairing code was wrong or had expired.
    #[error("pair code invalid or expired")]
    InvalidPairCode,

    /// An unexpected internal failure (IO, serialization, ...).
    #[error("internal error: {0}")]
    Internal(String),

    /// A requested capability is not configured on this daemon.
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    /// The request is syntactically valid but semantically un-fulfillable.
    ///
    /// Why: a precondition the daemon cannot satisfy (e.g. `POST /sessions`
    /// in spawn mode when the `claude` binary is missing from `PATH`) is not
    /// a malformed request (`400`) and not a missing resource (`404`) — it
    /// maps to HTTP 422 Unprocessable Entity, which exactly describes
    /// "well-formed but unprocessable".
    /// What: carries a human-readable reason.
    /// Test: `error_status_codes_map`, `spawn_session_without_claude_returns_422`.
    #[error("unprocessable: {0}")]
    Unprocessable(String),
}

impl DaemonError {
    /// The HTTP status this error maps to.
    ///
    /// Why: the `IntoResponse` impl and any caller inspecting the error (tests,
    /// the MCP backend) need the status without re-deriving the mapping.
    /// What: returns the canonical status per variant.
    /// Test: `error_status_codes_map`.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::SessionNotFound { .. } | Self::CheckpointNotFound { .. } => StatusCode::NOT_FOUND,
            Self::SessionNotActive { .. } => StatusCode::CONFLICT,
            Self::OverseerBlocked { .. } => StatusCode::FORBIDDEN,
            Self::InvalidRequest(_) | Self::InvalidPairCode => StatusCode::BAD_REQUEST,
            Self::TmuxUnavailable(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
        }
    }
}

/// Render a [`DaemonError`] as an HTTP response.
///
/// Why: axum handlers return `Result<_, DaemonError>`; this is the single seam
/// that turns a domain failure into a status code plus a JSON error body.
/// What: emits `(status, Json({ "error": <message> }))`.
/// Test: exercised indirectly by every handler test that asserts an error
/// status; the mapping itself is unit-tested in `error_status_codes_map`.
impl IntoResponse for DaemonError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = Json(serde_json::json!({ "error": self.to_string() }));
        (status, body).into_response()
    }
}

impl From<std::io::Error> for DaemonError {
    fn from(e: std::io::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_status_codes_map() {
        // Each variant must map to the documented HTTP status.
        assert_eq!(
            DaemonError::SessionNotFound { id: "x".into() }.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            DaemonError::CheckpointNotFound { id: "x".into() }.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            DaemonError::SessionNotActive {
                id: "x".into(),
                status: "stopped".into(),
            }
            .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            DaemonError::OverseerBlocked { reason: "x".into() }.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            DaemonError::InvalidRequest("x".into()).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            DaemonError::InvalidPairCode.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            DaemonError::TmuxUnavailable("x".into()).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            DaemonError::Internal("x".into()).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            DaemonError::ServiceUnavailable("x".into()).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            DaemonError::Unprocessable("x".into()).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn error_messages_include_context() {
        // The `Display` impl must surface the contextual fields so an operator
        // reading the JSON error body can tell what failed.
        let e = DaemonError::SessionNotFound {
            id: "tmpm-blue-fox".into(),
        };
        assert!(e.to_string().contains("tmpm-blue-fox"));

        let e = DaemonError::SessionNotActive {
            id: "abc".into(),
            status: "stopped".into(),
        };
        assert!(e.to_string().contains("abc"));
        assert!(e.to_string().contains("stopped"));
    }
}
