//! Per-stage identity and outcome records for the AUDIT sweep.
//!
//! Why: DOC-67 §9 forbids the sweep from aborting on a single stage failure,
//! and forbids a failed stage from reading as a clean pass. Both obligations
//! need the same thing: a value the caller can inspect afterwards that says
//! which stages ran, which failed, and why. These types are that value.
//! What: [`SweepStage`] (the fixed eight-stage vocabulary), [`StageStatus`],
//! [`StageOutcome`], and [`AuditSweepStats`] (the ordered record).
//! Test: `super::tests` covers ordering, failure capture, and the summary
//! rendering, including a stage that fails.

use std::fmt;
use std::time::{Duration, Instant};

/// One stage of the AUDIT sweep.
///
/// Why: PR B (#5239) names failed stages in the report's Gaps & Caveats
/// section, so a stage needs a stable identity that survives into the
/// orchestrator's output rather than being a bare string built at the call
/// site.
/// What: the eight data-collection subcommands DOC-67 §4 enumerates, in the
/// order [`super::run_full_sweep`] executes them.
/// Test: `super::tests::sweep_runs_every_stage_in_order_and_survives_failures`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SweepStage {
    /// `tga collect` — walk the configured repositories into `commits`.
    Collect,
    /// `tga classify` — run the four-tier classification cascade.
    Classify,
    /// `tga jira sync` — ingest JIRA transitions and comments.
    JiraSync,
    /// `tga deployments collect` — ingest deploy events into `fact_deployments`.
    Deployments,
    /// `tga incidents collect` — ingest incidents into `fact_incidents`.
    Incidents,
    /// `tga dora` — reduce the deployment/incident facts to the four DORA keys.
    Dora,
    /// `tga pr-metrics` — aggregate pull-request metrics per engineer.
    PrMetrics,
    /// `tga report` — render the CSV / JSON / Markdown reports.
    Report,
}

impl SweepStage {
    /// The stage's CLI-facing name, e.g. `"jira sync"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Collect => "collect",
            Self::Classify => "classify",
            Self::JiraSync => "jira sync",
            Self::Deployments => "deployments collect",
            Self::Incidents => "incidents collect",
            Self::Dora => "dora",
            Self::PrMetrics => "pr-metrics",
            Self::Report => "report",
        }
    }
}

impl fmt::Display for SweepStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How one stage ended.
///
/// Why: "did not run" and "ran and failed" are different facts to an acquirer
/// reading the gap list, so they are different variants rather than one
/// `Option<String>`.
/// What: `Succeeded`, or `Failed` carrying the error rendered with its full
/// `anyhow` cause chain.
/// Test: `super::tests::failed_stage_is_recorded_and_does_not_stop_the_sweep`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StageStatus {
    /// The stage's `run` function returned `Ok`.
    Succeeded,
    /// The stage's `run` function returned `Err`; the message is preserved.
    Failed(String),
}

impl StageStatus {
    /// Whether this status is [`StageStatus::Failed`].
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

/// The record of one executed stage.
///
/// Why/What/Test: see the module doc; this is a plain data carrier.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StageOutcome {
    /// Which stage this record describes.
    pub stage: SweepStage,
    /// How it ended.
    pub status: StageStatus,
    /// Wall-clock time the stage took.
    pub elapsed: Duration,
}

/// The ordered outcome of a full AUDIT sweep.
///
/// Why: DOC-67 §9 requires that a repo or dimension missing because a stage
/// failed is *named*, never silently absent — so the sweep's return value has
/// to carry every stage's fate, not a bool. Shaping it now means #5239's gap
/// reporting consumes this struct instead of reshaping the entry point.
/// What: the outcomes in execution order, plus the queries a caller needs —
/// [`AuditSweepStats::failures`], [`AuditSweepStats::any_failed`], and a
/// one-line [`AuditSweepStats::summary`].
/// Test: `super::tests::summary_counts_successes_and_failures`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AuditSweepStats {
    /// Every stage that was attempted, in execution order.
    pub outcomes: Vec<StageOutcome>,
}

impl AuditSweepStats {
    /// Time `result` as `stage`'s outcome and append it.
    ///
    /// Why: this is the single place a stage result becomes a record, which is
    /// what keeps continue-on-failure structural — there is no `?` on a stage
    /// result anywhere in [`super::run_full_sweep`], so no stage can abort the
    /// sweep by construction rather than by discipline (DOC-67 §9).
    /// What: converts `Err(e)` into [`StageStatus::Failed`] with the full
    /// `{e:#}` cause chain, logs it at `warn`, and pushes the record.
    /// Test: `super::tests::failed_stage_is_recorded_and_does_not_stop_the_sweep`.
    pub fn record(&mut self, stage: SweepStage, started: Instant, result: anyhow::Result<()>) {
        let status = match result {
            Ok(()) => StageStatus::Succeeded,
            Err(e) => {
                let message = format!("{e:#}");
                tracing::warn!(stage = stage.as_str(), error = %message, "audit stage failed");
                StageStatus::Failed(message)
            }
        };
        self.outcomes.push(StageOutcome {
            stage,
            status,
            elapsed: started.elapsed(),
        });
    }

    /// Every stage that failed, in execution order.
    pub fn failures(&self) -> impl Iterator<Item = &StageOutcome> {
        self.outcomes.iter().filter(|o| o.status.is_failure())
    }

    /// Whether any stage failed.
    pub fn any_failed(&self) -> bool {
        self.outcomes.iter().any(|o| o.status.is_failure())
    }

    /// One line describing the run, e.g. `"7 of 8 stage(s) succeeded"`.
    pub fn summary(&self) -> String {
        let total = self.outcomes.len();
        let ok = total - self.failures().count();
        format!("{ok} of {total} stage(s) succeeded")
    }
}
