//! A [`Tmux`] that answers each call from a script and records its argv.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::error::TmuxError;
use crate::tmux::{Permit, Tmux};

/// One scripted answer: stdout, or tmux's stderr for a failed call.
type Answer = Result<String, String>;

#[derive(Debug, Default)]
struct Script {
    queued: BTreeMap<String, VecDeque<Answer>>,
    fallback: BTreeMap<String, String>,
    calls: Vec<Vec<String>>,
}

/// A scripted [`Tmux`] for the send-path tests.
///
/// Why: [`crate::FakeTmux`] answers every call with an empty stdout, so it
/// cannot play a pane whose capture changes after a send, or a server that
/// fails one verb. The wake tests need per-call captures and owner answers
/// (#9171 A3b-1, the answers' "Gaps" line).
/// What: each verb (`args[0]`) has a FIFO of answers ([`ScriptedTmux::script`]
/// for stdout, [`ScriptedTmux::fail`] for a non-zero exit) and an optional
/// fallback answer used once its FIFO is empty. A verb with neither fails
/// with stderr `unscripted`, so a test never reads output it did not write.
/// Every call, `send-keys` included, is recorded before it is answered.
/// Test: `scripted_answers_in_order_then_falls_back_then_fails`,
/// `wake_refuses_table`.
#[derive(Debug, Default)]
pub struct ScriptedTmux {
    script: Mutex<Script>,
}

impl ScriptedTmux {
    /// A tmux with nothing scripted: every call fails.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues `stdout` as the next answer to `verb`.
    pub fn script(self, verb: &str, stdout: &str) -> Self {
        self.queue(verb, Ok(stdout.to_owned()))
    }

    /// Queues a failed call (exit 1 with `stderr`) as the next answer to `verb`.
    pub fn fail(self, verb: &str, stderr: &str) -> Self {
        self.queue(verb, Err(stderr.to_owned()))
    }

    /// Answers `verb` with `stdout` whenever its queue is empty.
    pub fn fallback(self, verb: &str, stdout: &str) -> Self {
        self.lock()
            .fallback
            .insert(verb.to_owned(), stdout.to_owned());
        self
    }

    /// Every argv this tmux was called with, oldest first.
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.lock().calls.clone()
    }

    /// The `send-keys` argvs only, oldest first.
    pub fn sends(&self) -> Vec<Vec<String>> {
        self.lock()
            .calls
            .iter()
            .filter(|c| c.first().is_some_and(|v| v == "send-keys"))
            .cloned()
            .collect()
    }

    fn queue(self, verb: &str, answer: Answer) -> Self {
        self.lock()
            .queued
            .entry(verb.to_owned())
            .or_default()
            .push_back(answer);
        self
    }

    // Each mutation is a single push or pop, so a poisoned guard still holds a
    // whole script.
    fn lock(&self) -> MutexGuard<'_, Script> {
        self.script.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Tmux for ScriptedTmux {
    fn execute(&self, args: &[&str], _permit: Permit) -> Result<String, TmuxError> {
        let verb = (*args.first().ok_or(TmuxError::EmptyCommand)?).to_owned();
        let mut script = self.lock();
        script
            .calls
            .push(args.iter().map(|a| (*a).to_owned()).collect());
        let answer = match script.queued.get_mut(&verb).and_then(VecDeque::pop_front) {
            Some(answer) => answer,
            None => script
                .fallback
                .get(&verb)
                .cloned()
                .ok_or_else(|| "unscripted".to_owned()),
        };
        answer.map_err(|stderr| TmuxError::Failed {
            verb,
            code: Some(1),
            stderr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_answers_in_order_then_falls_back_then_fails() {
        let tmux = ScriptedTmux::new()
            .script("capture-pane", "one")
            .fail("capture-pane", "boom")
            .fallback("capture-pane", "rest");
        let out: Vec<String> = (0..4)
            .map(|_| match tmux.run(&["capture-pane", "-t", "%1"]) {
                Ok(s) => s,
                Err(TmuxError::Failed { stderr, .. }) => format!("err {stderr}"),
                Err(e) => format!("other {e}"),
            })
            .collect();
        assert_eq!(out, ["one", "err boom", "rest", "rest"]);
        assert!(matches!(
            tmux.run(&["list-panes"]),
            Err(TmuxError::Failed { ref stderr, .. }) if stderr == "unscripted"
        ));
        assert_eq!(tmux.calls().len(), 5);
        assert!(tmux.sends().is_empty());
        // `run` still refuses send-keys before the script sees it.
        assert!(matches!(
            tmux.run(&["send-keys", "-t", "%1", "x"]),
            Err(TmuxError::SendKeysRefused)
        ));
        assert_eq!(tmux.calls().len(), 5);
    }
}
