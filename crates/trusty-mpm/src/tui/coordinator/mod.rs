//! Coordinator TUI — a Claude-Code-like input box over a live session list.
//!
//! Why: operators want one conversational surface to talk to a coordinator and
//! watch a fleet of sessions (DOC-13). This module is the NEW coordinator screen
//! built alongside (not on top of) the existing `tui/dashboard` — keeping it
//! separate avoids touching the at-cap dashboard/health files. Child #1 lands
//! the skeleton: layout, selection, input editing, and a panic-safe terminal.
//! What: thin façade re-exporting the submodules and exposing [`run`], the
//! `tm coordinator-tui` entry point. Live polling, the daemon-backed
//! `last_summary` column, and slash-command dispatch are deferred to later
//! children; Child #1 renders static placeholder rows.
//! Test: the pure pieces (rows, state, events, layout text) are unit-tested in
//! [`tests`]; [`run`] is the thin terminal glue exercised by launching the TUI.

pub mod events;
pub mod layout;
pub mod rows;
pub mod state;

#[cfg(test)]
mod tests;

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use events::handle_key;
use state::CoordinatorState;

/// RAII guard that restores the terminal on drop — including during a panic.
///
/// Why: the original `run` did its teardown (disable raw mode, leave the
/// alternate screen, show the cursor) only AFTER `run_loop` returned, so a
/// panic anywhere in the loop unwound straight past the cleanup and left the
/// operator's terminal in raw mode + the alternate screen (no echo, no prompt).
/// A `Drop` impl runs on BOTH the normal return and the unwind path, so the
/// terminal is always restored. The dashboard's `run`/`run_focused` still use
/// the older sequential teardown; this is the panic-safe pattern for the new
/// coordinator screen.
/// What: constructed right after entering raw mode + the alternate screen; its
/// `Drop` best-effort runs `disable_raw_mode`, `LeaveAlternateScreen`, and
/// `Show` (the cursor) against stdout, ignoring errors (nothing useful can be
/// done while unwinding). Idempotent enough to be the SOLE teardown path.
/// Test: terminal glue is exercised by launching the TUI; `Drop` cannot be
/// unit-tested without a real terminal, so correctness rests on it being the
/// only teardown seam (no manual cleanup duplicates it).
struct TerminalGuard;

impl Drop for TerminalGuard {
    /// Why: see [`TerminalGuard`] — runs on normal return and on panic unwind.
    /// What: best-effort restore of cooked mode, the main screen, and the
    /// cursor; every step ignores its error because a `Drop` cannot propagate
    /// one and a partial restore is still better than none.
    /// Test: side-effect-only teardown; covered by launching the TUI.
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

/// How long [`run`] blocks waiting for a key before redrawing.
///
/// Why: a short poll keeps input responsive while still yielding the CPU; the
/// skeleton has no live data, so the interval only bounds redraw latency.
/// What: 50ms, matching the dashboard's input cadence.
const KEY_POLL: Duration = Duration::from_millis(50);

/// Launch the coordinator TUI against `url`.
///
/// Why: the `tm coordinator-tui` subcommand needs one entry point that owns the
/// terminal lifecycle. Kept synchronous (no daemon IO in Child #1) so the
/// caller does not need an async context for the skeleton.
/// What: enters raw mode + the alternate screen, installs a [`TerminalGuard`]
/// whose `Drop` restores the terminal, then runs the event loop. Because the
/// guard restores on unwind as well as on normal return, a panic or early exit
/// never leaves the operator's terminal in raw mode / the alternate screen.
/// `url` and `interval_ms` are accepted now (and threaded into state via the
/// signature) so Child #2 can wire live polling without a signature change.
/// Test: terminal glue is exercised by launching the TUI; the loop's pure
/// pieces (key dispatch, layout text) are unit-tested.
pub fn run(url: String, interval_ms: u64) -> anyhow::Result<()> {
    // `url` / `interval_ms` are not consumed by the skeleton (no live poll yet);
    // logging them keeps the launch auditable and documents the deferred wiring.
    tracing::info!(%url, interval_ms, "launching coordinator TUI (skeleton)");

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // From here on the terminal is in raw mode + the alternate screen; the guard
    // restores both on every exit path — normal return AND panic unwind — so it
    // is the SOLE teardown (no manual cleanup follows `run_loop`).
    let _guard = TerminalGuard;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    run_loop(&mut terminal)
    // `_guard` drops here (or during an unwind), restoring the terminal.
}

/// The skeleton render + input loop.
///
/// Why: kept separate from [`run`] so terminal setup/teardown wraps it cleanly
/// and the loop body stays focused on draw → poll-key → dispatch.
/// What: seeds [`CoordinatorState::skeleton`], then each iteration draws the
/// frame, polls the keyboard for [`KEY_POLL`], dispatches any key via
/// [`handle_key`], and exits once `should_exit` is set (`q` / Ctrl-C).
/// Test: the dispatch and layout text are unit-tested; this terminal-bound loop
/// is exercised by launching the TUI.
fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> anyhow::Result<()> {
    let mut state = CoordinatorState::skeleton();
    loop {
        terminal.draw(|frame| layout::render(frame, &state))?;

        if event::poll(KEY_POLL)?
            && let Event::Key(key) = event::read()?
        {
            handle_key(&mut state, key);
            if state.should_exit {
                return Ok(());
            }
        }
    }
}
