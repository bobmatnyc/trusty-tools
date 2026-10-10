//! Test doubles for the grant registry: a fake process table and clock (#9070).
//!
//! Why: AC 7 — the ancestry and expiry rules are proven against a process
//! table and a clock the test controls.
//! What: [`FakeProcs`] maps pid to (parent, start time) and can fail every
//! read, fail one pid's start time, or panic on one pid. [`FakeClock`]
//! holds a settable time and can fail.
//! Test: used by `grant_tests.rs` and `ancestry_tests.rs`.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use super::ancestry::{ProcessError, ProcessTable, StartTime};
use super::grant::{Clock, ClockError};

#[derive(Default)]
struct ProcState {
    procs: HashMap<u32, (u32, u64)>,
    unreadable: bool,
    start_unreadable: HashSet<u32>,
    panic_on: Option<u32>,
}

/// A process table the test edits.
#[derive(Default)]
pub(crate) struct FakeProcs {
    state: Mutex<ProcState>,
}

impl FakeProcs {
    fn with<R>(&self, f: impl FnOnce(&mut ProcState) -> R) -> R {
        let mut guard = self.state.lock().expect("fake process table lock");
        f(&mut guard)
    }

    /// Add or replace `pid` with `parent` and `start`.
    pub(crate) fn add(&self, pid: u32, parent: u32, start: u64) -> &Self {
        self.with(|s| s.procs.insert(pid, (parent, start)));
        self
    }

    /// init(1) -> parent(10) -> { child(20, start 200) -> grandchild(30),
    /// sibling(21) }; unrelated(50) hangs off init.
    pub(crate) fn sample_tree() -> Self {
        let procs = Self::default();
        procs
            .add(1, 0, 1)
            .add(10, 1, 100)
            .add(20, 10, 200)
            .add(30, 20, 300)
            .add(21, 10, 210)
            .add(50, 1, 500);
        procs
    }

    /// Fail every read with `Unreadable`.
    pub(crate) fn set_unreadable(&self) {
        self.with(|s| s.unreadable = true);
    }

    /// Fail `pid`'s start time with `StartTimeUnreadable`.
    pub(crate) fn set_start_unreadable(&self, pid: u32) {
        self.with(|s| s.start_unreadable.insert(pid));
    }

    /// Panic when `pid`'s start time is read.
    pub(crate) fn panic_on_start(&self, pid: u32) {
        self.with(|s| s.panic_on = Some(pid));
    }
}

impl ProcessTable for FakeProcs {
    fn parent(&self, pid: u32) -> Result<u32, ProcessError> {
        self.with(|s| {
            if s.unreadable {
                return Err(ProcessError::Unreadable { pid });
            }
            s.procs
                .get(&pid)
                .map(|(parent, _)| *parent)
                .ok_or(ProcessError::Unreadable { pid })
        })
    }

    fn start_time(&self, pid: u32) -> Result<StartTime, ProcessError> {
        // The lock is released before the panic, so only the caller's poisons.
        let (panic, result) = self.with(|s| {
            let result = if s.unreadable {
                Err(ProcessError::Unreadable { pid })
            } else if s.start_unreadable.contains(&pid) {
                Err(ProcessError::StartTimeUnreadable { pid })
            } else {
                s.procs
                    .get(&pid)
                    .map(|(_, start)| StartTime::from_raw(*start))
                    .ok_or(ProcessError::Unreadable { pid })
            };
            (s.panic_on == Some(pid), result)
        });
        assert!(!panic, "fake process table: injected panic for pid {pid}");
        result
    }
}

/// A clock the test sets. `None` is a clock failure.
pub(crate) struct FakeClock {
    now: Mutex<Option<Duration>>,
}

impl FakeClock {
    /// A clock reading `secs` seconds since the epoch.
    pub(crate) fn at(secs: u64) -> Self {
        Self {
            now: Mutex::new(Some(Duration::from_secs(secs))),
        }
    }

    /// Set the clock to `secs`.
    pub(crate) fn set(&self, secs: u64) {
        *self.now.lock().expect("fake clock lock") = Some(Duration::from_secs(secs));
    }

    /// Make every read fail.
    pub(crate) fn fail(&self) {
        *self.now.lock().expect("fake clock lock") = None;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Result<Duration, ClockError> {
        self.now.lock().expect("fake clock lock").ok_or(ClockError)
    }
}
