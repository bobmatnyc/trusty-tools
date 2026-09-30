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
//! [`daemon_check`] folds the outcome into a [`DoctorCheck`]. #6288 step 1:
//! the probe runs over the daemon's unix socket, and the row names the
//! transport it used (the brief's answer to the issue's open wording question);
//! it still names no port.
//! Test: `src/bin/tm/commands/doctor_daemon_row_tests.rs`.

use std::time::Duration;

use trusty_mpm::client::{DaemonCallError, DaemonClient, HealthSnapshot};
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
/// #6288: a dial the OS itself refused (a sandbox without the socket in its
/// profile, a directory mode) is a third remedy — `tm start` cannot fix it.
/// What: the four outcomes [`daemon_check`] renders.
/// Test: `daemon_row_is_ok_when_reachable`, `daemon_row_warns_when_not_running`,
/// `daemon_row_is_unknown_when_unresponsive`,
/// `daemon_probe_names_the_errno_when_the_os_denies_the_dial`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonReachability {
    /// `/health` answered.
    Reachable,
    /// Nothing accepted the connection.
    NotRunning,
    /// The OS refused the dial with permission denied; the errno when known.
    Denied(Option<i32>),
    /// Something accepted the connection but did not answer the probe.
    Unresponsive,
}

/// `EPERM`/`EACCES` by name, else the raw errno.
pub(crate) fn errno_name(errno: Option<i32>) -> String {
    match errno {
        Some(1) => "EPERM".to_string(),
        Some(13) => "EACCES".to_string(),
        Some(n) => format!("errno {n}"),
        None => "permission denied".to_string(),
    }
}

/// Render one [`DaemonReachability`] as the appended check row.
///
/// Why: the message never names a port — "port 7880 unreachable" hard-coded a
/// port the discovery chain may never have used. #6288 step 1: it names the
/// transport the probe used instead, so an operator sees which socket failed.
/// What: `Reachable` is `Ok`; `NotRunning` is `Warn` (every local check still
/// ran, but session management and the MCP surface are unavailable); `Denied`
/// is `Warn` naming the errno, with no `tm start` advice; `Unresponsive` is
/// `Unknown`, because a socket that accepts and then says nothing has told us
/// nothing (#4005 precedent).
/// Test: `daemon_row_is_ok_when_reachable`, `daemon_row_warns_when_not_running`,
/// `daemon_row_is_unknown_when_unresponsive`,
/// `daemon_row_names_its_transport_and_no_port`.
pub(crate) fn daemon_check(reachability: DaemonReachability, transport: &str) -> DoctorCheck {
    match reachability {
        DaemonReachability::Reachable => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!("trusty-mpm daemon: reachable (via {transport})"),
        ),
        DaemonReachability::NotRunning => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!(
                "trusty-mpm daemon: not running (via {transport}) — every local check above \
                 still ran; start it with `tm start` when you need session management or the \
                 MCP surface"
            ),
        ),
        DaemonReachability::Denied(errno) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!(
                "trusty-mpm daemon: dial refused by the OS (via {transport}): {} — this \
                 process may not reach the socket (a sandbox profile without it, or a \
                 directory mode); the daemon's own state is unknown",
                errno_name(errno)
            ),
        ),
        DaemonReachability::Unresponsive => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!(
                "trusty-mpm daemon: unresponsive (via {transport}) — it accepted the connection \
                 but did not answer the health probe in time; restart it and re-run `tm doctor`"
            ),
        ),
    }
}

/// Probe the daemon once, bounded, and classify the outcome.
///
/// Why: `tm doctor` must never start, restart, or require a daemon, so this is
/// a read-only observation whose failure is a row rather than an abort. #6288:
/// the caller's client decides the transport; `tm doctor` and `tm status` hand
/// in a socket-only one.
/// What: one `GET /health` under [`PROBE_TIMEOUT`]. A dial the OS refused with
/// permission denied is `Denied`; any other failure that never established a
/// connection is `NotRunning`; any other failure (timeout, non-2xx,
/// undecodable body) is `Unresponsive`. Returns the snapshot too,
/// because the #2332 staleness and #4230 orphan checks reason about that same
/// single sample rather than probing again.
/// Test: `daemon_probe_reports_not_running_when_nothing_listens`,
/// `daemon_probe_over_an_absent_socket_is_not_running`,
/// `daemon_probe_names_the_errno_when_the_os_denies_the_dial`.
pub(crate) async fn probe_daemon(
    client: &DaemonClient,
) -> (DaemonReachability, Option<HealthSnapshot>) {
    // #6288 step 1: the client's transport decides; `tm doctor`/`tm status`
    // hand in a socket-only client, so an absent socket is `NotRunning`.
    classify_probe(client.health_snapshot_within(PROBE_TIMEOUT).await)
}

/// The pure half of [`probe_daemon`]: one probe result to its outcome.
///
/// Test: `daemon_probe_names_the_errno_when_the_os_denies_the_dial`.
pub(crate) fn classify_probe(
    result: Result<HealthSnapshot, DaemonCallError>,
) -> (DaemonReachability, Option<HealthSnapshot>) {
    match result {
        Ok(snapshot) => (DaemonReachability::Reachable, Some(snapshot)),
        Err(e) if e.denied_os_error().is_some() => (
            DaemonReachability::Denied(e.denied_os_error().and_then(|io| io.raw_os_error())),
            None,
        ),
        Err(e) if e.is_connect() => (DaemonReachability::NotRunning, None),
        Err(_) => (DaemonReachability::Unresponsive, None),
    }
}

#[cfg(test)]
#[path = "doctor_daemon_row_tests.rs"]
mod doctor_daemon_row_tests;
