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
    /// `/help` — list commands + keybindings (overlay; Child #4).
    Help,
    /// `/new` — open the new-session form (Child #4).
    New,
    /// `/sessions` — open the session picker (Child #4).
    Sessions,
    /// `/attach` — show/copy the tmux attach command (Child #5).
    Attach,
    /// `/stop` — stop a session's runtime (Child #5).
    Stop,
    /// `/resume` — resume a stopped session (Child #5).
    Resume,
    /// `/kill` — decommission a session (Child #5).
    Kill,
    /// A leading-`/` token that matched none of the known commands.
    Unknown(String),
}

/// Parse a submitted input line into a [`SlashCommand`], if it is one.
///
/// Why: Enter on a `/`-prefixed line should be classified as a command rather
/// than echoed as chat; doing the classification in a pure function keeps it
/// testable and keeps the leading-`/` contract (§3.4) in one place.
/// What: returns `None` when `input` does not start with `/`; otherwise lowercases
/// the first whitespace-delimited token (sans the `/`) and maps it to the
/// matching [`SlashCommand`], falling back to [`SlashCommand::Unknown`] carrying
/// that token.
/// Test: `parse_slash_recognises_known_commands`,
/// `parse_slash_classifies_unknown`, `parse_slash_ignores_plain_text`.
pub fn parse_slash(input: &str) -> Option<SlashCommand> {
    let rest = input.strip_prefix('/')?;
    let word = rest.split_whitespace().next().unwrap_or("").to_lowercase();
    let cmd = match word.as_str() {
        "help" => SlashCommand::Help,
        "new" => SlashCommand::New,
        "sessions" => SlashCommand::Sessions,
        "attach" => SlashCommand::Attach,
        "stop" => SlashCommand::Stop,
        "resume" => SlashCommand::Resume,
        "kill" => SlashCommand::Kill,
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
/// selection when the input is empty (↑/↓ recall history when it is not);
/// printable keys edit the buffer; Backspace deletes; Esc clears; Enter submits
/// (echoing to history — Child #1 does not dispatch). Returns any submitted line
/// so the caller can record a last-action note; slash lines are returned too
/// (the caller may `parse_slash` them) but are not acted on here.
/// Test: `handle_key_*` branch tests in `super::tests`.
pub fn handle_key(state: &mut CoordinatorState, key: KeyEvent) -> Option<String> {
    let input_empty = state.input.is_empty();
    if is_quit(&key, input_empty) {
        state.should_exit = true;
        return None;
    }

    match key.code {
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
        KeyCode::Enter => state.submit_input(),
        KeyCode::Char(c) => {
            state.push_char(c);
            None
        }
        _ => None,
    }
}
