//! `tm projects` 4-pane TUI skeleton — the project control plane (#2118, DOC-35 §5).
//!
//! Why: operators managing several projects' worth of managed sessions need
//! one screen that shows every registered project's aggregate health
//! (Projects pane), drills into one project's sessions (Sessions pane), and
//! previews the focused session's static record fields (Activity pane
//! skeleton — live `/activity` polling is #2119) — with the lifecycle verbs
//! (launch/kill/resume/decommission/attach) one keystroke away. This is the
//! FULL 4-pane layout landing as v1 (the owner resolved #2118 to skip a
//! 2-pane MVP).
//! What: [`run`] is the `tm projects` (bare, TTY) entry point — it primes the
//! state with one poll, then hands off to [`run_loop`] with a panic-safe
//! terminal guard (mirroring `tui::coordinator::run`'s `TerminalGuard`
//! pattern, since this screen also needs to restore the terminal on an
//! unexpected panic, not just a clean return). The event loop itself follows
//! the same draw → poll-key → dispatch → maybe-repoll shape as
//! `tui::coordinator::run_loop`.
//! Test: the pure pieces (state, poll projections, key routing, pane content)
//! are unit-tested in their own modules; this terminal-bound loop is
//! exercised by launching the TUI.

pub mod actions;
pub mod events;
pub mod layout;
pub mod panes;
pub mod poll;
pub mod state;

#[cfg(test)]
mod tests;

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::client::DaemonClient;
use events::handle_key;
use poll::project_ctl_poll_daemon;
use state::ProjectCtlState;

/// RAII guard that restores the terminal on drop — including during a panic.
///
/// Why: mirrors `tui::coordinator::TerminalGuard` — a `Drop` impl runs on both
/// the normal return and the unwind path, so a panic anywhere in the loop
/// never leaves the operator's terminal in raw mode / the alternate screen.
/// What: constructed right after entering raw mode + the alternate screen;
/// its `Drop` best-effort restores cooked mode, the main screen, and the
/// cursor, ignoring errors (nothing useful can be done while unwinding).
/// Test: side-effect-only teardown; covered by launching the TUI.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

/// How long [`run_loop`] blocks waiting for a key before redrawing.
///
/// Why: matches the coordinator/dashboard screens' input cadence — short
/// enough to feel responsive, independent of the daemon refresh cadence.
const KEY_POLL: Duration = Duration::from_millis(50);

/// Launch the `tm projects` 4-pane TUI against `url`, polling every `interval_ms`.
///
/// Why: the bare-`tm projects`-on-a-TTY entry point (#2118); `main.rs`'s
/// bare-dispatch branch calls this after confirming stdout is a TTY.
/// What: builds a [`DaemonClient`] for `url`, primes the state with one poll
/// (so the first frame is never an empty flash), enters raw mode + the
/// alternate screen, installs a [`TerminalGuard`], then runs [`run_loop`] on
/// the `interval_ms` cadence.
/// Test: terminal glue is exercised by launching the TUI; the loop's pure
/// pieces are unit-tested in their own modules.
pub async fn run(url: String, interval_ms: u64) -> anyhow::Result<()> {
    let mut client = DaemonClient::new(url);
    let mut state = ProjectCtlState::default();
    project_ctl_poll_daemon(&mut state, &mut client).await;

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // The guard restores the terminal on every exit path — normal return AND
    // panic unwind — so it is the SOLE teardown (no manual cleanup follows
    // `run_loop`).
    let _guard = TerminalGuard;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    run_loop(&mut terminal, &mut client, state, interval_ms).await
    // `_guard` drops here (or during an unwind), restoring the terminal.
}

/// The live render + poll + input loop.
///
/// Why: kept separate from [`run`] so terminal setup/teardown wraps it
/// cleanly and the loop body stays focused on draw → poll-key → dispatch →
/// maybe-repoll.
/// What: each iteration draws the frame, polls the keyboard for [`KEY_POLL`]
/// and routes any key through [`handle_key`], awaiting
/// [`actions::dispatch`] for any returned [`events::PendingAction`]. Re-polls
/// the daemon immediately after a successful mutating action (via
/// [`ProjectCtlState::take_repoll`]) or once at least `interval_ms` has
/// elapsed since the last poll. Exits once `should_exit` is set (`q` /
/// Ctrl-C).
async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: &mut DaemonClient,
    mut state: ProjectCtlState,
    interval_ms: u64,
) -> anyhow::Result<()> {
    let interval = Duration::from_millis(interval_ms);
    let mut last_poll = Instant::now();

    loop {
        terminal.draw(|frame| layout::render(frame, &mut state))?;

        if event::poll(KEY_POLL)?
            && let Event::Key(key) = event::read()?
        {
            if let Some(action) = handle_key(&mut state, key) {
                actions::dispatch(&mut state, client, action).await;
            }
            if state.should_exit {
                return Ok(());
            }
        }

        // A requested repoll (post-mutation) or the timer elapsing both mean
        // the same thing: refresh now and restart the interval clock. Merged
        // into one branch (rather than an if/else-if with duplicate bodies)
        // to satisfy clippy::if_same_then_else.
        if state.take_repoll() || last_poll.elapsed() >= interval {
            project_ctl_poll_daemon(&mut state, client).await;
            last_poll = Instant::now();
        }
    }
}
