//! [`LifecycleError`]: every way a lifecycle call fails.
//!
//! Test: see the type docs.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use super::Signal;

/// Every way a lifecycle call fails.
///
/// Why: a caller (a CLI verb, tctl) must tell "something is still running" from
/// "I could not decide" from "the new process never came up", and report the
/// pid involved. Each is its own variant, never a success.
/// Test: `tests/linux_lifecycle.rs` has a real-process test for
/// `ProbeInconclusive`, `Unidentified`, `RunningUnserved`, `IdentityMismatch`,
/// `IdentityUnreadable`, `InstanceUnreadable`, `Survivor`, `StoppedSignalNotReached`,
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
    /// A process runs the daemon's binary and arguments, but its environment
    /// cannot be read, so whether it is this instance is unknown (#11098).
    ///
    /// Nothing was signalled, and `restart` starts nothing.
    #[error(
        "pid {pid} runs `{binary}` with the daemon's arguments, but its environment cannot be \
         read ({source}); cannot tell whether it is this instance; not signalled"
    )]
    InstanceUnreadable {
        /// The matching pid.
        pid: u32,
        /// The descriptor's binary name.
        binary: String,
        /// The read error.
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
