//! Server configuration + bearer-token auth middleware (#181).
//!
//! Why: The server binds loopback (`127.0.0.1`) by default, so auth is not the
//! control that keeps it off the LAN — the bind is (#3329). Bearer-token auth
//! covers the one case the bind cannot: an operator who explicitly opts into a
//! non-loopback `--bind`, which `serve_with_config` refuses to start without a
//! token. It gates the sensitive `/api/*` routes while exempting the
//! `/api/health` and `/api/config` bootstrap probes and the embedded UI assets.
//! #5052: `/api/events` is NO LONGER exempt — see `auth_middleware` and
//! `super::event_tickets`.
//! What: `ApiConfig` carries the bind + port + optional token;
//! `auth_middleware` enforces the token; `ApiClientConfig` tells the UI whether
//! auth is needed.
//! Test: `auth_middleware_*` and `config_endpoint_*` in `super::tests`.

use axum::{
    Json,
    extract::State,
    http::{Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use subtle::ConstantTimeEq;

/// Server configuration. (#181, #3329)
///
/// Why: Bearer-token auth must be configurable per-launch so users can run
/// the server unauthenticated for local-only dev or token-protected when
/// exposing the bound port over a LAN. Keeping this as a struct (instead of
/// extra bare args to `serve`) gives us a clean place to grow more knobs
/// (TLS, allowed origins, …) without breaking callers.
/// What: `bind` is the interface to listen on (#3329: defaults to loopback
/// `127.0.0.1`; a non-loopback bind is an explicit opt-in that REQUIRES a
/// token — see `serve_with_config`). `port` is the TCP port. `token`, when
/// `Some`, makes every `/api/*` route (except the public probes) require an
/// `Authorization: Bearer <token>` header that exactly matches `token`.
/// Test: `auth_middleware_rejects_missing_token`, `auth_middleware_allows_health`,
/// and the loopback-doctrine tests in `super::tests::guard`.
#[derive(Clone, Debug)]
pub struct ApiConfig {
    /// Interface to bind. Defaults to loopback (`127.0.0.1`) per the
    /// loopback-only doctrine (#3329); non-loopback is opt-in via `--bind`.
    pub bind: std::net::IpAddr,
    pub port: u16,
    pub token: Option<String>,
}

impl ApiConfig {
    /// Convenience constructor mirroring the previous bare-port API.
    ///
    /// Why: Lets unauthenticated callers (tests, `serve`) construct a config
    /// without naming every field. Binds loopback — the only interface on
    /// which an unauthenticated server is permitted to start (#3329).
    /// What: Builds `ApiConfig { bind: 127.0.0.1, port, token: None }`.
    /// Test: Used by `serve` and `super::tests`.
    #[allow(dead_code)]
    pub fn unauthenticated(port: u16) -> Self {
        Self {
            bind: std::net::Ipv4Addr::LOCALHOST.into(),
            port,
            token: None,
        }
    }

    /// Build a config from an optional operator-supplied bind. (#3329)
    ///
    /// Why: `--bind` is parsed in two independent places — the `--api` early
    /// dispatch in `runtime::startup` (raw argv, so the Tauri sidecar can skip
    /// PM state init) and the clap path in `runtime::mode_dispatch`. Both must
    /// apply the same loopback default; an `unwrap_or(LOCALHOST)` open-coded at
    /// each site leaves a security-relevant default free to drift between them.
    /// `serve_with_config` still enforces the doctrine whatever this returns,
    /// so this is defence in depth rather than the primary control.
    /// What: `None` → `127.0.0.1`. `Some(ip)` is taken verbatim; a non-loopback
    /// `ip` is then refused by `serve_with_config` unless `token` is `Some`.
    /// Test: `unspecified_bind_defaults_to_loopback`.
    pub fn with_bind(bind: Option<std::net::IpAddr>, port: u16, token: Option<String>) -> Self {
        Self {
            bind: bind.unwrap_or_else(|| std::net::Ipv4Addr::LOCALHOST.into()),
            port,
            token,
        }
    }
}

/// Wrapper used by the auth middleware so axum can extract the optional
/// configured token from request state.
#[derive(Clone)]
pub(super) struct AuthState {
    pub(super) token: String,
}

/// Whether a presented bearer token matches the configured one. (#3761)
///
/// Why: Plain `==` short-circuits on the first differing byte, so an attacker
/// who can time their own requests learns the length of the matching prefix and
/// can recover the token byte-by-byte instead of brute-forcing it whole. This
/// is the HIGHER-value of the two secrets the server holds — it gates every
/// `/api/*` route, not just the internal relay — so it gets the same
/// constant-time treatment `relay::relay_authorized` does.
/// What: Rejects on differing length (a token's length is not secret; it is
/// already observable from the request frame, and `ct_eq` needs equal-length
/// slices), then compares the bytes in constant time. An empty configured token
/// is not special-cased here — `auth_middleware` only runs when the operator
/// configured one (see `routes::build_router_with_config`).
/// Test: `bearer_token_matches_is_exact`, `auth_middleware_accepts_valid_token`,
/// `auth_middleware_rejects_wrong_token`.
pub(super) fn bearer_token_matches(expected: &str, provided: &str) -> bool {
    if expected.len() != provided.len() {
        return false;
    }
    expected.as_bytes().ct_eq(provided.as_bytes()).into()
}

/// Bearer-token authentication middleware. (#181)
///
/// Why: On the loopback default this middleware is not installed at all — a
/// local caller reaches every route, which is the accepted posture for a
/// same-host daemon (#3329). It exists for the non-loopback `--bind` opt-in,
/// where `serve_with_config` requires a token: there, any host that can route
/// to the port must present it. We deliberately exempt `GET /api/health` so
/// probes from load balancers or healthchecks don't need credentials, and
/// exempt the
/// embedded UI's static assets so a browser can load `index.html` and obtain
/// the token via `/api/config` before issuing authenticated requests.
///
/// Two routes remain exempt, and #5052 settled each deliberately rather than by
/// inheritance:
///   - `/api/health` — a liveness probe, and the Tauri sidecar polls it BEFORE
///     it has any credential to present. Its body is
///     `{status, version, pid, ppid}`; nothing there is conversation content.
///   - `/api/config` — its entire purpose is pre-auth bootstrap: it tells the UI
///     whether to prompt for a token. Its body is the single boolean
///     `auth_required`, which any caller can already infer from whether an
///     `/api/*` request 401s, so putting it behind the middleware would remove
///     no information from an attacker while breaking the browser's only way to
///     learn it needs a token.
///
/// `/api/events` USED to be a third exemption, on the grounds that a browser
/// `EventSource` cannot send an `Authorization` header. That was a
/// cross-origin disclosure of live conversation content, not a telemetry
/// trade-off (#5052): the stream carries `PmThinking.text`, `AgentMessage.text`,
/// and `PmDelegating.task_preview`, and any page in the operator's browser could
/// read it from `127.0.0.1` with no credential at all. It is now authenticated
/// by ticket — see `super::event_tickets`.
/// What: For requests under `/api/*` (other than `/api/health` and
/// `/api/config`), checks `Authorization: Bearer <token>` and returns 401 JSON
/// `{"error":"unauthorized"}` on mismatch. #3761: the comparison is
/// constant-time (`bearer_token_matches`) — this is the higher-value secret of
/// the two the server holds, so it gets the same treatment as the relay token.
/// Test: `auth_middleware_rejects_missing_token`,
/// `auth_middleware_accepts_valid_token`, `auth_middleware_allows_health`,
/// `bearer_token_matches_is_exact`,
/// `event_tickets::mint_route_requires_bearer_token_when_configured`.
pub(super) async fn auth_middleware(
    State(auth): State<AuthState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();

    // Public endpoints — never require auth:
    //   - GET /api/health  (health probes; the sidecar polls it pre-credential)
    //   - GET /api/config  (UI bootstrap: tells client whether auth is needed)
    //   - any non-/api path (UI static assets at "/" and "/*path")
    // #5052: `/api/events` was a third exemption and is not one any more — it
    // streams conversation content and is now gated by a ticket that a browser
    // EventSource CAN carry (see `super::event_tickets`). Both remaining
    // exemptions are justified in this function's doc comment.
    if path == "/api/health" || path == "/api/config" || !path.starts_with("/api/") {
        return next.run(req).await;
    }

    let header_val = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim);

    match header_val {
        // #3761: constant-time compare — see `bearer_token_matches`.
        Some(t) if bearer_token_matches(&auth.token, t) => next.run(req).await,
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
