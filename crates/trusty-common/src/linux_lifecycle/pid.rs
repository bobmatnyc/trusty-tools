//! `stop_pid` and `restart_pid`: the lifecycle for a daemon the caller
//! identified by pid (#11085, AC10).
//!
//! Why: trusty-console keeps running when its socket bind fails, reachable
//! only through its `http_addr`. [`stop`] finds a daemon only through its
//! socket's peer, and the `/proc` scan refuses on such a daemon without
//! signalling it, so the console could not be stopped at all. The consumer
//! finds the pid itself — an `http_addr` lookup, a pid file — and this file
//! turns that pid into a stop with `stop`'s safety rules: a pidfd pin, an
//! identity and instance check before any signal, survivor detection.
//! What: [`stop_pid_with`] pins the pid, requires it to be this instance, then
//! signals and proves the stop as [`stop_with`] does. [`restart_pid_with`]
//! does the same check, refuses on any other process of this instance before
//! the stop, and then starts. A pid that no longer exists proves nothing, so
//! both hand over to the socket path ([`stop_with`], [`restart_with`]).
//! `refuse_restart_blockers` is the pre-stop check every restart shares.
//! Test: the `stop_pid_*` and `restart_pid_*` tests in `tests/linux_lifecycle.rs`.
//!
//! [`stop`]: crate::linux_lifecycle::stop()
//! [`stop_with`]: crate::linux_lifecycle::stop_with()
//! [`restart_with`]: crate::linux_lifecycle::restart_with()
//! [`stop_pid_with`]: crate::linux_lifecycle::stop_pid_with()
//! [`restart_pid_with`]: crate::linux_lifecycle::restart_pid_with()

use super::liveness::{effective_uid, process_matches, refuse_other_instances};
use super::process::{Dial, PinnedProcess, dial};
use super::start::{RestartOutcome, restart_with, start};
use super::stop::{
    KernelDelivery, SignalDelivery, StopOutcome, identity_holds, stop_with, terminate,
};
use super::{DaemonDescriptor, LifecycleError};

/// Stop the daemon running as `pid`, delivering signals through the kernel.
///
/// Why: AC10 — a daemon whose socket bind failed is found by pid, not socket.
/// What: [`stop_pid_with`] with [`KernelDelivery`].
/// Test: `stop_pid_stops_a_daemon_whose_socket_bind_failed`.
///
/// [`stop_pid_with`]: crate::linux_lifecycle::stop_pid_with()
/// [`KernelDelivery`]: crate::linux_lifecycle::KernelDelivery
pub async fn stop_pid(desc: &DaemonDescriptor, pid: u32) -> Result<StopOutcome, LifecycleError> {
    stop_pid_with(desc, pid, &KernelDelivery).await
}

/// Stop the daemon running as `pid`, delivering signals through `delivery`.
///
/// Why: the caller's pid comes from a file or an address lookup, either of
/// which can be stale or name a reused pid, so the pid is a hint that must be
/// proven to be this daemon instance before it is signalled.
/// What, in order:
///
/// 1. Pin `pid` with a pidfd. No such process: the answer is [`stop_with`]'s,
///    the socket and then the `/proc` scan, so a stale pid never reads as
///    `NotRunning` while the daemon runs under another pid.
/// 2. Require `/proc/<pid>/exe` to name `desc.binary` (`IdentityMismatch` or
///    `IdentityUnreadable` otherwise). Then require the pid to be this
///    instance: this uid, every `liveness_args` element, no
///    `instance_path_args` flag naming another path, every `instance_env`
///    value — `NotThisInstance` otherwise, and an unreadable `cmdline` or
///    `environ` is an error. The pidfd is checked after the reads: a pid that
///    exited mid-check goes to step 1's answer, and a reused pid is never read.
/// 3. SIGTERM, SIGKILL after `stop_grace`, `Survivor` after `kill_wait`, then
///    the stopped signal, as in [`stop_with`].
///
/// Test: `stop_pid_stops_a_daemon_whose_socket_bind_failed`,
/// `stop_pid_refuses_a_pid_that_is_not_this_instance`,
/// `stop_pid_of_a_pid_that_is_gone_answers_as_stop_does`,
/// `stop_pid_reports_a_survivor_and_restart_pid_starts_nothing`.
///
/// [`stop_with`]: crate::linux_lifecycle::stop_with()
pub async fn stop_pid_with(
    desc: &DaemonDescriptor,
    pid: u32,
    delivery: &dyn SignalDelivery,
) -> Result<StopOutcome, LifecycleError> {
    desc.validate()?;
    match identify(desc, pid)? {
        Some(target) => terminate(desc, delivery, &target).await,
        // #11085: a stale pid proves nothing; the socket and the scan decide.
        None => stop_with(desc, delivery).await,
    }
}

/// Restart the daemon running as `pid`, delivering signals through the kernel.
///
/// What: [`restart_pid_with`] with [`KernelDelivery`].
/// Test: `restart_pid_restarts_a_daemon_whose_socket_bind_failed`.
///
/// [`restart_pid_with`]: crate::linux_lifecycle::restart_pid_with()
/// [`KernelDelivery`]: crate::linux_lifecycle::KernelDelivery
pub async fn restart_pid(
    desc: &DaemonDescriptor,
    pid: u32,
) -> Result<RestartOutcome, LifecycleError> {
    restart_pid_with(desc, pid, &KernelDelivery).await
}

/// [`restart_pid`] with an explicit signal delivery (the test seam).
///
/// Why: D1 — stop, prove it stopped, then start — for a daemon found by pid.
/// What: the identity and instance check of [`stop_pid_with`]. When `pid` no
/// longer exists, this is [`restart_with`]. Otherwise any other process of
/// this instance is [`LifecycleError::RestartBlocked`] before any signal, then
/// the stop, then [`start`]; every stop error returns before a spawn.
/// Test: `restart_pid_restarts_a_daemon_whose_socket_bind_failed`,
/// `restart_pid_refuses_before_any_signal_when_another_process_is_this_instance`,
/// `stop_pid_reports_a_survivor_and_restart_pid_starts_nothing`.
///
/// [`restart_pid`]: crate::linux_lifecycle::restart_pid()
/// [`stop_pid_with`]: crate::linux_lifecycle::stop_pid_with()
/// [`restart_with`]: crate::linux_lifecycle::restart_with()
/// [`start`]: crate::linux_lifecycle::start()
/// [`LifecycleError::RestartBlocked`]: crate::linux_lifecycle::LifecycleError::RestartBlocked
pub async fn restart_pid_with(
    desc: &DaemonDescriptor,
    pid: u32,
    delivery: &dyn SignalDelivery,
) -> Result<RestartOutcome, LifecycleError> {
    desc.validate()?;
    let Some(target) = identify(desc, pid)? else {
        return restart_with(desc, delivery).await;
    };
    refuse_restart_blockers(desc, Some(target.pid())).await?;
    let stop = terminate(desc, delivery, &target).await?;
    let start = start(desc).await?;
    Ok(RestartOutcome { stop, start })
}

/// `pid`, pinned, once it is proven this instance; `None` when it is gone.
///
/// What: step 2 of [`stop_pid_with`]. `Ok(None)` only when the pidfd shows
/// the process exited (or it never existed); a live process that is not this
/// instance is an error, never `None`.
///
/// [`stop_pid_with`]: crate::linux_lifecycle::stop_pid_with()
fn identify(desc: &DaemonDescriptor, pid: u32) -> Result<Option<PinnedProcess>, LifecycleError> {
    let Some(target) = PinnedProcess::open(pid)? else {
        return Ok(None);
    };
    if !identity_holds(desc, &target)? {
        return Ok(None);
    }
    let instance = process_matches(pid, effective_uid(), desc)?;
    // Alive after the reads means they saw the pinned process.
    if target.has_exited() {
        return Ok(None);
    }
    if !instance {
        return Err(LifecycleError::NotThisInstance {
            pid,
            binary: desc.binary.clone(),
        });
    }
    Ok(Some(target))
}

/// Fail a restart before its stop when another process is this instance.
///
/// Why: #11085 — the start after the stop refuses on such a process, which
/// left a previously healthy daemon down.
/// What: the running daemon's pids are `known` (a pid already proven this
/// instance) and the socket's peer, when it serves. With neither, nothing
/// runs healthy and the stop's own checks decide. Otherwise the scan, with
/// those pids left out, must find nothing.
/// Test: `restart_refuses_before_any_signal_when_another_process_is_this_instance`.
pub(crate) async fn refuse_restart_blockers(
    desc: &DaemonDescriptor,
    known: Option<u32>,
) -> Result<(), LifecycleError> {
    let mut serving: Vec<u32> = known.into_iter().collect();
    if let Dial::Peer(Some(peer)) = dial(&desc.socket).await
        && !serving.contains(&peer)
    {
        serving.push(peer);
    }
    if serving.is_empty() {
        return Ok(());
    }
    serving.sort_unstable();
    refuse_other_instances(desc, &serving)
}
