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
        "stages after the failing jira-sync stage should still have run, but were missing"
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

    // `is_dir()` alone would pass for a sweep that creates the directory and
    // writes nothing, so assert the artifacts themselves. `summary.csv` and
    // `report.json` are fixed names from the CSV and JSON formatters, and
    // `pr-metrics.csv` is the name the sweep hands `pr-metrics --output`.
    let written: Vec<String> = std::fs::read_dir(&out)
        .expect("read report dir")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        written.len() > 1,
        "report stage wrote {} file(s) into {}: {written:?}",
        written.len(),
        out.display()
    );
    for expected in [
        crate::report::formatters::csv::SUMMARY_CSV,
        crate::report::formatters::json::REPORT_JSON,
        "pr-metrics.csv",
    ] {
        assert!(
            out.join(expected).is_file(),
            "expected artifact {expected} missing from {}: {written:?}",
            out.display()
        );
    }
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
    // Which `ErrorKind` clap picks for an unexpected positional depends on the
    // command shape and the clap version, so assert the observable contract —
    // the parse fails and the message names the rejected argument — instead of
    // pinning the discriminant.
    let err = AuditOnly::try_parse_from(["tga-audit", "acme"])
        .expect_err("a positional argument must be rejected");
    let rendered = err.to_string();
    assert!(
        rendered.contains("acme"),
        "rejection must name the offending argument, got: {rendered}"
    );
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

// ---------------------------------------------------------------------------
// Gaps & Caveats lines from the sweep's own record (#5239, #5244)
// ---------------------------------------------------------------------------

/// A config holding no credentials. The redaction path itself is proved end to
/// end against a real token in
/// `crate::report::dd_manifest_tests::a_token_straddling_the_excerpt_boundary_leaves_no_fragment`;
/// these tests are about the wording and ordering of the lines.
const NO_SECRETS: &[&str] = &[];

#[test]
fn sweep_gap_lines_name_each_failed_stage() {
    let mut stats = AuditSweepStats::default();
    stats.record(SweepStage::Collect, Instant::now(), Ok(()));
    stats.record(
        SweepStage::JiraSync,
        Instant::now(),
        Err(anyhow::anyhow!("no JIRA project configured")),
    );
    stats.record(
        SweepStage::Dora,
        Instant::now(),
        Err(anyhow::anyhow!("fact_deployments is empty")),
    );

    let lines = crate::audit::sweep_gap_lines(&stats, NO_SECRETS);

    assert_eq!(lines.len(), 2, "one line per failure, none for success");
    assert!(lines[0].contains("`jira sync`"), "{}", lines[0]);
    assert!(
        lines[0].contains("no JIRA project configured"),
        "{}",
        lines[0]
    );
    assert!(lines[1].contains("`dora`"), "{}", lines[1]);
    // DOC-67 §9: the line must refuse to read as a clean result.
    for line in &lines {
        assert!(line.contains("not assessed"), "{line}");
    }
    // Execution order, so two runs over the same failures read identically.
    assert_eq!(lines, crate::audit::sweep_gap_lines(&stats, NO_SECRETS));
}

#[test]
fn sweep_gap_lines_are_empty_for_a_clean_run() {
    let mut stats = AuditSweepStats::default();
    for stage in EXPECTED_ORDER {
        stats.record(stage, Instant::now(), Ok(()));
    }
    assert!(crate::audit::sweep_gap_lines(&stats, NO_SECRETS).is_empty());
}

#[test]
fn long_stage_reasons_are_truncated() {
    let mut stats = AuditSweepStats::default();
    stats.record(
        SweepStage::Collect,
        Instant::now(),
        Err(anyhow::anyhow!("x".repeat(4000))),
    );

    let line = crate::audit::sweep_gap_lines(&stats, NO_SECRETS).remove(0);

    assert!(line.contains('…'), "a long reason is excerpted: {line}");
    assert!(
        line.chars().count() < 400,
        "one verbose error must not dominate the Gaps section ({} chars)",
        line.chars().count()
    );
}

#[test]
fn data_handling_note_is_a_pending_claim() {
    let note = crate::audit::DATA_HANDLING_NOTE;
    // DOC-67 §10: pending, never asserted — and #5218 is where the real one comes from.
    assert!(note.contains("pending"), "{note}");
    assert!(note.contains("#5218"), "{note}");
    // §10's exact scope claim; the broader "no code" claim is wrong and must
    // not appear, because free-text columns can carry pasted snippets.
    assert!(note.contains("no file content, diffs, patches, hunks, or blobs"));
    assert!(!note.contains("no code"), "{note}");
}

// ---------------------------------------------------------------------------
// Invoking trusty-review as a subprocess (#5238)
// ---------------------------------------------------------------------------

#[test]
fn artifact_paths_are_parsed_from_stdout() {
    // trusty-review prints one written path per line on stdout; blank lines and
    // trailing whitespace must not become phantom artifacts.
    let stdout = "  /out/acme.md\n\n/out/acme.json\n";
    let paths = crate::audit::artifact_paths(stdout);
    assert_eq!(
        paths,
        vec![
            std::path::PathBuf::from("/out/acme.md"),
            std::path::PathBuf::from("/out/acme.json")
        ]
    );
    assert!(crate::audit::artifact_paths("").is_empty());
}

#[tokio::test]
async fn missing_binary_is_a_named_actionable_error() {
    // A renderer that is not installed must produce a message an operator can
    // act on — never a panic, and never a silent skip that leaves the run
    // looking successful with no report.
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = dir.path().join("manifest.toml");
    std::fs::write(&manifest, "[report]\ntitle = \"T\"\n").expect("write");

    // The binary is passed in, never installed through the environment: this is
    // the same path `run_review_report` takes once resolution has happened, and
    // it stays sound under the parallel harness (#5308 review).
    let missing = dir.path().join("definitely-not-installed");
    let err =
        super::review::run_review_report_with(missing.display().to_string(), &manifest, dir.path())
            .await
            .expect_err("a missing binary must be an error");

    let msg = err.to_string();
    assert!(
        matches!(err, crate::audit::ReviewRunError::BinaryNotFound { .. }),
        "{msg}"
    );
    assert!(
        msg.contains("TRUSTY_REVIEW_BIN"),
        "names the override: {msg}"
    );
    assert!(msg.contains("cargo install"), "names the fix: {msg}");
    assert!(
        msg.contains(&manifest.display().to_string()),
        "the written manifest is still usable and must be named: {msg}"
    );
}

/// The rule is exercised as a pure function over the override value, which is
/// what `resolve_review_binary` reads `TRUSTY_REVIEW_BIN` into. Mutating the
/// process environment to test it would be unsound under the parallel test
/// harness, and no `unsafe` block can make it sound (#5308 review).
#[test]
fn binary_resolution_prefers_the_env_override() {
    use super::review::binary_from_override;

    assert_eq!(
        binary_from_override(Some("/opt/bin/trusty-review")),
        "/opt/bin/trusty-review"
    );
    assert_eq!(
        binary_from_override(Some("")),
        crate::audit::DEFAULT_REVIEW_BIN,
        "an empty override falls back to the PATH lookup"
    );
    assert_eq!(binary_from_override(None), crate::audit::DEFAULT_REVIEW_BIN);
    // The public entry point returns one of the two, whatever this machine's
    // environment happens to hold — read only, never written.
    let resolved = crate::audit::resolve_review_binary();
    assert!(!resolved.is_empty(), "{resolved}");
}
