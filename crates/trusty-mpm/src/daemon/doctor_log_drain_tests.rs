//! Verdict-matrix tests for the `log_drain` doctor row (#6535).
//!
//! Why: the row's job is to distinguish "off", "on and working", and "on and
//! broken". Every one of those is a different operator action, so each gets its
//! own assertion — including the fail-open case a green row would hide.
//! What: drives [`build_log_drain_check`] directly, so no config file, state
//! directory, or daemon is involved.

use std::path::PathBuf;
use std::time::Duration;

use super::*;
use crate::core::trusty_tools_config::LOG_DRAIN_DEFAULT_INTERVAL_SECS;
use trusty_common::log_drain::DestinationUri;

/// An enabled plan pointing at `file:///tmp/drain`.
fn plan() -> LogDrainSetting {
    LogDrainSetting::Enabled(Box::new(
        crate::core::trusty_tools_config::ResolvedLogDrain {
            destination: DestinationUri::File {
                path: PathBuf::from("/tmp/drain"),
            },
            destination_display: "file:///tmp/drain".to_string(),
            interval: Duration::from_secs(LOG_DRAIN_DEFAULT_INTERVAL_SECS),
            max_file_bytes: 1024,
            secrets: Vec::new(),
            github_id: Some("octocat".to_string()),
            session_id: Some("sess-1".to_string()),
            sources: Vec::new(),
        },
    ))
}

/// A recorded status with `outcome` and `detail`.
fn status(outcome: DrainOutcome, detail: &str) -> LogDrainStatus {
    LogDrainStatus {
        outcome,
        at: "2026-09-01T00:00:00Z".to_string(),
        destination: Some("file:///tmp/drain".to_string()),
        scheme: Some("file".to_string()),
        uploaded: 3,
        skipped_unchanged: 1,
        detail: detail.to_string(),
    }
}

#[test]
fn config_error_fails() {
    let err = LogDrainConfigError::MissingDestination;
    let check = build_log_drain_check(Err(&err), None);
    assert_eq!(check.name, "log_drain");
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.message.contains("destination is unset"),
        "message must quote the config error: {}",
        check.message
    );
}

#[test]
fn disabled_is_ok() {
    let check = build_log_drain_check(Ok(&LogDrainSetting::Disabled), None);
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(check.message.contains("disabled"), "{}", check.message);
}

#[test]
fn enabled_with_no_run_warns() {
    let setting = plan();
    let check = build_log_drain_check(Ok(&setting), None);
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(
        check.message.contains("file → file:///tmp/drain"),
        "the row must name the scheme and destination: {}",
        check.message
    );
    assert!(check.message.contains("no run"), "{}", check.message);
}

#[test]
fn a_successful_run_is_ok() {
    let setting = plan();
    let recorded = status(DrainOutcome::Success, "3 file(s) uploaded");
    let check = build_log_drain_check(Ok(&setting), Some(&recorded));
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(
        check.message.contains("3 file(s) uploaded"),
        "{}",
        check.message
    );
}

#[test]
fn a_failed_run_fails() {
    // #6535 fail-open guard: a recorded failure must never read as a healthy
    // drain, whatever the counts on the record say.
    let setting = plan();
    let recorded = status(DrainOutcome::Failed, "cannot reach the destination: nope");
    let check = build_log_drain_check(Ok(&setting), Some(&recorded));
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.message.contains("FAILED"),
        "the row must say so out loud: {}",
        check.message
    );
    assert!(
        check.message.contains("cannot reach the destination"),
        "{}",
        check.message
    );
}

#[test]
fn a_stale_disabled_record_under_an_enabled_config_warns() {
    // The record predates the operator turning the drain on, so it says nothing
    // about whether the drain works now.
    let setting = plan();
    let recorded = status(DrainOutcome::SkippedDisabled, "log_drain is disabled");
    let check = build_log_drain_check(Ok(&setting), Some(&recorded));
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("no run"), "{}", check.message);
}
