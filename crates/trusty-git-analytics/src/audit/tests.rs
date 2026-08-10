//! Tests for the AUDIT sweep and the `tga audit` clap wiring.

use std::time::Instant;

use clap::{Args as _, FromArgMatches, Parser};

use crate::audit::{run_full_sweep, AuditSweepStats, StageStatus, SweepOptions, SweepStage};
use crate::commands::audit::AuditArgs;
use crate::core::config::Config;
use crate::core::db::Database;

/// The order `run_full_sweep` is contracted to execute in.
///
/// Data-flow order, not DOC-67 §5's prose order — deployments and incidents
/// populate what dora reduces, and report renders what everything else wrote.
const EXPECTED_ORDER: [SweepStage; 8] = [
    SweepStage::Collect,
    SweepStage::Classify,
    SweepStage::JiraSync,
    SweepStage::Deployments,
    SweepStage::Incidents,
    SweepStage::Dora,
    SweepStage::PrMetrics,
    SweepStage::Report,
];

// ---------------------------------------------------------------------------
// AuditSweepStats — per-stage outcome reporting, including a failing stage
// ---------------------------------------------------------------------------

#[test]
fn failed_stage_is_recorded_and_does_not_stop_the_sweep() {
    let mut stats = AuditSweepStats::default();

    stats.record(SweepStage::Collect, Instant::now(), Ok(()));
    stats.record(
        SweepStage::JiraSync,
        Instant::now(),
        Err(anyhow::anyhow!("no JIRA project configured")),
    );
    stats.record(SweepStage::Report, Instant::now(), Ok(()));

    // Every stage recorded, in call order — the failure did not truncate the run.
    let stages: Vec<_> = stats.outcomes.iter().map(|o| o.stage).collect();
    assert_eq!(
        stages,
        vec![
            SweepStage::Collect,
            SweepStage::JiraSync,
            SweepStage::Report
        ]
    );

    // The failure is individually observable, with its message intact.
    let failures: Vec<_> = stats.failures().collect();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].stage, SweepStage::JiraSync);
    assert_eq!(
        failures[0].status,
        StageStatus::Failed("no JIRA project configured".to_string())
    );
    assert!(stats.any_failed());
}

#[test]
fn error_cause_chain_is_preserved_in_the_stage_record() {
    let mut stats = AuditSweepStats::default();
    let err = anyhow::anyhow!("connection refused").context("fetching deploy events");

    stats.record(SweepStage::Deployments, Instant::now(), Err(err));

    let StageStatus::Failed(msg) = &stats.outcomes[0].status else {
        panic!("expected a failed status");
    };
    assert!(
        msg.contains("fetching deploy events") && msg.contains("connection refused"),
        "cause chain lost: {msg}"
    );
}

#[test]
fn summary_counts_successes_and_failures() {
    let mut stats = AuditSweepStats::default();
    assert_eq!(stats.summary(), "0 of 0 stage(s) succeeded");
    assert!(!stats.any_failed());

    stats.record(SweepStage::Collect, Instant::now(), Ok(()));
    stats.record(
        SweepStage::Dora,
        Instant::now(),
        Err(anyhow::anyhow!("boom")),
    );
    assert_eq!(stats.summary(), "1 of 2 stage(s) succeeded");
}

#[test]
fn stage_names_match_their_subcommands() {
    assert_eq!(SweepStage::JiraSync.as_str(), "jira sync");
    assert_eq!(SweepStage::PrMetrics.to_string(), "pr-metrics");
    assert_eq!(SweepStage::Deployments.as_str(), "deployments collect");
}

// ---------------------------------------------------------------------------
// run_full_sweep — sequencing over a real (empty) database
// ---------------------------------------------------------------------------

/// An empty config has no repositories, no JIRA, and no DORA sources, so no
/// stage touches the network. Stages that need configuration that is absent
/// fail — which is the point: the sweep must record them and keep going.
#[tokio::test]
async fn sweep_runs_every_stage_in_order_and_survives_failures() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut db = Database::open(&dir.path().join("tga.db")).expect("open db");
    let options = SweepOptions {
        output: Some(dir.path().join("out")),
        weeks: Some(1),
    };

    let stats = run_full_sweep(&Config::default(), &mut db, &options, None)
        .await
        .expect("sequencing itself must not fail");

    let stages: Vec<_> = stats.outcomes.iter().map(|o| o.stage).collect();
    assert_eq!(stages, EXPECTED_ORDER.to_vec());

    // JIRA is unconfigured here, so that stage must have failed — and the
    // seven stages after it must still have run.
    assert!(
        stats.any_failed(),
        "an unconfigured JIRA sync should have been recorded as a failure"
    );
    assert!(
        stats.failures().any(|o| o.stage == SweepStage::JiraSync),
        "expected the jira sync stage among the failures, got {:?}",
        stats.failures().map(|o| o.stage).collect::<Vec<_>>()
    );
    let jira_index = stages
        .iter()
        .position(|s| *s == SweepStage::JiraSync)
        .expect("jira stage present");
    assert_eq!(
        &stages[jira_index + 1..],
        &EXPECTED_ORDER[jira_index + 1..],
        "stages after the failing one were skipped"
    );
}

#[tokio::test]
async fn sweep_writes_reports_into_the_requested_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("audit-out");
    let mut db = Database::open(&dir.path().join("tga.db")).expect("open db");
    let options = SweepOptions {
        output: Some(out.clone()),
        weeks: None,
    };

    let stats = run_full_sweep(&Config::default(), &mut db, &options, None)
        .await
        .expect("sweep");

    let report = stats
        .outcomes
        .iter()
        .find(|o| o.stage == SweepStage::Report)
        .expect("report stage ran");
    assert_eq!(
        report.status,
        StageStatus::Succeeded,
        "report stage failed: {:?}",
        report.status
    );
    assert!(
        out.is_dir(),
        "report stage did not create {}",
        out.display()
    );
}

// ---------------------------------------------------------------------------
// clap wiring — `tga audit` parses its flags and dispatches
// ---------------------------------------------------------------------------

/// Standalone parser so the wiring can be asserted without building the whole
/// `tga` CLI, which the binary target owns.
#[derive(Parser, Debug)]
struct AuditOnly {
    #[command(flatten)]
    args: AuditArgs,
}

#[test]
fn audit_args_parse_every_flag() {
    let parsed = AuditOnly::try_parse_from([
        "tga-audit",
        "--org",
        "acme",
        "--title",
        "Acme DD",
        "--analyst",
        "J. Reviewer",
        "--client",
        "Acme Holdings",
        "--output",
        "/tmp/acme-dd",
        "--weeks",
        "26",
    ])
    .expect("flags must parse");

    assert_eq!(parsed.args.org.as_deref(), Some("acme"));
    assert_eq!(parsed.args.title.as_deref(), Some("Acme DD"));
    assert_eq!(parsed.args.analyst.as_deref(), Some("J. Reviewer"));
    assert_eq!(parsed.args.client.as_deref(), Some("Acme Holdings"));
    assert_eq!(
        parsed.args.output.as_deref(),
        Some(std::path::Path::new("/tmp/acme-dd"))
    );
    assert_eq!(parsed.args.weeks, Some(26));
}

#[test]
fn audit_runs_with_no_flags_at_all() {
    // DOC-67 §2: `tga audit` must be runnable as one shot with no operator
    // input, so no flag may be required.
    let parsed = AuditOnly::try_parse_from(["tga-audit"]).expect("bare invocation must parse");
    assert!(parsed.args.org.is_none());
    assert!(parsed.args.output.is_none());
}

#[test]
fn audit_takes_no_positional_arguments() {
    // A positional would be a place for an operator to be asked for something.
    let err = AuditOnly::try_parse_from(["tga-audit", "acme"])
        .expect_err("a positional argument must be rejected");
    assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn audit_args_expose_a_complete_clap_command() {
    // Guards the derive itself: every declared flag must reach the command.
    let cmd = AuditArgs::augment_args(clap::Command::new("audit"));
    let names: Vec<_> = cmd.get_arguments().map(|a| a.get_id().as_str()).collect();
    for expected in ["org", "title", "analyst", "client", "output", "weeks"] {
        assert!(
            names.contains(&expected),
            "missing --{expected} in {names:?}"
        );
    }

    let matches = cmd
        .try_get_matches_from(["audit", "--analyst", "me"])
        .expect("parse");
    let args = AuditArgs::from_arg_matches(&matches).expect("from_arg_matches");
    assert_eq!(args.analyst.as_deref(), Some("me"));
}
