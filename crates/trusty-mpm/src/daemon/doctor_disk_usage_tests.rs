//! Unit coverage for the `disk_usage` doctor probe (#7497).
//!
//! Why: the three statuses are the whole deliverable, and each must be provable
//! with a synthetic measurement — never against the runner's real volume.
//! What: below / at / unmeasurable, the rejected-configured-value report, and
//! the message content an operator acts on.
//! Test: this file IS the test.

use super::*;
use crate::core::disk_usage_guard::resolve_max_usage_pct;

/// A synthetic mount measurement.
fn measured(usage_pct: f32) -> MeasuredMount {
    MeasuredMount {
        mount_point: "/System/Volumes/Data".to_string(),
        usage_pct,
    }
}

/// A threshold with nothing rejected.
fn threshold(pct: u8) -> ResolvedThreshold {
    ResolvedThreshold {
        threshold_pct: pct,
        rejected: None,
    }
}

/// Why: below the threshold nothing is refused, so the probe must not nag.
/// What: 80% against a 90% threshold is `Ok` and still reports both numbers.
/// Test: this test.
#[test]
fn disk_usage_is_ok_below_the_threshold() {
    let check = build_disk_usage_check(Some(&measured(80.0)), &threshold(90));
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(check.message.contains("80.0%"), "{}", check.message);
    assert!(check.message.contains("90%"), "{}", check.message);
}

/// Why (#7497 review, LOW): a rounded figure could print `90.0%` beside an `Ok`
///      status, which reads as a contradiction.
/// What: 89.96% prints as 89.9% and stays `Ok`.
/// Test: this test.
#[test]
fn disk_usage_truncates_the_percent_it_reports() {
    let check = build_disk_usage_check(Some(&measured(89.96)), &threshold(90));
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(
        check.message.contains("89.9%") && !check.message.contains("90.0%"),
        "an Ok status must never print the threshold figure: {}",
        check.message
    );
}

/// Why: at the threshold the gate is already refusing worktrees, and the
///      operator has to be told that before their next `tm session new`.
/// What: 90% against 90% is `Warn` and names the mount, the key and the remedy.
/// Test: this test.
#[test]
fn disk_usage_warns_at_the_threshold() {
    let check = build_disk_usage_check(Some(&measured(90.0)), &threshold(90));
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

/// Why (#7497 review, MEDIUM 1): with `max_usage_pct: 150` the probe used to
///      report "threshold is 90%" and call it `Ok`, which hides that the
///      operator's own value was thrown away.
/// What: a rejected value is a `Warn` naming the discarded number, even on a
///      volume with plenty of headroom.
/// Test: this test.
#[test]
fn disk_usage_warns_when_the_configured_value_was_rejected() {
    let check = build_disk_usage_check(Some(&measured(10.0)), &resolve_max_usage_pct(Some(150)));
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("150"), "{}", check.message);
    assert!(check.message.contains("90%"), "{}", check.message);
}

/// Why: a probe that learned nothing must not read healthy.
/// What: an unmeasurable mount is `Unknown`, not `Ok`.
/// Test: this test.
#[test]
fn disk_usage_is_unknown_when_the_mount_cannot_be_measured() {
    let check = build_disk_usage_check(None, &threshold(90));
    assert_eq!(check.status, CheckStatus::Unknown);
    assert!(check.message.contains("undetermined"), "{}", check.message);
    assert!(check.message.contains("90%"), "{}", check.message);
}
