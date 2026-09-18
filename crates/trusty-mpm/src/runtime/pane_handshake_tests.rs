//! Tests for the pre-launch pane handshake (#8233 review round 2, findings 1
//! and 8).
//!
//! Why: the handshake is the only thing standing between a launch and a shell
//! that is not reading its tty, so every branch of it needs a driver whose
//! behaviour is chosen by the test rather than by a real tmux.
//! What: [`ScriptedShell`] is a fake pane whose response to the probe is
//! programmable — answer it, ignore it, or ignore it until an interrupt arrives.
//! Test: this file.

use std::sync::Mutex;
use std::time::Duration;

use super::{PaneState, confirm_prompt, continuation_kind, probe_reply};
use crate::session_manager::{ManagedError, ManagedTmuxDriver};

/// A test interval short enough that a whole exhausted probe costs milliseconds.
const FAST: Duration = Duration::from_millis(1);

/// How a [`ScriptedShell`] behaves toward the probe.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shell {
    /// A working shell: the probe is executed and prints.
    Executes,
    /// A shell that never executes anything (still in its init hooks).
    Deaf,
    /// Wedged at a continuation prompt; an interrupt frees it.
    WedgedUntilInterrupt,
    /// Wedged and stays wedged, interrupt or not.
    WedgedForever,
    /// tmux cannot read this pane at all.
    Unobservable,
}

/// A fake pane with a programmable relationship to the probe.
struct ScriptedShell {
    behaviour: Shell,
    text: Mutex<String>,
    interrupts: Mutex<usize>,
    sent: Mutex<Vec<String>>,
    freed: Mutex<bool>,
}

impl ScriptedShell {
    fn new(behaviour: Shell) -> Self {
        let text = match behaviour {
            Shell::WedgedUntilInterrupt | Shell::WedgedForever => "quote>".to_owned(),
            _ => "~/work %".to_owned(),
        };
        Self {
            behaviour,
            text: Mutex::new(text),
            interrupts: Mutex::new(0),
            sent: Mutex::new(Vec::new()),
            freed: Mutex::new(false),
        }
    }

    fn wedged_now(&self) -> bool {
        match self.behaviour {
            Shell::WedgedForever => true,
            Shell::WedgedUntilInterrupt => !*self.freed.lock().expect("freed"),
            _ => false,
        }
    }

    fn interrupt(&self) {
        *self.interrupts.lock().expect("interrupts") += 1;
        if self.behaviour == Shell::WedgedUntilInterrupt {
            *self.freed.lock().expect("freed") = true;
            *self.text.lock().expect("text") = "~/work %".to_owned();
        }
    }

    fn run(&self, line: &str) {
        self.sent.lock().expect("sent").push(line.to_owned());
        if self.behaviour == Shell::Deaf || self.wedged_now() {
            return;
        }
        if let Some(out) = probe_reply(line) {
            let mut t = self.text.lock().expect("text");
            t.push('\n');
            t.push_str(&out);
        }
    }
}

impl ManagedTmuxDriver for ScriptedShell {
    fn create_session(&self, _n: &str, _w: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, _n: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, _n: &str, text: &str) -> Result<(), ManagedError> {
        self.run(text);
        Ok(())
    }
    fn send_line_to_pane(&self, _n: &str, _p: &str, text: &str) -> Result<(), ManagedError> {
        self.run(text);
        Ok(())
    }
    fn send_interrupt(&self, _n: &str) -> Result<(), ManagedError> {
        self.interrupt();
        Ok(())
    }
    fn send_interrupt_to_pane(&self, _n: &str, _p: &str) -> Result<(), ManagedError> {
        self.interrupt();
        Ok(())
    }
    fn capture(&self, _n: &str, _l: usize) -> Result<String, ManagedError> {
        if self.behaviour == Shell::Unobservable {
            return Err(ManagedError::TmuxUnavailable("no pane".into()));
        }
        Ok(self.text.lock().expect("text").clone())
    }
    fn capture_pane(&self, n: &str, _p: &str, l: usize) -> Result<String, ManagedError> {
        self.capture(n, l)
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
}

fn confirm(shell: &ScriptedShell) -> PaneState {
    confirm_prompt(shell, "tmpm-x", Some("%1"), 2, 4, FAST)
}

#[test]
fn confirm_prompt_is_ready_when_the_probe_echoes() {
    assert_eq!(confirm(&ScriptedShell::new(Shell::Executes)), PaneState::Ready);
}

#[test]
fn confirm_prompt_reports_an_unresponsive_shell() {
    // A pane at an ordinary prompt that nonetheless never runs anything — the
    // `direnv`-stalled init hook this issue's live captures show.
    assert_eq!(
        confirm(&ScriptedShell::new(Shell::Deaf)),
        PaneState::Unresponsive
    );
}

#[test]
fn confirm_prompt_reports_a_wedged_continuation_prompt() {
    // #8233 finding 8: distinct from "no prompt yet", and it names the marker.
    assert_eq!(
        confirm(&ScriptedShell::new(Shell::WedgedForever)),
        PaneState::Continuation("quote>".to_owned())
    );
}

#[test]
fn confirm_prompt_interrupts_a_continuation_prompt_before_probing() {
    let shell = ScriptedShell::new(Shell::WedgedForever);
    let _ = confirm(&shell);
    assert!(
        *shell.interrupts.lock().expect("interrupts") > 0,
        "a visibly wedged pane must be flushed, not probed into"
    );
}

#[test]
fn confirm_prompt_recovers_a_pane_that_frees_up_after_the_interrupt() {
    // The live recovery an operator had to perform by hand: C-c, then resume.
    let shell = ScriptedShell::new(Shell::WedgedUntilInterrupt);
    assert_eq!(confirm(&shell), PaneState::Ready);
}

#[test]
fn confirm_prompt_never_sends_a_closing_quote_or_a_bare_enter() {
    // #8233 finding 8: recovery is an interrupt. A closing quote or a bare
    // Enter would RUN whatever the open construct had accumulated — on the live
    // pane, three concatenated launch commands.
    let shell = ScriptedShell::new(Shell::WedgedForever);
    let _ = confirm(&shell);
    for line in shell.sent.lock().expect("sent").iter() {
        assert!(
            probe_reply(line).is_some(),
            "only probe lines may be typed at a wedged pane, got {line:?}"
        );
    }
}

#[test]
fn confirm_prompt_is_unobservable_when_the_driver_cannot_capture() {
    let shell = ScriptedShell::new(Shell::Unobservable);
    let state = confirm(&shell);
    assert_eq!(state, PaneState::Unobservable);
    assert!(
        shell.sent.lock().expect("sent").is_empty(),
        "an unobservable pane must cost no probe and no delay"
    );
}

#[test]
fn unobservable_panes_are_still_launched_into() {
    assert!(PaneState::Unobservable.may_launch());
    assert!(PaneState::Ready.may_launch());
    assert!(!PaneState::Unresponsive.may_launch());
    assert!(!PaneState::Continuation("quote>".to_owned()).may_launch());
}

#[test]
fn a_wedged_pane_names_the_continuation_marker() {
    let msg = PaneState::Continuation("dquote>".to_owned())
        .refusal()
        .expect("a wedged pane is a refusal");
    assert!(msg.contains("dquote>"), "{msg}");
    assert!(msg.contains("Nothing was typed"), "{msg}");
}

#[test]
fn an_unresponsive_pane_says_nothing_was_reading_the_tty() {
    let msg = PaneState::Unresponsive
        .refusal()
        .expect("an unresponsive pane is a refusal");
    assert!(msg.contains("never executed a probe"), "{msg}");
    assert!(PaneState::Ready.refusal().is_none());
    assert!(PaneState::Unobservable.refusal().is_none());
}

#[test]
fn continuation_kind_recognises_an_open_quote() {
    // The exact tail captured from session tm-apex-companion.
    assert_eq!(continuation_kind("some output\nquote> "), Some("quote>"));
}

#[test]
fn continuation_kind_recognises_a_composed_zsh_prompt() {
    // The exact tail captured from session tm-writing-01.
    assert_eq!(continuation_kind("cmdand cursh>"), Some("cursh>"));
}

#[test]
fn continuation_kind_ignores_an_ordinary_prompt() {
    assert_eq!(continuation_kind("masa@host ~/work %"), None);
    // A bare `>` is bash's PS2 AND a common prompt suffix; matching it would
    // refuse healthy launches.
    assert_eq!(continuation_kind("some-prompt >"), None);
}

#[test]
fn continuation_kind_ignores_empty_output() {
    assert_eq!(continuation_kind(""), None);
    assert_eq!(continuation_kind("\n\n   \n"), None);
}

#[test]
fn the_probe_line_does_not_contain_its_own_output() {
    let shell = ScriptedShell::new(Shell::Executes);
    assert_eq!(confirm(&shell), PaneState::Ready);
    let sent = shell.sent.lock().expect("sent");
    let typed = sent.first().expect("a probe was typed");
    let printed = probe_reply(typed).expect("it is a probe line");
    assert!(
        !typed.contains(&printed),
        "the echo of the keystrokes must not satisfy the scrape: {typed:?}"
    );
    assert!(
        typed.len() < crate::core::tmux::MAX_PANE_COMMAND_BYTES,
        "the probe must never be truncatable itself"
    );
}

#[test]
fn probe_reply_answers_only_a_probe_line() {
    assert!(probe_reply("cd /tmp && claude").is_none());
    assert!(probe_reply("echo hello").is_none());
    assert!(probe_reply("echo tm-rea\"dy\"-abc123").is_some());
}
