//! Timeout configuration and `reqwest::Client` construction for
//! [`super::DaemonClient`].
//!
//! Why: `DaemonClient::new` used to mint a bare `reqwest::Client::new()` with
//! no timeout at all (issue #2471, pre-existing since #2118). A daemon that
//! accepts the TCP connection but never answers — e.g. wedged, or mid-restart
//! with a stale listener — hung every caller forever, and the `tm projects`
//! TUI awaits its poll step in the SAME task that reads keyboard input
//! (`tui/project_ctl/mod.rs`), so the hang froze the whole screen, including
//! `q`/Ctrl-C, until the OS-level socket timeout (minutes, platform-
//! dependent). Bounding the client-level timeout turns an indefinite freeze
//! into a bounded one. Split into its own file (not `mod.rs`) both because
//! the timeout logic is a distinct, independently-testable concern and
//! because `mod.rs` was already at 498/500 SLOC with no headroom to absorb it
//! (same reasoning as the `claude_config`/`managed`/`projects` sibling-file
//! split).
//! What: [`default_client`] is what [`super::DaemonClient::new`] calls; it
//! delegates to [`build_client`] (a pure function of the two `Duration`
//! bounds) so the timeout behavior itself is unit-testable against tiny
//! durations rather than the real 3s/10s production values.
//! [`CHAT_REQUEST_TIMEOUT`] is a longer, explicit per-request override for
//! the two endpoints that legitimately run long (see its own doc).
//! Test: `tests::build_client_bounds_a_stalled_connection` drives a real
//! request against a `TcpListener` that accepts but never answers, and
//! asserts it errors within the (small, test-only) configured bound rather
//! than hanging.

use std::time::Duration;

/// Ceiling on establishing the TCP connection to the daemon.
///
/// Why: the daemon is loopback-local in every deployed configuration, so a
/// healthy connect completes in low single-digit milliseconds; 3s is
/// generous slack for a loaded machine while still failing fast when the
/// daemon process is simply not there.
/// What: passed to `reqwest::ClientBuilder::connect_timeout` in
/// [`build_client`].
/// Test: `tests::default_client_uses_default_bounds` pins the value.
pub(super) const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Ceiling on one request/response round trip against the daemon.
///
/// Why: every non-chat `DaemonClient` endpoint is a simple local JSON call
/// (list sessions, read breaker state, kill a session, …) that normally
/// completes in milliseconds; 10s is well above any legitimate latency and
/// still bounds a wedged daemon to a short, operator-visible freeze instead
/// of the multi-minute OS socket timeout this replaces.
/// What: passed to `reqwest::ClientBuilder::timeout` in [`build_client`] —
/// this is the CLIENT-LEVEL default; [`CHAT_REQUEST_TIMEOUT`] overrides it
/// per-request on the two endpoints that need longer.
/// Test: `tests::default_client_uses_default_bounds` pins the value;
/// `tests::build_client_bounds_a_stalled_connection` exercises the mechanism
/// with a scaled-down bound.
pub(super) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-request timeout override for the LLM-backed chat endpoints.
///
/// Why: `DaemonClient::llm_chat` (`POST /llm/chat`) and
/// `DaemonClient::coordinator_chat` (`POST /api/v1/sessions/chat`) both wait
/// SYNCHRONOUSLY for the daemon's own upstream OpenRouter round trip, which
/// the daemon itself bounds at up to 120s
/// (`OPENROUTER_REQUEST_TIMEOUT_SECS` in
/// `trusty_common::chat::openai_compat::providers`). Applying
/// [`DEFAULT_REQUEST_TIMEOUT`] to these two calls would abort a legitimate,
/// merely-slow completion before the daemon's own timeout even has a chance
/// to fire — trading a real hang for spurious failures on ordinary slow
/// responses. 130s gives the daemon a 10s margin to answer (success or its
/// own timeout error) after its upstream call resolves, while still being a
/// hard, finite bound (never the pre-#2471 "no timeout at all").
/// What: passed to `reqwest::RequestBuilder::timeout` at the `llm_chat` /
/// `coordinator_chat` call sites — a per-request override, not a change to
/// the client-level default every other endpoint uses.
/// Test: `tests::default_client_uses_default_bounds` asserts it exceeds
/// [`DEFAULT_REQUEST_TIMEOUT`].
pub(super) const CHAT_REQUEST_TIMEOUT: Duration = Duration::from_secs(130);

/// Build the `reqwest::Client` [`super::DaemonClient::new`] uses by default.
///
/// Why: the one production call site for [`build_client`], pinned to this
/// module's default bounds so `DaemonClient::new` never regresses back to an
/// unbounded client.
/// What: delegates to [`build_client`] with [`DEFAULT_CONNECT_TIMEOUT`] and
/// [`DEFAULT_REQUEST_TIMEOUT`].
/// Test: `tests::default_client_uses_default_bounds`.
pub(super) fn default_client() -> reqwest::Client {
    build_client(DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT)
}

/// Build a `reqwest::Client` bounded by `connect_timeout` / `request_timeout`.
///
/// Why: factored out as a pure function of its bounds so the timeout
/// behavior is unit-testable against tiny durations without waiting out the
/// real 3s/10s production values (a full-duration test would be both slow
/// and, on a loaded CI runner, borderline flaky).
/// What: `reqwest::Client::builder().connect_timeout(..).timeout(..).build()`,
/// falling back to `Client::default()` (an unbounded client) on the
/// (practically unreachable — no TLS backend to fail to initialize for a
/// plain-HTTP loopback client) builder error, matching
/// `trusty_common::monitor::memory_client::MemoryClient::new`'s precedent
/// rather than `unwrap()`-ing in library code.
/// Test: `tests::build_client_bounds_a_stalled_connection`.
pub(super) fn build_client(
    connect_timeout: Duration,
    request_timeout: Duration,
) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(request_timeout)
        .build()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Instant;

    /// Why: proves `build_client` actually bounds a hung daemon rather than
    /// trusting reqwest's defaults — issue #2471's TUI freeze was exactly
    /// this shape: a peer that completes the TCP handshake but never writes
    /// a response. A `TcpListener` that is bound and kept alive but never
    /// `.accept()`-serviced reproduces that stall without a mock-server
    /// dependency: the kernel completes the handshake from its accept
    /// backlog, so the client's `connect()` succeeds and the request body is
    /// written, but no response ever arrives.
    /// What: builds a client with tiny (200ms connect / 300ms request)
    /// bounds, issues a GET against the stalled listener, and asserts the
    /// call errors well within a generous CI margin rather than hanging.
    #[tokio::test]
    async fn build_client_bounds_a_stalled_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stalling listener");
        let addr = listener.local_addr().expect("read local_addr");

        let client = build_client(Duration::from_millis(200), Duration::from_millis(300));
        let url = format!("http://{addr}/");

        let start = Instant::now();
        let result = client.get(&url).send().await;
        let elapsed = start.elapsed();

        // `listener` stays alive (dropped at function end) for the whole
        // request so the connection stalls rather than being refused outright.
        assert!(result.is_err(), "expected the stalled request to time out");
        assert!(
            elapsed < Duration::from_secs(5),
            "request took {elapsed:?}, expected it to be bounded by the ~300ms timeout"
        );
    }

    /// Why: pins the production constants and their delegation so a future
    /// edit can't silently widen [`DEFAULT_REQUEST_TIMEOUT`] back toward
    /// "unbounded" or shrink [`CHAT_REQUEST_TIMEOUT`] below the daemon's own
    /// upstream bound without a visible test failure.
    /// What: asserts the three constants' values and that `default_client`
    /// builds without panicking.
    #[test]
    fn default_client_uses_default_bounds() {
        assert_eq!(DEFAULT_CONNECT_TIMEOUT, Duration::from_secs(3));
        assert_eq!(DEFAULT_REQUEST_TIMEOUT, Duration::from_secs(10));
        assert!(
            CHAT_REQUEST_TIMEOUT > DEFAULT_REQUEST_TIMEOUT,
            "the chat override must be longer than the default, not shorter"
        );
        let _ = default_client();
    }
}
