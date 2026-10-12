//! #9171 A3b-1: the send path's acceptance criteria, with no Python source.
//!
//! - AC5 `wake_refuses_table`: wake sends nothing unless the live pane belongs
//!   to `tm-architect` and the box reads Empty or Suggestion by
//!   `input_state_for_send`.
//! - AC6 `wake_enter_only_when_box_equals_message`,
//!   `wake_cleans_up_only_its_own_tail`, `wake_bspace_never_exceeds_typed_len`:
//!   Enter only on equality; BSpace only over our own trailing text, bounded.
//! - `wake_rechecks_owner_before_every_send`: the owner is read before typing,
//!   before Enter and before BSpace.
//! - AC7 `dispatch_wake_clears_only_on_sent`.
//!
//! Every tmux call goes to a `ScriptedTmux`; nothing touches a tmux server.

mod support;

use std::time::Duration;

use serde_json::Value;
use support::{
    ALERTS, DIM, NOW, RESET, TestResult, owned_tmux, pane, pointer, screen, sent, sup_pane, waker,
    wrapped, wrapped_with,
};
use trusty_architect::poller::{PollState, dispatch_wake};
use trusty_architect::{FakeClock, ScriptedTmux, WakeError};

const PERMISSION: &str = "⏺ Bash(rm -rf build/)\n\n────────────────────────────────────────────────────────────────────────────────\n Bash command · from the local-ops agent\n\n   rm -rf build/\n   Remove the build dir\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and don't ask again for rm commands in this project\n   3. No, and tell Claude what to do differently (esc)\n\n Esc to cancel\n";

/// A path with a two-byte character, so a byte count and a char count differ.
const WIDE_ALERTS: &str = "/w/ïnbox/alerts.jsonl";

#[test]
fn wake_refuses_table() -> TestResult {
    let stale = "[poll 11:15Z] 4 new alerts -> /Users/x/inbox/alerts.jsonl";
    let no_bottom_rule = format!("{}\n❯\u{a0}\n  footer\n", support::rule());
    let empty = screen("", false);
    let other_session = vec![pane("tm-x", "%1", "11")?];
    // (name, panes, tmux) — every row must send no keys.
    let rows: Vec<(&str, _, ScriptedTmux)> = vec![
        (
            "typed",
            sup_pane()?,
            owned_tmux().fallback("capture-pane", &screen("draft", false)),
        ),
        (
            "no-prompt",
            sup_pane()?,
            owned_tmux().fallback("capture-pane", &no_bottom_rule),
        ),
        (
            "dialog",
            sup_pane()?,
            owned_tmux().fallback("capture-pane", PERMISSION),
        ),
        (
            "no-box",
            sup_pane()?,
            owned_tmux().fallback("capture-pane", "❯\n\n  ✻ Working… (esc to interrupt)\n"),
        ),
        (
            "own-stale-pointer",
            sup_pane()?,
            owned_tmux().fallback("capture-pane", &wrapped(stale)),
        ),
        (
            "wrapped draft",
            sup_pane()?,
            owned_tmux().fallback("capture-pane", &wrapped_with("first half", "second half")),
        ),
        (
            "wrapped dim suggestion reads typed",
            sup_pane()?,
            owned_tmux().fallback(
                "capture-pane",
                &wrapped_with(&format!("{DIM}a long dim{RESET}"), "suggestion"),
            ),
        ),
        (
            "wrong session",
            sup_pane()?,
            ScriptedTmux::new()
                .fallback("capture-pane", &empty)
                .fallback("display-message", "tm-api\n")
                .fallback("send-keys", ""),
        ),
        (
            "not listed under tm-architect",
            other_session,
            owned_tmux().fallback("capture-pane", &empty),
        ),
        (
            "display-message fails",
            sup_pane()?,
            ScriptedTmux::new()
                .fallback("capture-pane", &empty)
                .fail("display-message", "no server running")
                .fallback("send-keys", ""),
        ),
        (
            "capture fails",
            sup_pane()?,
            owned_tmux().fail("capture-pane", "can't find pane: %1"),
        ),
    ];
    for (name, panes, tmux) in rows {
        let clock = FakeClock::new(NOW);
        let result = waker(&tmux, &clock, ALERTS).wake(&panes, 1, None);
        assert!(tmux.sends().is_empty(), "{name}: sent {:?}", tmux.sends());
        assert!(
            !matches!(result, Ok(ref r) if r.starts_with("sent")),
            "{name}: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn wake_enter_only_when_box_equals_message() -> TestResult {
    let msg = pointer(ALERTS, 3, None)?;
    let empty = screen("", false);
    let cases = [
        ("equal, wrapped", wrapped(&msg), true),
        (
            "message plus extra",
            screen(&format!("{msg} and more"), false),
            false,
        ),
        (
            "extra plus message",
            screen(&format!("draft {msg}"), false),
            false,
        ),
        ("unrelated", screen("something else", false), false),
    ];
    for (name, after, enter) in cases {
        let tmux = owned_tmux()
            .script("capture-pane", &empty)
            .script("capture-pane", &after)
            .fallback("capture-pane", &empty);
        let clock = FakeClock::new(NOW);
        let result = waker(&tmux, &clock, ALERTS).wake(&sup_pane()?, 3, None)?;
        let pressed_enter = sent(&tmux).iter().any(|k| k == &["Enter"]);
        assert_eq!(pressed_enter, enter, "{name}: {result} {:?}", sent(&tmux));
        assert_eq!(result.starts_with("sent"), enter, "{name}: {result}");
    }
    Ok(())
}

#[test]
fn wake_cleans_up_only_its_own_tail() -> TestResult {
    let msg = pointer(WIDE_ALERTS, 3, None)?;
    let empty = screen("", false);
    let tmux = owned_tmux()
        .script("capture-pane", &empty)
        .script("capture-pane", &screen(&format!("my draft {msg}"), false))
        .script("capture-pane", &screen("my draft", false));
    let clock = FakeClock::new(NOW);
    let result = waker(&tmux, &clock, WIDE_ALERTS).wake(&sup_pane()?, 3, None)?;
    assert_eq!(
        result,
        "failed: text not confirmed in the input box; cleanup left box non-empty"
    );
    let sends = sent(&tmux);
    assert_eq!(sends.len(), 2, "{sends:?}");
    assert!(sends[1].iter().all(|k| k == "BSpace"));
    assert_eq!(
        sends[1].len(),
        msg.chars().count(),
        "erases exactly what it typed"
    );
    assert!(sends[1].len() < msg.len(), "counts characters, not bytes");
    assert_eq!(
        clock.sleeps(),
        [Duration::from_millis(500), Duration::from_millis(300)]
    );
    Ok(())
}

#[test]
fn wake_bspace_never_exceeds_typed_len() -> TestResult {
    let msg = pointer(WIDE_ALERTS, 3, None)?;
    // Our text twice (a stale copy, then the one just typed): only the copy
    // this call typed may be erased.
    let tmux = owned_tmux()
        .script("capture-pane", &screen("", false))
        .script("capture-pane", &screen(&format!("{msg}{msg}"), false))
        .script("capture-pane", &screen(&msg, false));
    let clock = FakeClock::new(NOW);
    waker(&tmux, &clock, WIDE_ALERTS).wake(&sup_pane()?, 3, None)?;
    let erased: usize = sent(&tmux)
        .iter()
        .flatten()
        .filter(|k| *k == "BSpace")
        .count();
    assert_eq!(erased, msg.chars().count());
    Ok(())
}

#[test]
fn wake_rechecks_owner_before_every_send() -> TestResult {
    let msg = pointer(ALERTS, 3, None)?;
    let empty = screen("", false);
    // (after typing, the capture; the owner flips before the second send)
    for (name, after) in [
        ("before Enter", wrapped(&msg)),
        ("before BSpace", screen(&format!("draft {msg}"), false)),
    ] {
        let tmux = ScriptedTmux::new()
            .script("display-message", "tm-architect\n")
            .script("display-message", "tm-api\n")
            .fallback("send-keys", "")
            .script("capture-pane", &empty)
            .script("capture-pane", &after)
            .fallback("capture-pane", &empty);
        let clock = FakeClock::new(NOW);
        let err = waker(&tmux, &clock, ALERTS).wake(&sup_pane()?, 3, None);
        assert!(
            matches!(err, Err(WakeError::NotOwner { .. })),
            "{name}: {err:?}"
        );
        assert_eq!(sent(&tmux).len(), 1, "{name}: only the typed text went out");
    }
    // Every send in a full run follows its own owner check.
    let tmux = owned_tmux()
        .script("capture-pane", &empty)
        .script("capture-pane", &wrapped(&msg))
        .fallback("capture-pane", &empty);
    let clock = FakeClock::new(NOW);
    waker(&tmux, &clock, ALERTS).wake(&sup_pane()?, 3, None)?;
    let calls = tmux.calls();
    for (i, call) in calls.iter().enumerate() {
        if call[0] == "send-keys" {
            assert_eq!(
                calls[i - 1][0],
                "display-message",
                "send {i} unchecked: {calls:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn wake_dry_run_sends_nothing() -> TestResult {
    let tmux = owned_tmux().fallback("capture-pane", &screen("", false));
    let clock = FakeClock::new(NOW);
    let dry = trusty_architect::poller::Waker {
        dry_run: true,
        ..waker(&tmux, &clock, ALERTS)
    };
    let result = dry.wake(&sup_pane()?, 2, None)?;
    assert!(
        result.starts_with("dry-run: would send to %1: [poll 08:53Z] 2 new alerts -> "),
        "{result}"
    );
    assert_eq!(
        tmux.calls().len(),
        1,
        "one capture, nothing else: {:?}",
        tmux.calls()
    );
    assert!(clock.sleeps().is_empty());
    Ok(())
}

#[test]
fn wake_message_uses_need_you_form() -> TestResult {
    assert_eq!(
        pointer(ALERTS, 1, Some(0))?,
        format!("[poll 08:53Z] 1 alert needs you (0 logged) -> {ALERTS}")
    );
    assert_eq!(
        pointer(ALERTS, 2, Some(5))?,
        format!("[poll 08:53Z] 2 alerts need you (5 logged) -> {ALERTS}")
    );
    assert_eq!(
        pointer(ALERTS, 1, None)?,
        format!("[poll 08:53Z] 1 new alerts -> {ALERTS}")
    );
    Ok(())
}

#[test]
fn dispatch_wake_clears_only_on_sent() {
    let not_sent = "failed: text not confirmed in the input box; box changed, nothing erased";
    // (wake result, dry_run, clears, stamps wake_sent_at)
    let cases: [(Result<&str, ()>, bool, bool, bool); 7] = [
        (Ok("skipped: input typed"), false, false, false),
        (Ok(not_sent), false, false, false),
        (Ok("dry-run: would send to %1: x"), true, false, false),
        (Ok("none"), false, false, false),
        (Ok("sent to %1"), false, true, true),
        (
            Ok("sent to %1 (still in box after Enter)"),
            false,
            true,
            true,
        ),
        (Err(()), false, false, false),
    ];
    for (outcome, dry_run, clears, stamps) in cases {
        let clock = FakeClock::new(NOW);
        let mut state = PollState::default();
        state.other.insert("wake_logged".to_owned(), Value::from(1));
        let result = dispatch_wake(&mut state, 2, Some(1), dry_run, &clock, |_, _| {
            outcome
                .map(str::to_owned)
                .map_err(|()| WakeError::NotOwner {
                    pane: "%1".to_owned(),
                    session: "tm-api".to_owned(),
                })
        });
        let want = if clears { 0 } else { 2 };
        assert_eq!(
            state.other.get("wake_pending"),
            Some(&Value::from(want)),
            "{result}"
        );
        assert_eq!(state.other.contains_key("wake_logged"), !clears, "{result}");
        assert_eq!(state.other.contains_key("wake_sent_at"), stamps, "{result}");
        if outcome.is_err() {
            assert_eq!(result, "failed: refusing to send to %1 (session 'tm-api')");
        }
    }
    // A dry run that reports `sent` clears the count but stamps no send time.
    let clock = FakeClock::new(NOW);
    let mut state = PollState::default();
    dispatch_wake(&mut state, 1, None, true, &clock, |_, _| {
        Ok("sent to %1".to_owned())
    });
    assert_eq!(state.other.get("wake_pending"), Some(&Value::from(0)));
    assert!(!state.other.contains_key("wake_sent_at"));
}

/// #9171 AC7: a stored `wake_pending` that is not a non-negative integer is
/// still a wake owed. It reads as at least 1, so the wake runs and a refusal
/// leaves the count non-zero; only `sent` zeroes it.
#[test]
fn dispatch_wake_unreadable_pending_reads_as_owed() {
    let bad = [
        ("string \"3\"", Value::from("3")),
        ("float 2.5", Value::from(2.5)),
        ("negative -1", Value::from(-1)),
    ];
    for (name, stored) in bad {
        // Refused: the wake is attempted and the count stays non-zero.
        let clock = FakeClock::new(NOW);
        let mut state = PollState::default();
        state
            .other
            .insert("wake_pending".to_owned(), stored.clone());
        let mut called = false;
        let result = dispatch_wake(&mut state, 0, None, false, &clock, |n, _| {
            called = true;
            assert!(n >= 1, "{name}: wake asked to send {n}");
            Ok("skipped: input typed".to_owned())
        });
        assert!(called, "{name}: no wake attempted, result {result}");
        let left = state.other.get("wake_pending").and_then(Value::as_u64);
        assert!(left.is_some_and(|c| c >= 1), "{name}: left {left:?}");

        // Sent: the count clears.
        let mut state = PollState::default();
        state.other.insert("wake_pending".to_owned(), stored);
        dispatch_wake(&mut state, 0, None, false, &clock, |_, _| {
            Ok("sent to %1".to_owned())
        });
        assert_eq!(state.other.get("wake_pending"), Some(&Value::from(0)));
    }
}

/// #9171 AC7: a failure after the pointer is typed never reports `sent` and
/// never clears the pending count.
#[test]
fn dispatch_wake_failure_after_typing_stays_owed() -> TestResult {
    let msg = pointer(ALERTS, 3, None)?;
    let empty = screen("", false);
    let rows = [
        (
            "re-read after typing fails",
            owned_tmux()
                .script("capture-pane", &empty)
                .fail("capture-pane", "can't find pane: %1"),
        ),
        (
            "owner flips before Enter",
            ScriptedTmux::new()
                .script("display-message", "tm-architect\n")
                .script("display-message", "tm-api\n")
                .fallback("send-keys", "")
                .script("capture-pane", &empty)
                .script("capture-pane", &wrapped(&msg)),
        ),
        (
            "Enter itself fails",
            ScriptedTmux::new()
                .fallback("display-message", "tm-architect\n")
                .script("send-keys", "")
                .fail("send-keys", "server exited")
                .script("capture-pane", &empty)
                .script("capture-pane", &wrapped(&msg)),
        ),
    ];
    for (name, tmux) in rows {
        let clock = FakeClock::new(NOW);
        let mut state = PollState::default();
        state
            .other
            .insert("wake_pending".to_owned(), Value::from(2));
        let result = waker(&tmux, &clock, ALERTS).dispatch(&mut state, &sup_pane()?, 1, None);
        assert!(!result.starts_with("sent"), "{name}: {result}");
        assert!(!tmux.sends().is_empty(), "{name}: nothing was typed");
        assert_eq!(
            state.other.get("wake_pending"),
            Some(&Value::from(3)),
            "{name}: {result}"
        );
    }
    Ok(())
}
