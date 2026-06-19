//! Key dispatch for the coordinator TUI (Child #1 skeleton).
//!
//! Why: separating "a key arrives → mutate state" from the terminal pump keeps
//! the dispatch pure and unit-testable, and keeps every file under the SLOC
//! cap. The slash-command *parse* lives here too, but deliberately stops at a
//! recognised-but-stubbed enum — Child #4 wires those to the daemon.
//! What: [`SlashCommand`] (the recognised set + an `Unknown` fallback),
//! [`parse_slash`] (leading-`/` detection → enum), and [`handle_key`] which
//! routes a [`KeyEvent`] into [`CoordinatorState`] mutations (selection,
//! editing, submit-to-history, quit).
//! Test: `super::tests` covers `parse_slash` (including the leading-`/` stub)
//! and the key-routing branches via synthetic `KeyEvent`s.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::CoordinatorState;

/// A recognised slash command, parsed but NOT yet dispatched (Child #1).
///
/// Why: the spec's slash-command framework (DOC-13 §5.2) lands in Child #4;
/// Child #1 only needs to *recognise* a leading `/` and classify it so the
/// later wiring slots into a typed enum rather than re-parsing raw strings.
/// What: one variant per documented command plus [`SlashCommand::Unknown`] for
/// an unrecognised `/word`. Carrying the raw text lets a later child surface a
/// "no such command" hint without re-splitting.
/// Test: `parse_slash_recognises_known_commands`,
/// `parse_slash_classifies_unknown`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    /// `/help` — list commands + keybindings.
    Help,
    /// `/new` (alias `/spawn`) — spawn a managed session (`ManagedNew`).
    New,
    /// `/sessions` (alias `/list`, `/managed-list`) — list managed sessions.
    Sessions,
    /// `/attach` (alias `/managed-attach`) — show the tmux attach command.
    Attach,
    /// `/stop` (alias `/managed-stop`) — stop a session's runtime.
    Stop,
    /// `/resume` (alias `/managed-resume`) — resume a stopped session.
    Resume,
    /// `/kill` (aliases `/decommission`, `/managed-decommission`) — decommission.
    Kill,
    /// `/send` (alias `/managed-send`) — inject text into a session's pane.
    Send,
    /// `/answer` (aliases `/approve`, `/managed-answer`) — answer a decision.
    Answer,
    /// `/activity` (alias `/managed-activity`) — inspect a session's activity.
    Activity,
    /// `/get` (aliases `/status`, `/managed-get`) — fetch a session's record.
    Get,
    /// A leading-`/` token that matched none of the known commands.
    Unknown(String),
}

impl SlashCommand {
    /// Whether dispatching this command mutates the fleet (create/kill/stop/resume).
    ///
    /// Why: STUI-1 requires an immediate daemon re-poll after a *list-mutating*
    /// action so the operator sees the change now rather than up to a whole
    /// `--interval-ms` later. Read-only commands (`/help`, `/sessions`,
    /// `/attach`, `/get`, `/activity`) must NOT force a refresh — they change no
    /// list-visible daemon state — so the re-poll trigger keys off this predicate
    /// rather than firing on every submission.
    /// What: returns `true` for [`SlashCommand::New`], [`SlashCommand::Kill`],
    /// [`SlashCommand::Stop`], and [`SlashCommand::Resume`] (the verbs that add or
    /// remove rows or flip a session's lifecycle state); `false` otherwise. `Send`
    /// and `Answer` inject into a pane without changing the list shape, so they are
    /// non-mutating for re-poll purposes.
    /// Test: `mutating_commands_are_classified`.
    pub fn is_mutating(&self) -> bool {
        matches!(
            self,
            SlashCommand::New | SlashCommand::Kill | SlashCommand::Stop | SlashCommand::Resume
        )
    }
}

/// Parse a submitted input line into a [`SlashCommand`], if it is one.
///
/// Why: Enter on a `/`-prefixed line should be classified as a command rather
/// than echoed as chat; doing the classification in a pure function keeps it
/// testable and keeps the leading-`/` contract (§3.4) in one place.
/// What: returns `None` when `input` does not start with `/`, or when the input
/// is a bare `/` with no command word (e.g. `/` or `/   `) — a lone slash is not
/// a command, so it falls through to chat rather than yielding
/// `Unknown("")`. Otherwise lowercases the first whitespace-delimited token
/// (sans the `/`) and maps it — and its documented aliases (`/spawn`→`New`,
/// `/list`→`Sessions`, `/status`→`Get`, `/approve`→`Answer`, the `managed-*`
/// spellings, …) — to the matching [`SlashCommand`], falling back to
/// [`SlashCommand::Unknown`] carrying that token.
/// Test: `parse_slash_recognises_known_commands`,
/// `parse_slash_classifies_unknown`, `parse_slash_ignores_plain_text`,
/// `parse_slash_bare_slash_is_not_a_command`.
pub fn parse_slash(input: &str) -> Option<SlashCommand> {
    let rest = input.strip_prefix('/')?;
    // A bare `/` (no command word) is not a command — treat it as chat so the
    // operator never sees a spurious `Unknown("")` classification.
    let word = rest.split_whitespace().next()?.to_lowercase();
    let cmd = match word.as_str() {
        "help" => SlashCommand::Help,
        "new" | "spawn" => SlashCommand::New,
        "sessions" | "list" | "managed-list" => SlashCommand::Sessions,
        "attach" | "managed-attach" => SlashCommand::Attach,
        "stop" | "managed-stop" => SlashCommand::Stop,
        "resume" | "managed-resume" => SlashCommand::Resume,
        "kill" | "decommission" | "managed-decommission" => SlashCommand::Kill,
        "send" | "managed-send" => SlashCommand::Send,
        "answer" | "approve" | "managed-answer" => SlashCommand::Answer,
        "activity" | "managed-activity" => SlashCommand::Activity,
        "get" | "status" | "managed-get" => SlashCommand::Get,
        other => SlashCommand::Unknown(other.to_string()),
    };
    Some(cmd)
}

/// True when this key event means "quit" (`q` or Ctrl-C).
///
/// Why: both the dispatcher and any future modal layer need the same quit
/// predicate; factoring it out keeps the rule single-sourced.
/// What: matches `q` (only when the input buffer is empty, so a `q` typed mid
/// message is not hijacked) or Ctrl-C regardless of buffer.
/// Test: `quit_on_q_when_empty`, `quit_on_ctrl_c`, `q_in_message_is_not_quit`.
pub fn is_quit(key: &KeyEvent, input_empty: bool) -> bool {
    let ctrl_c = key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
    let bare_q = key.code == KeyCode::Char('q') && input_empty;
    ctrl_c || bare_q
}

/// Route one key event into [`CoordinatorState`] mutations.
///
/// Why: this is the single seam the terminal pump calls per key; keeping it a
/// pure `&mut state` function (no terminal, no IO) makes every branch — quit,
/// selection, editing, submit — unit-testable with synthetic [`KeyEvent`]s.
/// What: Ctrl-C / bare `q` set `should_exit`; ↑/↓ and `k`/`j` move the
/// selection one row when the input is empty (↑/↓ recall history when it is
/// not); Page-Up/Page-Down jump the selection by a page (STUI-1) regardless of
/// the buffer; printable keys edit the buffer; Backspace deletes; Esc clears;
/// Enter submits (echoing to history — Child #1 does not dispatch). When the
/// submitted line is a *mutating* slash command ([`SlashCommand::is_mutating`])
/// it also requests an immediate daemon re-poll (STUI-1) so the fleet refreshes
/// now rather than on the next timer tick. Returns any submitted line so the
/// caller can record a last-action note; slash lines are returned too (the
/// caller may `parse_slash` them) but are not otherwise dispatched here.
/// Test: `handle_key_*` branch tests in `super::tests`, incl.
/// `handle_key_enter_mutating_command_requests_repoll`.
pub fn handle_key(state: &mut CoordinatorState, key: KeyEvent) -> Option<String> {
    let input_empty = state.input.is_empty();
    if is_quit(&key, input_empty) {
        state.should_exit = true;
        return None;
    }

    match key.code {
        // Page scrolling works regardless of the input buffer — Page-Up/Page-Down
        // are list-navigation keys with no text-editing meaning, so hijacking them
        // mid-message is unambiguous.
        KeyCode::PageUp => {
            state.page_up();
            None
        }
        KeyCode::PageDown => {
            state.page_down();
            None
        }
        KeyCode::Up | KeyCode::Char('k') if input_empty => {
            state.select_up();
            None
        }
        KeyCode::Down | KeyCode::Char('j') if input_empty => {
            state.select_down();
            None
        }
        // With text in the buffer, ↑/↓ recall input history instead of moving
        // the session selection (mirrors the dashboard's CommandBar behaviour).
        KeyCode::Up => {
            state.history_prev();
            None
        }
        KeyCode::Down => {
            state.history_next();
            None
        }
        KeyCode::Backspace => {
            state.backspace();
            None
        }
        KeyCode::Esc => {
            state.clear_input();
            None
        }
        KeyCode::Enter => {
            let submitted = state.submit_input();
            // STUI-1: a mutating slash command (create/kill/stop/resume) must
            // refresh the fleet immediately rather than waiting for the next
            // timer tick. We wire the re-poll TRIGGER here now (the run loop
            // already consumes `take_repoll`); the command's actual daemon
            // dispatch is still stubbed.
            //
            // TODO(STUI-6): replace this parse-and-flag with the real dispatch —
            // the create/kill/stop/resume handlers will call
            // `state.request_repoll()` only *after* a successful mutation, so a
            // failed/rejected command no longer forces a spurious refresh.
            if let Some(line) = submitted.as_deref()
                && parse_slash(line).is_some_and(|cmd| cmd.is_mutating())
            {
                state.request_repoll();
            }
            submitted
        }
        KeyCode::Char(c) => {
            state.push_char(c);
            None
        }
        _ => None,
    }
}
