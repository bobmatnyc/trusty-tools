//! Unit coverage for the `disk.max_usage_pct` worktree gate (#7497).
//!
//! Why: every threshold branch has to be provable without the runner's real
//! disk — a suite whose verdict depends on how full the developer's volume is
//! proves nothing about the change. Each decision here is fed a SYNTHETIC
//! percentage.
//! What: threshold resolution (absent / in range / rejected), the at/over/under
//! comparison, the two failure postures (fail closed for provisioning, fail
//! open for the Bash guard), the test-harness rule, and the refusal text.
//! Test: this file IS the test.

use std::path::Path;

use super::*;

/// A synthetic measurement — no filesystem, no sysinfo.
fn measured(usage_pct: f32) -> MeasuredMount {
    MeasuredMount {
        mount_point: "/System/Volumes/Data".to_string(),
        usage_pct,
    }
}

/// Write a `disk:` section into `<base>/.trusty-tools/trusty-mpm/config.yaml`.
fn write_config(base: &Path, body: &str) {
    let path = trusty_common::crate_config::crate_config_path_at(base, CRATE_NAME);
    std::fs::create_dir_all(path.parent().expect("config dir")).expect("create config dir");
    std::fs::write(&path, body).expect("write config");
}

/// Why: an absent key is the shipped behaviour — 90%, with nothing to configure.
/// What: `None` resolves to the documented default and rejects nothing.
/// Test: this test.
#[test]
fn absent_resolves_to_the_default() {
    let r = resolve_max_usage_pct(None);
    assert_eq!(r.threshold_pct, DEFAULT_MAX_USAGE_PCT);
    assert_eq!(r.threshold_pct, 90, "the owner-specified default is 90%");
    assert!(r.rejected.is_none());
}

/// Why: an in-range value is the whole point of the key.
/// What: 75 resolves to 75, unchanged and unrejected.
/// Test: this test.
#[test]
fn an_in_range_value_is_taken_as_written() {
    let r = resolve_max_usage_pct(Some(75));
    assert_eq!(r.threshold_pct, 75);
    assert!(r.rejected.is_none());
}

/// Why (#7497 decision 2): `0` would refuse every worktree on every machine.
///      It must not be applied, and it must not silently vanish either.
/// What: resolves to the default AND reports the discarded value.
/// Test: this test.
#[test]
fn zero_is_rejected_and_falls_back_to_the_default() {
    let r = resolve_max_usage_pct(Some(0));
    assert_eq!(r.threshold_pct, DEFAULT_MAX_USAGE_PCT);
    let reason = r.rejected.expect("a discarded value must be reported");
    assert!(reason.contains('0'), "the reason names the value: {reason}");
    assert!(
        reason.contains(MAX_USAGE_PCT_KEY),
        "the reason names the key: {reason}"
    );
}

/// Why (#7497 decision 2): `101` can never fire, so applying it would disable
///      the gate while looking configured.
/// What: resolves to the default AND reports the discarded value.
/// Test: this test.
#[test]
fn above_one_hundred_is_rejected_and_falls_back_to_the_default() {
    let r = resolve_max_usage_pct(Some(101));
    assert_eq!(r.threshold_pct, DEFAULT_MAX_USAGE_PCT);
    assert!(
        r.rejected.is_some_and(|s| s.contains("101")),
        "101 must be reported as discarded, never applied"
    );
}

/// Why: the comparison is the gate. An off-by-one at the boundary is the
///      difference between refusing at 90% and refusing at 91%.
/// What: strictly over refuses.
/// Test: this test.
#[test]
fn over_threshold_refuses() {
    assert!(refusal_if_over(Some(&measured(91.0)), 90).is_some());
}

/// Why (#7497 decision 3): the threshold is inclusive — "at or above".
/// What: exactly at the threshold refuses.
/// Test: this test.
#[test]
fn at_threshold_refuses() {
    assert!(
        refusal_if_over(Some(&measured(90.0)), 90).is_some(),
        "usage exactly AT the threshold must refuse"
    );
}

/// Why: the under-threshold case must be a complete no-op — the whole point of
///      "existing behaviour unchanged below the threshold".
/// What: one percent below refuses nothing.
/// Test: this test.
#[test]
fn under_threshold_allows() {
    assert!(refusal_if_over(Some(&measured(89.9)), 90).is_none());
}

/// Why (#7497 decision 4, Fail-Open Check): the Bash guard ALLOWS what it could
///      not measure. Replacing that branch with a deny turns this red.
/// What: an unmeasured mount yields no refusal at any threshold.
/// Test: this test.
#[test]
fn an_unmeasured_mount_yields_no_refusal() {
    assert!(
        refusal_if_over(None, 1).is_none(),
        "the fail-OPEN decision must allow an unmeasurable mount, even at a \
         threshold of 1% — a deny here is the regression this test exists for"
    );
}

/// Why (#7497 decision 4, ADR-0037): the provisioning path is the opposite —
///      a request it cannot prove safe fails.
/// What: the same unmeasured input the test above allows is an error here, and
///      the error names the path.
/// Test: this test.
#[test]
fn an_unmeasurable_mount_fails_closed() {
    let err = check_usage(None, 90, Path::new("/some/worktree"))
        .expect_err("provisioning must refuse what it cannot measure");
    let text = err.to_string();
    assert!(text.contains("/some/worktree"), "{text}");
    assert!(text.contains("could not be measured"), "{text}");
}

/// Why: the refusal is the whole operator-facing deliverable — mount, measured
///      percent, threshold, and the key that changes it.
/// What: asserts all four appear in the rendered message.
/// Test: this test.
#[test]
fn refusal_names_the_mount_the_threshold_and_the_key() {
    let err = refusal_if_over(Some(&measured(92.4)), 90).expect("92.4% >= 90% refuses");
    let text = err.to_string();
    assert!(text.contains("/System/Volumes/Data"), "mount: {text}");
    assert!(text.contains("92.4"), "measured percent: {text}");
    assert!(text.contains("90%"), "threshold: {text}");
    assert!(text.contains(MAX_USAGE_PCT_KEY), "config key: {text}");
    assert!(text.contains("config.yaml"), "config file: {text}");
}

/// Why: an operator who configured a threshold means it, test process or not —
///      this is what lets the gate's own end-to-end tests drive the real path.
/// What: an explicit value applies under a test harness.
/// Test: this test.
#[test]
fn an_explicit_threshold_applies_under_a_test_harness() {
    assert_eq!(active_threshold_from(Some(42), true), Some(42));
}

/// Why: a fixture worktree must not depend on the developer's free space.
/// What: absent + test harness disables the gate.
/// Test: this test.
#[test]
fn an_absent_threshold_disables_the_gate_under_a_test_harness() {
    assert_eq!(active_threshold_from(None, true), None);
}

/// Why: in production the default IS the feature — 90% with no config at all.
/// What: absent + not a test harness resolves to 90.
/// Test: this test.
#[test]
fn an_absent_threshold_is_the_default_in_production() {
    assert_eq!(
        active_threshold_from(None, false),
        Some(DEFAULT_MAX_USAGE_PCT)
    );
}

/// Why: a rejected value must not become a permissive threshold even on the
///      live resolution path.
/// What: `Some(200)` resolves to the default, never to `200` or to `None`.
/// Test: this test.
#[test]
fn a_rejected_value_still_yields_the_default_threshold() {
    assert_eq!(
        active_threshold_from(Some(200), false),
        Some(DEFAULT_MAX_USAGE_PCT)
    );
}

/// Why: the YAML the operator actually writes has to reach the gate.
/// What: a `disk: max_usage_pct: 55` config under a scratch home resolves to 55.
/// Test: this test.
#[test]
fn active_threshold_at_reads_the_operators_value() {
    let home = tempfile::tempdir().expect("tempdir");
    write_config(home.path(), "disk:\n  max_usage_pct: 55\n");
    assert_eq!(active_threshold_at(home.path(), true), Some(55));
}

/// Why (#6927's lesson, applied here): a protective gate must not be disabled
///      by an unrelated typo elsewhere in the file.
/// What: an unparseable config leaves the PRODUCTION default in force.
/// Test: this test.
#[test]
fn an_unreadable_config_leaves_the_default_in_force() {
    let home = tempfile::tempdir().expect("tempdir");
    write_config(home.path(), "disk:\n  max_usage_pct: [not, a, number\n");
    assert_eq!(
        active_threshold_at(home.path(), false),
        Some(DEFAULT_MAX_USAGE_PCT),
        "an unreadable config must not disable the gate"
    );
}

/// Why: "no `disk:` section changes nothing" is an acceptance criterion.
/// What: an empty config under a scratch home reads as absent.
/// Test: this test.
#[test]
fn an_absent_section_reads_as_no_configured_value() {
    let home = tempfile::tempdir().expect("tempdir");
    write_config(home.path(), "daemon:\n  allow_mcp_spawn: false\n");
    assert_eq!(read_configured(home.path()), None);
    let missing = tempfile::tempdir().expect("tempdir");
    assert_eq!(read_configured(missing.path()), None);
}
