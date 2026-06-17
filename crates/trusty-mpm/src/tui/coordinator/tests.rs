//! Unit tests for the coordinator TUI skeleton (Child #1).
//!
//! Why: the spec's pure/unit test tier (DOC-13 §9) requires the row-builders,
//! the selection clamp, and the slash-command parse to be verified WITHOUT a
//! terminal or a daemon. These tests exercise exactly that surface.
//! What: covers row construction, `CoordinatorState` selection/history, the
//! `parse_slash` stub (incl. leading-`/` detection), key dispatch branches, and
//! the layout text helpers.
//! Test: this IS the test module.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::events::{SlashCommand, handle_key, is_quit, parse_slash};
use super::layout::{
    CONTROLLER_STATUS, INPUT_PROMPT, STATUS_BAR_HINT, input_line, status_bar_text,
};
use super::rows::{
    COLUMN_SEPARATOR, CONTROLLER_GLYPH, SESSION_GLYPH, active_row_two_col, controller_bullet_row,
    session_row, session_short_id,
};
use super::state::{CoordinatorState, INPUT_HISTORY_LIMIT, SessionEntry};

/// Render a ratatui `Line` to a flat string for content assertions.
fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

// ---- rows -----------------------------------------------------------------

#[test]
fn controller_bullet_row_renders_glyph_label_and_status() {
    let line = controller_bullet_row("idle · ready", false);
    let text = line_text(&line);
    assert!(
        text.starts_with(CONTROLLER_GLYPH),
        "expected leading bullet: {text}"
    );
    assert!(text.contains("controller"), "missing label: {text}");
    assert!(text.contains("idle · ready"), "missing status: {text}");
}

#[test]
fn controller_bullet_row_selected_is_still_well_formed() {
    // The selected variant changes style, not text content.
    let plain = line_text(&controller_bullet_row("ready", false));
    let selected = line_text(&controller_bullet_row("ready", true));
    assert_eq!(plain, selected);
}

#[test]
fn session_row_renders_bullet_and_prefix() {
    let text = line_text(&session_row("4f9ca1b2", "aipowerranking", "Active"));
    assert!(
        text.starts_with(SESSION_GLYPH),
        "expected hollow bullet: {text}"
    );
    assert!(text.contains("4f9ca1b2"), "missing short id: {text}");
    assert!(text.contains("aipowerranking"), "missing prefix: {text}");
    assert!(text.contains("Active"), "missing status: {text}");
}

#[test]
fn active_row_two_col_splits_on_bar() {
    let text = line_text(&active_row_two_col("4f9ca1b2", "Running tests"));
    assert!(
        text.contains(COLUMN_SEPARATOR),
        "missing column separator: {text}"
    );
    // The id is left of the bar, the summary right of it.
    let (left, right) = text
        .split_once(COLUMN_SEPARATOR)
        .expect("a bar separator must be present");
    assert!(left.contains("4f9ca1b2"), "id not in left column: {left}");
    assert!(
        right.contains("Running tests"),
        "summary not in right column: {right}"
    );
}

#[test]
fn session_short_id_truncates() {
    assert_eq!(session_short_id("4f9ca1b2deadbeef"), "4f9ca1b2");
    // Shorter-than-8 ids are returned whole.
    assert_eq!(session_short_id("abc"), "abc");
    assert_eq!(session_short_id(""), "");
}

// ---- state: selection clamp (boundary cases) ------------------------------

#[test]
fn clamp_selection_empty_list() {
    // Empty session list: only the controller row exists (row 0).
    let mut state = CoordinatorState::default();
    assert_eq!(state.row_count(), 1);
    state.selected = 9;
    state.clamp_selection();
    assert_eq!(state.selected, 0, "clamp to the sole controller row");
}

#[test]
fn clamp_selection_single_row() {
    // Controller + exactly one session → valid indices are 0 and 1.
    let mut state = CoordinatorState::default();
    state
        .sessions
        .push(SessionEntry::new("id", "p", "Active", "s"));
    assert_eq!(state.row_count(), 2);
    state.selected = 1;
    state.clamp_selection();
    assert_eq!(state.selected, 1, "valid selection is preserved");
    state.selected = 5;
    state.clamp_selection();
    assert_eq!(state.selected, 1, "past-end clamps to last session");
}

#[test]
fn clamp_selection_past_end() {
    let mut state = CoordinatorState::skeleton(); // controller + 2 sessions
    assert_eq!(state.row_count(), 3);
    state.selected = 99;
    state.clamp_selection();
    assert_eq!(state.selected, 2, "clamps to the last valid row index");
}

#[test]
fn selection_movement_saturates() {
    let mut state = CoordinatorState::skeleton(); // rows 0..=2
    state.select_up(); // already at 0
    assert_eq!(state.selected, 0);
    state.select_down();
    assert_eq!(state.selected, 1);
    state.select_down();
    assert_eq!(state.selected, 2);
    state.select_down(); // saturates at last
    assert_eq!(state.selected, 2);
    state.select_up();
    assert_eq!(state.selected, 1);
}

#[test]
fn controller_is_selected_at_row_zero() {
    let mut state = CoordinatorState::skeleton();
    assert!(state.controller_selected());
    state.selected = 1;
    assert!(!state.controller_selected());
}

#[test]
fn selected_session_offsets_by_controller_row() {
    let mut state = CoordinatorState::skeleton();
    assert!(
        state.selected_session().is_none(),
        "row 0 is the controller"
    );
    state.selected = 1;
    assert_eq!(state.selected_session().unwrap().prefix, "aipowerranking");
    state.selected = 2;
    assert_eq!(state.selected_session().unwrap().prefix, "genealogy");
}

#[test]
fn default_state_has_placeholder_rows() {
    let state = CoordinatorState::skeleton();
    assert_eq!(state.sessions.len(), 2);
    assert_eq!(state.selected, 0);
}

// ---- state: input + history ring ------------------------------------------

#[test]
fn input_edits_buffer() {
    let mut state = CoordinatorState::default();
    state.push_char('h');
    state.push_char('i');
    assert_eq!(state.input, "hi");
    state.backspace();
    assert_eq!(state.input, "h");
    state.clear_input();
    assert!(state.input.is_empty());
}

#[test]
fn submit_records_history() {
    let mut state = CoordinatorState::default();
    state.input = "  status please  ".to_string();
    let submitted = state.submit_input();
    assert_eq!(
        submitted.as_deref(),
        Some("status please"),
        "trims and returns"
    );
    assert_eq!(state.history, vec!["status please".to_string()]);
    assert!(state.input.is_empty(), "buffer cleared after submit");
}

#[test]
fn submit_empty_input_is_noop() {
    let mut state = CoordinatorState::default();
    state.input = "   ".to_string();
    assert!(state.submit_input().is_none());
    assert!(state.history.is_empty());
}

#[test]
fn history_ring_is_bounded() {
    let mut state = CoordinatorState::default();
    for i in 0..(INPUT_HISTORY_LIMIT + 5) {
        state.input = format!("msg{i}");
        let _ = state.submit_input();
    }
    assert_eq!(state.history.len(), INPUT_HISTORY_LIMIT, "ring is capped");
    // Oldest entries were evicted; the newest is retained.
    assert_eq!(
        state.history.last().unwrap(),
        &format!("msg{}", INPUT_HISTORY_LIMIT + 4)
    );
    assert_eq!(state.history.first().unwrap(), "msg5", "oldest dropped");
}

#[test]
fn history_recall_walks_entries() {
    let mut state = CoordinatorState::default();
    for m in ["one", "two", "three"] {
        state.input = m.to_string();
        let _ = state.submit_input();
    }
    state.history_prev();
    assert_eq!(state.input, "three");
    state.history_prev();
    assert_eq!(state.input, "two");
    state.history_next();
    assert_eq!(state.input, "three");
    state.history_next();
    assert!(state.input.is_empty(), "stepping past newest clears buffer");
}

// ---- events: slash parsing (the stub) -------------------------------------

#[test]
fn parse_slash_recognises_known_commands() {
    assert_eq!(parse_slash("/help"), Some(SlashCommand::Help));
    assert_eq!(parse_slash("/new repo=x"), Some(SlashCommand::New));
    assert_eq!(parse_slash("/sessions"), Some(SlashCommand::Sessions));
    assert_eq!(parse_slash("/attach"), Some(SlashCommand::Attach));
    assert_eq!(parse_slash("/stop"), Some(SlashCommand::Stop));
    assert_eq!(parse_slash("/resume"), Some(SlashCommand::Resume));
    assert_eq!(parse_slash("/kill"), Some(SlashCommand::Kill));
    // Case-insensitive on the verb.
    assert_eq!(parse_slash("/HELP"), Some(SlashCommand::Help));
}

#[test]
fn parse_slash_classifies_unknown() {
    assert_eq!(
        parse_slash("/frobnicate now"),
        Some(SlashCommand::Unknown("frobnicate".to_string()))
    );
}

#[test]
fn parse_slash_ignores_plain_text() {
    assert_eq!(parse_slash("hello there"), None);
    assert_eq!(parse_slash("@aipowerranking: run tests"), None);
    assert_eq!(parse_slash(""), None);
}

// ---- events: key dispatch -------------------------------------------------

#[test]
fn quit_on_q_when_empty() {
    assert!(is_quit(&key(KeyCode::Char('q')), true));
    let mut state = CoordinatorState::skeleton();
    handle_key(&mut state, key(KeyCode::Char('q')));
    assert!(state.should_exit);
}

#[test]
fn quit_on_ctrl_c() {
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(is_quit(&ctrl_c, true));
    assert!(is_quit(&ctrl_c, false), "Ctrl-C quits even mid-message");
    let mut state = CoordinatorState::default();
    state.input = "typing".to_string();
    handle_key(&mut state, ctrl_c);
    assert!(state.should_exit);
}

#[test]
fn q_in_message_is_not_quit() {
    assert!(!is_quit(&key(KeyCode::Char('q')), false));
    let mut state = CoordinatorState::default();
    state.input = "fi".to_string();
    handle_key(&mut state, key(KeyCode::Char('q')));
    assert!(!state.should_exit, "q is editing text, not quitting");
    assert_eq!(state.input, "fiq");
}

#[test]
fn handle_key_moves_selection_when_input_empty() {
    let mut state = CoordinatorState::skeleton();
    handle_key(&mut state, key(KeyCode::Down));
    assert_eq!(state.selected, 1);
    handle_key(&mut state, key(KeyCode::Char('j')));
    assert_eq!(state.selected, 2);
    handle_key(&mut state, key(KeyCode::Char('k')));
    assert_eq!(state.selected, 1);
    handle_key(&mut state, key(KeyCode::Up));
    assert_eq!(state.selected, 0);
}

#[test]
fn handle_key_enter_submits_and_returns_line() {
    let mut state = CoordinatorState::default();
    state.input = "spin up a session".to_string();
    let submitted = handle_key(&mut state, key(KeyCode::Enter));
    assert_eq!(submitted.as_deref(), Some("spin up a session"));
    assert_eq!(state.history.len(), 1);
}

#[test]
fn handle_key_up_recalls_history_when_input_nonempty() {
    let mut state = CoordinatorState::default();
    state.input = "earlier".to_string();
    let _ = state.submit_input();
    // Type something, then ↑ should recall history rather than move selection.
    state.push_char('x');
    handle_key(&mut state, key(KeyCode::Up));
    assert_eq!(state.input, "earlier");
}

// ---- layout text ----------------------------------------------------------

#[test]
fn input_line_shows_prompt_and_cursor() {
    let mut state = CoordinatorState::default();
    state.input = "hi".to_string();
    let line = input_line(&state);
    assert!(line.starts_with(INPUT_PROMPT), "missing prompt: {line}");
    assert!(line.ends_with('_'), "missing cursor glyph: {line}");
    assert!(line.contains("hi"));
}

#[test]
fn status_bar_lists_keys() {
    let bar = status_bar_text();
    assert_eq!(bar, STATUS_BAR_HINT);
    assert!(bar.contains("/help"));
    assert!(bar.contains("quit"));
}

#[test]
fn controller_status_is_synthetic() {
    assert!(CONTROLLER_STATUS.contains("ready"));
}
