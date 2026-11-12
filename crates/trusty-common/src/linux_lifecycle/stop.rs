//! `stop`: identify the socket's listener, signal it, and prove it stopped.
//!
//! Why: the stop paths this replaces declared success on a fixed health wait,
//! on a signal's return code, or on a pid they never checked (#11084). Here
//! success has one meaning: the identified pid exited AND the descriptor's
//! stopped signal holds.
//! What: [`stop`], [`stop_with`], [`StopOutcome`], and the [`SignalDelivery`]
//! seam with its production [`KernelDelivery`].
//! Test: the `stop_*` tests in `tests/linux_lifecycle.rs`.

use std::io;

use tokio::time::Instant;

use super::process::{Dial, PinnedProcess, Signal, dial, exe_name};
use super::{
    DaemonDescriptor, LifecycleError, POLL_INTERVAL, PROBE_TIMEOUT, StoppedSignal, lock_is_free,
};
use crate::uds::{SocketVerdict, probe_socket_verdict};

/// What a successful `stop` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StopOutcome {
    /// The identified pid exited and the stopped signal holds.
    Stopped {
        /// The pid that was stopped.
        pid: u32,
        /// Whether SIGKILL was needed.
        forced: bool,
    },
    /// Nothing served the socket, and no signal was sent.
    NotRunning,
}

/// How `stop` delivers a signal to the pinned process.
///
/// Why: a process that survives SIGKILL cannot be produced by an unprivileged
/// test — the kernel always delivers it. Routing delivery through this trait
/// lets a test drop SIGKILL against a real, running process, so the survivor
/// path is exercised end to end rather than asserted in isolation. Production
/// code uses [`stop`], which passes [`KernelDelivery`].
/// What: one call per signal. An `ESRCH` error means the process had already
/// exited; the exit wait that follows decides the outcome.
/// Test: `stop_reports_a_survivor_when_sigkill_does_not_take`,
/// `restart_does_not_start_when_stop_reports_a_survivor`.
pub trait SignalDelivery: Send + Sync {
    /// Deliver `signal` to `target`.
    fn deliver(&self, target: &PinnedProcess, signal: Signal) -> io::Result<()>;
}

/// Delivers every signal through the target's pidfd.
#[derive(Debug, Clone, Copy, Default)]
pub struct KernelDelivery;

impl SignalDelivery for KernelDelivery {
    fn deliver(&self, target: &PinnedProcess, signal: Signal) -> io::Result<()> {
        target.send_signal(signal)
    }
}

/// Stop the daemon `desc` describes, delivering signals through the kernel.
///
/// Why: see the module docs; this is the production entry point.
/// What: [`stop_with`] with [`KernelDelivery`].
/// Test: `stop_returns_stopped_promptly_after_a_clean_exit`,
/// `stop_waits_for_pid_exit_not_socket_close`,
/// `stop_sends_sigkill_to_a_process_that_ignores_sigterm`.
pub async fn stop(desc: &DaemonDescriptor) -> Result<StopOutcome, LifecycleError> {
    stop_with(desc, &KernelDelivery).await
}

/// Stop the daemon `desc` describes, delivering signals through `delivery`.
///
/// Why: D1 — a stop that has not proven the process gone is not a stop.
/// What, in order:
///
/// 1. Dial the socket. Absent or refusing: `NotRunning`, no signal (for a
///    lock signal, only when the lock is also free). Undecided: an error.
/// 2. Pin the peer pid with a pidfd and require `/proc/<pid>/exe` to name
///    `desc.binary`. A mismatch is an error and no signal is sent.
/// 3. SIGTERM; wait up to `stop_grace` for the pid to exit. If it has not,
///    SIGKILL and wait up to `kill_wait`. Still there: `Survivor`.
/// 4. Wait up to `kill_wait` for the stopped signal; unmet is an error.
///
/// Test: `stop_returns_not_running_when_socket_is_absent`,
/// `stop_returns_not_running_when_socket_refuses_connections`,
/// `stop_refuses_to_signal_a_process_with_another_name`,
/// `stop_reports_a_survivor_when_sigkill_does_not_take`,
/// `stop_with_lock_signal_is_not_stopped_while_another_process_holds_the_lock`.
pub async fn stop_with(
    desc: &DaemonDescriptor,
    delivery: &dyn SignalDelivery,
) -> Result<StopOutcome, LifecycleError> {
    desc.validate()?;
    let pid = match dial(&desc.socket).await {
        Dial::NotServing => return not_running(desc),
        Dial::Inconclusive => {
            return Err(LifecycleError::ProbeInconclusive {
                socket: desc.socket.clone(),
            });
        }
        Dial::Peer(None) => {
            return Err(LifecycleError::NoPeerPid {
                socket: desc.socket.clone(),
            });
        }
        Dial::Peer(Some(pid)) => pid,
    };

    // A listener that exits between the connect and the pin stopped on its
    // own: nothing was signalled, so the answer is `NotRunning` once settled.
    let Some(target) = PinnedProcess::open(pid)? else {
        settle(desc, pid).await?;
        return Ok(StopOutcome::NotRunning);
    };
    if !identity_holds(desc, &target)? {
        settle(desc, pid).await?;
        return Ok(StopOutcome::NotRunning);
    }

    send(delivery, &target, Signal::Term)?;
    let mut forced = false;
    if !target.wait_exit(desc.stop_grace).await {
        send(delivery, &target, Signal::Kill)?;
        forced = true;
        if !target.wait_exit(desc.kill_wait).await {
            return Err(LifecycleError::Survivor { pid });
        }
    }
    settle(desc, pid).await?;
    Ok(StopOutcome::Stopped { pid, forced })
}

/// The socket does not serve: `NotRunning`, unless a lock signal's lock is held.
fn not_running(desc: &DaemonDescriptor) -> Result<StopOutcome, LifecycleError> {
    if let StoppedSignal::LockFree(lock) = &desc.stopped_signal
        && !lock_is_free(lock)?
    {
        return Err(LifecycleError::Unidentified {
            socket: desc.socket.clone(),
            lock: lock.clone(),
        });
    }
    Ok(StopOutcome::NotRunning)
}

/// `Ok(true)` when the pinned process is alive and runs `desc.binary`;
/// `Ok(false)` when it exited mid-check; an error, with nothing signalled,
/// when it runs something else or cannot be read.
fn identity_holds(desc: &DaemonDescriptor, target: &PinnedProcess) -> Result<bool, LifecycleError> {
    let pid = target.pid();
    let read = exe_name(pid);
    // Alive after the read means the read saw the pinned process: a pid
    // number cannot be reused while its process exists.
    if target.has_exited() {
        return Ok(false);
    }
    match read {
        Ok(found) if found == desc.binary => Ok(true),
        Ok(found) => Err(LifecycleError::IdentityMismatch {
            pid,
            expected: desc.binary.clone(),
            found,
        }),
        Err(source) => Err(LifecycleError::IdentityUnreadable { pid, source }),
    }
}

/// Deliver one signal; `ESRCH` (already exited) is left to the exit wait.
fn send(
    delivery: &dyn SignalDelivery,
    target: &PinnedProcess,
    signal: Signal,
) -> Result<(), LifecycleError> {
    match delivery.deliver(target, signal) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => Ok(()),
        Err(source) => Err(LifecycleError::Signal {
            pid: target.pid(),
            signal,
            source,
        }),
    }
}

/// Wait up to `desc.kill_wait` for the stopped signal to hold.
async fn settle(desc: &DaemonDescriptor, pid: u32) -> Result<(), LifecycleError> {
    let deadline = Instant::now() + desc.kill_wait;
    loop {
        if signal_holds(desc).await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(LifecycleError::StoppedSignalNotReached {
                pid,
                signal: describe(desc),
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn signal_holds(desc: &DaemonDescriptor) -> Result<bool, LifecycleError> {
    match &desc.stopped_signal {
        StoppedSignal::SocketRefuses => Ok(
            probe_socket_verdict(&desc.socket, PROBE_TIMEOUT).await == SocketVerdict::NotServing
        ),
        StoppedSignal::LockFree(lock) => lock_is_free(lock),
    }
}

fn describe(desc: &DaemonDescriptor) -> String {
    match &desc.stopped_signal {
        StoppedSignal::SocketRefuses => {
            format!("\"{} refuses connections\"", desc.socket.display())
        }
        StoppedSignal::LockFree(lock) => format!("\"flock on {} is free\"", lock.display()),
    }
}
