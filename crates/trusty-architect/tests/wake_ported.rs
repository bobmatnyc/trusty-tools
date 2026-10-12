//! #9171 A3b-1: the prototype's `wake()`, `dispatch_wake()` and
//! `send_to_supervisor()` tests, ported by name against
//! `trusty_architect::poller::{Waker, dispatch_wake}`.
//!
//! Source: `tests/test_fleet_poll.py` at Architect commit 988152c (plan cases
//! 1-10). Each test carries a `// Python: <name>` line; `tests/ported_names.rs`
//! checks the set against `tests/fixtures/a3/python_tests_a3b.txt`. The
//! prototype tests replace `capture` and `send_to_supervisor`; the ports
//! script tmux itself (`ScriptedTmux`), so every `display-message` owner
//! check and every `send-keys` is in the recorded argv. Three cases are
//! rewritten, each named at its test: one by Architect ruling (A3b Q1), two
//! because the frozen test's three-argument fake `wake` raises `TypeError`
//! against the live four-argument call.

mod support;

use serde_json::Value;
use support::{
    ALERTS, NOW, TestResult, owned_tmux, pointer, screen, sent, sup_pane, waker, wrapped,
};
use trusty_architect::classify::{BoxReading, parse_pane};
use trusty_architect::poller::{PollState, StoredCount, dispatch_wake};
use trusty_architect::{FakeClock, InputState, ScriptedTmux, WakeError};

/// A key as `poll-state.json` would hold it (#9171 A3b-2: the `wake_*` keys
/// are typed fields).
fn key(state: &PollState, key: &str) -> Option<Value> {
    serde_json::to_value(state).ok()?.get(key).cloned()
}

// Python: test_wake_reports_no_input_box_found_not_a_misleading_box_state
#[test]
fn test_wake_reports_no_input_box_found_not_a_misleading_box_state() -> TestResult {
    let cap = "❯\n\n  ✻ Doing work… (2m 14s · esc to interrupt)\n";
    let tmux = owned_tmux().fallback("capture-pane", cap);
    let clock = FakeClock::new(NOW);
    assert!(!parse_pane(cap).has_box, "the actual cause");
    assert_eq!(
        waker(&tmux, &clock, ALERTS).wake(&sup_pane()?, 1, None)?,
        "skipped: no input box found"
    );
    assert!(tmux.sends().is_empty());
    Ok(())
}

// Python: test_wake_reports_input_typed_when_a_real_box_holds_a_draft
#[test]
fn test_wake_reports_input_typed_when_a_real_box_holds_a_draft() -> TestResult {
    let cap = screen("half-typed reply", false);
    let tmux = owned_tmux().fallback("capture-pane", &cap);
    let clock = FakeClock::new(NOW);
    assert!(parse_pane(&cap).has_box);
    assert_eq!(
        waker(&tmux, &clock, ALERTS).wake(&sup_pane()?, 1, None)?,
        "skipped: input typed"
    );
    assert!(tmux.sends().is_empty());
    Ok(())
}

// The prototype test bypasses the owner check; here display-message is
// scripted and recorded before each send.
// Python: test_wake_confirms_a_message_that_wraps_across_screen_rows
#[test]
fn test_wake_confirms_a_message_that_wraps_across_screen_rows() -> TestResult {
    let msg = pointer(ALERTS, 3, None)?;
    let tmux = owned_tmux()
        .script("capture-pane", &screen("", false))
        .script("capture-pane", &wrapped(&msg))
        .script("capture-pane", &screen("", false));
    let clock = FakeClock::new(NOW);
    let result = waker(&tmux, &clock, ALERTS).wake(&sup_pane()?, 3, None)?;
    assert!(result.starts_with("sent to"), "{result}");
    assert!(
        msg.starts_with("[poll ") && msg.contains("3 new alerts ->"),
        "{msg}"
    );
    assert_eq!(
        sent(&tmux),
        [
            vec!["-l".to_owned(), "--".to_owned(), msg],
            vec!["Enter".to_owned()]
        ]
    );
    Ok(())
}

// Python (diverges by ruling A3b Q1): the prototype sends BSpace x len(msg)
// into a box that stayed empty. An empty box after typing now sends nothing
// further, and the result says the box changed.
// Python: test_wake_cleans_up_its_own_text_when_confirmation_fails
#[test]
fn test_wake_cleans_up_its_own_text_when_confirmation_fails() -> TestResult {
    let tmux = owned_tmux().fallback("capture-pane", &screen("", false)); // box stays empty
    let clock = FakeClock::new(NOW);
    let result = waker(&tmux, &clock, ALERTS).wake(&sup_pane()?, 2, None)?;
    assert!(
        result.starts_with("failed:") && result.contains("box changed"),
        "{result}"
    );
    let sends = sent(&tmux);
    assert_eq!(sends.len(), 1, "only the typed text: {sends:?}");
    assert_eq!(sends[0][0], "-l");
    assert!(
        !sends.iter().flatten().any(|k| k == "BSpace"),
        "no cleanup into an empty box"
    );
    assert!(
        !sends.iter().flatten().any(|k| k == "Enter"),
        "never submits unconfirmed text"
    );
    Ok(())
}

// Python: test_wake_detects_its_own_stale_pointer_and_does_not_send
#[test]
fn test_wake_detects_its_own_stale_pointer_and_does_not_send() -> TestResult {
    let stale = "[poll 11:15Z] 4 new alerts -> /Users/x/inbox/alerts.jsonl";
    let cap = wrapped(stale);
    let tmux = owned_tmux().fallback("capture-pane", &cap);
    let clock = FakeClock::new(NOW);
    assert_eq!(
        parse_pane(&cap).box_reading,
        BoxReading::Input(InputState::Typed)
    );
    assert_eq!(
        waker(&tmux, &clock, ALERTS).wake(&sup_pane()?, 1, None)?,
        "skipped: own stale pointer in box"
    );
    assert!(tmux.sends().is_empty());
    Ok(())
}

// Python: test_wake_does_not_mistake_a_human_draft_starting_with_poll_for_its_own
#[test]
fn test_wake_does_not_mistake_a_human_draft_starting_with_poll_for_its_own() -> TestResult {
    let tmux = owned_tmux().fallback(
        "capture-pane",
        &screen("poll the team about the release date", false),
    );
    let clock = FakeClock::new(NOW);
    assert_eq!(
        waker(&tmux, &clock, ALERTS).wake(&sup_pane()?, 1, None)?,
        "skipped: input typed"
    );
    Ok(())
}

// Rewritten: the frozen test's fake `wake(n, panes, dry_run)` raises
// TypeError against the live four-argument call. The fake here takes the
// live arguments, and the test also asserts `wake_sent_at` is set on `sent`.
// Python: test_dispatch_wake_carries_pending_across_a_skip_then_wakes_once
#[test]
fn test_dispatch_wake_carries_pending_across_a_skip_then_wakes_once() {
    let clock = FakeClock::new(NOW);
    let mut calls = Vec::new();
    let mut state = PollState::default();
    let r1 = dispatch_wake(&mut state, 3, None, false, &clock, |n, logged| {
        calls.push((n, logged));
        Ok("skipped: no input box found".to_owned())
    });
    assert_eq!(r1, "skipped: no input box found");
    assert_eq!(key(&state, "wake_pending"), Some(Value::from(3)));
    let r2 = dispatch_wake(&mut state, 2, None, false, &clock, |n, logged| {
        calls.push((n, logged));
        Ok("sent to %1".to_owned())
    });
    assert_eq!(r2, "sent to %1");
    assert_eq!(
        calls,
        [(3, None), (5, None)],
        "one wake, carrying both cycles"
    );
    assert_eq!(key(&state, "wake_pending"), Some(Value::from(0)));
    assert_eq!(key(&state, "wake_sent_at"), Some(Value::from(NOW)));
}

// Rewritten for the same TypeError; also asserts `wake_sent_at` stays unset.
// Python: test_dispatch_wake_carries_pending_on_a_failed_confirm
#[test]
fn test_dispatch_wake_carries_pending_on_a_failed_confirm() {
    let clock = FakeClock::new(NOW);
    let mut calls = Vec::new();
    let mut state = PollState::default();
    for new in [4, 0] {
        dispatch_wake(&mut state, new, None, false, &clock, |n, logged| {
            calls.push((n, logged));
            Ok("failed: text not confirmed in the input box; Enter not sent".to_owned())
        });
    }
    assert_eq!(calls, [(4, None), (4, None)], "pending still retried");
    assert_eq!(key(&state, "wake_pending"), Some(Value::from(4)));
    assert_eq!(key(&state, "wake_sent_at"), None);
}

// Python: test_dispatch_wake_none_when_nothing_pending
#[test]
fn test_dispatch_wake_none_when_nothing_pending() {
    let clock = FakeClock::new(NOW);
    let mut state = PollState {
        wake_logged: Some(StoredCount::Count(2)),
        ..PollState::default()
    };
    let mut called = false;
    let result = dispatch_wake(&mut state, 0, None, false, &clock, |_, _| {
        called = true;
        Ok("sent to %1".to_owned())
    });
    assert_eq!(result, "none");
    assert!(!called, "nothing pending: no wake");
    assert_eq!(key(&state, "wake_pending"), Some(Value::from(0)));
    assert_eq!(key(&state, "wake_logged"), None, "the footnote is dropped");
}

// The prototype raises TmuxError from `send_to_supervisor`; the port returns
// `WakeError::NotOwner` from the same owner check inside `wake`.
// Python: test_send_refuses_any_pane_outside_tm_supervisor
#[test]
fn test_send_refuses_any_pane_outside_tm_supervisor() -> TestResult {
    let tmux = ScriptedTmux::new()
        .fallback("capture-pane", &screen("", false))
        .fallback("display-message", "tm-api\n")
        .fallback("send-keys", "");
    let clock = FakeClock::new(NOW);
    let err = waker(&tmux, &clock, ALERTS).wake(&sup_pane()?, 1, None);
    assert!(
        matches!(err, Err(WakeError::NotOwner { ref pane, ref session }) if pane == "%1" && session == "tm-api"),
        "{err:?}"
    );
    assert!(!tmux.calls().iter().any(|c| c[0] == "send-keys"));
    Ok(())
}
