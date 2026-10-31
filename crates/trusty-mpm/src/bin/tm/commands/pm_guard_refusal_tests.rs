//! Fail-open regression tests for `pm_guard_refusal` (#10600, #10641).

use super::*;

/// The reason of a deny object, or a panic naming what it was.
fn deny_reason(response: Option<serde_json::Value>) -> String {
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

#[tokio::test]
async fn a_panicking_guard_denies() {
    let reason = deny_reason(fail_closed(panicking_guard(), || false).await);
    assert!(reason.starts_with("tm pm-guard: "), "{reason}");
    assert!(
        reason.contains("panicked") && reason.contains("#10600"),
        "{reason}"
    );
    assert!(
        reason.ends_with(&guard_policy::GUARD_FAILURE.tag()),
        "{reason}"
    );
}

#[tokio::test]
async fn an_erroring_guard_denies() {
    let failing = async { Err(anyhow::anyhow!("config unreadable")) };
    let reason = deny_reason(fail_closed(failing, || false).await);
    assert!(reason.contains("config unreadable"), "{reason}");
    assert!(reason.contains("[policy GP-003 guard-failure;"), "{reason}");
}

#[tokio::test]
async fn a_completed_guard_keeps_its_own_answer() {
    // An allow prints nothing; a printed deny stands. Neither gets a second object.
    assert_eq!(fail_closed(async { Ok(()) }, || false).await, None);
    assert_eq!(fail_closed(async { Ok(()) }, || true).await, None);
}

#[tokio::test]
async fn a_failure_after_a_printed_decision_prints_nothing_more() {
    // stdout carries exactly one object: the decision already printed stands.
    assert_eq!(fail_closed(panicking_guard(), || true).await, None);
    let failing = async { Err(anyhow::anyhow!("late")) };
    assert_eq!(fail_closed(failing, || true).await, None);
}
