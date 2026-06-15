//! Same-origin guard for destructive console write routes (#1222 review #3).
//!
//! Why: the console serves its HTTP API behind `CorsLayer::permissive()` (CORS is
//! intentionally open so the SPA, tailnet clients, and tooling can read state).
//! That is fine for the GET/read surface, but the session write routes
//! (`POST /sessions`, `…/{id}/stop`, `…/{id}/resume`, `DELETE /{id}`,
//! `…/supervisor/auto-resume`) are DESTRUCTIVE — a permissive CORS policy plus
//! no auth means any web page the operator visits could fire a cross-origin
//! `fetch` (a classic CSRF vector) and spawn/stop/decommission sessions. The
//! console is a loopback/tailnet operator tool, so the proportionate, minimal
//! defence (not full auth) is a same-origin check: a browser always sends an
//! `Origin` header on cross-origin state-changing requests, so we reject a write
//! whose `Origin` is present and is NOT a loopback host. Requests with no
//! `Origin` (curl, the console's own server-side calls, native MCP clients) are
//! allowed — they are not the CSRF threat model.
//! What: [`guard_write_origin`] is an axum middleware: it inspects the `Origin`
//! header; absent → pass through; present and loopback (`localhost`,
//! `127.0.0.0/8`, `[::1]`) → pass through; present and non-loopback → `403`.
//! [`origin_is_loopback`] is the pure host-classification helper it delegates to.
//! Test: `origin_is_loopback_*` unit tests below classify hosts; the middleware
//! itself is exercised end-to-end by `server.rs`'s
//! `write_route_rejects_cross_origin` / `…_allows_loopback_origin` /
//! `…_allows_missing_origin` integration tests.

use axum::extract::Request;
use axum::http::{StatusCode, header::ORIGIN};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Classify whether an `Origin` header value names a loopback host.
///
/// Why: the same-origin guard must permit the legitimate operator surface (the
/// console SPA served from `http://127.0.0.1:7788` or `http://localhost:…`) while
/// rejecting genuinely cross-origin browser requests. Centralising the host check
/// keeps the policy in one tested place.
/// What: parses the scheme-qualified `Origin` (e.g. `http://127.0.0.1:7788`),
/// extracts the host (dropping scheme and `:port`, unwrapping `[…]` IPv6
/// brackets), and returns `true` for `localhost`, any `127.x.x.x` IPv4, or the
/// `::1` IPv6 loopback. Anything else (including a missing host) is `false`.
/// Test: `origin_is_loopback_*` below.
pub fn origin_is_loopback(origin: &str) -> bool {
    // Strip the scheme (`http://` / `https://`); if there is no `://` the value
    // is malformed for an Origin and we treat it as non-loopback (reject).
    let after_scheme = match origin.split_once("://") {
        Some((_scheme, rest)) => rest,
        None => return false,
    };

    // The authority ends at the first `/` (there should be none in an Origin,
    // but be defensive), then we split host from port.
    let authority = after_scheme.split('/').next().unwrap_or("");

    let host = if let Some(rest) = authority.strip_prefix('[') {
        // Bracketed IPv6 literal: `[::1]:7788` → `::1`.
        match rest.split_once(']') {
            Some((h, _port)) => h,
            None => return false,
        }
    } else {
        // host[:port] — drop the port if present.
        authority.split(':').next().unwrap_or("")
    };

    host == "localhost" || host == "::1" || host.starts_with("127.")
}

/// axum middleware that rejects cross-origin requests to destructive routes.
///
/// Why: applied only to the session write routes so a malicious page cannot use
/// the operator's authenticated-by-locality console to mutate the session fleet
/// (CSRF). Read routes are unaffected — they leak no destructive capability.
/// What: only acts on state-changing methods (`POST`/`PUT`/`PATCH`/`DELETE`) so
/// it can be layered on a router that also serves safe `GET`/`HEAD` reads without
/// blocking them. For a guarded method, if the request carries an `Origin` header
/// that is present, valid UTF-8, and NOT loopback, responds `403 FORBIDDEN` with a
/// short JSON body and does not call the inner handler. Absent / unreadable /
/// loopback `Origin`, and all safe methods, pass through to `next`.
/// Test: `server.rs` integration tests (`write_route_rejects_cross_origin`,
/// `write_route_allows_loopback_origin`, `write_route_allows_missing_origin`,
/// `read_route_allows_cross_origin`).
pub async fn guard_write_origin(req: Request, next: Next) -> Response {
    // Safe (non-state-changing) methods are never CSRF-relevant — pass through so
    // this middleware can sit on a mixed read/write router.
    if !req.method().is_safe()
        && let Some(origin) = req.headers().get(ORIGIN)
    {
        match origin.to_str() {
            Ok(value) if !origin_is_loopback(value) => {
                tracing::warn!(
                    origin = %value,
                    "console write route rejected cross-origin request (same-origin guard)"
                );
                return (
                    StatusCode::FORBIDDEN,
                    axum::Json(serde_json::json!({
                        "error": "cross-origin write requests are not allowed",
                    })),
                )
                    .into_response();
            }
            // Loopback origin → allowed.
            Ok(_) => {}
            // Non-UTF-8 Origin is malformed; reject to be safe.
            Err(_) => {
                tracing::warn!("console write route rejected request with non-UTF-8 Origin header");
                return (
                    StatusCode::FORBIDDEN,
                    axum::Json(serde_json::json!({
                        "error": "malformed Origin header",
                    })),
                )
                    .into_response();
            }
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the SPA's own origins (localhost / 127.x / ::1, with or without a
    /// port) must be classified as loopback so the guard never blocks the
    /// legitimate operator surface.
    /// Test: this test.
    #[test]
    fn origin_is_loopback_accepts_local_hosts() {
        assert!(origin_is_loopback("http://127.0.0.1:7788"));
        assert!(origin_is_loopback("http://127.0.0.1"));
        assert!(origin_is_loopback("http://localhost:7788"));
        assert!(origin_is_loopback("http://localhost"));
        assert!(origin_is_loopback("http://127.5.6.7:9000"));
        assert!(origin_is_loopback("http://[::1]:7788"));
        assert!(origin_is_loopback("https://localhost:443"));
    }

    /// Why: genuinely remote / cross-origin hosts (the CSRF threat) must be
    /// classified as non-loopback so the guard rejects them.
    /// Test: this test.
    #[test]
    fn origin_is_loopback_rejects_remote_hosts() {
        assert!(!origin_is_loopback("http://evil.example.com"));
        assert!(!origin_is_loopback("https://evil.example.com:8443"));
        assert!(!origin_is_loopback("http://10.0.0.5:7788"));
        assert!(!origin_is_loopback("http://100.64.1.2:7788")); // tailnet CGNAT IP
        assert!(!origin_is_loopback("http://127evil.com")); // not a 127.x host
    }

    /// Why: a value with no scheme is not a well-formed Origin; treat as
    /// non-loopback (reject) rather than silently allowing it.
    /// Test: this test.
    #[test]
    fn origin_is_loopback_rejects_malformed() {
        assert!(!origin_is_loopback("127.0.0.1:7788")); // no scheme
        assert!(!origin_is_loopback(""));
        assert!(!origin_is_loopback("garbage"));
    }
}
