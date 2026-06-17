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

use std::io::Stdout;
use std::time::Duration;

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use events::handle_key;
use state::CoordinatorState;

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
/// What: enters raw mode + the alternate screen, runs the event loop, and
/// ALWAYS restores the terminal afterward — even if the loop returns an error —
/// so a panic or early exit never leaves the operator's terminal corrupted.
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
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal);

    // Always restore the terminal, even on error (panic-safe boundary).
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
    result
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
