//! `tm doctor`'s single daemon-reachability row (#6336).
//!
//! Why: `tm doctor` used to round-trip the WHOLE report through the daemon's
//! `GET /api/v1/doctor`, so an unreachable daemon aborted the command with
//! `doctor failed: daemon unreachable: …` and not one of the 27 purely local
//! checks ran. The daemon's presence is a fact worth reporting, not a
//! precondition for reporting anything: doctor now runs the battery in-process
//! and appends this one row.
//! What: [`probe_daemon`] issues one bounded `GET /health` against the URL the
//! CLI already resolved through the gateway/lock-file discovery chain, and
//! [`daemon_check`] folds the outcome into a [`DoctorCheck`]. The row's message
//! names no port and no transport, so the probe can move to a Unix socket
//! (#6288) without the operator-visible wording changing.
//! Test: `src/bin/tm/commands/doctor_daemon_row_tests.rs`.

use std::time::Duration;

use trusty_mpm::client::{DaemonClient, HealthSnapshot};
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

/// Name of this check as it appears in `tm doctor` output.
pub(crate) const CHECK_NAME: &str = "trusty_mpm_daemon";

/// Ceiling on doctor's own daemon probe.
///
/// Why: this probe is one row of an interactive report, so its budget is a
/// latency budget, not the wedged-daemon correctness ceiling `DaemonClient`'s
/// client-level default exists to enforce. A loopback daemon answers `/health`
/// in single-digit milliseconds; two seconds is far above any legitimate
/// latency and still keeps `tm doctor` responsive when nothing is listening.
/// What: passed per-request to `DaemonClient::health_snapshot_within`.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What one bounded `/health` probe established about the daemon.
///
/// Why: "unreachable" is two different operator situations with two different
/// remedies — nothing is listening (start it), versus something is listening
/// but not answering (it is wedged; restart it). Collapsing them loses the only
/// part of the row an operator acts on.
/// What: the three outcomes [`daemon_check`] renders.
/// Test: `daemon_row_is_ok_when_reachable`, `daemon_row_warns_when_not_running`,
/// `daemon_row_is_unknown_when_unresponsive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonReachability {
    /// `/health` answered.
    Reachable,
    /// Nothing accepted the connection.
    NotRunning,
    /// Something accepted the connection but did not answer the probe.
    Unresponsive,
}

/// Render one [`DaemonReachability`] as the appended check row.
///
/// Why: the message deliberately names neither a port nor a transport. The old
/// wording — "port 7880 unreachable" — was wrong twice over: it hard-coded a
/// port the discovery chain may never have used, and it will be wrong again the
/// day the daemon moves to a Unix socket. Naming only the daemon keeps the row
/// correct across that move (#6288).
/// What: `Reachable` is `Ok`; `NotRunning` is `Warn` (every local check still
/// ran, but session management and the MCP surface are unavailable);
/// `Unresponsive` is `Unknown`, because a socket that accepts and then says
/// nothing has told us nothing (#4005 precedent).
/// Test: `daemon_row_is_ok_when_reachable`, `daemon_row_warns_when_not_running`,
/// `daemon_row_is_unknown_when_unresponsive`, `daemon_row_never_names_a_port`.
pub(crate) fn daemon_check(reachability: DaemonReachability) -> DoctorCheck {
    match reachability {
        DaemonReachability::Reachable => {
            DoctorCheck::new(CHECK_NAME, CheckStatus::Ok, "trusty-mpm daemon: reachable")
        }
        DaemonReachability::NotRunning => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            "trusty-mpm daemon: not running — every local check above still ran; \
             start it with `tm start` when you need session management or the MCP surface",
        ),
        DaemonReachability::Unresponsive => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "trusty-mpm daemon: unresponsive — it accepted the connection but did not \
             answer the health probe in time; restart it and re-run `tm doctor`",
        ),
    }
}

/// Probe the daemon once, bounded, and classify the outcome.
///
/// Why: `tm doctor` must never start, restart, or require a daemon, so this is
/// a read-only observation whose failure is a row rather than an abort. The URL
/// is the one `main` already resolved via
/// [`trusty_mpm::core::resolve_daemon_url_via_gateway`] — explicit
/// `--url`/`TRUSTY_MPM_URL` first, then the trusty-console gateway, then the
/// lock file — so no port is ever hard-coded here.
/// What: one `GET /health` under [`PROBE_TIMEOUT`]. A transport error that
/// never established a connection is `NotRunning`; any other failure (timeout,
/// non-2xx, undecodable body) is `Unresponsive`. Returns the snapshot too,
/// because the #2332 staleness and #4230 orphan checks reason about that same
/// single sample rather than probing again.
/// Test: `daemon_probe_reports_not_running_when_nothing_listens`.
pub(crate) async fn probe_daemon(url: &str) -> (DaemonReachability, Option<HealthSnapshot>) {
    // See #6288 — when the trusty-mpm daemon moves off TCP, only this call
    // swaps transport; the row `daemon_check` renders does not change.
    let client = DaemonClient::new(url.to_string());
    match client.health_snapshot_within(PROBE_TIMEOUT).await {
        Ok(snapshot) => (DaemonReachability::Reachable, Some(snapshot)),
        Err(e) if e.is_connect() => (DaemonReachability::NotRunning, None),
        Err(_) => (DaemonReachability::Unresponsive, None),
    }
}

#[cfg(test)]
#[path = "doctor_daemon_row_tests.rs"]
mod doctor_daemon_row_tests;
