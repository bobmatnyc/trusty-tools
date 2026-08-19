//! The CLI's [`ProgressSink`] — rustup-style narration over `trusty-progress`.
//!
//! Why: #5823 asks for a live display, and this crate must not grow a second
//! one. `trusty-progress` is the workspace's single progress renderer (#1315),
//! already used by `trusty-installer` and `trusty-mpm`, and it owns terminal
//! detection, the plain non-TTY fallback and the indicatif draw target — so
//! reaching for `indicatif` here would be a second implementation of a shared
//! capability, which CLAUDE.md's common-entry-point rule makes a defect.
//!
//! What: [`TerminalProgress`], which turns [`ProgressUpdate`]s into an `info:`
//! narration line per state change plus, on a TTY only, a spinner that carries
//! the current stage.
//!
//! ## Off a TTY
//!
//! `trusty-progress` hands a spinner a hidden draw target whenever the mode is
//! not [`Mode::Interactive`], so nothing repaints and no escape sequence is
//! written — the same posture `tga` describes for its own bars. What remains is
//! one plain line per state change, and the mid-flight
//! [`StageState::Advanced`] events are dropped there rather than printed:
//! a counter that ticks is a repaint by another name when it cannot repaint.
//! Test: `tests::a_non_tty_run_emits_no_control_characters`.
//!
//! ## Where the lines go
//!
//! stderr. `crate::cli::render`'s report is stdout, and a progress display
//! interleaved into it would corrupt anything piping the report onward.
//!
//! Test: `tests`.

use std::sync::Mutex;

use trusty_progress::{Mode, Narrator, Output, ProgressHandle};

use super::{Operation, ProgressSink, ProgressUpdate, StageState, UnitOutcome};

/// A live display on the terminal, degrading to plain lines off a TTY.
///
/// Why/What: see the module docs.
/// Test: `tests::a_non_tty_run_emits_no_control_characters`,
/// `tests::a_failed_unit_states_its_reason`.
#[derive(Debug)]
pub struct TerminalProgress {
    output: Output,
    narrator: Narrator,
    /// The spinner for the unit in flight, if any. `None` off a TTY, and `None`
    /// between units — a finished unit always clears its own handle, which is
    /// what stops a killed child from leaving one spinning forever.
    active: Mutex<Option<ProgressHandle>>,
}

impl TerminalProgress {
    /// A display on stderr, animated only when stderr is a terminal.
    pub fn to_stderr() -> Self {
        Self::new(Output::to_stderr())
    }

    /// A display over a caller-supplied [`Output`].
    ///
    /// Why: the tests render into a capture buffer and assert on the bytes,
    /// which is the only way to prove the no-escape-sequences guarantee without
    /// a terminal.
    pub fn new(output: Output) -> Self {
        Self {
            narrator: Narrator::new(output.clone()),
            output,
            active: Mutex::new(None),
        }
    }

    /// Whether this display animates. Off a TTY it only narrates.
    fn animates(&self) -> bool {
        self.output.mode() == Mode::Interactive
    }

    /// Narrate a line, discarding a write failure.
    ///
    /// A display that cannot write must not fail the operation it describes —
    /// that is the whole reason [`ProgressSink::update`] is infallible.
    fn say(&self, line: &str) {
        let _ = self.narrator.info(line);
    }

    /// Replace the in-flight spinner, clearing whatever it displaces.
    fn set_spinner(&self, spinner: Option<ProgressHandle>) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = active.take() {
            previous.finish_and_clear();
        }
        *active = spinner;
    }

    /// Retarget the in-flight spinner, if there is one.
    fn set_message(&self, message: String) {
        let active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(handle) = active.as_ref() {
            handle.set_message(message);
        }
    }
}

impl ProgressSink for TerminalProgress {
    /// Render one update.
    ///
    /// What: an `info:` line for every state change, plus a spinner on a TTY.
    /// The one update that does NOT get a line is a mid-flight
    /// [`StageState::Advanced`], which only moves the spinner — off a TTY it is
    /// dropped, because an append-only log of a ticking counter is noise.
    /// Test: `tests::a_non_tty_run_emits_no_control_characters`.
    fn update(&self, update: ProgressUpdate) {
        match update {
            ProgressUpdate::OperationStarted { operation, total } => {
                self.say(&format!(
                    "{} ({total} {})",
                    operation.label(),
                    operation.units(total)
                ));
            }
            ProgressUpdate::UnitStarted {
                operation,
                target,
                index,
                total,
            } => {
                let line = format!("[{index}/{total}] {} {target}", verb(operation));
                if self.animates() {
                    self.set_spinner(Some(ProgressHandle::spinner(&self.output, line)));
                } else {
                    self.say(&line);
                }
            }
            ProgressUpdate::UnitStage { target, stage } => {
                let detail = stage
                    .detail
                    .as_deref()
                    .map(|d| format!(" — {}", first_line(d)))
                    .unwrap_or_default();
                let line = format!(
                    "{target}: {} {} [{}]{detail}",
                    stage.stage.to_lowercase(),
                    stage.target,
                    label(stage.state)
                );
                if self.animates() {
                    self.set_message(line);
                } else if stage.state != StageState::Advanced {
                    self.say(&line);
                }
            }
            ProgressUpdate::UnitFinished {
                operation,
                target,
                outcome,
            } => {
                // Clear FIRST: whatever happened to the unit, the spinner
                // describing it is now wrong, and a child killed mid-stage must
                // not leave one turning.
                self.set_spinner(None);
                let _ = match &outcome {
                    UnitOutcome::Succeeded => {
                        self.narrator.info(&format!("{target} {}", done(operation)))
                    }
                    UnitOutcome::Failed(reason) => self
                        .narrator
                        .warn(&format!("{target} failed: {}", first_line(reason))),
                    UnitOutcome::Skipped(reason) => self
                        .narrator
                        .warn(&format!("{target} skipped: {}", first_line(reason))),
                };
            }
            ProgressUpdate::OperationFinished {
                operation,
                succeeded,
                total,
            } => {
                self.set_spinner(None);
                self.say(&format!(
                    "{succeeded} of {total} {} {}",
                    operation.units(total),
                    done(operation)
                ));
            }
        }
    }
}

/// What this operation does to a unit, in the present tense.
fn verb(operation: Operation) -> &'static str {
    match operation {
        Operation::InstallTools => "installing",
        Operation::CloneRepos => "cloning",
        Operation::Sweep => "auditing",
        Operation::Package | Operation::Distribute => "packaging",
        Operation::Rerender => "re-rendering",
    }
}

/// What this operation leaves a unit having been.
fn done(operation: Operation) -> &'static str {
    match operation {
        Operation::InstallTools => "installed",
        Operation::CloneRepos => "cloned",
        Operation::Sweep => "audited",
        Operation::Package | Operation::Distribute => "packaged",
        Operation::Rerender => "re-rendered",
    }
}

/// One-word status for a relayed stage state.
///
/// The wildcard is not laziness: [`StageState`] is `#[non_exhaustive]`, so a
/// newer producer can send a state this build has never heard of. Rendering it
/// as `running` is the honest reading — the unit is in flight and this display
/// cannot say more — and it is what keeps an old parent working against a newer
/// child rather than failing to compile against it.
fn label(state: StageState) -> &'static str {
    match state {
        StageState::Started => "started",
        StageState::Completed => "ok",
        StageState::Failed => "failed",
        StageState::Skipped => "skipped",
        // `Advanced`, and anything a newer producer adds.
        _ => "running",
    }
}

/// The first line of a possibly multi-line reason.
///
/// Why: a failure reason is an `anyhow` cause chain, which routinely runs to
/// several lines. A spinner message containing a newline breaks the display,
/// and a narration line containing one stops being a line.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::StageEvent;
    use trusty_progress::Capture;

    fn capturing() -> (TerminalProgress, Capture) {
        let (output, capture) = Output::for_capture(Mode::Plain);
        (TerminalProgress::new(output), capture)
    }

    /// Drive one repository from start to verdict, as the sweep does.
    fn drive(sink: &TerminalProgress) {
        sink.update(ProgressUpdate::OperationStarted {
            operation: Operation::Sweep,
            total: 2,
        });
        sink.update(ProgressUpdate::UnitStarted {
            operation: Operation::Sweep,
            target: "acme/api".into(),
            index: 1,
            total: 2,
        });
        sink.update(ProgressUpdate::UnitStage {
            target: "acme/api".into(),
            stage: StageEvent::new("Audit", "collect", StageState::Started)
                .with_detail("stage 1 of 9"),
        });
        sink.update(ProgressUpdate::UnitStage {
            target: "acme/api".into(),
            stage: StageEvent::new("Collect", "acme/api", StageState::Advanced)
                .with_counts(120, Some(400)),
        });
        sink.update(ProgressUpdate::UnitStage {
            target: "acme/api".into(),
            stage: StageEvent::new("Audit", "collect", StageState::Completed),
        });
        sink.update(ProgressUpdate::UnitFinished {
            operation: Operation::Sweep,
            target: "acme/api".into(),
            outcome: UnitOutcome::Succeeded,
        });
        sink.update(ProgressUpdate::OperationFinished {
            operation: Operation::Sweep,
            succeeded: 1,
            total: 2,
        });
    }

    /// Why: the requirement this display exists under — CI, a pipe, and the
    /// Tauri shell capturing output all read a non-terminal, and an escape
    /// sequence or a carriage return there corrupts the capture. This is the
    /// regression test for the whole non-TTY posture.
    /// What: a full run in [`Mode::Plain`] contains no ESC, no CR, and no
    /// backspace, and every line stands on its own.
    /// Test: this is the test.
    #[test]
    fn a_non_tty_run_emits_no_control_characters() {
        let (sink, capture) = capturing();
        drive(&sink);
        let rendered = capture.contents();

        assert!(!rendered.is_empty(), "a plain run still narrates");
        for forbidden in ['\u{1b}', '\r', '\u{8}'] {
            assert!(
                !rendered.contains(forbidden),
                "{forbidden:?} reached a non-TTY stream:\n{rendered:?}"
            );
        }
        assert!(
            rendered.chars().all(|c| c == '\n' || !c.is_control()),
            "{rendered:?}"
        );
    }

    /// Why: off a TTY the display is append-only, so a per-tick counter would
    /// be thousands of lines of noise in a CI log. Only state CHANGES earn one.
    /// What: the `Advanced` stage produces no line, while the surrounding
    /// `Started` and `Completed` ones do.
    /// Test: this is the test.
    #[test]
    fn a_plain_run_reports_state_changes_and_not_ticks() {
        let (sink, capture) = capturing();
        drive(&sink);
        let rendered = capture.contents();

        assert!(
            rendered.contains("auditing repositories (2 repositories)"),
            "{rendered}"
        );
        assert!(rendered.contains("[1/2] auditing acme/api"), "{rendered}");
        assert!(rendered.contains("collect [started]"), "{rendered}");
        assert!(rendered.contains("collect [ok]"), "{rendered}");
        assert!(!rendered.contains("[running]"), "{rendered}");
        assert!(rendered.contains("acme/api audited"), "{rendered}");
        assert!(
            rendered.contains("1 of 2 repositories audited"),
            "{rendered}"
        );
    }

    /// Why: a repository that failed must say so, and the reason is what the
    /// operator acts on. A `warning:` prefix keeps it visually distinct from
    /// the `info:` narration around it.
    /// What: a multi-line failure reason is rendered as one `warning:` line
    /// carrying its first line.
    /// Test: this is the test.
    #[test]
    fn a_failed_unit_states_its_reason() {
        let (sink, capture) = capturing();
        sink.update(ProgressUpdate::UnitFinished {
            operation: Operation::Sweep,
            target: "acme/web".into(),
            outcome: UnitOutcome::Failed(
                "`tga audit` exited with code 3\ncaused by: no such config".into(),
            ),
        });
        let rendered = capture.contents();
        assert_eq!(
            rendered,
            "warning: acme/web failed: `tga audit` exited with code 3\n"
        );
    }

    /// Why: a child killed mid-stage leaves the display holding a spinner for
    /// a unit that no longer exists — on a TTY that is a line that turns
    /// forever, which is the wedged display #5823 names.
    /// What: the terminal update clears the in-flight handle before it
    /// narrates, so nothing is left active after a failed unit, and the
    /// underlying reason still reaches the stream.
    /// Test: this is the test.
    #[test]
    fn a_unit_that_dies_mid_stage_leaves_no_spinner() {
        let (output, capture) = Output::for_capture(Mode::Interactive);
        let sink = TerminalProgress::new(output);
        sink.update(ProgressUpdate::UnitStarted {
            operation: Operation::Sweep,
            target: "acme/api".into(),
            index: 1,
            total: 1,
        });
        sink.update(ProgressUpdate::UnitStage {
            target: "acme/api".into(),
            stage: StageEvent::new("Audit", "classify", StageState::Started),
        });
        assert!(
            sink.active.lock().expect("not poisoned").is_some(),
            "a started unit holds a spinner on a TTY"
        );

        sink.update(ProgressUpdate::UnitFinished {
            operation: Operation::Sweep,
            target: "acme/api".into(),
            outcome: UnitOutcome::Failed("`tga audit` was killed by a signal".into()),
        });
        assert!(
            sink.active.lock().expect("not poisoned").is_none(),
            "the spinner outlived the unit it described"
        );
        assert!(
            capture.contents().contains("killed by a signal"),
            "the failure must not be swallowed by the display: {:?}",
            capture.contents()
        );
    }
}
