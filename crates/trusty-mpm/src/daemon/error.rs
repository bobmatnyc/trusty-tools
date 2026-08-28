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
//! `thiserror`-derived `Display`, an `axum::IntoResponse` that picks the status
//! code and renders a `{ "error": <message> }` body, and — since #6288 — a
//! `From<DaemonError> for RpcError` that picks the JSON-RPC error code for the
//! same failure on the Unix socket. Both transports read the SAME variant, so a
//! route served over HTTP and over the socket cannot disagree about what went
//! wrong.
//! Test: `error_status_codes_map` asserts the variant → status mapping;
//! `rpc_error_codes_track_http_statuses` asserts the variant → code mapping.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;
use trusty_common::uds::server::{CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS, RpcError};

// ---- JSON-RPC error codes for the daemon's non-standard failure kinds -------
//
// Why these are declared here rather than in `trusty_common::uds::server::wire`:
// that module owns the codes JSON-RPC itself reserves (-32700..-32600) plus the
// two streaming codes #6286 added. Everything below is a DOMAIN failure kind
// with no JSON-RPC counterpart, so it lives beside the enum it describes.
//
// The numbers are not arbitrary. -32004 and -32005 are already spent by
// `trusty-analyze` (`service::events::CODE_NOT_FOUND`, `CODE_DEADLINE_EXCEEDED`)
// and -32010/-32011 by `trusty-common`'s streaming pair, so #6288 takes free
// slots around them and reuses -32004 for the SAME meaning — a later
// consolidation into `trusty-common` is then a move, not a renumbering.
// Test: `rpc_error_codes_track_http_statuses`.

/// HTTP 404 over the socket — the same code `trusty-analyze` uses.
pub const CODE_NOT_FOUND: i64 = -32004;

/// HTTP 503 over the socket: a capability this daemon is not configured for.
pub const CODE_UNAVAILABLE: i64 = -32002;

/// HTTP 403 over the socket: a guard rejected the request.
pub const CODE_FORBIDDEN: i64 = -32003;

/// HTTP 422 over the socket: well-formed but un-fulfillable.
pub const CODE_UNPROCESSABLE: i64 = -32006;

/// HTTP 409 over the socket: the request conflicts with the record's state.
pub const CODE_CONFLICT: i64 = -32009;

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

    /// No registered project matches the given `repo_url`, so a Deliverable
    /// id cannot be scoped against it (#2379 review MEDIUM).
    ///
    /// Why: distinct from [`DeliverableNotFound`](Self::DeliverableNotFound) —
    /// the Deliverable itself may well exist; what's missing is the PROJECT
    /// identity to scope it against. Returning `DeliverableNotFound` here
    /// would misleadingly imply the id itself was bad, when the actual gap is
    /// "no project is registered for this repo at all".
    /// What: carries the `repo_url` that failed to resolve; maps to HTTP 404
    /// (there is genuinely no project resource for this identity).
    /// Test: `error_status_codes_map`, and
    /// `validate_deliverable_scope_unknown_project_is_404`.
    #[error("no registered project matches repo_url {repo_url:?}; cannot scope deliverable")]
    ProjectNotFoundForRepoUrl {
        /// The `repo_url` that did not resolve to any registered project.
        repo_url: String,
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

    /// A resource of a kind with no dedicated variant above was not found.
    ///
    /// Why (#6288): the `/claude-config/*` routes 404 on an unknown
    /// recommendation id and an unknown profile name. Both used to be a bare
    /// `StatusCode::NOT_FOUND` returned straight from the handler, which
    /// carried no message. Sharing one body between HTTP and RPC means the
    /// failure has to be a typed value, and neither resource is a session, a
    /// checkpoint, a deliverable, or a milestone.
    /// What: carries the operator-facing message; maps to HTTP 404 and to
    /// [`CODE_NOT_FOUND`](crate::daemon::error::CODE_NOT_FOUND) on the socket.
    /// Test: `error_status_codes_map`, `rpc_error_codes_track_http_statuses`.
    #[error("not found: {0}")]
    NotFound(String),

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
            | Self::MilestoneNotFound { .. }
            | Self::ProjectNotFoundForRepoUrl { .. }
            | Self::NotFound(_) => StatusCode::NOT_FOUND,
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

/// Map a daemon failure onto the JSON-RPC error frame a socket client reads.
///
/// Why (#6288): `RpcRouter::typed` takes `Result<Resp, RpcError>`, and every
/// route this slice moves onto the socket already reports its failures as a
/// [`DaemonError`]. Without this conversion each RPC registration would spell
/// its own status-to-code table, and the HTTP status and the RPC code would be
/// free to drift apart for the same variant.
/// What: derives the code from [`DaemonError::status`], so the code is a pure
/// function of the status the HTTP transport would have sent. The message
/// crosses verbatim — it is the same string the HTTP body's `error` field
/// carries, which is what makes a parity assertion meaningful.
/// Test: `rpc_error_codes_track_http_statuses`;
/// `rpc_tmux_snapshot_unknown_session_reports_a_coded_error` drives one arm
/// through a real failure.
impl From<DaemonError> for RpcError {
    fn from(e: DaemonError) -> Self {
        let code = match e.status() {
            StatusCode::BAD_REQUEST => CODE_INVALID_PARAMS,
            StatusCode::NOT_FOUND => CODE_NOT_FOUND,
            StatusCode::FORBIDDEN => CODE_FORBIDDEN,
            StatusCode::CONFLICT => CODE_CONFLICT,
            StatusCode::UNPROCESSABLE_ENTITY => CODE_UNPROCESSABLE,
            StatusCode::SERVICE_UNAVAILABLE => CODE_UNAVAILABLE,
            // Everything left maps to 500, and a caller could not have sent any
            // of them differently.
            _ => CODE_INTERNAL_ERROR,
        };
        RpcError::new(code, e.to_string())
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
            DaemonError::ProjectNotFoundForRepoUrl {
                repo_url: "x".into()
            }
            .status(),
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

    /// Why (#6288): the socket and HTTP must never disagree about what a failure
    /// was. The conversion derives the code FROM the status, so this pins that
    /// derivation — a variant whose status changes moves its RPC code with it,
    /// and a variant silently falling into the catch-all `internal` arm shows up
    /// here.
    /// Test: this function IS the test.
    #[test]
    fn rpc_error_codes_track_http_statuses() {
        let cases: Vec<(DaemonError, i64)> = vec![
            (
                DaemonError::SessionNotFound { id: "x".into() },
                CODE_NOT_FOUND,
            ),
            (DaemonError::NotFound("x".into()), CODE_NOT_FOUND),
            (
                DaemonError::CheckpointNotFound { id: "x".into() },
                CODE_NOT_FOUND,
            ),
            (DaemonError::InvalidRequest("x".into()), CODE_INVALID_PARAMS),
            (DaemonError::Forbidden("x".into()), CODE_FORBIDDEN),
            (
                DaemonError::SessionNotActive {
                    id: "x".into(),
                    status: "stopped".into(),
                },
                CODE_CONFLICT,
            ),
            (DaemonError::Unprocessable("x".into()), CODE_UNPROCESSABLE),
            (
                DaemonError::ServiceUnavailable("x".into()),
                CODE_UNAVAILABLE,
            ),
            (DaemonError::Internal("x".into()), CODE_INTERNAL_ERROR),
            (
                DaemonError::TmuxUnavailable("x".into()),
                CODE_INTERNAL_ERROR,
            ),
        ];

        for (err, expected) in cases {
            let message = err.to_string();
            let rpc: RpcError = err.into();
            assert_eq!(rpc.code, expected, "wrong code for {message:?}");
            assert_eq!(
                rpc.message, message,
                "the RPC message must be the HTTP body's message verbatim"
            );
        }
    }

    /// Why: the #6288 variant is new, so its 404 mapping needs its own
    /// assertion rather than riding on the pre-existing table above.
    /// Test: this function IS the test.
    #[test]
    fn not_found_variant_is_404() {
        assert_eq!(
            DaemonError::NotFound("profile `nope`".into()).status(),
            StatusCode::NOT_FOUND
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

    #[test]
    fn project_not_found_message_names_repo_url_not_deliverable() {
        // #2379 review MEDIUM: the message must talk about the missing
        // PROJECT, never imply the deliverable id itself was bad.
        let e = DaemonError::ProjectNotFoundForRepoUrl {
            repo_url: "/local/path/checkout".into(),
        };
        let msg = e.to_string();
        assert!(msg.contains("/local/path/checkout"), "{msg}");
        assert!(msg.contains("project"), "{msg}");
        assert!(
            !msg.contains("deliverable not found"),
            "must not read as a deliverable-not-found error: {msg}"
        );
    }
}
