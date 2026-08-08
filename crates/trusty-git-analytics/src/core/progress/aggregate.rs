//! Folding [`ProgressEvent`]s into renderable state.
//!
//! Why: a draw call is hard to test, so everything that decides *what* the
//! progress pane shows lives here as pure state transitions over an event
//! stream — leaving the renderer with nothing but layout.
//! What: [`ProgressAggregate`], which keeps one [`TargetRow`] per
//! `(stage, target)` and a per-stage roll-up, plus a bounded activity log.
//! Test: `super::tests::aggregate_*`.

use std::collections::BTreeMap;

use super::event::{Outcome, ProgressEvent, Stage};

/// How many activity lines the aggregate retains.
///
/// Why: the log is a scrolling tail, not a transcript; an unbounded Vec would
/// grow without limit across a multi-hour run.
/// What: `500` lines, oldest evicted first.
/// Test: `super::tests::aggregate_log_is_bounded`.
pub const LOG_CAPACITY: usize = 500;

/// Live state of one unit of work.
///
/// Why: the progress pane renders one line per repo / batch; this is that line
/// in structured form.
/// What: the counters and the terminal verdict, if it has arrived.
/// Test: `super::tests::aggregate_tracks_per_target_rows`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TargetRow {
    /// The unit's name — a repository, a board key, or a batch label.
    pub target: String,
    /// Units finished so far.
    pub done: u64,
    /// Units expected, when known.
    pub total: Option<u64>,
    /// Terminal verdict; `None` while still running.
    pub outcome: Option<Outcome>,
    /// The most recent detail line seen for this target.
    pub detail: Option<String>,
}

impl TargetRow {
    /// Completion ratio in `0.0..=1.0`, when a total is known.
    ///
    /// Why: gauges need a fraction; rows with an unknown total render as a
    /// spinner instead, so the caller must be able to tell the two apart.
    /// What: `Some(done / total)` clamped to 1.0 when `total > 0`, else `None`.
    /// A terminal row with no total reports `Some(1.0)` — it is finished, and a
    /// finished bar that never fills reads as a hang.
    /// Test: `super::tests::target_row_fraction`.
    pub fn fraction(&self) -> Option<f64> {
        match self.total {
            Some(t) if t > 0 => Some((self.done as f64 / t as f64).min(1.0)),
            _ if self.outcome.is_some() => Some(1.0),
            _ => None,
        }
    }

    /// Whether this unit is still running.
    ///
    /// Why/What/Test: inverse of a terminal outcome; see
    /// `super::tests::aggregate_tracks_per_target_rows`.
    pub fn is_running(&self) -> bool {
        self.outcome.is_none()
    }
}

/// Per-stage roll-up over its [`TargetRow`]s.
///
/// Why: the header line answers "how far along is Collect?" without the
/// consumer re-deriving it from every row on every frame.
/// What: counts of running / completed / failed / skipped targets.
/// Test: `super::tests::aggregate_rolls_up_per_stage`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct StageSummary {
    /// Targets with no terminal event yet.
    pub running: usize,
    /// Targets that ended in [`Outcome::Completed`].
    pub completed: usize,
    /// Targets that ended in [`Outcome::Failed`].
    pub failed: usize,
    /// Targets that ended in [`Outcome::Skipped`].
    pub skipped: usize,
}

impl StageSummary {
    /// Total targets seen for the stage.
    ///
    /// Why/What/Test: sum of the four counters; see
    /// `super::tests::aggregate_rolls_up_per_stage`.
    pub fn total(&self) -> usize {
        self.running + self.completed + self.failed + self.skipped
    }

    /// Whether the stage has been seen at all.
    ///
    /// Why: stages with no events render dimmed rather than as "0 of 0".
    /// What: `total() > 0`.
    /// Test: `super::tests::aggregate_rolls_up_per_stage`.
    pub fn is_started(&self) -> bool {
        self.total() > 0
    }
}

/// Everything a progress display needs, folded from the event stream.
///
/// Why: this is the single place that decides how events become rows, so both
/// the TUI and any future non-TUI consumer agree, and so the logic is testable
/// without a terminal.
/// What: per-`(stage, target)` rows in insertion order, a per-stage roll-up,
/// a bounded activity log, and the count of events the bus had to drop.
/// Test: `super::tests::aggregate_*`.
#[derive(Debug, Clone, Default)]
pub struct ProgressAggregate {
    rows: BTreeMap<Stage, Vec<TargetRow>>,
    log: std::collections::VecDeque<String>,
    dropped: u64,
}

impl ProgressAggregate {
    /// An empty aggregate.
    ///
    /// Why/What/Test: `Default`; see `super::tests::aggregate_starts_empty`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one event into the aggregate.
    ///
    /// Why: the consumer's per-tick loop is `for e in bus.drain() { agg.apply(e) }`,
    /// so all merge semantics belong here.
    /// What: upserts the `(stage, target)` row — counters are overwritten by
    /// the newer event, a terminal outcome is recorded, and an event with no
    /// `total` does not erase a `total` an earlier event established. Appends
    /// an activity line for terminal events and for events carrying a detail.
    /// Test: `super::tests::aggregate_tracks_per_target_rows`,
    /// `aggregate_advance_keeps_known_total`.
    pub fn apply(&mut self, event: ProgressEvent) {
        let rows = self.rows.entry(event.stage).or_default();
        let idx = rows.iter().position(|r| r.target == event.target);
        let idx = match idx {
            Some(i) => i,
            None => {
                rows.push(TargetRow {
                    target: event.target.clone(),
                    done: 0,
                    total: None,
                    outcome: None,
                    detail: None,
                });
                rows.len() - 1
            }
        };
        let row = &mut rows[idx];
        row.done = event.done.max(row.done);
        if event.total.is_some() {
            row.total = event.total;
        }
        if event.detail.is_some() {
            row.detail = event.detail.clone();
        }
        if event.outcome.is_some() {
            row.outcome = event.outcome.clone();
        }

        if let Some(line) = activity_line(&event) {
            self.push_log(line);
        }
    }

    /// Record how many events the bus discarded.
    ///
    /// Why: a consumer that renders a gap should be able to label it.
    /// What: stores the latest cumulative count from
    /// [`super::ProgressBus::dropped`].
    /// Test: `super::tests::aggregate_records_dropped`.
    pub fn set_dropped(&mut self, dropped: u64) {
        self.dropped = dropped;
    }

    /// How many events the bus discarded.
    ///
    /// Why/What/Test: see [`ProgressAggregate::set_dropped`].
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Append a line to the activity log that did not come from an event.
    ///
    /// Why: #5197 diverts the process's `tracing` output into the TUI while it
    /// owns the terminal — writing it to stderr would print inside the drawn
    /// frame. Those lines have no `ProgressEvent` behind them, but they belong
    /// in the same pane and under the same bound as the ones that do.
    /// What: pushes onto the bounded log, evicting the oldest past
    /// [`LOG_CAPACITY`].
    /// Test: `super::tests::aggregate_accepts_external_activity_lines`.
    pub fn push_activity(&mut self, line: String) {
        self.push_log(line);
    }

    /// Rows for one stage, in first-seen order.
    ///
    /// Why: the pane lists repos in the order the pipeline reached them, which
    /// is more useful than any sort.
    /// What: an empty slice when the stage has produced nothing.
    /// Test: `super::tests::aggregate_tracks_per_target_rows`.
    pub fn rows(&self, stage: Stage) -> &[TargetRow] {
        self.rows.get(&stage).map_or(&[], Vec::as_slice)
    }

    /// Roll-up for one stage.
    ///
    /// Why/What/Test: see [`StageSummary`] and
    /// `super::tests::aggregate_rolls_up_per_stage`.
    pub fn summary(&self, stage: Stage) -> StageSummary {
        let mut s = StageSummary::default();
        for row in self.rows(stage) {
            match &row.outcome {
                None => s.running += 1,
                Some(Outcome::Completed) => s.completed += 1,
                Some(Outcome::Failed { .. }) => s.failed += 1,
                Some(Outcome::Skipped { .. }) => s.skipped += 1,
            }
        }
        s
    }

    /// The bounded activity log, oldest first.
    ///
    /// Why/What/Test: see [`LOG_CAPACITY`] and
    /// `super::tests::aggregate_log_is_bounded`.
    pub fn log(&self) -> impl DoubleEndedIterator<Item = &String> {
        self.log.iter()
    }

    /// Whether any stage has produced an event.
    ///
    /// Why: the TUI shows a "press r to run" hint until work starts.
    /// What: `true` when no stage has any row.
    /// Test: `super::tests::aggregate_starts_empty`.
    pub fn is_empty(&self) -> bool {
        self.rows.values().all(Vec::is_empty)
    }

    /// Whether every seen target has reached a terminal outcome.
    ///
    /// Why: the event loop flips back to the results view once work settles.
    /// What: `false` when the aggregate is empty (nothing has run yet).
    /// Test: `super::tests::aggregate_is_settled`.
    pub fn is_settled(&self) -> bool {
        !self.is_empty() && self.rows.values().flatten().all(|r| !r.is_running())
    }

    fn push_log(&mut self, line: String) {
        while self.log.len() >= LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }
}

/// Render one event as an activity-log line, or `None` if it is not worth one.
///
/// Why: mid-flight counter ticks would flood the log; only terminal events and
/// events carrying an explicit detail earn a line.
/// What: `"HH:MM:SS Stage target — status detail"`.
/// Test: `super::tests::activity_line_only_for_notable_events`.
fn activity_line(event: &ProgressEvent) -> Option<String> {
    let status = match &event.outcome {
        Some(o) => o.label(),
        None if event.detail.is_some() => "…",
        None => return None,
    };
    let ts = event.at.format("%H:%M:%S");
    let mut line = format!("{ts} {} {} — {status}", event.stage.label(), event.target);
    if let Some(d) = event.detail.as_deref() {
        // A failure's detail is its reason, already implied by the status;
        // still print it, since the reason is the whole point of the line.
        line.push_str(": ");
        line.push_str(d);
    }
    Some(line)
}
