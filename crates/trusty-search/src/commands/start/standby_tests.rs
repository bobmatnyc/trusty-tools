//! Unit tests for the launchd standby decision (#10358).

use std::time::Duration;

use super::standby::{
    assume_supervised, holder_action, retry_interval, seam_warning, HolderAction,
    ASSUME_SUPERVISED_ENV, DEFAULT_RETRY,
};

/// Why: the decision is the whole safety argument, so all four inputs are pinned.
/// What: only supervised + healthy stands by; every other pair refuses.
/// Test: itself.
#[test]
fn standby_only_when_supervised_and_the_holder_is_healthy() {
    assert_eq!(holder_action(true, true), HolderAction::Standby);
    assert_eq!(holder_action(false, true), HolderAction::Refuse);
    assert_eq!(holder_action(true, false), HolderAction::Refuse);
    assert_eq!(holder_action(false, false), HolderAction::Refuse);
}

/// Why: a bad override must never become a busy loop.
/// What: a valid value is honoured; unset, junk, zero and sub-50 ms use the default.
/// Test: itself.
#[test]
fn retry_interval_parses_the_override_and_ignores_junk() {
    assert_eq!(retry_interval(Some("200")), Duration::from_millis(200));
    for bad in [None, Some("abc"), Some("0"), Some("49"), Some("")] {
        assert_eq!(retry_interval(bad), DEFAULT_RETRY, "input {bad:?}");
    }
}

/// Why: only an explicit `1` may claim supervision.
/// What: `1` is on; unset, `0`, `true` and empty are off.
/// Test: itself.
#[test]
fn the_test_seam_is_on_only_for_one() {
    assert!(assume_supervised(Some("1")));
    for off in [None, Some("0"), Some("true"), Some("")] {
        assert!(!assume_supervised(off), "input {off:?}");
    }
}

/// Why: a stray seam must be visible in the log.
/// What: any set value warns and names the variable; unset is silent.
/// Test: itself.
#[test]
fn the_test_seam_warns_whenever_it_is_set() {
    assert_eq!(seam_warning(None), None);
    for v in ["1", "0", ""] {
        let w = seam_warning(Some(v)).expect("set means warn");
        assert!(
            w.contains(ASSUME_SUPERVISED_ENV) && w.contains("test-only"),
            "{w}"
        );
    }
}
