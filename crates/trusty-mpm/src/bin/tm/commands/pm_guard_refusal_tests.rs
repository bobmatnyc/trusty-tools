//! Fail-open regression tests for `pm_guard_refusal` (#10600, #10641).

use std::sync::{Arc, mpsc};

use serde_json::json;

use super::*;
use crate::commands::pm_guard_recovery::RECOVERY_NOTE;

/// The reason of a deny object, or a panic naming what it was.
fn deny_reason(response: Option<Value>) -> String {
    let response = response.expect("a failed guard must produce a deny, not an allow");
    assert_eq!(response["hookSpecificOutput"]["permissionDecision"], "deny");
    response["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// A guard body that panics, typed as the guard's own future output.
async fn panicking_guard() -> anyhow::Result<()> {
    panic!("injected guard panic (#10600 test)")
}

fn write_call() -> Value {
    json!({ "session_id": "s-1", "cwd": "/repo", "tool_name": "Write",
            "tool_input": { "file_path": "/repo/a.md", "content": "x" } })
}

fn doctor_call() -> Value {
    json!({ "session_id": "s-1", "cwd": "/repo", "tool_name": "Bash",
            "tool_input": { "command": "tm doctor" } })
}

#[tokio::test]
async fn a_panicking_guard_denies() {
    let reason = deny_reason(fail_closed(panicking_guard(), || false, || Some(write_call())).await);
    assert!(reason.starts_with("tm pm-guard: "), "{reason}");
    assert!(
        reason.contains("panicked") && reason.contains("#10600"),
        "{reason}"
    );
    assert!(reason.contains(RECOVERY_NOTE), "{reason}");
    assert!(
        reason.ends_with(&guard_policy::GUARD_FAILURE.tag()),
        "{reason}"
    );
    // A payload the guard never read gets no allowlist.
    assert!(
        fail_closed(panicking_guard(), || false, || None)
            .await
            .is_some()
    );
}

#[tokio::test]
async fn an_erroring_guard_denies() {
    let failing = async { Err(anyhow::anyhow!("config unreadable")) };
    let reason = deny_reason(fail_closed(failing, || false, || Some(write_call())).await);
    assert!(reason.contains("config unreadable"), "{reason}");
    assert!(reason.contains("[policy GP-003 guard-failure;"), "{reason}");
}

#[tokio::test]
async fn a_failing_guard_allows_a_recovery_command() {
    // #10641: the failure refuses writes, never the repair.
    assert_eq!(
        fail_closed(panicking_guard(), || false, || Some(doctor_call())).await,
        None
    );
    let failing = async { Err(anyhow::anyhow!("x")) };
    assert_eq!(
        fail_closed(failing, || false, || Some(doctor_call())).await,
        None
    );
}

#[tokio::test]
async fn a_completed_guard_keeps_its_own_answer() {
    // An allow prints nothing; a printed deny stands. Neither gets a second object.
    assert_eq!(fail_closed(async { Ok(()) }, || false, || None).await, None);
    assert_eq!(fail_closed(async { Ok(()) }, || true, || None).await, None);
}

#[tokio::test]
async fn emit_marks_the_decision_so_a_later_panic_prints_nothing() {
    // The critic's wiring check: `emit` goes through the process-wide
    // decision, and the wrapper reads that decision before it denies.
    emit("{\"test\":\"emit_marks_the_decision\"}");
    assert!(DECISION.is_decided(), "emit records the printed decision");
    let late = fail_closed(
        panicking_guard(),
        || DECISION.is_decided(),
        || Some(write_call()),
    );
    assert_eq!(late.await, None, "stdout carries exactly one object");
    // The same holds for a local decision printed into a buffer.
    let local = Decision::new();
    assert!(local.print_once(&mut Vec::new(), &"first"));
    assert!(!local.print_once(&mut Vec::new(), &"second"));
    assert!(!local.claim());
}

/// A cloneable in-memory stdout for the watchdog.
#[derive(Clone, Default)]
struct SharedOut(Arc<Mutex<Vec<u8>>>);

impl Write for SharedOut {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SharedOut {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

#[test]
fn a_slow_judgement_is_decided_by_the_watchdog() {
    // #10641 timeout arm: the judgement is still running at the deadline. The
    // watchdog refuses a write (GP-049, with the recovery note) and lets an
    // allowlisted command through; the late judgement then prints nothing.
    for (payload, want_deny) in [(write_call(), true), (doctor_call(), false)] {
        let decision: &'static Decision = Box::leak(Box::new(Decision::new()));
        let out = SharedOut::default();
        let (tx, rx) = mpsc::channel();
        let wait = std::time::Duration::from_millis(50);
        spawn_watchdog(
            decision,
            wait,
            move || Some(payload),
            out.clone(),
            move || {
                let _ = tx.send(());
            },
        );
        let slow = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            decision.print_once(&mut std::io::sink(), &"the late judgement")
        });
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("the watchdog fired");
        let text = out.text();
        if want_deny {
            assert!(text.contains("\"permissionDecision\":\"deny\""), "{text}");
            assert!(text.contains(&guard_policy::GUARD_TIMEOUT.tag()), "{text}");
            assert!(text.contains("before its deadline"), "{text}");
        } else {
            assert!(
                text.is_empty(),
                "an allowlisted call prints nothing: {text}"
            );
        }
        assert!(decision.is_decided());
        assert!(
            !slow.join().expect("join"),
            "the late judgement prints nothing"
        );
    }
}

#[test]
fn the_deadline_leaves_a_margin_inside_the_hook_timeout() {
    // The PM hook entry registers `timeout: 10` (`session_launch::settings`).
    let hook_timeout = crate::commands::hook_stdin::REGISTERED_HOOK_TIMEOUT;
    let budgets = crate::commands::hook_stdin::PM_GUARD_STDIN_TIMEOUT
        + crate::commands::pm_guard_deny_log::AUDIT_POST_TIMEOUT;
    assert!(
        GUARD_DEADLINE >= budgets,
        "a slow read plus its audit still decides itself"
    );
    assert!(hook_timeout - GUARD_DEADLINE >= std::time::Duration::from_secs(2));
}

#[test]
fn hook_mode_runs_the_guard_through_this_wrapper() {
    // The critic's wiring pin: `tm hook --pm-guard` reaches `run`, not the bare guard.
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/tm/commands/hook_mode.rs");
    let text = std::fs::read_to_string(&path).expect("read hook_mode.rs");
    assert!(
        text.contains("pm_guard_refusal::run(url, started)"),
        "{text}"
    );
    assert!(!text.contains("pm_guard::pm_guard("), "{text}");
}

#[test]
fn the_failure_note_names_every_recovery_command() {
    for part in [
        "tm doctor",
        "tm install",
        "cargo install trusty-mpm",
        "[pm_guard] enabled = false",
    ] {
        assert!(RECOVERY_NOTE.contains(part), "{part}");
    }
}
