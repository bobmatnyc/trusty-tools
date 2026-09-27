//! Per-data-root election of the one process that runs maintenance (#8733).
//!
//! Why: several processes can open one data root with writer intent — the
//! launchd daemon, a second daemon on another socket, `kg-rebuild` — and each
//! used to run its own dream passes (dedup, prune, consolidation) and open-time
//! TTL purge. Two maintainers deleting from one store make a removed drawer
//! impossible to attribute (#8729). User writes are not affected: only
//! maintenance is elected.
//! What: [`MaintenanceLease`] holds an exclusive `flock` on
//! `<data_root>/maintenance.lock` from the first successful
//! [`MaintenanceLease::try_hold`] until the process exits. A process that
//! cannot take it runs no maintenance and retries on its next maintenance
//! tick, so a crashed holder — whose lock the kernel drops on exit — is
//! replaced within one tick. A lock file that cannot be opened fails closed.
//! The holder writes its pid into the file and logs the acquisition at warn,
//! which is the read-only way to see who maintains a root.
//! Test: `maintenance_lease_tests`.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// File name of the lease inside a data root.
pub const MAINTENANCE_LOCK_FILE: &str = "maintenance.lock";

/// Sidecar lock that makes "lock taken" and "pid written" one step (#8733).
const PID_GATE_SUFFIX: &str = ".gate";

/// How long a contender polls for the pid gate before going ungated (#8733).
///
/// A peer holds the gate only for a few syscalls, but a loaded host was
/// measured delaying a thread by over 100 ms, so the bound leaves headroom.
const PID_GATE_WAIT: Duration = Duration::from_millis(500);

/// Poll interval while the pid gate is busy.
const PID_GATE_POLL: Duration = Duration::from_millis(1);

/// Outcome of one [`MaintenanceLease::try_hold`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseStatus {
    /// This process holds the lease and may run maintenance.
    Held,
    /// Another process holds the lease; `holder_pid` is the pid it recorded.
    ///
    /// `None` only when the pid could not be read: the holder failed to write
    /// it, or a contender stalled inside its acquisition for longer than the
    /// pid-gate bound. Callers render it as "unknown" (#8733).
    HeldElsewhere { holder_pid: Option<u32> },
    /// The lock file could not be opened or locked; maintenance fails closed.
    Unavailable { reason: String },
}

impl LeaseStatus {
    /// Whether this process may run maintenance.
    #[must_use]
    pub fn is_held(&self) -> bool {
        matches!(self, Self::Held)
    }
}

/// Lifetime lease on one data root's maintenance role.
///
/// Why: `flock` is released by the kernel when the holder exits or crashes,
/// so the election needs no stale-lock cleanup; holding it for the process
/// lifetime (not per pass) stops two processes alternating passes, and keeps
/// one process's many per-palace loops from contending with each other —
/// `flock` conflicts per open file description, so a second descriptor in the
/// same process would lose to the first.
/// What: lazily opens the lock file on the first [`Self::try_hold`]; no I/O
/// happens at construction. Status changes are logged at warn once each.
/// Test: `only_one_of_two_leases_on_a_root_is_held`,
/// `a_released_lease_is_taken_over`, `an_uncreatable_lock_file_fails_closed`.
#[derive(Debug)]
pub struct MaintenanceLease {
    path: PathBuf,
    /// Bound on the pid-gate wait; [`PID_GATE_WAIT`] outside tests.
    gate_wait: Duration,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// The locked file while held; dropping it releases the lease.
    file: Option<File>,
    /// Last non-held status reported, so a repeat is not logged again.
    last_denial: Option<LeaseStatus>,
}

impl MaintenanceLease {
    /// A lease on `<data_root>/maintenance.lock`. Performs no I/O.
    #[must_use]
    pub fn new(data_root: &Path) -> Self {
        Self {
            path: data_root.join(MAINTENANCE_LOCK_FILE),
            gate_wait: PID_GATE_WAIT,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The same lease with a different pid-gate bound, so a test does not
    /// depend on host scheduling latency.
    #[cfg(test)]
    fn with_gate_wait(mut self, gate_wait: Duration) -> Self {
        self.gate_wait = gate_wait;
        self
    }

    /// Path of the lock file this lease guards.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Take the lease if it is free, or report who has it.
    ///
    /// Why: every maintenance entry point asks this before it deletes
    /// anything, so a non-holder skips the pass instead of racing the holder.
    /// What: returns [`LeaseStatus::Held`] at once when already held.
    /// Otherwise opens the lock file (never truncating another holder's pid)
    /// and tries a non-blocking exclusive lock. On success it records this
    /// pid in the file and logs at warn. A busy lock yields `HeldElsewhere`
    /// with the recorded pid; an open or lock error yields `Unavailable`.
    /// The lock attempt and the pid write or read run under a sidecar
    /// `maintenance.lock.gate` lock, so a loser reads the winner's pid (#8733).
    /// Test: `only_one_of_two_leases_on_a_root_is_held`,
    /// `a_loser_waits_for_the_winners_pid_write`,
    /// `an_uncreatable_lock_file_fails_closed`.
    pub fn try_hold(&self) -> LeaseStatus {
        let mut inner = self.inner.lock();
        if inner.file.is_some() {
            return LeaseStatus::Held;
        }
        match self.acquire() {
            Ok(file) => {
                inner.file = Some(file);
                inner.last_denial = None;
                tracing::warn!(
                    pid = std::process::id(),
                    lock = %self.path.display(),
                    "maintenance lease acquired: this process runs dream and TTL-purge \
                     passes for this data root (#8733)"
                );
                LeaseStatus::Held
            }
            Err(status) => {
                if inner.last_denial.as_ref() != Some(&status) {
                    log_denial(&self.path, &status);
                    inner.last_denial = Some(status.clone());
                }
                status
            }
        }
    }

    fn acquire(&self) -> Result<File, LeaseStatus> {
        let unavailable = |e: std::io::Error| LeaseStatus::Unavailable {
            reason: e.to_string(),
        };
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)
            .map_err(unavailable)?;
        // #8733: a winner's lock and its pid write are two syscalls, so a loser
        // could see the lock held with the file still empty (None) or still
        // carrying the previous holder's pid. Both steps, and the loser's read,
        // run under the gate; dropping `_gate` at return releases it.
        let _gate = enter_pid_gate(&gate_path(&self.path), self.gate_wait);
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(LeaseStatus::HeldElsewhere {
                    holder_pid: recorded_holder_pid(&self.path),
                });
            }
            Err(TryLockError::Error(e)) => return Err(unavailable(e)),
        }
        // The pid is diagnostic only; failing to write it never gates the lease.
        let pid_line = format!("{}\n", std::process::id());
        let recorded = file
            .set_len(0)
            .and_then(|()| file.seek(SeekFrom::Start(0)))
            .and_then(|_| file.write_all(pid_line.as_bytes()));
        if let Err(e) = recorded {
            tracing::warn!(lock = %self.path.display(), "maintenance lease: pid not recorded: {e}");
        }
        Ok(file)
    }
}

/// Path of the pid gate that sits beside `lock_path`.
fn gate_path(lock_path: &Path) -> PathBuf {
    let mut name = lock_path.as_os_str().to_owned();
    name.push(PID_GATE_SUFFIX);
    PathBuf::from(name)
}

/// Take the pid gate, polling for at most `wait` (#8733).
///
/// Why: the gate orders a winner's "lock + write pid" before any loser's
/// "see lock busy + read pid", so a loser reads the current holder's pid.
/// What: returns the locked gate file, or `None` when it cannot be opened or
/// stays busy past the bound. `None` means the caller proceeds ungated: the
/// election itself rests on the lease lock alone, so only the pid a loser
/// reports can then be missing. Never blocks longer than the bound, so a
/// stopped process that holds the gate cannot wedge maintenance ticks.
/// Test: `a_loser_waits_for_the_winners_pid_write`,
/// `a_wedged_pid_gate_does_not_block_the_election`.
fn enter_pid_gate(path: &Path, wait: Duration) -> Option<File> {
    let gate = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .inspect_err(|e| tracing::debug!(gate = %path.display(), "pid gate not opened: {e}"))
        .ok()?;
    let deadline = Instant::now() + wait;
    loop {
        match gate.try_lock() {
            Ok(()) => return Some(gate),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(PID_GATE_POLL);
            }
            Err(e) => {
                tracing::debug!(gate = %path.display(), "pid gate not taken: {e}");
                return None;
            }
        }
    }
}

fn log_denial(path: &Path, status: &LeaseStatus) {
    match status {
        LeaseStatus::HeldElsewhere { holder_pid } => tracing::warn!(
            holder_pid = ?holder_pid,
            lock = %path.display(),
            "maintenance lease held by another process: this process serves reads and \
             writes but runs no dream or TTL-purge pass (#8733)"
        ),
        LeaseStatus::Unavailable { reason } => tracing::warn!(
            lock = %path.display(),
            "maintenance lease unavailable ({reason}): this process runs no dream or \
             TTL-purge pass (fail closed, #8733)"
        ),
        LeaseStatus::Held => {}
    }
}

/// The pid the current or last holder wrote into `lock_path`, if readable.
///
/// Stale once that holder exits; it identifies, it does not prove liveness.
#[must_use]
pub fn recorded_holder_pid(lock_path: &Path) -> Option<u32> {
    std::fs::read_to_string(lock_path).ok()?.trim().parse().ok()
}

#[cfg(test)]
#[path = "maintenance_lease_tests.rs"]
mod maintenance_lease_tests;
