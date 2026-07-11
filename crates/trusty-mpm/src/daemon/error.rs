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

    /// No Deliverable matched the supplied id (within the named project).
    ///
    /// Why: the Deliverable CRUD routes (#2378) need a 404 distinct from the
    /// session/checkpoint not-founds so the error message names the right
    /// resource.
    /// What: carries the id (or malformed id string); maps to HTTP 404.
    /// Test: `error_status_codes_map`, and the deliverable route tests.
    #[error("deliverable not found: {id}")]
    DeliverableNotFound {
        /// The Deliverable id that failed to resolve.
        id: String,
    },

    /// No Milestone matched the supplied id (within the named project).
    #[error("milestone not found: {id}")]
    MilestoneNotFound {
        /// The Milestone id that failed to resolve.
        id: String,
    },

    /// A requested Deliverable status change is not a legal transition (#2380).
    ///
    /// Why: the §10.3 state machine rejects illegal `set-status` requests (e.g.
    /// `proposed → complete`). A dedicated variant lets the error body name the
    /// legal next states so the caller can self-correct, and maps to 409 Conflict
    /// (the requested change conflicts with the record's current state) — the same
    /// class as [`SessionNotActive`](Self::SessionNotActive).
    /// What: carries the `from`/`to` labels and the legal `allowed` next states;
    /// [`into_response`](Self::into_response) surfaces `allowed` as a JSON array.
    /// Test: `error_status_codes_map`, `invalid_transition_body_lists_allowed`.
    #[error(
        "invalid status transition {from} \u{2192} {to}; legal next states from {from}: [{}]",
        allowed.join(", ")
    )]
    InvalidTransition {
        /// The Deliverable's current status.
        from: String,
        /// The requested (rejected) target status.
        to: String,
        /// The states that would have been legal transitions from `from`.
        allowed: Vec<String>,
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

    /// The request was rejected by a security guard (e.g. the cross-origin CSRF
    /// guard on the browser-facing, action-capable chat endpoint).
    ///
    /// Why: distinct from [`OverseerBlocked`](Self::OverseerBlocked) (which is a
    /// policy decision about a *session*), this is a transport-level rejection of
    /// the *request itself* — it never reached business logic. A dedicated variant
    /// keeps the operator message self-describing and maps cleanly to 403.
    /// What: carries a human-readable reason; maps to HTTP 403 Forbidden.
    /// Test: `error_status_codes_map`, and the origin-guard handler tests.
    #[error("forbidden: {0}")]
    Forbidden(String),

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
            Self::SessionNotFound { .. }
            | Self::CheckpointNotFound { .. }
            | Self::DeliverableNotFound { .. }
            | Self::MilestoneNotFound { .. } => StatusCode::NOT_FOUND,
            Self::SessionNotActive { .. } | Self::InvalidTransition { .. } => StatusCode::CONFLICT,
            Self::OverseerBlocked { .. } | Self::Forbidden(_) => StatusCode::FORBIDDEN,
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
        // #2380: an invalid transition surfaces the legal next states as a
        // structured `allowed_next` array so the caller can self-correct without
        // parsing the message string.
        let body = match &self {
            Self::InvalidTransition { from, to, allowed } => Json(serde_json::json!({
                "error": self.to_string(),
                "from": from,
                "to": to,
                "allowed_next": allowed,
            })),
            _ => Json(serde_json::json!({ "error": self.to_string() })),
        };
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
            DaemonError::Forbidden("x".into()).status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            DaemonError::Unprocessable("x".into()).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            DaemonError::DeliverableNotFound { id: "x".into() }.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            DaemonError::MilestoneNotFound { id: "x".into() }.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            DaemonError::InvalidTransition {
                from: "proposed".into(),
                to: "complete".into(),
                allowed: vec!["in-progress".into()],
            }
            .status(),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn invalid_transition_message_lists_allowed() {
        // The Display must name the from-state, to-state, and legal next states
        // so an operator reading the plain error string can self-correct.
        let e = DaemonError::InvalidTransition {
            from: "proposed".into(),
            to: "complete".into(),
            allowed: vec!["in-progress".into()],
        };
        let msg = e.to_string();
        assert!(msg.contains("proposed"), "{msg}");
        assert!(msg.contains("complete"), "{msg}");
        assert!(msg.contains("in-progress"), "{msg}");
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
