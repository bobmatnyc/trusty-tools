//! The AUDIT full-dataset sweep — one call, eight stages, no TTY.
//!
//! Why: DOC-67 §7 (resolved Q1/Q6) requires that `tga audit` and the TUI's
//! "Run Audit" button drive the SAME sweep, and that neither re-sequences the
//! eight subcommands itself. A library function is the only shape that serves
//! both: `tga audit` cannot depend on ratatui or a terminal (§2, one-shot,
//! non-interactive), and the TUI cannot depend on clap having parsed anything.
//! What: [`SweepOptions`] and [`run_full_sweep`], which call the existing
//! `crate::commands::*::run` functions in dependency order and record each
//! one's outcome instead of propagating it.
//! Test: `super::tests`.

use std::path::PathBuf;
use std::time::Instant;

use crate::commands::args::{ClassifyArgs, CollectArgs, ReportArgs};
use crate::commands::deployments::DeploymentsCollectArgs;
use crate::commands::incidents::IncidentsCollectArgs;
use crate::commands::jira::JiraSyncArgs;
use crate::commands::{classify, collect, deployments, dora, incidents, jira, pr_metrics, report};
use crate::commands::{dora::DoraArgs, pr_metrics::PrMetricsArgs};
use crate::core::config::Config;
use crate::core::db::Database;
use crate::core::progress::ProgressBus;

use super::stage::{AuditSweepStats, SweepStage};

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
/// What: runs collect → classify → jira sync → deployments → incidents → dora
/// → pr-metrics → report by calling each subcommand's own `run`, recording
/// every outcome into an [`AuditSweepStats`]. No stage result is propagated
/// with `?`, so no stage can abort the run. `--allow-stale` is applied to
/// collection as a fixed default (§9) — not an operator choice. `progress`,
/// when supplied, is the existing bus the collection pipeline emits into; the
/// remaining stages have no progress instrumentation of their own today and
/// are silent on it.
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
/// `super::tests::failed_stage_is_recorded_and_does_not_stop_the_sweep`.
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
    // The bus is accepted but not yet threaded past collection: only
    // `CollectionPipeline` emits events today, and it reads the bus off the
    // config-carrying pipeline rather than off the command wrapper. Kept in the
    // signature per DOC-67 §7 so the TUI caller does not need a second one.
    let _ = progress;

    // Created once, up front, so the stages that write into it do not each
    // race to create it — and so an unwritable path fails as a precondition
    // rather than as eight identical stage failures.
    if let Some(dir) = options.output.as_ref() {
        std::fs::create_dir_all(dir)?;
    }

    let mut stats = AuditSweepStats::default();

    let t = Instant::now();
    let args = CollectArgs {
        weeks: options.weeks,
        // #5217: DOC-67 §9 — a one-shot org sweep cannot inherit `tga
        // collect`'s abort-on-fetch-failure default; a stale repo is a named
        // gap, not a reason to halt the other 199.
        allow_stale: true,
        ..CollectArgs::default()
    };
    stats.record(
        SweepStage::Collect,
        t,
        collect::run(config.clone(), db, args).await,
    );

    let t = Instant::now();
    let args = ClassifyArgs {
        weeks: options.weeks,
        ..ClassifyArgs::default()
    };
    stats.record(
        SweepStage::Classify,
        t,
        classify::run(config.clone(), db, args).await,
    );

    let t = Instant::now();
    stats.record(
        SweepStage::JiraSync,
        t,
        jira::run_sync(config.clone(), db, JiraSyncArgs::default()).await,
    );

    // Deployments and incidents populate `fact_deployments` / `fact_incidents`,
    // which `dora` reduces — so they precede it here even though DOC-67 §5's
    // prose lists dora first. See the module-level note in `super`.
    let t = Instant::now();
    stats.record(
        SweepStage::Deployments,
        t,
        deployments::run(config.clone(), db, DeploymentsCollectArgs::default()).await,
    );

    let t = Instant::now();
    stats.record(
        SweepStage::Incidents,
        t,
        incidents::run(config.clone(), db, IncidentsCollectArgs::default()),
    );

    let t = Instant::now();
    stats.record(
        SweepStage::Dora,
        t,
        dora::run(config.clone(), db, DoraArgs::default()),
    );

    let t = Instant::now();
    // `pr-metrics --output` names a FILE, not a directory (unlike `report
    // --output`), so it gets a path inside the run directory rather than the
    // directory itself — passing the directory makes it write a regular file
    // there and the report stage then cannot create the directory.
    let args = PrMetricsArgs {
        weeks: options.weeks,
        csv: true,
        output: options.output.as_ref().map(|d| d.join("pr-metrics.csv")),
    };
    stats.record(
        SweepStage::PrMetrics,
        t,
        pr_metrics::run(config.clone(), db, args),
    );

    let t = Instant::now();
    let args = ReportArgs {
        output: options.output.clone(),
        ..ReportArgs::default()
    };
    stats.record(SweepStage::Report, t, report::run(config.clone(), db, args));

    tracing::info!(summary = %stats.summary(), "audit sweep finished");
    Ok(stats)
}
