//! Reverse-proxy handlers for `/api/{service}/{*path}` (primary) and the
//! deprecated `/proxy/{daemon}/{*path}` alias (#1849 Phase 2).
//!
//! Why: Provides a single handler that forwards every HTTP method to the live
//! upstream daemon URL resolved from the background health-poll cache, enabling
//! all daemon APIs and UIs to be reached through the console port without knowing
//! per-daemon port numbers.
//! What: `proxy_handler` resolves the service key, normalises the upstream base
//! URL (double-scheme guard), forwards the request (method, allowed headers,
//! body) via `reqwest`, and streams the response back.  Returns 400 for unknown
//! service keys and 503/502 when the daemon cache is cold or unreachable.
//! `deprecated_proxy_handler` wraps `proxy_handler` with a debug deprecation
//! note to nudge callers toward the new `/api/{service}/…` prefix.
//! Test: `tests::test_build_upstream_url_*` and `test_normalize_base_url_*` below.

use axum::{
    body::{Body, Bytes},
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use reqwest::Method;
use tracing::{debug, trace, warn};

use crate::server::AppState;

// Hop-by-hop headers that must not be forwarded in either direction.
// RFC 7230 §6.1 and common proxy practice.
static HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
    // Console-specific: do not forward the host header (reqwest sets its own).
    "host",
];

/// Map a short service key (as it appears in the URL) to the full service ID
/// stored in `CachedSnapshot.services`.
///
/// Why: The URL uses short names (`search`, `memory`, …) while `ServiceInfo.id`
/// uses the full `trusty-*` prefix.  This function is the single source of
/// truth for the proxy allowlist: `None` means the key is not permitted.
/// `mpm` is in the allowlist (#1849 Phase 1) so `/api/mpm/*path` forwards to
/// the live trusty-mpm daemon URL resolved from the connector's `ServiceInfo`.
/// What: Returns the full service ID, or `None` for unknown/disallowed keys.
/// Test: `test_service_key_mapping` below.
fn full_id(service_key: &str) -> Option<&'static str> {
    match service_key {
        "search" => Some("trusty-search"),
        "memory" => Some("trusty-memory"),
        "analyze" => Some("trusty-analyze"),
        "review" => Some("trusty-review"),
        // #1849 Phase 1: mpm added to the proxy allowlist so the console can
        // forward requests to the live trusty-mpm HTTP daemon via its base URL
        // resolved from the standard http_addr discovery file.
        "mpm" => Some("trusty-mpm"),
        _ => None,
    }
}

/// Guard that rejects any upstream URL that is not a local loopback address.
///
/// Why: The console is a strictly local tool.  If a bug or compromise caused a
/// non-loopback URL to enter the poller cache, forwarding to it would turn the
/// console into an SSRF vector.  This guard prevents that by enforcing that the
/// resolved base URL is always a local address before any bytes are sent.
/// What: Returns `true` if `url` starts with `http://127.`, `http://[::1]`, or
/// `http://localhost`; `false` for anything else.
/// Test: `test_is_local_upstream_*` below.
fn is_local_upstream(url: &str) -> bool {
    url.starts_with("http://127.")
        || url.starts_with("http://[::1]")
        || url.starts_with("http://localhost")
}

/// Normalize a service base URL to strip any accidental double-scheme prefix.
///
/// Why: Defense in depth against a misconfigured discovery file that already
/// contains a scheme (`http://127.0.0.1:7788`).  The connector prepends
/// `http://` via `detect_service`, which would produce `http://http://127.0.0.1:7788`.
/// Stripping all leading scheme prefixes and re-adding exactly one `http://`
/// ensures a single well-formed `http://host:port` URL regardless of how many
/// schemes were stacked.  Idempotent on a correctly-formed URL.
/// What: Delegates to `crate::url_util::strip_schemes` (the single shared
/// implementation) so the loop logic cannot drift between this site and the
/// connector layer.  Emits a `warn!` when `https://` is present — that
/// indicates a misconfigured discovery file (all upstream connections are
/// loopback HTTP only).
/// Test: `test_normalize_base_url_*` below.
pub fn normalize_base_url(url: &str) -> String {
    if url.contains("https://") {
        warn!(
            "proxy: upstream base URL contains https:// — stripping to http:// \
             (loopback upstream connections are HTTP only). \
             Check the service's http_addr discovery file."
        );
    }
    format!("http://{}", crate::url_util::strip_schemes(url))
}

/// Build the upstream URL from a base URL, sub-path, and optional query string.
///
/// Why: Centrally-tested URL construction keeps the proxy handler clean.
/// What: Appends `subpath` (with a leading slash) to `base_url`, then appends
/// `?{query}` if the query string is non-empty.
/// Test: `test_build_upstream_url_*` below.
pub fn build_upstream_url(base_url: &str, subpath: &str, query: Option<&str>) -> String {
    let base = base_url.trim_end_matches('/');
    let path = subpath.trim_start_matches('/');
    let url = if path.is_empty() {
        format!("{base}/")
    } else {
        format!("{base}/{path}")
    };
    match query {
        Some(q) if !q.is_empty() => format!("{url}?{q}"),
        _ => url,
    }
}

/// Strip hop-by-hop headers and copy the remainder into a new `HeaderMap`.
///
/// Why: Forwarding hop-by-hop headers to the upstream or back to the client
/// violates HTTP/1.1 proxy semantics and can cause connection reuse failures.
/// What: Iterates `headers`, skips any name in `HOP_BY_HOP`, and copies the
/// rest.
/// Test: Exercised implicitly by proxy round-trip tests.
fn filter_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if !HOP_BY_HOP.contains(&name.as_str()) {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

/// Build a plain-text error response.
///
/// Why: Centralises error body construction so callers are one-liners.
/// What: Returns a `Response` with the given status and a UTF-8 text body.
/// Test: Exercised by error-path coverage.
fn error_response(status: StatusCode, body: &'static str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `ANY /api/{service}/{*path}` — reverse-proxy to the service's live URL.
///
/// Why: Lets operators and the console SPA reach every daemon API through the
/// console port without knowing per-daemon port numbers.
/// What: Resolves the service's base URL from the background health-poll cache,
/// normalises the URL (double-scheme guard), forwards the request (method, safe
/// headers, body) via reqwest, and streams the upstream response back.
/// Unknown service keys → 400; daemon not reachable → 502.
/// Test: URL construction is unit-tested in `tests` below.  End-to-end proxy
/// behaviour requires a live daemon and is not tested in CI.
pub async fn proxy_handler(
    State(state): State<AppState>,
    Path((service_key, subpath)): Path<(String, String)>,
    req: Request,
) -> Response {
    // Defensive guard: "console" is a reserved service key that routes to the
    // console's own /api/console/* namespace.  Reject it explicitly here as a
    // routing-independent second layer so the proxy can never target itself,
    // even if axum's literal-segment priority were somehow bypassed.
    if service_key.as_str() == "console" {
        warn!(
            "proxy: service_key 'console' is reserved and cannot be proxied (routing invariant violated)"
        );
        return error_response(StatusCode::BAD_REQUEST, "reserved service key");
    }

    // Map short key → full id via the exhaustive match in full_id(), which is
    // the single source of truth for the proxy allowlist.
    let Some(full_service_id) = full_id(&service_key) else {
        warn!("proxy: unknown service key '{service_key}'");
        return error_response(StatusCode::BAD_REQUEST, "unknown daemon");
    };

    let base_url = {
        let snap = state.poller_cache().snapshot().await;
        match snap {
            None => {
                warn!("proxy: cache not yet populated for '{service_key}'");
                return error_response(StatusCode::SERVICE_UNAVAILABLE, "cache not ready");
            }
            Some(s) => {
                let map = s.url_map();
                match map.get(full_service_id).cloned() {
                    Some(url) => url,
                    None => {
                        warn!("proxy: service '{service_key}' is not running");
                        return error_response(StatusCode::BAD_GATEWAY, "daemon not running");
                    }
                }
            }
        }
    };

    // Normalize base URL — strips any accidental double-scheme prefix produced
    // by a malformed discovery file (#1849 Phase 2 hardening).
    let base_url = normalize_base_url(&base_url);

    // SSRF guard: the console is a local-only tool; reject any upstream that is
    // not a loopback address.  A non-local URL in the cache would be a bug or
    // compromise — fail closed rather than forward.
    if !is_local_upstream(&base_url) {
        warn!("proxy: upstream '{base_url}' is not a local address — rejecting (SSRF guard)");
        return error_response(StatusCode::BAD_GATEWAY, "upstream not local");
    }

    // Decompose request into parts so we can access headers and body.
    let (parts, body) = req.into_parts();

    // Build the upstream URL.
    let query = parts.uri.query();
    let upstream_url = build_upstream_url(&base_url, &subpath, query);
    debug!("proxy: {service_key} → {upstream_url}");

    // Convert axum Method to reqwest Method.
    let method = match Method::from_bytes(parts.method.as_str().as_bytes()) {
        Ok(m) => m,
        Err(_) => {
            return error_response(StatusCode::BAD_REQUEST, "unsupported method");
        }
    };

    // Filter headers before consuming body.
    let safe_headers = filter_headers(&parts.headers);

    // Collect body bytes (64 MiB cap).
    const BODY_LIMIT: usize = 64 * 1024 * 1024;
    let body_bytes: Bytes = match axum::body::to_bytes(body, BODY_LIMIT).await {
        Ok(b) => b,
        Err(e) => {
            warn!("proxy: failed to read request body: {e}");
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body exceeds proxy limit of 64 MiB",
            );
        }
    };

    // Build the upstream request with safe headers and body.
    let client = state.http_client();
    let upstream_req = client
        .request(method, &upstream_url)
        .headers(safe_headers)
        .body(body_bytes);

    // Execute.
    let upstream_resp = match upstream_req.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("proxy: upstream request failed for '{service_key}': {e}");
            return error_response(StatusCode::BAD_GATEWAY, "upstream request failed");
        }
    };

    // Map the upstream response back.
    let status = StatusCode::from_u16(upstream_resp.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut resp_builder = Response::builder().status(status);

    // Copy allowed upstream response headers.
    for (name, value) in upstream_resp.headers() {
        if !HOP_BY_HOP.contains(&name.as_str())
            && let Ok(n) = HeaderName::from_bytes(name.as_str().as_bytes())
            && let Ok(v) = HeaderValue::from_bytes(value.as_bytes())
        {
            resp_builder = resp_builder.header(n, v);
        }
    }

    // Stream the body.
    let resp_body = match upstream_resp.bytes().await {
        Ok(b) => Body::from(b),
        Err(e) => {
            warn!("proxy: failed to read upstream body: {e}");
            Body::from("upstream body error")
        }
    };

    resp_builder
        .body(resp_body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `ANY /proxy/{daemon}/{*path}` — deprecated alias for `proxy_handler`.
///
/// Why: The `/proxy/` prefix was renamed to `/api/` in #1849 Phase 2 to align
/// with the console's `/api/console/…` namespace.  This alias keeps old callers
/// working without a hard break while nudging them toward the new path.
/// What: Logs a debug-level deprecation note then delegates to `proxy_handler`
/// with the same extracted path components.
/// Test: `test_deprecated_proxy_alias_*` in `server.rs`.
pub async fn deprecated_proxy_handler(
    State(state): State<AppState>,
    Path((service_key, subpath)): Path<(String, String)>,
    req: Request,
) -> Response {
    trace!("proxy: DEPRECATED /proxy/{service_key}/… — use /api/{service_key}/… instead (#1849)");
    proxy_handler(State(state), Path((service_key, subpath)), req).await
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: URL must be built correctly for a subpath with no query string.
    /// What: asserts build_upstream_url("http://127.0.0.1:7878", "health", None)
    /// → "http://127.0.0.1:7878/health".
    /// Test: this test itself.
    #[test]
    fn test_build_upstream_url_simple_path() {
        assert_eq!(
            build_upstream_url("http://127.0.0.1:7878", "health", None),
            "http://127.0.0.1:7878/health"
        );
    }

    /// Why: a query string must be appended after `?`.
    /// What: asserts build_upstream_url with query "top_k=5" → correct URL.
    /// Test: this test itself.
    #[test]
    fn test_build_upstream_url_with_query() {
        assert_eq!(
            build_upstream_url(
                "http://127.0.0.1:7879",
                "indexes/abc/complexity_hotspots",
                Some("top_k=5")
            ),
            "http://127.0.0.1:7879/indexes/abc/complexity_hotspots?top_k=5"
        );
    }

    /// Why: an empty subpath must still produce a valid URL with trailing slash.
    /// What: asserts build_upstream_url with empty subpath.
    /// Test: this test itself.
    #[test]
    fn test_build_upstream_url_empty_path() {
        assert_eq!(
            build_upstream_url("http://127.0.0.1:7070", "", None),
            "http://127.0.0.1:7070/"
        );
    }

    /// Why: base URL with trailing slash must not produce a double slash.
    /// What: passes base URL with trailing slash, asserts no double slash.
    /// Test: this test itself.
    #[test]
    fn test_build_upstream_url_base_trailing_slash() {
        assert_eq!(
            build_upstream_url("http://127.0.0.1:7878/", "health", None),
            "http://127.0.0.1:7878/health"
        );
    }

    /// Why: an empty query string must not append a `?`.
    /// What: passes Some("") as query; asserts no trailing `?`.
    /// Test: this test itself.
    #[test]
    fn test_build_upstream_url_empty_query_omitted() {
        assert_eq!(
            build_upstream_url("http://127.0.0.1:7878", "health", Some("")),
            "http://127.0.0.1:7878/health"
        );
    }

    /// Why: normalize_base_url must be idempotent on a correctly-formed URL.
    /// What: passes `http://127.0.0.1:7878`; asserts it is returned unchanged.
    /// Test: this test itself.
    #[test]
    fn test_normalize_base_url_idempotent_on_correct_url() {
        assert_eq!(
            normalize_base_url("http://127.0.0.1:7878"),
            "http://127.0.0.1:7878"
        );
    }

    /// Why: a double-scheme URL (produced when a discovery file already contains
    /// `http://` and detect_service prepends another) must be collapsed to one
    /// scheme (#1849 Phase 2 double-scheme hardening).
    /// What: passes `http://http://127.0.0.1:7878`; asserts `http://127.0.0.1:7878`.
    /// Test: this test itself.
    #[test]
    fn test_normalize_base_url_collapses_double_http_scheme() {
        assert_eq!(
            normalize_base_url("http://http://127.0.0.1:7878"),
            "http://127.0.0.1:7878"
        );
    }

    /// Why: an https-prefixed discovery URL must also be normalised to http://
    /// (all upstream connections are loopback HTTP only).
    /// What: passes `https://127.0.0.1:7878`; asserts `http://127.0.0.1:7878`.
    /// Test: this test itself.
    #[test]
    fn test_normalize_base_url_replaces_https_with_http() {
        assert_eq!(
            normalize_base_url("https://127.0.0.1:7878"),
            "http://127.0.0.1:7878"
        );
    }

    /// Why: the SSRF guard must accept loopback IPv4, IPv6, and localhost but
    /// reject any other URL including external hosts and non-loopback RFC-1918.
    /// What: calls is_local_upstream with accepted and rejected URLs.
    /// Test: this test itself.
    #[test]
    fn test_is_local_upstream_accepted() {
        assert!(is_local_upstream("http://127.0.0.1:7878"));
        assert!(is_local_upstream("http://127.0.0.1:7878/health"));
        assert!(is_local_upstream("http://127.1.2.3:9000"));
        assert!(is_local_upstream("http://[::1]:8080"));
        assert!(is_local_upstream("http://localhost:7070"));
        assert!(is_local_upstream("http://localhost"));
    }

    /// Why: non-local URLs must be rejected to prevent SSRF.
    /// What: calls is_local_upstream with external and RFC-1918 URLs.
    /// Test: this test itself.
    #[test]
    fn test_is_local_upstream_rejected() {
        assert!(!is_local_upstream("http://192.168.1.1:7878"));
        assert!(!is_local_upstream("http://10.0.0.1:7879"));
        assert!(!is_local_upstream("http://evil.example.com/steal"));
        assert!(!is_local_upstream("https://127.0.0.1:7878")); // https, not http
        assert!(!is_local_upstream("http://0.0.0.0:7878"));
    }

    /// Why: full_id must map all known short service keys to their trusty-* IDs.
    /// What: calls full_id for each known key and the unknown key.
    /// Test: this test itself.
    #[test]
    fn test_service_key_mapping() {
        assert_eq!(full_id("search"), Some("trusty-search"));
        assert_eq!(full_id("memory"), Some("trusty-memory"));
        assert_eq!(full_id("analyze"), Some("trusty-analyze"));
        assert_eq!(full_id("review"), Some("trusty-review"));
        // #1849 Phase 1: mpm must be in the allowlist.
        assert_eq!(full_id("mpm"), Some("trusty-mpm"));
        assert_eq!(full_id("unknown"), None);
    }

    /// Why: a double-scheme URL like `http://http://127.0.0.1:7878` must NOT
    /// pass the SSRF guard — it starts with `http://http://`, not `http://127.`,
    /// `http://[::1]`, or `http://localhost`.  This locks in the ordering
    /// safety: normalize_base_url must run before is_local_upstream so the
    /// guard only ever sees a clean single-scheme URL.
    /// What: asserts is_local_upstream("http://http://127.0.0.1:7878") is false.
    /// Test: this test itself (#1849 Phase 2 double-scheme SSRF regression guard).
    #[test]
    fn test_is_local_upstream_rejects_double_scheme() {
        assert!(
            !is_local_upstream("http://http://127.0.0.1:7878"),
            "double-scheme URL must not pass the loopback guard"
        );
    }

    /// Why: the "console" service key is reserved; full_id returns None for it
    /// so it would be caught by the unknown-key guard — but the explicit
    /// console check must fire first (defensive depth).
    /// What: asserts full_id("console") is None (the allowlist does not list it).
    /// Test: this test itself (unit-level guard; HTTP-level guard tested in
    /// server.rs::test_api_proxy_console_key_returns_400).
    #[test]
    fn test_console_key_not_in_allowlist() {
        assert_eq!(
            full_id("console"),
            None,
            "console must never appear in the proxy allowlist"
        );
    }

    /// Why: hop-by-hop headers must be stripped; safe headers must pass through.
    /// What: builds a HeaderMap with a hop-by-hop ("connection") and a safe
    /// header ("x-custom"), calls filter_headers, asserts only safe one remains.
    /// Test: this test itself.
    #[test]
    fn test_filter_headers_strips_hop_by_hop() {
        let mut h = HeaderMap::new();
        h.insert("connection", HeaderValue::from_static("keep-alive"));
        h.insert("x-custom", HeaderValue::from_static("hello"));
        let filtered = filter_headers(&h);
        assert!(!filtered.contains_key("connection"));
        assert!(filtered.contains_key("x-custom"));
    }
}
