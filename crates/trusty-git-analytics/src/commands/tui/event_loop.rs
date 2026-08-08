//! Key mapping and the draw / poll loop for `tga tui`.
//!
//! Why: the loop itself is untestable glue, so every decision it makes is
//! pushed into two pure functions — [`key_to_action`] maps a keypress to an
//! [`Action`], and [`apply`] folds an action into the state. The loop is then
//! just "poll, map, apply, draw".
//! What: the [`Action`] vocabulary, the two pure functions, and [`run_loop`].
//! Test: `super::tests::key_*` and `action_*`.

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use super::render::render;
use super::state::{TuiState, View, WorkStatus};
use super::work::{WorkMode, WorkerHandle};

/// How often the keyboard is polled — also the redraw floor.
///
/// Why: 100 ms keeps keys feeling instant while leaving the progress fold
/// cheap; the pipelines emit far more slowly than that.
/// What: 100 ms.
/// Test: timing glue; not asserted.
const TICK: Duration = Duration::from_millis(100);

/// What a keypress asks the TUI to do.
///
/// Why: naming the intent separately from the keycode is what lets the key map
/// and the state transition each be tested without a terminal.
/// What: one variant per binding, plus [`Action::None`] for unbound keys.
/// Test: `super::tests::key_to_action_maps_every_binding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Leave the TUI.
    Quit,
    /// Advance to the next view.
    NextView,
    /// Jump straight to a view.
    GoTo(View),
    /// Move the active cursor by this many rows.
    Move(isize),
    /// Toggle the repo under the cursor.
    Toggle,
    /// Select all repos, or none if all are selected.
    ToggleAll,
    /// Start a pull + correlate run over the selection.
    RunFull,
    /// Start a correlate-only run — no network, no credentials.
    RunCorrelateOnly,
    /// Cycle the results filter.
    CycleFilter,
    /// Reload the results window from the database.
    Reload,
    /// Toggle the help overlay.
    ToggleHelp,
    /// Nothing is bound to this key.
    None,
}

/// Map a keypress to an [`Action`].
///
/// Why: a pure function over `(KeyCode, KeyModifiers)` is testable; a `match`
/// buried in the loop is not.
/// What: the bindings listed in `render::KEY_HINT`. `Ctrl-C` quits alongside
/// `q` and `Esc`. Unbound keys map to [`Action::None`].
/// Test: `super::tests::key_to_action_maps_every_binding`.
pub fn key_to_action(code: KeyCode, modifiers: KeyModifiers) -> Action {
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        return Action::Quit;
    }
    match code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Tab => Action::NextView,
        KeyCode::Char('1') => Action::GoTo(View::Repos),
        KeyCode::Char('2') => Action::GoTo(View::Progress),
        KeyCode::Char('3') => Action::GoTo(View::Results),
        KeyCode::Up | KeyCode::Char('k') => Action::Move(-1),
        KeyCode::Down | KeyCode::Char('j') => Action::Move(1),
        KeyCode::PageUp => Action::Move(-10),
        KeyCode::PageDown => Action::Move(10),
        KeyCode::Char(' ') => Action::Toggle,
        KeyCode::Char('a') => Action::ToggleAll,
        KeyCode::Char('r') => Action::RunFull,
        KeyCode::Char('c') => Action::RunCorrelateOnly,
        KeyCode::Char('f') => Action::CycleFilter,
        KeyCode::Char('g') => Action::Reload,
        KeyCode::Char('?') => Action::ToggleHelp,
        _ => Action::None,
    }
}

/// What [`apply`] wants the caller to do after folding an action.
///
/// Why: starting a worker and reading the database are I/O the pure fold must
/// not do itself; it returns the request instead.
/// What: `Nothing`, `Start(mode)`, or `Reload`.
/// Test: `super::tests::action_run_requests_a_worker`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// No I/O required.
    Nothing,
    /// Start a worker in this mode.
    Start(WorkMode),
    /// Reload the results window from the database.
    Reload,
}

/// Fold one action into the state, returning any I/O the caller must perform.
///
/// Why: this is where every UI rule lives — which keys are refused mid-run,
/// what a filter change implies, when the view switches by itself — so all of
/// it is testable without a terminal.
/// What: mutates `state` and returns the [`Effect`] the loop must run. A run
/// request while a run is in flight is refused with a message rather than
/// queued. Starting a run switches to the progress view; changing the filter
/// asks for a reload.
/// Test: `super::tests::action_*`.
pub fn apply(state: &mut TuiState, action: Action) -> Effect {
    if action != Action::None {
        state.message = None;
    }
    // #5197: the confirmation is armed for the very next keypress only.
    if action != Action::Quit {
        state.quit_armed = false;
    }
    match action {
        Action::None => Effect::Nothing,
        Action::Quit => {
            if state.show_help {
                state.show_help = false;
            } else if state.status.is_running() && !state.quit_armed {
                // #5197: the worker is a detached thread (`work::start`) that
                // nothing joins, so quitting mid-run cut an in-flight fetch or
                // per-week write off at process exit, silently, with exit code
                // 0. Confirming beats joining here: a join would park the draw
                // loop behind a hung network call in raw mode, leaving the
                // operator no key that works — the confirmation keeps the
                // abandon deliberate and the UI responsive.
                state.quit_armed = true;
                state.set_message(
                    "a run is in flight — press [q]/[Esc] again to abandon it mid-write",
                );
            } else {
                state.quit = true;
            }
            Effect::Nothing
        }
        Action::NextView => {
            state.view = state.view.next();
            Effect::Nothing
        }
        Action::GoTo(view) => {
            state.view = view;
            Effect::Nothing
        }
        Action::Move(delta) => {
            state.move_cursor(delta);
            Effect::Nothing
        }
        Action::Toggle => {
            if state.view == View::Repos {
                state.toggle_selected();
            }
            Effect::Nothing
        }
        Action::ToggleAll => {
            if state.view == View::Repos {
                state.toggle_select_all();
            }
            Effect::Nothing
        }
        Action::RunFull => start(state, WorkMode::PullAndCorrelate),
        Action::RunCorrelateOnly => start(state, WorkMode::CorrelateOnly),
        Action::CycleFilter => {
            state.filter = state.filter.next();
            state.row_cursor = 0;
            Effect::Reload
        }
        Action::Reload => Effect::Reload,
        Action::ToggleHelp => {
            state.show_help = !state.show_help;
            Effect::Nothing
        }
    }
}

/// Shared guard for both run actions.
fn start(state: &mut TuiState, mode: WorkMode) -> Effect {
    if state.status.is_running() {
        state.set_message("a run is already in flight");
        return Effect::Nothing;
    }
    if mode == WorkMode::PullAndCorrelate && state.offline {
        state.set_message("--correlate-only is set — press [c] to correlate without a pull");
        return Effect::Nothing;
    }
    if mode == WorkMode::PullAndCorrelate && state.selected_repositories().is_empty() {
        state.set_message("no repositories selected — press [Space] or [a] in the Repos view");
        return Effect::Nothing;
    }
    state.status = WorkStatus::Running;
    state.view = View::Progress;
    Effect::Start(mode)
}

/// Draw, poll, and dispatch until the operator quits.
///
/// Why: separated from terminal setup so `mod.rs` can restore the terminal on
/// every exit path, including an error out of here.
/// What: each tick pumps the progress bus into the aggregate, checks whether
/// the worker finished, draws, then reads at most one key event and applies
/// it. Effects are executed here because they need the database and the tokio
/// handle, neither of which the pure fold has.
/// Test: the pure halves are unit-tested; the loop itself is exercised by
/// running `tga tui`.
pub fn run_loop<B: std::io::Write>(
    terminal: &mut Terminal<CrosstermBackend<B>>,
    state: &mut TuiState,
    worker: &mut WorkerHandle,
) -> anyhow::Result<()> {
    loop {
        state.pump_progress();
        if let Some(summary) = worker.poll_finished() {
            state.status = WorkStatus::Finished(summary);
            worker.reload_results(state);
        }

        terminal.draw(|frame| render(frame, state))?;

        if !event::poll(TICK)? {
            continue;
        }
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) = event::read()?
        else {
            continue;
        };
        // Windows reports both Press and Release; acting on one is enough.
        if kind != KeyEventKind::Press {
            continue;
        }

        match apply(state, key_to_action(code, modifiers)) {
            Effect::Nothing => {}
            Effect::Start(mode) => worker.start(state, mode),
            Effect::Reload => worker.reload_results(state),
        }
        if state.quit {
            return Ok(());
        }
    }
}
