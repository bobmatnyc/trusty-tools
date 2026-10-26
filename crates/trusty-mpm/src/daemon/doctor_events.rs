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
//! and grades the observation with [`grade`]. The probe runs over the Unix
//! socket only, never TCP. It first connects and closes at once, which the
//! daemon's serve loop treats as a liveness probe and does not log; that tells
//! a missing socket from a stale one. #10071: the kernel completes that connect
//! for a stopped or wedged daemon too, so a served socket then gets one
//! `events.subscribe` asking for a heartbeat every [`PROBE_HEARTBEAT_MS`]. The
//! probe waits at most [`FIRST_FRAME_TIMEOUT`] for the first frame and then
//! closes the connection; it spawns no task. The row is fail-closed: only a
//! frame on that subscription is `Ok`. Silence after the connect is `Fail`
//! (`hung`). A refusal of `heartbeat_ms` is `Warn`: the bus is older than this
//! tm, and they ship in lockstep with no fallback. A missing socket, a stale
//! one (the file exists, nothing accepts), a path that is not a socket, a path
//! that cannot be read, or a subscription that ends without a frame is `Fail`;
//! a connect that neither completes nor is refused is `Warn`. Read-only.
//!
//! Test: `doctor_events_tests.rs`.

use std::path::Path;
use std::time::Duration;

use trusty_common::control_bus::{SubscribeItem, SubscribeParams, SubscribeRequest};
use trusty_common::uds::probe::{SocketVerdict, probe_socket_verdict};
use trusty_common::uds::server::{CODE_INVALID_PARAMS, RpcError};
use trusty_common::uds::{UdsRpcError, send_framed_stream_request};

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The `tm doctor` row name.
pub(crate) const CHECK_NAME: &str = "events";

/// How long one connect may take. A local socket answers in microseconds.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// The heartbeat interval the probe asks the bus for (#10071, owner ruling Dd1).
const PROBE_HEARTBEAT_MS: u64 = 500;

/// How long the probe waits for the bus's first frame (#10071, Dd1). The bus
/// sends a heartbeat at once on open, so a serving bus answers in
/// milliseconds; four heartbeat intervals is a hang, not a slow bus.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(2);

/// What one look at the bus socket found.
///
/// Why: "no socket", "a socket nobody serves" and "a socket we cannot read" are
/// different faults with different remedies, and the row must never fold any of
/// them into healthy.
/// Test: `doctor_events_tests.rs` covers each variant through [`grade`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SocketState {
    /// The bus answered a subscription with a frame.
    Serving,
    /// #10071: a connect completed but the bus sent no frame in time.
    Hung,
    /// #10071: the bus refused `heartbeat_ms`, so it predates this tm; carries
    /// its refusal.
    VersionSkew(String),
    /// A connect completed but the subscription ended without a frame (a
    /// hang-up, an error, or a frame that does not decode); carries the error.
    Unanswered(String),
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
/// at once. A served socket is then subscribed to by [`await_first_frame`].
/// Test: `a_bus_that_sends_a_heartbeat_is_ok`, `a_missing_socket_is_absent`,
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
        // #10071: an accept is not an answer; ask the bus for a frame.
        SocketVerdict::Serving => await_first_frame(path).await,
        // The file was there a moment ago, so a refusal (or the file vanishing
        // in between) means nothing serves it.
        SocketVerdict::NotServing => SocketState::Stale,
        _ => SocketState::Undetermined,
    }
}

/// Open one heartbeat subscription on a served socket and wait for a frame.
///
/// Why (#10071): the kernel accepts a connect and buffers the request for a
/// daemon that is stopped or wedged, so only a frame back proves the bus runs.
/// What: sends `events.subscribe` with `heartbeat_ms` [`PROBE_HEARTBEAT_MS`]
/// and live-only params, so the bus's first frame is its heartbeat. One
/// deadline, [`FIRST_FRAME_TIMEOUT`], bounds the open and the first read. Any
/// decoded item is `Serving`. Silence after the open is `Hung`; an open that
/// does not complete is `Undetermined`. An `invalid_params` refusal naming
/// `heartbeat_ms` is `VersionSkew`; any other end is `Unanswered`. The stream
/// is dropped on return, which closes the connection.
/// Test: `a_silent_bus_is_hung_within_the_bound`,
/// `a_listener_that_never_accepts_is_hung`, `a_bus_that_sends_a_heartbeat_is_ok`,
/// `a_bus_that_refuses_heartbeat_ms_is_version_skew`,
/// `a_bus_that_hangs_up_without_a_frame_fails`.
async fn await_first_frame(path: &Path) -> SocketState {
    let deadline = tokio::time::Instant::now() + FIRST_FRAME_TIMEOUT;
    let params = SubscribeParams {
        heartbeat_ms: Some(PROBE_HEARTBEAT_MS),
        ..SubscribeParams::default()
    };
    let request = SubscribeRequest::new(1, params);
    let opened = tokio::time::timeout_at(
        deadline,
        send_framed_stream_request::<_, SubscribeItem>(path, &request, FIRST_FRAME_TIMEOUT),
    )
    .await;
    let mut stream = match opened {
        Ok(Ok(stream)) => stream,
        Err(_) | Ok(Err(UdsRpcError::Timeout { .. })) => return SocketState::Undetermined,
        Ok(Err(e)) => return SocketState::Unanswered(e.to_string()),
    };
    match tokio::time::timeout_at(deadline, stream.next_frame()).await {
        Ok(Some(Ok(_))) => SocketState::Serving,
        Err(_) | Ok(Some(Err(UdsRpcError::Timeout { .. }))) => SocketState::Hung,
        Ok(Some(Err(UdsRpcError::Stream { error, .. }))) if refuses_heartbeat(&error) => {
            SocketState::VersionSkew(error.message)
        }
        Ok(Some(Err(e))) => SocketState::Unanswered(e.to_string()),
        Ok(None) => SocketState::Unanswered("the bus ended the subscription at once".to_owned()),
    }
}

/// Whether a refusal is a pre-#10071 bus rejecting `heartbeat_ms`: its params
/// are `deny_unknown_fields`, so it answers `invalid_params` naming the field.
fn refuses_heartbeat(error: &RpcError) -> bool {
    error.code == CODE_INVALID_PARAMS && error.message.contains("heartbeat_ms")
}

/// Grade one observation of the socket at `socket` as a doctor row.
///
/// Why: a pure function, so every verdict is testable without a daemon.
/// What: `Serving` is `Ok`; `VersionSkew` and `Undetermined` are `Warn`; every
/// other state is `Fail` with its remedy. #10042: the not-running remedy
/// `tctl start trusty-events` works because the LaunchAgent restarts the bus
/// after any exit, so a loaded job is never left stopped. #10071: a hung bus
/// has not exited, so launchd leaves it alone and `tctl start` answers
/// "already loaded"; its remedy is `tctl restart`, which boots the job out and
/// back in. #10071 review: a wedged bus keeps a backlog slot per connect, so a
/// full backlog refuses the connect and reads `Stale`; that remedy is
/// `tctl restart` too.
/// Test: `serving_is_ok_and_says_the_bus_answered`,
/// `every_non_serving_state_is_never_ok`, `the_not_running_remedy_is_tctl_start`,
/// `a_silent_bus_is_hung_within_the_bound`, `the_stale_remedy_restarts_a_hung_bus`.
pub(crate) fn grade(socket: &Path, state: &SocketState) -> DoctorCheck {
    let at = socket.display();
    let (status, message) = match state {
        SocketState::Serving => (
            CheckStatus::Ok,
            format!(
                "trusty-events serving at {at}: the bus answered a subscription within {}s.",
                FIRST_FRAME_TIMEOUT.as_secs()
            ),
        ),
        SocketState::Hung => (
            CheckStatus::Fail,
            format!(
                "trusty-events is hung at {at}: the socket accepts connections but the bus \
                 sent no frame within {}s, so it is stopped or wedged and producers' events \
                 go nowhere. Restart it with `tctl restart trusty-events`, then read \
                 `~/.trusty-events/logs/stderr.log`.",
                FIRST_FRAME_TIMEOUT.as_secs()
            ),
        ),
        SocketState::VersionSkew(refusal) => (
            CheckStatus::Warn,
            format!(
                "trusty-events at {at} is older than this tm: it refused the `heartbeat_ms` \
                 liveness request ({refusal}), so whether it is hung cannot be checked. The \
                 two ship in lockstep; run `tctl upgrade trusty-events`."
            ),
        ),
        SocketState::Unanswered(err) => (
            CheckStatus::Fail,
            format!(
                "trusty-events at {at} accepted a connect but did not answer a subscription: \
                 {err}. Read `~/.trusty-events/logs/stderr.log`; if this persists, run \
                 `tctl restart trusty-events`."
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
                "stale trusty-events socket at {at}: the file exists but the connect is \
                 refused, so the daemon exited without removing it, or is hung with a full \
                 backlog. launchd restarts an exited LaunchAgent within 10 s; if this \
                 persists, run `tctl restart trusty-events` and read \
                 `~/.trusty-events/logs/stderr.log`."
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
/// `run_doctor_produces_seventy_nine_checks`.
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
