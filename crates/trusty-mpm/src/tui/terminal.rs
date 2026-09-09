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
//! Test: `terminal_guard_drop_is_idempotent` in `super::tests`; the escape
//! sequences themselves are side-effect-only and verified by launching a TUI.

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

/// Enter raw mode + the alternate screen and build the ratatui terminal.
///
/// Why: every caller wants the same four steps in the same order, and getting
/// the order wrong (building the backend before the alternate screen) leaves a
/// frame of the shell's own scrollback painted over.
/// What: `enable_raw_mode`, `EnterAlternateScreen` on stdout, then a
/// `CrosstermBackend` terminal over that same stdout. The caller is responsible
/// for holding a [`TerminalGuard`] so a failure after this point still restores.
/// Test: side-effect-only; covered by launching a TUI.
pub fn enter() -> io::Result<TuiTerminal> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
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
