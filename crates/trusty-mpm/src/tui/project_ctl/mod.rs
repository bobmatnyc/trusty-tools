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
mod drive;
pub mod events;
pub mod layout;
pub mod panes;
pub mod poll;
pub mod state;
mod trace;

#[cfg(test)]
mod tests;

use std::io::Stdout;
use std::time::Duration;

use ratatui::{Terminal, backend::CrosstermBackend};

use crate::client::DaemonClient;
use poll::project_ctl_poll_daemon;
use state::ProjectCtlState;
use trace::TuiTrace;

// #7224: the panic-safe terminal guard moved to `tui::terminal`, shared with
// the coordinator screen and the `tm ls` session TUI.
use crate::tui::terminal::TerminalGuard;

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
    run_focused(url, interval_ms, None).await
}

/// Launch the multipane dashboard, optionally pre-selecting one session.
///
/// Why (#6483): `tm tui` and `tm attach <target>` now open THIS surface by
/// default instead of the single-pane coordinator chat. `tm attach` resolves a
/// session id first and expects the TUI to open on that session, so the entry
/// point needs the same `focus_id` seam `tui::run_focused` already had.
/// What: same client/prime/terminal-guard sequence as [`run`]; after the
/// priming poll it calls [`ProjectCtlState::focus_session`], which selects the
/// owning project and session row (and is a no-op for an id the fleet does not
/// know). A missed focus sets a notice rather than failing the launch — the
/// session may simply not have been polled yet.
/// Test: the focus selection itself is unit-tested
/// (`focus_session_selects_owning_project_and_row`); this terminal glue is
/// exercised by launching the TUI.
pub async fn run_focused(
    url: String,
    interval_ms: u64,
    focus_id: Option<String>,
) -> anyhow::Result<()> {
    run_focused_with_client(DaemonClient::new(url), interval_ms, focus_id).await
}

/// [`run_focused`] against a caller-built client, so a pinned URL (#9556)
/// reaches the poll loop.
pub(crate) async fn run_focused_with_client(
    mut client: DaemonClient,
    interval_ms: u64,
    focus_id: Option<String>,
) -> anyhow::Result<()> {
    let mut state = ProjectCtlState::default();
    project_ctl_poll_daemon(&mut state, &mut client).await;
    if let Some(id) = focus_id.as_deref() {
        // #6483: a `tm attach` target that the priming poll has not surfaced
        // yet leaves the selection alone and says so, rather than silently
        // opening on an unrelated project.
        if state.focus_session(id) {
            state.set_notice(format!("attached to {id}"));
        } else {
            state.set_notice(format!("session {id} not in the polled fleet yet"));
        }
    }

    // The guard restores the terminal on every exit path — normal return AND
    // panic unwind — so it is the SOLE teardown (no manual cleanup follows
    // `run_loop`).
    // #7224: setup and guard both come from `tui::terminal`.
    let mut terminal = crate::tui::terminal::enter()?;
    let _guard = TerminalGuard;

    run_loop(&mut terminal, &mut client, state, interval_ms).await
    // `_guard` drops here (or during an unwind), restoring the terminal.
}

/// Run the projects view inside a terminal the caller already owns.
///
/// Why (#10491): the console home screen opens this view on Enter and gets
/// the terminal back when the operator leaves it, so it must not enter or
/// restore the terminal itself.
/// What: the priming poll [`run_focused_with_client`] does, then [`run_loop`]
/// until `q` / Ctrl-C.
/// Test: exercised by the home-screen smoke run; the loop is unchanged.
pub(crate) async fn run_in_terminal(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: &mut DaemonClient,
    interval_ms: u64,
) -> anyhow::Result<()> {
    let mut state = ProjectCtlState::default();
    project_ctl_poll_daemon(&mut state, client).await;
    terminal.clear()?;
    run_loop(terminal, client, state, interval_ms).await
}

/// The live render + poll + input loop.
///
/// Why: kept separate from [`run`] so terminal setup/teardown wraps it
/// cleanly; the loop itself lives in [`drive::drive`] so a test can drive it
/// without a terminal.
/// What: binds [`drive::drive`] to this terminal's draw, the crossterm event
/// source, the off-loop [`poll::tick::fetch_tick`] (#10476) and the
/// `TM_TUI_TRACE` trace.
/// Test: `keypress_redraws_while_poll_in_flight_10476` drives the same loop.
async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: &mut DaemonClient,
    state: ProjectCtlState,
    interval_ms: u64,
) -> anyhow::Result<()> {
    let mut draw = |state: &mut ProjectCtlState| -> anyhow::Result<()> {
        terminal.draw(|frame| layout::render(frame, state))?;
        Ok(())
    };
    drive::drive(
        &mut draw,
        &mut drive::TerminalEvents,
        poll::tick::fetch_tick,
        client,
        state,
        Duration::from_millis(interval_ms),
        &mut TuiTrace::from_env(),
    )
    .await
}
