//! The seam a front end renders live progress through.
//!
//! Why: #5823. Installing the pinned triple downloads four binaries, cloning
//! acquires a repository at a time, and the sweep spends up to four hours per
//! repository inside a `tga audit` child — and none of it showed the operator
//! anything. The obvious fix, printing from inside those functions, is the one
//! this crate cannot have: [`crate::session::Session::execute`] is the single
//! door every front end goes through (#5502), and a door that writes to a
//! terminal is a door the Tauri shell cannot open.
//!
//! So progress is a SINK the caller supplies. The operations emit
//! [`ProgressUpdate`]s; what those become — a spinner, a plain log line, a
//! window — is the front end's decision and lives nowhere near the dispatch.
//! `Session` defaults to [`Progress::none`], on which every update is dropped,
//! so a caller that wants no display writes no code and pays nothing.
//!
//! What: [`ProgressUpdate`] (the vocabulary), [`ProgressSink`] (the trait a
//! front end implements), and [`Progress`] (the cheap clonable handle the
//! operations hold). [`terminal::TerminalProgress`] is the CLI's implementation,
//! built on `trusty-progress` — it is the only part of this module that knows
//! what a terminal is, and nothing in `session.rs` mentions it.
//!
//! Test: `super::progress_tests`, and `terminal`'s own tests for the rendering.

use std::sync::Arc;

pub mod terminal;

/// The stage vocabulary a spawned child reports in, re-exported so a front end
/// does not have to depend on `trusty-progress` to match on one.
pub use trusty_progress::relay::{StageEvent, StageState};

/// Which long-running capability is reporting.
///
/// Why: a renderer says "cloning 12 repositories", not "doing 12 things", and
/// the noun differs per operation. Carrying the operation on every update means
/// a sink can render without tracking what it is inside.
/// What: the three capabilities that take long enough to need a display.
/// `#[non_exhaustive]` so a later one is additive.
/// Test: `super::progress_tests::labels_name_the_unit_being_counted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Operation {
    /// Downloading and verifying the pinned tool set (#5495).
    InstallTools,
    /// Acquiring the selected repositories (#5215).
    CloneRepos,
    /// Running `tga audit` over the selected repositories (#5555).
    Sweep,
    /// Assembling the deliverable to send back (#5499).
    ///
    /// One unit — the archive. It earns an operation anyway because #5824's
    /// chain would otherwise fall silent for its last phase, and a display that
    /// stops before the run does reads as a hang.
    Package,
}

impl Operation {
    /// Present-participle description, e.g. `"cloning repositories"`.
    pub fn label(self) -> &'static str {
        match self {
            Self::InstallTools => "installing pinned tools",
            Self::CloneRepos => "cloning repositories",
            Self::Sweep => "auditing repositories",
            Self::Package => "assembling the return package",
        }
    }

    /// Singular noun for one unit of this operation.
    pub fn unit(self) -> &'static str {
        match self {
            Self::InstallTools => "tool",
            Self::CloneRepos | Self::Sweep => "repository",
            Self::Package => "archive",
        }
    }

    /// The unit noun agreeing with `count` — `"repository"` / `"repositories"`.
    pub fn units(self, count: usize) -> &'static str {
        if count == 1 {
            return self.unit();
        }
        match self {
            Self::InstallTools => "tools",
            Self::CloneRepos | Self::Sweep => "repositories",
            Self::Package => "archives",
        }
    }
}

/// How one unit of an operation ended.
///
/// Why: every one of these operations continues past a unit that failed — that
/// is the whole point of a hundred-repository sweep — so "ended" is not
/// "succeeded", and a display that renders them alike is the fail-open shape
/// the rest of this crate exists to avoid.
/// What: three verdicts, the two non-success ones carrying a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnitOutcome {
    /// The unit finished as asked.
    Succeeded,
    /// The unit failed; the string is safe to show the operator.
    Failed(String),
    /// The unit was deliberately not attempted.
    Skipped(String),
}

impl UnitOutcome {
    /// One-word status, matching the vocabulary `trusty-progress` renders.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Succeeded => "ok",
            Self::Failed(_) => "failed",
            Self::Skipped(_) => "skipped",
        }
    }

    /// The reason attached to a non-success verdict.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Succeeded => None,
            Self::Failed(r) | Self::Skipped(r) => Some(r.as_str()),
        }
    }
}

/// One thing worth telling the operator.
///
/// Why: structured rather than a formatted string, for the same reason
/// [`crate::session::Outcome`] is — the CLI renders these as `info:` lines and
/// spinners, and a GUI renders the same values as rows. Text here would force
/// the second front end to parse or to grow its own path.
/// What: the operation's lifecycle, its units' lifecycles, and the stages a
/// spawned child reports from inside one unit.
/// Test: `super::progress_tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProgressUpdate {
    /// The operation began, over `total` units.
    OperationStarted {
        /// Which capability.
        operation: Operation,
        /// How many units it will attempt.
        total: usize,
    },
    /// One unit began.
    UnitStarted {
        /// Which capability.
        operation: Operation,
        /// The unit — a tool name, an `owner/name`, a repository.
        target: String,
        /// Its 1-based position in the operation.
        index: usize,
        /// How many units the operation will attempt.
        total: usize,
    },
    /// A spawned child reported a stage of its own, inside `target`.
    ///
    /// This is the relayed half: `tga audit` runs nine stages per repository,
    /// and without this the display would sit on one row for the whole hour.
    UnitStage {
        /// The unit whose child produced it.
        target: String,
        /// What the child said.
        stage: StageEvent,
    },
    /// One unit ended.
    UnitFinished {
        /// Which capability.
        operation: Operation,
        /// The unit.
        target: String,
        /// How it ended.
        outcome: UnitOutcome,
    },
    /// The operation ended.
    OperationFinished {
        /// Which capability.
        operation: Operation,
        /// How many units succeeded.
        succeeded: usize,
        /// How many were attempted.
        total: usize,
    },
}

/// Where progress updates go.
///
/// Why: the one interface a front end implements. It is deliberately infallible
/// — a display that cannot draw must never fail the four-hour sweep it was
/// describing, so an implementation swallows its own write errors rather than
/// handing them back.
/// What: one method. `Send + Sync` because the sweep forwards a child's output
/// from a spawned task.
/// Test: `super::progress_tests` drives operations through a recording sink.
pub trait ProgressSink: std::fmt::Debug + Send + Sync {
    /// Render, record, or ignore one update.
    fn update(&self, update: ProgressUpdate);
}

/// The handle every long-running operation holds.
///
/// Why: an `Option<&dyn ProgressSink>` threaded through six signatures makes
/// every emit site a branch and every test a `None`. This is `tga`'s
/// `ProgressBus::disabled` shape: absent is a value, not a case.
/// What: cheap to clone (`Arc` inside). [`Progress::none`] discards everything.
/// Test: `super::progress_tests::an_absent_sink_discards_every_update`.
#[derive(Clone, Default)]
pub struct Progress(Option<Arc<dyn ProgressSink>>);

impl Progress {
    /// A handle that discards every update and allocates nothing.
    pub fn none() -> Self {
        Self(None)
    }

    /// A handle delivering to `sink`.
    pub fn to(sink: Arc<dyn ProgressSink>) -> Self {
        Self(Some(sink))
    }

    /// Whether anything is listening.
    ///
    /// An emit site that would have to allocate to build its update can skip
    /// the work entirely when nothing is.
    pub fn is_active(&self) -> bool {
        self.0.is_some()
    }

    /// Deliver one update.
    pub fn send(&self, update: ProgressUpdate) {
        if let Some(sink) = &self.0 {
            sink.update(update);
        }
    }

    /// Announce the start of an operation over `total` units.
    pub fn operation_started(&self, operation: Operation, total: usize) {
        self.send(ProgressUpdate::OperationStarted { operation, total });
    }

    /// Announce that unit `index` of `total` began.
    pub fn unit_started(
        &self,
        operation: Operation,
        target: impl Into<String>,
        index: usize,
        total: usize,
    ) {
        self.send(ProgressUpdate::UnitStarted {
            operation,
            target: target.into(),
            index,
            total,
        });
    }

    /// Relay a stage a child reported from inside `target`.
    pub fn unit_stage(&self, target: impl Into<String>, stage: StageEvent) {
        self.send(ProgressUpdate::UnitStage {
            target: target.into(),
            stage,
        });
    }

    /// Announce how one unit ended.
    pub fn unit_finished(
        &self,
        operation: Operation,
        target: impl Into<String>,
        outcome: UnitOutcome,
    ) {
        self.send(ProgressUpdate::UnitFinished {
            operation,
            target: target.into(),
            outcome,
        });
    }

    /// Announce how the operation ended.
    pub fn operation_finished(&self, operation: Operation, succeeded: usize, total: usize) {
        self.send(ProgressUpdate::OperationFinished {
            operation,
            succeeded,
            total,
        });
    }
}

impl std::fmt::Debug for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Progress")
            .field(&self.0.as_ref().map(|_| "sink"))
            .finish()
    }
}

/// A sink that remembers everything, for tests that assert on what was said.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct Recorder(std::sync::Mutex<Vec<ProgressUpdate>>);

#[cfg(test)]
impl Recorder {
    /// A recorder and the [`Progress`] handle delivering to it.
    pub(crate) fn new() -> (Arc<Self>, Progress) {
        let sink = Arc::new(Self::default());
        let progress = Progress::to(Arc::clone(&sink) as Arc<dyn ProgressSink>);
        (sink, progress)
    }

    /// Everything delivered so far, in order.
    pub(crate) fn updates(&self) -> Vec<ProgressUpdate> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Every stage event relayed from a child, in order.
    pub(crate) fn stages(&self) -> Vec<StageEvent> {
        self.updates()
            .into_iter()
            .filter_map(|u| match u {
                ProgressUpdate::UnitStage { stage, .. } => Some(stage),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
impl ProgressSink for Recorder {
    fn update(&self, update: ProgressUpdate) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(update);
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    /// Why: [`Progress::none`] is what `Session` holds unless a front end says
    /// otherwise, so "no display" has to be free and silent rather than a
    /// branch every call site remembers.
    /// What: sending through an absent sink neither panics nor records.
    /// Test: this is the test.
    #[test]
    fn an_absent_sink_discards_every_update() {
        let progress = Progress::none();
        assert!(!progress.is_active());
        progress.operation_started(Operation::Sweep, 3);
        progress.unit_started(Operation::Sweep, "acme/api", 1, 3);
        progress.unit_finished(Operation::Sweep, "acme/api", UnitOutcome::Succeeded);
        progress.operation_finished(Operation::Sweep, 1, 3);
    }

    /// Why: a sink renders the noun, so an operation that named the wrong one
    /// would say "4 repositories installed".
    /// What: each operation's unit noun matches what it counts.
    /// Test: this is the test.
    #[test]
    fn labels_name_the_unit_being_counted() {
        assert_eq!(Operation::InstallTools.unit(), "tool");
        assert_eq!(Operation::CloneRepos.unit(), "repository");
        assert_eq!(Operation::Sweep.unit(), "repository");
        let labels: Vec<&str> = [
            Operation::InstallTools,
            Operation::CloneRepos,
            Operation::Sweep,
        ]
        .map(Operation::label)
        .to_vec();
        // Distinct, or a display cannot tell one operation from another.
        assert_eq!(
            labels
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            labels.len(),
            "{labels:?}"
        );
    }

    /// Why: the display distinguishes a failure from a skip, and both from a
    /// success — the reason is what an operator acts on.
    /// What: labels and reasons match the verdict.
    /// Test: this is the test.
    #[test]
    fn an_outcome_carries_its_reason() {
        assert_eq!(UnitOutcome::Succeeded.reason(), None);
        assert_eq!(UnitOutcome::Succeeded.label(), "ok");
        let failed = UnitOutcome::Failed("exited with code 3".into());
        assert_eq!(failed.label(), "failed");
        assert_eq!(failed.reason(), Some("exited with code 3"));
        let skipped = UnitOutcome::Skipped("budget spent".into());
        assert_eq!(skipped.label(), "skipped");
        assert_eq!(skipped.reason(), Some("budget spent"));
    }

    /// Why: the recorder underpins every other test in this crate that asserts
    /// on progress; confirm it preserves order and content.
    /// What: four updates arrive in the order they were sent.
    /// Test: this is the test.
    #[test]
    fn a_recording_sink_preserves_order() {
        let (recorder, progress) = Recorder::new();
        progress.operation_started(Operation::CloneRepos, 2);
        progress.unit_started(Operation::CloneRepos, "acme/api", 1, 2);
        progress.unit_stage(
            "acme/api",
            StageEvent::new("Audit", "collect", StageState::Started),
        );
        progress.operation_finished(Operation::CloneRepos, 1, 2);

        let updates = recorder.updates();
        assert_eq!(updates.len(), 4);
        assert!(matches!(
            updates[0],
            ProgressUpdate::OperationStarted {
                operation: Operation::CloneRepos,
                total: 2
            }
        ));
        assert_eq!(recorder.stages().len(), 1);
    }
}
