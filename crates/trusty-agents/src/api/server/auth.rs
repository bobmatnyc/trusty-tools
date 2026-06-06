//! Server configuration + bearer-token auth middleware (#181).
//!
//! Why: The server binds `0.0.0.0`, so any process on the LAN can reach the
//! REST API + UI. Bearer-token auth, configurable per launch, gates the
//! sensitive `/api/*` routes while exempting health/config/events probes and
//! the embedded UI assets.
//! What: `ApiConfig` carries the port + optional token; `auth_middleware`
//! enforces the token; `ApiClientConfig` tells the UI whether auth is needed.
//! Test: `auth_middleware_*` and `config_endpoint_*` in `super::tests`.

use axum::{
    Json,
    extract::State,
    http::{Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Server configuration. (#181)
///
/// Why: Bearer-token auth must be configurable per-launch so users can run
/// the server unauthenticated for local-only dev or token-protected when
/// exposing the bound port over a LAN. Keeping this as a struct (instead of
/// extra bare args to `serve`) gives us a clean place to grow more knobs
/// (TLS, allowed origins, …) without breaking callers.
/// What: `port` is the TCP port to bind on `0.0.0.0`. `token`, when `Some`,
/// makes every `/api/*` route (except `GET /api/health`) require an
/// `Authorization: Bearer <token>` header that exactly matches `token`.
/// Test: `auth_middleware_rejects_missing_token`, `auth_middleware_allows_health`.
#[derive(Clone, Debug)]
pub struct ApiConfig {
    pub port: u16,
    pub token: Option<String>,
}

impl ApiConfig {
    /// Convenience constructor mirroring the previous bare-port API.
    ///
    /// Why: Lets unauthenticated callers (tests, `serve`) construct a config
    /// without naming the `token` field.
    /// What: Builds `ApiConfig { port, token: None }`.
    /// Test: Used by `serve` and `super::tests`.
    #[allow(dead_code)]
    pub fn unauthenticated(port: u16) -> Self {
        Self { port, token: None }
    }
}

/// Wrapper used by the auth middleware so axum can extract the optional
/// configured token from request state.
#[derive(Clone)]
pub(super) struct AuthState {
    pub(super) token: String,
}

/// Bearer-token authentication middleware. (#181)
///
/// Why: The server binds `0.0.0.0`, so any process on the LAN can reach the
/// REST API + UI. When the operator sets a token, we reject requests that
/// don't present it. We deliberately exempt `GET /api/health` so probes from
/// load balancers or healthchecks don't need credentials, and exempt the
/// embedded UI's static assets so a browser can load `index.html` and obtain
/// the token via `/api/config` before issuing authenticated requests.
/// What: For requests under `/api/*` (other than `/api/health`), checks
/// `Authorization: Bearer <token>` and returns 401 JSON
/// `{"error":"unauthorized"}` on mismatch.
/// Test: `auth_middleware_rejects_missing_token`,
/// `auth_middleware_accepts_valid_token`, `auth_middleware_allows_health`.
pub(super) async fn auth_middleware(
    State(auth): State<AuthState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();

    // Public endpoints — never require auth:
    //   - GET /api/health  (health probes)
    //   - GET /api/config  (UI bootstrap: tells client whether auth is needed)
    //   - any non-/api path (UI static assets at "/" and "/*path")
    // #192 Phase B: `/api/events` is exempt from Bearer auth because the
    // browser EventSource API cannot attach custom Authorization headers.
    // Auth-sensitive deployments should front the server with a reverse proxy
    // and gate `/api/events` there (e.g. mTLS or cookie-based auth) since the
    // event stream itself contains only telemetry, not actionable controls.
    if path == "/api/health"
        || path == "/api/config"
        || path == "/api/events"
        || !path.starts_with("/api/")
    {
        return next.run(req).await;
    }

    let header_val = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim);

    match header_val {
        Some(t) if t == auth.token => next.run(req).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response(),
    }
}

/// Body returned by `GET /api/config`. (#181)
///
/// Why: The browser-served UI needs to know whether to attach a bearer token
/// to its requests. Rather than embedding the token into HTML (which would
/// leak via view-source), we publish only a boolean flag. The token itself
/// is provided to the user out-of-band and pasted into the UI.
/// What: A single `auth_required` boolean serialized to JSON.
/// Test: `config_endpoint_reports_auth_required_true`/`_false`.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ApiClientConfig {
    pub(super) auth_required: bool,
}
