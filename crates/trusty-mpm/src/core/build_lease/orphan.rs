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
//!   pid the kernel reused is gone too: the live process must have started
//!   within [`START_WINDOW_SECS`] of `started_at` AND run the recorded program
//!   (process name or executable, #8736).
//! - [`Leftover::Running`] — the recorded build is still alive: the slot
//!   stays held.
//! - [`Leftover::Unknown`] — the record is corrupt, or the pid, start time or
//!   process name cannot be read. The slot reads `Broken` — never taken, and a
//!   `tm doctor` FAIL — so an unreadable signal never reads as a free slot
//!   (#8736).
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

/// What the process table says about one pid.
#[derive(Debug, Clone, Default)]
struct ProcessFacts {
    /// Start time in Unix seconds, when readable.
    start_secs: Option<i64>,
    /// The process name, when readable.
    name: Option<String>,
    /// The executable's file name, when readable.
    exe_name: Option<String>,
}

/// The reads orphan detection needs; a seam so each failure arm is testable.
trait ProcessTable {
    /// `Ok(true)` alive (another user's process included), `Ok(false)` gone,
    /// `Err` when the check itself failed.
    fn alive(&self, pid: u32) -> Result<bool, String>;
    /// `None` when the table holds no entry for `pid`.
    fn facts(&self, pid: u32) -> Option<ProcessFacts>;
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

    fn facts(&self, pid: u32) -> Option<ProcessFacts> {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
        let pid = Pid::from_u32(pid);
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
        );
        let process = sys.process(pid)?;
        Some(ProcessFacts {
            start_secs: i64::try_from(process.start_time()).ok().filter(|s| *s > 0),
            name: Some(process.name().to_string_lossy().into_owned()).filter(|n| !n.is_empty()),
            exe_name: process
                .exe()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned()),
        })
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
    let Some(facts) = table.facts(pid) else {
        // It may have exited between the two reads.
        return match table.alive(pid) {
            Ok(false) => Leftover::Clear,
            _ => Leftover::Unknown(format!("no process-table entry for live build pid {pid}")),
        };
    };
    let Some(start) = facts.start_secs else {
        return Leftover::Unknown(format!("start time of build pid {pid} unreadable"));
    };
    if !starts_in_window(start, recorded.timestamp()) {
        return Leftover::Clear;
    }
    let Some(program) = program_name(&record.command) else {
        return Leftover::Unknown(format!("the record for build pid {pid} names no program"));
    };
    if facts.name.is_none() && facts.exe_name.is_none() {
        return Leftover::Unknown(format!("process name of build pid {pid} unreadable"));
    }
    if runs_program(&facts, &program) {
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

/// The basename of the recorded command's program word.
fn program_name(command: &str) -> Option<String> {
    let first = shlex::split(command)
        .and_then(|argv| argv.into_iter().next())
        .or_else(|| command.split_whitespace().next().map(str::to_string))?;
    let base = first.rsplit('/').next().unwrap_or(&first);
    (!base.is_empty()).then(|| base.to_string())
}

/// Whether `facts` names `program`, by process name or executable.
fn runs_program(facts: &ProcessFacts, program: &str) -> bool {
    // Linux truncates a process name to 15 bytes.
    let name_matches =
        |name: &String| name == program || (name.len() >= 15 && program.starts_with(name.as_str()));
    facts.name.as_ref().is_some_and(name_matches) || facts.exe_name.as_deref() == Some(program)
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
        facts: Option<ProcessFacts>,
    }

    impl ProcessTable for Fake {
        fn alive(&self, _pid: u32) -> Result<bool, String> {
            self.alive.clone()
        }
        fn facts(&self, _pid: u32) -> Option<ProcessFacts> {
            self.facts.clone()
        }
    }

    fn live_cargo(started: i64) -> Fake {
        Fake {
            alive: Ok(true),
            facts: Some(ProcessFacts {
                start_secs: Some(started),
                name: Some("cargo".into()),
                exe_name: Some("cargo".into()),
            }),
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

    /// #8736: identity by process name or executable, including a name the
    /// kernel truncated; another program at the pid frees the slot.
    #[test]
    fn a_live_pid_holds_the_slot_only_while_it_runs_the_recorded_program() {
        let now = chrono::Utc::now().timestamp();
        assert!(matches!(
            classify_now(&live_cargo(now)),
            Leftover::Running(_)
        ));
        let mut other = live_cargo(now);
        other.facts = Some(ProcessFacts {
            start_secs: Some(now),
            name: Some("postgres".into()),
            exe_name: Some("postgres".into()),
        });
        assert_eq!(classify_now(&other), Leftover::Clear);
        let truncated = ProcessFacts {
            start_secs: Some(now),
            name: Some("cargo-nextest-w".into()),
            exe_name: None,
        };
        let rec = record(
            "/x/cargo-nextest-wrapper run",
            Some(42),
            chrono::Utc::now().to_rfc3339(),
        );
        let table = Fake {
            alive: Ok(true),
            facts: Some(truncated),
        };
        assert!(matches!(classify(rec, &table), Leftover::Running(_)));
    }

    /// #8736 fail-open arm: the pid check itself failed.
    #[test]
    fn a_failed_pid_check_is_unknown() {
        let table = Fake {
            alive: Err("EINVAL".into()),
            facts: None,
        };
        assert!(matches!(classify_now(&table), Leftover::Unknown(e) if e.contains("EINVAL")));
    }

    /// #8736 fail-open arm: a live build whose start time cannot be read.
    #[test]
    fn an_unreadable_start_time_is_unknown() {
        let mut table = live_cargo(0);
        table.facts = Some(ProcessFacts {
            start_secs: None,
            ..table.facts.clone().expect("facts")
        });
        assert!(matches!(classify_now(&table), Leftover::Unknown(e) if e.contains("start time")));
        let absent = Fake {
            alive: Ok(true),
            facts: None,
        };
        assert!(
            matches!(classify_now(&absent), Leftover::Unknown(e) if e.contains("no process-table"))
        );
    }

    /// #8736 fail-open arm: a live build whose name and executable cannot be
    /// read.
    #[test]
    fn an_unreadable_process_name_is_unknown() {
        let table = Fake {
            alive: Ok(true),
            facts: Some(ProcessFacts {
                start_secs: Some(chrono::Utc::now().timestamp()),
                name: None,
                exe_name: None,
            }),
        };
        assert!(matches!(classify_now(&table), Leftover::Unknown(e) if e.contains("process name")));
    }
}
