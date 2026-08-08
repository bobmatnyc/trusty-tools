//! Where the collect pipeline's operator-facing lines go.
//!
//! Why: `tga collect` writes per-week lines to stdout and warnings to stderr,
//! which is exactly right for a plain CLI run. Under `tga tui` it is not: the
//! TUI owns the alternate screen, so a `println!` from the worker thread lands
//! inside the drawn frame, and ratatui's diff renderer only repaints cells it
//! believes changed — so the garbling survives every later redraw and is
//! permanent for the session (#5197).
//!
//! What: two free functions that take the pipeline's [`ProgressBus`] and pick
//! the destination from it. With no bus attached (every CLI path) they are the
//! `println!` / `eprintln!` they replaced, byte for byte. With one attached
//! they emit the same text as a `Collect` detail event, which the TUI folds
//! into its ACTIVITY pane — so the operator still reads the line, on a surface
//! that can hold it.
//!
//! Test: `tests` in this module.

use crate::core::progress::{ProgressBus, ProgressEvent, Stage};

/// Report normal progress — the stdout half of the pipeline's chatter.
///
/// Why/What/Test: see the module doc; `tests::progress_prints_when_no_bus_is_attached`
/// and `tests::progress_reaches_the_bus_instead_of_stdout`.
pub(super) fn progress(bus: &ProgressBus, target: &str, line: &str) {
    if emit(bus, target, line) {
        return;
    }
    println!("{line}");
}

/// Report a warning — the stderr half of the pipeline's chatter.
///
/// Why/What/Test: see the module doc; `tests::warning_reaches_the_bus_instead_of_stderr`.
pub(super) fn warning(bus: &ProgressBus, target: &str, line: &str) {
    if emit(bus, target, line) {
        return;
    }
    eprintln!("{line}");
}

/// Publish `line` as a detail on `target`'s Collect row, if anyone is listening.
///
/// Why: an `advanced` event with `done = 0` and `total = None` cannot regress a
/// row the pipeline already established — the aggregate keeps the larger `done`
/// and never lets a `None` erase a known total — so this carries the text and
/// nothing else.
/// What: `true` when the line was published (and therefore must NOT also be
/// printed), `false` when no consumer is attached.
/// Test: `tests::progress_reaches_the_bus_instead_of_stdout`.
fn emit(bus: &ProgressBus, target: &str, line: &str) -> bool {
    if !bus.is_active() {
        return false;
    }
    bus.emit(ProgressEvent::advanced(Stage::Collect, target, 0, None).with_detail(line));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_reaches_the_bus_instead_of_stdout() {
        let bus = ProgressBus::new();
        progress(
            &bus,
            "trusty-tools",
            "Collected W31 2026: 104 commits [trusty-tools]",
        );
        let events = bus.drain();
        assert_eq!(events.len(), 1, "an active bus must receive the line");
        assert_eq!(events[0].stage, Stage::Collect);
        assert_eq!(events[0].target, "trusty-tools");
        assert_eq!(
            events[0].detail.as_deref(),
            Some("Collected W31 2026: 104 commits [trusty-tools]")
        );
        assert!(
            !events[0].is_terminal(),
            "a chatter line must not settle the row"
        );
    }

    #[test]
    fn warning_reaches_the_bus_instead_of_stderr() {
        let bus = ProgressBus::new();
        warning(&bus, "repo", "warning: [repo] no since_date");
        let events = bus.drain();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].detail.as_deref(),
            Some("warning: [repo] no since_date")
        );
    }

    /// The plain-CLI path: nothing is published, so the caller printed instead.
    ///
    /// This asserts the routing decision, which is what changed. It cannot
    /// assert the bytes reached stdout — capturing an in-process `println!`
    /// needs an fd swap that would race every other test in the binary.
    #[test]
    fn progress_prints_when_no_bus_is_attached() {
        let bus = ProgressBus::disabled();
        assert!(
            !emit(&bus, "repo", "Collected W31 2026: 1 commits [repo]"),
            "a disabled bus must fall through to println!"
        );
        progress(&bus, "repo", "Collected W31 2026: 1 commits [repo]");
        warning(&bus, "repo", "warning: something");
        assert!(bus.drain().is_empty());
    }
}
