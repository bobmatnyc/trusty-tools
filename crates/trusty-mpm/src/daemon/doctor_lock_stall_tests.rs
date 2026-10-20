//! Unit tests for `daemon::doctor_lock_stall` (#8751).
//! Test: this module IS the test suite for `super`.

use std::time::Duration;

use serde_json::{Value, json};

use super::*;

const LIMIT: Duration = Duration::from_secs(60);

fn body(stalled: Value) -> Value {
    json!({"worker": {"wedged": false, "stall_tracking_ok": true, "stalled_lock": stalled}})
}

fn check(body: &Value) -> Option<DoctorCheck> {
    stalled_lock_check("memory", "trusty-memory", "/s.sock", body, LIMIT)
}

/// Why: the defect — a lock past the line left the row green.
/// Test: itself.
#[test]
fn a_lock_held_past_the_threshold_warns_naming_lock_palace_age_and_remedy() {
    let got = check(&body(
        json!({"palace": "p1", "lock": "commit", "age_secs": 61}),
    ))
    .expect("a lock past the line earns a verdict");
    assert_eq!(got.status, CheckStatus::Warn);
    for needle in [
        "commit",
        "p1",
        "61s",
        "60s",
        "TRUSTY_DOCTOR_LOCK_STALL_SECS",
        "thread",
    ] {
        assert!(
            got.message.contains(needle),
            "`{needle}` in {}",
            got.message
        );
    }
}

/// Why: the daemon reports a lock under the line too; that is healthy.
/// Test: itself.
#[test]
fn a_lock_held_up_to_the_threshold_keeps_the_pass_line() {
    assert!(
        check(&body(
            json!({"palace": "p1", "lock": "write", "age_secs": 60})
        ))
        .is_none()
    );
}

/// Why: no lock held is the healthy shape; the field is omitted then.
/// Test: itself.
#[test]
fn an_absent_stalled_lock_keeps_the_pass_line() {
    assert!(check(&json!({"worker": {"wedged": false, "stall_tracking_ok": true}})).is_none());
    assert!(check(&json!({})).is_none());
}

/// Why (fail-closed): a block this client cannot read is not evidence of health.
/// Each shape is one error arm: wrong type, each field missing, a null.
/// Test: itself.
#[test]
fn an_unreadable_stalled_lock_is_unknown() {
    for stalled in [
        json!(null),
        json!("held"),
        json!({"lock": "write", "age_secs": 999}),
        json!({"palace": "p1", "age_secs": 999}),
        json!({"palace": "p1", "lock": "write"}),
        json!({"palace": "p1", "lock": "write", "age_secs": "999"}),
    ] {
        let got = check(&body(stalled.clone())).expect("unreadable earns a verdict");
        assert_eq!(got.status, CheckStatus::Unknown, "{stalled}");
        assert!(got.message.contains("UNKNOWN"), "{}", got.message);
    }
}

/// Why: only a restart clears a call blocked inside the HNSW graph.
/// Test: itself.
#[test]
fn an_hnsw_lock_names_the_restart_command() {
    let got = check(&body(
        json!({"palace": "p1", "lock": "hnsw", "age_secs": 600}),
    ))
    .expect("verdict");
    assert!(got.message.contains("launchctl bootout"), "{}", got.message);
    assert!(
        got.message.contains("never `launchctl kickstart -k`"),
        "{}",
        got.message
    );
}

/// Why: one setting, resolved the way trusty-memory resolves it.
/// Test: itself.
#[test]
fn threshold_resolves_override_then_write_lock_then_default() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        derive_threshold(move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_string())
        })
    };
    assert_eq!(env(&[]), Duration::from_secs(60));
    assert_eq!(env(&[(WRITE_LOCK_ENV, "90")]), Duration::from_secs(90));
    assert_eq!(
        env(&[(WRITE_LOCK_ENV, "90"), (LOCK_STALL_ENV, " 300 ")]),
        Duration::from_secs(300)
    );
    assert_eq!(env(&[(LOCK_STALL_ENV, "soon")]), Duration::from_secs(60));
}
