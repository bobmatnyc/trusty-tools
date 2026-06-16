//! Spinner / progress-bar handle — a thin wrapper over `indicatif`.
//!
//! Provides one type, [`ProgressHandle`], for both indeterminate steps
//! (spinner) and determinate steps (a bar with a known total). The handle
//! respects the shared [`Output`] mode: in plain/silent/non-TTY modes the
//! indicatif draw target is hidden, so no control characters leak into logs.

use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use crate::output::Output;

/// How often the spinner animation ticks while active.
const SPINNER_TICK: Duration = Duration::from_millis(80);
/// Template for an indeterminate spinner (`⠋ message`).
const SPINNER_TEMPLATE: &str = "{spinner:.cyan} {msg}";
/// Template for a determinate bar (`[#####>----] 12/40 message`).
const BAR_TEMPLATE: &str = "{bar:40.cyan/blue} {pos}/{len} {msg}";

/// Why: Callers want a single ergonomic handle that hides indicatif's setup
/// (styles, tick intervals, draw targets) and automatically degrades to no
/// animation in non-TTY/quiet contexts — without each call site re-deriving
/// terminal state. This wrapper owns that policy.
/// What: Wraps an `indicatif::ProgressBar` whose draw target was chosen from
/// the shared [`Output`] mode. Supports indeterminate (spinner) and
/// determinate (bar) progress; methods are no-ops when the target is hidden.
/// Test: `progress::tests` construct hidden handles (`Mode::Plain`) and drive
/// the API to confirm it does not panic and tracks position.
#[derive(Debug, Clone)]
pub struct ProgressHandle {
    bar: ProgressBar,
}

impl ProgressHandle {
    /// Why: Indeterminate work (connecting, resolving, "downloading…") has no
    /// known total, so a spinner is the right affordance.
    /// What: Starts a spinner with `message`, ticking on a timer in interactive
    /// mode and hidden otherwise.
    /// Test: `progress::tests::spinner_is_hidden_in_plain_mode`.
    pub fn spinner(output: &Output, message: impl Into<String>) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_draw_target(output.draw_target());
        // SAFETY: const template string, validated at construction; runtime
        // failure is impossible. `ProgressStyle::with_template` only fails on a
        // malformed template, and `SPINNER_TEMPLATE` is a compile-time `const`
        // literal we control — a parse failure here is a programmer error (a bad
        // edit to the constant), never a runtime condition, so `expect` is the
        // correct response: it surfaces the mistake loudly in dev/test rather than
        // silently rendering an unstyled spinner. This is a sanctioned `expect`.
        let style = ProgressStyle::with_template(SPINNER_TEMPLATE).expect("const template valid");
        bar.set_style(style);
        bar.set_message(message.into());
        bar.enable_steady_tick(SPINNER_TICK);
        Self { bar }
    }

    /// Why: Determinate work (N components, N files) should show a filling bar
    /// and a position/total readout.
    /// What: Starts a bar with the given `total` and `message`; hidden in
    /// non-interactive modes.
    /// Test: `progress::tests::bar_tracks_position`.
    pub fn bar(output: &Output, total: u64, message: impl Into<String>) -> Self {
        let bar = ProgressBar::new(total);
        bar.set_draw_target(output.draw_target());
        // SAFETY: const template string, validated at construction; runtime
        // failure is impossible. See `spinner` above: `BAR_TEMPLATE` is a `const`
        // literal, so a parse failure is a programmer error and `expect` is the
        // correct response.
        let style = ProgressStyle::with_template(BAR_TEMPLATE).expect("const template valid");
        bar.set_style(style);
        bar.set_message(message.into());
        Self { bar }
    }

    /// Why: Long-running steps update their label as they progress (e.g.
    /// switch from "resolving" to the current filename).
    /// What: Replaces the handle's message text.
    /// Test: `progress::tests::bar_tracks_position` (exercises set_message path).
    pub fn set_message(&self, message: impl Into<String>) {
        self.bar.set_message(message.into());
    }

    /// Why: Determinate callers report units of work completed; advancing the
    /// bar is the core determinate verb.
    /// What: Advances the bar position by `delta`.
    /// Test: `progress::tests::bar_tracks_position`.
    pub fn inc(&self, delta: u64) {
        self.bar.inc(delta);
    }

    /// Why: Some callers know the absolute position rather than a delta.
    /// What: Sets the bar position to `pos`.
    /// Test: `progress::tests::bar_tracks_position`.
    pub fn set_position(&self, pos: u64) {
        self.bar.set_position(pos);
    }

    /// Why: The current position is occasionally needed for assertions and for
    /// callers that mix `inc`/`set_position`.
    /// What: Returns the bar's current position.
    /// Test: `progress::tests::bar_tracks_position`.
    pub fn position(&self) -> u64 {
        self.bar.position()
    }

    /// Why: A finished step should clear its spinner/bar line (interactive) so
    /// the narrator's `info:` summary line reads cleanly afterwards.
    /// What: Stops the animation and removes the line from the terminal.
    /// Test: `progress::tests::finish_and_clear_is_safe`.
    pub fn finish_and_clear(self) {
        self.bar.finish_and_clear();
    }

    /// Why: Some flows want the final state to stay on screen with a closing
    /// message (e.g. "done").
    /// What: Stops the animation, leaving the line with `message`.
    /// Test: `progress::tests::finish_with_message_is_safe`.
    pub fn finish_with_message(self, message: impl Into<String>) {
        self.bar.finish_with_message(message.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::Mode;

    /// Why: In non-TTY/plain mode the spinner must use a hidden draw target so
    /// no control characters reach piped logs; the handle must still be usable.
    /// What: Creates a spinner in plain mode and confirms it is hidden.
    /// Test: This is the test.
    #[test]
    fn spinner_is_hidden_in_plain_mode() {
        let (out, _cap) = Output::for_capture(Mode::Plain);
        let handle = ProgressHandle::spinner(&out, "downloading");
        assert!(handle.bar.is_hidden());
        handle.finish_and_clear();
    }

    /// Why: The determinate API must accurately track position regardless of
    /// draw target (logic is independent of rendering).
    /// What: Advances a bar via `inc`/`set_position`/`set_message` and asserts
    /// the position.
    /// Test: This is the test.
    #[test]
    fn bar_tracks_position() {
        let (out, _cap) = Output::for_capture(Mode::Plain);
        let handle = ProgressHandle::bar(&out, 10, "installing");
        handle.inc(3);
        assert_eq!(handle.position(), 3);
        handle.set_position(7);
        handle.set_message("almost there");
        assert_eq!(handle.position(), 7);
        handle.finish_and_clear();
    }

    /// Why: `finish_and_clear` consumes the handle; verify it does not panic
    /// even with a hidden target.
    /// What: Finishes a hidden spinner.
    /// Test: This is the test.
    #[test]
    fn finish_and_clear_is_safe() {
        let (out, _cap) = Output::for_capture(Mode::Plain);
        ProgressHandle::spinner(&out, "x").finish_and_clear();
    }

    /// Why: `finish_with_message` is the alternate terminal verb; verify it is
    /// panic-free in plain mode.
    /// What: Finishes a hidden bar with a message.
    /// Test: This is the test.
    #[test]
    fn finish_with_message_is_safe() {
        let (out, _cap) = Output::for_capture(Mode::Plain);
        ProgressHandle::bar(&out, 5, "x").finish_with_message("done");
    }
}
