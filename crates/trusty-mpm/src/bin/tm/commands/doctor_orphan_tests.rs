//! Tests for the `tm doctor` orphan-daemon check (issue #4230).
//!
//! Why: the pre-#4230 stack had no check that could fail in the observed state —
//! an unsupervised daemon answering `/health` 200 while launchd's job was down.
//! These tests pin the FAIL branch specifically, using the exact input pair
//! recorded in the incident (`plist_exists = true`, `supervised = false`,
//! `version = "1.0.2"`), so the check cannot regress into an always-green line.
//! What: exercises every quadrant of `verdict`'s (plist_exists, supervised)
//! table, the severity, and the remediation text.
//! Test: this file.

use super::*;

/// The #4230 signature, verbatim from the incident report: a registered
/// `com.trusty.mpm` unit plus a daemon reporting `supervised: false`.
#[test]
fn fails_when_a_launchd_unit_exists_and_the_daemon_is_unsupervised() {
    let check = verdict(true, false, "1.0.2");
    assert_eq!(check.status, CheckStatus::Fail);
    assert_eq!(check.name, CHECK_NAME);
}

/// A launchd-supervised daemon is the intended state — never flag it.
#[test]
fn ok_when_supervised() {
    assert_eq!(verdict(true, true, "1.3.0").status, CheckStatus::Ok);
}

/// No registered unit means a bare dev-run daemon, which is not hazardous —
/// this is the same carve-out `compute_supervised` makes.
#[test]
fn ok_when_no_launchd_unit_is_registered() {
    assert_eq!(verdict(false, false, "1.3.0").status, CheckStatus::Ok);
    assert_eq!(verdict(false, true, "1.3.0").status, CheckStatus::Ok);
}

/// The whole point of the check is that the operator can act on it without
/// reading the source: it must name how to find the PID, how to kill it, and
/// the launchctl verb that actually restarts the supervised job (`bootout` is a
/// no-op here, which is why the incident dragged on).
#[test]
fn failure_names_the_pid_lookup_the_kill_and_the_launchctl_restart() {
    let msg = verdict(true, false, "1.0.2").message;
    assert!(msg.contains("lsof"), "message was: {msg}");
    assert!(msg.contains("kill -TERM"), "message was: {msg}");
    assert!(msg.contains("launchctl kickstart"), "message was: {msg}");
    assert!(msg.contains("#4230"), "message was: {msg}");
}

/// Fail, not Warn: while the orphan serves, every deploy verification is
/// measuring a binary that is no longer installed. That is a broken stack, not
/// a configuration preference.
#[test]
fn failure_is_a_hard_fail_not_a_warn() {
    assert_ne!(verdict(true, false, "1.0.2").status, CheckStatus::Warn);
    assert_eq!(verdict(true, false, "1.0.2").status, CheckStatus::Fail);
}

/// The serving version is the actionable half of the report — it is what tells
/// the operator their install shipped nothing.
#[test]
fn failure_names_the_serving_version() {
    let msg = verdict(true, false, "1.0.2").message;
    assert!(msg.contains("1.0.2"), "message was: {msg}");
}

/// A daemon predating #2332 omits `version`; the message must say so rather
/// than printing a dangling "version ".
#[test]
fn unknown_version_is_labelled_not_blank() {
    let msg = verdict(true, false, "").message;
    assert!(msg.contains("version unknown"), "message was: {msg}");
}
