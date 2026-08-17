//! Writing [`ProgressEvent`]s out of this process, for a parent that spawned it.
//!
//! Why: #5823. `trusty-audit` spawns `tga audit` and waits up to four hours with
//! its stdout and stderr going to a log file, so an hour-long sweep shows the
//! operator nothing. The events a live display needs already exist — the sweep
//! emits one per stage on the [`ProgressBus`] — but they never leave the
//! process, because the only consumer wired up is the in-process TUI.
//!
//! What: [`StageRelay`], which turns the bus on when the parent asked for it and
//! writes each event to stderr as one [`trusty_progress::relay`] line.
//!
//! ## Why an environment variable, not a flag
//!
//! The parent pins the `tga` version it runs (`trusty-audit`'s engagement
//! config), so a parent newer than its pinned child is ordinary. An unknown
//! flag makes that child exit 2 before it does any work; an unknown environment
//! variable is ignored, and the parent simply shows the coarse progress it can
//! derive itself. The relay degrades, rather than breaking the run.
//!
//! ## Why stderr
//!
//! stdout is the operator-facing report. stderr already carries this crate's
//! tracing output, and a parent that reads it line by line drops anything that
//! is not a relay line — see [`trusty_progress::relay::StageEvent::decode`].
//!
//! Test: `super::tests::relay_*`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use trusty_progress::relay::{relay_requested, StageEvent, StageState, ENV_RELAY};

use super::{Outcome, ProgressBus, ProgressEvent};

/// How often the relay task moves queued events onto stderr.
///
/// Why: the bus is drop-oldest with 1024 slots and the sweep emits per stage,
/// so this only has to beat the emit rate by a wide margin. Polling rather than
/// waking on every emit keeps the producer's cost unchanged.
const DRAIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// One [`ProgressEvent`] as the line a parent reads.
///
/// Why: the two vocabularies are deliberately separate — `trusty-progress` does
/// not know what stages this crate has, and this crate does not own the wire
/// format. This function is the only place they meet.
/// What: maps the stage label and the outcome onto a
/// [`StageEvent`], preserving counters and detail.
///
/// A non-terminal event with `done == 0` is relayed as
/// [`StageState::Started`]. That is an approximation: [`ProgressEvent`] does
/// not record which constructor built it, so nothing distinguishes a `started`
/// from an `advanced` that has not advanced yet. It costs nothing — the two
/// carry identical information — and it is what puts the row on screen before
/// the first counter moves.
/// Test: `super::tests::relay_line_carries_the_stage_and_outcome`.
pub fn line_for(event: &ProgressEvent) -> String {
    let state = match &event.outcome {
        None if event.done == 0 => StageState::Started,
        None => StageState::Advanced,
        Some(Outcome::Completed) => StageState::Completed,
        Some(Outcome::Failed { .. }) => StageState::Failed,
        Some(Outcome::Skipped { .. }) => StageState::Skipped,
    };
    let mut relayed = StageEvent::new(event.stage.label(), &event.target, state)
        .with_counts(event.done, event.total);
    // The outcome's reason is the more specific of the two when both are set;
    // `ProgressEvent::failed` puts the same text in both.
    if let Some(text) = event
        .outcome
        .as_ref()
        .and_then(Outcome::reason)
        .or(event.detail.as_deref())
    {
        relayed = relayed.with_detail(text);
    }
    relayed.encode()
}

/// The bus, plus the task that drains it onto stderr.
///
/// Why: `run_full_sweep` takes an `Option<&ProgressBus>` and runs for the whole
/// sweep, so somebody has to drain concurrently or the ring simply overflows
/// and the parent learns nothing until the end. Bundling the bus with its
/// drainer means a caller cannot start one without the other.
/// What: inactive when the parent did not ask — [`StageRelay::bus`] is then
/// `None`, every emit inside the sweep is a no-op, and no task is spawned.
/// Test: `super::tests::relay_is_off_unless_the_parent_asks`,
/// `super::tests::relay_writes_every_event_it_is_given`.
#[derive(Debug)]
pub struct StageRelay {
    bus: Option<ProgressBus>,
    stop: Arc<AtomicBool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl StageRelay {
    /// A relay that is on only if the process environment asks for it.
    ///
    /// Why/What/Test: see [`trusty_progress::relay::relay_requested`]; this is
    /// that rule applied to [`ENV_RELAY`], with the task spawned when it says
    /// yes.
    pub fn from_env() -> Self {
        if relay_requested(std::env::var(ENV_RELAY).ok().as_deref()) {
            Self::started()
        } else {
            Self::off()
        }
    }

    /// An inactive relay: no bus, no task, no output.
    pub fn off() -> Self {
        Self {
            bus: None,
            stop: Arc::new(AtomicBool::new(true)),
            task: None,
        }
    }

    /// An active relay writing to stderr until [`StageRelay::finish`].
    ///
    /// # Panics
    ///
    /// Never on its own, but it spawns a tokio task, so it must be called from
    /// within a runtime — `tga audit` always is.
    pub fn started() -> Self {
        let bus = ProgressBus::new();
        let stop = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(drain_to_stderr(bus.clone(), Arc::clone(&stop)));
        Self {
            bus: Some(bus),
            stop,
            task: Some(task),
        }
    }

    /// The bus to hand the sweep, or `None` when the relay is off.
    pub fn bus(&self) -> Option<&ProgressBus> {
        self.bus.as_ref()
    }

    /// Stop draining, after flushing whatever is still queued.
    ///
    /// Why: the last events of a sweep are the ones a parent most needs — the
    /// final stage's verdict — and they are emitted microseconds before the
    /// sweep returns. Dropping the relay without this would race the task's
    /// last tick and lose them.
    /// What: sets the stop flag and awaits the task, which drains once more
    /// before returning. A task that panicked is ignored: progress is
    /// cosmetic and must not fail the run that produced it.
    /// Test: `super::tests::relay_writes_every_event_it_is_given`.
    pub async fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for StageRelay {
    /// A relay dropped without [`StageRelay::finish`] still stops its task.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Move queued events onto stderr until asked to stop, then once more.
async fn drain_to_stderr(bus: ProgressBus, stop: Arc<AtomicBool>) {
    loop {
        let stopping = stop.load(Ordering::Relaxed);
        write_lines(&bus.drain());
        if stopping {
            return;
        }
        tokio::time::sleep(DRAIN_INTERVAL).await;
    }
}

/// Write one line per event, ignoring a write that fails.
///
/// A closed or full stderr is not a reason to fail a sweep — the parent that
/// wanted the events is the same process that closed the pipe.
fn write_lines(events: &[ProgressEvent]) {
    use std::io::Write as _;
    if events.is_empty() {
        return;
    }
    let mut rendered = String::new();
    for event in events {
        rendered.push_str(&line_for(event));
        rendered.push('\n');
    }
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(rendered.as_bytes());
    let _ = err.flush();
}
