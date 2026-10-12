//! A [`Tmux`] that records calls instead of running tmux.

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::error::TmuxError;
use crate::tmux::Tmux;

/// A recording [`Tmux`] for tests in this crate and its callers.
///
/// Why: the send path must be proven to send, or not send, without a live tmux
/// server; the recorded argv is the evidence.
/// What: every [`Tmux::run`] appends its argv to a call log and returns an
/// empty stdout. Thread-safe, so a threaded poller can share one.
/// Test: `send_guarded_refuses_typed_and_sends_on_empty`.
#[derive(Debug, Default)]
pub struct FakeTmux {
    calls: Mutex<Vec<Vec<String>>>,
}

impl FakeTmux {
    /// A fake with an empty call log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every argv passed to [`Tmux::run`], oldest first.
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.log().clone()
    }

    // A panic in another holder cannot leave the log half-written: a push is
    // the only mutation, so a poisoned guard still holds a whole log.
    fn log(&self) -> MutexGuard<'_, Vec<Vec<String>>> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Tmux for FakeTmux {
    fn run(&self, args: &[&str]) -> Result<String, TmuxError> {
        if args.is_empty() {
            return Err(TmuxError::EmptyCommand);
        }
        self.log()
            .push(args.iter().map(|a| (*a).to_owned()).collect());
        Ok(String::new())
    }
}
