//! Running the leased command: spawn, signal forwarding, the phase flip and
//! the exit status (#8261, #8692).
//!
//! Why: split out of `build_lease.rs`, which sat at the 500-SLOC cap, when
//! #8692 added the compile-to-run flip to the wait loop.
//! Test: `tests/tm_build_lease.rs` (real processes and flocks).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use trusty_mpm::core::build_lease::phase::{PHASE_POLL, Phase, PhaseTracker};
use trusty_mpm::core::build_lease::slots::{HolderRecord, SlotGuard};
use trusty_mpm::core::build_lease::stale_guard::cargo_lock_held;
use trusty_mpm::core::build_lease::test_summary::{
    EmptyTargetScan, hides_a_cargo_test, wants_empty_target_check,
};

/// The exit code for a `cargo test` that exited 0 although a target ran no
/// test with every test filtered out and no filter requested (`EX_DATAERR`,
/// #11661).
pub(super) const EXIT_EMPTY_TEST_TARGET: i32 = 65;

/// How long the output readers may run on after the build exits: a test that
/// left a grandchild holding the pipe must not hold the lease (#11661).
const DRAIN_BOUND: std::time::Duration = std::time::Duration::from_secs(5);

/// The build's status and, for a checked `cargo test`, its empty targets.
pub(super) struct RunOutcome {
    status: std::process::ExitStatus,
    empty_targets: Vec<String>,
}

/// A held slot, its record, and where its cargo takes its build lock.
pub(super) struct Lease<'a> {
    /// The held slot.
    pub(super) guard: &'a mut SlotGuard,
    /// The record written into it.
    pub(super) record: &'a mut HolderRecord,
    /// #8692: the directory this lease owns (slot flock or dir lock) whose
    /// `.cargo-lock` ends the compile phase; `None` (it owns none) never flips.
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
/// #11661: an unfiltered `cargo test` ([`wants_empty_target_check`]) has its
/// stdout and stderr piped, forwarded line by line to this process's own, and
/// read by an [`EmptyTargetScan`].
/// Test: `a_second_build_is_admitted_while_the_first_ones_test_binary_runs_8692`,
/// `a_running_cargo_run_server_does_not_hold_capacity_8692`,
/// `a_shared_dir_lease_keeps_its_capacity_while_its_test_binary_runs_8692`.
pub(super) async fn spawn_and_wait(
    command: &[String],
    target_dir: Option<&str>,
    dir: Option<&Path>,
    mut lease: Option<Lease<'_>>,
) -> std::io::Result<RunOutcome> {
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
    // #11661: a wrapped `cargo test` is never skipped silently.
    if hides_a_cargo_test(command) {
        eprintln!(
            "tm build-lease: the empty-test-target check (#11661) did not run — this command \
             wraps its `cargo test` (a shell or `env -S`), so the lease cannot read its filter. \
             A target that ran 0 tests will not fail this build; run `cargo test` directly \
             under the lease to have it checked."
        );
    }
    let scan = wants_empty_target_check(command).then(|| {
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        Arc::new(Mutex::new(EmptyTargetScan::new()))
    });
    // #8736: a SIGKILL between spawn and the child_pid write leaves the slot Free; waived as unreachable, tracked in #8692
    let mut child = cmd.spawn()?;
    let pid = child.id();
    let mut readers = Vec::new();
    if let Some(scan) = &scan {
        if let Some(out) = child.stdout.take() {
            readers.push(tokio::spawn(forward(
                out,
                tokio::io::stdout(),
                scan.clone(),
            )));
        }
        if let Some(err) = child.stderr.take() {
            readers.push(tokio::spawn(forward(
                err,
                tokio::io::stderr(),
                scan.clone(),
            )));
        }
    }
    if let (Some(lease), Some(pid)) = (lease.as_mut(), pid) {
        lease.record.child_pid = Some(pid);
        let _ = write_record(lease.guard, lease.record);
    }
    let mut tracker = PhaseTracker::new();
    let mut tick = tokio::time::interval(PHASE_POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        // #8692: a lease that owns no directory has no lock_dir and stays compile.
        let tracking = lease.as_ref().is_some_and(|l| l.lock_dir.is_some()) && !tracker.is_done();
        let sig = tokio::select! {
            status = child.wait() => {
                let status = status?;
                for reader in readers.drain(..) {
                    // #11661: bounded; a reader still running past it is dropped.
                    if tokio::time::timeout(DRAIN_BOUND, reader).await.is_err() {
                        eprintln!(
                            "tm build-lease: the empty-test-target check (#11661) is \
                             INCOMPLETE — output was still arriving {}s after the build \
                             exited (a process it started holds the pipe), and the rest \
                             was not read.",
                            DRAIN_BOUND.as_secs()
                        );
                    }
                }
                // #11661: a poisoned scan still reports what it read.
                let empty_targets = scan
                    .as_ref()
                    .map(|s| {
                        s.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .empty_targets()
                            .to_vec()
                    })
                    .unwrap_or_default();
                return Ok(RunOutcome { status, empty_targets });
            }
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

/// Copy `from` to `to` line by line, feeding each line to `scan` (#11661).
///
/// What: bytes are forwarded unchanged and flushed per line; the scan reads a
/// lossy UTF-8 copy. A write error stops forwarding but not the reading, so
/// the child never blocks on a full pipe.
async fn forward(
    from: impl AsyncRead + Unpin,
    mut to: impl AsyncWrite + Unpin,
    scan: Arc<Mutex<EmptyTargetScan>>,
) {
    let mut from = BufReader::new(from);
    let mut line = Vec::new();
    let mut writable = true;
    loop {
        line.clear();
        match from.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if writable {
            writable = to.write_all(&line).await.is_ok() && to.flush().await.is_ok();
        }
        scan.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .line(String::from_utf8_lossy(&line).trim_end());
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
///
/// #11661: a `cargo test` that exited 0 with an empty target exits
/// [`EXIT_EMPTY_TEST_TARGET`] instead, naming each target; a failed one keeps
/// its own code and names them too.
/// Test: `a_cargo_test_target_that_filters_out_every_test_unasked_fails_11661`.
pub(super) fn exit_with(outcome: std::io::Result<RunOutcome>) -> ! {
    use std::os::unix::process::ExitStatusExt;
    match outcome {
        Ok(RunOutcome {
            status,
            empty_targets,
        }) => {
            for target in &empty_targets {
                eprintln!(
                    "tm build-lease: FAILED — test target {target}, and this run requested no \
                     filter. libtest counts a test as filtered out only under a filter, so \
                     the binary that ran does not match this command: likely one built from \
                     another worktree's sources. Nothing was tested (#11661)."
                );
            }
            if status.success() && !empty_targets.is_empty() {
                std::process::exit(EXIT_EMPTY_TEST_TARGET)
            }
            std::process::exit(
                status
                    .code()
                    .or_else(|| status.signal().map(|s| 128 + s))
                    .unwrap_or(1),
            )
        }
        Err(err) => {
            eprintln!("tm build-lease: could not run the command: {err}");
            std::process::exit(127)
        }
    }
}
