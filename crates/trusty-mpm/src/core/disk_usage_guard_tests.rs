//! Unit coverage for the `disk.max_usage_pct` worktree gate (#7497).
//!
//! Why: every threshold branch has to be provable without the runner's real
//! disk — a suite whose verdict depends on how full the developer's volume is
//! proves nothing about the change. Each decision here is fed a SYNTHETIC
//! percentage.
//! What: threshold resolution (absent / in range / rejected), the at/over/under
//! comparison, the two failure postures (fail closed for provisioning, fail
//! open for the Bash guard), the refusal text, and the absence of any ambient
//! off switch.
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

/// A threshold with nothing rejected.
fn threshold(pct: u8) -> ResolvedThreshold {
    ResolvedThreshold {
        threshold_pct: pct,
        rejected: None,
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

/// Why (#7497 review, MEDIUM 1): with `max_usage_pct: 150` the earlier refusal
///      called 90 "the configured threshold", sending the operator to look for
///      a 90 that is nowhere in their file.
/// What: the rejection travels with the threshold into the message, which names
///      the discarded value and says the default applies.
/// Test: this test.
#[test]
fn a_rejected_value_is_named_in_the_refusal() {
    let resolved = resolve_max_usage_pct(Some(150));
    let err = refusal_if_over(Some(&measured(95.0)), &resolved).expect("95% >= 90% refuses");
    let text = err.to_string();
    assert!(text.contains("threshold of 90%"), "{text}");
    assert!(text.contains("150"), "the discarded value is named: {text}");
    assert!(
        text.contains("outside 1..=100"),
        "the message says WHY it was discarded: {text}"
    );
}

/// Why: the comparison is the gate. An off-by-one at the boundary is the
///      difference between refusing at 90% and refusing at 91%.
/// What: strictly over refuses.
/// Test: this test.
#[test]
fn over_threshold_refuses() {
    assert!(refusal_if_over(Some(&measured(91.0)), &threshold(90)).is_some());
}

/// Why (#7497 decision 3): the threshold is inclusive — "at or above".
/// What: exactly at the threshold refuses.
/// Test: this test.
#[test]
fn at_threshold_refuses() {
    assert!(
        refusal_if_over(Some(&measured(90.0)), &threshold(90)).is_some(),
        "usage exactly AT the threshold must refuse"
    );
}

/// Why: the under-threshold case must be a complete no-op — the whole point of
///      "existing behaviour unchanged below the threshold".
/// What: one percent below refuses nothing.
/// Test: this test.
#[test]
fn under_threshold_allows() {
    assert!(refusal_if_over(Some(&measured(89.9)), &threshold(90)).is_none());
}

/// Why (#7497 decision 4, Fail-Open Check): the Bash guard ALLOWS what it could
///      not measure. Replacing that branch with a deny turns this red.
/// What: an unmeasured mount yields no refusal at any threshold.
/// Test: this test.
#[test]
fn an_unmeasured_mount_yields_no_refusal() {
    assert!(
        refusal_if_over(None, &threshold(1)).is_none(),
        "the fail-OPEN decision must allow an unmeasurable mount, even at a \
         threshold of 1% — a deny here is the regression this test exists for"
    );
}

/// Why (#7497 decision 4, ADR-0037): the provisioning path is the opposite —
///      a request it cannot prove safe fails. Reachable since the #7497 review:
///      `mount_for_path` no longer substitutes `/` for an unenumerated mount.
/// What: the same unmeasured input the test above allows is an error here, and
///      the error names the path.
/// Test: this test.
#[test]
fn an_unmeasurable_mount_fails_closed() {
    let err = check_usage(None, &threshold(90), Path::new("/some/worktree"))
        .expect_err("provisioning must refuse what it cannot measure");
    let text = err.to_string();
    assert!(text.contains("/some/worktree"), "{text}");
    assert!(text.contains("could not be measured"), "{text}");
    assert!(text.contains("90%"), "{text}");
}

/// Why: the refusal is the whole operator-facing deliverable — mount, measured
///      percent, threshold, and the key that changes it.
/// What: asserts all four appear in the rendered message.
/// Test: this test.
#[test]
fn refusal_names_the_mount_the_threshold_and_the_key() {
    let err = refusal_if_over(Some(&measured(92.4)), &threshold(90)).expect("92.4% >= 90% refuses");
    let text = err.to_string();
    assert!(text.contains("/System/Volumes/Data"), "mount: {text}");
    assert!(text.contains("92.4"), "measured percent: {text}");
    assert!(text.contains("90%"), "threshold: {text}");
    assert!(text.contains(MAX_USAGE_PCT_KEY), "config key: {text}");
    assert!(text.contains("config.yaml"), "config file: {text}");
}

/// Why (#7497 review, LOW): a rounded `90.0%` printed beside an `Ok` status —
///      or beside a refusal that did not happen — reads as a contradiction.
/// What: truncation toward zero, so the printed figure never exceeds the
///      measured one.
/// Test: this test.
#[test]
fn a_percent_is_truncated_not_rounded() {
    assert_eq!(fmt_pct(&89.96), "89.9%");
    assert_eq!(fmt_pct(&90.0), "90.0%");
    assert_eq!(fmt_pct(&92.45), "92.4%");
}

/// Why (#7497 review, HIGH 3): the gate must have NO ambient off switch. An
///      earlier revision disabled the default threshold whenever
///      `TRUSTY_TEST_HARNESS=1` was inherited, so one exported variable turned
///      the shipped gate off in an installed `tm` with nothing in the log.
/// What: with no config at all, the threshold is the default — and this test
///      process is itself a cargo test harness, which is exactly the condition
///      that used to disable it.
/// Test: this test.
#[test]
fn a_test_harness_does_not_disable_the_default_threshold() {
    let home = tempfile::tempdir().expect("tempdir");
    assert!(
        trusty_common::running_under_test_harness(),
        "precondition: these tests run in the harness the old seam keyed on"
    );
    assert_eq!(
        active_threshold_at(home.path()).threshold_pct,
        DEFAULT_MAX_USAGE_PCT,
        "no config plus a test harness must still gate at the default"
    );
}

/// Why: the YAML the operator actually writes has to reach the gate.
/// What: a `disk: max_usage_pct: 55` config under a scratch home resolves to 55.
/// Test: this test.
#[test]
fn active_threshold_at_reads_the_operators_value() {
    let home = tempfile::tempdir().expect("tempdir");
    write_config(home.path(), "disk:\n  max_usage_pct: 55\n");
    let resolved = active_threshold_at(home.path());
    assert_eq!(resolved.threshold_pct, 55);
    assert!(resolved.rejected.is_none());
}

/// Why (#6927's lesson, applied here): a protective gate must not be disabled
///      by an unrelated typo elsewhere in the file.
/// What: an unparseable config leaves the default in force — never `None`, and
///      never a permissive threshold.
/// Test: this test.
#[test]
fn an_unreadable_config_leaves_the_default_in_force() {
    let home = tempfile::tempdir().expect("tempdir");
    write_config(home.path(), "disk:\n  max_usage_pct: [not, a, number\n");
    assert_eq!(
        active_threshold_at(home.path()).threshold_pct,
        DEFAULT_MAX_USAGE_PCT,
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

/// Why (#7603): [`DiskGate::Pinned`] is the seam that lets a test decide its
/// own verdict instead of the runner's real disk — if it silently fell back to
/// measuring the path it would defeat the whole injection.
/// What: a pinned value comes back unchanged for a path whose real mount usage
/// (if any) is certainly not the synthetic 99% pinned here.
/// Test: this test.
#[test]
fn a_pinned_gate_measures_nothing() {
    let pinned = measured(99.0);
    let gate = DiskGate::Pinned(Some(pinned.clone()));
    assert_eq!(
        gate.measurement_for(Path::new("/nonexistent/path")),
        Some(pinned)
    );

    let empty_gate = DiskGate::Pinned(None);
    assert_eq!(empty_gate.measurement_for(Path::new("/")), None);
}

/// Why (#7603): [`DiskGate::MeasureTarget`] is production behaviour — it must
/// still delegate to [`measure`] rather than silently going inert.
/// What: `MeasureTarget` reports the same mount as a direct `measure()` call on
/// the same path. `usage_pct` is read live twice, so it is compared with slack
/// rather than for exact equality — the point is delegation, not a frozen
/// percentage.
/// Test: this test.
#[test]
fn the_default_gate_measures_the_real_mount() {
    let path = Path::new(".");
    assert_eq!(DiskGate::default(), DiskGate::MeasureTarget);
    let via_gate = DiskGate::MeasureTarget.measurement_for(path);
    let direct = measure(path);
    match (via_gate, direct) {
        (Some(a), Some(b)) => {
            assert_eq!(a.mount_point, b.mount_point);
            assert!(
                (a.usage_pct - b.usage_pct).abs() < 1.0,
                "usage_pct drifted more than expected between two live reads: {a:?} vs {b:?}"
            );
        }
        (None, None) => {}
        (a, b) => panic!("MeasureTarget and a direct measure() disagree: {a:?} vs {b:?}"),
    }
}
