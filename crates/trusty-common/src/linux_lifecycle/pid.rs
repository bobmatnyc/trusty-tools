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
//! What: [`stop_pid_with`] pins the pid, requires it to be this instance and
//! the socket to be unserved or served by it, then signals and proves the stop
//! as [`stop_with`] does. [`restart_pid_with`] does the same checks, refuses on
//! any other process of this instance before the stop, and then starts. A pid
//! that no longer exists proves nothing, so both hand over to the socket path
//! ([`stop_with`], [`restart_with`]).
//! Test: the `stop_pid_*` and `restart_pid_*` tests in `tests/linux_lifecycle.rs`.
//!
//! [`stop`]: crate::linux_lifecycle::stop()
//! [`stop_with`]: crate::linux_lifecycle::stop_with()
//! [`restart_with`]: crate::linux_lifecycle::restart_with()
//! [`stop_pid_with`]: crate::linux_lifecycle::stop_pid_with()
//! [`restart_pid_with`]: crate::linux_lifecycle::restart_pid_with()

use super::liveness::{
    Instance, ProcessTable, effective_uid, process_instance, refuse_other_instances,
};
use super::process::{Dial, PinnedProcess, dial};
use super::start::{RestartOutcome, installed_program, restart_with_table, start_with_table};
use super::stop::{
    KernelDelivery, SignalDelivery, StopOutcome, identity_holds, stop_with_table, terminate,
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
/// 1. Empty `liveness_args` is `InvalidDescriptor`: every process of the
///    binary, a stdio bridge included, would pass the instance check.
/// 2. Pin `pid` with a pidfd. No such process: the answer is [`stop_with`]'s,
///    the socket and then the `/proc` scan, so a stale pid never reads as
///    `NotRunning` while the daemon runs under another pid.
/// 3. Require `/proc/<pid>/exe` to name `desc.binary` (`IdentityMismatch` or
///    `IdentityUnreadable` otherwise). Then require the pid to be this
///    instance: this uid, every `liveness_args` element, no
///    `instance_path_args` flag naming another path, every `instance_env`
///    value — `NotThisInstance` otherwise. A path flag with no value, or a
///    relative or unresolvable one, is `InstanceUndecided`, and an unreadable
///    `cmdline` or `environ` is an error. The pidfd is checked after the
///    reads: a pid that exited mid-check goes to step 2's answer, and a reused
///    pid is never read.
/// 4. Dial the socket. Served by another pid is `ServedByAnother`, since the
///    stopped signal could not hold after the stop; an undecided dial is
///    `ProbeInconclusive` or `NoPeerPid`.
/// 5. SIGTERM, SIGKILL after `stop_grace`, `Survivor` after `kill_wait`, then
///    the stopped signal, as in [`stop_with`].
///
/// Test: `stop_pid_stops_a_daemon_whose_socket_bind_failed`,
/// `stop_pid_refuses_a_pid_that_is_not_this_instance`,
/// `stop_pid_and_restart_pid_refuse_an_undecided_instance_before_any_signal`,
/// `stop_pid_and_restart_pid_refuse_before_any_signal_when_another_pid_serves_the_socket`,
/// `stop_pid_and_restart_pid_stop_short_when_the_socket_cannot_be_probed`,
/// `stop_pid_of_a_pid_that_is_gone_answers_as_stop_does`,
/// `stop_pid_reports_a_survivor_and_restart_pid_starts_nothing`.
///
/// [`stop_with`]: crate::linux_lifecycle::stop_with()
pub async fn stop_pid_with(
    desc: &DaemonDescriptor,
    pid: u32,
    delivery: &dyn SignalDelivery,
) -> Result<StopOutcome, LifecycleError> {
    stop_pid_with_table(desc, pid, delivery, &ProcessTable::system()).await
}

/// [`stop_pid_with`], with the scan reading `table` instead of `/proc`
/// (#11085).
///
/// What: [`stop_pid_with`]'s steps. `pid` itself is still pinned and checked
/// in the real `/proc`; `table` is read only by the scan [`stop_with_table`]
/// runs when `pid` is gone.
/// Test: `stop_pid_of_a_pid_that_is_gone_answers_as_stop_does`,
/// `stop_pid_stops_a_daemon_whose_socket_bind_failed` (through [`stop_pid_with`]).
///
/// [`stop_pid_with`]: crate::linux_lifecycle::stop_pid_with()
/// [`stop_with_table`]: crate::linux_lifecycle::stop_with_table()
pub async fn stop_pid_with_table(
    desc: &DaemonDescriptor,
    pid: u32,
    delivery: &dyn SignalDelivery,
    table: &ProcessTable,
) -> Result<StopOutcome, LifecycleError> {
    validate_by_pid(desc)?;
    let Some(target) = identify(desc, pid)? else {
        // #11085: a stale pid proves nothing; the socket and the scan decide.
        return stop_with_table(desc, delivery, table).await;
    };
    if let Some(peer) = other_socket_peer(desc, pid).await? {
        return Err(LifecycleError::ServedByAnother {
            pid,
            peer,
            socket: desc.socket.clone(),
        });
    }
    terminate(desc, delivery, &target).await
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
/// What: steps 1-3 of [`stop_pid_with`], with `BinaryMissing` checked first.
/// When `pid` no longer exists, this is [`restart_with`]. Otherwise, before
/// any signal: a socket served by another pid is
/// [`LifecycleError::RestartBlocked`] naming that peer, an undecided dial is
/// `ProbeInconclusive` or `NoPeerPid`, and any other process of this instance
/// is `RestartBlocked`. Then the stop, then [`start`]; every stop error
/// returns before a spawn.
/// Test: `restart_pid_restarts_a_daemon_whose_socket_bind_failed`,
/// `restart_pid_refuses_before_any_signal_when_another_process_is_this_instance`,
/// `stop_pid_and_restart_pid_refuse_before_any_signal_when_another_pid_serves_the_socket`,
/// `restart_and_restart_pid_stop_nothing_when_the_install_binary_is_missing`,
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
    restart_pid_with_table(desc, pid, delivery, &ProcessTable::system()).await
}

/// [`restart_pid_with`], with every scan reading `table` instead of `/proc`
/// (#11085).
///
/// What: [`restart_pid_with`]'s steps. `pid` itself is still pinned and
/// checked in the real `/proc`; the scan for another instance and the
/// start's scan read `table`.
/// Test: `restart_pid_refuses_before_any_signal_when_another_process_is_this_instance`,
/// `restart_pid_restarts_a_daemon_whose_socket_bind_failed` (through
/// [`restart_pid_with`]).
///
/// [`restart_pid_with`]: crate::linux_lifecycle::restart_pid_with()
pub async fn restart_pid_with_table(
    desc: &DaemonDescriptor,
    pid: u32,
    delivery: &dyn SignalDelivery,
    table: &ProcessTable,
) -> Result<RestartOutcome, LifecycleError> {
    validate_by_pid(desc)?;
    // #11085: a missing binary found only at the start left the daemon down.
    installed_program(desc)?;
    let Some(target) = identify(desc, pid)? else {
        return restart_with_table(desc, delivery, table).await;
    };
    // #11085: with another pid serving, the start after the stop would refuse.
    if let Some(peer) = other_socket_peer(desc, pid).await? {
        return Err(LifecycleError::RestartBlocked {
            serving: vec![pid],
            binary: desc.binary.clone(),
            others: vec![peer],
        });
    }
    refuse_other_instances(desc, &[pid], table)?;
    let stop = terminate(desc, delivery, &target).await?;
    let start = start_with_table(desc, table).await?;
    Ok(RestartOutcome { stop, start })
}

/// Step 1 of [`stop_pid_with`]: the descriptor, and `liveness_args` set.
///
/// [`stop_pid_with`]: crate::linux_lifecycle::stop_pid_with()
fn validate_by_pid(desc: &DaemonDescriptor) -> Result<(), LifecycleError> {
    desc.validate()?;
    // #11085: with no liveness_args every process of the binary passes.
    if desc.liveness_args.is_empty() {
        return Err(LifecycleError::InvalidDescriptor {
            reason: "stop and restart by pid need liveness_args: without them any process of \
                     the binary, a stdio bridge included, passes the instance check"
                .to_string(),
        });
    }
    Ok(())
}

/// `pid`, pinned, once it is proven this instance; `None` when it is gone.
///
/// What: step 3 of [`stop_pid_with`]. `Ok(None)` only when the pidfd shows
/// the process exited (or it never existed); a live process that is not
/// proven this instance is an error, never `None`.
///
/// [`stop_pid_with`]: crate::linux_lifecycle::stop_pid_with()
fn identify(desc: &DaemonDescriptor, pid: u32) -> Result<Option<PinnedProcess>, LifecycleError> {
    let Some(target) = PinnedProcess::open(pid)? else {
        return Ok(None);
    };
    if !identity_holds(desc, &target)? {
        return Ok(None);
    }
    // #11085: the pinned pid is read in the real `/proc`, never an injected
    // table, so "alive after the reads" proves the reads saw it.
    let instance = process_instance(pid, effective_uid(), desc, &ProcessTable::system())?;
    // Alive after the reads means they saw the pinned process.
    if target.has_exited() {
        return Ok(None);
    }
    match instance {
        Instance::This => Ok(Some(target)),
        Instance::Not => Err(LifecycleError::NotThisInstance {
            pid,
            binary: desc.binary.clone(),
        }),
        // #11085: the scan counts undecided as a match; a signal needs proof.
        Instance::Undecided(flag) => Err(LifecycleError::InstanceUndecided {
            pid,
            binary: desc.binary.clone(),
            flag: flag.to_string(),
        }),
    }
}

/// The socket's peer when it is a pid other than `pid`; `None` when the
/// socket is not served or `pid` serves it.
///
/// What: an undecided dial is [`LifecycleError::ProbeInconclusive`] or
/// [`LifecycleError::NoPeerPid`].
///
/// [`LifecycleError::ProbeInconclusive`]: crate::linux_lifecycle::LifecycleError::ProbeInconclusive
/// [`LifecycleError::NoPeerPid`]: crate::linux_lifecycle::LifecycleError::NoPeerPid
async fn other_socket_peer(
    desc: &DaemonDescriptor,
    pid: u32,
) -> Result<Option<u32>, LifecycleError> {
    match dial(&desc.socket).await {
        Dial::NotServing => Ok(None),
        Dial::Peer(Some(peer)) => Ok((peer != pid).then_some(peer)),
        Dial::Peer(None) => Err(LifecycleError::NoPeerPid {
            socket: desc.socket.clone(),
        }),
        Dial::Inconclusive => Err(LifecycleError::ProbeInconclusive {
            socket: desc.socket.clone(),
        }),
    }
}
