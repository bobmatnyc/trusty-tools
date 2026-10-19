//! `tm doctor` row `events`: is the trusty-events bus socket served (#10042)?
//!
//! Why: ADR-0065 runs the event bus as its own launchd daemon, `trusty-events
//! serve`. Producers never block on it — `PushClient` buffers and drops — so a
//! dead bus is silent everywhere else: events stop reaching readers and nothing
//! reports it. This row is where it shows.
//!
//! What: [`check_events`] resolves the socket through
//! `trusty_common::control_bus::events_socket_path` (the helper every producer
//! and reader uses, ADR-0065 decision 2), observes it with [`observe_socket`],
//! and grades the observation with [`grade`]. The probe is one connect over the
//! Unix socket, closed at once — never TCP, and no request is sent, so it holds
//! no connection open and opens no stream. The daemon's serve loop treats a
//! connect-and-close as a liveness probe and logs nothing for it. The row is
//! fail-closed: only a completed connect is `Ok`. A missing socket, a stale one
//! (the file exists, nothing accepts), a path that is not a socket, or a path
//! that cannot be read is `Fail`; a connect that neither completes nor is
//! refused is `Warn`. Liveness beyond "the socket accepts" is #10071 and is not
//! checked; the `Ok` detail says so. Read-only.
//!
//! Test: `doctor_events_tests.rs`.

use std::path::Path;
use std::time::Duration;

use trusty_common::uds::probe::{SocketVerdict, probe_socket_verdict};

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The `tm doctor` row name.
pub(crate) const CHECK_NAME: &str = "events";

/// How long one connect may take. A local socket answers in microseconds.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// What one look at the bus socket found.
///
/// Why: "no socket", "a socket nobody serves" and "a socket we cannot read" are
/// different faults with different remedies, and the row must never fold any of
/// them into healthy.
/// Test: `doctor_events_tests.rs` covers each variant through [`grade`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SocketState {
    /// A connect completed: something accepts on the socket.
    Serving,
    /// No file at the path: the daemon is not running.
    Absent,
    /// The file exists and a connect is refused: a stale socket.
    Stale,
    /// Something other than a socket sits at the path.
    NotASocket,
    /// The path could not be inspected; carries the OS error.
    Unreadable(String),
    /// The connect neither completed nor was refused (a timeout or a permission
    /// error on the connect itself).
    Undetermined,
}

/// Look at the socket at `path` once.
///
/// Why: `probe_socket_verdict` answers serving / not serving / inconclusive, but
/// "not serving" covers both a missing file and a stale one, which need
/// different remedies, so the file is inspected first.
/// What: `symlink_metadata` decides Absent, NotASocket and Unreadable; a socket
/// file is then dialled once through
/// [`trusty_common::uds::probe::probe_socket_verdict`], whose stream is dropped
/// at once.
/// Test: `a_bound_socket_is_serving`, `a_missing_socket_is_absent`,
/// `doctor_events_reports_a_dead_socket`, `a_regular_file_is_not_a_socket`,
/// `an_unreadable_parent_is_unreadable`.
pub(crate) async fn observe_socket(path: &Path, timeout: Duration) -> SocketState {
    use std::os::unix::fs::FileTypeExt as _;

    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SocketState::Absent,
        Err(e) => return SocketState::Unreadable(e.to_string()),
        Ok(meta) if !meta.file_type().is_socket() => return SocketState::NotASocket,
        Ok(_) => {}
    }
    match probe_socket_verdict(path, timeout).await {
        SocketVerdict::Serving => SocketState::Serving,
        // The file was there a moment ago, so a refusal (or the file vanishing
        // in between) means nothing serves it.
        SocketVerdict::NotServing => SocketState::Stale,
        _ => SocketState::Undetermined,
    }
}

/// Grade one observation of the socket at `socket` as a doctor row.
///
/// Why: a pure function, so every verdict is testable without a daemon.
/// What: `Serving` is `Ok`, naming the #10071 limit; `Undetermined` is `Warn`;
/// every other state is `Fail` with its remedy.
/// Test: `serving_is_ok_and_names_the_liveness_limit`,
/// `every_non_serving_state_is_never_ok`.
pub(crate) fn grade(socket: &Path, state: &SocketState) -> DoctorCheck {
    let at = socket.display();
    let (status, message) = match state {
        SocketState::Serving => (
            CheckStatus::Ok,
            format!(
                "trusty-events serving at {at}: the socket accepts connections. Liveness \
                 beyond the accept is not checked (#10071)."
            ),
        ),
        SocketState::Absent => (
            CheckStatus::Fail,
            format!(
                "trusty-events is not running: no socket at {at}. Producers buffer, then \
                 drop, events until it runs. Install it with `tctl install trusty-events`, or start it \
                 with `tctl start trusty-events`."
            ),
        ),
        SocketState::Stale => (
            CheckStatus::Fail,
            format!(
                "stale trusty-events socket at {at}: the file exists but nothing accepts \
                 on it, so the daemon exited without removing it. launchd restarts its \
                 LaunchAgent within 10 s; if this persists, run `tctl start \
                 trusty-events` and read `~/.trusty-events/logs/stderr.log`."
            ),
        ),
        SocketState::NotASocket => (
            CheckStatus::Fail,
            format!(
                "{at} exists but is not a socket, so trusty-events cannot serve there. \
                 Move the file aside and restart trusty-events."
            ),
        ),
        SocketState::Unreadable(err) => (
            CheckStatus::Fail,
            format!("cannot read the trusty-events socket at {at}: {err}"),
        ),
        SocketState::Undetermined => (
            CheckStatus::Warn,
            format!(
                "trusty-events socket at {at} neither accepted nor refused a connect \
                 within {}s (a timeout or a permission error); its state is unknown.",
                CONNECT_TIMEOUT.as_secs()
            ),
        ),
    };
    DoctorCheck::new(CHECK_NAME, status, message)
}

/// The `events` row: resolve the bus socket, look at it once, grade it.
///
/// Why / What: see the module docs. A socket path that cannot be resolved is
/// `Fail` — no producer can reach the bus either.
/// Test: `an_unresolvable_socket_path_fails`; the wiring is covered by
/// `run_doctor_produces_seventy_six_checks`.
pub(super) async fn check_events() -> DoctorCheck {
    grade_resolved(trusty_common::control_bus::events_socket_path()).await
}

/// [`check_events`] over an already-resolved path, so the resolution failure is
/// testable without process-global environment.
async fn grade_resolved(socket: anyhow::Result<std::path::PathBuf>) -> DoctorCheck {
    match socket {
        Ok(path) => grade(&path, &observe_socket(&path, CONNECT_TIMEOUT).await),
        Err(e) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!("cannot resolve the trusty-events socket path: {e:#}"),
        ),
    }
}

#[cfg(test)]
#[path = "doctor_events_tests.rs"]
mod tests;
