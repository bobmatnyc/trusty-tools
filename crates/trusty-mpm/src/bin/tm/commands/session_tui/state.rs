//! Pure state machine behind the `tm ls` session TUI (#7224).
//!
//! Why: everything a TUI can get wrong — a selection that survives a refresh, a
//! confirm step that a stray keystroke skips, a rename that reaches the daemon
//! with a name the daemon will reject — is decidable without a terminal. Split
//! the decisions out and the untestable remainder shrinks to the draw calls and
//! the `crossterm` read, which is the same split
//! [`session_picker_filter`](super::super::session_picker_filter) already
//! makes.
//!
//! What: [`Input`] is one keystroke reduced to what this surface understands;
//! [`TuiState::apply`] maps `(mode, input)` to an [`Action`] the I/O driver
//! performs; [`TuiState::sync`] re-points the selection after the session list
//! is replaced. The confirm step reuses
//! [`confirm_is_yes`](super::super::picker_delete::confirm_is_yes) /
//! [`confirm_is_force`](super::super::picker_delete::confirm_is_force) and the
//! rename step reuses `validate_session_name`, so neither invents a second
//! spelling of a rule the CLI already has.
//!
//! Test: `state_*` in `super::tests`.

use trusty_mpm::client::ManagedSessionSummary;
use trusty_mpm::session_manager::rename::validate_session_name;

use super::super::picker_delete::{
    confirm_is_force, confirm_is_yes, delete_needs_force, delete_needs_stop_first,
};

/// How far `PageUp`/`PageDown` move the selection.
const PAGE: usize = 10;

/// One keystroke, reduced to what this surface understands.
///
/// Why: the mapping from `crossterm` to this enum is the only mode-independent
/// part of input handling — `d` is "delete" while browsing and a literal `d`
/// while typing a name, so the mode-aware half has to see the character, not a
/// pre-resolved command.
/// What: the ten inputs [`TuiState::apply`] acts on.
/// Test: every `state_*` transition test constructs these directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Input {
    /// A printable character.
    Char(char),
    /// Enter / Return.
    Enter,
    /// Backspace.
    Backspace,
    /// Esc.
    Escape,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Home.
    Home,
    /// End.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// Ctrl-C / Ctrl-D — leave, from any mode.
    Cancel,
}

/// What the I/O driver should do after an [`Input`].
///
/// Why: the state machine never issues HTTP or touches the terminal; returning
/// an intent keeps every daemon round trip in one place (the driver) and every
/// decision in another (here).
/// What: `Ignore` means the frame did not change and need not be redrawn;
/// `Redraw` means it did. The three action variants each carry the index into
/// the session list the driver should act on.
/// Test: asserted by every `state_*` test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// The frame changed; draw it again.
    Redraw,
    /// Nothing changed.
    Ignore,
    /// Leave the TUI.
    Quit,
    /// Open (resume/attach) the session at this index.
    Open(usize),
    /// Delete the session at this index, `force` for a running one.
    Delete {
        /// Index into the session list.
        index: usize,
        /// Whether the running-session force flag is required and confirmed.
        force: bool,
        /// Stop the runtime first, then delete — the errored-session path
        /// (#7224). Never set together with `force`.
        stop_first: bool,
    },
    /// Rename the session at this index to an ALREADY-VALIDATED name.
    Rename {
        /// Index into the session list.
        index: usize,
        /// The trimmed, validated name `validate_session_name` returned.
        name: String,
    },
    /// Re-fetch the session list from the daemon.
    Refresh,
}

/// Which overlay, if any, is in front of the session list.
///
/// Why: the confirm and rename steps are modes rather than prompts because a
/// TUI has no line reader to block on — the same key loop has to serve both,
/// and only the mode says whether `d` deletes or types a `d`.
/// What: `Browse` is the list; `Confirm` and `Rename` each carry the row they
/// target and the text typed so far; `Help` is the key list.
/// Test: `state_*` transitions in `super::tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The session list, with the action keys live.
    Browse,
    /// Delete confirmation for one row.
    Confirm {
        /// Index into the session list.
        index: usize,
        /// True when the row is running, so the word `force` is required.
        force: bool,
        /// True when confirming also stops the runtime first (#7224), so the
        /// prompt can say so before the operator agrees to it.
        stop_first: bool,
        /// What the operator has typed so far.
        typed: String,
    },
    /// Inline rename of one row.
    Rename {
        /// Index into the session list.
        index: usize,
        /// The proposed name, seeded with the row's current one.
        typed: String,
    },
    /// The key reference.
    Help,
}

/// Severity of the one-line message under the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Severity {
    /// An outcome worth reading — a rename applied, a delete done.
    Info,
    /// A refusal or a failure.
    Error,
}

/// The whole mutable state of the session TUI.
///
/// Why: one owner for the selection, the mode, the scroll offset, and the
/// status line means a refresh has exactly one place to re-point them and the
/// renderer has exactly one thing to read.
/// What: `pinned_id` is what makes the selection survive a refresh (#7224) —
/// the session list is replaced wholesale by every re-fetch, so an index alone
/// would silently re-point at whatever row moved into that position.
/// `self_session_id` / `self_tmux_name` are how a row is recognised as the
/// session this very terminal is attached to; both are injected rather than
/// read from the environment here, because `src/bin/tm/**` is ratcheted against
/// process-global env access (#5544).
/// Test: `state_*` in `super::tests`.
#[derive(Debug, Clone)]
pub(crate) struct TuiState {
    mode: Mode,
    selected: usize,
    scroll: usize,
    pinned_id: Option<String>,
    message: Option<(String, Severity)>,
    self_session_id: Option<String>,
    self_tmux_name: Option<String>,
}

impl TuiState {
    /// Start browsing with the first row selected.
    ///
    /// `self_session_id` is `$TM_MANAGED_SESSION_ID` and `self_tmux_name` the
    /// tmux session this process runs inside; both may be absent (an ad-hoc
    /// terminal), which simply means no row is "self".
    pub(crate) fn new(self_session_id: Option<String>, self_tmux_name: Option<String>) -> Self {
        Self {
            mode: Mode::Browse,
            selected: 0,
            scroll: 0,
            pinned_id: None,
            message: None,
            self_session_id,
            self_tmux_name,
        }
    }

    /// The current overlay.
    pub(crate) fn mode(&self) -> &Mode {
        &self.mode
    }

    /// The 0-based index of the selected row.
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// The scroll offset the last render computed.
    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    /// Record the scroll offset a render computed, so the next one holds still.
    pub(crate) fn set_scroll(&mut self, scroll: usize) {
        self.scroll = scroll;
    }

    /// The status line under the table, if any.
    pub(crate) fn message(&self) -> Option<(&str, Severity)> {
        self.message.as_ref().map(|(t, s)| (t.as_str(), *s))
    }

    /// Replace the status line.
    pub(crate) fn set_message(&mut self, text: impl Into<String>, severity: Severity) {
        self.message = Some((text.into(), severity));
    }

    /// Re-point the selection onto a freshly fetched session list (#7224).
    ///
    /// Why: every refresh — the periodic one, and the one after a delete or a
    /// rename — hands back a new `Vec` whose ordering the daemon and the
    /// scope's sort decide. Holding an index across that would move the
    /// selection to an unrelated session, which is the shape of an accidental
    /// delete.
    /// What: prefers the row whose id equals `pinned_id`; failing that (the
    /// session left the fleet, or nothing is pinned yet) clamps the existing
    /// index into range and re-pins to whatever that resolves to. Any overlay
    /// whose target row is gone falls back to `Browse` — a confirm dialog must
    /// never survive the disappearance of the thing it was confirming.
    /// Test: `state_sync_follows_the_pinned_session_across_a_reorder`,
    /// `state_sync_clamps_when_the_pinned_session_is_gone`,
    /// `state_sync_drops_an_overlay_whose_row_vanished`.
    pub(crate) fn sync(&mut self, sessions: &[ManagedSessionSummary]) {
        if sessions.is_empty() {
            self.selected = 0;
            self.pinned_id = None;
            self.mode = Mode::Browse;
            return;
        }
        let by_pin = self
            .pinned_id
            .as_deref()
            .and_then(|id| sessions.iter().position(|s| s.id == id));
        self.selected = by_pin.unwrap_or_else(|| self.selected.min(sessions.len() - 1));
        self.pinned_id = Some(sessions[self.selected].id.clone());
        let target = match &self.mode {
            Mode::Confirm { index, .. } | Mode::Rename { index, .. } => Some(*index),
            _ => None,
        };
        if target.is_some_and(|i| i >= sessions.len()) {
            self.mode = Mode::Browse;
        }
    }

    /// Route one keystroke, given the sessions currently on screen.
    ///
    /// Why: the mode decides what a key means, and folding that decision in
    /// here (rather than in the key loop) is what makes every refusal — the
    /// self-session delete guard, an invalid rename, a confirm that was not
    /// typed out — assertable without a terminal.
    /// What: `Cancel` leaves from any mode. Otherwise dispatches to the
    /// per-mode handler. Every handler returns [`Action::Ignore`] when nothing
    /// changed, so the driver can skip a redraw.
    /// Test: every `state_*` test in `super::tests`.
    pub(crate) fn apply(&mut self, input: Input, sessions: &[ManagedSessionSummary]) -> Action {
        if input == Input::Cancel {
            return Action::Quit;
        }
        match self.mode.clone() {
            Mode::Browse => self.apply_browse(input, sessions),
            Mode::Help => {
                self.mode = Mode::Browse;
                Action::Redraw
            }
            Mode::Confirm {
                index,
                force,
                stop_first,
                typed,
            } => self.apply_confirm(input, index, force, stop_first, typed),
            Mode::Rename { index, typed } => self.apply_rename(input, index, typed, sessions),
        }
    }

    /// Browse-mode keys: navigation plus the four action keys.
    fn apply_browse(&mut self, input: Input, sessions: &[ManagedSessionSummary]) -> Action {
        let last = sessions.len().saturating_sub(1);
        match input {
            Input::Char('q') | Input::Escape => Action::Quit,
            Input::Char('?') => {
                self.mode = Mode::Help;
                Action::Redraw
            }
            Input::Up | Input::Char('k') => self.move_to(self.selected.saturating_sub(1), sessions),
            Input::Down | Input::Char('j') => self.move_to((self.selected + 1).min(last), sessions),
            Input::Home | Input::Char('g') => self.move_to(0, sessions),
            Input::End | Input::Char('G') => self.move_to(last, sessions),
            Input::PageUp => self.move_to(self.selected.saturating_sub(PAGE), sessions),
            Input::PageDown => self.move_to((self.selected + PAGE).min(last), sessions),
            Input::Char('R') => Action::Refresh,
            Input::Enter => match sessions.is_empty() {
                true => Action::Ignore,
                false => Action::Open(self.selected),
            },
            Input::Char('d') => self.begin_delete(sessions),
            Input::Char('r') => self.begin_rename(sessions),
            _ => Action::Ignore,
        }
    }

    /// Move the selection and re-pin it to the session now under it.
    fn move_to(&mut self, index: usize, sessions: &[ManagedSessionSummary]) -> Action {
        if sessions.is_empty() || index == self.selected {
            return Action::Ignore;
        }
        self.selected = index;
        self.pinned_id = sessions.get(index).map(|s| s.id.clone());
        Action::Redraw
    }

    /// Open the delete confirmation, refusing the session we are attached to.
    ///
    /// Why (#7224 requirement 4): deleting the session this terminal is inside
    /// tears down the pane the operator is reading the TUI from. The refusal is
    /// a visible message rather than a silent no-op, because a key that does
    /// nothing reads as a broken key.
    /// What: [`is_self_session`] decides; otherwise the mode becomes `Confirm`,
    /// with `force` set by the same [`delete_needs_force`] the line picker uses,
    /// so a running session demands the word `force` in both surfaces, and
    /// `stop_first` set by [`delete_needs_stop_first`] — the errored row whose
    /// runtime the daemon will refuse to delete around (#7224). One `y` covers
    /// both legs; the overlay says so before it is typed.
    /// Test: `state_delete_refuses_the_attached_self_session`,
    /// `state_delete_on_a_running_row_requires_the_force_word`,
    /// `state_delete_on_an_errored_row_asks_to_stop_and_delete`.
    fn begin_delete(&mut self, sessions: &[ManagedSessionSummary]) -> Action {
        let Some(session) = sessions.get(self.selected) else {
            return Action::Ignore;
        };
        if is_self_session(
            session,
            self.self_session_id.as_deref(),
            self.self_tmux_name.as_deref(),
        ) {
            self.set_message(
                format!(
                    "'{}' is the session this terminal is attached to — detach first, \
                     then delete it",
                    session.name
                ),
                Severity::Error,
            );
            return Action::Redraw;
        }
        self.mode = Mode::Confirm {
            index: self.selected,
            force: delete_needs_force(&session.state),
            stop_first: delete_needs_stop_first(&session.state),
            typed: String::new(),
        };
        self.message = None;
        Action::Redraw
    }

    /// Open the inline rename, seeded with the row's current name.
    fn begin_rename(&mut self, sessions: &[ManagedSessionSummary]) -> Action {
        let Some(session) = sessions.get(self.selected) else {
            return Action::Ignore;
        };
        self.mode = Mode::Rename {
            index: self.selected,
            typed: session.name.clone(),
        };
        self.message = None;
        Action::Redraw
    }

    /// Confirm-mode keys: type the confirmation word, then Enter.
    ///
    /// Why: a single-keypress `y` is too easy to hit by accident on a
    /// destructive action, and the CLI already settled which word each case
    /// takes — `y`/`yes` for a stopped session, the literal word `force` for a
    /// running one. Reusing both classifiers is what keeps the two surfaces
    /// from disagreeing about what counts as a confirmation.
    /// Test: `state_delete_on_a_stopped_row_confirms_with_yes`,
    /// `state_delete_on_a_running_row_requires_the_force_word`,
    /// `state_delete_on_an_errored_row_asks_to_stop_and_delete`,
    /// `state_confirm_backspace_and_escape_both_step_back`.
    fn apply_confirm(
        &mut self,
        input: Input,
        index: usize,
        force: bool,
        stop_first: bool,
        mut typed: String,
    ) -> Action {
        match input {
            Input::Escape => {
                self.mode = Mode::Browse;
                self.set_message("delete cancelled", Severity::Info);
                Action::Redraw
            }
            Input::Char(c) => {
                typed.push(c);
                self.mode = Mode::Confirm {
                    index,
                    force,
                    stop_first,
                    typed,
                };
                Action::Redraw
            }
            Input::Backspace => {
                typed.pop();
                self.mode = Mode::Confirm {
                    index,
                    force,
                    stop_first,
                    typed,
                };
                Action::Redraw
            }
            Input::Enter => {
                let confirmed = match force {
                    true => confirm_is_force(&typed),
                    false => confirm_is_yes(&typed),
                };
                self.mode = Mode::Browse;
                if !confirmed {
                    self.set_message("delete cancelled", Severity::Info);
                    return Action::Redraw;
                }
                Action::Delete {
                    index,
                    force,
                    stop_first,
                }
            }
            _ => Action::Ignore,
        }
    }

    /// Rename-mode keys: edit the name, then Enter to validate and send.
    ///
    /// Why (#7224 requirement 5): the daemon rejects an invalid name with a
    /// 400, which in a TUI would arrive as an error banner after a round trip.
    /// Running the SAME `validate_session_name` the daemon runs turns that into
    /// immediate feedback with the daemon's own message, and — because it is
    /// literally the same function — the two can never disagree about which
    /// names are valid.
    /// What: an invalid name keeps the operator in the rename overlay with the
    /// validator's message; a valid one yields [`Action::Rename`] carrying the
    /// TRIMMED name the validator returned, not the raw buffer.
    /// Test: `state_rename_rejects_an_invalid_name_with_the_shared_validator`,
    /// `state_rename_sends_the_trimmed_validated_name`,
    /// `state_rename_escape_cancels`.
    fn apply_rename(
        &mut self,
        input: Input,
        index: usize,
        mut typed: String,
        sessions: &[ManagedSessionSummary],
    ) -> Action {
        match input {
            Input::Escape => {
                self.mode = Mode::Browse;
                self.set_message("rename cancelled", Severity::Info);
                Action::Redraw
            }
            Input::Char(c) => {
                typed.push(c);
                self.mode = Mode::Rename { index, typed };
                Action::Redraw
            }
            Input::Backspace => {
                typed.pop();
                self.mode = Mode::Rename { index, typed };
                Action::Redraw
            }
            Input::Enter => match validate_session_name(&typed) {
                Err(msg) => {
                    self.mode = Mode::Rename { index, typed };
                    self.set_message(msg, Severity::Error);
                    Action::Redraw
                }
                Ok(name) if sessions.get(index).is_some_and(|s| s.name == name) => {
                    self.mode = Mode::Browse;
                    self.set_message("name unchanged", Severity::Info);
                    Action::Redraw
                }
                Ok(name) => {
                    self.mode = Mode::Browse;
                    Action::Rename { index, name }
                }
            },
            _ => Action::Ignore,
        }
    }
}

/// Is this row the managed session the current terminal is attached to?
///
/// Why (#7224 requirement 4): deleting it would kill the pane the operator is
/// looking at. Two independent signals answer the question, and either alone is
/// enough: `$TM_MANAGED_SESSION_ID` is exported into every managed pane's
/// shell, and the tmux session name IS a managed session's identity name (see
/// `session_manager::rename`), so a `tm ls` run from inside the pane matches on
/// one or the other even when the env var was lost to a nested shell.
/// What: `true` when the id matches `self_id`, or the name matches
/// `self_tmux_name`. Both `None` (an ad-hoc terminal) means no row is self.
/// Test: `is_self_session_matches_by_env_id`,
/// `is_self_session_matches_by_tmux_name`,
/// `is_self_session_false_outside_a_managed_pane`.
pub(crate) fn is_self_session(
    session: &ManagedSessionSummary,
    self_id: Option<&str>,
    self_tmux_name: Option<&str>,
) -> bool {
    self_id.is_some_and(|id| id == session.id)
        || self_tmux_name.is_some_and(|name| name == session.name)
}
