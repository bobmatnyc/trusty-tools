//! Running the leased command: spawn, signal forwarding, the phase flip and
//! the exit status (#8261, #8692).
//!
//! Why: split out of `build_lease.rs`, which sat at the 500-SLOC cap, when
//! #8692 added the compile-to-run flip to the wait loop.
//! Test: `tests/tm_build_lease.rs` (real processes and flocks).

use std::path::{Path, PathBuf};

use trusty_mpm::core::build_lease::phase::{PHASE_POLL, Phase, PhaseTracker};
use trusty_mpm::core::build_lease::slots::{HolderRecord, SlotGuard};
use trusty_mpm::core::build_lease::stale_guard::cargo_lock_held;

/// A held slot, its record, and where its cargo takes its build lock.
pub(super) struct Lease<'a> {
    /// The held slot.
    pub(super) guard: &'a mut SlotGuard,
    /// The record written into it.
    pub(super) record: &'a mut HolderRecord,
    /// #8692: the slot directory whose `.cargo-lock` ends the compile phase;
    /// `None` (a `Keep` or `Unset` plan) never flips, until PR B (#9223).
    pub(super) lock_dir: Option<PathBuf>,
}

/// Overwrite the slot's record, naming a failure on stderr.
pub(super) fn write_record(guard: &mut SlotGuard, record: &HolderRecord) -> std::io::Result<()> {
    guard.write_record(record).inspect_err(|err| {
        eprintln!(
            "tm build-lease: could not write the holder record to {}: {err}",
            guard.path().display()
        );
    })
}

/// Spawn the build, record its pid, forward termination signals, and wait.
///
/// What: the signal handlers are installed BEFORE the spawn (#8261 round 3),
/// so a SIGTERM that lands between the spawn and the handler cannot kill this
/// holder with the default action and orphan the build.
/// `dir` is the `--chdir` directory the build runs in (#8969). #8692: with a
/// lease that has a `lock_dir` (a slot-owned plan), every [`PHASE_POLL`] the
/// [`PhaseTracker`] reads cargo's build lock there; once compilation is over
/// the record is rewritten as
/// [`Phase::Run`], which frees compile capacity while this process keeps the
/// slot's flock — and so its directory — until the command exits.
/// Test: `a_second_build_is_admitted_while_the_first_ones_test_binary_runs_8692`,
/// `a_running_cargo_run_server_does_not_hold_capacity_8692`,
/// `a_shared_dir_lease_keeps_its_capacity_while_its_test_binary_runs_8692`.
pub(super) async fn spawn_and_wait(
    command: &[String],
    target_dir: Option<&str>,
    dir: Option<&Path>,
    mut lease: Option<Lease<'_>>,
) -> std::io::Result<std::process::ExitStatus> {
    let Some((program, rest)) = command.split_first() else {
        return Err(std::io::Error::other("no command given after `--`"));
    };
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(rest);
    if let Some(dir) = dir {
        cmd.current_dir(dir).env("PWD", dir);
    }
    if let Some(dir) = target_dir {
        cmd.env("CARGO_TARGET_DIR", dir);
    }
    // #8736: a SIGKILL between spawn and the child_pid write leaves the slot Free; waived as unreachable, tracked in #8692
    let mut child = cmd.spawn()?;
    let pid = child.id();
    if let (Some(lease), Some(pid)) = (lease.as_mut(), pid) {
        lease.record.child_pid = Some(pid);
        let _ = write_record(lease.guard, lease.record);
    }
    let mut tracker = PhaseTracker::new();
    let mut tick = tokio::time::interval(PHASE_POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        // #8692: a Keep/Unset lease has no lock_dir and stays compile (#9223).
        let tracking = lease.as_ref().is_some_and(|l| l.lock_dir.is_some()) && !tracker.is_done();
        let sig = tokio::select! {
            status = child.wait() => return status,
            _ = tick.tick(), if tracking => {
                if let Some(lease) = lease.as_mut() {
                    flip_when_compiled(&mut tracker, lease);
                }
                continue;
            }
            _ = term.recv() => libc::SIGTERM,
            _ = int.recv() => libc::SIGINT,
            _ = hup.recv() => libc::SIGHUP,
        };
        if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
            // SAFETY: kill(2) on the child's pid with a valid signal number.
            unsafe { libc::kill(pid, sig) };
        }
    }
}

/// One phase poll for `lease`: rewrite its record as [`Phase::Run`] when due.
///
/// What: #8692 — a failed write restores the in-memory phase to
/// [`Phase::Compile`], so the record on disk and in memory agree, and the
/// tracker retries on the next poll.
fn flip_when_compiled(tracker: &mut PhaseTracker, lease: &mut Lease<'_>) {
    let Some(dir) = lease.lock_dir.as_deref() else {
        return;
    };
    let (guard, record) = (&mut *lease.guard, &mut *lease.record);
    tracker.tick(
        || Ok(cargo_lock_held(dir)),
        || {
            record.phase = Phase::Run;
            write_record(guard, record).inspect_err(|_| record.phase = Phase::Compile)
        },
    );
}

/// Exit with the build's status: its code, or 128 + the signal that ended it.
pub(super) fn exit_with(status: std::io::Result<std::process::ExitStatus>) -> ! {
    use std::os::unix::process::ExitStatusExt;
    match status {
        Ok(status) => std::process::exit(
            status
                .code()
                .or_else(|| status.signal().map(|s| 128 + s))
                .unwrap_or(1),
        ),
        Err(err) => {
            eprintln!("tm build-lease: could not run the command: {err}");
            std::process::exit(127)
        }
    }
}
