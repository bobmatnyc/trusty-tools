//! Coordinator TUI — a Claude-Code-like input box over a live session list.
//!
//! Why: operators want one conversational surface to talk to a coordinator and
//! watch a fleet of sessions (DOC-13). This module is the NEW coordinator screen
//! built alongside (not on top of) the existing `tui/dashboard` — keeping it
//! separate avoids touching the at-cap dashboard/health files. Child #1 landed
//! the skeleton (layout, selection, input editing, panic-safe terminal); Child
//! #2 wired live session-list polling + daemon-down handling; STUI-1 (#1278)
//! adds the numbered, scrollable list (ratatui `ListState`), Page-Up/Page-Down,
//! selection + scroll-offset preservation across refreshes, and the
//! immediate-re-poll-after-mutation hook (see [`nav`] and
//! [`state::CoordinatorState::request_repoll`]).
//! What: thin façade re-exporting the submodules and exposing [`run`], the
//! `tm session tui` entry point. [`run`] polls the daemon's
//! coordinator-context endpoint on the `--interval-ms` cadence and maps each
//! session into the live list (see [`poll`]). The daemon-backed `last_summary`
//! column and slash-command dispatch are deferred to later children; the summary
//! is derived client-side from `recent_output` until then.
//! Test: the pure pieces (rows, state, events, layout text) are unit-tested in
//! [`tests`]; [`run`] is the thin terminal glue exercised by launching the TUI.

pub mod banner;
pub mod dispatch;
pub mod events;
pub mod layout;
pub mod nav;
pub mod poll;
pub mod render;
pub mod rows;
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

use crate::client::{CommandExecutor, DaemonClient};
use dispatch::{Dispatch, route};
use events::{handle_key, parse_slash};
use poll::coord_poll_daemon;
use render::render_result;
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

/// How long [`run_loop`] blocks waiting for a key before redrawing.
///
/// Why: a short key poll keeps input responsive while still yielding the CPU;
/// it is independent of the daemon refresh cadence (`interval_ms`), so input
/// never waits a whole poll interval to register.
/// What: 50ms, matching the dashboard's input cadence.
const KEY_POLL: Duration = Duration::from_millis(50);

/// Launch the coordinator TUI against `url`, polling every `interval_ms`.
///
/// Why: the `tm session tui` subcommand needs one entry point that owns the
/// terminal lifecycle. Now async (Child #2 added live daemon IO) so it runs on
/// the same tokio runtime as `tui::run`.
/// What: builds a [`DaemonClient`] for `url`, enters raw mode + the alternate
/// screen, installs a [`TerminalGuard`] whose `Drop` restores the terminal, then
/// runs the event loop polling the daemon on the `interval_ms` cadence. Because
/// the guard restores on unwind as well as on normal return, a panic or early
/// exit never leaves the operator's terminal in raw mode / the alternate screen.
/// Test: terminal glue is exercised by launching the TUI; the loop's pure
/// pieces (key dispatch, layout text, session mapping) are unit-tested.
pub async fn run(url: String, interval_ms: u64) -> anyhow::Result<()> {
    tracing::info!(%url, interval_ms, "launching coordinator TUI");
    let mut client = DaemonClient::new(url);

    // STUI-0: prime the fleet state with one poll BEFORE the alternate screen so
    // the startup banner can report the active-session count (or `daemon
    // unreachable` when the daemon is down). This is the same priming poll the
    // run loop would otherwise do first; doing it here lets the banner and the
    // first frame share one initial fetch.
    let mut state = CoordinatorState::live();
    coord_poll_daemon(&mut state, &mut client).await;
    let active_count = state.daemon_reachable.then_some(state.sessions.len());

    // STUI-0: render the Claude-Code-style banner (name + version, memory +
    // search probes, active-session count + `/help` hint) to stderr before the
    // alternate screen swallows the scrollback. Probes are fail-safe — a down
    // backplane shows `○ unreachable`, never blocking the TUI (DOC-16 §3.1).
    banner::print_startup_banner(None, None, active_count).await;

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // From here on the terminal is in raw mode + the alternate screen; the guard
    // restores both on every exit path — normal return AND panic unwind — so it
    // is the SOLE teardown (no manual cleanup follows `run_loop`).
    let _guard = TerminalGuard;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    run_loop(&mut terminal, &mut client, state, interval_ms).await
    // `_guard` drops here (or during an unwind), restoring the terminal.
}

/// The live render + poll + input loop.
///
/// Why: kept separate from [`run`] so terminal setup/teardown wraps it cleanly
/// and the loop body stays focused on poll → draw → poll-key → dispatch.
/// What: takes the already-primed [`CoordinatorState`] (the STUI-0 banner poll
/// in [`run`] populated it so the list is ready before the first frame), then
/// each iteration draws the frame, polls the keyboard for [`KEY_POLL`] and
/// dispatches any key via [`handle_key`], and re-polls the daemon once at least
/// `interval_ms` has elapsed since the last poll (tokio [`Instant`] timer).
/// Exits once `should_exit` is set (`q` / Ctrl-C).
/// Test: the dispatch, layout text, and session mapping are unit-tested; this
/// terminal-bound loop is exercised by launching the TUI.
async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: &mut DaemonClient,
    mut state: CoordinatorState,
    interval_ms: u64,
) -> anyhow::Result<()> {
    let interval = Duration::from_millis(interval_ms);
    // The state arrives already primed from `run`'s banner poll, so the operator
    // never sees an empty list flash before the first interval tick.
    let mut last_poll = Instant::now();

    loop {
        terminal.draw(|frame| layout::render(frame, &mut state))?;

        // NOTE: `event::poll` blocks this async task for up to `KEY_POLL`. This
        // mirrors the existing dashboard event loop (`tui/event_loop.rs`); we are
        // intentionally not diverging one screen.
        //
        // EventStream-based live refresh (STUI-8 spike): the daemon DOES expose
        // SSE, but only for *hook events* (`GET /events`,
        // `GET /sessions/{id}/events`) — there is NO push endpoint that emits the
        // `CoordinatorContext` snapshot this list renders. Wiring a live push
        // would therefore require a new daemon-side coordinator-context stream,
        // which is out of scope for STUI-1. Timer polling on `interval_ms`
        // (below) remains the refresh mechanism; the post-mutation
        // `take_repoll` hook gives operators an immediate refresh without it.
        // Converting the input pump to `crossterm::event::EventStream` +
        // `tokio::select!` is tracked as a separate follow-up.
        if event::poll(KEY_POLL)?
            && let Event::Key(key) = event::read()?
        {
            // `handle_key` mutates the buffer/selection and, on Enter, returns the
            // submitted line. Phase 1C routes that line through the chat-core
            // executor (the old stub merely echoed it).
            if let Some(line) = handle_key(&mut state, key) {
                dispatch_submitted_line(&mut state, client, &line).await;
            }
            if state.should_exit {
                return Ok(());
            }
        }

        // Immediate re-poll after a mutation (STUI-1): a list-mutating action sets
        // `needs_repoll` so the operator sees the fleet refreshed now rather than
        // up to a whole interval later. Consuming the flag also resets the timer
        // so the next scheduled poll is a full interval out from this one. The
        // trigger is already wired: `handle_key` flags a re-poll when the operator
        // submits a mutating slash command (`/new`, `/kill`, `/stop`, `/resume`).
        //
        // TODO(STUI-6): once real slash-command dispatch lands, move the
        // `request_repoll()` call so it fires only *after* a successful mutation
        // (a rejected command should not force a refresh); this branch is the
        // consumer either way.
        if state.take_repoll() {
            coord_poll_daemon(&mut state, client).await;
            last_poll = Instant::now();
        } else if last_poll.elapsed() >= interval {
            // Throttle the data refresh: only re-poll the daemon every interval_ms.
            coord_poll_daemon(&mut state, client).await;
            last_poll = Instant::now();
        }
    }
}

/// Route one submitted input line through chat-core and append the result.
///
/// Why: this is the Phase 1C dispatch seam — it replaces the old echo-only stub.
/// Keeping it OUT of [`dispatch::route`] (which is pure) isolates the async/IO
/// half: build the executor for the client's current URL, await the command, and
/// append the rendered result to the output log. Running it inline in the loop
/// (rather than on a background task) keeps the render thread's view of `state`
/// single-owned — the executor call is brief and the loop already awaits the
/// daemon for polling, so there is no separate thread to block.
/// What: echoes `line` into the output log, routes it via [`route`] against the
/// focused session, and for a [`Dispatch::Command`] runs it through a
/// [`CommandExecutor`] (built for `client.base_url()` so it tracks the TUI's
/// self-healed URL) and appends [`render_result`]'s lines. A [`Dispatch::Hint`]
/// appends the hint; a [`Dispatch::Echo`] records nothing further. After a
/// SUCCESSFUL mutating command it requests an immediate re-poll so the list
/// reflects the change now.
/// Test: the routing decision is unit-tested in [`dispatch`] and the rendering in
/// [`render`]; this async glue is exercised by launching the TUI.
async fn dispatch_submitted_line(state: &mut CoordinatorState, client: &DaemonClient, line: &str) {
    // Echo the operator's own input so the log reads as a transcript.
    state.push_output(ratatui::text::Line::from(format!("› {line}")));

    let focused = state.focused_target().map(str::to_string);
    match route(line, focused.as_deref()) {
        Dispatch::Echo => {}
        Dispatch::Hint(msg) => {
            state.push_output(ratatui::text::Line::from(msg));
        }
        Dispatch::Command(cmd) => {
            // Mutating verbs (create/kill/stop/resume) should refresh the list,
            // but only when they actually succeed — keyed off the parsed verb.
            let is_mutating = parse_slash(line).is_some_and(|c| c.is_mutating());
            let executor = CommandExecutor::new(client.base_url().to_string());
            let result = executor.execute(cmd).await;
            let succeeded = !matches!(result, crate::client::CommandResult::Error(_));
            state.push_output_lines(render_result(&result));
            if is_mutating && succeeded {
                state.request_repoll();
            }
        }
    }
}
