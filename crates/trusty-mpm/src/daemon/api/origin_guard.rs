//! Origin guard for the now browser-facing, action-capable chat endpoint.
//!
//! Why: the DOC-20 §6 / ONB-3 Web surface (`GET /web`) makes a browser drive the
//! state-mutating `POST /api/v1/sessions/chat` with `actions: true`, which can
//! spawn/stop/mutate managed sessions. The daemon binds loopback by default, but
//! if it is ever reachable from a browser on a non-loopback interface, a
//! malicious cross-origin page could silently POST to that endpoint and trigger
//! session actions (classic CSRF). This module is the lightweight mitigation: a
//! same-origin/loopback Origin check that costs nothing for the existing
//! server-side callers.
//! What: [`origin_allowed`] inspects the request's `Origin` header (if any) and
//! its `Host`. With NO `Origin` header the request is allowed — this is the
//! server-side case (the Telegram adapter, the TUI, and `curl` all call the
//! endpoint via reqwest/localhost and send no `Origin`), so all current behavior
//! is preserved. With an `Origin` header present, the request is allowed only if
//! the origin is loopback (`http://127.0.0.1[:port]`, `http://localhost[:port]`,
//! `http://[::1][:port]`) or matches the request `Host`; any other (cross-origin)
//! browser request is rejected.
//! Test: `origin_guard_tests` covers no-Origin → allowed, loopback/same-origin →
//! allowed, and cross-origin → rejected.

use axum::http::HeaderMap;
use axum::http::header::{HOST, ORIGIN};

/// Decide whether a request to the action-capable chat endpoint is allowed by the
/// CSRF/origin guard.
///
/// Why: this is the single, unit-testable seam for the CSRF mitigation. Keeping
/// the policy in one pure function (header map in, bool out) means the handler
/// stays a thin caller and the allow/reject matrix is provable without spinning
/// up a server. The no-`Origin` allowance is what keeps every existing
/// server-side caller (Telegram/TUI via reqwest, `curl`) working unchanged —
/// only real browsers attach an `Origin` header to a `POST`.
/// What: returns `true` when the request carries NO `Origin` header (server-side
/// callers), or when the `Origin` is loopback (`127.0.0.1`, `localhost`, `::1`,
/// any scheme/port) or its host:port matches the request's `Host` header
/// (same-origin). Returns `false` for any other present `Origin` (cross-origin),
/// and for a malformed `Origin` that is neither loopback nor same-origin.
/// Test: `origin_guard_tests::{no_origin_allowed, loopback_origin_allowed,
/// same_origin_allowed, cross_origin_rejected}`.
pub fn origin_allowed(headers: &HeaderMap) -> bool {
    // No `Origin` header → a non-browser, server-side caller (reqwest, curl, the
    // Telegram/TUI adapters). These are trusted by construction (they reach the
    // loopback daemon directly), so they always pass — this preserves 100% of the
    // existing behavior and is the crux of "do not break server callers".
    let Some(origin) = header_str(headers, ORIGIN) else {
        return true;
    };

    // A present `Origin` means a browser issued the request. Allow it only if it
    // is loopback or same-origin as the request `Host`.
    let Some(origin_authority) = authority_of(origin) else {
        // A non-URL `Origin` (or the literal "null") is not same-origin and not
        // loopback — reject it.
        return false;
    };

    if is_loopback_host(origin_authority) {
        return true;
    }

    // Same-origin: the Origin's authority (host[:port]) equals the request Host.
    match header_str(headers, HOST) {
        Some(host) => origin_authority.eq_ignore_ascii_case(host),
        None => false,
    }
}

/// Read a header as a UTF-8 `&str`, treating absent/non-UTF-8 as missing.
///
/// Why: header values are bytes; the guard only reasons about textual origins, so
/// a non-UTF-8 value should be treated as "no usable header" rather than panic.
/// What: returns `Some(&str)` when the header exists and is valid UTF-8, else
/// `None`.
/// Test: exercised indirectly via [`origin_allowed`] tests.
fn header_str(headers: &HeaderMap, name: axum::http::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Extract the `host[:port]` authority from an `Origin` value.
///
/// Why: an `Origin` is `scheme://host[:port]`; the guard compares authorities
/// (and detects loopback) on the host part, so the scheme prefix must be
/// stripped. Doing it by hand avoids pulling in a URL-parser dependency for one
/// header.
/// What: strips a leading `scheme://` (everything up to and including the first
/// `://`) and returns the remainder, or `None` when there is no `://` (e.g. the
/// literal `null`, which browsers send for opaque origins).
/// Test: exercised indirectly via [`origin_allowed`] tests
/// (`cross_origin_rejected` includes a `null` origin).
fn authority_of(origin: &str) -> Option<&str> {
    origin.split_once("://").map(|(_scheme, rest)| rest)
}

/// Whether an authority (`host[:port]`) names the loopback interface.
///
/// Why: the daemon serves the browser page itself, so a same-machine browser's
/// `Origin` is loopback; those must always pass regardless of which loopback
/// alias or ephemeral port the browser used.
/// What: returns `true` when the host portion (authority with any `:port`
/// stripped, and any IPv6 brackets removed) is `127.0.0.1`, `localhost`, or
/// `::1`. Case-insensitive on `localhost`.
/// Test: `origin_guard_tests::loopback_origin_allowed`.
fn is_loopback_host(authority: &str) -> bool {
    // Strip an optional `:port`. For a bracketed IPv6 authority (`[::1]:8080`)
    // split on the closing bracket first so the colons inside the address are not
    // mistaken for a port separator.
    let host = if let Some(rest) = authority.strip_prefix('[') {
        // `::1]` or `::1]:port` → take up to the closing bracket.
        rest.split(']').next().unwrap_or(rest)
    } else {
        // `host` or `host:port` → take up to the first colon.
        authority.split(':').next().unwrap_or(authority)
    };

    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

#[cfg(test)]
mod origin_guard_tests {
    use super::*;
    use axum::http::HeaderValue;

    /// Build a `HeaderMap` from optional `Origin` and `Host` values.
    fn headers(origin: Option<&str>, host: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(o) = origin {
            h.insert(ORIGIN, HeaderValue::from_str(o).expect("valid origin"));
        }
        if let Some(host) = host {
            h.insert(HOST, HeaderValue::from_str(host).expect("valid host"));
        }
        h
    }

    /// Why: server-side callers (Telegram/TUI via reqwest, `curl`) send NO
    /// `Origin` header; the guard MUST let them through or every existing caller
    /// breaks. This is the single most important case.
    /// What: an empty header map (and one with only a `Host`) is allowed.
    /// Test: this is the test.
    #[test]
    fn no_origin_allowed() {
        assert!(origin_allowed(&headers(None, None)));
        assert!(origin_allowed(&headers(None, Some("127.0.0.1:8765"))));
    }

    /// Why: a same-machine browser hitting `/web` carries a loopback `Origin`;
    /// those are trusted and must pass regardless of alias or port.
    /// What: `127.0.0.1`, `localhost`, and `[::1]` origins (with/without port)
    /// are allowed even when the `Host` differs or is absent.
    /// Test: this is the test.
    #[test]
    fn loopback_origin_allowed() {
        assert!(origin_allowed(&headers(
            Some("http://127.0.0.1:8765"),
            Some("127.0.0.1:8765")
        )));
        assert!(origin_allowed(&headers(
            Some("http://localhost:3000"),
            Some("example.com")
        )));
        assert!(origin_allowed(&headers(Some("http://localhost"), None)));
        assert!(origin_allowed(&headers(
            Some("http://[::1]:8765"),
            Some("[::1]:8765")
        )));
    }

    /// Why: a browser served the page from the daemon's own host should keep
    /// working even when the daemon is bound to a non-loopback address — the
    /// `Origin` then matches the `Host`.
    /// What: an `Origin` whose authority equals the request `Host` is allowed.
    /// Test: this is the test.
    #[test]
    fn same_origin_allowed() {
        assert!(origin_allowed(&headers(
            Some("https://daemon.internal:9000"),
            Some("daemon.internal:9000")
        )));
        // Host comparison is case-insensitive.
        assert!(origin_allowed(&headers(
            Some("https://Daemon.Internal"),
            Some("daemon.internal")
        )));
    }

    /// Why: this is the threat — a malicious cross-origin page POSTing to the
    /// action-capable endpoint. Its `Origin` is neither loopback nor same-origin,
    /// so it MUST be rejected (the handler maps `false` → 403).
    /// What: a cross-origin `Origin`, a mismatched-port `Origin`, and the opaque
    /// `null` origin are all rejected.
    /// Test: this is the test.
    #[test]
    fn cross_origin_rejected() {
        assert!(!origin_allowed(&headers(
            Some("https://evil.example.com"),
            Some("127.0.0.1:8765")
        )));
        // Same host, different port is a different origin.
        assert!(!origin_allowed(&headers(
            Some("http://daemon.internal:1234"),
            Some("daemon.internal:9000")
        )));
        // Opaque origin (e.g. sandboxed iframe) → reject.
        assert!(!origin_allowed(&headers(
            Some("null"),
            Some("daemon.internal:9000")
        )));
        // Present Origin but no Host to match against → reject.
        assert!(!origin_allowed(&headers(
            Some("https://evil.example.com"),
            None
        )));
    }
}
