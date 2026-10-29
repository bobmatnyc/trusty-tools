//! `ServiceConnector` for the trusty-events bus daemon (#10042, ADR-0065).
//!
//! Why: the bus runs as its own launchd daemon, and producers never block on
//! it, so a dead bus shows nowhere else. ADR-0035 makes the console's roster
//! the health aggregate, so the bus gets a row there like every other daemon.
//!
//! What: [`EventsConnector::detect`] checks for the binary on PATH, resolves
//! the socket through `trusty_common::control_bus::events_socket_path` (the
//! helper every producer and reader uses), and looks at it once:
//! [`observe_socket`] inspects the file and dials it with
//! `trusty_common::uds::probe::probe_socket_verdict` — one connect, closed at
//! once, never TCP, no request. The bus names no health method, so the probe
//! cannot read a version and the row carries none. Only a completed connect is
//! `Running`; liveness beyond the accept is #10071.
//!
//! This reads the socket directly rather than PR3's
//! `/api/console/events/status` (#10072), which is not merged on this base.
//!
//! Test: the `tests` module below — every observation's status, a stale and a
//! live socket observed for real, and the binary gate.

use std::path::{Path, PathBuf};
use std::time::Duration;

use trusty_common::uds::probe::{SocketVerdict, probe_socket_verdict};

use crate::connector::{ServiceConnector, ServiceInfo, ServiceLifecycle, ServiceStatus};

use super::helpers::binary_on_path;

/// How long one connect may take. A local socket answers in microseconds.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// What one look at the bus socket found.
///
/// Why: "not running", "stale socket" and "cannot tell" need different hints;
/// none of them may render as `Running`.
/// Test: `only_a_serving_socket_is_running`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SocketState {
    /// A connect completed.
    Serving,
    /// No file at the path.
    Absent,
    /// The file exists and a connect is refused.
    Stale,
    /// Something other than a socket sits at the path.
    NotASocket,
    /// The path could not be inspected; carries the OS error.
    Unreadable(String),
    /// The connect neither completed nor was refused.
    Undetermined,
}

/// Look at the socket at `path` once.
///
/// Why a thread of its own: `detect()` is synchronous and the poller runs it
/// inside `spawn_blocking`; the shared probe is async, and blocking on a
/// runtime from inside another runtime's worker panics. A current-thread
/// runtime on a fresh thread is safe from any caller (as `socket_health` does).
/// What: `symlink_metadata` decides Absent, NotASocket and Unreadable; a
/// socket file is then dialled once and the stream dropped.
/// Test: `a_stale_socket_is_observed_stale`, `a_bound_socket_is_observed_serving`.
pub(crate) fn observe_socket(path: &Path) -> SocketState {
    use std::os::unix::fs::FileTypeExt as _;

    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SocketState::Absent,
        Err(e) => return SocketState::Unreadable(e.to_string()),
        Ok(meta) if !meta.file_type().is_socket() => return SocketState::NotASocket,
        Ok(_) => {}
    }
    let path = path.to_path_buf();
    let dial = std::thread::Builder::new()
        .name("console-events-probe".to_owned())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            Some(rt.block_on(probe_socket_verdict(&path, CONNECT_TIMEOUT)))
        });
    let verdict = dial.ok().and_then(|h| h.join().ok()).flatten();
    match verdict {
        Some(SocketVerdict::Serving) => SocketState::Serving,
        Some(SocketVerdict::NotServing) => SocketState::Stale,
        _ => SocketState::Undetermined,
    }
}

/// ServiceConnector for `trusty-events`.
///
/// What: implements `detect()` — binary on PATH, then one look at the socket.
/// Test: see the module docs.
pub struct EventsConnector {
    /// Override for the socket path (tests), so a test never dials the
    /// operator's live bus.
    socket: Option<PathBuf>,
}

impl EventsConnector {
    /// Create a new `EventsConnector`.
    #[must_use]
    pub fn new() -> Self {
        Self { socket: None }
    }

    /// Create a connector that looks at `socket` instead of the resolved path.
    ///
    /// Test: `events_connector_never_reports_running_for_an_absent_socket`.
    #[must_use]
    pub fn with_socket(socket: PathBuf) -> Self {
        Self {
            socket: Some(socket),
        }
    }

    /// The socket this connector looks at, or why it could not be resolved.
    fn socket_path(&self) -> Result<PathBuf, String> {
        match &self.socket {
            Some(p) => Ok(p.clone()),
            None => trusty_common::control_bus::events_socket_path()
                .map_err(|e| format!("could not resolve the trusty-events socket path: {e:#}")),
        }
    }

    /// Build the row for one observation.
    ///
    /// Why: a pure mapping, so every state's status is assertable without a
    /// socket or the binary on PATH.
    /// What: `Serving` is `Running`; `Undetermined` is `Degraded` (something
    /// is at the path but it neither accepted nor refused); every other state
    /// is `Available` with a hint naming the fault. Never `Running` otherwise.
    /// Test: `only_a_serving_socket_is_running`.
    pub(crate) fn info_for(&self, socket: &Path, state: &SocketState) -> ServiceInfo {
        let at = socket.display();
        let (status, hint) = match state {
            SocketState::Serving => (ServiceStatus::Running, None),
            SocketState::Absent => (ServiceStatus::Available, None),
            SocketState::Stale => (
                ServiceStatus::Available,
                Some(format!(
                    "stale socket at {at}: the file exists but nothing accepts on it; \
                     launchd restarts it within 10 s"
                )),
            ),
            SocketState::NotASocket => (
                ServiceStatus::Available,
                Some(format!(
                    "{at} is not a socket; trusty-events cannot serve there"
                )),
            ),
            SocketState::Unreadable(err) => (
                ServiceStatus::Available,
                Some(format!(
                    "cannot read the trusty-events socket at {at}: {err}"
                )),
            ),
            SocketState::Undetermined => (
                ServiceStatus::Degraded,
                Some(format!(
                    "the socket at {at} neither accepted nor refused a connect; state unknown"
                )),
            ),
        };
        self.base(status, hint)
    }

    /// A row with this connector's fixed fields.
    fn base(&self, status: ServiceStatus, hint: Option<String>) -> ServiceInfo {
        ServiceInfo {
            id: self.id().to_string(),
            display_name: self.display_name().to_string(),
            status,
            // The bus answers no health method, so there is no version to read.
            version: None,
            url: None,
            hint,
            lifecycle: ServiceLifecycle::Daemon,
            cpu_pct: None,
            rss_bytes: None,
        }
    }
}

impl Default for EventsConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceConnector for EventsConnector {
    fn id(&self) -> &'static str {
        "trusty-events"
    }

    fn display_name(&self) -> &'static str {
        "Trusty Events"
    }

    /// Detect trusty-events status.
    ///
    /// What: `Absent` without the binary; `Available` with the reason when the
    /// socket path cannot be resolved; otherwise [`EventsConnector::info_for`]
    /// over one [`observe_socket`].
    /// Test: `events_connector_never_reports_running_for_an_absent_socket`.
    fn detect(&self) -> ServiceInfo {
        if !binary_on_path("trusty-events") {
            return self.base(ServiceStatus::Absent, None);
        }
        match self.socket_path() {
            Ok(socket) => self.info_for(&socket, &observe_socket(&socket)),
            Err(reason) => self.base(ServiceStatus::Available, Some(reason)),
        }
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#10042, fail-open check): the row may say `Running` only when the
    /// socket accepted a connect; every other observation is a fault or unknown.
    /// What: maps each state and asserts only `Serving` is `Running`, and each
    /// fault carries a hint.
    /// Test: this is the test.
    #[test]
    fn only_a_serving_socket_is_running() {
        let c = EventsConnector::new();
        let p = Path::new("/x/trusty-events.sock");
        assert_eq!(
            c.info_for(p, &SocketState::Serving).status,
            ServiceStatus::Running
        );
        for state in [
            SocketState::Absent,
            SocketState::Stale,
            SocketState::NotASocket,
            SocketState::Unreadable("Permission denied".to_owned()),
            SocketState::Undetermined,
        ] {
            let info = c.info_for(p, &state);
            assert_ne!(info.status, ServiceStatus::Running, "{state:?}");
            assert_eq!(info.id, "trusty-events");
            assert_eq!(info.lifecycle, ServiceLifecycle::Daemon);
            if state != SocketState::Absent {
                assert!(info.hint.is_some(), "{state:?} must say why");
            }
        }
        assert!(
            c.info_for(p, &SocketState::Stale)
                .hint
                .is_some_and(|h| h.contains("stale"))
        );
    }

    /// Why (#10042): a socket file a crashed daemon left behind must not read as
    /// a live bus.
    /// What: a socket file with nothing listening on it observes `Stale`.
    /// Test: this is the test.
    #[test]
    fn a_stale_socket_is_observed_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("trusty-events.sock");
        // #10323: bind but never listen. A dropped listener stayed open in any
        // child a sibling test forked before its exec, so the probe connected.
        let socket = tokio::net::UnixSocket::new_stream().expect("socket");
        socket.bind(&path).expect("bind");
        drop(socket);
        assert_eq!(observe_socket(&path), SocketState::Stale);
    }

    /// Why: the one healthy observation must be reachable.
    /// What: a bound socket observes `Serving`; a missing one `Absent`; a
    /// regular file `NotASocket`.
    /// Test: this is the test.
    #[test]
    fn a_bound_socket_is_observed_serving() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("trusty-events.sock");
        assert_eq!(observe_socket(&path), SocketState::Absent);
        let _listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        assert_eq!(observe_socket(&path), SocketState::Serving);
        let file = dir.path().join("plain");
        std::fs::write(&file, b"x").expect("write");
        assert_eq!(observe_socket(&file), SocketState::NotASocket);
    }

    /// Why: with no bus running, the row must never read `Running`, whether or
    /// not the binary is installed on the machine running the test.
    /// What: points the connector at a path in an empty tempdir and asserts the
    /// status, branching only on whether the binary is on PATH.
    /// Test: this is the test.
    #[test]
    fn events_connector_never_reports_running_for_an_absent_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let info = EventsConnector::with_socket(dir.path().join("absent.sock")).detect();
        let expected = if which::which("trusty-events").is_ok() {
            ServiceStatus::Available
        } else {
            ServiceStatus::Absent
        };
        assert_eq!(info.status, expected);
        assert!(info.url.is_none(), "a UDS daemon has no URL to render");
        assert!(info.version.is_none());
    }
}
