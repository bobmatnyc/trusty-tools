//! Tests for the `tm doctor` orphan-daemon check (issue #4230).
//!
//! Why: the pre-#4230 stack had no check that could fail in the observed state —
//! an unsupervised daemon answering `/health` 200 while launchd's job was down.
//! These tests pin the FAIL branches specifically, and pin that the AUTHORITATIVE
//! tier overrides the daemon's self-report in both directions, because the review
//! established that `supervised` reads `true` for a real orphan whose spawning
//! parent exited before the daemon's startup probe.
//! What: exercises all three decision tiers, the `--force` downgrade, the
//! severities, and the remediation text.
//! Test: this file.

use super::*;

/// The canonical label used across these tests.
const LABEL: &str = "com.trusty.mpm";

/// AUTHORITATIVE tier: launchd owns a DIFFERENT process than the one serving. A
/// duplicate daemon splitting traffic — detectable without trusting either side's
/// self-report.
#[test]
fn authoritative_fail_when_launchd_runs_a_different_pid() {
    let check = verdict(
        Some(LABEL),
        Some(111),
        Some(222),
        Some(true),
        false,
        "1.3.0",
    );
    assert_eq!(check.status, CheckStatus::Fail);
    assert_eq!(check.name, CHECK_NAME);
    assert!(
        check.message.contains("111"),
        "message was: {}",
        check.message
    );
    assert!(
        check.message.contains("222"),
        "message was: {}",
        check.message
    );
}

/// The #4230 incident verbatim: the unit is registered, launchd's job is down
/// (no PID), and something is nonetheless answering on the port.
#[test]
fn authoritative_fail_when_launchd_runs_nothing_but_a_daemon_answers() {
    let check = verdict(Some(LABEL), None, Some(98606), Some(false), false, "1.0.2");
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.message.contains("98606"),
        "message was: {}",
        check.message
    );
}

/// The intended state: launchd's PID and the responding PID are the same process.
#[test]
fn authoritative_ok_when_launchd_owns_the_responding_pid() {
    let check = verdict(
        Some(LABEL),
        Some(73599),
        Some(73599),
        Some(true),
        false,
        "1.3.0",
    );
    assert_eq!(check.status, CheckStatus::Ok);
}

/// The core of the review's HIGH-1: the authoritative tier must decide REGARDLESS
/// of the self-report, in both directions. `supervised: true` from a genuine
/// orphan (the `getppid() == 1` prong) must not rescue it, and `supervised: false`
/// from a launchd-owned daemon must not condemn it.
#[test]
fn authoritative_verdict_ignores_a_lying_self_report() {
    // Orphan that claims to be supervised — still Fail.
    let lying_orphan = verdict(Some(LABEL), None, Some(222), Some(true), false, "1.3.0");
    assert_eq!(lying_orphan.status, CheckStatus::Fail);
    // launchd genuinely owns it, but the heuristic said false — still Ok.
    let pessimistic = verdict(
        Some(LABEL),
        Some(333),
        Some(333),
        Some(false),
        false,
        "1.3.0",
    );
    assert_eq!(pessimistic.status, CheckStatus::Ok);
}

/// No registered daemon unit means a bare dev-run daemon, which is not hazardous —
/// the same carve-out `compute_supervised` makes. Also the `tctl install`
/// supervisor-only host, where `daemon_launchd_label` resolves `None`.
#[test]
fn ok_when_no_launchd_unit_is_registered() {
    for supervised in [Some(false), Some(true), None] {
        for pid in [None, Some(222)] {
            assert_eq!(
                verdict(None, None, pid, supervised, false, "1.3.0").status,
                CheckStatus::Ok,
                "supervised={supervised:?} pid={pid:?}"
            );
        }
    }
}

/// FALLBACK tier: a daemon too old to report `pid` but reporting
/// `supervised: false` is the payload the #4230 orphan actually produced. The
/// heuristic and the registration agree it is hazardous, so this stays a Fail.
#[test]
fn fallback_fail_when_old_daemon_reports_unsupervised() {
    let check = verdict(Some(LABEL), None, None, Some(false), false, "1.0.2");
    assert_eq!(check.status, CheckStatus::Fail);
}

/// FALLBACK tier, the review's central point: `supervised: true` with no pid must
/// be `Unknown`, never `Ok`. The flag's fallback prong is `getppid() == 1`, which
/// this same change deletes from the operator runbook as non-evidence — the check
/// cannot treat it as conclusive either.
#[test]
fn fallback_unknown_when_old_daemon_claims_supervised() {
    let check = verdict(Some(LABEL), None, None, Some(true), false, "1.0.2");
    assert_eq!(check.status, CheckStatus::Unknown);
    assert_ne!(check.status, CheckStatus::Ok);
    assert!(
        check.message.contains("getppid"),
        "the message must explain why the self-report is not conclusive: {}",
        check.message
    );
}

/// An absent `supervised` field must not be defaulted into a pass — that was the
/// smaller half of the fail-open the review identified.
#[test]
fn fallback_unknown_when_supervised_is_absent() {
    let check = verdict(Some(LABEL), None, None, None, false, "");
    assert_eq!(check.status, CheckStatus::Unknown);
}

/// `tm daemon --force` is the opt-in BOTH #4230 refusal messages recommend. A hard
/// Fail on it would make the tool condemn its own escape hatch and train operators
/// to ignore the check.
#[test]
fn warns_not_fails_when_the_operator_forced_it() {
    for (launchd_pid, serving_pid, supervised) in [
        (None, Some(222_u32), Some(false)),
        (Some(111_u32), Some(222_u32), Some(true)),
        (None, None, Some(false)),
    ] {
        let check = verdict(
            Some(LABEL),
            launchd_pid,
            serving_pid,
            supervised,
            true,
            "1.3.0",
        );
        assert_eq!(
            check.status,
            CheckStatus::Warn,
            "forced run must Warn, not Fail: {launchd_pid:?}/{serving_pid:?}"
        );
        assert!(
            check.message.contains("--force"),
            "message was: {}",
            check.message
        );
    }
}

/// The whole point of the check is that the operator can act on it without
/// reading the source: it must name how to find the PID, how to kill it, and the
/// launchctl verb that actually restarts the supervised job (`bootout` is a no-op
/// here, which is why the incident dragged on).
#[test]
fn failure_names_the_pid_lookup_the_kill_and_the_launchctl_restart() {
    let msg = verdict(Some(LABEL), None, Some(98606), Some(false), false, "1.0.2").message;
    assert!(msg.contains("lsof"), "message was: {msg}");
    assert!(msg.contains("kill -TERM"), "message was: {msg}");
    assert!(msg.contains("launchctl kickstart"), "message was: {msg}");
    assert!(msg.contains("#4230"), "message was: {msg}");
}

/// The failure must name `--force` so an operator running an intentional
/// unsupervised daemon can silence it correctly instead of ignoring the check.
#[test]
fn failure_names_the_force_opt_in() {
    let msg = verdict(Some(LABEL), None, Some(98606), Some(false), false, "1.0.2").message;
    assert!(msg.contains("tm daemon --force"), "message was: {msg}");
}

/// Every remediation must name the label actually registered on this host, not a
/// hardcoded `com.trusty.mpm` that may not exist (review HIGH-2).
#[test]
fn remediation_names_the_resolved_label() {
    let drifted = "com.trusty.mpm.dogfood";
    for check in [
        verdict(Some(drifted), None, Some(222), Some(false), false, "1.0.2"),
        verdict(Some(drifted), None, None, Some(true), false, "1.0.2"),
    ] {
        assert!(
            check.message.contains(drifted),
            "message was: {}",
            check.message
        );
    }
}

/// Fail, not Warn: while an unrequested orphan serves, every deploy verification
/// is measuring a binary that is no longer installed. That is a broken stack, not
/// a configuration preference.
#[test]
fn failure_is_a_hard_fail_not_a_warn() {
    let check = verdict(Some(LABEL), None, Some(98606), Some(false), false, "1.0.2");
    assert_eq!(check.status, CheckStatus::Fail);
    assert_ne!(check.status, CheckStatus::Warn);
}

/// The serving version is the actionable half of the report — it is what tells the
/// operator their install shipped nothing.
#[test]
fn failure_names_the_serving_version() {
    let msg = verdict(Some(LABEL), None, Some(98606), Some(false), false, "1.0.2").message;
    assert!(msg.contains("1.0.2"), "message was: {msg}");
}

/// A daemon predating #2332 omits `version`; the message must say so rather than
/// printing a dangling "version ".
#[test]
fn unknown_version_is_labelled_not_blank() {
    let msg = verdict(Some(LABEL), None, Some(98606), Some(false), false, "").message;
    assert!(msg.contains("version unknown"), "message was: {msg}");
}

/// A failed `/health` fetch must be `Unknown`, not a pass and not a Fail — this is
/// what keeps the documented `bootout → install → bootstrap` window quiet.
#[test]
fn unknown_when_no_health_snapshot_is_available() {
    let check = orphan_daemon_check(None);
    assert_eq!(check.status, CheckStatus::Unknown);
    assert_eq!(check.name, CHECK_NAME);
}
