//! The AUDIT full-dataset sweep — one call, eight stages, no TTY.
//!
//! Why: DOC-67 §7 (resolved Q1/Q6) requires that `tga audit` and the TUI's
//! "Run Audit" button drive the SAME sweep, and that neither re-sequences the
//! subcommands itself. A library function is the only shape that serves
//! both: `tga audit` cannot depend on ratatui or a terminal (§2, one-shot,
//! non-interactive), and the TUI cannot depend on clap having parsed anything.
//! What: [`SweepOptions`] and [`run_full_sweep`], which call the existing
//! `crate::commands::*::run` functions in dependency order — plus the
//! correlation pass (#5405) — and record each one's outcome instead of
//! propagating it.
//! Test: `super::tests`.

use std::path::PathBuf;
use std::time::Instant;

use crate::collect::collector::{FetchOutcome, PerRepoFetch};
use crate::collect::correlate::correlate_commits;
use crate::commands::args::{ClassifyArgs, CollectArgs, ReportArgs};
use crate::commands::deployments::DeploymentsCollectArgs;
use crate::commands::incidents::IncidentsCollectArgs;
use crate::commands::jira::JiraSyncArgs;
use crate::commands::{classify, collect, deployments, dora, incidents, jira, pr_metrics, report};
use crate::commands::{dora::DoraArgs, pr_metrics::PrMetricsArgs};
use crate::core::config::Config;
use crate::core::db::Database;
use crate::core::progress::{ProgressBus, ProgressEvent, Stage};

use super::stage::{AuditSweepStats, StaleFetch, SweepStage};

/// How many stages [`run_full_sweep`] drives.
///
/// Why: the per-stage start event says "stage 3 of 9", and that denominator has
/// to come from one place or it drifts the next time a stage is added.
/// What: `9` — the eight subcommands plus the correlation pass (#5405).
/// Test: `super::tests::sweep_runs_every_stage_in_order_and_survives_failures`
/// asserts the sweep records exactly this many outcomes.
pub(crate) const TOTAL_STAGES: usize = 9;

/// The knobs `tga audit` (or a TUI action) hands the sweep.
///
/// Why: everything else the stages need already lives in `Config`; these two
/// are the run-scoped values an audit operator supplies per invocation, and
/// they are deliberately the only ones — DOC-67 §2 forbids a mid-run choice,
/// and §9 fixes the stale-refs policy rather than exposing it as an option.
/// What: the directory reports are written to, and an optional lookback
/// window in ISO weeks applied to collection and PR metrics.
/// Test: `super::tests::sweep_writes_reports_into_the_requested_directory`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SweepOptions {
    /// Directory for the stage-3 report output. `None` uses the config default.
    pub output: Option<PathBuf>,
    /// Limit collection and PR metrics to the last N ISO weeks.
    pub weeks: Option<u32>,
}

/// Run tga's full data-collection pipeline once, start to finish.
///
/// Why: an acquirer's audit is one action over a whole org, and DOC-67 §2
/// binds it to a single non-interactive shot — so there is no operator present
/// to see an abort and re-run with a flag. §9 therefore makes a stage failure
/// a *recorded* fact rather than a terminating one: a transient fetch failure
/// on one repo among two hundred must not waste the shot for the other 199.
/// This function is also #5217's callable surface, so the TUI's "Run Audit"
/// button and `tga audit` execute byte-identical sequencing instead of two
/// drifting copies.
///
/// What: runs collect → correlate → classify → jira sync → deployments →
/// incidents → dora → pr-metrics → report by calling each subcommand's own
/// `run`, recording every outcome into an [`AuditSweepStats`]. No stage result
/// is propagated with `?`, so no stage can abort the run. `--allow-stale` is
/// applied to collection as a fixed default (§9) — not an operator choice.
///
/// `progress`, when supplied, receives a [`Stage::Audit`] start event before
/// each of the nine stages and a completed/failed event after it, with
/// [`SweepStage::as_str`] as the target — so a ten-minute sweep is observable
/// even though seven of the eight subcommands have no instrumentation of their
/// own (#5361). The collection stage additionally gets the SAME bus handed to
/// its pipeline, so its per-repository [`Stage::Collect`] events land there
/// too. `None` emits nothing at all.
///
/// A failed STAGE is reported inside `AuditSweepStats`, never as `Err` —
/// `Err` means the run could not be started at all. Callers must therefore check
/// [`AuditSweepStats::any_failed`] rather than treating `Ok` as a clean pass —
/// #5239 turns those records into the report's Gaps & Caveats lines.
///
/// # Errors
///
/// Returns `Err` only if the pre-flight fails — today, if `options.output` is
/// set and cannot be created. That is a whole-run precondition, not a stage
/// outcome: without a writable directory every artifact-producing stage would
/// fail identically, which is noise rather than a gap list.
///
/// Test: `super::tests::sweep_runs_every_stage_in_order_and_survives_failures`,
/// `super::tests::failed_stage_is_recorded_and_does_not_stop_the_sweep`,
/// `super::tests::sweep_emits_progress_for_non_collection_stages`.
///
/// # Spec References
/// - [`SPEC-TGAUDIT-02~draft`](../../../../docs/specs/DOC-67-tga-audit-mode.md#SPEC-TGAUDIT-02~draft)
/// - [`SPEC-TGAUDIT-07~draft`](../../../../docs/specs/DOC-67-tga-audit-mode.md#SPEC-TGAUDIT-07~draft)
/// - [`SPEC-TGAUDIT-09~draft`](../../../../docs/specs/DOC-67-tga-audit-mode.md#SPEC-TGAUDIT-09~draft)
pub async fn run_full_sweep(
    config: &Config,
    db: &mut Database,
    options: &SweepOptions,
    progress: Option<&ProgressBus>,
) -> anyhow::Result<AuditSweepStats> {
    // Created once, up front, so the stages that write into it do not each
    // race to create it — and so an unwritable path fails as a precondition
    // rather than as eight identical stage failures.
    if let Some(dir) = options.output.as_ref() {
        std::fs::create_dir_all(dir)?;
    }

    let mut stats = AuditSweepStats::default();
    // #5361: the collection pipeline wants an owned bus and an absent one is
    // the disabled bus, on which every emit is a no-op.
    let collect_bus = progress.cloned().unwrap_or_default();

    let t = begin(progress, &stats, SweepStage::Collect);
    let args = CollectArgs {
        weeks: options.weeks,
        // #5217: DOC-67 §9 — a one-shot org sweep cannot inherit `tga
        // collect`'s abort-on-fetch-failure default; a stale repo is a named
        // gap, not a reason to halt the other 199.
        allow_stale: true,
        ..CollectArgs::default()
    };
    let result = collect::run_reporting_fetch(config.clone(), db, args, &collect_bus).await;
    // #5321: do this BEFORE `finish` — the stage is about to be recorded as
    // succeeded, and a succeeded stage produces no gap line of its own.
    let result = record_stale_fetches(&mut stats, result);
    finish(progress, &mut stats, SweepStage::Collect, t, result);

    // #5405: the commit ↔ board-item join, which until now ran only from `tga
    // tui`. It sits immediately after collection because every production
    // writer of `work_items` runs inside that stage (ADO and Linear), so this
    // is the earliest point at which the join sees a complete board.
    let t = begin(progress, &stats, SweepStage::Correlate);
    let result = correlate_commits(db.connection_mut(), &collect_bus)
        .map(|outcome| tracing::info!(summary = %outcome.summary(), "correlation pass finished"))
        .map_err(anyhow::Error::from);
    finish(progress, &mut stats, SweepStage::Correlate, t, result);

    let t = begin(progress, &stats, SweepStage::Classify);
    let args = ClassifyArgs {
        weeks: options.weeks,
        ..ClassifyArgs::default()
    };
    let result = classify::run(config.clone(), db, args).await;
    finish(progress, &mut stats, SweepStage::Classify, t, result);

    let t = begin(progress, &stats, SweepStage::JiraSync);
    let result = jira::run_sync(config.clone(), db, JiraSyncArgs::default()).await;
    finish(progress, &mut stats, SweepStage::JiraSync, t, result);

    // Deployments and incidents populate `fact_deployments` / `fact_incidents`,
    // which `dora` reduces — so they precede it here even though DOC-67 §5's
    // prose lists dora first. See the module-level note in `super`.
    let t = begin(progress, &stats, SweepStage::Deployments);
    let result = deployments::run(config.clone(), db, DeploymentsCollectArgs::default()).await;
    finish(progress, &mut stats, SweepStage::Deployments, t, result);

    let t = begin(progress, &stats, SweepStage::Incidents);
    let result = incidents::run(config.clone(), db, IncidentsCollectArgs::default());
    finish(progress, &mut stats, SweepStage::Incidents, t, result);

    let t = begin(progress, &stats, SweepStage::Dora);
    let result = dora::run(config.clone(), db, DoraArgs::default());
    finish(progress, &mut stats, SweepStage::Dora, t, result);

    let t = begin(progress, &stats, SweepStage::PrMetrics);
    // `pr-metrics --output` names a FILE, not a directory (unlike `report
    // --output`), so it gets a path inside the run directory rather than the
    // directory itself — passing the directory makes it write a regular file
    // there and the report stage then cannot create the directory.
    let args = PrMetricsArgs {
        weeks: options.weeks,
        csv: true,
        output: options.output.as_ref().map(|d| d.join("pr-metrics.csv")),
    };
    let result = pr_metrics::run(config.clone(), db, args);
    finish(progress, &mut stats, SweepStage::PrMetrics, t, result);

    let t = begin(progress, &stats, SweepStage::Report);
    let args = ReportArgs {
        output: options.output.clone(),
        ..ReportArgs::default()
    };
    let result = report::run(config.clone(), db, args);
    finish(progress, &mut stats, SweepStage::Report, t, result);

    tracing::info!(summary = %stats.summary(), "audit sweep finished");
    Ok(stats)
}

/// Keep every unreachable remote from collection, and hand back the bare result.
///
/// Why: `--allow-stale` is fixed on for the sweep (DOC-67 §9), so a repository
/// whose remote is unreachable is walked on whatever its local clone already
/// held and collection still returns `Ok`. That is the right behaviour for a
/// one-shot org sweep and the wrong thing to keep to ourselves: without this
/// step the only trace is a stderr line, and the report reads as if every
/// repository were current (#5321).
/// What: records one [`StaleFetch`] per [`FetchOutcome::Failed`] outcome, then
/// discards the outcome list so the caller has the `Result<()>` that
/// [`finish`] takes. An `Err` passes straight through with nothing recorded —
/// a stage that failed already reaches the report through its own gap line.
/// Test: `super::tests::a_repo_that_fell_back_to_stale_local_refs_is_named_in_the_gap_lines`.
fn record_stale_fetches(
    stats: &mut AuditSweepStats,
    result: anyhow::Result<Vec<PerRepoFetch>>,
) -> anyhow::Result<()> {
    for fetch in result? {
        if let FetchOutcome::Failed { remote, error } = fetch.outcome {
            stats.record_stale_fetch(StaleFetch {
                repo: fetch.repo,
                remote,
                error,
            });
        }
    }
    Ok(())
}

/// Announce that `stage` is starting, and return the instant to time it from.
///
/// Why: a stage with no instrumentation of its own is indistinguishable from a
/// hang until it ends, which for `collect` against a large org is minutes of
/// frozen screen (#5302). The start event is what puts the row on screen.
/// What: emits [`ProgressEvent::started`] on `progress` under
/// [`Stage::Audit`], targeted at the stage name and detailed with its position
/// in the run, then returns `Instant::now()`. A `None` bus emits nothing; the
/// returned instant is unaffected either way.
/// Test: `super::tests::sweep_emits_progress_for_non_collection_stages`.
fn begin(progress: Option<&ProgressBus>, stats: &AuditSweepStats, stage: SweepStage) -> Instant {
    if let Some(bus) = progress {
        let position = stats.outcomes.len() + 1;
        bus.emit(
            ProgressEvent::started(Stage::Audit, stage.as_str(), Some(1))
                .with_detail(format!("stage {position} of {TOTAL_STAGES}")),
        );
    }
    Instant::now()
}

/// Publish `stage`'s verdict, then record it in `stats`.
///
/// Why: the bus and the stats must never disagree about how a stage ended, so
/// one function owns both writes — a caller cannot record an outcome without
/// announcing it, or announce one it did not record.
/// What: emits [`ProgressEvent::completed`] or [`ProgressEvent::failed`]
/// (carrying the full `{e:#}` cause chain, the same text
/// [`AuditSweepStats::record`] stores), then hands `result` to
/// [`AuditSweepStats::record`]. Never propagates: a stage failure is a recorded
/// fact, per DOC-67 §9.
/// Test: `super::tests::sweep_emits_progress_for_non_collection_stages`.
fn finish(
    progress: Option<&ProgressBus>,
    stats: &mut AuditSweepStats,
    stage: SweepStage,
    started: Instant,
    result: anyhow::Result<()>,
) {
    if let Some(bus) = progress {
        bus.emit(match &result {
            Ok(()) => ProgressEvent::completed(Stage::Audit, stage.as_str(), 1),
            Err(e) => ProgressEvent::failed(Stage::Audit, stage.as_str(), format!("{e:#}")),
        });
    }
    stats.record(stage, started, result);
}
