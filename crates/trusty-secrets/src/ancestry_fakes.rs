//! A fake process table for the ancestry tests (#9070, #9066).
//!
//! Why: AC 7 of #9070 — the ancestry rules are proven against a process
//! table the test controls. #9066: it lives beside `ancestry` so an
//! `ancestry`-only test run builds it with no `server` code.
//! What: [`FakeProcs`] maps pid to (parent, start time) and can fail every
//! read, fail one pid's start time, panic on one pid, mark a pid as
//! Claude Code, or give a pid an executable path (#9066).
//! Test: used by `ancestry_tests.rs`, `grant_tests.rs` and `wire_tests.rs`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use crate::ancestry::{ProcessError, ProcessTable, StartTime};

#[derive(Default)]
struct ProcState {
    procs: HashMap<u32, (u32, u64)>,
    unreadable: bool,
    start_unreadable: HashSet<u32>,
    panic_on: Option<u32>,
    agents: HashSet<u32>,
    // #9066: the executable each pid runs; a pid with none is unreadable.
    exes: HashMap<u32, PathBuf>,
    parent_delay: Option<Duration>,
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

    /// Remove `pid`, as when the process exits (#9621).
    pub(crate) fn remove(&self, pid: u32) {
        self.with(|s| s.procs.remove(&pid));
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

    /// Mark `pid` as a Claude Code process (#9070 slice 2).
    pub(crate) fn set_agent(&self, pid: u32) {
        self.with(|s| s.agents.insert(pid));
    }

    /// Give `pid` the executable `path` (#9066).
    pub(crate) fn set_executable(&self, pid: u32, path: &str) {
        self.with(|s| s.exes.insert(pid, PathBuf::from(path)));
    }

    /// Panic when `pid`'s start time is read.
    pub(crate) fn panic_on_start(&self, pid: u32) {
        self.with(|s| s.panic_on = Some(pid));
    }

    /// Sleep `delay` in every `parent` read, outside this fake's own lock, so
    /// racing callers overlap inside the ancestry walk.
    pub(crate) fn set_parent_delay(&self, delay: Duration) {
        self.with(|s| s.parent_delay = Some(delay));
    }
}

impl ProcessTable for FakeProcs {
    fn parent(&self, pid: u32) -> Result<u32, ProcessError> {
        if let Some(delay) = self.with(|s| s.parent_delay) {
            std::thread::sleep(delay);
        }
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

    fn is_agent(&self, pid: u32) -> Result<bool, ProcessError> {
        self.with(|s| {
            if s.unreadable || !s.procs.contains_key(&pid) {
                return Err(ProcessError::Unreadable { pid });
            }
            Ok(s.agents.contains(&pid))
        })
    }

    fn executable(&self, pid: u32) -> Result<PathBuf, ProcessError> {
        self.with(|s| {
            if s.unreadable {
                return Err(ProcessError::Unreadable { pid });
            }
            s.exes
                .get(&pid)
                .cloned()
                .ok_or(ProcessError::Unreadable { pid })
        })
    }
}
