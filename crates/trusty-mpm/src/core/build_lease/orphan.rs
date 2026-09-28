//! A build whose `tm build-lease` holder died while the build kept running
//! (#8261 repair r3).
//!
//! Why: cargo releases its `.cargo-lock` before it runs test binaries, and
//! before `cargo run` execs the program (`cargo_releases_its_build_lock_while_
//! test_binaries_run` proves it with a real cargo). A holder SIGKILLed during
//! a `cargo test` run therefore left a build with its slot flock free, its
//! `.cargo-lock` free and no compiler process for the census to count. The
//! slot read free, and a second build took it while the first one's tests ran:
//! two builders in one slot.
//!
//! What: a clean release clears the slot file's record (`SlotGuard`'s `Drop`),
//! so a record left in a FREE slot file was written by a holder that died.
//! [`is_orphaned_build`] reports it while the build it names (`child_pid`) is
//! alive and started within [`START_WINDOW_SECS`] of the record's
//! `started_at`, so a pid the kernel reused for an unrelated process does not
//! hold the slot. A start time that cannot be read counts as the build: an
//! unreadable signal degrades toward the ceiling, never toward admission.
//! Test: the `#[cfg(test)]` suite below;
//! `a_sigkilled_holders_live_test_run_keeps_its_slot` in
//! `tests/tm_build_lease.rs` with a real `cargo test`.

use super::slots::HolderRecord;

/// How long after the record's `started_at` its build may have started.
///
/// Why: the holder writes the record, logs its decision (at most ~1.1 s) and
/// then spawns the build, so the build starts within seconds of `started_at`.
/// A process that started later than this is a reused pid, not the build.
pub const START_WINDOW_SECS: i64 = 60;

/// Whether `record`, found in a FREE slot file, names a build still running.
///
/// Test: `a_record_naming_a_live_build_is_an_orphan`,
/// `a_record_naming_a_dead_build_is_not_an_orphan`,
/// `a_reused_pid_is_not_an_orphan`.
#[must_use]
pub fn is_orphaned_build(record: &HolderRecord) -> bool {
    let Some(child) = record.child_pid.filter(|pid| *pid != 0) else {
        return false;
    };
    if !pid_alive(child) {
        return false;
    }
    let Ok(started) = chrono::DateTime::parse_from_rfc3339(&record.started_at) else {
        return true;
    };
    process_start_secs(child).is_none_or(|start| starts_in_window(start, started.timestamp()))
}

/// Whether a process that started at `start` can be the build a record
/// written at `recorded` spawned (both Unix seconds).
fn starts_in_window(start: i64, recorded: i64) -> bool {
    // The process table rounds to whole seconds; allow one either side.
    (recorded - 1..=recorded + START_WINDOW_SECS).contains(&start)
}

/// Whether `pid` names a live process, including one owned by another user.
fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: kill(2) with signal 0 only checks that the process exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The start time of `pid` in Unix seconds, when the process table has it.
fn process_start_secs(pid: u32) -> Option<i64> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(pid)
        .and_then(|p| i64::try_from(p.start_time()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(child_pid: Option<u32>, started_at: String) -> HolderRecord {
        let mut record = HolderRecord::new(0, "cargo test", "/repo");
        record.pid = 0;
        record.child_pid = child_pid;
        record.started_at = started_at;
        record
    }

    #[test]
    fn a_record_naming_a_live_build_is_an_orphan() {
        let started = chrono::Utc::now().to_rfc3339();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn a stand-in build");
        let orphan = is_orphaned_build(&record(Some(child.id()), started));
        let _ = child.kill();
        let _ = child.wait();
        assert!(orphan, "a live build named by a dead holder's record");
    }

    #[test]
    fn a_record_naming_a_dead_build_is_not_an_orphan() {
        let started = chrono::Utc::now().to_rfc3339();
        let mut child = std::process::Command::new("true").spawn().expect("spawn");
        let pid = child.id();
        child.wait().expect("reap");
        assert!(!is_orphaned_build(&record(Some(pid), started)));
        assert!(!is_orphaned_build(&record(None, String::new())));
    }

    /// This test process is alive, but it started long before a record
    /// written an hour from now: a reused pid, not the record's build.
    #[test]
    fn a_reused_pid_is_not_an_orphan() {
        let later = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        assert!(!is_orphaned_build(&record(Some(std::process::id()), later)));
        assert!(starts_in_window(100, 100));
        assert!(starts_in_window(99, 100));
        assert!(!starts_in_window(98, 100));
        assert!(!starts_in_window(100 + START_WINDOW_SECS + 1, 100));
    }
}
