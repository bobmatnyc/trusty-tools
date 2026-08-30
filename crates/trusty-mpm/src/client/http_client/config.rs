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
//! [`CHAT_REQUEST_TIMEOUT`] and [`PROVISION_REQUEST_TIMEOUT`] are longer,
//! explicit per-request overrides for the endpoints that legitimately run
//! long (see each constant's own doc).
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

/// Per-request timeout override for session-provisioning endpoints.
///
/// Why: `DaemonClient::spawn_managed_session` (`POST
/// /api/v1/sessions/managed`) has a server handler that runs
/// `WorkspaceProvisioner::provision`/`provision_in` SYNCHRONOUSLY inside the
/// request — a git clone/fetch plus worktree add plus agent/skill deploy. The
/// first spawn against a newly-registered project pays a full clone and can
/// easily exceed [`DEFAULT_REQUEST_TIMEOUT`]'s 10s, which would hard-fail the
/// client mid-provision while the daemon keeps working, orphaning a session
/// the caller believes failed. 180s is bounded but generous: well above any
/// legitimate clone/worktree/deploy sequence on a slow network or loaded
/// machine, while still being a hard, finite bound (never the pre-#2471 "no
/// timeout at all").
/// What: passed to `reqwest::RequestBuilder::timeout` at the
/// `spawn_managed_session` call site — a per-request override, not a change
/// to the client-level default every other endpoint uses. Mirrors the
/// [`CHAT_REQUEST_TIMEOUT`] pattern for the same reason: the endpoint's own
/// server-side work legitimately runs long.
/// Test: `tests::default_client_uses_default_bounds` asserts it exceeds
/// [`DEFAULT_REQUEST_TIMEOUT`].
///
/// `pub(crate)` (#4488): `connectors::tm::TmConnector::create_session` POSTs
/// the SAME route from a different module tree and needs the same bound; a
/// second copy of the constant there would be free to drift.
pub(crate) const PROVISION_REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// Per-request timeout override for `GET /api/v1/doctor` (#5111).
///
/// Why: the doctor handler runs the whole ~32-check battery SYNCHRONOUSLY
/// inside the request. Several checks are unbounded in the operator's
/// environment rather than in the daemon: `check_gh_account` shells out to
/// `gh auth status` (measured 3.06s on this machine), and `check_worktrees` /
/// `check_worktree_disk` walk the managed workspace root. Measured against
/// [`DEFAULT_REQUEST_TIMEOUT`]: 8 of 10 runs failed, every failure clustered at
/// 10.2–10.3s and every pass at 9.6–9.7s — the client was hanging up on a
/// report the daemon was still producing, so Telegram `/doctor` and Slack
/// `/doctor` reported "daemon unreachable" for a healthy daemon. 120s is well
/// clear of that battery on a loaded machine and stays a hard, finite bound.
/// What: passed to `reqwest::RequestBuilder::timeout` at the
/// [`super::DaemonClient::doctor`] call site — a per-request override, not a
/// change to the client-level default every other endpoint uses. Same pattern
/// and same reason as [`CHAT_REQUEST_TIMEOUT`] and
/// [`PROVISION_REQUEST_TIMEOUT`].
/// Test: `tests::default_client_uses_default_bounds` pins the value;
/// `client::executor::tests::execute_doctor_against_test_daemon` exercises the
/// real round trip that was racing the default bound.
pub(super) const DOCTOR_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Per-request timeout override for `POST …/managed/prune-worktrees` when the
/// merged-PR reclaim pass is requested (#5830).
///
/// Why: that request runs the whole merged-PR survey synchronously inside the
/// handler. The survey classifies every registered worktree (~92s measured
/// against the real store) and then byte-walks each one, including its
/// multi-gigabyte `target/` — a walk that
/// [`SurveyBudget`](crate::session_manager::worktree_reclaim_sweep::SurveyBudget)'s
/// own doc measures at over 600 seconds for 46 worktrees. Under
/// [`DEFAULT_REQUEST_TIMEOUT`] the client hung up at 10s on EVERY invocation, so
/// `tm session prune-worktrees --merged-prs` had never once completed on this
/// repository. Retrying could not help — the bound was 60x short of the work.
/// 1800s is roughly 3x the measured worst case, and stays a hard, finite bound.
/// What: passed to `reqwest::RequestBuilder::timeout` at the
/// `session_prune_worktrees` call site, and ONLY when `merged_prs` is set — the
/// orphan-only sweep keeps the 10s default, so a wedged daemon still fails fast
/// for every other prune. Mirrors [`CHAT_REQUEST_TIMEOUT`] and
/// [`PROVISION_REQUEST_TIMEOUT`], which exist for the same reason.
/// Test: `tests::default_client_uses_default_bounds` pins the value;
/// `merged_pr_request_outlives_the_default_client_timeout` (in
/// `bin/tm/commands/managed_merged_prs_tests.rs`) proves the override reaches
/// the wire.
///
/// `pub` (re-exported by [`super`]): a `[[bin]]` target is a distinct crate from
/// the library, so `tm`'s own command module cannot see `pub(crate)` — the same
/// crate boundary [`super::default_client`] exists to cross.
pub const RECLAIM_SURVEY_REQUEST_TIMEOUT: Duration = Duration::from_secs(1800);

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
/// rather than `unwrap()`-ing in library code. The fallback is logged at
/// `warn` level (issue #2512 review) so a silent regression back to an
/// UNBOUNDED default client — the exact failure mode this module exists to
/// prevent — can never happen without a trace.
/// Test: `tests::build_client_bounds_a_stalled_connection`.
pub(super) fn build_client(
    connect_timeout: Duration,
    request_timeout: Duration,
) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(request_timeout)
        .build()
        .unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                "DaemonClient: reqwest::ClientBuilder::build failed, falling back to an \
                 UNBOUNDED default client (connect_timeout/timeout bounds are NOT applied)"
            );
            reqwest::Client::default()
        })
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
    /// "unbounded" or shrink [`CHAT_REQUEST_TIMEOUT`]/[`PROVISION_REQUEST_TIMEOUT`]
    /// below the bound each exists to protect (the daemon's own upstream chat
    /// timeout, and a worst-case first-clone provisioning respectively)
    /// without a visible test failure.
    /// What: asserts the four constants' values and that `default_client`
    /// builds without panicking.
    #[test]
    fn default_client_uses_default_bounds() {
        assert_eq!(DEFAULT_CONNECT_TIMEOUT, Duration::from_secs(3));
        assert_eq!(DEFAULT_REQUEST_TIMEOUT, Duration::from_secs(10));
        assert!(
            CHAT_REQUEST_TIMEOUT > DEFAULT_REQUEST_TIMEOUT,
            "the chat override must be longer than the default, not shorter"
        );
        assert_eq!(PROVISION_REQUEST_TIMEOUT, Duration::from_secs(180));
        assert!(
            PROVISION_REQUEST_TIMEOUT > DEFAULT_REQUEST_TIMEOUT,
            "the provisioning override must be longer than the default, not shorter"
        );
        // #5111: the doctor battery measured 10.2-10.3s against the 10s
        // default, so anything at or near that bound reinstates the race.
        assert_eq!(DOCTOR_REQUEST_TIMEOUT, Duration::from_secs(120));
        assert!(
            DOCTOR_REQUEST_TIMEOUT > DEFAULT_REQUEST_TIMEOUT * 2,
            "the doctor override must clear the measured 10.2s battery by a \
             wide margin, not sit beside it"
        );
        // #5830: the merged-PR survey outruns every other override here, so it
        // must be the longest of them — shrinking it back toward the provision
        // bound reinstates the always-times-out failure.
        assert_eq!(RECLAIM_SURVEY_REQUEST_TIMEOUT, Duration::from_secs(1800));
        assert!(
            RECLAIM_SURVEY_REQUEST_TIMEOUT > PROVISION_REQUEST_TIMEOUT,
            "the merged-PR survey outlasts a first-clone provision, so its bound \
             must exceed the provisioning one"
        );
        let _ = default_client();
    }
}
