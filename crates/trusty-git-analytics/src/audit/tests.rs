//! Tests for the AUDIT sweep and the `tga audit` clap wiring.

use std::time::Instant;

use clap::{Args as _, FromArgMatches, Parser};

use crate::audit::{run_full_sweep, AuditSweepStats, StageStatus, SweepOptions, SweepStage};
use crate::commands::audit::AuditArgs;
use crate::core::config::Config;
use crate::core::db::Database;
use crate::core::progress::{ProgressBus, Stage};

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
    // The "stage N of 8" denominator in the progress start events is this
    // constant; a stage added without updating it would lie to the operator.
    assert_eq!(stages.len(), super::sweep::TOTAL_STAGES);

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
// Progress reporting from every stage, not just collection (#5361)
// ---------------------------------------------------------------------------

/// #5361: `run_full_sweep` used to bind and discard its `progress` parameter,
/// so a caller that supplied a bus saw nothing for classify, report, pr-metrics,
/// jira sync, dora, deployments, or incidents — a ~10-minute sweep behind a
/// frozen screen. This asserts the non-collection stages reach the bus the
/// caller passed in; before the fix the drained event list is empty.
#[tokio::test]
async fn sweep_emits_progress_for_non_collection_stages() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut db = Database::open(&dir.path().join("tga.db")).expect("open db");
    let options = SweepOptions {
        output: Some(dir.path().join("out")),
        weeks: Some(1),
    };
    let bus = ProgressBus::new();

    let stats = run_full_sweep(&Config::default(), &mut db, &options, Some(&bus))
        .await
        .expect("sweep");

    let events = bus.drain();
    assert_eq!(bus.dropped(), 0, "the run must fit in the default ring");
    assert!(
        !events.is_empty(),
        "a bus handed to run_full_sweep received nothing at all"
    );

    // Report and classify are the stages furthest from collection: neither has
    // any instrumentation of its own, so if the parameter is not threaded they
    // are silent no matter what the collection pipeline does.
    for stage in [SweepStage::Classify, SweepStage::Report] {
        let mine: Vec<_> = events
            .iter()
            .filter(|e| e.stage == Stage::Audit && e.target == stage.as_str())
            .collect();
        assert!(
            mine.iter().any(|e| !e.is_terminal()),
            "no start event for the {stage} stage: {events:#?}"
        );
        assert!(
            mine.iter().any(|e| e.is_terminal()),
            "no completion event for the {stage} stage: {events:#?}"
        );
    }

    // Every stage, not just those two — and the bus's verdict agrees with the
    // recorded one, so a stage cannot report green on screen and red in stats.
    for outcome in &stats.outcomes {
        let terminal = events
            .iter()
            .find(|e| {
                e.stage == Stage::Audit && e.target == outcome.stage.as_str() && e.is_terminal()
            })
            .unwrap_or_else(|| panic!("no terminal event for {}", outcome.stage));
        let announced_failure = terminal
            .outcome
            .as_ref()
            .is_some_and(|o| matches!(o, crate::core::progress::Outcome::Failed { .. }));
        assert_eq!(
            announced_failure,
            outcome.status.is_failure(),
            "{} announced and recorded different verdicts",
            outcome.stage
        );
    }
}

/// An inactive bus must change nothing — the `None` caller's contract, proved
/// through the one shape that is observable from outside.
#[tokio::test]
async fn a_disabled_bus_stays_a_no_op() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut db = Database::open(&dir.path().join("tga.db")).expect("open db");
    let options = SweepOptions {
        output: Some(dir.path().join("out")),
        weeks: Some(1),
    };
    let bus = ProgressBus::disabled();

    let stats = run_full_sweep(&Config::default(), &mut db, &options, Some(&bus))
        .await
        .expect("sweep");

    assert!(bus.drain().is_empty(), "a disabled bus queued events");
    assert_eq!(bus.dropped(), 0);
    let stages: Vec<_> = stats.outcomes.iter().map(|o| o.stage).collect();
    assert_eq!(
        stages,
        EXPECTED_ORDER.to_vec(),
        "sequencing must not depend on whether anybody is watching"
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

/// #5321: a `collect` stage that fell back to stale local refs used to reach
/// the report as nothing at all.
///
/// The sweep hard-codes `--allow-stale` (DOC-67 §9), so an unreachable remote
/// is not an error: the pipeline walks whatever the local clone already had and
/// the stage records `Succeeded`. `sweep_gap_lines` only reads
/// [`AuditSweepStats::failures`], so a succeeded stage contributed no line and
/// the manifest carried no note — the reader could not tell "no stale refs" from
/// "the data may be months behind". Before the fix this test fails on the final
/// assertion: the gap lines name the failed `jira sync` stage and nothing else.
#[tokio::test]
async fn a_repo_that_fell_back_to_stale_local_refs_is_named_in_the_gap_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_path = dir.path().join("acme-service");
    // An `origin` whose scheme has no transport: the fetch fails without
    // touching the network, exactly as the reported run's SSH remote did
    // ("unsupported URL protocol"), so the test is offline and deterministic.
    init_repo_with_dead_origin(
        &repo_path,
        "sshx://git@example.invalid/acme/acme-service.git",
    );

    let mut config = Config::default();
    config
        .repositories
        .push(crate::core::config::RepositoryConfig {
            name: Some("acme-service".to_string()),
            path: repo_path,
            branch: None,
            since_date: None,
            until_date: None,
            org: None,
            head_only: false,
            fetch_timeout_secs: None,
        });

    let mut db = Database::open(&dir.path().join("tga.db")).expect("open db");
    let options = SweepOptions {
        output: Some(dir.path().join("out")),
        weeks: Some(1),
    };
    let stats = run_full_sweep(&config, &mut db, &options, None)
        .await
        .expect("sweep");

    // The defect's own shape: the stage reports `ok`. That is correct — the
    // sweep must not abort — which is exactly why the degradation needs its own
    // channel to the report.
    let collect = stats
        .outcomes
        .iter()
        .find(|o| o.stage == SweepStage::Collect)
        .expect("collect stage ran");
    assert_eq!(
        collect.status,
        StageStatus::Succeeded,
        "collect should still report ok under --allow-stale: {:?}",
        collect.status
    );

    let lines = crate::audit::sweep_gap_lines(&stats, NO_SECRETS);
    let stale = lines
        .iter()
        .find(|l| l.contains("acme-service"))
        .unwrap_or_else(|| {
            panic!("no Gaps & Caveats line names the repo that fell back to stale refs: {lines:#?}")
        });
    assert!(
        stale.contains("origin"),
        "the line must name the affected remote: {stale}"
    );
    assert!(
        stale.contains("behind the true remote state"),
        "the line must say the data may be out of date: {stale}"
    );
}

/// A git repository with one commit and an `origin` that cannot be fetched.
fn init_repo_with_dead_origin(path: &std::path::Path, remote_url: &str) {
    std::fs::create_dir_all(path).expect("mkdir");
    let repo = git2::Repository::init(path).expect("git init");
    repo.remote("origin", remote_url).expect("add origin");

    std::fs::write(path.join("README.md"), "acme").expect("write file");
    let mut index = repo.index().expect("index");
    index
        .add_path(std::path::Path::new("README.md"))
        .expect("index add");
    index.write().expect("index write");
    let tree = repo
        .find_tree(index.write_tree().expect("write_tree"))
        .expect("find_tree");
    let sig = git2::Signature::now("Test", "t@example.com").expect("signature");
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .expect("commit");
}

/// The stale-refs line travels the same redaction path a stage message does,
/// and it needs to: git2 quotes the remote URL back in the error, so an HTTPS
/// remote with an embedded token puts that token in the manifest and the
/// delivered report. Also pins the ordering — failed stages first, then the
/// repositories that were collected anyway.
#[test]
fn a_credential_in_a_fetch_error_never_reaches_the_gap_line() {
    let mut stats = AuditSweepStats::default();
    stats.record(
        SweepStage::Dora,
        Instant::now(),
        Err(anyhow::anyhow!("fact_deployments is empty")),
    );
    stats.record_stale_fetch(crate::audit::StaleFetch {
        repo: "acme-service".to_string(),
        remote: "origin".to_string(),
        error: "failed to connect to https://x-access-token:ghp_SECRETVALUE@github.com/acme/svc"
            .to_string(),
    });

    let lines = crate::audit::sweep_gap_lines(&stats, &["ghp_SECRETVALUE"]);

    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(lines[0].contains("`dora`"), "{}", lines[0]);
    assert!(lines[1].contains("acme-service"), "{}", lines[1]);
    assert!(
        !lines[1].contains("ghp_SECRETVALUE"),
        "the token survived into the report: {}",
        lines[1]
    );
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

// ─── #5454: inference is required ────────────────────────────────────────────

/// #5454 regression. Before this change `invoke` built `report --manifest <m>
/// --analyze --out <dir>` and never passed `--synthesize`, so `model.synthesis`
/// was `None` on every audit that had ever run and the whole report was
/// deterministic. Pins the flag into the argument vector, and pins the rest of
/// the vector alongside it so a future edit cannot drop `--analyze` while
/// "fixing" this one.
#[test]
fn invocation_requests_inference() {
    use std::path::Path;

    let args = super::review::report_args(Path::new("/o/manifest.toml"), Path::new("/o"));
    let rendered: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    assert_eq!(
        rendered,
        vec![
            "report",
            "--manifest",
            "/o/manifest.toml",
            "--analyze",
            "--synthesize",
            "--out",
            "/o",
        ],
        "the audit's renderer invocation must request inference"
    );
}

/// #5454. The credential is the one prerequisite knowable before the sweep
/// starts, and DOC-67 §2 gives the run a single non-interactive shot — so a
/// missing key must be named up front, with the variable and the way to set it,
/// rather than surfacing minutes later at the render step.
#[test]
fn absent_credential_is_a_named_actionable_error() {
    use super::review::credential_is_present;

    assert!(!credential_is_present(None), "unset is absent");
    assert!(!credential_is_present(Some("")), "empty is absent");
    assert!(
        !credential_is_present(Some("   \n")),
        "whitespace-only is absent"
    );

    let msg = crate::audit::MissingInferenceCredential.to_string();
    assert!(
        msg.contains(crate::audit::ENV_INFERENCE_CREDENTIAL),
        "names the variable: {msg}"
    );
    assert!(
        msg.contains("export OPENROUTER_API_KEY="),
        "says how to set it: {msg}"
    );
}

/// #5454. The preflight must not stand between an operator who HAS a key and
/// their run, and it must never copy the key anywhere — the message it can
/// produce is a constant with no interpolation site for one.
#[test]
fn present_credential_passes_the_precheck() {
    use super::review::credential_is_present;

    let secret = "sk-or-v1-DEADBEEFdeadbeef";
    assert!(credential_is_present(Some(secret)));

    let msg = crate::audit::MissingInferenceCredential.to_string();
    assert!(!msg.contains(secret), "no key material may appear: {msg}");
    assert!(!msg.contains("sk-or"), "no key-shaped text: {msg}");
}

// ─── #5454 review: exit 0 is not evidence of a synthesis pass ────────────────

/// A `ReviewRun` shaped like a clean render: the child exited 0 and printed both
/// halves of the report pair.
///
/// Writes `report_json` to a real file, because the check reads the artifact the
/// renderer wrote rather than anything the child said about it.
fn successful_run_over(dir: &std::path::Path, report_json: &str) -> crate::audit::ReviewRun {
    let md = dir.join("2026-08-11-acme.md");
    let json = dir.join("2026-08-11-acme.json");
    std::fs::write(&md, "# Acme\n").expect("write md");
    std::fs::write(&json, report_json).expect("write json");
    crate::audit::ReviewRun {
        success: true,
        code: Some(0),
        stdout: format!("{}\n{}\n", md.display(), json.display()),
        stderr: String::new(),
        artifacts: vec![md, json],
    }
}

/// The exact JSON a pre-0.15 `trusty-review` writes when its provider fails
/// mid-render: `SynthesisStatus::Unavailable` and every prose field empty.
const DEGRADED_0_14_REPORT: &str = r#"{
  "title": "Acme — Technical Due Diligence",
  "synthesis": {
    "status": { "state": "unavailable", "reason": "provider build failed: 401 Unauthorized" },
    "top_risks": [],
    "findings": [],
    "notes": []
  }
}"#;

/// #5454 review, THE arm this guard exists for. A new `tga` beside a pre-0.15
/// `trusty-review` is an ordinary pairing — the two are resolved through PATH,
/// not a Cargo edge — and that renderer takes `--synthesize`, falls back to a
/// deterministic report on any provider failure, writes it, and exits 0.
/// `ReviewRun::success` is `output.status.success()` and nothing more, so the
/// `if !run.success` check never fired and `tga audit` reported a clean pass over
/// the exact narrative-free report #5454 exists to abolish.
///
/// Pins the failure AND its remedy: a message that says only "no narrative"
/// leaves the operator with a symptom and no action.
#[test]
fn exit_zero_over_a_narrative_free_report_is_a_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let run = successful_run_over(dir.path(), DEGRADED_0_14_REPORT);
    assert!(run.success, "the child exited 0 — that is the whole point");

    let err = crate::audit::require_rendered_report_carries_synthesis(&run)
        .expect_err("a report with no written analysis must not pass as a successful audit");
    let msg = err.to_string();
    assert!(
        msg.contains("trusty-review") && msg.contains("predates"),
        "the message must name the stale renderer as the cause: {msg}"
    );
    assert!(
        msg.contains("tctl install trusty-review"),
        "the message must name the upgrade that fixes it: {msg}"
    );

    // The rule itself, over both narrative-free shapes: 0.14's degraded object,
    // and a report carrying no `synthesis` key at all.
    use crate::audit::review::json_carries_synthesis;
    assert_eq!(json_carries_synthesis(DEGRADED_0_14_REPORT), Some(false));
    assert_eq!(json_carries_synthesis(r#"{"title": "Acme"}"#), Some(false));
}

/// The guard must not stand between an operator and a report that DID get its
/// narrative written — including the one arm 0.15 still allows through, where the
/// numeric guardrail rejected the executive summary on its own and the
/// deterministic composition (#5374) fills §2 while the top-risk rows survive.
#[test]
fn a_synthesized_report_passes_the_check() {
    use crate::audit::review::json_carries_synthesis;

    let dir = tempfile::tempdir().expect("tempdir");
    let full = r#"{
      "title": "Acme",
      "synthesis": {
        "executive_summary": "Two of three applications carry RED findings.",
        "top_risks": [{"description": "No tests", "severity": "RED", "cost": "high", "apps": "web"}],
        "findings": [],
        "notes": []
      }
    }"#;
    crate::audit::require_rendered_report_carries_synthesis(&successful_run_over(dir.path(), full))
        .expect("a synthesized report passes");

    // Guardrail rejected the summary; rows survived. Still a synthesized report.
    assert_eq!(
        json_carries_synthesis(
            r#"{"synthesis": {"top_risks": [{"description": "x"}], "findings": [], "notes": ["synthesis: rejected (unverified figure)"]}}"#
        ),
        Some(true)
    );
    // Only finding prose survived.
    assert_eq!(
        json_carries_synthesis(r#"{"synthesis": {"top_risks": [], "findings": [{"title": "x"}]}}"#),
        Some(true)
    );
    // A blank summary is not prose.
    assert_eq!(
        json_carries_synthesis(r#"{"synthesis": {"executive_summary": "   "}}"#),
        Some(false)
    );
}

/// A check that cannot be performed must fail, not pass — passing on "I could not
/// look" reopens the hole the guard closes. Covers both ways the artifact can go
/// missing: no `.json` path printed, and a path that is not JSON.
#[test]
fn an_uncheckable_report_fails_rather_than_passes() {
    use crate::audit::review::json_carries_synthesis;

    let no_json = crate::audit::ReviewRun {
        success: true,
        code: Some(0),
        stdout: "/o/report.md\n".to_string(),
        stderr: String::new(),
        artifacts: vec![std::path::PathBuf::from("/o/report.md")],
    };
    let err = crate::audit::require_rendered_report_carries_synthesis(&no_json)
        .expect_err("no .json artifact means nothing can be asserted about the report");
    assert!(err.to_string().contains("could not be checked"), "{err}");

    let dir = tempfile::tempdir().expect("tempdir");
    let err = crate::audit::require_rendered_report_carries_synthesis(&successful_run_over(
        dir.path(),
        "not json at all",
    ))
    .expect_err("an unparseable report cannot be claimed to carry a narrative");
    assert!(err.to_string().contains("could not be checked"), "{err}");

    assert_eq!(json_carries_synthesis("not json at all"), None);
}

/// #5454 review. The version skew is knowable before stage 1, and DOC-67 §2 gives
/// the sweep one non-interactive shot — so an operator learning about it only
/// after eight stages have run learns it at the worst moment. Pins the floor, the
/// parse, and the deliberate proceed-on-unreadable behaviour that leaves
/// `require_rendered_report_carries_synthesis` as the thing that actually closes
/// the hole.
#[test]
fn stale_renderer_is_rejected_before_the_sweep() {
    use crate::audit::review::{parse_review_version, version_verdict};
    use crate::audit::MIN_REVIEW_VERSION;

    assert_eq!(MIN_REVIEW_VERSION, (0, 15, 0));

    // The version actually on PATH when this defect was found. The message names
    // the version found, the floor, and the upgrade that fixes it.
    let err = version_verdict("trusty-review", "trusty-review 0.14.1\n")
        .expect_err("a pre-0.15 renderer must not clear the preflight");
    let msg = err.to_string();
    for needle in ["0.14.1", "0.15.0", "tctl install trusty-review", "exits 0"] {
        assert!(msg.contains(needle), "missing {needle:?}: {msg}");
    }

    for ok in ["trusty-review 0.15.0", "trusty-review 0.15.1", "tr v1.0.0"] {
        version_verdict("trusty-review", ok).unwrap_or_else(|e| panic!("{ok} must pass: {e}"));
    }
    // Pre-release and build suffixes read as their release core.
    assert_eq!(
        parse_review_version("trusty-review 0.15.0-rc.1+build.7"),
        Some((0, 15, 0))
    );

    // Unreadable → the caller proceeds; the delivered-artifact check is what
    // closes the hole, not this.
    for unreadable in ["", "\n\n", "trusty-review", "trusty-review unknown"] {
        assert_eq!(parse_review_version(unreadable), None, "{unreadable:?}");
        version_verdict("trusty-review", unreadable)
            .unwrap_or_else(|e| panic!("{unreadable:?} must proceed, not fail: {e}"));
    }
}

// ---------------------------------------------------------------------------
// #5670 — the analyze daemon the audit's report is built from
// ---------------------------------------------------------------------------

/// A localhost port nothing is listening on.
///
/// Binds port 0 so the OS picks a free one, then releases it — hard-coding a
/// high port instead can collide with something already bound on a busy host.
pub(super) fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let port = listener
        .local_addr()
        .expect("read the bound address")
        .port();
    drop(listener);
    port
}

/// A listener standing in for `trusty-analyze`'s `/health`, answering
/// `503 Service Unavailable` to its first `degraded_replies` callers and
/// `200 OK` to every caller after that. Returns the port it bound.
///
/// 503 is not an arbitrary sad path: it is what the real daemon answers whenever
/// trusty-search is unreachable
/// (`crates/trusty-analyze/src/service/routes.rs`'s `health`, which returns
/// `SERVICE_UNAVAILABLE` + `status: "degraded"`). `probe_once` counts only a
/// 2xx, so a 503 daemon reads to the guard exactly like no daemon.
///
/// Counting replies rather than sleeping is what makes the slow-start tests
/// deterministic — the guard's first probe is guaranteed to miss on a loaded
/// machine, where a wall-clock delay could be overtaken.
async fn serve_health(degraded_replies: usize) -> u16 {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral port");
    let port = listener
        .local_addr()
        .expect("read the bound address")
        .port();
    let served = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let served = Arc::clone(&served);
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt as _;
                let nth = served.fetch_add(1, Ordering::SeqCst);
                let reply: &[u8] = if nth < degraded_replies {
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n"
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"
                };
                let _ = stream.write_all(reply).await;
            });
        }
    });
    port
}

/// A listener answering `HTTP/1.1 200 OK` to everything, standing in for a
/// healthy daemon. Returns the port it bound.
async fn serve_healthy() -> u16 {
    serve_health(0).await
}

/// An executable stub at `dir/name` running `script`, standing in for a
/// `trusty-analyze` binary without needing one on the machine.
#[cfg(unix)]
fn stub_binary(dir: &std::path::Path, name: &str, script: &str) -> String {
    use std::os::unix::fs::PermissionsExt as _;

    let stub = dir.join(name);
    std::fs::write(&stub, script).expect("write the stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
        .expect("make the stub executable");
    stub.to_str().expect("a UTF-8 temp path").to_string()
}

/// A guard pointed at `port`, with budgets short enough for a unit test.
fn guard_on(port: u16, binary: &str) -> crate::audit::AnalyzeGuard {
    crate::audit::AnalyzeGuard {
        url: format!("http://127.0.0.1:{port}"),
        binary: binary.to_string(),
        startup_timeout: std::time::Duration::from_millis(400),
        poll_interval: std::time::Duration::from_millis(50),
    }
}

/// Both overrides win when set, and an empty one is not a setting.
///
/// The empty case matters because `trusty-audit` exports `TRUSTY_ANALYZE_BIN`
/// unconditionally, and a half-written export leaves it set to `""` — which must
/// resolve to the PATH lookup rather than to an unspawnable empty program name.
#[test]
fn analyze_resolution_prefers_the_env_overrides() {
    use crate::audit::analyze::{binary_from_override, url_from_override};
    use crate::audit::{DEFAULT_ANALYZE_BIN, DEFAULT_ANALYZE_URL};

    assert_eq!(
        binary_from_override(Some("/pinned/trusty-analyze")),
        "/pinned/trusty-analyze"
    );
    assert_eq!(binary_from_override(None), DEFAULT_ANALYZE_BIN);
    assert_eq!(binary_from_override(Some("")), DEFAULT_ANALYZE_BIN);

    assert_eq!(
        url_from_override(Some("http://127.0.0.1:9999")),
        "http://127.0.0.1:9999"
    );
    assert_eq!(url_from_override(None), DEFAULT_ANALYZE_URL);
    assert_eq!(url_from_override(Some("")), DEFAULT_ANALYZE_URL);
}

/// `serve` takes `--port`, not a URL, so an operator who moved the daemon must
/// get a daemon on the port they named — spawning on 7879 and then probing the
/// override would burn the whole budget and refuse a correct configuration.
#[test]
fn the_spawn_port_comes_from_the_configured_url() {
    use crate::audit::analyze::port_of;
    use crate::audit::DEFAULT_ANALYZE_PORT;

    assert_eq!(port_of("http://127.0.0.1:9312"), 9312);
    assert_eq!(port_of("http://localhost:9312/"), 9312);
    assert_eq!(port_of("https://localhost:9312"), 9312);
    // No port, and unreadable ports, fall back rather than failing the run.
    assert_eq!(port_of("http://localhost"), DEFAULT_ANALYZE_PORT);
    assert_eq!(port_of("http://localhost:not-a-port"), DEFAULT_ANALYZE_PORT);
}

/// A daemon that is already up is left alone.
///
/// The binary is a path that cannot exist, so any spawn attempt would fail the
/// run — passing is therefore proof the fast path returned without spawning
/// anything, which is what keeps `tga audit` from starting a second daemon
/// beside an operator's own.
#[tokio::test]
async fn a_reachable_analyze_daemon_is_not_restarted() {
    let port = serve_healthy().await;
    let guard = guard_on(port, "/nonexistent/trusty-analyze");
    crate::audit::ensure_analyze_daemon_with(&guard)
        .await
        .expect("a healthy daemon must satisfy the preflight without a spawn");
}

/// #5670, the spawn-failure arm: a binary that will not start is a refusal, not
/// a warning the sweep proceeds past.
///
/// This is the shape the defect had — `trusty-analyze` absent from the machine —
/// and before the fix nothing looked for it at all, so the audit ran to
/// completion and delivered a report with three empty sections.
#[tokio::test]
async fn an_unspawnable_analyze_binary_refuses_the_audit() {
    let guard = guard_on(free_port(), "/nonexistent/trusty-analyze");
    let err = crate::audit::ensure_analyze_daemon_with(&guard)
        .await
        .expect_err("a binary that cannot be spawned must stop the audit");

    let msg = err.to_string();
    for needle in [
        "trusty-analyze",
        "trusty-search",
        "/nonexistent/trusty-analyze",
    ] {
        assert!(msg.contains(needle), "missing {needle:?}: {msg}");
    }
}

/// The argument vector is the tga→trusty-analyze contract, asserted without
/// spawning anything — the same pure-function split `report_args` uses on the
/// renderer side.
#[test]
fn the_spawn_arguments_are_serve_on_the_configured_port() {
    use crate::audit::analyze::serve_args;

    assert_eq!(serve_args(9312), vec!["serve", "--port", "9312"]);
}

/// #5670, the readiness arm: a spawn that succeeds is not a daemon.
///
/// `trusty-analyze serve` exits 1 when trusty-search is unreachable, so the PID
/// a spawn returns proves nothing. This stub reproduces exactly that — it execs
/// and exits at once — and the refusal must still happen. The `cause` is what
/// separates this arm from the spawn-failure one above: reaching the readiness
/// timeout is only possible after `spawn_detached` returned a live PID.
#[cfg(unix)]
#[tokio::test]
async fn an_analyze_daemon_that_never_comes_up_refuses_the_audit() {
    let dir = tempfile::tempdir().expect("create a temp dir");
    let stub = stub_binary(dir.path(), "trusty-analyze-stub", "#!/bin/sh\nexit 1\n");

    let guard = guard_on(free_port(), &stub);
    let err = crate::audit::ensure_analyze_daemon_with(&guard)
        .await
        .expect_err("a daemon that exits at once must stop the audit");

    // The spawn itself succeeded — this is the readiness verdict, not a spawn
    // failure wearing the same error type.
    assert!(
        err.cause.contains("did not become ready"),
        "expected the readiness arm, got: {}",
        err.cause
    );

    // And the refusal names the ordering that actually fixes it.
    let msg = err.to_string();
    for needle in ["trusty-search start", "reads as a clean bill of health"] {
        assert!(msg.contains(needle), "missing {needle:?}: {msg}");
    }
}

/// #5670, the degraded arm: an analyze daemon that is up but whose trusty-search
/// is down does not satisfy the preflight either.
///
/// This is the arm that decides how far #5670 actually reaches. `trusty-analyze`
/// answers its own `/health` with 503 `degraded` whenever trusty-search is
/// unreachable, and `probe_once` counts only a 2xx — so the guard's probe
/// re-reads trusty-search's LIVE status on every `tga audit` run, not only when
/// it has to spawn. An operator whose analyze daemon has been up for days and
/// whose trusty-search died an hour ago is refused, with the same message.
///
/// The stub daemon here answers 503 forever, standing in for that daemon; the
/// spawned replacement exits at once, as the real binary does at its own search
/// check. The refusal must therefore come from the readiness poll — proof the
/// guard kept probing the live 503 rather than accepting the bound port.
#[cfg(unix)]
#[tokio::test]
async fn a_degraded_analyze_daemon_refuses_the_audit() {
    let dir = tempfile::tempdir().expect("create a temp dir");
    let stub = stub_binary(dir.path(), "trusty-analyze-stub", "#!/bin/sh\nexit 1\n");

    // usize::MAX degraded replies: it never recovers, exactly like an analyze
    // daemon sitting on top of a dead trusty-search.
    let port = serve_health(usize::MAX).await;
    let guard = guard_on(port, &stub);
    let err = crate::audit::ensure_analyze_daemon_with(&guard)
        .await
        .expect_err("a daemon answering 503 must stop the audit");

    assert!(
        err.cause.contains("did not become ready"),
        "expected the readiness arm against a live-but-degraded daemon, got: {}",
        err.cause
    );
    assert!(
        err.to_string().contains("trusty-search start"),
        "the refusal must name trusty-search: {err}"
    );
}

/// #5670, the concurrency arm: two guards racing the same slow daemon both get a
/// correct verdict, and neither observes state the other corrupted.
///
/// [`crate::audit::ensure_analyze_daemon_with`] holds no state between calls —
/// its guard is a shared `&AnalyzeGuard` it only reads — so what this pins is
/// the consequence: two overlapping calls each reach their own verdict from
/// their own probe, and the daemon that comes up mid-flight satisfies both.
///
/// It also pins the spawn count at **two**, which is the honest number. There is
/// no cross-call deduplication, in-process or otherwise, and adding one would
/// guard a path no caller reaches: `ensure_analyze_daemon` is called once per
/// `tga audit` process, and `trusty-audit run` audits its repositories
/// sequentially (`crates/trusty-audit/src/run.rs`'s `run_one` loop). A second
/// spawn is also self-limiting rather than damaging — `trusty-analyze serve`
/// takes an exclusive redb lock on the facts store and binds a fixed port, so
/// the loser of either race exits and the winner is the one daemon. Should a
/// concurrent caller ever appear, this assertion is what will fail and say so.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_concurrent_guards_both_resolve_against_one_slow_daemon() {
    let dir = tempfile::tempdir().expect("create a temp dir");
    let spawn_log = dir.path().join("spawns.log");
    let stub = stub_binary(
        dir.path(),
        "trusty-analyze-stub",
        &format!(
            "#!/bin/sh\necho \"$@\" >> {}\n",
            spawn_log.to_str().expect("a UTF-8 temp path")
        ),
    );

    // Degraded for exactly the two opening probes — one per guard — then ready.
    // Both calls therefore miss, spawn, and find the daemon on a later poll.
    let port = serve_health(2).await;
    let mut guard = guard_on(port, &stub);
    guard.startup_timeout = std::time::Duration::from_secs(5);

    let (first, second) = tokio::join!(
        crate::audit::ensure_analyze_daemon_with(&guard),
        crate::audit::ensure_analyze_daemon_with(&guard),
    );
    first.expect("the first concurrent guard must resolve");
    second.expect("the second concurrent guard must resolve");

    // The guard both calls borrowed is unchanged: it is read-only input, and a
    // reader that mutated it would have had to do so through a shared `&`.
    assert_eq!(guard.url, format!("http://127.0.0.1:{port}"));
    assert_eq!(guard.binary, stub);

    // The stubs are detached, so their writes land after the guards return.
    // Poll for the expected count rather than sleeping a guessed interval —
    // and keep polling past it, so a THIRD spawn would be seen rather than
    // raced past.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if std::fs::read_to_string(&spawn_log)
            .unwrap_or_default()
            .lines()
            .count()
            >= 2
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    // Then keep waiting past the expected count, so a THIRD spawn is seen
    // rather than raced past.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let spawns = std::fs::read_to_string(&spawn_log).unwrap_or_default();

    let spawned: Vec<&str> = spawns.lines().collect();
    assert_eq!(
        spawned.len(),
        2,
        "each call spawns for its own missed probe; got {spawned:?}"
    );
    for line in &spawned {
        assert_eq!(
            line.trim(),
            format!("serve --port {port}"),
            "every spawn carries the configured port"
        );
    }
}

// ─── #5670: indexing every audited repository before the render ──────────────

use crate::audit::repo_index::{binary_from_override as search_binary_from_override, index_args};
use crate::audit::{
    index_gap_lines, index_id_for, RepoIndexOutcome, RepoIndexStatus, DEFAULT_SEARCH_BIN,
};
use crate::report::dd_manifest::{
    build_dd_manifest, dd_repository_entries, DdManifestOptions, DdRepositoryEntry,
};

/// One manifest entry, as `dd_repository_entries` would produce it.
fn repo_entry(name: &str, path: &std::path::Path) -> DdRepositoryEntry {
    DdRepositoryEntry {
        name: name.to_string(),
        path: path.to_path_buf(),
    }
}

/// The same override-then-PATH rule the other two sibling binaries use. `""` is
/// not a setting: `trusty-audit` exports the pinned-tool variables
/// unconditionally, so a half-written export must fall back to PATH rather than
/// try to spawn an empty program name.
#[test]
fn search_binary_resolution_prefers_the_env_override() {
    assert_eq!(
        search_binary_from_override(Some("/opt/bin/trusty-search")),
        "/opt/bin/trusty-search"
    );
    assert_eq!(search_binary_from_override(Some("")), DEFAULT_SEARCH_BIN);
    assert_eq!(search_binary_from_override(None), DEFAULT_SEARCH_BIN);
    assert!(!crate::audit::resolve_search_binary().is_empty());
}

/// The cross-process contract, pinned as a rule.
///
/// trusty-review looks the index up by the checkout path's basename
/// (`report::analyze_adapter::derive_index_id`, a `path.file_name()` mapped to a
/// `String`). No Cargo edge joins the two crates, so this is a copy of that
/// rule — including the two shapes that are easy to get wrong: a trailing `.`,
/// which `Path` normalises away rather than treating as the final component, and
/// a root path, which has no basename at all and which BOTH sides must decline.
#[test]
fn the_index_id_is_the_checkout_basename() {
    use std::path::Path;

    assert_eq!(
        index_id_for(Path::new("/src/northwind-web")).as_deref(),
        Some("northwind-web")
    );
    assert_eq!(
        index_id_for(Path::new("/src/northwind-web/")).as_deref(),
        Some("northwind-web"),
        "a trailing separator is not a component"
    );
    assert_eq!(
        index_id_for(Path::new("/src/northwind-web/.")).as_deref(),
        Some("northwind-web"),
        "`base_dir.join(\".\")` is what a `path: .` config entry anchors to"
    );
    assert_eq!(index_id_for(Path::new("/")), None);
}

/// The ids indexed are derived from the paths the renderer reads.
///
/// This is the whole no-op risk of #5670: index every repository under an id
/// nobody queries and the reports stay exactly as hollow as before, with the
/// work done and nothing to show for it. The manifest is the only channel
/// between the two processes, so the entries handed to the indexer must be the
/// entries the manifest carries — asserted here against `build_dd_manifest`'s
/// own output rather than against a copy of the mapping.
#[test]
fn index_ids_match_the_manifest_paths_the_renderer_reads() {
    let cfg = Config {
        repositories: vec![
            crate::core::config::RepositoryConfig {
                path: std::path::PathBuf::from("/src/northwind-web"),
                name: Some("Northwind Web".to_string()),
                ..Default::default()
            },
            crate::core::config::RepositoryConfig {
                // Relative, so the base-dir anchoring is exercised too.
                path: std::path::PathBuf::from("checkouts/northwind-api"),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let base_dir = std::path::PathBuf::from("/work");

    let entries = dd_repository_entries(&cfg, &base_dir);
    let manifest = build_dd_manifest(
        &cfg,
        &DdManifestOptions {
            title: "T".to_string(),
            base_dir: base_dir.clone(),
            ..Default::default()
        },
    )
    .expect("builds");

    assert_eq!(
        entries, manifest.repositories,
        "the indexer must be handed the manifest's own entries"
    );
    let ids: Vec<Option<String>> = entries.iter().map(|e| index_id_for(&e.path)).collect();
    assert_eq!(
        ids,
        vec![
            Some("northwind-web".to_string()),
            Some("northwind-api".to_string())
        ],
        "each id is the basename of the path written into manifest.toml"
    );
}

/// The two argument vectors, asserted without spawning anything.
///
/// `--name` is the load-bearing flag: without it trusty-search names the index
/// after the directory the audit happens to be running from, while the renderer
/// looks up the checkout basename — so the run would index the right code under
/// the wrong id.
#[test]
fn the_index_invocation_names_the_path_and_the_id() {
    use crate::audit::repo_index::probe_args;
    use std::path::Path;

    let rendered = |args: Vec<std::ffi::OsString>| {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        rendered(index_args(Path::new("/src/acme-web"), "acme-web")),
        vec!["index", "/src/acme-web", "--name", "acme-web"]
    );
    assert_eq!(
        rendered(probe_args("acme-web")),
        vec!["index-status", "acme-web"]
    );
}

/// A `trusty-search` stub that logs every invocation and decides by argument.
///
/// `index-status <id>` exits 0 only for an id containing `served` — the real CLI
/// exits non-zero on the daemon's 404, which is the membership signal this
/// module reads. `index <path>` fails for a path containing `broken`.
#[cfg(unix)]
fn search_stub(dir: &std::path::Path, log: &std::path::Path) -> String {
    let script = format!(
        "#!/bin/sh\n\
         echo \"$@\" >> {log}\n\
         case \"$1\" in\n\
         index-status)\n\
         case \"$2\" in *served*) exit 0 ;; *) exit 1 ;; esac ;;\n\
         index)\n\
         case \"$2\" in\n\
         *broken*) echo 'progress: walking' >&2; echo 'error: not a directory' >&2; exit 3 ;;\n\
         *) exit 0 ;;\n\
         esac ;;\n\
         esac\n\
         exit 9\n",
        log = log.display()
    );
    stub_binary(dir, "trusty-search-stub", &script)
}

/// Every line the stub was invoked with, in order.
#[cfg(unix)]
fn stub_log(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// The defect itself: nothing indexed the repository, so the renderer's
/// membership check missed and three report sections came back empty over an
/// exit 0. The audit now probes, misses, and indexes — under the id the renderer
/// will look up.
#[cfg(unix)]
#[tokio::test]
async fn an_unindexed_repository_is_indexed_before_the_render() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("invocations");
    let stub = search_stub(dir.path(), &log);
    let entries = vec![repo_entry("Acme Web", &dir.path().join("acme-web"))];

    let outcomes = crate::audit::repo_index::ensure_repositories_indexed_with(stub, &entries).await;

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, RepoIndexStatus::Indexed);
    assert_eq!(outcomes[0].index_id.as_deref(), Some("acme-web"));
    let calls = stub_log(&log);
    assert_eq!(calls.len(), 2, "one probe, then one index: {calls:?}");
    assert_eq!(calls[0], "index-status acme-web");
    assert!(
        calls[1].starts_with("index ") && calls[1].ends_with("--name acme-web"),
        "the index is built under the id the renderer looks up: {calls:?}"
    );
    assert!(
        index_gap_lines(&outcomes, &[] as &[String]).is_empty(),
        "an indexed repository is not a gap"
    );
}

/// A repository trusty-search already serves must not be re-indexed: an audit
/// over an org that is already indexed would otherwise spend its one shot
/// re-embedding unchanged code. The probe answers, and nothing else is spawned.
#[cfg(unix)]
#[tokio::test]
async fn an_already_indexed_repository_is_not_reindexed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("invocations");
    let stub = search_stub(dir.path(), &log);
    // The stub answers the probe for any id containing `served`.
    let entries = vec![repo_entry("Acme Web", &dir.path().join("acme-served"))];

    let outcomes = crate::audit::repo_index::ensure_repositories_indexed_with(stub, &entries).await;

    assert_eq!(outcomes[0].status, RepoIndexStatus::AlreadyServed);
    let calls = stub_log(&log);
    assert_eq!(
        calls,
        vec!["index-status acme-served".to_string()],
        "the probe is the only invocation — no reindex: {calls:?}"
    );
}

/// DOC-67 §9's per-repository rule, in both directions.
///
/// One repository that will not index must not cost the other two their audit:
/// the run continues past it (the third repository is still indexed, after the
/// failure), the failure is NAMED in the Gaps & Caveats lines, and the pass
/// returns a value rather than an error — nothing here can change the exit
/// status. The two assertions fail for opposite regressions: making the failure
/// abort the run drops the third repository's invocation, and swallowing it
/// drops the gap line.
#[cfg(unix)]
#[tokio::test]
async fn one_repository_that_fails_to_index_does_not_stop_the_others() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("invocations");
    let stub = search_stub(dir.path(), &log);
    let entries = vec![
        repo_entry("Acme Web", &dir.path().join("acme-web")),
        repo_entry("Acme Broken", &dir.path().join("acme-broken")),
        repo_entry("Acme API", &dir.path().join("acme-api")),
    ];

    let outcomes = crate::audit::repo_index::ensure_repositories_indexed_with(stub, &entries).await;

    assert_eq!(outcomes[0].status, RepoIndexStatus::Indexed);
    assert!(outcomes[1].failed(), "{:?}", outcomes[1]);
    assert_eq!(
        outcomes[2].status,
        RepoIndexStatus::Indexed,
        "the run must continue past the failure"
    );
    let calls = stub_log(&log);
    assert!(
        calls
            .iter()
            .any(|c| c.contains("acme-api") && c.starts_with("index ")),
        "the repository after the failure is still indexed: {calls:?}"
    );

    let gaps = index_gap_lines(&outcomes, &[] as &[String]);
    assert_eq!(gaps.len(), 1, "one line per distinct reason: {gaps:?}");
    assert!(
        gaps[0].contains("Acme Broken") && gaps[0].contains("not a directory"),
        "the failure is named, with its cause: {}",
        gaps[0]
    );
    assert!(
        !gaps[0].contains("Acme Web") && !gaps[0].contains("Acme API"),
        "a repository that indexed is not a gap: {}",
        gaps[0]
    );
    assert!(
        gaps[0].contains("not assessed, not clean"),
        "an empty section must not read as a clean pass: {}",
        gaps[0]
    );
}

/// `trusty-search` is resolved from PATH, not from a Cargo edge, so its absence
/// is ordinary rather than exotic. It must not panic, must not abort the audit,
/// and must name both ways to supply the binary — an engagement that pins its
/// tools uses the override.
#[tokio::test]
async fn a_missing_search_binary_is_named_and_the_run_continues() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("definitely-not-installed");
    let entries = vec![
        repo_entry("Acme Web", &dir.path().join("acme-web")),
        repo_entry("Acme API", &dir.path().join("acme-api")),
    ];

    let outcomes = crate::audit::repo_index::ensure_repositories_indexed_with(
        missing.display().to_string(),
        &entries,
    )
    .await;

    assert_eq!(outcomes.len(), 2, "every repository is still reported on");
    assert!(outcomes.iter().all(RepoIndexOutcome::failed));

    let gaps = index_gap_lines(&outcomes, &[] as &[String]);
    assert_eq!(
        gaps.len(),
        1,
        "one fault affecting every repository is one line, not N: {gaps:?}"
    );
    assert!(
        gaps[0].contains("Acme Web") && gaps[0].contains("Acme API"),
        "every unassessed application is named: {}",
        gaps[0]
    );
    assert!(
        gaps[0].contains("TRUSTY_SEARCH_BIN") && gaps[0].contains("cargo install trusty-search"),
        "the remedy names both ways to supply the binary: {}",
        gaps[0]
    );
}

/// A run where every repository is served adds no line — a clean run must not
/// dilute the Gaps section with a report of its own success.
#[test]
fn index_gap_lines_are_empty_when_every_repository_is_served() {
    let outcomes = vec![
        RepoIndexOutcome {
            repo: "Acme Web".to_string(),
            index_id: Some("acme-web".to_string()),
            status: RepoIndexStatus::AlreadyServed,
        },
        RepoIndexOutcome {
            repo: "Acme API".to_string(),
            index_id: Some("acme-api".to_string()),
            status: RepoIndexStatus::Indexed,
        },
    ];
    assert!(index_gap_lines(&outcomes, &[] as &[String]).is_empty());
}

/// The failure reason is a child's message, so it can quote a credential back at
/// us — a token in an HTTPS remote URL, or in a path. It is scrubbed before it
/// is excerpted, for the reason `gaps.rs` documents: cutting first leaves a
/// fragment no later scrub can match, `build_dd_manifest`'s included.
#[test]
fn a_credential_in_an_index_failure_never_reaches_the_gap_line() {
    let token = "ghp_averyrealisticlookingtoken0123456789";
    let outcomes = vec![RepoIndexOutcome {
        repo: "Acme Web".to_string(),
        index_id: Some("acme-web".to_string()),
        status: RepoIndexStatus::Failed(format!(
            "failed to clone https://{token}@github.com/acme/web.git"
        )),
    }];

    let gaps = index_gap_lines(&outcomes, &[token.to_string()]);
    assert_eq!(gaps.len(), 1);
    assert!(!gaps[0].contains(token), "{}", gaps[0]);
    assert!(
        gaps[0].contains("Acme Web"),
        "redaction must not cost the reader the repository name: {}",
        gaps[0]
    );
}
