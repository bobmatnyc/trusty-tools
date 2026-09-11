//! Unit coverage for the `disk_usage` doctor probe (#7497).
//!
//! Why: the three statuses are the whole deliverable, and each must be provable
//! with a synthetic measurement — never against the runner's real volume.
//! What: below / at / unmeasurable, plus the message content an operator acts on.
//! Test: this file IS the test.

use super::*;

/// A synthetic mount measurement.
fn measured(usage_pct: f32) -> MeasuredMount {
    MeasuredMount {
        mount_point: "/System/Volumes/Data".to_string(),
        usage_pct,
    }
}

/// Why: below the threshold nothing is refused, so the probe must not nag.
/// What: 80% against a 90% threshold is `Ok` and still reports both numbers.
/// Test: this test.
#[test]
fn disk_usage_is_ok_below_the_threshold() {
    let check = build_disk_usage_check(Some(&measured(80.0)), 90);
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(check.message.contains("80.0%"), "{}", check.message);
    assert!(check.message.contains("90%"), "{}", check.message);
}

/// Why: at the threshold the gate is already refusing worktrees, and the
///      operator has to be told that before their next `tm session new`.
/// What: 90% against 90% is `Warn` and names the mount, the key and the remedy.
/// Test: this test.
#[test]
fn disk_usage_warns_at_the_threshold() {
    let check = build_disk_usage_check(Some(&measured(90.0)), 90);
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(
        check.message.contains("/System/Volumes/Data"),
        "{}",
        check.message
    );
    assert!(
        check.message.contains(MAX_USAGE_PCT_KEY),
        "{}",
        check.message
    );
    assert!(check.message.contains("REFUSED"), "{}", check.message);
}

/// Why: a probe that learned nothing must not read healthy.
/// What: an unmeasurable mount is `Unknown`, not `Ok`.
/// Test: this test.
#[test]
fn disk_usage_is_unknown_when_the_mount_cannot_be_measured() {
    let check = build_disk_usage_check(None, 90);
    assert_eq!(check.status, CheckStatus::Unknown);
    assert!(check.message.contains("undetermined"), "{}", check.message);
    assert!(check.message.contains("90%"), "{}", check.message);
}
