//! Real-time phase progress streaming to the terminal (#149).
//!
//! Why: Workflow runs take 20–70 minutes and previously emitted nothing on
//! stdout/stderr until the `observe` phase completed. Users had no signal that
//! the harness was alive, which is terrible UX. Streaming a one-line summary
//! per phase (and per code-wave) to stderr — colorized when stderr is a TTY —
//! gives operators live progress without polluting stdout (which is reserved
//! for the structured `--json` envelope).
//!
//! What: `ProgressReporter` is a tiny stderr-printer with three lifecycle
//! hooks (`phase_start`, `phase_done`, `phase_failed`) plus a `wave_start` /
//! `wave_done` pair for the code phase's per-wave loop and a final
//! `workflow_done` summary. Output is pure stderr `eprintln!` — no logging
//! framework, no allocations on the hot path beyond the formatted line.
//!
//! Test: see unit tests at the bottom of this file:
//! - `progress_reporter_formats_phase_start`
//! - `progress_reporter_formats_phase_done_no_color`

use std::io::IsTerminal;
use std::time::{Duration, Instant};

/// ANSI color helpers used when stderr is a TTY. Plain strings otherwise.
///
/// Why: Colorizing only when the output is interactive avoids dumping escape
/// codes into log files / CI captures while still giving humans a readable
/// stream when running locally.
/// What: `wrap` returns either `\x1b[<code>m{}\x1b[0m` or the original string
/// based on `use_color`.
fn wrap(use_color: bool, code: &str, s: &str) -> String {
    if use_color {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

const COLOR_BOLD: &str = "1";
const COLOR_DIM: &str = "2";
const COLOR_GREEN: &str = "32";
const COLOR_RED: &str = "31";
const COLOR_CYAN: &str = "36";

/// Streams human-readable progress lines to stderr while a workflow runs.
///
/// Why: A purpose-built reporter (vs. ad-hoc `eprintln!`s sprinkled in the
/// engine) keeps the formatting in one place so the terminal output stays
/// consistent across phases, waves, and failure paths.
/// What: Stores the workflow start `Instant` so `workflow_done` can compute a
/// total elapsed, plus a `use_color` flag captured once at construction.
/// Test: `progress_reporter_formats_phase_start`,
/// `progress_reporter_formats_phase_done_no_color`.
pub struct ProgressReporter {
    #[allow(dead_code)]
    start: Instant,
    use_color: bool,
}

impl Default for ProgressReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressReporter {
    /// Construct a reporter, detecting whether stderr is a TTY for color.
    ///
    /// Why: Avoid emitting ANSI codes when stderr is piped/captured.
    /// What: Captures `Instant::now()` and `std::io::stderr().is_terminal()`.
    /// Test: Indirect via formatting tests (force-disable color via `with_color`).
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            use_color: std::io::stderr().is_terminal(),
        }
    }

    /// Force-set the color flag (used by tests for deterministic output).
    #[allow(dead_code)]
    pub fn with_color(mut self, use_color: bool) -> Self {
        self.use_color = use_color;
        self
    }

    /// Format a phase-start line. Public for testing; production paths call
    /// `phase_start` which prints to stderr.
    pub fn format_phase_start(&self, phase: &str) -> String {
        let arrow = wrap(self.use_color, COLOR_CYAN, "▶");
        let arrow = wrap(self.use_color, COLOR_DIM, &arrow);
        let name = wrap(self.use_color, COLOR_BOLD, &format!("{phase:<10}"));
        format!("[open-mpm] {arrow} {name} starting…")
    }

    /// Print "phase starting" to stderr.
    pub fn phase_start(&self, phase: &str) {
        eprintln!("\r{}", self.format_phase_start(phase));
    }

    /// Format a phase-done line.
    pub fn format_phase_done(
        &self,
        phase: &str,
        elapsed: Duration,
        cost: f32,
        note: Option<&str>,
    ) -> String {
        let check = wrap(self.use_color, COLOR_GREEN, "✓");
        let name = wrap(self.use_color, COLOR_BOLD, &format!("{phase:<10}"));
        let stats = format!("({}, ~${:.2})", format_duration(elapsed), cost);
        let stats = wrap(self.use_color, COLOR_DIM, &stats);
        match note {
            Some(n) if !n.is_empty() => {
                format!("[open-mpm] {check} {name} done  {stats} — {n}")
            }
            _ => format!("[open-mpm] {check} {name} done  {stats}"),
        }
    }

    /// Print "phase done" to stderr, with elapsed time and cost.
    pub fn phase_done(&self, phase: &str, elapsed: Duration, cost: f32, note: Option<&str>) {
        eprintln!("\r{}", self.format_phase_done(phase, elapsed, cost, note));
    }

    /// Print "phase failed" to stderr with elapsed time and a short error.
    pub fn phase_failed(&self, phase: &str, elapsed: Duration, error: &str) {
        let cross = wrap(self.use_color, COLOR_RED, "✗");
        let name = wrap(self.use_color, COLOR_BOLD, &format!("{phase:<10}"));
        let stats = wrap(
            self.use_color,
            COLOR_DIM,
            &format!("({})", format_duration(elapsed)),
        );
        // Truncate long errors to keep the line readable.
        let short = error.lines().next().unwrap_or(error);
        let short: String = short.chars().take(120).collect();
        eprintln!("\r[open-mpm] {cross} {name} failed {stats} — {short}");
    }

    /// Print a "code wave starting" line for the wave-loop branch of the code
    /// phase. The wave loop runs sequentially across multiple files; surfacing
    /// per-wave progress prevents the code phase from looking stuck.
    pub fn wave_start(&self, wave_index: usize, wave_total: usize, file_count: usize) {
        let arrow = wrap(self.use_color, COLOR_DIM, "▶");
        let name = wrap(self.use_color, COLOR_BOLD, "code      ");
        eprintln!(
            "\r[open-mpm] {arrow} {name} wave {wave_index}/{wave_total}  ({file_count} file{plural})",
            plural = if file_count == 1 { "" } else { "s" },
        );
    }

    /// Print a "code wave done" line. Symmetric counterpart to `wave_start`.
    pub fn wave_done(&self, wave_index: usize, wave_total: usize, elapsed: Duration) {
        let check = wrap(self.use_color, COLOR_GREEN, "✓");
        let name = wrap(self.use_color, COLOR_BOLD, "code      ");
        let stats = wrap(
            self.use_color,
            COLOR_DIM,
            &format!("({})", format_duration(elapsed)),
        );
        eprintln!("\r[open-mpm] {check} {name} wave {wave_index}/{wave_total} done {stats}");
    }

    /// Print the final workflow-complete summary line.
    pub fn workflow_done(&self, total_elapsed: Duration, total_cost: f32) {
        let check = wrap(self.use_color, COLOR_GREEN, "✓");
        let name = wrap(self.use_color, COLOR_BOLD, "workflow  ");
        let stats = wrap(
            self.use_color,
            COLOR_DIM,
            &format!(
                "({}, ${:.2} total)",
                format_duration(total_elapsed),
                total_cost
            ),
        );
        eprintln!("\r[open-mpm] {check} {name} complete  {stats}");
    }

    /// Workflow-level start time accessor (used by callers that want to show
    /// the wall-clock elapsed alongside per-phase numbers).
    #[allow(dead_code)]
    pub fn workflow_started(&self) -> Instant {
        self.start
    }
}

/// Format a `Duration` as `MmSSs` for short runs and `Hh MmSSs` for long ones.
///
/// Why: Operators care about seconds for sub-minute phases and want to see
/// minutes/seconds for long ones. Strict `secs.millis` is not friendly when a
/// phase runs for several minutes.
/// What: `42s`, `3m12s`, `1h05m12s`.
/// Test: `format_duration_renders_human_friendly`.
pub fn format_duration(d: Duration) -> String {
    let total = d.as_secs();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}h{m:02}m{s:02}s")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_reporter_formats_phase_start() {
        let r = ProgressReporter::new().with_color(false);
        let s = r.format_phase_start("research");
        assert!(s.contains("research"), "got: {s}");
        assert!(s.contains("starting"), "got: {s}");
        assert!(s.starts_with("[open-mpm]"), "got: {s}");
        // No ANSI when color disabled.
        assert!(!s.contains("\x1b["), "expected no ANSI in: {s}");
    }

    #[test]
    fn progress_reporter_formats_phase_done_no_color() {
        let r = ProgressReporter::new().with_color(false);
        let s = r.format_phase_done("plan", Duration::from_secs(18), 0.04, None);
        assert!(s.contains("plan"), "got: {s}");
        assert!(s.contains("done"), "got: {s}");
        assert!(s.contains("18s"), "got: {s}");
        assert!(s.contains("$0.04"), "got: {s}");
        assert!(!s.contains("\x1b["), "expected no ANSI in: {s}");
    }

    #[test]
    fn progress_reporter_phase_done_includes_note() {
        let r = ProgressReporter::new().with_color(false);
        let s = r.format_phase_done("qa", Duration::from_secs(28), 0.09, Some("35/35 passed"));
        assert!(s.contains("35/35 passed"), "got: {s}");
    }

    #[test]
    fn progress_reporter_uses_color_when_enabled() {
        let r = ProgressReporter::new().with_color(true);
        let s = r.format_phase_start("research");
        assert!(s.contains("\x1b["), "expected ANSI in: {s}");
    }

    #[test]
    fn format_duration_renders_human_friendly() {
        assert_eq!(format_duration(Duration::from_secs(42)), "42s");
        assert_eq!(format_duration(Duration::from_secs(192)), "3m12s");
        assert_eq!(format_duration(Duration::from_secs(3912)), "1h05m12s");
        assert_eq!(format_duration(Duration::from_secs(0)), "0s");
    }
}
