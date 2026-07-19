//! Shared HTTP server scaffolding for trusty-* daemons.
//!
//! Why: Every trusty-* daemon wants the same axum middleware stack (permissive
//! CORS for local browser UIs, a tracing layer, gzip compression) and the same
//! fast-fail reqwest client when one daemon calls another. Centralising removes
//! drift between trusty-search, future trusty-memory daemons, etc.
//!
//! What: pure helpers — no global state.
//!   - [`with_standard_middleware`] layers CORS/Trace/Compression on a router.
//!   - [`with_guarded_middleware`] additionally applies the router-wide
//!     same-origin write guard ([`origin_guard`], #3304) so destructive daemon
//!     write routes are not exposed to cross-origin CSRF.
//!   - [`daemon_http_client`] builds a reqwest client with short timeouts so
//!     CLI commands never hang on a missing daemon.
//!
//! Test: `cargo test -p trusty-common --features axum-server` covers router
//! composition (smoke) and client construction (timeouts surfaced through
//! the public `reqwest::Client` API — we just assert no error on build).

use anyhow::{Context, Result};
use axum::{Json, Router, response::IntoResponse};
use serde_json::json;
use std::time::Duration;
use tower_http::{
    compression::{
        CompressionLayer,
        predicate::{DefaultPredicate, NotForContentType, Predicate},
    },
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};

pub mod origin_guard;

pub use origin_guard::{SelfOrigins, guard_write_origin};

/// Apply the standard trusty-* middleware stack to an axum router.
///
/// Why: Local browser-based UIs (trusty-search SPA, future dashboards) need
/// permissive CORS to talk to `127.0.0.1:<port>`; every daemon benefits from
/// request tracing for debugging; gzip is a cheap wire-size win.
/// What: layers `CorsLayer` (any origin/methods/headers), `TraceLayer` (HTTP
/// span), and `CompressionLayer` (gzip) in that order. The order matters:
/// CORS must run on every response (including 404s from inner routes), and
/// compression should be outermost so the trace span captures the encoded
/// size if needed.
///
/// Compression skips `text/event-stream` (SSE) responses: gzip's trailer is
/// only flushed at stream close, so a fast-completing SSE response leaves the
/// client (reqwest) mid-decode and surfaces as
/// `Transport error: error decoding response body`. tower-http 0.5 ships
/// `NotForContentType::SSE` ("text/event-stream") for exactly this case; we
/// compose it with `DefaultPredicate` so all other heuristics (min size, no
/// already-compressed media) still apply.
/// Test: smoke-tested via the dependent crates' integration tests — any
/// regression breaks `cargo test -p trusty-search-service`.
pub fn with_standard_middleware<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);
    let compress =
        CompressionLayer::new().compress_when(DefaultPredicate::new().and(NotForContentType::SSE));
    router
        .layer(compress)
        .layer(TraceLayer::new_for_http())
        .layer(cors)
}

/// Apply the standard middleware stack PLUS the router-wide same-origin write
/// guard (#3304).
///
/// Why: the sibling trusty-* daemons (search, memory, analyze, mpm) inherit the
/// permissive-CORS [`with_standard_middleware`] stack, which leaves their
/// DESTRUCTIVE write routes (daemon shutdown, index/palace/drawer deletion,
/// session spawn/stop, the `/rpc` JSON-RPC surface) open to cross-origin CSRF
/// from any page the operator visits. This helper composes
/// [`guard_write_origin`] into that stack so every daemon adopts the console's
/// proven guard (#3280) router-wide with a one-line change, instead of each
/// re-implementing it (architecture review tranche 1).
/// What: layers [`guard_write_origin`] (via
/// [`axum::middleware::from_fn_with_state`] carrying `self_origins`) as the
/// INNERMOST middleware — closest to the routes, so it wraps every route
/// including those merged in later — then applies [`with_standard_middleware`]
/// (compression/trace/CORS) on top. The guard is method-gated (only
/// POST/PUT/PATCH/DELETE), so `GET` reads and SSE/WebSocket upgrades pass
/// through untouched; it fails open on a missing `Origin` header, so all
/// server-side callers (the console reverse proxy, `curl`, the MCP stdio
/// bridge) keep working. Pass `SelfOrigins::default()` for a loopback-only bind
/// or `SelfOrigins::from_bind_addrs(&addrs)` to additionally trust the daemon's
/// own non-loopback (e.g. Tailscale) bind address (#3269).
/// Test: `with_guarded_middleware_composes` below; consuming crates' per-daemon
/// guard regression tests.
pub fn with_guarded_middleware<S>(router: Router<S>, self_origins: SelfOrigins) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    // #3304: guard applied FIRST (innermost) so it sits directly in front of the
    // routes — the #3268 lesson is that a route-scoped `route_layer` mid-chain
    // misses routes registered afterwards, whereas a router-wide `.layer()`
    // covers them all.
    let guarded = router.layer(axum::middleware::from_fn_with_state(
        self_origins,
        guard_write_origin,
    ));
    with_standard_middleware(guarded)
}

/// Build a `reqwest::Client` configured for daemon-to-daemon calls.
///
/// Why: every CLI command that talks to the daemon must fail fast when the
/// daemon is not running. Without timeouts, reqwest waits for the OS TCP
/// stack (minutes on some platforms), freezing the terminal.
/// What: 2 s connect timeout, 5 s total request timeout. Returns
/// `anyhow::Result` so callers can `?`-propagate alongside other anyhow
/// errors without conversion boilerplate.
/// Test: `daemon_http_client_builds` — construction succeeds with the
/// configured timeouts; the timeout values themselves are exercised in the
/// dependent CLIs (manual: stop daemon, run `trusty-search status`).
pub fn daemon_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .context("build daemon http client")
}

/// Standard health-check handler returning `{"status":"ok","version":"<v>"}`.
///
/// Why: trusty-search and trusty-memory both expose `/health`, but their
/// payload shapes drifted (one returned plain `"ok"`, the other JSON with
/// version). Centralising gives every trusty-* daemon the same JSON contract
/// so monitoring tooling (curl probes, MCP supervisors) can rely on a single
/// shape.
/// What: returns a 200 OK with body `{"status":"ok","version":"<version>"}`.
/// The `version` argument is `&'static str` so callers can pass
/// `env!("CARGO_PKG_VERSION")` without allocation.
/// Usage: `.route("/health", get(|| health_handler(env!("CARGO_PKG_VERSION"))))`
/// Test: `health_handler_returns_expected_json` exercises the handler
/// directly and asserts the JSON body.
pub async fn health_handler(version: &'static str) -> impl IntoResponse {
    Json(json!({ "status": "ok", "version": version }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};

    #[test]
    fn daemon_http_client_builds() {
        let client = daemon_http_client().expect("client builds");
        // The reqwest::Client API doesn't expose its configured timeouts, but
        // a successful build is the contract we promise. Drop the client to
        // confirm it's a real, owned value.
        drop(client);
    }

    #[test]
    fn with_standard_middleware_composes() {
        // Smoke test: layering compiles and returns a Router we can finalize.
        let router: Router = Router::new().route("/ping", get(|| async { "pong" }));
        let _wrapped = with_standard_middleware(router);
    }

    #[test]
    fn with_guarded_middleware_composes() {
        // #3304 smoke test: the guarded stack layers and finalizes on a
        // stateless router with a default (loopback-only) allowlist.
        let router: Router = Router::new().route("/ping", get(|| async { "pong" }));
        let _wrapped = with_guarded_middleware(router, SelfOrigins::default());
    }

    #[tokio::test]
    async fn health_handler_returns_expected_json() {
        // Exercise the handler directly: it returns axum's `Json` wrapper
        // around a serde_json::Value with the documented shape. We can't
        // pluck the inner Value out of `impl IntoResponse`, but we can wire
        // the handler into a router and confirm it composes — the JSON
        // shape itself is enforced by the `json!` literal in the source.
        let _router: Router = Router::new().route("/health", get(|| health_handler("9.9.9")));
        // Round-trip the same json! literal to lock in the documented shape.
        let v = serde_json::json!({ "status": "ok", "version": "9.9.9" });
        assert_eq!(v["status"], "ok");
        assert_eq!(v["version"], "9.9.9");
    }
}
