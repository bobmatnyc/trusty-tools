//! `start` and `restart`.
//!
//! Why: a start that returns once the spawn call succeeds reports success for
//! a daemon that dies a millisecond later, and a restart that starts after a
//! failed stop runs two daemons, or none (#11084, D1).
//! What: [`start`] spawns `<install dir>/<binary>` detached and waits until the
//! spawned pid itself serves the socket. [`restart`] runs [`super::stop`] and
//! starts only on `Stopped` or `NotRunning` (P-b).
//! Test: the `start_*` and `restart_*` tests in `tests/linux_lifecycle.rs`.

use tokio::time::Instant;

use super::process::{Dial, PinnedProcess, dial};
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
///    peer pid is the child is success. The deadline is `NotReady`, and the
///    child is left running with its pid in the error.
///
/// The spawned process must bind the socket itself; a daemon that forks after
/// `listen` would fail the peer-pid check.
/// Test: `restart_starts_a_daemon_that_is_not_running`,
/// `start_from_another_process_spawns_the_install_dir_binary_and_it_outlives_the_caller`,
/// `start_errors_when_the_socket_never_serves`,
/// `start_errors_when_the_child_exits_at_boot`.
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
    // Never reaped here, so an early exit stays a zombie and still pins.
    let child = PinnedProcess::open(pid)?.ok_or(LifecycleError::ChildExited { pid })?;

    let deadline = Instant::now() + desc.ready_timeout;
    loop {
        if child.has_exited() {
            return Err(LifecycleError::ChildExited { pid });
        }
        if let Dial::Peer(Some(peer)) = dial(&desc.socket).await
            && peer == pid
        {
            return Ok(StartOutcome { pid });
        }
        if Instant::now() >= deadline {
            return Err(LifecycleError::NotReady {
                pid,
                timeout: desc.ready_timeout,
            });
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
