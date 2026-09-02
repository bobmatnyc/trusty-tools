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
use crate::daemon::log_drain::LogDrainDestinationStatus;
use trusty_common::log_drain::{DestinationUri, DrainTarget};

/// The project every fixture pass drains under.
const FIXTURE_PROJECT: &str = "octocat/fixtures";

/// One resolved pass at `file://<path>`, with no sources.
fn group(path: &str) -> crate::core::trusty_tools_config::ResolvedDrainDestination {
    crate::core::trusty_tools_config::ResolvedDrainDestination {
        destination: DestinationUri::File {
            path: PathBuf::from(path),
        },
        destination_display: format!("file://{path}"),
        target: DrainTarget {
            owner: "octocat".to_string(),
            project: "fixtures".to_string(),
        },
        sources: Vec::new(),
    }
}

/// An enabled plan over `destinations`.
fn plan_over(
    destinations: Vec<crate::core::trusty_tools_config::ResolvedDrainDestination>,
) -> LogDrainSetting {
    plan_with_disabled(destinations, Vec::new())
}

/// An enabled plan over `destinations`, plus the sources switched off.
fn plan_with_disabled(
    destinations: Vec<crate::core::trusty_tools_config::ResolvedDrainDestination>,
    disabled: Vec<crate::core::trusty_tools_config::DisabledSource>,
) -> LogDrainSetting {
    LogDrainSetting::Enabled(Box::new(
        crate::core::trusty_tools_config::ResolvedLogDrain {
            destinations,
            disabled,
            interval: Duration::from_secs(LOG_DRAIN_DEFAULT_INTERVAL_SECS),
            max_file_bytes: 1024,
            max_wire_bytes: 1024,
            secrets: Vec::new(),
        },
    ))
}

/// An enabled plan pointing at `file:///tmp/drain`.
fn plan() -> LogDrainSetting {
    plan_over(vec![group("/tmp/drain")])
}

/// One recorded destination outcome.
fn recorded(path: &str, outcome: DrainOutcome, detail: &str) -> LogDrainDestinationStatus {
    LogDrainDestinationStatus {
        destination: format!("file://{path}"),
        project: FIXTURE_PROJECT.to_string(),
        scheme: "file".to_string(),
        outcome,
        uploaded: 3,
        skipped_unchanged: 1,
        detail: detail.to_string(),
    }
}

/// A recorded status with `outcome` and `detail`, over one destination.
fn status(outcome: DrainOutcome, detail: &str) -> LogDrainStatus {
    status_over(
        outcome,
        vec![recorded("/tmp/drain", outcome, detail)],
        detail,
    )
}

/// A recorded status over an explicit destination list.
fn status_over(
    outcome: DrainOutcome,
    destinations: Vec<LogDrainDestinationStatus>,
    detail: &str,
) -> LogDrainStatus {
    LogDrainStatus {
        outcome,
        at: "2026-09-01T00:00:00Z".to_string(),
        destinations,
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
        check
            .message
            .contains("file → file:///tmp/drain [octocat/fixtures]"),
        "the row must name the scheme, destination, and project: {}",
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
fn the_row_lists_every_destination_with_its_own_outcome() {
    // #6657: one unreachable account among two is a different repair from both
    // being down, so the row names each destination and quotes each verdict.
    let setting = plan_over(vec![group("/tmp/drain-a"), group("/tmp/drain-b")]);
    let recorded_status = status_over(
        DrainOutcome::Failed,
        vec![
            recorded("/tmp/drain-a", DrainOutcome::Success, "2 file(s) uploaded"),
            recorded(
                "/tmp/drain-b",
                DrainOutcome::Failed,
                "cannot reach the destination: no such directory",
            ),
        ],
        "aggregate detail nobody should be reading here",
    );
    let check = build_log_drain_check(Ok(&setting), Some(&recorded_status));

    // One destination failing fails the row.
    assert_eq!(check.status, CheckStatus::Fail);
    // Both destinations are named, in plan order, with their own verdicts.
    assert!(
        check.message.contains(
            "file → file:///tmp/drain-a [octocat/fixtures], \
             file → file:///tmp/drain-b [octocat/fixtures]"
        ),
        "the row must name every configured pass: {}",
        check.message
    );
    assert!(
        check
            .message
            .contains("file:///tmp/drain-a [octocat/fixtures]: ok — 2 file(s) uploaded"),
        "the healthy destination keeps its own verdict: {}",
        check.message
    );
    assert!(
        check
            .message
            .contains("file:///tmp/drain-b [octocat/fixtures]: FAILED — cannot reach"),
        "the broken destination is named as the broken one: {}",
        check.message
    );
}

#[test]
fn a_record_predating_per_destination_outcomes_still_reads() {
    // A `status.json` written before #6657 carries no breakdown; the row falls
    // back to its single detail line rather than claiming nothing ran.
    let setting = plan();
    let recorded_status = status_over(DrainOutcome::Success, Vec::new(), "3 file(s) uploaded");
    let check = build_log_drain_check(Ok(&setting), Some(&recorded_status));
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(
        check.message.contains("3 file(s) uploaded"),
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

#[test]
fn the_row_lists_a_disabled_source() {
    // #6657: `enabled: false` on one project is a deliberate opt-out, and the
    // row has to distinguish it from a project that fell out of the config.
    let setting = plan_with_disabled(
        vec![group("/tmp/drain")],
        vec![crate::core::trusty_tools_config::DisabledSource {
            crate_name: "trusty-code".to_string(),
            destination_display: Some("s3://owner-bucket/live".to_string()),
        }],
    );
    let check = build_log_drain_check(Ok(&setting), None);
    assert!(
        check
            .message
            .contains("trusty-code → s3://owner-bucket/live: disabled"),
        "the row must name the opted-out project and where it would have gone: {}",
        check.message
    );
}
