//! Test doubles for the grant registry: a fake clock (#9070), and the fake
//! process table re-exported from `ancestry_fakes` (#9066).
//!
//! Why: AC 7 — the ancestry and expiry rules are proven against a process
//! table and a clock the test controls.
//! What: [`FakeClock`] holds a settable time and can fail. [`FakeProcs`]
//! moved beside the `ancestry` feature (#9066); this path still names it.
//! Test: used by `grant_tests.rs` and `wire_tests.rs`.

use std::sync::Mutex;
use std::time::Duration;

pub(crate) use crate::ancestry_fakes::FakeProcs;

use super::grant::{Clock, ClockError};

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
