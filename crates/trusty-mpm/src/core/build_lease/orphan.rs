//! A build whose `tm build-lease` holder died while the build kept running
//! (#8261).
//!
//! Why: cargo releases its `.cargo-lock` before it runs test binaries
//! (`cargo_releases_its_build_lock_while_test_binaries_run` proves it with a
//! real cargo). A holder SIGKILLed during a `cargo test` run therefore left a
//! build with its slot flock free, its `.cargo-lock` free and no compiler
//! process for the census to count. The slot read free, and a second build
//! took it while the first one's tests ran: two builders in one slot.
//!
//! What: a clean release clears the slot file's record (`SlotGuard`'s `Drop`),
//! so a record left in a FREE slot file was written by a holder that died.
//! [`read_leftover`] classifies that file:
//!
//! - [`Leftover::Clear`] — empty, or its build is gone: the slot is free. A
//!   pid that started outside [`START_WINDOW_SECS`] of `started_at` is a pid
//!   the kernel reused, so it is gone too.
//! - [`Leftover::Running`] — the pid is alive and started inside the window:
//!   the slot stays held. The process name is deliberately NOT compared: cargo
//!   execs external subcommands (`cargo nextest` runs as `cargo-nextest`), and
//!   the rustup proxy, `sh -c` and `env -S` exec too, so a name check would
//!   free a live build (#8736). A reused pid that started inside the window
//!   holds the slot until it exits — the accepted fail-closed cost.
//! - [`Leftover::Unknown`] — the record is corrupt, or the pid or its start
//!   time cannot be read. The slot reads `Broken` — never taken, and a `tm
//!   doctor` FAIL — so an unreadable signal never reads as a free slot (#8736).
//!
//! Test: the `#[cfg(test)]` suite below, the `slots` suite, and
//! `a_sigkilled_holders_live_test_run_keeps_its_slot` in
//! `tests/tm_build_lease.rs` with a real `cargo test`.

use std::path::Path;

use super::slots::HolderRecord;

/// How long after the record's `started_at` its build may have started.
///
/// Why: the holder writes the record, logs its decision (at most ~1.1 s) and
/// then spawns the build, so the build starts within seconds of `started_at`.
/// A process that started later than this is a reused pid, not the build.
pub const START_WINDOW_SECS: i64 = 60;

/// What a FREE slot file's leftover record means for the slot.
///
/// Test: `a_record_naming_a_live_build_is_an_orphan`,
/// `a_record_naming_a_dead_build_is_not_an_orphan`, and one test per
/// [`Leftover::Unknown`] arm below and in the `slots` suite.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Leftover {
    /// No record, or its build is gone: the slot is free.
    Clear,
    /// A dead holder's build still runs: the slot stays held.
    Running(HolderRecord),
    /// The record or its process could not be read or checked; the reason.
    Unknown(String),
}

/// Classify the record in the FREE slot file at `path`.
///
/// What: an empty file is [`Leftover::Clear`]; an unreadable file or a
/// non-empty body that is not a record is [`Leftover::Unknown`]; a record is
/// checked against the live process table.
/// Test: `a_corrupt_record_in_a_free_slot_is_broken`,
/// `an_orphaned_build_keeps_its_slot`.
#[must_use]
pub fn read_leftover(path: &Path) -> Leftover {
    let body = match std::fs::read(path) {
        Ok(body) => body,
        Err(err) => return Leftover::Unknown(format!("slot record unreadable: {err}")),
    };
    if body.iter().all(u8::is_ascii_whitespace) {
        return Leftover::Clear;
    }
    match serde_json::from_slice::<HolderRecord>(&body) {
        Ok(record) => classify(record, &LiveTable),
        Err(err) => Leftover::Unknown(format!("corrupt slot record ({} bytes): {err}", body.len())),
    }
}

/// The reads orphan detection needs; a seam so each failure arm is testable.
trait ProcessTable {
    /// `Ok(true)` alive (another user's process included), `Ok(false)` gone,
    /// `Err` when the check itself failed.
    fn alive(&self, pid: u32) -> Result<bool, String>;
    /// `None` when the table holds no entry for `pid`; `Some(None)` when it
    /// does but the start time is unreadable; else the Unix start seconds.
    fn start_secs(&self, pid: u32) -> Option<Option<i64>>;
}

/// The live process table: `kill(2)` and `sysinfo`.
struct LiveTable;

impl ProcessTable for LiveTable {
    fn alive(&self, pid: u32) -> Result<bool, String> {
        let raw = i32::try_from(pid).map_err(|_| format!("pid {pid} is out of range"))?;
        // SAFETY: kill(2) with signal 0 only checks that the process exists.
        if unsafe { libc::kill(raw, 0) } == 0 {
            return Ok(true);
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EPERM) => Ok(true),
            Some(libc::ESRCH) => Ok(false),
            _ => Err(err.to_string()),
        }
    }

    fn start_secs(&self, pid: u32) -> Option<Option<i64>> {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
        let pid = Pid::from_u32(pid);
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        let process = sys.process(pid)?;
        Some(i64::try_from(process.start_time()).ok().filter(|s| *s > 0))
    }
}

/// Classify a leftover `record` against `table`.
fn classify(record: HolderRecord, table: &dyn ProcessTable) -> Leftover {
    let Some(pid) = record.child_pid.filter(|pid| *pid != 0) else {
        return Leftover::Clear;
    };
    match table.alive(pid) {
        Ok(true) => {}
        Ok(false) => return Leftover::Clear,
        Err(err) => return Leftover::Unknown(format!("could not check build pid {pid}: {err}")),
    }
    let Ok(recorded) = chrono::DateTime::parse_from_rfc3339(&record.started_at) else {
        return Leftover::Unknown(format!(
            "unparseable started_at {:?} for build pid {pid}",
            record.started_at
        ));
    };
    let Some(start) = table.start_secs(pid) else {
        // It may have exited between the two reads.
        return match table.alive(pid) {
            Ok(false) => Leftover::Clear,
            _ => Leftover::Unknown(format!("no process-table entry for live build pid {pid}")),
        };
    };
    let Some(start) = start else {
        return Leftover::Unknown(format!("start time of build pid {pid} unreadable"));
    };
    if starts_in_window(start, recorded.timestamp()) {
        Leftover::Running(record)
    } else {
        Leftover::Clear
    }
}

/// Whether a process that started at `start` can be the build a record
/// written at `recorded` spawned (both Unix seconds).
fn starts_in_window(start: i64, recorded: i64) -> bool {
    // The process table rounds to whole seconds; allow one either side.
    (recorded - 1..=recorded + START_WINDOW_SECS).contains(&start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(command: &str, child_pid: Option<u32>, started_at: String) -> HolderRecord {
        let mut record = HolderRecord::new(0, command, "/repo");
        record.pid = 0;
        record.child_pid = child_pid;
        record.started_at = started_at;
        record
    }

    /// A process table with scripted readings.
    struct Fake {
        alive: Result<bool, String>,
        start: Option<Option<i64>>,
    }

    impl ProcessTable for Fake {
        fn alive(&self, _pid: u32) -> Result<bool, String> {
            self.alive.clone()
        }
        fn start_secs(&self, _pid: u32) -> Option<Option<i64>> {
            self.start
        }
    }

    fn classify_now(table: &Fake) -> Leftover {
        classify(
            record("cargo test", Some(42), chrono::Utc::now().to_rfc3339()),
            table,
        )
    }

    #[test]
    fn a_record_naming_a_live_build_is_an_orphan() {
        let started = chrono::Utc::now().to_rfc3339();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn a stand-in build");
        let rec = record("sleep 30", Some(child.id()), started);
        let found = classify(rec.clone(), &LiveTable);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(found, Leftover::Running(rec), "a dead holder's live build");
    }

    #[test]
    fn a_record_naming_a_dead_build_is_not_an_orphan() {
        let started = chrono::Utc::now().to_rfc3339();
        let mut child = std::process::Command::new("true").spawn().expect("spawn");
        let pid = child.id();
        child.wait().expect("reap");
        assert_eq!(
            classify(record("true", Some(pid), started), &LiveTable),
            Leftover::Clear
        );
        assert_eq!(
            classify(record("true", None, String::new()), &LiveTable),
            Leftover::Clear
        );
    }

    /// This test process is alive, but it started long before a record
    /// written an hour from now: a reused pid, not the record's build.
    #[test]
    fn a_reused_pid_is_not_an_orphan() {
        let later = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let rec = record("cargo test", Some(std::process::id()), later);
        assert_eq!(classify(rec, &LiveTable), Leftover::Clear);
        assert!(starts_in_window(100, 100));
        assert!(starts_in_window(99, 100));
        assert!(!starts_in_window(98, 100));
        assert!(!starts_in_window(100 + START_WINDOW_SECS + 1, 100));
    }

    /// #8736 fail-open arm: the pid check itself failed.
    #[test]
    fn a_failed_pid_check_is_unknown() {
        let table = Fake {
            alive: Err("EINVAL".into()),
            start: None,
        };
        assert!(matches!(classify_now(&table), Leftover::Unknown(e) if e.contains("EINVAL")));
    }

    /// #8736 fail-open arms: a live build whose start time cannot be read, or
    /// that the process table does not list.
    #[test]
    fn an_unreadable_start_time_is_unknown() {
        let unreadable = Fake {
            alive: Ok(true),
            start: Some(None),
        };
        assert!(
            matches!(classify_now(&unreadable), Leftover::Unknown(e) if e.contains("start time"))
        );
        let absent = Fake {
            alive: Ok(true),
            start: None,
        };
        assert!(
            matches!(classify_now(&absent), Leftover::Unknown(e) if e.contains("no process-table"))
        );
    }
}
