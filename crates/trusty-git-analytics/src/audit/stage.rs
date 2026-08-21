//! Per-stage identity and outcome records for the AUDIT sweep.
//!
//! Why: DOC-67 §9 forbids the sweep from aborting on a single stage failure,
//! and forbids a failed stage from reading as a clean pass. Both obligations
//! need the same thing: a value the caller can inspect afterwards that says
//! which stages ran, which failed, and why. These types are that value.
//! What: [`SweepStage`] (the fixed nine-stage vocabulary), [`StageStatus`],
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
/// What: the eight data-collection subcommands DOC-67 §4 enumerates plus the
/// correlation pass (#5405), in the order [`super::run_full_sweep`] executes
/// them.
/// Test: `super::tests::sweep_runs_every_stage_in_order_and_survives_failures`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SweepStage {
    /// `tga collect` — walk the configured repositories into `commits`.
    Collect,
    /// The deterministic commit ↔ board-item join (#5405).
    ///
    /// Runs immediately after [`Self::Collect`] because every production writer
    /// of `work_items` runs inside that stage — Azure DevOps
    /// (`collect::collector`) and Linear (`collect::linear_pipeline`). `jira
    /// sync` writes `fact_ticket_transitions`, not `work_items` (#5219), so it
    /// does not constrain this position.
    Correlate,
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
            Self::Correlate => "correlate",
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

/// A repository the sweep walked on stale local refs because its remote could
/// not be fetched (#5321).
///
/// Why: this is the degradation a [`StageStatus`] cannot express. The sweep
/// hard-codes `--allow-stale` (DOC-67 §9), so an unreachable remote does not
/// fail the `collect` stage — it silently narrows what the stage collected. A
/// separate record is what lets the report distinguish "no stale repositories"
/// from "this repository's figures describe a local clone of unknown age".
/// What: the repository's display name, the remote that could not be reached,
/// and the fetch error verbatim. The error is redacted and excerpted later, by
/// [`super::sweep_gap_lines`], for the same reason a stage message is.
/// Test: `super::tests::a_repo_that_fell_back_to_stale_local_refs_is_named_in_the_gap_lines`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StaleFetch {
    /// Display name of the repository, as the collection pipeline reported it.
    pub repo: String,
    /// The remote that could not be fetched (usually `origin`).
    pub remote: String,
    /// The fetch error, unredacted and untruncated.
    pub error: String,
}

/// A collection leg the config declared absent before the sweep started
/// (#6130).
///
/// Why: the third thing a [`StageStatus`] cannot express, beside
/// [`StaleFetch`]. A leg that was never attempted leaves the stage
/// `Succeeded` and its tables empty, which on the page is indistinguishable
/// from a leg that ran and found nothing. Keeping the declaration as its own
/// record is what holds the line #5620 draws: a RECORDED skip proceeds and is
/// named, a BLIND one is a defect, and a leg that actually ran and failed
/// still fails its stage closed.
/// What: which leg, and the reason its declarer gave. The reason is redacted
/// and excerpted later by [`super::sweep_gap_lines`] — it is text this process
/// did not author, so it can carry a path or a credential.
/// Test: `super::tests::a_declared_absent_leg_is_named_in_the_gap_lines`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeclaredSkip {
    /// The leg that was not attempted, e.g. `"GitHub work items"`.
    pub leg: String,
    /// Why, as the config declared it — unredacted and untruncated.
    pub reason: String,
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
    /// Repositories collected from stale local refs because their remote could
    /// not be fetched, in collection order (#5321). Empty when every configured
    /// remote was reached — a clean run adds no entry.
    pub stale_fetches: Vec<StaleFetch>,
    /// Collection legs the config declared absent before the sweep ran, in
    /// declaration order (#6130). Empty when every configured leg was
    /// attempted.
    pub declared_skips: Vec<DeclaredSkip>,
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

    /// Record that `fetch`'s repository was walked on stale local refs.
    ///
    /// Why: the stage this happened inside reports `Succeeded`, so without an
    /// explicit record the fact has nowhere to go — #5321 is precisely that it
    /// went nowhere. Logging it here as well as storing it keeps the terminal
    /// and the report saying the same thing.
    /// What: warns, then appends. Order of arrival is collection order.
    /// Test: `super::tests::a_repo_that_fell_back_to_stale_local_refs_is_named_in_the_gap_lines`.
    pub fn record_stale_fetch(&mut self, fetch: StaleFetch) {
        tracing::warn!(
            repo = %fetch.repo,
            remote = %fetch.remote,
            error = %fetch.error,
            "collected from stale local refs; data may be behind the remote"
        );
        self.stale_fetches.push(fetch);
    }

    /// Record that `skip`'s leg was declared absent and never attempted.
    ///
    /// Why: #6130 — the declaration is made in the config, before any stage
    /// runs, so nothing downstream would otherwise know the difference between
    /// a leg that was skipped on purpose and one that ran and found nothing.
    /// What: warns with the reason, then appends. Logging here as well as
    /// storing keeps the run log and the report's Gaps section saying the same
    /// sentence, which is what lets an operator match them up.
    /// Test: `super::tests::a_declared_absent_leg_is_named_in_the_gap_lines`.
    pub fn record_declared_skip(&mut self, skip: DeclaredSkip) {
        tracing::warn!(
            leg = %skip.leg,
            reason = %skip.reason,
            "collection leg declared absent; its sections are unassessed"
        );
        self.declared_skips.push(skip);
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
