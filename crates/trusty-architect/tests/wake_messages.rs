//! #9167 PR2: a submitted wake is recorded once through the caller's
//! [`MessageSink`]; a sink error never stops the wake, and a wake that was not
//! submitted records nothing.
//!
//! Every tmux call goes to a `ScriptedTmux`; nothing touches a tmux server.

mod support;

use std::cell::RefCell;

use support::{ALERTS, NOW, TestResult, owned_tmux, pointer, screen, sent, sup_pane, waker};
use trusty_architect::poller::{
    MessageSink, PaneMessage, PaneMessageKind, PollState, StoredCount, Waker,
};
use trusty_architect::{FakeClock, MessageSinkError, ScriptedTmux};

/// One recorded call, owned.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Recorded {
    kind: PaneMessageKind,
    session: String,
    pane: String,
    text: String,
}

/// A sink that keeps every call and answers `fail` when set.
#[derive(Default)]
struct Recorder {
    calls: RefCell<Vec<Recorded>>,
    fail: Option<&'static str>,
}

impl MessageSink for Recorder {
    fn record(&self, m: &PaneMessage<'_>) -> Result<(), MessageSinkError> {
        self.calls.borrow_mut().push(Recorded {
            kind: m.kind,
            session: m.session.to_owned(),
            pane: m.pane.to_owned(),
            text: m.text.to_owned(),
        });
        match self.fail {
            Some(reason) => Err(MessageSinkError(reason.to_owned())),
            None => Ok(()),
        }
    }
}

/// A tmux whose box is empty, then holds `msg` after typing (confirmed).
fn confirming(msg: &str) -> ScriptedTmux {
    let empty = screen("", false);
    owned_tmux()
        .script("capture-pane", &empty)
        .script("capture-pane", &screen(msg, false))
        .fallback("capture-pane", &empty)
}

fn with_sink<'a>(
    tmux: &'a ScriptedTmux,
    clock: &'a FakeClock,
    sink: &'a Recorder,
) -> Waker<'a, ScriptedTmux, FakeClock> {
    Waker {
        messages: sink,
        ..waker(tmux, clock, ALERTS)
    }
}

/// A wake that Enter submitted is one `PollWake` record naming the session,
/// the pane and the exact text typed, recorded after the Enter.
#[test]
fn a_submitted_wake_is_recorded_once_with_the_typed_text_9167() -> TestResult {
    let msg = pointer(ALERTS, 3, None)?;
    let tmux = confirming(&msg);
    let clock = FakeClock::new(NOW);
    let sink = Recorder::default();
    let mut state = PollState::default();
    let result = with_sink(&tmux, &clock, &sink).dispatch(&mut state, &sup_pane()?, 3, None);
    assert_eq!(result, "sent to %1");
    let calls = sink.calls.borrow();
    assert_eq!(
        *calls,
        vec![Recorded {
            kind: PaneMessageKind::PollWake,
            session: "tm-architect".to_owned(),
            pane: "%1".to_owned(),
            text: msg.clone(),
        }]
    );
    // The typed literal is exactly the recorded text, and Enter followed it.
    let keys = sent(&tmux);
    assert!(keys.iter().any(|k| k.last() == Some(&msg)), "{keys:?}");
    assert_eq!(
        keys.last().map(Vec::as_slice),
        Some(&["Enter".to_owned()][..])
    );
    Ok(())
}

/// Fail-Open Check: a sink that errors does not stop the wake. Enter is still
/// sent, the count is still cleared, and the reason is on the logged phrase.
#[test]
fn a_sink_error_never_stops_the_wake_and_is_logged_9167() -> TestResult {
    let msg = pointer(ALERTS, 2, None)?;
    let tmux = confirming(&msg);
    let clock = FakeClock::new(NOW);
    let sink = Recorder {
        fail: Some("trusty-events unreachable"),
        ..Recorder::default()
    };
    let mut state = PollState::default();
    let result = with_sink(&tmux, &clock, &sink).dispatch(&mut state, &sup_pane()?, 2, None);
    assert_eq!(
        result,
        "sent to %1 (message event not recorded: trusty-events unreachable)"
    );
    assert!(
        sent(&tmux).iter().any(|k| k == &["Enter"]),
        "Enter was sent"
    );
    assert_eq!(state.wake_pending, Some(StoredCount::Count(0)));
    assert_eq!(sink.calls.borrow().len(), 1, "one attempt, no retry loop");
    Ok(())
}

/// A wake that was skipped, dry-run, or typed but never submitted is not a
/// message, so the sink is never called.
#[test]
fn a_wake_that_was_not_submitted_records_nothing_9167() -> TestResult {
    let msg = pointer(ALERTS, 1, None)?;
    let empty = screen("", false);
    let cases: Vec<(&str, ScriptedTmux, bool)> = vec![
        (
            "typed draft: skipped",
            owned_tmux().fallback("capture-pane", &screen("draft", false)),
            false,
        ),
        (
            "box changed: not confirmed",
            owned_tmux()
                .script("capture-pane", &empty)
                .script("capture-pane", &screen("someone typing", false)),
            false,
        ),
        (
            "own pointer erased",
            owned_tmux()
                .script("capture-pane", &empty)
                .script("capture-pane", &screen(&format!("x{msg}"), false))
                .fallback("capture-pane", &empty),
            false,
        ),
        (
            "dry run",
            owned_tmux().fallback("capture-pane", &empty),
            true,
        ),
    ];
    for (name, tmux, dry_run) in cases {
        let clock = FakeClock::new(NOW);
        let sink = Recorder::default();
        let waker = Waker {
            dry_run,
            ..with_sink(&tmux, &clock, &sink)
        };
        let result = waker.wake(&sup_pane()?, 1, None)?;
        assert!(!result.starts_with("sent"), "{name}: {result}");
        assert!(sink.calls.borrow().is_empty(), "{name}: {result}");
    }
    Ok(())
}
