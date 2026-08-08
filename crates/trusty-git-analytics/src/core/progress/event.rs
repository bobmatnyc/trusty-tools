//! Structured progress events emitted by the collect and correlate pipelines.
//!
//! Why: `indicatif` bars are one-shot per stage and are hidden off-TTY, so
//! nothing in the crate could drive a live multi-stage display. #5197 needs a
//! machine-readable event a TUI (or any other consumer) can fold into state.
//! What: a [`Stage`] discriminator, an [`Outcome`] terminal verdict, and the
//! [`ProgressEvent`] record carrying stage / target / counters / outcome. The
//! event is `#[non_exhaustive]` and built through named constructors so later
//! field additions stay SemVer-compatible for downstream crates.
//! Test: `super::tests::event_constructors_set_expected_fields` and
//! `event_is_terminal_only_when_outcome_present`.

use chrono::{DateTime, Utc};

/// Which pipeline stage produced an event.
///
/// Why: a consumer groups rows by stage, so the stage must be a closed-ish
/// discriminator rather than a free-form string that drifts between emit sites.
/// What: the three long-running stages that emit today. `#[non_exhaustive]` so
/// adding `Report` later is not a breaking change for downstream matchers.
/// Test: `super::tests::stage_label_is_stable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Stage {
    /// Stage 1 — git walk plus board/PR pulls (`collect/`).
    Collect,
    /// Commit ↔ board-item correlation over `work_items` / `commit_work_items`.
    Correlate,
    /// Stage 2 — the classification cascade (`classify/`).
    Classify,
}

impl Stage {
    /// Short display label for this stage.
    ///
    /// Why: renderers and log lines both need one canonical spelling.
    /// What: `"Collect"` / `"Correlate"` / `"Classify"`.
    /// Test: `super::tests::stage_label_is_stable`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Collect => "Collect",
            Self::Correlate => "Correlate",
            Self::Classify => "Classify",
        }
    }

    /// Every stage, in pipeline order.
    ///
    /// Why: the aggregate view lists stages in a stable order even before any
    /// event for a stage has arrived.
    /// What: `[Collect, Correlate, Classify]`.
    /// Test: `super::tests::stage_label_is_stable`.
    pub fn all() -> [Stage; 3] {
        [Self::Collect, Self::Correlate, Self::Classify]
    }
}

/// Terminal verdict for one unit of work.
///
/// Why: "done" is not enough — an operator must distinguish a repo that was
/// collected from one that was skipped because its week was already recorded,
/// and from one that failed with a reason worth reading.
/// What: `Completed`, `Failed { reason }`, `Skipped { reason }`.
/// `#[non_exhaustive]` so a later `Cancelled` variant is additive.
/// Test: `super::tests::event_is_terminal_only_when_outcome_present`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// The unit finished successfully.
    Completed,
    /// The unit failed; `reason` is the operator-facing explanation.
    Failed {
        /// Why the unit failed.
        reason: String,
    },
    /// The unit was intentionally not run; `reason` says why.
    Skipped {
        /// Why the unit was skipped.
        reason: String,
    },
}

impl Outcome {
    /// One-word label for this outcome.
    ///
    /// Why: the results and progress panes render a fixed-width status column.
    /// What: `"ok"` / `"failed"` / `"skipped"`.
    /// Test: `super::tests::outcome_label_is_stable`.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Completed => "ok",
            Self::Failed { .. } => "failed",
            Self::Skipped { .. } => "skipped",
        }
    }

    /// The explanation attached to a non-success outcome, if any.
    ///
    /// Why: renderers show the reason inline without matching on the variant.
    /// What: `Some(reason)` for `Failed` / `Skipped`, `None` for `Completed`.
    /// Test: `super::tests::outcome_label_is_stable`.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Completed => None,
            Self::Failed { reason } | Self::Skipped { reason } => Some(reason.as_str()),
        }
    }
}

/// One observation about the progress of a long-running pipeline.
///
/// Why: consumers need enough to render a live display without knowing
/// anything about the pipeline internals — which stage, which repo or item,
/// how far along, and how it ended.
/// What: an immutable record. `#[non_exhaustive]` blocks struct-literal
/// construction outside this crate, so every event is built through
/// [`ProgressEvent::started`], [`ProgressEvent::advanced`],
/// [`ProgressEvent::completed`], [`ProgressEvent::failed`], or
/// [`ProgressEvent::skipped`]. That keeps field additions non-breaking for
/// the published `tga` crate.
/// Test: `super::tests::event_constructors_set_expected_fields`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProgressEvent {
    /// Which pipeline stage emitted this event.
    pub stage: Stage,
    /// The unit of work: a repository name, a board key, or a batch label.
    pub target: String,
    /// Units finished so far within `target` (or within the stage when the
    /// event describes stage-level progress).
    pub done: u64,
    /// Total units expected, when the producer knows it up front.
    pub total: Option<u64>,
    /// Terminal verdict. `None` means the unit is still running.
    pub outcome: Option<Outcome>,
    /// Optional human-readable detail shown in the activity log.
    pub detail: Option<String>,
    /// When the producer created the event.
    pub at: DateTime<Utc>,
}

impl ProgressEvent {
    /// Base constructor shared by the named builders below.
    fn new(stage: Stage, target: impl Into<String>) -> Self {
        Self {
            stage,
            target: target.into(),
            done: 0,
            total: None,
            outcome: None,
            detail: None,
            at: Utc::now(),
        }
    }

    /// A unit of work has begun.
    ///
    /// Why: the consumer needs the row to appear before any counter moves, so
    /// a slow first item still shows as in-flight rather than as nothing.
    /// What: `done = 0`, `total = total`, `outcome = None`.
    /// Test: `super::tests::event_constructors_set_expected_fields`.
    pub fn started(stage: Stage, target: impl Into<String>, total: Option<u64>) -> Self {
        Self {
            total,
            ..Self::new(stage, target)
        }
    }

    /// The unit has made progress but has not finished.
    ///
    /// Why: mid-flight counters are what turn a frozen-looking pull into an
    /// observable one.
    /// What: sets `done` / `total`; `outcome` stays `None`.
    /// Test: `super::tests::event_constructors_set_expected_fields`.
    pub fn advanced(
        stage: Stage,
        target: impl Into<String>,
        done: u64,
        total: Option<u64>,
    ) -> Self {
        Self {
            done,
            total,
            ..Self::new(stage, target)
        }
    }

    /// The unit finished successfully.
    ///
    /// Why/What/Test: see [`ProgressEvent::started`]; this sets
    /// `outcome = Some(Outcome::Completed)` and `done = done`.
    pub fn completed(stage: Stage, target: impl Into<String>, done: u64) -> Self {
        Self {
            done,
            total: Some(done),
            outcome: Some(Outcome::Completed),
            ..Self::new(stage, target)
        }
    }

    /// The unit failed; `reason` is surfaced verbatim to the operator.
    ///
    /// Why/What/Test: see [`ProgressEvent::started`]; this sets
    /// `outcome = Some(Outcome::Failed { reason })`.
    pub fn failed(stage: Stage, target: impl Into<String>, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            detail: Some(reason.clone()),
            outcome: Some(Outcome::Failed { reason }),
            ..Self::new(stage, target)
        }
    }

    /// The unit was deliberately not run.
    ///
    /// Why/What/Test: see [`ProgressEvent::started`]; this sets
    /// `outcome = Some(Outcome::Skipped { reason })`.
    pub fn skipped(stage: Stage, target: impl Into<String>, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            detail: Some(reason.clone()),
            outcome: Some(Outcome::Skipped { reason }),
            ..Self::new(stage, target)
        }
    }

    /// Attach a free-form detail line to this event.
    ///
    /// Why: some emit sites have a useful one-liner (a branch name, an item
    /// count) that does not fit the structured fields.
    /// What: builder setter for [`ProgressEvent::detail`].
    /// Test: `super::tests::event_constructors_set_expected_fields`.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Whether this event ends its unit of work.
    ///
    /// Why: the aggregate stops counting a target as in-flight once a terminal
    /// event arrives.
    /// What: `self.outcome.is_some()`.
    /// Test: `super::tests::event_is_terminal_only_when_outcome_present`.
    pub fn is_terminal(&self) -> bool {
        self.outcome.is_some()
    }
}
