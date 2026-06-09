//! Reverse-proxy handler for `/proxy/{daemon}/{*path}`.
//!
//! Why: Provides a single handler that forwards every HTTP method to the live
//! upstream daemon URL resolved from the background health-poll cache, enabling
//! all daemon APIs and UIs to be reached through the console port.
//! What: `proxy_handler` strips the `/proxy/{daemon}/` prefix, resolves the
//! daemon's base URL from the cached snapshot, forwards the request (method,
//! allowed headers, body) via `reqwest`, and streams the response (status,
//! allowed response headers, body) back to the caller.  Returns 400 for unknown
//! daemon IDs and 502 when the daemon is not reachable.
//! Test: `tests::test_build_upstream_url_*` below exercise URL construction.

use axum::{
    body::{Body, Bytes},
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use reqwest::Method;
use tracing::{debug, warn};

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

/// Map a short daemon key (as it appears in the URL) to the full service ID
/// stored in `CachedSnapshot.services`.
///
/// Why: The URL uses short names (`search`, `memory`, …) while `ServiceInfo.id`
/// uses the full `trusty-*` prefix.  This function is the single source of
/// truth for the proxy allowlist: `None` means the key is not permitted.
/// What: Returns the full service ID, or `None` for unknown/disallowed keys.
/// Test: `test_daemon_key_mapping` below.
fn full_id(daemon_key: &str) -> Option<&'static str> {
    match daemon_key {
        "search" => Some("trusty-search"),
        "memory" => Some("trusty-memory"),
        "analyze" => Some("trusty-analyze"),
        "review" => Some("trusty-review"),
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

/// `ANY /proxy/{daemon}/{*path}` — reverse-proxy to the daemon's live URL.
///
/// Why: Lets operators and the console SPA reach every daemon API through the
/// console port without knowing per-daemon port numbers.
/// What: Resolves the daemon's base URL from the background health-poll cache,
/// forwards the request (method, safe headers, body) via reqwest, and streams
/// the upstream response back.  Unknown daemon IDs → 400; daemon not reachable
/// → 502.
/// Test: URL construction is unit-tested in `tests` below.  End-to-end proxy
/// behaviour requires a live daemon and is not tested in CI.
pub async fn proxy_handler(
    State(state): State<AppState>,
    Path((daemon_key, subpath)): Path<(String, String)>,
    req: Request,
) -> Response {
    // Map short key → full id via the exhaustive match in full_id(), which is
    // the single source of truth for the proxy allowlist.
    let Some(full_daemon_id) = full_id(&daemon_key) else {
        warn!("proxy: unknown daemon key '{daemon_key}'");
        return error_response(StatusCode::BAD_REQUEST, "unknown daemon");
    };

    let base_url = {
        let snap = state.poller_cache().snapshot().await;
        match snap {
            None => {
                warn!("proxy: cache not yet populated for '{daemon_key}'");
                return error_response(StatusCode::SERVICE_UNAVAILABLE, "cache not ready");
            }
            Some(s) => {
                let map = s.url_map();
                match map.get(full_daemon_id).cloned() {
                    Some(url) => url,
                    None => {
                        warn!("proxy: daemon '{daemon_key}' is not running");
                        return error_response(StatusCode::BAD_GATEWAY, "daemon not running");
                    }
                }
            }
        }
    };

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
    debug!("proxy: {daemon_key} → {upstream_url}");

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
            warn!("proxy: upstream request failed for '{daemon_key}': {e}");
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

    /// Why: full_id must map all known short keys to their trusty-* IDs.
    /// What: calls full_id for each known key and the unknown key.
    /// Test: this test itself.
    #[test]
    fn test_daemon_key_mapping() {
        assert_eq!(full_id("search"), Some("trusty-search"));
        assert_eq!(full_id("memory"), Some("trusty-memory"));
        assert_eq!(full_id("analyze"), Some("trusty-analyze"));
        assert_eq!(full_id("review"), Some("trusty-review"));
        assert_eq!(full_id("unknown"), None);
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
