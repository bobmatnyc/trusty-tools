//! Tests for doctor's palace-lock stall threshold (#8751).
//!
//! Why: doctor read "workers progressing" while a palace lock was held past
//! the point queued writers time out. These pin both sides of the line and
//! the unreadable-telemetry arm.
//! What: drives [`interpret_health_body_with_stall`] with a healthy body whose
//! `stalled_lock.age_secs` is set, against a fixed threshold — no clock, no
//! sleep, no env.
//! Test: this IS the test module.

use std::time::Duration;

use super::super::checks::{interpret_health_body_with_stall, summarize};
use super::super::CheckStatus;
use super::derive_lock_stall_threshold;

const URL: &str = "/tmp/tm.sock";
const THRESHOLD: Duration = Duration::from_secs(60);

/// A healthy, detector-vouched body whose longest-held lock is `stalled`.
fn body(stalled: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "status": "ok",
        "daemon_state": "ready",
        "worker": {
            "in_flight": 1,
            "oldest_age_secs": 60,
            "wedged": false,
            "stall_tracking_ok": true,
            "stalled_lock": stalled,
        },
    })
}

fn verdict(stalled: serde_json::Value) -> super::CheckResult {
    let body = body(stalled);
    interpret_health_body_with_stall("daemon socket".into(), URL, 200, Some(&body), THRESHOLD)
}

/// Why (#8751): a write lock held past the stall line must name its palace and
/// age. Fails against the pre-fix chain, which passed this body.
/// What: `age_secs` one second over the threshold; asserts `Warn` naming the
/// lock kind, the palace, the age and the threshold.
/// Test: itself.
#[test]
fn a_lock_held_past_the_threshold_warns_with_palace_and_age() {
    let result =
        verdict(serde_json::json!({"palace": "trusty-tools", "lock": "write", "age_secs": 61}));
    assert_eq!(result.status, CheckStatus::Warn, "{result:?}");
    let detail = result.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("write lock of palace 'trusty-tools'")
            && detail.contains("held for 61s")
            && detail.contains("60s stall threshold"),
        "the stalled palace and its age must be named: {detail}"
    );
}

/// Why (#8751): below the line the existing occupancy output must not change.
/// What: `age_secs` exactly at the threshold; asserts the pre-#8751 pass line.
/// Test: itself.
#[test]
fn a_lock_held_up_to_the_threshold_keeps_the_pass_line() {
    let result =
        verdict(serde_json::json!({"palace": "trusty-tools", "lock": "write", "age_secs": 60}));
    assert_eq!(result.status, CheckStatus::Pass, "{result:?}");
    assert_eq!(
        result.detail.as_deref(),
        Some("/tmp/tm.sock → 200, workers progressing, 1 in flight, oldest 60s")
    );
}

/// Why (#8751): a held-lock report doctor cannot parse is telemetry it could
/// not read, and must never end the run green.
/// What: a `stalled_lock` with no `age_secs`; asserts `Unknown` and an
/// unhealthy run.
/// Test: itself.
#[test]
fn an_unreadable_stalled_lock_is_undetermined() {
    let result = verdict(serde_json::json!({"palace": "trusty-tools", "lock": "write"}));
    assert_eq!(result.status, CheckStatus::Unknown, "{result:?}");
    assert!(
        result
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("UNKNOWN"),
        "{result:?}"
    );
    assert!(!summarize(&[result]).healthy);
}

/// Why (#8751): the default must track the write-lock bound, and the env
/// override must replace it.
/// What: the pure derivation with and without an override.
/// Test: itself.
#[test]
fn stall_threshold_defaults_to_the_write_lock_bound() {
    let write_lock = Duration::from_secs(90);
    assert_eq!(derive_lock_stall_threshold(None, write_lock), write_lock);
    assert_eq!(
        derive_lock_stall_threshold(Some(15), write_lock),
        Duration::from_secs(15)
    );
}
