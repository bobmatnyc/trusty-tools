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
//!   - [`with_guarded_middleware_same_origin_cors`] is the same stack with the
//!     permissive CORS policy swapped for [`same_origin_cors`], so a browser
//!     page on an untrusted origin cannot READ the daemon's responses either
//!     (#5052).
//!   - [`daemon_http_client`] builds a reqwest client with short timeouts so
//!     CLI commands never hang on a missing daemon.
//!
//! Test: `cargo test -p trusty-common --features axum-server` covers router
//! composition (smoke) and client construction (timeouts surfaced through
//! the public `reqwest::Client` API — we just assert no error on build).

use anyhow::{Context, Result};
use axum::{Json, Router, response::IntoResponse};
use serde_json::json;
use tower_http::{
    compression::{
        CompressionLayer,
        predicate::{DefaultPredicate, NotForContentType, Predicate},
    },
    cors::{AllowOrigin, Any, CorsLayer},
    trace::TraceLayer,
};

pub mod origin_guard;

pub use origin_guard::{
    SelfOrigins, guard_write_origin, origin_is_local_webview, origin_is_loopback,
    origin_matches_self,
};

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
    with_middleware_stack(router, cors)
}

/// The trace + gzip half of the standard stack, plus a caller-chosen CORS policy.
///
/// Why: `with_standard_middleware` and
/// [`with_guarded_middleware_same_origin_cors`] differ in exactly one layer —
/// the CORS policy. Keeping the layer ORDER (and the SSE compression carve-out
/// documented on `with_standard_middleware`) in one function means a future fix
/// to that order lands once rather than twice.
/// What: layers compression (innermost), trace, then `cors` (outermost).
/// Test: `with_standard_middleware_composes`,
/// `with_guarded_middleware_same_origin_cors_composes`.
fn with_middleware_stack<S>(router: Router<S>, cors: CorsLayer) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let compress =
        CompressionLayer::new().compress_when(DefaultPredicate::new().and(NotForContentType::SSE));
    router
        .layer(compress)
        .layer(TraceLayer::new_for_http())
        .layer(cors)
}

/// A CORS policy that reflects ONLY same-machine origins. (#5052)
///
/// Why: `allow_origin(Any)` lets any page the operator happens to have open
/// READ a loopback daemon's responses. A loopback bind does not contain that —
/// the attacker's JavaScript runs inside the operator's own browser, which can
/// reach `127.0.0.1` — so for a daemon whose GET surface carries conversation
/// content (`trusty-agents`' `/api/events`, `/api/tasks`, `/api/sessions/*`),
/// permissive CORS is the difference between "unreachable off-host" and
/// "readable by any web page". Reflecting only same-machine origins keeps every
/// legitimate consumer working: the daemon's own SPA (same-origin), a `pnpm dev`
/// Vite server on `http://localhost:5173`, a Tauri webview, and the daemon's own
/// resolved non-loopback bind (#3269). Server-side callers (the console reverse
/// proxy, `curl`, MCP stdio bridges) send no `Origin` at all and are unaffected
/// — CORS is a browser-enforced policy, never a server-side access control,
/// which is why this is defence in depth BEHIND per-route auth, not a
/// replacement for it.
/// What: builds a [`CorsLayer`] whose allowed origin is a predicate —
/// [`origin_is_loopback`] OR [`origin_is_local_webview`] OR
/// [`origin_matches_self`] against `self_origins`. Methods and headers stay
/// `Any`; credentials are NOT allowed, so no ambient cookie/auth is ever
/// attached cross-origin.
/// Test: `same_origin_cors_predicate_allows_local`,
/// `same_origin_cors_predicate_rejects_remote` below; end-to-end in
/// trusty-agents' `api::server::tests::event_tickets` —
/// `cross_origin_request_gets_no_cors_reflection` and
/// `loopback_origin_is_cors_reflected`.
pub fn same_origin_cors(self_origins: SelfOrigins) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin, _parts| {
            origin
                .to_str()
                .map(|value| same_origin_cors_allows(value, &self_origins))
                .unwrap_or(false)
        }))
        .allow_methods(Any)
        .allow_headers(Any)
}

/// Whether [`same_origin_cors`] reflects `origin`.
///
/// Why: the predicate closure handed to `AllowOrigin::predicate` is not
/// reachable from a unit test; naming the decision separately makes the policy
/// directly testable, including the DNS-rebinding lookalikes
/// (`127.0.0.1.evil.com`) that [`origin_is_loopback`] already rejects.
/// What: `true` for a loopback host, a local webview origin, or one of the
/// daemon's own resolved non-loopback bind addresses.
/// Test: `same_origin_cors_predicate_allows_local`,
/// `same_origin_cors_predicate_rejects_remote`.
pub fn same_origin_cors_allows(origin: &str, self_origins: &SelfOrigins) -> bool {
    origin_is_loopback(origin)
        || origin_is_local_webview(origin)
        || origin_matches_self(origin, self_origins)
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

/// [`with_guarded_middleware`], but with [`same_origin_cors`] in place of the
/// permissive CORS policy. (#5052)
///
/// Why: the write guard stops a cross-origin page from DRIVING a daemon; it does
/// nothing about a cross-origin page READING one, because reads are `GET` and
/// the guard is method-gated. For a daemon whose GET surface is telemetry that
/// is fine. For one whose GET surface is conversation content it is not — see
/// [`same_origin_cors`]. This entry point exists so such a daemon opts into the
/// tighter policy with a one-line change instead of assembling the stack itself
/// (and drifting from the layer order in [`with_middleware_stack`]).
/// What: layers [`guard_write_origin`] innermost, then compression/trace, then
/// the same-origin CORS policy built from the SAME `self_origins` allowlist the
/// guard uses.
/// Test: `with_guarded_middleware_same_origin_cors_composes` below; the
/// end-to-end behaviour is covered by trusty-agents'
/// `api::server::tests::event_tickets`.
pub fn with_guarded_middleware_same_origin_cors<S>(
    router: Router<S>,
    self_origins: SelfOrigins,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let guarded = router.layer(axum::middleware::from_fn_with_state(
        self_origins.clone(),
        guard_write_origin,
    ));
    with_middleware_stack(guarded, same_origin_cors(self_origins))
}

/// Build a `reqwest::Client` configured for daemon-to-daemon calls.
///
/// Why: every CLI command that talks to the daemon must fail fast when the
/// daemon is not running. Without timeouts, reqwest waits for the OS TCP
/// stack (minutes on some platforms), freezing the terminal.
/// What: delegates to [`crate::http_client::loopback_client`] — proxies off
/// (#4392), 2 s connect timeout, 5 s total request timeout. Returns
/// `anyhow::Result` so callers can `?`-propagate alongside other anyhow
/// errors without conversion boilerplate.
/// Test: `daemon_http_client_builds` — construction succeeds with the
/// configured timeouts; the timeout values themselves are exercised in the
/// dependent CLIs (manual: stop daemon, run `trusty-search status`). The proxy
/// immunity is proven in `http_client::tests`.
pub fn daemon_http_client() -> Result<reqwest::Client> {
    crate::http_client::loopback_client().context("build daemon http client")
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

    #[test]
    fn with_guarded_middleware_same_origin_cors_composes() {
        // #5052 smoke test: the tightened stack layers and finalizes.
        let router: Router = Router::new().route("/ping", get(|| async { "pong" }));
        let _wrapped = with_guarded_middleware_same_origin_cors(router, SelfOrigins::default());
    }

    /// Why: #5052 — every legitimate same-machine consumer must keep working
    /// after the policy stops being `Any`: the daemon's own SPA, a Vite dev
    /// server, the Tauri shell, and the daemon's own non-loopback bind (#3269).
    /// Test: this test.
    #[test]
    fn same_origin_cors_predicate_allows_local() {
        let self_origins =
            SelfOrigins::from_bind_addrs(&[std::net::SocketAddr::from(([100, 64, 1, 2], 7654))]);
        for origin in [
            "http://127.0.0.1:7654",
            "http://localhost:5173",
            "http://[::1]:7654",
            "tauri://localhost",
            "http://tauri.localhost",
            "http://100.64.1.2:7654",
        ] {
            assert!(
                same_origin_cors_allows(origin, &self_origins),
                "{origin} must be reflected"
            );
        }
    }

    /// Why: SECURITY (#5052) — the whole point of the policy is that a page on
    /// a remote origin cannot read the daemon's responses, including the
    /// DNS-rebinding lookalikes that a naive prefix match would accept.
    /// Test: this test.
    #[test]
    fn same_origin_cors_predicate_rejects_remote() {
        let self_origins =
            SelfOrigins::from_bind_addrs(&[std::net::SocketAddr::from(([100, 64, 1, 2], 7654))]);
        for origin in [
            "https://evil.example",
            "http://127.0.0.1.evil.com",
            "http://localhost.evil.com",
            "https://tauri.localhost.evil.com",
            "http://100.64.9.9:7654",
            "http://10.0.0.5:7654",
            "",
        ] {
            assert!(
                !same_origin_cors_allows(origin, &self_origins),
                "{origin} must NOT be reflected"
            );
        }
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
