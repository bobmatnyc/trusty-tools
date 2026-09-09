//! Shared ratatui terminal setup/teardown for every full-screen TUI surface.
//!
//! Why: three surfaces now enter raw mode and the alternate screen — the
//! coordinator chat, the `project_ctl` multipane dashboard, and (#7224) the
//! `tm ls` session TUI. The first two each carried their own private
//! `TerminalGuard` with byte-identical `Drop` bodies. A third copy would make
//! the restore sequence a thing to keep in step across three files instead of
//! one, and leaving a terminal in raw mode is the worst failure any of them can
//! have: the operator's shell stops echoing and Ctrl-C stops working.
//!
//! What: [`TerminalGuard`] is the `Drop`-based safety net (normal return AND
//! panic unwind); [`enter`] builds a ratatui terminal on stdout already in raw
//! mode and on the alternate screen; [`leave`] hands the real screen back
//! WITHOUT dropping the terminal, so a caller can shell out (a tmux attach,
//! say) and then [`enter`] again.
//!
//! Test: `terminal_guard_drop_is_idempotent`,
//! `enter_with_restores_the_terminal_when_the_screen_fails`,
//! `enter_with_leaves_the_terminal_alone_when_raw_mode_fails` and
//! `term_supports_raw_mode_rejects_dumb_and_missing` in `super::tests`; the
//! escape sequences themselves are side-effect-only and verified by launching a
//! TUI.

use std::io::{self, Stdout};

use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

/// The ratatui terminal type every full-screen surface in this crate uses.
pub type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Restores cooked mode, the main screen, and the cursor however a TUI exits.
///
/// Why: an early `?` return or a panic unwind would otherwise leave the
/// operator's shell in raw mode on the alternate screen. A `Drop` guard covers
/// both paths that a trailing teardown call misses.
/// What: hold one for the lifetime of the TUI. Its `Drop` is best-effort and
/// idempotent — running it after [`leave`] has already restored the screen is a
/// no-op in practice, which is what lets a suspend/resume cycle keep the guard
/// installed across the hand-off.
/// Test: `terminal_guard_drop_is_idempotent`.
pub struct TerminalGuard;

impl Drop for TerminalGuard {
    /// Why: see [`TerminalGuard`] — runs on normal return and on panic unwind.
    /// What: best-effort restore of cooked mode, the main screen, and the
    /// cursor; every step ignores its error because a `Drop` cannot propagate
    /// one and a partial restore is still better than none.
    /// Test: side-effect-only teardown; covered by launching a TUI.
    fn drop(&mut self) {
        restore();
    }
}

/// Restore cooked mode, the main screen, and the cursor — best effort.
///
/// Why: the one place the restore sequence is written, so the `Drop` guard and
/// the explicit [`leave`] can never drift apart on the order of operations.
/// What: `disable_raw_mode`, then `LeaveAlternateScreen` + `Show` on stdout;
/// every error is dropped for the reason [`TerminalGuard::drop`] gives.
/// Test: side-effect-only; covered by launching a TUI.
fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
}

/// May a full-screen TUI take this `TERM` over, or must the caller print statically?
///
/// Why: a `dumb` (or absent) `TERM` has no cursor addressing, so a redraw smears
/// the frame down the screen instead of repainting it — and raw mode has already
/// taken the operator's echo away by then. One predicate so every gate that
/// guards [`enter`] answers this the same way; a second copy is how `tm ls` and
/// `tm f` came to disagree (#7224).
/// What: true only for a `TERM` that is present, non-empty, and not `dumb`
/// (case-insensitively). `NO_COLOR` is deliberately not consulted: it suppresses
/// color, not cursor addressing.
/// Test: `term_supports_raw_mode_rejects_dumb_and_missing`,
/// `term_supports_raw_mode_accepts_a_real_terminal`.
pub fn term_supports_raw_mode(term: Option<&str>) -> bool {
    matches!(term, Some(t) if !t.is_empty() && !t.eq_ignore_ascii_case("dumb"))
}

/// Enter raw mode + the alternate screen and build the ratatui terminal.
///
/// Why: every caller wants the same four steps in the same order, and getting
/// the order wrong (building the backend before the alternate screen) leaves a
/// frame of the shell's own scrollback painted over.
/// What: `enable_raw_mode`, `EnterAlternateScreen` on stdout, then a
/// `CrosstermBackend` terminal over that same stdout — atomically, so a failure
/// at either later step restores the terminal before propagating (#7224).
/// Callers construct their [`TerminalGuard`] only after this returns `Ok`, so
/// until then this function is the only thing that can undo the raw mode it
/// just took.
/// Test: the atomicity by `enter_with_restores_the_terminal_when_the_screen_fails`
/// and `enter_with_leaves_the_terminal_alone_when_raw_mode_fails`; the real
/// escape sequences are side-effect-only and covered by launching a TUI.
pub fn enter() -> io::Result<TuiTerminal> {
    enter_with(io::stdout(), enable_raw_mode, restore)
}

/// [`enter`] with its two terminal side effects injected, so failure is testable.
///
/// Why: crossterm's raw-mode calls act on the process's real controlling
/// terminal, which a unit test can neither fake nor safely disturb. Taking the
/// screen writer, the raw-mode enable, and the restore as parameters lets a test
/// hand in a writer that fails on `EnterAlternateScreen` and then assert the
/// restore actually ran — the property that was missing (#7224).
/// What: enables raw mode, enters the alternate screen on `screen`, and builds
/// the backend over it. On a failure at either step AFTER raw mode engaged it
/// calls `restore_terminal` and propagates the error; a failure of `enable`
/// itself changed nothing, so nothing is restored. The step-3 restore goes
/// through `restore_terminal` rather than `screen` because `Terminal::new`
/// consumes the writer on the error path.
/// Test: `enter_with_restores_the_terminal_when_the_screen_fails`,
/// `enter_with_leaves_the_terminal_alone_when_raw_mode_fails`.
pub(super) fn enter_with<W: io::Write>(
    mut screen: W,
    enable: impl FnOnce() -> io::Result<()>,
    restore_terminal: impl FnOnce(),
) -> io::Result<Terminal<CrosstermBackend<W>>> {
    enable()?;
    if let Err(e) = execute!(screen, EnterAlternateScreen) {
        restore_terminal();
        return Err(e);
    }
    Terminal::new(CrosstermBackend::new(screen)).inspect_err(|_| restore_terminal())
}

/// Hand the real screen back while keeping the terminal value alive (#7224).
///
/// Why: the `tm ls` TUI opens a session by shelling out to `tmux attach`, which
/// needs a cooked terminal on the MAIN screen — and control comes back here
/// when the operator detaches, at which point the TUI has to reappear. A
/// `Drop`-only teardown cannot express that: it ends the terminal for good.
/// What: `disable_raw_mode`, then `LeaveAlternateScreen` + `Show` through the
/// terminal's own backend, so the buffered ratatui state stays intact and
/// [`resume`] can pick it back up. Errors propagate: unlike the `Drop` path,
/// this one has a caller that can report them.
/// Test: side-effect-only; the hand-off is verified by the tmux smoke run.
pub fn suspend(terminal: &mut TuiTerminal) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        crossterm::cursor::Show
    )
}

/// Take the screen back after a [`suspend`] (#7224).
///
/// Why: the operator detached from the tmux session the TUI handed them to, so
/// control is back here and the list has to reappear where it was.
/// What: re-enters raw mode and the alternate screen through the SAME backend,
/// then clears ratatui's cached buffer — without the clear, the first frame
/// diffs against what the pre-suspend screen held and paints nothing.
/// Test: side-effect-only; verified by the tmux smoke run.
pub fn resume(terminal: &mut TuiTerminal) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()
}
