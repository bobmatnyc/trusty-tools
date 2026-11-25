//! A [`Tmux`] that records calls instead of running tmux.

use crate::error::TmuxError;
use crate::scripted::ScriptedTmux;
use crate::tmux::{Permit, Tmux};

/// A recording [`Tmux`] for tests in this crate and its callers.
///
/// Why: the send path must be proven to send, or not send, without a live tmux
/// server; the recorded argv is the evidence.
/// What: a [`ScriptedTmux`] whose every verb answers an empty stdout (#10571
/// item E). Every call that reaches the fake (`run` for any verb but
/// `send-keys`, and the guarded send path) appends its argv to a call log.
/// Thread-safe, so a threaded poller can share one.
/// Test: `send_guarded_refuses_typed_and_sends_on_empty`,
/// `fake_answers_every_verb_empty_and_records_it`.
#[derive(Debug)]
pub struct FakeTmux {
    inner: ScriptedTmux,
}

impl Default for FakeTmux {
    fn default() -> Self {
        Self {
            inner: ScriptedTmux::new().fallback_any(""),
        }
    }
}

impl FakeTmux {
    /// A fake with an empty call log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every argv passed to [`Tmux::run`], oldest first.
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.inner.calls()
    }
}

impl Tmux for FakeTmux {
    fn execute(&self, args: &[&str], permit: Permit) -> Result<String, TmuxError> {
        self.inner.execute(args, permit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_answers_every_verb_empty_and_records_it() {
        let fake = FakeTmux::default();
        assert_eq!(
            fake.run(&["capture-pane", "-t", "%1"]).ok(),
            Some(String::new())
        );
        assert_eq!(fake.run(&["any-verb-at-all"]).ok(), Some(String::new()));
        assert!(matches!(fake.run(&[]), Err(TmuxError::EmptyCommand)));
        assert!(matches!(
            fake.run(&["send-keys", "-t", "%1", "x"]),
            Err(TmuxError::SendKeysRefused)
        ));
        assert_eq!(
            fake.calls(),
            [vec!["capture-pane", "-t", "%1"], vec!["any-verb-at-all"]]
        );
    }
}
