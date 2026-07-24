//! In-place, per-component live progress checklist (`indicatif::MultiProgress`).
//!
//! Why: A from-scratch install runs several components in sequence; scrolling
//! `info:` lines give no sense of "what's happening right now" versus "what's
//! done" during a live demo. rustup-style tools solve this with one line per
//! item that updates in place. [`LiveChecklist`] wraps `indicatif::MultiProgress`
//! so a caller gets that behaviour for free, while degrading automatically (via
//! the shared [`Output`] draw target) to a fully hidden, zero-byte no-op in any
//! non-interactive mode ([`Mode::Plain`] / [`Mode::Silent`]) — the same policy
//! [`crate::ProgressHandle`] already uses for a single spinner/bar.
//!
//! What: [`LiveChecklist::new`] pre-renders one spinner row per named component
//! (state [`ComponentState::Pending`]); [`LiveChecklist::set`] transitions a
//! named row through [`ComponentState::Downloading`] / `Verifying` and finally
//! one of the three terminal states (`Installed` / `Skipped` / `Failed`), after
//! which the row stops animating and stays put. [`LiveChecklist::note`] prints a
//! line ABOVE the active bars (via `MultiProgress::println`) for the rare
//! mid-loop message that must interleave without corrupting the redraw region —
//! callers should prefer collecting notes and printing them after the checklist
//! finishes wherever the message can wait.
//!
//! Test: `live::tests` cover row construction, state-label rendering, and the
//! hidden-in-plain-mode degrade (mirroring `progress::tests`).

use std::collections::HashMap;
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use crate::output::Output;

/// How often an in-progress (non-terminal) row's spinner ticks.
const TICK: Duration = Duration::from_millis(80);
/// Template for a live row: a ticking spinner glyph + the status text.
const TEMPLATE: &str = "{spinner:.cyan} {msg}";

/// One component's lifecycle state as the install loop drives it forward.
///
/// Why: A typed state (rather than ad-hoc strings at each call site) keeps the
/// glyph/label/terminal-ness rules in one place and makes the intended
/// progression (`Pending -> Downloading -> Verifying -> <terminal>`) explicit
/// at the type level.
/// What: `Pending`/`Downloading`/`Verifying` are in-flight (still spinning);
/// `Installed`/`Skipped`/`Failed` are terminal (the row stops animating and a
/// fixed glyph replaces the spinner). `Skipped`/`Failed` carry a short reason.
/// Test: `tests::state_label_and_terminal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentState {
    /// Not started yet.
    Pending,
    /// Prebuilt download (or `cargo install` fallback) in flight.
    Downloading,
    /// Post-download health gate / verification in flight.
    Verifying,
    /// Landed and health-gated successfully.
    Installed,
    /// Not installed, but by design (e.g. an optional member with no
    /// prebuilt for this platform) — never a failure.
    Skipped(String),
    /// Installed attempt failed.
    Failed(String),
}

impl ComponentState {
    /// Why: The terminal states get a fixed, non-animated glyph so the row's
    /// final look is immediately scannable; in-flight states show the ticking
    /// spinner instead (handled by the template), so they need no glyph here.
    /// What: Returns the leading glyph for a terminal state, or an empty
    /// string for an in-flight one (the spinner already occupies that slot).
    /// Test: `tests::state_label_and_terminal`.
    fn glyph(&self) -> &'static str {
        match self {
            ComponentState::Pending | ComponentState::Downloading | ComponentState::Verifying => "",
            ComponentState::Installed => "✓",
            ComponentState::Skipped(_) => "-",
            ComponentState::Failed(_) => "✗",
        }
    }

    /// Why: The human-readable status word/phrase shown after the component
    /// name; centralised so `new`/`set` cannot drift on wording.
    /// What: Returns e.g. `"pending"`, `"downloading"`, `"failed (<reason>)"`.
    /// Test: `tests::state_label_and_terminal`.
    fn label(&self) -> String {
        match self {
            ComponentState::Pending => "pending".to_owned(),
            ComponentState::Downloading => "downloading".to_owned(),
            ComponentState::Verifying => "verifying".to_owned(),
            ComponentState::Installed => "installed".to_owned(),
            ComponentState::Skipped(reason) => format!("skipped ({reason})"),
            ComponentState::Failed(reason) => format!("failed ({reason})"),
        }
    }

    /// Why: A terminal row must stop ticking (`finish_with_message`) instead
    /// of continuing to animate; this is the single predicate both `new`
    /// (defensive) and `set` branch on.
    /// What: `true` for `Installed`/`Skipped`/`Failed`.
    /// Test: `tests::state_label_and_terminal`.
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            ComponentState::Installed | ComponentState::Skipped(_) | ComponentState::Failed(_)
        )
    }
}

/// A live, in-place checklist: one row per component, updating without
/// scrolling.
///
/// Why: `indicatif::MultiProgress` already solves in-place multi-row rendering
/// and, crucially, already knows how to fully hide itself (zero bytes, no
/// escape codes) when handed a hidden draw target — exactly the non-TTY/`--json`
/// contract [`Output::draw_target`] encodes. Wrapping it here means callers
/// never touch `indicatif` directly and cannot construct a checklist that
/// disagrees with the shared TTY/quiet/silent policy.
/// What: Pre-renders every row up front (in the given order) as `Pending`;
/// [`Self::set`] moves a named row through its states. Rows for unknown names
/// are silently ignored (defensive — a caller typo must never panic a purely
/// cosmetic display).
/// Test: `tests::rows_hidden_in_plain_mode`, `tests::unknown_name_is_a_no_op`.
pub struct LiveChecklist {
    multi: MultiProgress,
    bars: HashMap<String, ProgressBar>,
    name_width: usize,
}

impl LiveChecklist {
    /// Why: The whole checklist must exist as one block before any row
    /// transitions, so the operator sees every component's name immediately
    /// (as `pending`) rather than rows popping in one at a time.
    /// What: Builds one spinner row per entry in `names`, in order, sharing
    /// `output`'s draw target (hidden automatically in `Plain`/`Silent` mode).
    /// Test: `tests::rows_hidden_in_plain_mode`.
    pub fn new(output: &Output, names: &[String]) -> Self {
        let multi = MultiProgress::with_draw_target(output.draw_target());
        let name_width = names.iter().map(|n| n.chars().count()).max().unwrap_or(0);
        let mut bars = HashMap::with_capacity(names.len());
        for name in names {
            let bar = multi.add(ProgressBar::new_spinner());
            // SAFETY: const template string, validated at construction; see
            // `ProgressHandle::spinner`'s identical sanctioned `expect`.
            let style = ProgressStyle::with_template(TEMPLATE).expect("const template valid");
            bar.set_style(style);
            bar.set_message(Self::row_text(name, &ComponentState::Pending, name_width));
            bar.enable_steady_tick(TICK);
            bars.insert(name.clone(), bar);
        }
        Self {
            multi,
            bars,
            name_width,
        }
    }

    /// Why: Every row shares one name column width so the status text lines
    /// up as a block regardless of which component is longest.
    /// What: Renders `<glyph> <name padded> <label>` (glyph empty for
    /// in-flight states — the spinner glyph already precedes this text).
    fn row_text(name: &str, state: &ComponentState, name_width: usize) -> String {
        let glyph = state.glyph();
        let label = state.label();
        if glyph.is_empty() {
            format!("{name:<name_width$}  {label}")
        } else {
            format!("{glyph} {name:<name_width$}  {label}")
        }
    }

    /// Why: The install loop drives one row forward as work happens; this is
    /// the single mutation verb.
    /// What: Updates the named row's text for `state`; finishes (stops
    /// ticking) the row when `state` is terminal. A name with no matching row
    /// is a silent no-op (see struct doc).
    /// Test: `tests::unknown_name_is_a_no_op` (panic-free); the visible text
    /// is exercised via `row_text` directly in `tests::state_label_and_terminal`.
    pub fn set(&self, name: &str, state: ComponentState) {
        let Some(bar) = self.bars.get(name) else {
            return;
        };
        let text = Self::row_text(name, &state, self.name_width);
        if state.is_terminal() {
            bar.finish_with_message(text);
        } else {
            bar.set_message(text);
        }
    }

    /// Print a line above the active rows without corrupting the redraw
    /// region (`MultiProgress::println`).
    ///
    /// Why: Writing directly to the sink while rows are actively ticking
    /// (raw `eprintln!`/`Narrator`) races indicatif's own redraws and can
    /// interleave mid-line. `MultiProgress::println` is indicatif's supported
    /// mechanism for exactly this — it suspends the redraw, prints above the
    /// bars, and resumes. Prefer collecting a note and printing it AFTER the
    /// checklist finishes; use this only when a message genuinely cannot wait.
    /// What: Emits `line` (best-effort; a write failure is not actionable for
    /// a purely cosmetic display, so it is swallowed rather than propagated).
    /// Test: Side-effect-only; covered manually (see PR description).
    pub fn note(&self, line: &str) {
        let _ = self.multi.println(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::Mode;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    /// Why: The non-TTY/`--json` safety requirement is that a live checklist
    /// degrades to fully hidden (no animation, no bytes) exactly like a single
    /// `ProgressHandle` already does — never a corrupted partial render.
    /// What: Builds a checklist in `Mode::Plain` and asserts every row's bar
    /// reports hidden, mirroring `progress::tests::spinner_is_hidden_in_plain_mode`.
    /// Test: This is the test.
    #[test]
    fn rows_hidden_in_plain_mode() {
        let (out, _cap) = Output::for_capture(Mode::Plain);
        let checklist = LiveChecklist::new(&out, &names(&["trusty-search", "trusty-mpm"]));
        for bar in checklist.bars.values() {
            assert!(bar.is_hidden());
        }
    }

    /// Why: A live demo's whole point is a clean top-to-bottom read; pin the
    /// label wording and the terminal/in-flight split so a future edit cannot
    /// silently drop a state.
    /// What: Asserts each state's label text and terminal-ness.
    /// Test: This is the test.
    #[test]
    fn state_label_and_terminal() {
        assert_eq!(ComponentState::Pending.label(), "pending");
        assert_eq!(ComponentState::Downloading.label(), "downloading");
        assert_eq!(ComponentState::Verifying.label(), "verifying");
        assert_eq!(ComponentState::Installed.label(), "installed");
        assert_eq!(
            ComponentState::Skipped("no prebuilt".to_owned()).label(),
            "skipped (no prebuilt)"
        );
        assert_eq!(
            ComponentState::Failed("network error".to_owned()).label(),
            "failed (network error)"
        );
        assert!(!ComponentState::Pending.is_terminal());
        assert!(!ComponentState::Downloading.is_terminal());
        assert!(!ComponentState::Verifying.is_terminal());
        assert!(ComponentState::Installed.is_terminal());
        assert!(ComponentState::Skipped(String::new()).is_terminal());
        assert!(ComponentState::Failed(String::new()).is_terminal());
    }

    /// Why: A typo'd component name must never panic a cosmetic display.
    /// What: Calls `set` with a name that has no row and asserts it does not
    /// panic.
    /// Test: This is the test.
    #[test]
    fn unknown_name_is_a_no_op() {
        let (out, _cap) = Output::for_capture(Mode::Plain);
        let checklist = LiveChecklist::new(&out, &names(&["trusty-search"]));
        checklist.set("does-not-exist", ComponentState::Installed);
    }

    /// Why: Row text must render the padded name + label so the block aligns;
    /// guard the exact shape (glyph, spacing) since it's the visual contract.
    /// What: Asserts `row_text` output for a pending and a terminal state.
    /// Test: This is the test.
    #[test]
    fn row_text_shape() {
        let pending = LiveChecklist::row_text("short", &ComponentState::Pending, 10);
        assert_eq!(pending, "short       pending");
        let installed = LiveChecklist::row_text("short", &ComponentState::Installed, 10);
        assert_eq!(installed, "✓ short       installed");
    }

    /// Why: `note` must not panic even against a hidden (Plain-mode) multi —
    /// the mid-loop escape hatch has to be safe to call unconditionally.
    /// What: Calls `note` on a hidden checklist.
    /// Test: This is the test.
    #[test]
    fn note_is_safe_when_hidden() {
        let (out, _cap) = Output::for_capture(Mode::Plain);
        let checklist = LiveChecklist::new(&out, &names(&["trusty-search"]));
        checklist.note("above the bars");
    }
}
