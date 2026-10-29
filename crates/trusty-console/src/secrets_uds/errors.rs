//! Every answer the secrets bridge gives that is not the server's result
//! (#9066, DOC-74 §15.6).
//!
//! Why: a refusal must never echo what the client sent or what the server
//! said beyond its machine-readable kind. A serde message can quote a value,
//! and the server's `message` is its own business. So every error body here
//! is a fixed sentence chosen by a closed table.
//! What: [`Refusal`], the console's own refusals (guard, decode, dial); and
//! [`UPSTREAM_KINDS`], the closed table from the server's wire
//! `error.data.kind` to an HTTP status and fixed text. A kind outside the
//! table, or none, is [`Refusal::UnknownUpstream`].
//! Test: `upstream_errors_relay_only_the_kind`,
//! `console_kind_table_matches_the_server_error_kinds`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// A refusal the console decides itself. Each one has a fixed status and text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The request arrived on the tailnet listener.
    Tailnet,
    /// The console does not know its own loopback authority yet.
    OriginUnknown,
    /// `Host` or `Origin` is absent, repeated, or not this listener.
    Origin,
    /// A CORS preflight; no secrets route answers one.
    Preflight,
    /// A method other than `POST`.
    MethodNotAllowed,
    /// The CSRF header is absent or wrong.
    Csrf,
    /// The body is larger than the cap.
    TooLarge,
    /// The body did not decode into the route's request.
    BadBody,
    /// `resolve` has no console route.
    ResolveUnsupported,
    /// No secrets route has this path.
    UnknownRoute,
    /// The secrets server cannot be started, or is absent in external mode.
    Unavailable,
    /// The server was reached but the exchange failed.
    Transport,
    /// The server answered with a kind outside the table, or none.
    UnknownUpstream,
    /// This target has no secrets bridge.
    PlatformUnsupported,
    /// The tm project registry could not be found, read or parsed (#9067).
    RegistryUnavailable,
}

impl Refusal {
    /// The HTTP status and fixed text for this refusal.
    pub fn status_and_text(self) -> (StatusCode, &'static str) {
        match self {
            Self::Tailnet => (
                StatusCode::FORBIDDEN,
                "secrets are not served on the tailnet listener",
            ),
            Self::OriginUnknown => (
                StatusCode::FORBIDDEN,
                "the console does not know its own address yet",
            ),
            Self::Origin => (
                StatusCode::FORBIDDEN,
                "the request is not from this console's own page",
            ),
            Self::Preflight => (StatusCode::FORBIDDEN, "cross-origin requests are refused"),
            Self::MethodNotAllowed => (
                StatusCode::METHOD_NOT_ALLOWED,
                "secrets routes accept POST only",
            ),
            Self::Csrf => (
                StatusCode::FORBIDDEN,
                "the request token is missing or wrong",
            ),
            Self::TooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "the request body is too large",
            ),
            Self::BadBody => (
                StatusCode::BAD_REQUEST,
                "the request body does not match this route",
            ),
            Self::ResolveUnsupported => (
                StatusCode::NOT_IMPLEMENTED,
                "secret values are never resolved through the console",
            ),
            Self::UnknownRoute => (StatusCode::NOT_FOUND, "no such secrets route"),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "the secrets server is not available",
            ),
            Self::Transport => (
                StatusCode::BAD_GATEWAY,
                "the secrets server did not complete the request",
            ),
            Self::UnknownUpstream => (
                StatusCode::BAD_GATEWAY,
                "the secrets server answered with an unknown error",
            ),
            Self::PlatformUnsupported => (
                StatusCode::NOT_IMPLEMENTED,
                "secrets are not available on this platform",
            ),
            Self::RegistryUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "the tm project registry could not be read",
            ),
        }
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let (status, text) = self.status_and_text();
        (status, Json(json!({ "error": text }))).into_response()
    }
}

const BAD_REQUEST: &str = "the secrets server refused the request as malformed";
const FORBIDDEN: &str = "the secrets server refused the request";
const NOT_FOUND: &str = "the secret was not found";
const CONFLICT: &str = "the secrets store is busy";
const UNPROCESSABLE: &str = "the secrets configuration does not allow this request";
const TOO_MANY: &str = "the secrets server limit was reached";
const INTERNAL: &str = "the secrets server failed";
const UNAVAILABLE: &str = "the secrets store is not available";
const TIMEOUT: &str = "the secrets server ran out of time";

/// Every `error.data.kind` the server sends, with the console's status and
/// fixed text for it.
///
/// Why: trusty-secrets' `ErrorKind` lives behind its `server` feature, which
/// the console does not link (owner ruling Df0); this is the client's copy.
/// What: one row per kind, in the server's declaration order.
/// Test: `console_kind_table_matches_the_server_error_kinds` pins it to the
/// server's `ErrorKind::as_str`.
pub const UPSTREAM_KINDS: [(&str, StatusCode, &str); 38] = [
    ("invalid_params", StatusCode::BAD_REQUEST, BAD_REQUEST),
    ("project_invalid", StatusCode::BAD_REQUEST, BAD_REQUEST),
    (
        "project_unresolved",
        StatusCode::UNPROCESSABLE_ENTITY,
        UNPROCESSABLE,
    ),
    ("vault_out_of_scope", StatusCode::FORBIDDEN, FORBIDDEN),
    ("invalid_value", StatusCode::BAD_REQUEST, BAD_REQUEST),
    ("not_found", StatusCode::NOT_FOUND, NOT_FOUND),
    (
        "unsupported",
        StatusCode::UNPROCESSABLE_ENTITY,
        UNPROCESSABLE,
    ),
    (
        "unknown_backend",
        StatusCode::UNPROCESSABLE_ENTITY,
        UNPROCESSABLE,
    ),
    ("backend_failed", StatusCode::BAD_GATEWAY, INTERNAL),
    ("orphaned_backend_entry", StatusCode::CONFLICT, CONFLICT),
    ("index_corrupt", StatusCode::INTERNAL_SERVER_ERROR, INTERNAL),
    ("index_busy", StatusCode::CONFLICT, CONFLICT),
    (
        "storage_unavailable",
        StatusCode::SERVICE_UNAVAILABLE,
        UNAVAILABLE,
    ),
    (
        "config_invalid",
        StatusCode::UNPROCESSABLE_ENTITY,
        UNPROCESSABLE,
    ),
    (
        "home_unavailable",
        StatusCode::SERVICE_UNAVAILABLE,
        UNAVAILABLE,
    ),
    ("same_backend", StatusCode::BAD_REQUEST, BAD_REQUEST),
    ("agent_use_refused", StatusCode::FORBIDDEN, FORBIDDEN),
    ("invalid_env_entry", StatusCode::BAD_REQUEST, BAD_REQUEST),
    (
        "env_resolution_failed",
        StatusCode::UNPROCESSABLE_ENTITY,
        UNPROCESSABLE,
    ),
    ("dotenv_syntax", StatusCode::BAD_REQUEST, BAD_REQUEST),
    ("remote_host_unsupported", StatusCode::FORBIDDEN, FORBIDDEN),
    ("storage_refused", StatusCode::FORBIDDEN, FORBIDDEN),
    ("tracked_backend_refused", StatusCode::FORBIDDEN, FORBIDDEN),
    (
        "audit_unavailable",
        StatusCode::SERVICE_UNAVAILABLE,
        UNAVAILABLE,
    ),
    ("tracked_audit_refused", StatusCode::FORBIDDEN, FORBIDDEN),
    (
        "tracked_cli_setting_refused",
        StatusCode::FORBIDDEN,
        FORBIDDEN,
    ),
    (
        "cli_not_installed",
        StatusCode::UNPROCESSABLE_ENTITY,
        UNPROCESSABLE,
    ),
    ("backend_locked", StatusCode::CONFLICT, CONFLICT),
    (
        "file_backend_not_selected",
        StatusCode::UNPROCESSABLE_ENTITY,
        UNPROCESSABLE,
    ),
    (
        "backend_not_enabled",
        StatusCode::UNPROCESSABLE_ENTITY,
        UNPROCESSABLE,
    ),
    ("deadline_exceeded", StatusCode::GATEWAY_TIMEOUT, TIMEOUT),
    ("vault_not_visible", StatusCode::FORBIDDEN, FORBIDDEN),
    ("backend_timeout", StatusCode::GATEWAY_TIMEOUT, TIMEOUT),
    ("grant_refused", StatusCode::FORBIDDEN, FORBIDDEN),
    (
        "grant_limit_reached",
        StatusCode::TOO_MANY_REQUESTS,
        TOO_MANY,
    ),
    ("bridge_refused", StatusCode::FORBIDDEN, FORBIDDEN),
    // #10459: a per-key deny, split from `agent_use_refused`.
    ("agent_key_denied", StatusCode::FORBIDDEN, FORBIDDEN),
    ("internal", StatusCode::INTERNAL_SERVER_ERROR, INTERNAL),
];

/// The response for a server error carrying `kind`.
///
/// What: a known kind answers its row's status and text, plus the kind
/// itself so the page can branch on it; anything else is
/// [`Refusal::UnknownUpstream`]. The server's `message` is never read.
/// Test: `upstream_errors_relay_only_the_kind`.
pub fn upstream_response(kind: Option<&str>) -> Response {
    let row = kind.and_then(|kind| UPSTREAM_KINDS.iter().find(|(k, _, _)| *k == kind));
    match row {
        Some((kind, status, text)) => {
            (*status, Json(json!({ "error": text, "kind": kind }))).into_response()
        }
        None => Refusal::UnknownUpstream.into_response(),
    }
}
