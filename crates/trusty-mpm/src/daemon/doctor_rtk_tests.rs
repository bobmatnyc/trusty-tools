//! Unit tests for the `tm doctor` `rtk` check's pure fold (#7311).
//!
//! Why: the check's verdict must be provable without an `rtk` on the test
//! machine's PATH — otherwise the suite passes or fails on which developer ran
//! it. Every case below injects its own resolver and version probe.
//! What: both branches of [`super::build_rtk_check`], the advisory-only
//! guarantee, and the standing constraint that no message ever suggests
//! `rtk init`.
//! Test: this is the test file.

use super::*;

/// A resolver that always finds `rtk` at `path`.
fn found(path: &'static str) -> impl Fn(&str) -> Option<PathBuf> {
    move |name| {
        assert_eq!(name, "rtk", "the check must resolve `rtk`, not `{name}`");
        Some(PathBuf::from(path))
    }
}

/// A resolver that never finds anything.
fn absent(_name: &str) -> Option<PathBuf> {
    None
}

/// Why: a resolved binary with a working `--version` is the healthy case and
/// must report `Ok` naming both the path and the version.
/// What: injects a resolver returning `/opt/homebrew/bin/rtk` and a version
/// probe returning `rtk 0.9.1`.
/// Test: this is the test.
#[test]
fn build_rtk_check_present_is_ok() {
    let check = build_rtk_check(&found("/opt/homebrew/bin/rtk"), &|_| {
        Some("rtk 0.9.1".to_owned())
    });
    assert_eq!(check.name, "rtk");
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(
        check.message.contains("/opt/homebrew/bin/rtk"),
        "message must name the resolved path: {}",
        check.message
    );
    assert!(
        check.message.contains("rtk 0.9.1"),
        "message must carry the version: {}",
        check.message
    );
}

/// Why: a version probe that times out or fails must not downgrade a binary
/// the resolver actually found — presence is what the check reports.
/// What: injects a resolver that finds the binary and a probe returning `None`.
/// Test: this is the test.
#[test]
fn build_rtk_check_present_without_version_is_ok() {
    let check = build_rtk_check(&found("/usr/local/bin/rtk"), &|_| None);
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(
        check.message.contains("/usr/local/bin/rtk"),
        "message must still name the path: {}",
        check.message
    );
}

/// Why: an absent binary is the whole reason the check exists, and the operator
/// needs the install command AND the `rtk init` prohibition in the same line.
/// What: injects the always-absent resolver; asserts `Warn` plus the exact
/// remediation literal.
/// Test: this is the test.
#[test]
fn build_rtk_check_absent_warns_with_remediation() {
    let check = build_rtk_check(&absent, &|_| {
        panic!("must not probe a binary that is absent")
    });
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(
        check.message.contains(RTK_REMEDIATION),
        "message must carry the remediation: {}",
        check.message
    );
    assert!(
        check.message.contains("brew install rtk"),
        "remediation must name the install command: {}",
        check.message
    );
}

/// Why: a missing rtk leaves `tm compress` working through its native fallback,
/// so this check must never turn `tm doctor` red.
/// What: asserts neither branch returns `Fail`.
/// Test: this is the test.
#[test]
fn rtk_check_is_advisory_only() {
    for check in [
        build_rtk_check(&found("/opt/homebrew/bin/rtk"), &|_| None),
        build_rtk_check(&absent, &|_| None),
    ] {
        assert_ne!(
            check.status,
            CheckStatus::Fail,
            "the rtk check is advisory: {}",
            check.message
        );
    }
}

/// Why: `rtk init` / `rtk init -g` install a PreToolUse Bash hook that competes
/// with tm's own; tm invokes rtk directly and must never point an operator at
/// them. A future message edit that turns the prohibition into an instruction
/// fails here.
/// What: asserts that a message mentioning `rtk init` at all mentions it only
/// inside the "do not run" prohibition.
/// Test: this is the test.
// #7311: rtk is an install dependency; never run rtk init.
#[test]
fn rtk_messages_only_mention_rtk_init_as_a_prohibition() {
    for check in [
        build_rtk_check(&found("/opt/homebrew/bin/rtk"), &|_| {
            Some("rtk 0.9.1".to_owned())
        }),
        build_rtk_check(&absent, &|_| None),
    ] {
        if !check.message.contains("rtk init") {
            continue;
        }
        assert_eq!(
            check.message.matches("rtk init").count(),
            1,
            "only the prohibition may name `rtk init`: {}",
            check.message
        );
        assert!(
            check.message.contains("do not run `rtk init`"),
            "`rtk init` may appear only as a prohibition: {}",
            check.message
        );
    }
}
