//! Tests for [`super`] — the `transcript_saving` doctor probe (issue #4467).
//!
//! Why: this check exists because the defect was silent, so its own tests have
//! to prove it can FAIL — in both directions, and for every launch line it
//! claims to cover. `verdict` is exercised directly for the failure branches (the
//! production builders are correct, so the wrapper can only ever show the happy
//! path), and the wrapper is exercised against every real builder so the two can
//! never drift apart.

use super::*;

/// A representative path label for direct `verdict` calls.
const PATH: &str = "probe::builder";

/// The gate that matters: EVERY real launch line must preserve transcript
/// saving. Fails if anyone drops the scrub from any builder.
#[test]
fn every_production_launch_line_preserves_transcript_saving() {
    let check = check_transcript_saving();
    assert_eq!(
        check.status,
        CheckStatus::Ok,
        "every production launch line must preserve transcript saving; got: {}",
        check.message
    );
    assert_eq!(check.name, CHECK_NAME);
}

/// Per-builder assertion, so a failure names the offender instead of only
/// reporting that "something" leaks. Marker names are hard-coded: deriving them
/// from the constant would let an emptied list pass.
#[test]
fn launch_lines_covers_every_builder() {
    let lines = super::launch_lines();
    let labels: Vec<&str> = lines.iter().map(|(label, _)| *label).collect();

    for expected in [
        "runtime::claude_code::env_bin_prefix",
        "core::model_inject::build_claude_command",
        "daemon::spawn_command::relaunch_command",
        "core::standalone::run::build_launch_command",
        "control::backend::stream_json::build_claude_command",
    ] {
        assert!(
            labels.iter().any(|l| l.contains(expected)),
            "the check must cover {expected}; covered: {labels:?}"
        );
    }

    // Every covered builder must actually unset the suppressing marker, and none
    // may unset a deliberate variable.
    for (label, unset) in &lines {
        assert!(
            unset.iter().any(|n| n == "CLAUDE_CODE_CHILD_SESSION"),
            "{label} must unset CLAUDE_CODE_CHILD_SESSION; unsets: {unset:?}"
        );
        assert!(
            !unset.iter().any(|n| n == "CLAUDE_CONFIG_DIR"),
            "{label} must NOT unset CLAUDE_CONFIG_DIR (#4455); unsets: {unset:?}"
        );
        assert!(
            !unset.iter().any(|n| n == "CLAUDE_CODE_OAUTH_TOKEN"),
            "{label} must NOT unset CLAUDE_CODE_OAUTH_TOKEN (#2246); unsets: {unset:?}"
        );
    }
}

/// The probe must supply an OAuth token, or an over-scrub of it stays invisible.
#[test]
fn probe_supplies_an_oauth_token_so_over_scrub_is_visible() {
    let config_dir = std::path::PathBuf::from(PROBE_CONFIG_DIR);
    let prefix = crate::runtime::env_bin_prefix("claude", Some(&config_dir), Some(PROBE_TOKEN));
    assert!(
        prefix.contains("CLAUDE_CODE_OAUTH_TOKEN="),
        "the probe prefix must carry the token assignment: {prefix}"
    );
    assert!(
        prefix.contains("CLAUDE_CONFIG_DIR="),
        "the probe prefix must carry the config-dir assignment: {prefix}"
    );
}

#[test]
fn ok_when_the_marker_is_scrubbed() {
    let check = verdict(
        PATH,
        &["ANTHROPIC_API_KEY", TRANSCRIPT_SUPPRESSING_MARKER],
        &[],
    );
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

/// Under-scrub: the whole point of the check.
#[test]
fn fails_when_the_suppressing_marker_is_not_scrubbed() {
    let check = verdict(PATH, &["ANTHROPIC_API_KEY"], &[]);
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

/// A failure has to say WHICH builder leaks.
#[test]
fn failure_names_the_leaking_path() {
    let check = verdict("core::some::builder", &["ANTHROPIC_API_KEY"], &[]);
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.message.contains("core::some::builder"),
        "the message must name the offending builder: {}",
        check.message
    );
}

/// Over-scrub: the opposite failure, which would silently restore #4451.
#[test]
fn fails_when_the_scrub_would_take_the_config_dir() {
    let check = verdict(
        PATH,
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

/// The OTHER deliberate variable: invisible to the earlier literal-only form.
#[test]
fn over_scrub_of_the_oauth_token_is_detected() {
    let check = verdict(
        PATH,
        &[TRANSCRIPT_SUPPRESSING_MARKER, "CLAUDE_CODE_OAUTH_TOKEN"],
        &[],
    );
    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "unsetting the OAuth token must FAIL: {}",
        check.message
    );
    assert!(
        check.message.contains("CLAUDE_CODE_OAUTH_TOKEN"),
        "the message must name the token: {}",
        check.message
    );
    assert!(
        check.message.contains("2246"),
        "the message must cite the login-loop regression: {}",
        check.message
    );
}

/// An over-scrub is reported even though the transcript marker IS present, so
/// the two branches cannot mask each other.
#[test]
fn config_dir_over_scrub_takes_precedence_in_the_message() {
    let check = verdict(
        PATH,
        &[TRANSCRIPT_SUPPRESSING_MARKER, "CLAUDE_CONFIG_DIR"],
        &[],
    );
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.message.contains("4455"),
        "the deliberate-env branch must be the one reported: {}",
        check.message
    );
}

/// Data loss is not a preference — both failures must be hard.
#[test]
fn failure_is_a_hard_fail_not_a_warn() {
    for unset in [
        vec!["ANTHROPIC_API_KEY"],
        vec![TRANSCRIPT_SUPPRESSING_MARKER, "CLAUDE_CONFIG_DIR"],
        vec![TRANSCRIPT_SUPPRESSING_MARKER, "CLAUDE_CODE_OAUTH_TOKEN"],
    ] {
        let check = verdict(PATH, &unset, &[]);
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
    let clean = verdict(PATH, &[TRANSCRIPT_SUPPRESSING_MARKER], &[]);
    assert_eq!(clean.status, CheckStatus::Ok);
    assert!(
        clean.message.contains("no inherited markers"),
        "a clean env must say so: {}",
        clean.message
    );

    let leaking = verdict(
        PATH,
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
