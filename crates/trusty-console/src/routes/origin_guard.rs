//! Same-origin guard for destructive console write routes (#1222 review #3,
//! router-wide since #3268, bind-aware since #3269).
//!
//! Why: the console serves its HTTP API behind `CorsLayer::permissive()` (CORS is
//! intentionally open so the SPA, tailnet clients, and tooling can read state).
//! That is fine for the GET/read surface, but the state-changing write routes —
//! including the session write routes (`POST /sessions`, `…/{id}/stop`,
//! `…/{id}/resume`, `DELETE /{id}`, `…/supervisor/auto-resume`) AND the
//! reverse-proxied upstream daemon routes (`/api/{service}/{*path}`,
//! `/proxy/{daemon}/{*path}`) — are DESTRUCTIVE. A permissive CORS policy plus
//! no auth means any web page the operator visits could fire a cross-origin
//! `fetch` (a classic CSRF vector) and spawn/stop/decommission sessions, or
//! reach destructive daemon endpoints through the proxy (index deletion, daemon
//! shutdown, etc — #3268). The console is a loopback/tailnet operator tool, so
//! the proportionate, minimal defence (not full auth) is a same-origin check: a
//! browser always sends an `Origin` header on cross-origin state-changing
//! requests, so we reject a write whose `Origin` is present and is NOT one of
//! the trusted self-origins. Requests with no `Origin` (curl, the console's own
//! server-side calls, native MCP clients) are allowed — they are not the CSRF
//! threat model.
//!
//! The guard is applied **router-wide** via `Router::layer` in
//! `server::build_router` (not `route_layer`, which only covers routes
//! registered before it in the same chain — the root cause of #3268: the
//! reverse-proxy routes are declared after the `route_layer` call and were
//! never guarded). A single router-wide `.layer()` covers every route,
//! including routes added later, so the proxy surface is protected too.
//!
//! Trusted self-origins are not limited to loopback: when the console binds on
//! a non-loopback address (e.g. Tailscale CGNAT `100.64.0.0/10`, #3269), the
//! server's own actually-resolved bind addresses are passed in as an
//! additional allowlist (see [`SelfOrigins`]) so the console's own write UI,
//! served from that address, is not 403'd. This does NOT open the guard to
//! arbitrary non-loopback origins — only the exact addresses the server itself
//! is listening on.
//!
//! What: [`guard_write_origin`] is an axum middleware constructor: it captures
//! a [`SelfOrigins`] allowlist and returns a middleware function that inspects
//! the `Origin` header; absent → pass through; present and matching loopback OR
//! a trusted self-origin → pass through; otherwise → `403`.
//! [`origin_is_loopback`] classifies loopback hosts; [`origin_matches_self`]
//! additionally checks the bind-derived allowlist.
//! Test: `origin_is_loopback_*` / `origin_matches_self_*` unit tests below
//! classify hosts; the middleware itself is exercised end-to-end by
//! `server/tests.rs`'s `write_route_rejects_cross_origin` /
//! `…_allows_loopback_origin` / `…_allows_missing_origin` /
//! `proxy_route_rejects_cross_origin_write` /
//! `proxy_route_allows_self_origin_write` integration tests.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header::ORIGIN};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// The set of non-loopback addresses the console itself is bound to.
///
/// Why: #3269 — the origin guard must trust the console's own served origin
/// even when it is not loopback (Tailscale bind mode), without opening the
/// door to arbitrary remote origins. Wrapping the bind-derived addresses in a
/// dedicated `Arc`-backed type keeps the allowlist cheap to clone into the
/// middleware closure and keeps its provenance explicit (always derived from
/// `bind::resolve_bind_addrs`, never user input).
/// What: An `Arc<HashSet<SocketAddr>>` newtype with `Default` (empty — the
/// loopback-only behaviour used by every existing test and by `Local` bind
/// mode) and a `From<&[SocketAddr]>` / `FromIterator` constructor.
/// Test: `origin_matches_self_*` below; `guard_write_origin` integration tests
/// in `server/tests.rs`.
#[derive(Debug, Clone, Default)]
pub struct SelfOrigins(Arc<HashSet<SocketAddr>>);

impl FromIterator<SocketAddr> for SelfOrigins {
    fn from_iter<T: IntoIterator<Item = SocketAddr>>(iter: T) -> Self {
        Self(Arc::new(iter.into_iter().collect()))
    }
}

impl SelfOrigins {
    /// Build a `SelfOrigins` allowlist from the server's resolved bind
    /// addresses, keeping only non-loopback entries (loopback is always
    /// trusted by [`origin_is_loopback`], so it need not be duplicated here).
    ///
    /// Why: `bind::resolve_bind_addrs` returns every address the server binds
    /// (loopback plus, in Tailscale mode, the tailnet address); the guard only
    /// needs the non-loopback ones as extra trust anchors.
    /// What: Filters `addrs` to non-loopback `SocketAddr`s and collects them.
    /// Test: `origin_matches_self_trusts_bind_derived_tailscale_addr` below.
    pub fn from_bind_addrs(addrs: &[SocketAddr]) -> Self {
        addrs
            .iter()
            .copied()
            .filter(|a| !a.ip().is_loopback())
            .collect()
    }
}

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
    match parse_origin_authority(origin) {
        Some((host, _port)) => host == "localhost" || host == "::1" || host.starts_with("127."),
        None => false,
    }
}

/// Check whether an `Origin` header value matches one of the console's own
/// bind-derived, non-loopback addresses (#3269).
///
/// Why: in Tailscale bind mode the console's legitimate write UI is served
/// from a non-loopback address, so loopback-only matching (above) 403s the
/// operator's own traffic. This function extends trust to exactly the
/// addresses the server resolved for its own listeners — nothing broader.
/// What: parses the `Origin` host/port, parses the host as an `IpAddr`
/// (hostnames never match — the allowlist is IP-derived from
/// `resolve_bind_addrs`), and returns `true` if `(ip, port)` is a member of
/// `self_origins`.
/// Test: `origin_matches_self_*` below.
pub fn origin_matches_self(origin: &str, self_origins: &SelfOrigins) -> bool {
    let Some((host, port)) = parse_origin_authority(origin) else {
        return false;
    };
    let Ok(ip) = IpAddr::from_str(host) else {
        return false;
    };
    let Some(port) = port.and_then(|p| p.parse::<u16>().ok()) else {
        return false;
    };
    self_origins.0.contains(&SocketAddr::new(ip, port))
}

/// Extract `(host, port)` from a scheme-qualified `Origin` header value.
///
/// Why: shared parsing core for [`origin_is_loopback`] and
/// [`origin_matches_self`] — keeps the (fiddly, IPv6-bracket-aware) authority
/// parsing in exactly one place.
/// What: strips the `scheme://` prefix, takes everything up to the first `/`
/// as the authority, and splits it into host and optional port, unwrapping
/// `[…]` IPv6 literals. Returns `None` for a value with no `://`.
/// Test: exercised indirectly via `origin_is_loopback_*` and
/// `origin_matches_self_*`.
fn parse_origin_authority(origin: &str) -> Option<(&str, Option<&str>)> {
    let after_scheme = origin.split_once("://").map(|(_, rest)| rest)?;
    let authority = after_scheme.split('/').next().unwrap_or("");

    if let Some(rest) = authority.strip_prefix('[') {
        // Bracketed IPv6 literal: `[::1]:7788` → host `::1`, port `7788`.
        let (host, remainder) = rest.split_once(']')?;
        let port = remainder.strip_prefix(':');
        Some((host, port))
    } else {
        // host[:port] — split at the FIRST `:` (`split_once`). For a
        // well-formed, non-bracketed authority (IPv4 dotted-quad or hostname
        // plus an optional `:port`) there is at most one `:`, so first and
        // last coincide; a bare host has no `:` at all. A raw (unbracketed)
        // IPv6 literal — which should never appear in a well-formed
        // `Origin` — would produce a garbage `host` here, but that only
        // degrades to a safe rejection in `origin_is_loopback` /
        // `origin_matches_self`, never a false accept.
        match authority.split_once(':') {
            Some((h, p)) => Some((h, Some(p))),
            None => Some((authority, None)),
        }
    }
}

/// axum middleware that rejects cross-origin requests to destructive routes,
/// trusting loopback plus the bind-derived self-origins passed in as state.
///
/// Why: applied router-wide (#3268 — see module docs) so a malicious page
/// cannot use the operator's authenticated-by-locality console to mutate the
/// session fleet OR reach destructive daemon endpoints through the reverse
/// proxy (CSRF). Read routes are unaffected — they leak no destructive
/// capability. Taking `self_origins` via `State` (bound with
/// `axum::middleware::from_fn_with_state` in `server::build_router`) lets
/// Tailscale-bound deployments trust their own non-loopback origin without
/// opening the guard to arbitrary remote origins (#3269).
/// What: Only acts on state-changing methods (`POST`/`PUT`/`PATCH`/`DELETE`)
/// so it can be layered on a router that also serves safe `GET`/`HEAD` reads
/// without blocking them. For a guarded method, if the request carries an
/// `Origin` header that is present, valid UTF-8, and neither loopback nor a
/// member of `self_origins`, responds `403 FORBIDDEN` with a short JSON body
/// and does not call the inner handler. Absent / unreadable Origin on a
/// trusted host, and all safe methods, pass through to `next`.
/// Test: `server/tests.rs` integration tests (`write_route_rejects_cross_origin`,
/// `write_route_allows_loopback_origin`, `write_route_allows_missing_origin`,
/// `proxy_route_rejects_cross_origin_write`,
/// `proxy_route_allows_self_origin_write`).
pub async fn guard_write_origin(
    State(self_origins): State<SelfOrigins>,
    req: Request,
    next: Next,
) -> Response {
    // Safe (non-state-changing) methods are never CSRF-relevant — pass through
    // so this middleware can sit on a mixed read/write router.
    if !req.method().is_safe()
        && let Some(origin) = req.headers().get(ORIGIN)
    {
        match origin.to_str() {
            Ok(value)
                if !origin_is_loopback(value) && !origin_matches_self(value, &self_origins) =>
            {
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
            // Loopback or trusted self-origin → allowed.
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

    /// Why: #3269 regression guard — a Tailscale bind's own resolved address
    /// must be trusted as a self-origin so the write UI served from it works.
    /// Test: this test.
    #[test]
    fn origin_matches_self_trusts_bind_derived_tailscale_addr() {
        let addrs = [
            SocketAddr::from(([127, 0, 0, 1], 7788)),
            SocketAddr::from(([100, 64, 1, 2], 7788)),
        ];
        let self_origins = SelfOrigins::from_bind_addrs(&addrs);
        assert!(origin_matches_self("http://100.64.1.2:7788", &self_origins));
    }

    /// Why: the allowlist must not blanket-trust the whole 100.64.0.0/10 CGNAT
    /// range — only the exact address(es) the server itself resolved.
    /// Test: this test.
    #[test]
    fn origin_matches_self_rejects_other_hosts_in_cgnat_range() {
        let addrs = [SocketAddr::from(([100, 64, 1, 2], 7788))];
        let self_origins = SelfOrigins::from_bind_addrs(&addrs);
        assert!(!origin_matches_self(
            "http://100.64.9.9:7788",
            &self_origins
        ));
    }

    /// Why: a matching host on the wrong port is not the server's own origin.
    /// Test: this test.
    #[test]
    fn origin_matches_self_rejects_wrong_port() {
        let addrs = [SocketAddr::from(([100, 64, 1, 2], 7788))];
        let self_origins = SelfOrigins::from_bind_addrs(&addrs);
        assert!(!origin_matches_self(
            "http://100.64.1.2:9999",
            &self_origins
        ));
    }

    /// Why: an empty allowlist (the `Default` used by `Local` bind mode and
    /// every existing test) must never match any non-loopback origin.
    /// Test: this test.
    #[test]
    fn origin_matches_self_empty_allowlist_matches_nothing() {
        let self_origins = SelfOrigins::default();
        assert!(!origin_matches_self(
            "http://100.64.1.2:7788",
            &self_origins
        ));
        assert!(!origin_matches_self("http://127.0.0.1:7788", &self_origins));
    }

    /// Why: `from_bind_addrs` must drop loopback entries — they are already
    /// covered by `origin_is_loopback` and should not be duplicated.
    /// Test: this test.
    #[test]
    fn self_origins_from_bind_addrs_drops_loopback() {
        let addrs = [
            SocketAddr::from(([127, 0, 0, 1], 7788)),
            SocketAddr::from(([100, 64, 1, 2], 7788)),
        ];
        let self_origins = SelfOrigins::from_bind_addrs(&addrs);
        assert_eq!(self_origins.0.len(), 1);
    }
}
