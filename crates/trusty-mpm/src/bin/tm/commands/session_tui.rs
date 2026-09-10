//! `tm ls` — the full-screen, resize-aware managed-session TUI (#7224).
//!
//! Why: the numbered picker this replaces on the `tm ls` path computed its
//! column widths once, from the terminal width at startup, and read whole lines
//! from stdin. A resize left it formatted for a terminal that no longer
//! existed, and every action was a typed verb (`d2`, `r3 new-name`) an operator
//! had to remember. #6752 proposed reflowing that same line-based menu at a
//! fixed 80 columns; this supersedes that surface instead — a redrawn frame is
//! resize handling for free, and a selected row makes the verbs into single
//! keys.
//!
//! What this REUSES, and what it does not. Every action routes through the path
//! the CLI already has: Enter calls
//! [`guided_resume::resume_guided_session`](super::guided_resume::resume_guided_session)
//! — the same function the numbered picker's `Resume` arm and `tm session
//! resume <id>` reach; `d` calls
//! [`picker_delete::delete_managed_then_local`](super::picker_delete::delete_managed_then_local),
//! the shared managed→local routing `tm session delete` uses, after a confirm
//! step that reuses that module's own `confirm_is_yes`/`confirm_is_force`
//! classifiers; `r` calls
//! [`rename::do_rename_request`](super::rename::do_rename_request), the exact
//! PATCH `tm sessions rename` issues, after the daemon's own
//! `validate_session_name`. The terminal setup, the panic-safe guard, and the
//! suspend/resume around the tmux hand-off come from
//! [`trusty_mpm::tui::terminal`], shared with the coordinator and `project_ctl`
//! screens. What is NOT shared is the input loop: a full-screen surface needs
//! raw-mode keys, and the numbered picker keeps its line reader for the cases
//! this surface cannot serve — an empty fleet (only the picker can launch a
//! NEW session) and a terminal that cannot enter raw mode.
//!
//! #7224: bare `tm` opens this surface too, through
//! [`super::session_ls_connector::run_bare_tm_surface`] — the same gate, not a
//! second copy of it.
//!
//! `tm ls --plain`, `--json`, `--all`, `--attached`, and any non-TTY stream
//! still print the static table, unchanged — see
//! [`super::session_ls_connector::should_show_picker`].
//!
//! Test: the pure halves are unit-tested in `session_tui/tests.rs` — the state
//! machine, the width-driven column layout, the scroll window, and
//! `TestBackend` renders at 80x24, 200x50, and 20x5. The daemon round trips are
//! the ones the reused functions already carry tests for; the terminal
//! hand-off is verified by the tmux smoke run in the PR body.

pub(crate) mod layout;
pub(crate) mod render;
pub(crate) mod state;

#[cfg(test)]
#[path = "session_tui/tests.rs"]
mod tests;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use trusty_mpm::client::ManagedSessionSummary;
use trusty_mpm::tui::terminal::{self, TerminalGuard};

use super::picker_delete::{DeleteReport, route_delete};
use super::session_picker::{PickerScope, fetch_live_sessions};
use state::{Action, Input, Severity, TuiState};

/// Map one `crossterm` key event onto the surface's [`Input`] alphabet.
///
/// Why: a pure boundary between "what crossterm reported" and "what this
/// surface understands" is what lets every transition be tested without a
/// terminal. Characters pass through unresolved on purpose — `d` is delete
/// while browsing and a literal `d` while typing a name, and only
/// [`TuiState::apply`] knows which mode is open.
/// What: `None` for a key with no meaning here (a release event, a function
/// key, a Ctrl chord other than C/D), so the caller can skip the redraw.
/// Test: `input_maps_ctrl_c_to_cancel`, `input_ignores_key_release`,
/// `input_passes_characters_through_unresolved`.
pub(crate) fn map_key(key: KeyEvent) -> Option<Input> {
    // Windows reports both press and release; act on press only.
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c' | 'd') if ctrl => Some(Input::Cancel),
        KeyCode::Char(_) if ctrl => None,
        KeyCode::Char(c) => Some(Input::Char(c)),
        KeyCode::Enter => Some(Input::Enter),
        KeyCode::Backspace => Some(Input::Backspace),
        KeyCode::Esc => Some(Input::Escape),
        KeyCode::Up => Some(Input::Up),
        KeyCode::Down => Some(Input::Down),
        KeyCode::Home => Some(Input::Home),
        KeyCode::End => Some(Input::End),
        KeyCode::PageUp => Some(Input::PageUp),
        KeyCode::PageDown => Some(Input::PageDown),
        _ => None,
    }
}

/// Run the `tm ls` session TUI until the operator quits or hands the terminal off.
///
/// Why: one entry point owns the terminal lifecycle, so the alternate screen is
/// always left before a tmux attach and always restored afterwards — including
/// on a panic, via [`TerminalGuard`].
/// What: loops — order the list through
/// [`prepare_menu`](super::session_picker_order::prepare_menu) (the same
/// sort/filter/pin the numbered picker uses, so the two surfaces cannot show
/// different orders), draw, read one key, and dispatch the [`Action`] the state
/// machine returns. `Open` suspends the terminal, calls the shared resume path,
/// and either returns (a `switch-client` hand-off left this pane invisible —
/// #2678) or resumes the TUI and re-fetches. `Delete` and `Rename` are HTTP
/// round trips that report into the status line and re-fetch; neither leaves
/// the alternate screen.
///
/// `self_session_id` is `$TM_MANAGED_SESSION_ID` and `self_tmux_name` the tmux
/// session this process runs inside — both injected rather than read here,
/// because `src/bin/tm/**` is ratcheted against process-global env access
/// (#5544). Together they are how the delete guard recognises the session the
/// operator is attached to.
/// Test: the pure halves in `session_tui/tests.rs`; the loop itself by the tmux
/// smoke run.
pub(crate) async fn run_session_tui(
    client: &reqwest::Client,
    url: &str,
    scope: &mut PickerScope,
    sessions: Vec<ManagedSessionSummary>,
    self_session_id: Option<String>,
    self_tmux_name: Option<String>,
) -> anyhow::Result<()> {
    let mut sessions = sessions;
    let mut state = TuiState::new(self_session_id, self_tmux_name);
    let mut terminal = terminal::enter()?;
    // The guard restores cooked mode and the main screen on every exit path —
    // normal return AND panic unwind — so it is the SOLE teardown.
    let _guard = TerminalGuard;

    loop {
        // #7224: the ORDER comes from the numbered picker's own `prepare_menu`,
        // so `tm ls` shows one ordering whichever surface renders it.
        let menu = super::session_picker_order::prepare_menu(sessions, scope);
        sessions = menu.sessions;
        state.sync(&sessions);
        scope.selected_id = sessions.get(state.selected()).map(|s| s.id.clone());

        terminal.draw(|frame| render::render(frame, &sessions, &mut state))?;

        // A `Resize` arrives as an ordinary event and simply falls through to
        // the next draw, which recomputes the column set from the new width —
        // that is the whole of the resize handling (#7224 requirement 2).
        let Event::Key(key) = event::read()? else {
            continue;
        };
        let Some(input) = map_key(key) else { continue };

        match state.apply(input, &sessions) {
            Action::Ignore | Action::Redraw => continue,
            Action::Quit => break,
            Action::Refresh => {
                state.set_message("refreshed", Severity::Info);
            }
            Action::Open(index) => {
                // #7224 requirement 3: the alternate screen goes away BEFORE
                // the tmux hand-off, so `attach-session` gets the real terminal
                // in cooked mode.
                terminal::suspend(&mut terminal)?;
                let outcome =
                    super::guided_resume::resume_guided_session(client, url, &sessions[index])
                        .await;
                match outcome {
                    // #2678: a `switch-client` hand-off (or a fail-closed skip)
                    // means this pane is no longer visible — stop rather than
                    // redrawing onto a screen nobody is looking at.
                    Ok(outcome) if outcome.ends_interactive_loop() => return Ok(()),
                    Ok(_) => terminal::resume(&mut terminal)?,
                    Err(e) => {
                        terminal::resume(&mut terminal)?;
                        state.set_message(format!("open failed: {e}"), Severity::Error);
                    }
                }
            }
            Action::Delete {
                index,
                force,
                stop_first,
            } => {
                let session = &sessions[index];
                // #7224: an errored row takes the stop-then-delete route, which
                // is the CLI step the daemon's refusal used to send the operator
                // out of this surface to run by hand. The numbered fallback
                // picker shares this routing rather than re-deciding it.
                let report = route_delete(client, url, &session.id, force, stop_first).await;
                let (text, severity) = delete_outcome(&session.name, report);
                state.set_message(text, severity);
            }
            Action::Rename { index, name } => {
                let session = &sessions[index];
                match super::rename::do_rename_request(
                    client,
                    url,
                    &session.name,
                    &session.id,
                    name,
                )
                .await
                {
                    Ok(msg) => state.set_message(msg, Severity::Info),
                    Err(e) => state.set_message(format!("rename failed: {e}"), Severity::Error),
                }
            }
        }

        // Every dispatched action can have changed the fleet; re-fetch so the
        // next frame is the daemon's answer, not this process's guess. A fetch
        // failure keeps the list already in hand and says so, rather than
        // dropping the operator back to a shell.
        match fetch_live_sessions(client, url, scope.source_id.as_deref(), false).await {
            Ok(fetched) => sessions = fetched,
            Err(e) => state.set_message(format!("refresh failed: {e}"), Severity::Error),
        }
    }
    Ok(())
}

/// Turn a [`DeleteReport`] into the status line the TUI shows.
///
/// Why: a pure function so the wording — including the soft-delete caveat that
/// a managed record stays listed — is assertable without a daemon. The line
/// picker prints the same three outcomes; keeping this separate is what lets
/// the TUI say them in one line instead of two.
/// What: `Deleted` reports the store it came from (a managed record is SOFT
/// deleted and stays in the listing, #2012); `Refused` carries the daemon's own
/// 409 text; `StopFailed` (#7224) says the stop leg failed and NOTHING was
/// deleted, carrying the daemon's full status and body; `NotFound` says the
/// record was already gone. A transport error becomes an error line rather than
/// ending the TUI.
/// Test: `delete_outcome_names_the_soft_delete`,
/// `delete_outcome_surfaces_a_refusal`,
/// `delete_outcome_reports_a_failed_stop_as_not_deleted`,
/// `delete_outcome_reports_not_found`,
/// `delete_outcome_reports_a_transport_error`.
pub(crate) fn delete_outcome(
    name: &str,
    report: anyhow::Result<DeleteReport>,
) -> (String, Severity) {
    match report {
        Ok(DeleteReport::Deleted {
            name,
            prior_state,
            local: true,
        }) => (
            format!("deleted '{name}' [was {prior_state}] from the project store"),
            Severity::Info,
        ),
        Ok(DeleteReport::Deleted {
            name, prior_state, ..
        }) => (
            format!(
                "'{name}' [was {prior_state}] marked --deleted-- \
                 (still listed; `tm sessions prune --state deleted` removes it)"
            ),
            Severity::Info,
        ),
        Ok(DeleteReport::Refused(msg)) => {
            (format!("delete refused: {}", msg.trim()), Severity::Error)
        }
        Ok(DeleteReport::StopFailed(msg)) => (
            format!("'{name}' NOT deleted — {}", msg.trim()),
            Severity::Error,
        ),
        Ok(DeleteReport::NotFound) => {
            (format!("'{name}' not found — already gone"), Severity::Info)
        }
        Err(e) => (format!("delete failed: {e}"), Severity::Error),
    }
}
