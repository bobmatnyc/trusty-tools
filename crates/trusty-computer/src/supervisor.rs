//! Owns one bounded cua-driver daemon on a private socket.
//!
//! Why: the broker must start, stop and restart its own daemon, refuse any
//! driver other than the pinned one, rescope it per lease, and recover from
//! an ended session, without ever touching the user's default daemon (plan
//! section 3; spike `driver.py` `DriverSupervisor`).
//! What: [`Supervisor`] lives in `<state>/driver/` (0700): socket
//! `cua-driver.sock`, manifest `manifest.yaml`. The daemon is identified by
//! that socket plus the pid its metadata reports, never by process name.
//! [`Supervisor::call`] injects the supervisor's session id; a session-ended
//! refusal starts the session again and retries once for a read-only call,
//! and returns [`DriverError::SessionRestarted`] for a mutating one.
//! Test: `tests/spike_supervisor.rs`, `tests/supervisor_session.rs`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::driver::{DriverClient, DriverError};
use crate::launcher::{DRIVER_VERSION, Launcher, is_default_socket};
use crate::manifest::{self, LeaseScope, Lifetimes};
use crate::private;

/// Tools the supervisor may retry after a session restart: they change nothing.
pub const READ_ONLY_TOOLS: [&str; 6] = [
    "get_window_state",
    "list_windows",
    "list_apps",
    "verify_state",
    "check_permissions",
    "get_session",
];

/// One supervised daemon.
pub struct Supervisor<L: Launcher> {
    launcher: L,
    socket: PathBuf,
    manifest: PathBuf,
    scope: Option<LeaseScope>,
    lifetimes: Lifetimes,
    pinned: String,
    ready_timeout: Duration,
    request_timeout: Duration,
    session: String,
    pid: Option<u32>,
    restarts: u32,
}

impl<L: Launcher> Supervisor<L> {
    /// A supervisor whose files live in `<state>/driver/` (created 0700).
    ///
    /// Refuses a socket path that is a default daemon socket.
    pub fn new(launcher: L, state: &Path) -> Result<Supervisor<L>, DriverError> {
        let dir = state.join("driver");
        private::ensure_private_dir(&dir)
            .map_err(|e| DriverError::Lifecycle(format!("{}: {e}", dir.display())))?;
        let socket = dir.join("cua-driver.sock");
        if is_default_socket(&socket) {
            return Err(DriverError::Lifecycle(format!(
                "refusing the default daemon socket {}",
                socket.display()
            )));
        }
        Ok(Supervisor {
            launcher,
            manifest: dir.join("manifest.yaml"),
            socket,
            scope: None,
            lifetimes: Lifetimes::default(),
            pinned: DRIVER_VERSION.to_owned(),
            ready_timeout: Duration::from_secs(15),
            request_timeout: crate::driver::DEFAULT_TIMEOUT,
            session: format!("trusty-computer-{}", std::process::id()),
            pid: None,
            restarts: 0,
        })
    }

    /// Set how long start and stop wait for the daemon.
    pub fn with_ready_timeout(mut self, timeout: Duration) -> Self {
        self.ready_timeout = timeout;
        self
    }

    /// Set the manifest lifetimes.
    pub fn with_lifetimes(mut self, lifetimes: Lifetimes) -> Self {
        self.lifetimes = lifetimes;
        self
    }

    /// The launcher (tests read its counters).
    pub fn launcher(&self) -> &L {
        &self.launcher
    }
    /// The launcher, mutably (tests set failure knobs).
    pub fn launcher_mut(&mut self) -> &mut L {
        &mut self.launcher
    }
    /// The private socket.
    pub fn socket(&self) -> &Path {
        &self.socket
    }
    /// The manifest path.
    pub fn manifest_path(&self) -> &Path {
        &self.manifest
    }
    /// The live scope, if started.
    pub fn scope(&self) -> Option<LeaseScope> {
        self.scope
    }
    /// The daemon pid from its metadata, while running.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
    /// Restarts since construction.
    pub fn restarts(&self) -> u32 {
        self.restarts
    }
    /// The session id injected into every call.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// A client for the private socket.
    pub fn client(&self) -> DriverClient {
        DriverClient::with_timeout(&self.socket, self.request_timeout)
    }

    /// The daemon's version, or `None` when nothing answers on the socket.
    pub fn running_version(&self) -> Option<String> {
        let meta = self.client().metadata().ok()?;
        meta.get("driver_version")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// Start the daemon for `scope` and open the session; idempotent while it runs.
    ///
    /// Why: every start re-checks the pin, so an upgraded app is never driven.
    /// What: refuses unless the installed binary is the pinned version, writes
    /// the manifest, launches, waits for metadata, refuses (and stops) a daemon
    /// reporting another version, chmods the socket 0600, then `start_session`.
    /// Postcondition on Ok: `pid()` is the metadata pid and the socket answers.
    /// Postcondition on Err: `pid()` and `scope()` are `None`, so no later
    /// rescope mistakes an unverified daemon for a running one (#11460).
    /// Test: `supervisor_start_stop_restart`, `supervisor_refuses_unpinned_binary`,
    /// `supervisor_stops_daemon_reporting_wrong_version`, `supervisor_start_is_idempotent_when_running`,
    /// `failed_start_clears_pid_and_scope`.
    pub fn start(&mut self, scope: LeaseScope) -> Result<Value, DriverError> {
        let result = self.start_checked(scope);
        if result.is_err() {
            self.pid = None;
            self.scope = None;
        }
        result
    }

    fn start_checked(&mut self, scope: LeaseScope) -> Result<Value, DriverError> {
        let binary = self.launcher.binary_version()?;
        if binary != self.pinned {
            return Err(DriverError::Version(format!(
                "installed cua-driver {binary} != pinned {}",
                self.pinned
            )));
        }
        if self.running_version().is_none() {
            manifest::write(&self.manifest, scope, &self.lifetimes)
                .map_err(|e| DriverError::Lifecycle(format!("manifest: {e}")))?;
            self.launcher.start(&self.socket, &self.manifest)?;
            self.scope = Some(scope);
        } else if self.scope != Some(scope) {
            return Err(DriverError::Lifecycle(
                "a daemon already serves another scope; use rescope".into(),
            ));
        }
        let meta = self.wait_ready()?;
        let pid = meta
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok());
        self.pid = pid;
        if !pid.is_some_and(|p| self.launcher.owns(p, &self.socket)) {
            return Err(DriverError::Lifecycle(format!(
                "pid {pid:?} on {} is not a daemon serving that socket",
                self.socket.display()
            )));
        }
        let version = meta.get("driver_version").and_then(Value::as_str);
        if version != Some(self.pinned.as_str()) {
            self.stop()?;
            return Err(DriverError::Version(format!(
                "daemon reports {} != pinned {}",
                version.unwrap_or("none"),
                self.pinned
            )));
        }
        set_mode_0600(&self.socket);
        self.start_session()?;
        Ok(Value::Object(meta))
    }

    fn wait_ready(&self) -> Result<serde_json::Map<String, Value>, DriverError> {
        let deadline = Instant::now() + self.ready_timeout;
        loop {
            match self.client().metadata() {
                Ok(meta) => return Ok(meta),
                Err(e) if Instant::now() > deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    /// Stop the daemon and wait until the socket no longer answers.
    pub fn stop(&mut self) -> Result<(), DriverError> {
        self.launcher.stop(&self.socket, self.pid)?;
        let deadline = Instant::now() + self.ready_timeout;
        while self.running_version().is_some() {
            if Instant::now() > deadline {
                return Err(DriverError::Lifecycle("daemon did not stop".into()));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.pid = None;
        Ok(())
    }

    /// Stop, then start for `scope`.
    pub fn restart(&mut self, scope: LeaseScope) -> Result<Value, DriverError> {
        self.stop()?;
        self.restarts += 1;
        self.start(scope)
    }

    /// Move the daemon to `scope`; restarts only when the scope differs.
    ///
    /// Returns the restart's wall time, or `None` when nothing changed.
    /// Test: `rescope_restarts_only_on_a_scope_change`.
    pub fn rescope(&mut self, scope: LeaseScope) -> Result<Option<Duration>, DriverError> {
        if self.scope == Some(scope) && self.pid.is_some() {
            return Ok(None);
        }
        let started = Instant::now();
        self.restart(scope)?;
        Ok(Some(started.elapsed()))
    }

    /// `start_session` for this supervisor's session id.
    pub fn start_session(&self) -> Result<Value, DriverError> {
        self.client()
            .call("start_session", &json!({"session": self.session}))
    }

    /// Call `tool` in the supervisor's session.
    ///
    /// Why: an ended session refuses every call until `start_session`
    /// (GO-STOP.md:129), and a mutating call must never run twice (plan section 4).
    /// What: injects `session`; on [`DriverError::SessionEnded`] starts the
    /// session again, then retries once when `tool` is in [`READ_ONLY_TOOLS`],
    /// else returns [`DriverError::SessionRestarted`].
    /// Test: `read_only_call_retries_once_after_session_ended`,
    /// `mutating_call_is_not_rerun_after_session_ended`.
    pub fn call(&self, tool: &str, args: &Value) -> Result<Value, DriverError> {
        let mut args = args.clone();
        if let Value::Object(map) = &mut args {
            map.insert("session".into(), json!(self.session));
        }
        match self.client().call(tool, &args) {
            Err(DriverError::SessionEnded { .. }) => {
                self.start_session()?;
                if READ_ONLY_TOOLS.contains(&tool) {
                    self.client().call(tool, &args)
                } else {
                    Err(DriverError::SessionRestarted {
                        tool: tool.to_owned(),
                    })
                }
            }
            other => other,
        }
    }
}

fn set_mode_0600(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    // Best effort: the 0700 parent already keeps other users out.
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
