//! Linux start / stop / restart for a trusty daemon, driven by a descriptor
//! (#11084, epic #11083 PHASE_1).
//!
//! Why: on Linux, `tctl restart` had no backend — every daemon except tm was
//! routed to launchd and failed, so daemons kept running binaries an upgrade had
//! already replaced. The existing per-daemon stop paths each had a defect: one
//! waited a fixed 3 s on health and never sent SIGKILL, one exited 0 while a
//! survivor remained, one signalled a pid it never identified. This module is
//! the one Linux backend those daemons move onto in PHASE_2, so the stop and
//! identity rules are written, and tested against real processes, once.
//!
//! What: every daemon detail comes from a [`DaemonDescriptor`] the caller
//! builds; nothing here names a daemon.
//!
//! - [`stop`] identifies the listener by the `SO_PEERCRED` peer pid of the
//!   descriptor's socket, pins it with a pidfd, checks its executable name,
//!   sends SIGTERM, waits for the pid to exit, sends SIGKILL after the stop
//!   grace, and then waits for the descriptor's [`StoppedSignal`]. It returns
//!   [`StopOutcome::Stopped`], [`StopOutcome::NotRunning`], or an error —
//!   [`LifecycleError::Survivor`] when the pid outlives SIGKILL.
//!   A socket that does not serve is `NotRunning` only when no process of
//!   the caller's uid runs `binary` with every one of the descriptor's
//!   `liveness_args` (#11098); otherwise it is
//!   [`LifecycleError::RunningUnserved`] and nothing is signalled.
//! - [`start`] spawns `<install dir>/<binary>` detached, in the descriptor's
//!   `working_dir` and with only [`DaemonDescriptor::BASE_ENV`] plus its
//!   `env_passthrough` names from the caller's environment (#11098), and
//!   returns only once the spawned pid serves the socket. A failed start kills
//!   and reaps the child it spawned, so nothing it started is left running.
//! - [`restart`] is [`stop`], then [`start`]; `NotRunning` counts as stopped,
//!   and any stop error returns before a spawn.
//!
//! Waits are condition waits: pid exit is a pidfd becoming readable, and the
//! stopped signal and readiness are polled conditions with a deadline. None of
//! them is a fixed sleep standing in for proof.
//!
//! Precondition: every async function here runs on a tokio runtime with IO and
//! time enabled, as every `uds::` function does.
//!
//! Test: `tests/linux_lifecycle.rs` runs each path against real child
//! processes; the unit tests are in `tests.rs`.
//!
//! [`DaemonDescriptor`]: crate::linux_lifecycle::DaemonDescriptor
//! [`StoppedSignal`]: crate::linux_lifecycle::StoppedSignal
//! [`StopOutcome::Stopped`]: crate::linux_lifecycle::StopOutcome::Stopped
//! [`StopOutcome::NotRunning`]: crate::linux_lifecycle::StopOutcome::NotRunning
//! [`LifecycleError::Survivor`]: crate::linux_lifecycle::LifecycleError::Survivor
//! [`LifecycleError::RunningUnserved`]: crate::linux_lifecycle::LifecycleError::RunningUnserved
//! [`DaemonDescriptor::BASE_ENV`]: crate::linux_lifecycle::DaemonDescriptor::BASE_ENV
//! [`stop`]: crate::linux_lifecycle::stop()
//! [`start`]: crate::linux_lifecycle::start()
//! [`restart`]: crate::linux_lifecycle::restart()

mod liveness;
mod process;
mod start;
mod stop;
#[cfg(test)]
mod tests;

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use process::{PinnedProcess, Signal};
pub use start::{RestartOutcome, StartOutcome, restart, restart_with, start};
pub use stop::{KernelDelivery, SignalDelivery, StopOutcome, stop, stop_with};

/// How long one connect attempt may take before the answer is "inconclusive".
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Interval between checks of a polled condition (stopped signal, readiness).
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// What must hold, after the pid has exited, before `stop` reports `Stopped`.
///
/// Why: a daemon's socket can close before its pid exits, and a lock can be
/// held after it — so "stopped" is a per-daemon fact the caller names, never
/// something this module guesses. `bind_singleton`'s `<socket>.lock` is held
/// only while binding, so it is deliberately not offered as a signal.
/// What: [`StoppedSignal::SocketRefuses`] — the socket file is absent or
/// refuses connections. [`StoppedSignal::LockFree`] — an exclusive `flock` on
/// the named file can be taken (an absent file counts as free).
/// Test: `stop_returns_stopped_promptly_after_a_clean_exit`,
/// `stop_with_lock_signal_is_not_stopped_while_another_process_holds_the_lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StoppedSignal {
    /// The pid has exited and the socket refuses connections.
    SocketRefuses,
    /// The pid has exited and this lock file's `flock` is free.
    LockFree(PathBuf),
}

/// Everything the lifecycle needs to know about one daemon.
///
/// Why: the module must not assume any daemon (P-a keeps tm off it; the other
/// daemons differ in socket, lock and args), so every detail is caller data.
/// The caller resolves its data dir and derives the socket path, lock path and
/// spawn args from it; this type carries the resolved values.
/// What: `binary` is both the file name under `install_dir` that [`start`]
/// spawns and the executable name [`stop`] requires of the socket's peer.
/// `stop_grace` bounds the SIGTERM wait, `kill_wait` bounds the SIGKILL wait
/// and the stopped-signal wait, and `ready_timeout` bounds [`start`].
/// `liveness_args` tells the daemon from a stdio bridge of the same binary
/// when the socket does not answer; `working_dir` and `env_passthrough` shape
/// the spawned process (#11098).
/// `#[non_exhaustive]`: build it with [`DaemonDescriptor::new`] or
/// [`DaemonDescriptor::with_install_dir`], then assign the public fields.
/// Test: `descriptor_rejects_a_binary_name_with_a_path_separator`,
/// `descriptor_new_uses_the_canonical_bin_dir`,
/// `descriptor_rejects_bad_spawn_and_liveness_fields`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DaemonDescriptor {
    /// File name of the daemon binary; never a path.
    pub binary: String,
    /// Directory the binary is spawned from.
    pub install_dir: PathBuf,
    /// The daemon's Unix-domain socket.
    pub socket: PathBuf,
    /// Arguments passed to the binary on spawn.
    pub spawn_args: Vec<String>,
    /// Which condition proves the daemon stopped.
    pub stopped_signal: StoppedSignal,
    /// SIGTERM-to-SIGKILL grace.
    pub stop_grace: Duration,
    /// Bound on the post-SIGKILL exit wait and on the stopped-signal wait.
    pub kill_wait: Duration,
    /// Bound on `start`'s readiness wait.
    pub ready_timeout: Duration,
    /// Arguments only the daemon carries (#11098).
    ///
    /// When the socket does not serve, a live process of the caller's uid
    /// whose executable is `binary` and whose argv (after `argv[0]`) holds
    /// every one of these as a whole argument means the daemon still runs:
    /// `stop` and `start` refuse with [`LifecycleError::RunningUnserved`].
    /// Empty (the default) counts every process of `binary`, stdio bridges
    /// included, which fails closed; a consumer sets the daemon-only
    /// arguments, such as `["serve", "--foreground"]`.
    pub liveness_args: Vec<String>,
    /// The spawned daemon's working directory; `/` by default (#11098).
    ///
    /// Never the caller's cwd, which can hold a `.env.local` the daemon would
    /// load (#9161). Must be absolute.
    pub working_dir: PathBuf,
    /// Names copied from the caller's environment, when set, on top of
    /// [`DaemonDescriptor::BASE_ENV`]; nothing else reaches the daemon (#11098).
    pub env_passthrough: Vec<String>,
}

impl DaemonDescriptor {
    /// Default SIGTERM grace.
    pub const DEFAULT_STOP_GRACE: Duration = Duration::from_secs(10);
    /// Default post-SIGKILL wait.
    pub const DEFAULT_KILL_WAIT: Duration = Duration::from_secs(5);
    /// Default readiness bound.
    pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(30);

    /// The variables every spawned daemon gets from the caller, when set.
    ///
    /// Why: #11098 — the daemon must not inherit the caller's environment
    /// wholesale (tokens, API keys, a parent-death stamp), but it needs its
    /// home, search path, locale and XDG dirs to find its data.
    /// `TRUSTY_DATA_DIR_OVERRIDE` and `TRUSTY_SANDBOX` are here because
    /// dropping them would move a sandboxed caller's daemon onto the real data
    /// dir and the real `.env.local` tiers. No other `TRUSTY_*` name is: some
    /// carry credentials. A daemon-specific variable goes in `env_passthrough`.
    /// Test: `daemon_env_carries_the_base_and_the_allowlist_only`.
    pub const BASE_ENV: &'static [&'static str] = &[
        "HOME",
        "USER",
        "LOGNAME",
        "PATH",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "LC_MESSAGES",
        "TZ",
        "TMPDIR",
        "XDG_RUNTIME_DIR",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        crate::data_dir::DATA_DIR_OVERRIDE_ENV,
        // The `.env.local` loader's opt-out (`credentials::dotenv::SANDBOX_ENV_VAR`).
        "TRUSTY_SANDBOX",
    ];

    /// A descriptor whose install dir is [`crate::bin_resolve::canonical_bin_dir`].
    ///
    /// Why: the install dir is where `cargo install` and `tctl upgrade` put the
    /// binary; spawning from it, never `$PATH` or `current_exe()`, is what makes
    /// a restart run the new binary.
    /// What: fails with [`LifecycleError::NoInstallDir`] when neither
    /// `CARGO_HOME` nor a home directory resolves, and with
    /// [`LifecycleError::InvalidDescriptor`] for a bad binary name.
    /// Test: `descriptor_new_uses_the_canonical_bin_dir`.
    pub fn new(
        binary: impl Into<String>,
        socket: impl Into<PathBuf>,
        stopped_signal: StoppedSignal,
    ) -> Result<Self, LifecycleError> {
        let install_dir =
            crate::bin_resolve::canonical_bin_dir().ok_or(LifecycleError::NoInstallDir)?;
        Self::with_install_dir(binary, install_dir, socket, stopped_signal)
    }

    /// A descriptor with an explicit install dir.
    ///
    /// Test: `descriptor_rejects_a_binary_name_with_a_path_separator`.
    pub fn with_install_dir(
        binary: impl Into<String>,
        install_dir: impl Into<PathBuf>,
        socket: impl Into<PathBuf>,
        stopped_signal: StoppedSignal,
    ) -> Result<Self, LifecycleError> {
        let desc = Self {
            binary: binary.into(),
            install_dir: install_dir.into(),
            socket: socket.into(),
            spawn_args: Vec::new(),
            stopped_signal,
            stop_grace: Self::DEFAULT_STOP_GRACE,
            kill_wait: Self::DEFAULT_KILL_WAIT,
            ready_timeout: Self::DEFAULT_READY_TIMEOUT,
            liveness_args: Vec::new(),
            working_dir: PathBuf::from("/"),
            env_passthrough: Vec::new(),
        };
        desc.validate()?;
        Ok(desc)
    }

    /// The absolute program [`start`] spawns: `<install_dir>/<binary>`.
    pub fn program(&self) -> PathBuf {
        self.install_dir.join(&self.binary)
    }

    /// Re-checked at every entry point, because the fields are public.
    ///
    /// Test: `descriptor_rejects_a_binary_name_with_a_path_separator`.
    pub(crate) fn validate(&self) -> Result<(), LifecycleError> {
        let bad = |reason: &str| {
            Err(LifecycleError::InvalidDescriptor {
                reason: reason.to_string(),
            })
        };
        if self.binary.is_empty() || self.binary == "." || self.binary == ".." {
            return bad("binary name is empty or a dot entry");
        }
        if self.binary.contains('/') {
            return bad("binary must be a file name, not a path");
        }
        if !self.install_dir.is_absolute() {
            return bad("install_dir must be absolute");
        }
        if self.socket.as_os_str().is_empty() {
            return bad("socket path is empty");
        }
        // #11098: a relative cwd would resolve against the caller's cwd.
        if !self.working_dir.is_absolute() {
            return bad("working_dir must be absolute");
        }
        // No argv element holds a NUL, so such an arg would never match and a
        // running daemon would read as not running.
        if self.liveness_args.iter().any(|a| a.contains('\0')) {
            return bad("liveness_args must not contain NUL");
        }
        if self
            .env_passthrough
            .iter()
            .any(|n| n.is_empty() || n.contains('=') || n.contains('\0'))
        {
            return bad("env_passthrough names must be non-empty, without '=' or NUL");
        }
        Ok(())
    }
}

/// Every way a lifecycle call fails.
///
/// Why: a caller (a CLI verb, tctl) must tell "something is still running" from
/// "I could not decide" from "the new process never came up", and report the
/// pid involved. Each is its own variant, never a success.
/// Test: `tests/linux_lifecycle.rs` has a real-process test for
/// `ProbeInconclusive`, `Unidentified`, `RunningUnserved`, `IdentityMismatch`,
/// `IdentityUnreadable`, `Survivor`, `StoppedSignalNotReached`,
/// `AlreadyRunning`, `ChildExited`, `NotReady` and `ChildUnpinned`; the unit
/// tests in `tests.rs` cover `InvalidDescriptor` and `NoInstallDir`.
/// `NoPeerPid`, `Signal`, `BinaryMissing`, `Spawn` and `Io` have no test.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LifecycleError {
    /// The descriptor cannot be used.
    #[error("invalid daemon descriptor: {reason}")]
    InvalidDescriptor {
        /// What is wrong.
        reason: String,
    },
    /// Neither `CARGO_HOME` nor a home directory resolves.
    #[error("no install directory: neither CARGO_HOME nor a home directory is set")]
    NoInstallDir,
    /// A connect neither succeeded nor proved the socket dead.
    #[error("cannot tell whether {} is served (connect timed out or failed)", socket.display())]
    ProbeInconclusive {
        /// The socket probed.
        socket: PathBuf,
    },
    /// The socket serves, but the kernel reported no peer pid.
    #[error("{} is served, but SO_PEERCRED reported no peer pid", socket.display())]
    NoPeerPid {
        /// The socket probed.
        socket: PathBuf,
    },
    /// The socket does not serve, yet the stop lock is held by someone.
    #[error(
        "{} is not served but {} is locked; the holder cannot be identified",
        socket.display(),
        lock.display()
    )]
    Unidentified {
        /// The socket probed.
        socket: PathBuf,
        /// The held lock.
        lock: PathBuf,
    },
    /// The socket does not serve, yet a process runs the daemon (#11098).
    ///
    /// Its socket was unlinked or stopped answering, so `SO_PEERCRED` cannot
    /// pin it: nothing was signalled, and `restart` starts nothing.
    #[error(
        "{} is not served, but pid(s) {} run `{binary}` with the daemon's arguments; not signalled",
        socket.display(),
        pid_list(pids)
    )]
    RunningUnserved {
        /// The socket probed.
        socket: PathBuf,
        /// The descriptor's binary name.
        binary: String,
        /// The matching pids, ascending.
        pids: Vec<u32>,
    },
    /// The socket's peer is not the descriptor's binary; no signal was sent.
    #[error("pid {pid} serves the socket but runs `{found}`, not `{expected}`; not signalled")]
    IdentityMismatch {
        /// The peer pid.
        pid: u32,
        /// The descriptor's binary name.
        expected: String,
        /// The peer's executable name.
        found: String,
    },
    /// `/proc/<pid>/exe` could not be read; no signal was sent.
    #[error("cannot read the executable of pid {pid}: {source}")]
    IdentityUnreadable {
        /// The peer pid.
        pid: u32,
        /// The read error.
        source: io::Error,
    },
    /// Delivering a signal failed for a reason other than the pid exiting.
    #[error("sending {signal:?} to pid {pid} failed: {source}")]
    Signal {
        /// The target pid.
        pid: u32,
        /// The signal.
        signal: Signal,
        /// The kernel error.
        source: io::Error,
    },
    /// The pid still exists after SIGKILL and the kill wait.
    #[error("pid {pid} survived SIGKILL; the daemon is not stopped")]
    Survivor {
        /// The surviving pid.
        pid: u32,
    },
    /// The pid exited, but the stopped signal did not hold within the kill wait.
    #[error("pid {pid} exited, but {signal} still does not hold")]
    StoppedSignalNotReached {
        /// The exited pid.
        pid: u32,
        /// A description of the unmet signal.
        signal: String,
    },
    /// `<install_dir>/<binary>` is not a file.
    #[error("daemon binary {} is not installed", path.display())]
    BinaryMissing {
        /// The program path.
        path: PathBuf,
    },
    /// `start` found the socket already served.
    #[error("{} is already served{}", socket.display(), pid.map(|p| format!(" by pid {p}")).unwrap_or_default())]
    AlreadyRunning {
        /// The socket.
        socket: PathBuf,
        /// The serving pid, when the kernel reported one.
        pid: Option<u32>,
    },
    /// The spawn itself failed.
    #[error("could not spawn {}: {message}", program.display())]
    Spawn {
        /// The program path.
        program: PathBuf,
        /// The rendered spawn error.
        message: String,
    },
    /// The spawned child exited before it served the socket.
    #[error("pid {pid} exited before serving the socket")]
    ChildExited {
        /// The spawned pid.
        pid: u32,
    },
    /// The spawned child did not serve the socket within the readiness bound.
    // #11084: the child is killed and reaped, never left running unfindable.
    #[error("pid {pid} did not serve the socket within {timeout:?}; {}", reap_note(*killed))]
    NotReady {
        /// The spawned pid.
        pid: u32,
        /// The readiness bound.
        timeout: Duration,
        /// Whether the child was killed and reaped within the kill wait.
        killed: bool,
    },
    /// The child was spawned, but no pidfd could be opened for it.
    // #11084: the child is killed and reaped rather than left running.
    #[error("could not pin spawned pid {pid} with a pidfd ({source}); {}", reap_note(*killed))]
    ChildUnpinned {
        /// The spawned pid.
        pid: u32,
        /// Whether the child was killed and reaped within the kill wait.
        killed: bool,
        /// The `pidfd_open` error.
        source: io::Error,
    },
    /// An OS call outside the cases above failed.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// The error.
        source: io::Error,
    },
}

/// `1, 2, 3`, for an error message.
fn pid_list(pids: &[u32]) -> String {
    let list: Vec<String> = pids.iter().map(u32::to_string).collect();
    list.join(", ")
}

/// The tail of a failed-start message: what became of the spawned child.
fn reap_note(killed: bool) -> &'static str {
    if killed {
        "it was killed and reaped"
    } else {
        "it was sent SIGKILL but not reaped within the kill wait"
    }
}

/// Whether the stop signal's lock file can be locked right now.
///
/// What: opens the file read-only (absent is free) and tries an exclusive,
/// non-blocking `flock`, releasing it at once.
/// Test: `stop_with_lock_signal_returns_stopped_once_the_lock_is_released`.
pub(crate) fn lock_is_free(lock: &Path) -> Result<bool, LifecycleError> {
    let file = match std::fs::File::open(lock) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(source) => {
            return Err(LifecycleError::Io {
                context: format!("open lock {}", lock.display()),
                source,
            });
        }
    };
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(source)) => Err(LifecycleError::Io {
            context: format!("flock {}", lock.display()),
            source,
        }),
    }
}
