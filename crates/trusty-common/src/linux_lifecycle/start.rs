//! `start` and `restart`.
//!
//! Why: a start that returns once the spawn call succeeds reports success for
//! a daemon that dies a millisecond later, and a restart that starts after a
//! failed stop runs two daemons, or none (#11084, D1).
//! What: [`start`] spawns `<install dir>/<binary>` detached and waits until the
//! spawned pid itself serves the socket. [`restart`] runs [`super::stop`] and
//! starts only on `Stopped` or `NotRunning` (P-b).
//! Test: the `start_*` and `restart_*` tests in `tests/linux_lifecycle.rs`.

use std::io;
use std::time::Duration;

use tokio::time::Instant;

use super::process::{Dial, PinnedProcess, Signal, dial};
use super::stop::{KernelDelivery, SignalDelivery, StopOutcome, stop_with};
use super::{DaemonDescriptor, LifecycleError, POLL_INTERVAL, StoppedSignal, lock_is_free};

/// A started daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct StartOutcome {
    /// The spawned pid, which serves the socket.
    pub pid: u32,
}

/// A restarted daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RestartOutcome {
    /// What the stop half found.
    pub stop: StopOutcome,
    /// The new process.
    pub start: StartOutcome,
}

/// Start the daemon `desc` describes and wait until it serves.
///
/// Why: the binary must be the installed one — `$PATH` or `current_exe()` can
/// name an older copy — and success must mean the new process serves.
/// What:
///
/// 1. Refuse when `<install_dir>/<binary>` is not a file, when the socket
///    already serves, or (lock signal) when the lock is held.
/// 2. Spawn it detached with `spawn_args` (new session, null stdio, no
///    inherited fds), so it outlives the caller.
/// 3. Until `ready_timeout`: an exited child is `ChildExited`; a connect whose
///    peer pid is the child is success. The deadline is `NotReady`.
///
/// Postcondition: on every error after the spawn (`ChildUnpinned`,
/// `ChildExited`, `NotReady`) the child has been sent SIGKILL and reaped; the
/// `killed` field of `ChildUnpinned` and `NotReady` says whether the reap
/// finished within `kill_wait`.
/// [`super::stop`] finds a daemon only through its socket, so a child left
/// running here could never be found again.
///
/// The spawned process must bind the socket itself; a daemon that forks after
/// `listen` would fail the peer-pid check.
/// Test: `restart_starts_a_daemon_that_is_not_running`,
/// `start_from_another_process_spawns_the_install_dir_binary_and_it_outlives_the_caller`,
/// `start_errors_when_the_socket_never_serves`,
/// `start_kills_the_child_when_its_pidfd_cannot_be_opened`,
/// `start_errors_when_the_child_exits_at_boot`,
/// `start_refuses_a_socket_that_already_serves_and_spawns_no_second_daemon`.
pub async fn start(desc: &DaemonDescriptor) -> Result<StartOutcome, LifecycleError> {
    desc.validate()?;
    let program = desc.program();
    if !program.is_file() {
        return Err(LifecycleError::BinaryMissing { path: program });
    }
    match dial(&desc.socket).await {
        Dial::NotServing => {}
        Dial::Peer(pid) => {
            return Err(LifecycleError::AlreadyRunning {
                socket: desc.socket.clone(),
                pid,
            });
        }
        Dial::Inconclusive => {
            return Err(LifecycleError::ProbeInconclusive {
                socket: desc.socket.clone(),
            });
        }
    }
    if let StoppedSignal::LockFree(lock) = &desc.stopped_signal
        && !lock_is_free(lock)?
    {
        return Err(LifecycleError::Unidentified {
            socket: desc.socket.clone(),
            lock: lock.clone(),
        });
    }

    let args: Vec<&str> = desc.spawn_args.iter().map(String::as_str).collect();
    let pid = crate::daemon_guard::spawn_detached(&program, &args).map_err(|e| {
        LifecycleError::Spawn {
            program: program.clone(),
            message: e.to_string(),
        }
    })?;
    // Not reaped until it fails, so an early exit stays a zombie and still pins.
    let child = match PinnedProcess::open(pid) {
        Ok(Some(child)) => child,
        // Only a reaped child is missing: something else reaped it.
        Ok(None) => return Err(LifecycleError::ChildExited { pid }),
        // #11084: without a pidfd the child is unfindable; kill it via its pid.
        Err(e) => {
            let source = match e {
                LifecycleError::Io { source, .. } => source,
                other => io::Error::other(other.to_string()),
            };
            let killed = kill_and_reap(pid, None, desc.kill_wait).await;
            return Err(LifecycleError::ChildUnpinned {
                pid,
                killed,
                source,
            });
        }
    };

    let deadline = Instant::now() + desc.ready_timeout;
    loop {
        if child.has_exited() {
            kill_and_reap(pid, Some(&child), desc.kill_wait).await;
            return Err(LifecycleError::ChildExited { pid });
        }
        if let Dial::Peer(Some(peer)) = dial(&desc.socket).await
            && peer == pid
        {
            return Ok(StartOutcome { pid });
        }
        if Instant::now() >= deadline {
            // #11084: a child that never served is killed, not left to pile up.
            let killed = kill_and_reap(pid, Some(&child), desc.kill_wait).await;
            return Err(LifecycleError::NotReady {
                pid,
                timeout: desc.ready_timeout,
                killed,
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// SIGKILL a child `start` spawned and reap it; true once it is reaped.
///
/// Why: a failed start must not leave a process that `stop` cannot find
/// (#11084). What: the signal goes through `pinned` when there is a pidfd and
/// through `kill(2)` otherwise, which is safe because the child is unreaped,
/// so its pid cannot be reused. Then `waitpid(WNOHANG)` is polled until the
/// child is reaped or `limit` passes. `ECHILD` (another reaper took it, as
/// with SIGCHLD ignored) counts as reaped.
/// Test: `start_errors_when_the_socket_never_serves`,
/// `start_kills_the_child_when_its_pidfd_cannot_be_opened`.
async fn kill_and_reap(pid: u32, pinned: Option<&PinnedProcess>, limit: Duration) -> bool {
    let Ok(raw) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // A failed signal leaves the reap wait to report `false`.
    match pinned {
        Some(child) => {
            let _ = child.send_signal(Signal::Kill);
        }
        // SAFETY: kill(2) on this process's own unreaped child.
        None => unsafe {
            libc::kill(raw, libc::SIGKILL);
        },
    }
    let deadline = Instant::now() + limit;
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: waitpid on our own child with a valid status pointer.
        let rc = unsafe { libc::waitpid(raw, &raw mut status, libc::WNOHANG) };
        if rc == raw {
            return true;
        }
        if rc < 0 {
            match io::Error::last_os_error().raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::ECHILD) => return true,
                _ => return false,
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Stop, make sure it stopped, and only then start (D1).
///
/// What: [`restart_with`] with [`KernelDelivery`].
/// Test: `restart_starts_a_daemon_that_is_not_running`.
pub async fn restart(desc: &DaemonDescriptor) -> Result<RestartOutcome, LifecycleError> {
    restart_with(desc, &KernelDelivery).await
}

/// [`restart`] with an explicit signal delivery (the test seam).
///
/// What: `NotRunning` from stop proceeds to start (P-b); every stop error is
/// returned as is, before any spawn.
/// Test: `restart_does_not_start_when_stop_reports_a_survivor`.
pub async fn restart_with(
    desc: &DaemonDescriptor,
    delivery: &dyn SignalDelivery,
) -> Result<RestartOutcome, LifecycleError> {
    let stop = stop_with(desc, delivery).await?;
    let start = start(desc).await?;
    Ok(RestartOutcome { stop, start })
}
