//! Tests for [`super`] — the `transcript_saving` doctor probe (issue #4467).
//!
//! Why: this check exists because the defect was silent, so its own tests have
//! to prove it can FAIL — in both directions. `verdict` is exercised directly
//! for the failure branches (the production prefix is correct, so the wrapper
//! can only ever show the happy path), and the wrapper is exercised once against
//! the real spawn builder so the two can never drift apart.

use super::*;

/// The gate that matters: the REAL production spawn prefix must preserve
/// transcript saving. Fails if anyone drops the scrub from `env_bin_prefix`.
#[test]
fn production_spawn_preserves_transcript_saving() {
    let check = check_transcript_saving();
    assert_eq!(
        check.status,
        CheckStatus::Ok,
        "the production managed spawn must preserve transcript saving; got: {}",
        check.message
    );
    assert_eq!(check.name, CHECK_NAME);
}

#[test]
fn ok_when_the_marker_is_scrubbed() {
    let check = verdict(&["ANTHROPIC_API_KEY", TRANSCRIPT_SUPPRESSING_MARKER], &[]);
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

/// Under-scrub: the whole point of the check.
#[test]
fn fails_when_the_suppressing_marker_is_not_scrubbed() {
    let check = verdict(&["ANTHROPIC_API_KEY"], &[]);
    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "omitting the suppressing marker must FAIL: {}",
        check.message
    );
    assert!(
        check.message.contains(TRANSCRIPT_SUPPRESSING_MARKER),
        "the message must name the marker: {}",
        check.message
    );
    assert!(
        check.message.contains("4467"),
        "the message must cite the issue: {}",
        check.message
    );
}

/// Over-scrub: the opposite failure, which would silently restore #4451.
#[test]
fn fails_when_the_scrub_would_take_the_config_dir() {
    let check = verdict(
        &[
            "ANTHROPIC_API_KEY",
            TRANSCRIPT_SUPPRESSING_MARKER,
            "CLAUDE_CONFIG_DIR",
        ],
        &[],
    );
    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "unsetting CLAUDE_CONFIG_DIR must FAIL: {}",
        check.message
    );
    assert!(
        check.message.contains("CLAUDE_CONFIG_DIR"),
        "the message must name the variable: {}",
        check.message
    );
    assert!(
        check.message.contains("4451"),
        "the message must cite the roster regression it prevents: {}",
        check.message
    );
}

/// An over-scrub is reported even though the transcript marker IS present, so
/// the two branches cannot mask each other.
#[test]
fn config_dir_over_scrub_takes_precedence_in_the_message() {
    let check = verdict(&[TRANSCRIPT_SUPPRESSING_MARKER, "CLAUDE_CONFIG_DIR"], &[]);
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.message.contains("4455"),
        "the config-dir branch must be the one reported: {}",
        check.message
    );
}

/// Data loss is not a preference — both failures must be hard.
#[test]
fn failure_is_a_hard_fail_not_a_warn() {
    for unset in [
        vec!["ANTHROPIC_API_KEY"],
        vec![TRANSCRIPT_SUPPRESSING_MARKER, "CLAUDE_CONFIG_DIR"],
    ] {
        let check = verdict(&unset, &[]);
        assert_ne!(
            check.status,
            CheckStatus::Warn,
            "must never be a Warn for {unset:?}: {}",
            check.message
        );
        assert_eq!(check.status, CheckStatus::Fail);
    }
}

/// The live-marker set is context in the message, never the pass/fail
/// condition — a clean environment must still produce a meaningful Ok.
#[test]
fn ok_message_reports_markers_live_in_the_environment() {
    let clean = verdict(&[TRANSCRIPT_SUPPRESSING_MARKER], &[]);
    assert_eq!(clean.status, CheckStatus::Ok);
    assert!(
        clean.message.contains("no inherited markers"),
        "a clean env must say so: {}",
        clean.message
    );

    let leaking = verdict(
        &[TRANSCRIPT_SUPPRESSING_MARKER],
        &[TRANSCRIPT_SUPPRESSING_MARKER],
    );
    assert_eq!(
        leaking.status,
        CheckStatus::Ok,
        "a live marker is context, not a failure: {}",
        leaking.message
    );
    assert!(
        leaking.message.contains("running with the leak present"),
        "a leaking env must be reported as context: {}",
        leaking.message
    );
}
