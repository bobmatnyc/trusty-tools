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
//! a connect that neither completes nor is refused is `Warn`. #10151: a host
//! without the bus is supported (Architect ruling 2026-10-26). #10975: a
//! missing socket is `NotInstalled`, an INFO line (an `Ok` row whose message
//! starts `info:`) naming no command, only when no trusty-events LaunchAgent
//! plist exists, the binary does not resolve, and no launchd label is loaded.
//! The bus unlinks its socket on a clean stop, so a missing socket beside the
//! binary or a loaded label is `Unsupervised`, a `Warn`. A missing socket
//! beside an installed plist grades as above. Read-only.
//!
//! Test: `doctor_events_tests.rs`.

use std::path::{Path, PathBuf};
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
    /// No file at the path, and the bus is installed: the daemon is not running.
    Absent,
    /// #10151: no file at the path and no trusty-events LaunchAgent plist: the
    /// host runs without the bus, a supported setup. #10975: also no binary
    /// and no loaded label.
    NotInstalled,
    /// #10975: no file at the path and no plist, but the binary resolves or
    /// the launchd label is loaded: a bus that ran here is not serving.
    Unsupervised,
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
/// What: `Serving` is `Ok`; `NotInstalled` is an INFO line (`Ok`, message
/// starting `info:`); `VersionSkew`, `Unsupervised` and `Undetermined` are
/// `Warn`; every other state is `Fail` with its remedy. #10151: the
/// not-running remedy no longer names `tctl install trusty-events`, which
/// exits 3 since the bus is internal (#10130); `NotInstalled` names no command.
/// #10975: no row names `tctl upgrade`, which skips internal members, and the
/// skew and unsupervised rows name no install command (owner ruling
/// 2026-11-13, "defer the install text"). #10042: the not-running remedy
/// `tctl start trusty-events` works because the LaunchAgent restarts the bus
/// after any exit, so a loaded job is never left stopped. #10071: a hung bus
/// has not exited, so launchd leaves it alone and `tctl start` answers
/// "already loaded"; its remedy is `tctl restart`, which boots the job out and
/// back in. #10071 review: a wedged bus keeps a backlog slot per connect, so a
/// full backlog refuses the connect and reads `Stale`; that remedy is
/// `tctl restart` too.
/// Test: `serving_is_ok_and_says_the_bus_answered`,
/// `every_non_serving_state_is_never_ok`, `the_not_running_remedy_is_tctl_start`,
/// `a_silent_bus_is_hung_within_the_bound`, `the_stale_remedy_restarts_a_hung_bus`,
/// `a_host_without_the_bus_is_an_info_line_with_no_fix_command`,
/// `a_bus_that_refuses_heartbeat_ms_is_version_skew`,
/// `a_bus_footprint_without_a_socket_is_never_ok`.
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
                "trusty-events at {at} is a build older than #10071 and this tm is {}: it \
                 refused the `heartbeat_ms` liveness request ({refusal}), so whether it is \
                 hung cannot be checked. The two ship in lockstep.",
                env!("CARGO_PKG_VERSION")
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
                 drop, events until it runs. Its LaunchAgent is installed; start it with \
                 `tctl start trusty-events`."
            ),
        ),
        // #10975: a host without the bus is supported, so this is an INFO line
        // with no fix command (owner ruling 2026-11-13).
        SocketState::NotInstalled => (
            CheckStatus::Ok,
            "info: trusty-events: not installed (internal, optional)".to_owned(),
        ),
        // #10975: the bus unlinks its socket on a clean stop, so a binary or a
        // loaded label with no socket is a bus that is not serving.
        SocketState::Unsupervised => (
            CheckStatus::Warn,
            format!(
                "trusty-events is not running: no socket at {at} and no trusty-events \
                 LaunchAgent, but its binary resolves or its launchd label is loaded. \
                 Producers buffer, then drop, events until it runs."
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
/// `Fail` — no producer can reach the bus either. #10151: `home` locates the
/// LaunchAgents directory that tells a host without the bus from a stopped one.
/// #10975: the binary's search path is `PATH` plus `~/.cargo/bin`, since a
/// launchd-started daemon's `PATH` may omit the cargo install directory.
/// Test: `an_unresolvable_socket_path_fails`; the wiring is covered by
/// `run_doctor_produces_eighty_checks`.
pub(super) async fn check_events(home: &Path) -> DoctorCheck {
    let agents = super::doctor_launchd_process_type::launch_agents_dir(home).path;
    let mut bin_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    bin_dirs.push(home.join(".cargo").join("bin"));
    let probe = InstallProbe {
        agents_dir: &agents,
        bin_dirs,
        label_loaded,
    };
    grade_resolved(trusty_common::control_bus::events_socket_path(), &probe).await
}

/// Where the row looks for a trusty-events install besides its socket.
///
/// Why (#10975): the bus unlinks its socket on a clean stop, so "no socket and
/// no plist" also describes a bus started by hand and stopped, or a loaded label
/// whose plist was deleted. Each input is injected so tests need no real
/// `PATH` or launchd.
pub(super) struct InstallProbe<'a> {
    /// The LaunchAgents directory that would hold the bus's plist.
    pub agents_dir: &'a Path,
    /// The directories searched for the `trusty-events` binary.
    pub bin_dirs: Vec<PathBuf>,
    /// Whether a launchd label is loaded; `true` when that cannot be told.
    pub label_loaded: fn(&str) -> bool,
}

/// What a missing socket means on this host.
enum Footprint {
    /// The plist is installed (or could not be inspected).
    Plist,
    /// No plist, but the binary resolves or the label is loaded.
    Unsupervised,
    /// No plist, no binary, no loaded label.
    None,
}

/// [`check_events`] over an already-resolved path and an [`InstallProbe`], so
/// the resolution failure and the no-bus host are testable without
/// process-global environment.
///
/// What: observes the socket and grades it. #10975: an `Absent` socket is
/// `NotInstalled` only when [`footprint`] finds nothing; a binary or a loaded
/// label with no plist is `Unsupervised`.
/// Test: `a_host_without_the_bus_is_an_info_line_with_no_fix_command`,
/// `a_host_with_the_bus_still_fails_on_every_fault`,
/// `a_bus_footprint_without_a_socket_is_never_ok`.
async fn grade_resolved(socket: anyhow::Result<PathBuf>, probe: &InstallProbe<'_>) -> DoctorCheck {
    match socket {
        Ok(path) => {
            let state = match observe_socket(&path, CONNECT_TIMEOUT).await {
                SocketState::Absent => match footprint(probe) {
                    Footprint::Plist => SocketState::Absent,
                    Footprint::Unsupervised => SocketState::Unsupervised,
                    Footprint::None => SocketState::NotInstalled,
                },
                state => state,
            };
            grade(&path, &state)
        }
        Err(e) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!("cannot resolve the trusty-events socket path: {e:#}"),
        ),
    }
}

/// What trace of the bus the host carries besides its socket.
///
/// Why (#10151, #10975): `tctl` installs the bus by writing its plist, and the
/// socket's directory is no signal — resolving the socket path creates it. A
/// plist that cannot be inspected counts as installed, so the row stays
/// fail-closed.
/// What: the plist at `<agents_dir>/<canonical label>.plist` exists or its
/// lookup errors → `Plist`; else a `trusty-events` file in any `bin_dirs`
/// entry, or a loaded label → `Unsupervised`; else `None`.
/// Test: `a_host_with_the_bus_still_fails_on_every_fault`,
/// `a_bus_footprint_without_a_socket_is_never_ok`.
fn footprint(probe: &InstallProbe<'_>) -> Footprint {
    let service = trusty_common::control_bus::EVENTS_SERVICE;
    let label = trusty_common::launchd_labels::canonical_label(service);
    let plist = probe.agents_dir.join(format!("{label}.plist"));
    if plist.try_exists().unwrap_or(true) {
        return Footprint::Plist;
    }
    let binary = probe.bin_dirs.iter().any(|d| d.join(service).is_file());
    if binary || (probe.label_loaded)(&label) {
        Footprint::Unsupervised
    } else {
        Footprint::None
    }
}

/// Whether `label` is loaded in this user's launchd GUI domain.
///
/// Why (#10975): `trusty_common::launchd::label_is_loaded` reads a failed query
/// as "not loaded", which would turn an unknown into a green row here.
/// What: `launchctl list <label>` with its output discarded (#8756: never
/// `launchctl print`, which prints the job's environment). Exit 0 is loaded;
/// exit 113, launchd's "could not find service", is not; any other exit or a
/// spawn failure counts as loaded, except off macOS, where there is no launchd.
/// Test: side-effecting `launchctl` call; the decision it feeds is covered by
/// `a_bus_footprint_without_a_socket_is_never_ok` through an injected fake.
fn label_loaded(label: &str) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    std::process::Command::new("launchctl")
        .args(["list", label])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_or(true, |s| s.code() != Some(113))
}

#[cfg(test)]
#[path = "doctor_events_tests.rs"]
mod tests;
