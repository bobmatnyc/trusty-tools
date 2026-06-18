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

use super::banner::{
    GLYPH_ACTIVE, GLYPH_UNREACHABLE, HELP_HINT, ProbeOutcome, USER_PALACE, compose_banner,
    ensure_user_palace_with, probe_client, probe_memory, probe_search,
};
use super::events::{SlashCommand, handle_key, is_quit, parse_slash};
use super::layout::{
    CATALOG_STALE_HINT, CONTROLLER_STATUS, DAEMON_REACHABLE, DAEMON_UNREACHABLE, INPUT_PROMPT,
    STATUS_BAR_HINT, catalog_indicator, daemon_indicator, input_line, status_bar_text,
};
use super::nav::{ListNav, MIN_VISIBLE_ROWS, PAGE_JUMP};
use super::poll::{coord_poll_daemon, coord_session_to_entry};
use super::rows::{
    COLUMN_SEPARATOR, CONTROLLER_GLYPH, SESSION_GLYPH, active_row_two_col, controller_bullet_row,
    numbered_session_row, session_row, session_short_id,
};
use super::state::{CoordinatorState, INPUT_HISTORY_LIMIT, SessionEntry};
use crate::client::{CoordinatorSession, DaemonClient};

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
fn numbered_session_row_prefixes_number() {
    // STUI-1: the list is numbered (1, 2, 3, …); the row text must lead with the
    // 1-based list number while still carrying the standard session spans.
    let text = line_text(&numbered_session_row(
        3,
        "4f9ca1b2",
        "aipowerranking",
        "Active",
    ));
    assert!(
        text.trim_start().starts_with("3."),
        "expected a leading list number `3.`: {text}"
    );
    // The bullet, id, prefix, and status are still present after the number.
    assert!(text.contains(SESSION_GLYPH), "missing bullet: {text}");
    assert!(text.contains("4f9ca1b2"), "missing short id: {text}");
    assert!(text.contains("aipowerranking"), "missing prefix: {text}");
    assert!(text.contains("Active"), "missing status: {text}");
}

#[test]
fn session_short_id_truncates() {
    assert_eq!(session_short_id("4f9ca1b2deadbeef"), "4f9ca1b2");
    // Shorter-than-8 ids are returned whole.
    assert_eq!(session_short_id("abc"), "abc");
    assert_eq!(session_short_id(""), "");
}

// ---- nav: pure ListNav navigation (STUI-1) --------------------------------

#[test]
fn min_visible_rows_is_five() {
    // The spec fixes a five-row floor for the list viewport.
    assert_eq!(MIN_VISIBLE_ROWS, 5);
    // Page scrolling jumps by a page (== the visible-row floor in the pure model).
    assert_eq!(PAGE_JUMP, MIN_VISIBLE_ROWS);
}

#[test]
fn nav_default_selects_controller_row() {
    // The list always has at least the controller row; row 0 is selected and the
    // offset starts at the top.
    let nav = ListNav::default();
    assert_eq!(nav.selected(), 0);
    assert_eq!(nav.offset(), 0);
}

#[test]
fn nav_up_down_saturate() {
    // A 4-row list (len 4): down saturates at row 3, up saturates at row 0.
    let mut nav = ListNav::default();
    nav.sync_len(4);
    nav.up(); // already at 0
    assert_eq!(nav.selected(), 0, "up saturates at the top");
    nav.down();
    nav.down();
    nav.down();
    assert_eq!(nav.selected(), 3, "stepped to the last row");
    nav.down(); // saturates
    assert_eq!(nav.selected(), 3, "down saturates at the last row");
    nav.up();
    assert_eq!(nav.selected(), 2);
}

#[test]
fn nav_select_clamps_into_bounds() {
    // `select` past the end clamps to the last valid row.
    let mut nav = ListNav::default();
    nav.sync_len(3); // rows 0..=2
    nav.select(99);
    assert_eq!(nav.selected(), 2, "select clamps to the last row");
    nav.select(1);
    assert_eq!(nav.selected(), 1, "an in-bounds select is honoured");
}

#[test]
fn page_down_jumps_by_page_size() {
    // A long list (len 20): Page-Down advances by exactly PAGE_JUMP rows.
    let mut nav = ListNav::default();
    nav.sync_len(20);
    nav.page_down();
    assert_eq!(nav.selected(), PAGE_JUMP, "page down jumps by a page");
}

#[test]
fn page_up_jumps_by_page_size() {
    let mut nav = ListNav::default();
    nav.sync_len(20);
    nav.select(2 * PAGE_JUMP);
    nav.page_up();
    assert_eq!(nav.selected(), PAGE_JUMP, "page up jumps back by a page");
}

#[test]
fn page_down_saturates_at_bottom() {
    // A short list (len 3): Page-Down cannot move past the last row.
    let mut nav = ListNav::default();
    nav.sync_len(3); // rows 0..=2
    nav.page_down();
    assert_eq!(nav.selected(), 2, "page down saturates at the last row");
}

#[test]
fn page_up_saturates_at_top() {
    let mut nav = ListNav::default();
    nav.sync_len(20);
    nav.select(2); // within one page of the top
    nav.page_up();
    assert_eq!(nav.selected(), 0, "page up saturates at the top");
}

#[test]
fn nav_sync_len_preserves_selection_and_offset() {
    // A refresh that GROWS (or keeps) the list must leave the selection AND the
    // scroll offset exactly where the operator left them.
    let mut nav = ListNav::default();
    nav.sync_len(20);
    nav.select(12);
    // Force a non-zero offset by scrolling the viewport via the render seam.
    *nav.state_mut().offset_mut() = 8;
    assert_eq!(nav.selected(), 12);
    assert_eq!(nav.offset(), 8);

    // Grow the list: cursor + offset are untouched (both still in bounds).
    nav.sync_len(25);
    assert_eq!(nav.selected(), 12, "grown list preserves the selection");
    assert_eq!(nav.offset(), 8, "grown list preserves the scroll offset");

    // Unchanged length: still preserved.
    nav.sync_len(25);
    assert_eq!(nav.selected(), 12);
    assert_eq!(nav.offset(), 8);
}

#[test]
fn nav_sync_len_clamps_on_shrink() {
    // A refresh that SHRINKS the list past the cursor/offset must clamp both
    // inward so neither indexes past the new end.
    let mut nav = ListNav::default();
    nav.sync_len(20);
    nav.select(18);
    *nav.state_mut().offset_mut() = 15;

    // Shrink to 5 rows (0..=4): selection and offset clamp to the last row.
    nav.sync_len(5);
    assert_eq!(
        nav.selected(),
        4,
        "selection clamps to the last row on shrink"
    );
    assert!(
        nav.offset() <= 4,
        "offset clamps within the shrunk list: {}",
        nav.offset()
    );

    // Shrink to the controller-only list (len 1): everything pins to row 0.
    nav.sync_len(1);
    assert_eq!(nav.selected(), 0);
    assert_eq!(nav.offset(), 0);
}

#[test]
fn nav_sync_len_floors_at_one_row() {
    // The controller row always exists, so a zero row_count still yields len 1
    // and a valid row-0 selection (never an underflow).
    let mut nav = ListNav::default();
    nav.sync_len(0);
    assert_eq!(
        nav.selected(),
        0,
        "an empty list still has the controller row"
    );
}

// ---- state: selection clamp (boundary cases) ------------------------------

#[test]
fn clamp_selection_empty_list() {
    // Empty session list: only the controller row exists (row 0).
    let mut state = CoordinatorState::default();
    assert_eq!(state.row_count(), 1);
    state.select_row(9);
    state.clamp_selection();
    assert_eq!(state.selected(), 0, "clamp to the sole controller row");
}

#[test]
fn clamp_selection_single_row() {
    // Controller + exactly one session → valid indices are 0 and 1.
    let mut state = CoordinatorState::default();
    state
        .sessions
        .push(SessionEntry::new("id", "p", "Active", "s"));
    assert_eq!(state.row_count(), 2);
    state.select_row(1);
    state.clamp_selection();
    assert_eq!(state.selected(), 1, "valid selection is preserved");
    state.select_row(5);
    state.clamp_selection();
    assert_eq!(state.selected(), 1, "past-end clamps to last session");
}

#[test]
fn clamp_selection_past_end() {
    let mut state = CoordinatorState::skeleton(); // controller + 2 sessions
    assert_eq!(state.row_count(), 3);
    state.select_row(99);
    state.clamp_selection();
    assert_eq!(state.selected(), 2, "clamps to the last valid row index");
}

#[test]
fn selection_movement_saturates() {
    let mut state = CoordinatorState::skeleton(); // rows 0..=2
    state.select_up(); // already at 0
    assert_eq!(state.selected(), 0);
    state.select_down();
    assert_eq!(state.selected(), 1);
    state.select_down();
    assert_eq!(state.selected(), 2);
    state.select_down(); // saturates at last
    assert_eq!(state.selected(), 2);
    state.select_up();
    assert_eq!(state.selected(), 1);
}

#[test]
fn select_row_clamps_into_bounds() {
    // `select_row` re-syncs the live row count then clamps into 0..row_count.
    let mut state = CoordinatorState::skeleton(); // controller + 2 sessions → rows 0..=2
    state.select_row(1);
    assert_eq!(state.selected(), 1, "an in-bounds row is honoured");
    state.select_row(99);
    assert_eq!(
        state.selected(),
        2,
        "past-end select clamps to the last row"
    );
}

#[test]
fn clamp_selection_preserves_valid_selection() {
    // STUI-1: a refresh that leaves the selection in bounds must NOT move it back
    // to the controller — the operator stays on the row they chose.
    let mut state = CoordinatorState::skeleton(); // rows 0..=2
    state.select_row(2);
    // Simulate a poll that keeps the same number of rows.
    state.clamp_selection();
    assert_eq!(
        state.selected(),
        2,
        "a still-valid selection survives the refresh"
    );

    // A poll that GROWS the list also preserves the selection.
    state
        .sessions
        .push(SessionEntry::new("new", "p", "Active", "s"));
    state.clamp_selection();
    assert_eq!(state.selected(), 2, "grown list keeps the cursor in place");
}

#[test]
fn page_scroll_moves_by_page_and_saturates() {
    // Build a controller + enough sessions that a page jump does not hit the end.
    let mut state = CoordinatorState::live();
    for i in 0..20 {
        state
            .sessions
            .push(SessionEntry::new(format!("id{i}"), "p", "Active", "s"));
    }
    // rows 0..=20 (controller + 20 sessions).
    state.page_down();
    assert_eq!(state.selected(), PAGE_JUMP, "page down jumps by a page");
    state.page_down();
    assert_eq!(
        state.selected(),
        2 * PAGE_JUMP,
        "second page down jumps again"
    );
    state.page_up();
    assert_eq!(state.selected(), PAGE_JUMP, "page up jumps back by a page");
    // Page-up from within a page of the top saturates at the controller row.
    state.select_row(2);
    state.page_up();
    assert_eq!(
        state.selected(),
        0,
        "page up saturates at the controller row"
    );
}

#[test]
fn request_repoll_sets_and_take_clears() {
    // STUI-1: a mutation requests a re-poll; the run loop consumes it exactly once.
    let mut state = CoordinatorState::live();
    assert!(!state.take_repoll(), "no re-poll requested initially");
    state.request_repoll();
    assert!(state.take_repoll(), "the requested re-poll fires once");
    assert!(
        !state.take_repoll(),
        "a consumed re-poll does not re-fire on the next tick"
    );
}

#[test]
fn controller_is_selected_at_row_zero() {
    let mut state = CoordinatorState::skeleton();
    assert!(state.controller_selected());
    state.select_row(1);
    assert!(!state.controller_selected());
}

#[test]
fn selected_session_offsets_by_controller_row() {
    let mut state = CoordinatorState::skeleton();
    assert!(
        state.selected_session().is_none(),
        "row 0 is the controller"
    );
    state.select_row(1);
    assert_eq!(state.selected_session().unwrap().prefix, "aipowerranking");
    state.select_row(2);
    assert_eq!(state.selected_session().unwrap().prefix, "genealogy");
}

#[test]
fn default_state_has_placeholder_rows() {
    let state = CoordinatorState::skeleton();
    assert_eq!(state.sessions.len(), 2);
    assert_eq!(state.selected(), 0);
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

#[test]
fn mutating_commands_are_classified() {
    // STUI-1: only fleet-mutating commands force an immediate re-poll.
    assert!(SlashCommand::New.is_mutating());
    assert!(SlashCommand::Kill.is_mutating());
    assert!(SlashCommand::Stop.is_mutating());
    assert!(SlashCommand::Resume.is_mutating());
    // Read-only commands must NOT trigger a refresh.
    assert!(!SlashCommand::Help.is_mutating());
    assert!(!SlashCommand::Sessions.is_mutating());
    assert!(!SlashCommand::Attach.is_mutating());
    assert!(!SlashCommand::Unknown("frob".to_string()).is_mutating());
}

#[test]
fn parse_slash_bare_slash_is_not_a_command() {
    // A lone `/` (or `/` followed only by whitespace) has no command word, so it
    // must NOT classify as `Unknown("")` — it falls through to chat as `None`.
    assert_eq!(parse_slash("/"), None);
    assert_eq!(parse_slash("/   "), None);
    assert_eq!(parse_slash("/\t"), None);
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
    assert_eq!(state.selected(), 1);
    handle_key(&mut state, key(KeyCode::Char('j')));
    assert_eq!(state.selected(), 2);
    handle_key(&mut state, key(KeyCode::Char('k')));
    assert_eq!(state.selected(), 1);
    handle_key(&mut state, key(KeyCode::Up));
    assert_eq!(state.selected(), 0);
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
fn handle_key_page_keys_scroll_regardless_of_buffer() {
    // STUI-1: Page-Up/Page-Down move the selection even with text in the buffer
    // (they have no text-editing meaning).
    let mut state = CoordinatorState::live();
    for i in 0..20 {
        state
            .sessions
            .push(SessionEntry::new(format!("id{i}"), "p", "Active", "s"));
    }
    state.input = "half-typed".to_string(); // a non-empty buffer
    handle_key(&mut state, key(KeyCode::PageDown));
    assert_eq!(
        state.selected(),
        PAGE_JUMP,
        "page down scrolls even mid-message"
    );
    handle_key(&mut state, key(KeyCode::PageUp));
    assert_eq!(state.selected(), 0, "page up scrolls back even mid-message");
    assert_eq!(state.input, "half-typed", "page keys never edit the buffer");
}

#[test]
fn handle_key_enter_mutating_command_requests_repoll() {
    // STUI-1: submitting a mutating slash command flags an immediate re-poll;
    // a plain chat line or a read-only command does not.
    let mut state = CoordinatorState::live();
    state.input = "/new repo=x".to_string();
    let submitted = handle_key(&mut state, key(KeyCode::Enter));
    assert_eq!(submitted.as_deref(), Some("/new repo=x"));
    assert!(
        state.take_repoll(),
        "a mutating slash command requests a re-poll"
    );

    // A read-only command does NOT request a re-poll.
    state.input = "/help".to_string();
    handle_key(&mut state, key(KeyCode::Enter));
    assert!(
        !state.take_repoll(),
        "a read-only command must not force a refresh"
    );

    // Plain chat does NOT request a re-poll.
    state.input = "just chatting".to_string();
    handle_key(&mut state, key(KeyCode::Enter));
    assert!(!state.take_repoll(), "plain chat must not force a refresh");
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

// ---- Child #2: live polling — pure session mapping ------------------------

/// Build a `CoordinatorSession` wire value for the mapping tests.
fn coord_session(id: &str, prefix: &str, status: &str, recent: &[&str]) -> CoordinatorSession {
    CoordinatorSession {
        id: id.to_string(),
        name: format!("tmpm-{prefix}"),
        prefix: prefix.to_string(),
        workdir: "/tmp".to_string(),
        status: status.to_string(),
        active_delegations: 0,
        recent_output: recent.iter().map(|s| s.to_string()).collect(),
        // #1275 fields are not exercised by these DOC-13 mapping tests; default
        // them (None / false) so the wire-shape change stays additive here.
        ..Default::default()
    }
}

#[test]
fn coord_session_to_entry_summarizes_last_output() {
    // The summary is the LAST non-empty line of recent_output (a trailing blank
    // line must be skipped), per DOC-13 §6.2 option C.
    let s = coord_session(
        "4f9ca1b2deadbeef", // pragma: allowlist secret — fake session id
        "aipowerranking",
        "Active",
        &["", "Running tests"],
    );
    let entry = coord_session_to_entry(s);
    assert_eq!(entry.summary, "Running tests");
    assert_eq!(entry.short_id, "4f9ca1b2");
    assert_eq!(entry.prefix, "aipowerranking");
    assert_eq!(entry.status, "Active");
}

#[test]
fn coord_session_to_entry_falls_back_to_status() {
    // With no recent_output the summary falls back to the status word.
    let s = coord_session("7b2ec0d1cafef00d", "genealogy", "Paused", &[]);
    let entry = coord_session_to_entry(s);
    assert_eq!(entry.summary, "Paused", "empty pane → status word");
    // An all-whitespace pane tail also falls back to the status word.
    let blank = coord_session("abc", "p", "Idle", &["   ", "\t"]);
    assert_eq!(coord_session_to_entry(blank).summary, "Idle");
}

#[test]
fn coord_session_to_entry_derives_short_id() {
    // Long UUIDs truncate to 8 hex; short ids pass through whole.
    // pragma: allowlist secret — fake session id
    let long = coord_session("0123456789abcdef0123", "p", "Active", &["x"]);
    assert_eq!(coord_session_to_entry(long).short_id, "01234567");
    let short = coord_session("abc", "p", "Active", &["x"]);
    assert_eq!(coord_session_to_entry(short).short_id, "abc");
}

#[test]
fn live_state_starts_empty_and_unreachable() {
    let state = CoordinatorState::live();
    assert!(
        state.sessions.is_empty(),
        "no placeholder rows in live mode"
    );
    assert!(
        !state.daemon_reachable,
        "daemon presumed down until first poll"
    );
    assert_eq!(state.selected(), 0, "controller selected at start");
}

#[test]
fn daemon_indicator_reflects_reachability() {
    let mut state = CoordinatorState::live();
    assert_eq!(daemon_indicator(&state), DAEMON_UNREACHABLE);
    state.daemon_reachable = true;
    assert_eq!(daemon_indicator(&state), DAEMON_REACHABLE);
    // The two indicators are visually distinct.
    assert_ne!(DAEMON_REACHABLE, DAEMON_UNREACHABLE);
}

#[test]
fn catalog_indicator_reflects_staleness() {
    // The HR-3 rebuild offer shows ONLY when the daemon is reachable AND reports
    // the catalog is stale; otherwise the status bar stays clean.
    let mut state = CoordinatorState::live();
    // Down daemon, no flag → nothing.
    assert_eq!(catalog_indicator(&state), None);

    // Stale flag but daemon DOWN → still nothing (a down daemon's flag is moot).
    state.catalog_stale = true;
    assert_eq!(catalog_indicator(&state), None);

    // Reachable + stale → the rebuild-offer hint appears.
    state.daemon_reachable = true;
    assert_eq!(catalog_indicator(&state), Some(CATALOG_STALE_HINT));

    // Reachable + fresh → nothing.
    state.catalog_stale = false;
    assert_eq!(catalog_indicator(&state), None);

    // The hint names the apply command so the offer is actionable.
    assert!(CATALOG_STALE_HINT.contains("tm catalog apply"));
}

// ---- Child #2: live polling — daemon-down branch (async) ------------------

/// Reserve, then immediately release, a loopback port so a client pointed at it
/// is guaranteed to be refused (nothing is listening).
///
/// Why: the daemon-down test needs a `DaemonClient` whose probe deterministically
/// fails. Binding `127.0.0.1:0` lets the OS hand us a free port; dropping the
/// listener frees it again, so a connect to it is refused rather than hanging on
/// an open-but-silent socket.
/// What: binds an ephemeral TCP listener, reads its local addr, drops it, and
/// returns `http://127.0.0.1:<port>`.
/// Test: consumed by `poll_marks_unreachable_clears_sessions`.
fn dead_loopback_url() -> String {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback port");
    let port = listener.local_addr().expect("read bound local addr").port();
    drop(listener); // release the port so a later connect is refused
    format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn poll_marks_unreachable_clears_sessions() {
    // Why: `coord_poll_daemon` must, on an unreachable daemon, clear any rows it
    // previously showed and flip `daemon_reachable` to false (DOC-13 §6) so the
    // operator never sees stale sessions behind a `daemon ✗` indicator. The live
    // (daemon-UP) branch needs a running daemon and is exercised manually / by
    // the daemon suites, not here.
    //
    // Determinism note: `coord_poll_daemon` self-heals via `rediscover`, which
    // re-resolves the URL from the lock file on a failed probe. If a real daemon
    // is discoverable on this machine the function will (correctly) re-point to
    // it and report reachable, so we only assert the daemon-down outcome when
    // discovery itself yields no reachable daemon — which is always the case in
    // CI. We pin the client to discovery's own result so the no-op `rediscover`
    // path is taken and the probe is the sole determinant.
    let discovered = crate::core::resolve_daemon_url(None);
    let probe = DaemonClient::new(discovered.clone());
    if probe.is_healthy().await {
        // A real daemon is up locally; the down-branch is unreachable through the
        // self-healing poll, so there is nothing deterministic to assert here.
        // CI (no daemon) always exercises the assertions below.
        eprintln!(
            "skipping daemon-down assertions: a reachable daemon was discovered at {discovered}"
        );
        return;
    }

    // No daemon is discoverable. Pin the client at a guaranteed-dead port; since
    // `discovered` is itself unreachable, `rediscover` cannot rescue the probe.
    let mut state = CoordinatorState::live();
    state.daemon_reachable = true; // pretend a prior poll had succeeded
    state.sessions = vec![
        SessionEntry::new("4f9ca1b2", "aipowerranking", "Active", "Running tests"),
        SessionEntry::new("7b2ec0d1", "genealogy", "Idle", "Idle"),
    ];
    state.select_row(2); // selection sits on the last session

    let mut client = DaemonClient::new(dead_loopback_url());
    coord_poll_daemon(&mut state, &mut client).await;

    assert!(
        !state.daemon_reachable,
        "an unreachable daemon must flip daemon_reachable to false"
    );
    assert!(
        state.sessions.is_empty(),
        "a daemon-down poll must clear stale session rows"
    );
    assert_eq!(
        state.selected(),
        0,
        "clearing the list clamps the selection back to the controller row"
    );
}

// ---- STUI-0 banner --------------------------------------------------------

/// Why: §3.1 fixes the banner's first line to `trusty-mpm sessions v<version>`;
/// the composition must embed the crate `CARGO_PKG_VERSION` verbatim so the
/// operator sees which build is running.
/// What: composes a banner and asserts the version line is present with the
/// compile-time version.
/// Test: this IS the test.
#[test]
fn compose_banner_includes_name_and_version() {
    let banner = compose_banner(
        env!("CARGO_PKG_VERSION"),
        &ProbeOutcome::active(None),
        &ProbeOutcome::active(None),
        Some(3),
    );
    let expected = format!("trusty-mpm sessions  v{}", env!("CARGO_PKG_VERSION"));
    assert!(
        banner.lines().next() == Some(expected.as_str()),
        "banner must lead with the name + version line, got:\n{banner}"
    );
}

/// Why: §3.1 pins the active glyph `●` and the word `active` for a reachable
/// backplane; both memory and search lines must render them.
/// What: composes an all-active banner and asserts each service line carries
/// the active glyph + word.
/// Test: this IS the test.
#[test]
fn compose_banner_active_shows_glyphs() {
    let banner = compose_banner(
        "1.2.3",
        &ProbeOutcome::active(None),
        &ProbeOutcome::active(None),
        Some(2),
    );
    assert!(
        banner.contains(&format!("memory  {GLYPH_ACTIVE} active")),
        "active memory must show `● active`, got:\n{banner}"
    );
    assert!(
        banner.contains(&format!("search  {GLYPH_ACTIVE} active")),
        "active search must show `● active`, got:\n{banner}"
    );
}

/// Why: §3.1 error conditions require an unreachable service to render
/// `○ unreachable` (a glyph distinct from active) without aborting — so the
/// composer must emit it for a down probe.
/// What: composes a banner with both services unreachable and asserts the
/// unreachable glyph + word appear for each.
/// Test: this IS the test.
#[test]
fn compose_banner_unreachable_shows_glyphs() {
    let banner = compose_banner(
        "1.2.3",
        &ProbeOutcome::unreachable(None),
        &ProbeOutcome::unreachable(None),
        Some(0),
    );
    assert!(
        banner.contains(&format!("memory  {GLYPH_UNREACHABLE} unreachable")),
        "down memory must show `○ unreachable`, got:\n{banner}"
    );
    assert!(
        banner.contains(&format!("search  {GLYPH_UNREACHABLE} unreachable")),
        "down search must show `○ unreachable`, got:\n{banner}"
    );
}

/// Why: decision D4 confirms memory against the fixed `user` palace; the banner
/// must surface that palace as a parenthetical note so the operator sees WHICH
/// palace was checked.
/// What: composes a banner whose memory outcome carries the `palace: user` note
/// and asserts the rendered memory line includes it.
/// Test: this IS the test.
#[test]
fn compose_banner_active_shows_palace_note() {
    let banner = compose_banner(
        "0.1.0",
        &ProbeOutcome::active(Some(format!("palace: {USER_PALACE}"))),
        &ProbeOutcome::active(None),
        Some(1),
    );
    assert!(
        banner.contains(&format!("(palace: {USER_PALACE})")),
        "memory line must note the `user` palace per D4, got:\n{banner}"
    );
}

/// Why: §3.1 requires the startup line to point operators at `/help`.
/// What: composes a banner and asserts the final line carries the `/help` hint.
/// Test: this IS the test.
#[test]
fn compose_banner_appends_help_hint() {
    let banner = compose_banner(
        "0.1.0",
        &ProbeOutcome::active(None),
        &ProbeOutcome::active(None),
        Some(4),
    );
    let last = banner.lines().last().unwrap_or_default();
    assert!(
        last.ends_with(HELP_HINT),
        "startup line must end with the /help hint, got: {last:?}"
    );
    assert!(
        last.starts_with("4 active sessions"),
        "startup line must lead with the active-session count, got: {last:?}"
    );
}

/// Why: a single active session must read naturally (`1 active session`, not
/// `1 active sessions`); the count line pluralises on the count.
/// What: composes banners for counts 0, 1, and 5 and asserts the exact lead.
/// Test: this IS the test.
#[test]
fn compose_banner_pluralizes_count() {
    let line_for = |n: usize| {
        let banner = compose_banner(
            "0.1.0",
            &ProbeOutcome::active(None),
            &ProbeOutcome::active(None),
            Some(n),
        );
        banner.lines().last().unwrap_or_default().to_string()
    };
    assert!(line_for(0).starts_with("0 active sessions"));
    assert!(line_for(1).starts_with("1 active session  ·"));
    assert!(line_for(5).starts_with("5 active sessions"));
}

/// Why: when the daemon is unreachable at startup the count is unknown; the
/// banner must say so rather than render a misleading `0 active sessions`.
/// What: composes a banner with `active_count = None` and asserts the startup
/// line reports `daemon unreachable` while still carrying the `/help` hint.
/// Test: this IS the test.
#[test]
fn compose_banner_handles_unknown_count() {
    let banner = compose_banner(
        "0.1.0",
        &ProbeOutcome::unreachable(None),
        &ProbeOutcome::unreachable(None),
        None,
    );
    let last = banner.lines().last().unwrap_or_default();
    assert!(
        last.starts_with("daemon unreachable"),
        "unknown count must read `daemon unreachable`, got: {last:?}"
    );
    assert!(
        last.ends_with(HELP_HINT),
        "the /help hint must remain even when the count is unknown, got: {last:?}"
    );
}

/// Why: §3.1 makes probes fail-safe — a down search daemon yields `○ unreachable`
/// and never panics or hangs the TUI.
/// What: probes a guaranteed-dead loopback address and asserts the outcome is
/// inactive.
/// Test: this IS the test.
#[tokio::test]
async fn probe_unreachable_search_is_inactive() {
    let outcome = probe_search(Some(&dead_loopback_url())).await;
    assert!(
        !outcome.active,
        "an unreachable search daemon must probe inactive"
    );
}

/// Why: §3.1 + D4 make the memory probe fail-safe — a down memory daemon yields
/// `○ unreachable` with a reason and never panics.
/// What: probes a guaranteed-dead loopback address and asserts the outcome is
/// inactive and carries a reason note.
/// Test: this IS the test.
#[tokio::test]
async fn probe_unreachable_memory_is_inactive() {
    let outcome = probe_memory(Some(&dead_loopback_url())).await;
    assert!(
        !outcome.active,
        "an unreachable memory daemon must probe inactive"
    );
    assert!(
        outcome.note.is_some(),
        "a degraded memory probe should carry a reason note"
    );
}

/// Why: the print site composes the banner and prints it with `eprintln!`; if the
/// composed banner itself ended with a newline AND `startup_line` also ended with
/// one, the operator would see two blank lines. The contract is a single trailing
/// newline so the print site yields exactly one blank separator.
/// What: composes a banner and asserts it ends with exactly one `\n` (not two and
/// not zero).
/// Test: this IS the test.
#[test]
fn compose_banner_has_single_trailing_newline() {
    let banner = compose_banner(
        "0.1.0",
        &ProbeOutcome::active(None),
        &ProbeOutcome::active(None),
        Some(2),
    );
    assert!(
        banner.ends_with('\n'),
        "composed banner must end with a newline, got:\n{banner:?}"
    );
    assert!(
        !banner.ends_with("\n\n"),
        "composed banner must NOT end with a double newline, got:\n{banner:?}"
    );
}

/// Spawn a one-shot HTTP mock that answers the FIRST request with `status_line`
/// and records whether a SECOND request (the would-be `POST` create) ever
/// arrives, signalling that via the returned watch channel.
///
/// Why: `ensure_user_palace_with` must only POST a create on a real 404. To prove
/// it does NOT create on a non-404 error, the test needs to observe whether a
/// second connection (the POST) is attempted at all.
/// What: binds an ephemeral port; the first accepted connection replies with
/// `status_line`; if a second connection arrives it flips `post_seen` to `true`
/// and replies `200 OK`. Returns the base URL and a receiver for `post_seen`.
/// Test: used by `ensure_user_palace_does_not_create_on_non_404`.
async fn spawn_palace_mock(
    status_line: &'static str,
) -> (String, tokio::sync::watch::Receiver<bool>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        // First request: the GET palace lookup → reply with the given status.
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let body = "{}";
            let resp = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
        // Any SECOND request means a create POST was attempted — record it.
        if let Ok((mut sock, _)) = listener.accept().await {
            let _ = tx.send(true);
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let resp =
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_string();
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    (format!("http://{addr}"), rx)
}

/// Why: the review flagged that `ensure_user_palace` over-eagerly created the
/// palace — any non-2xx GET (including a transient `500`/`503`) fell through to a
/// create POST. The fix gates creation on a *real* 404; any other error status
/// must return `false` WITHOUT attempting a write.
/// What: stubs the palace GET to answer `500`, runs `ensure_user_palace_with`,
/// and asserts it returns `false` AND that no second (POST) request was made.
/// Test: this IS the test.
#[tokio::test]
async fn ensure_user_palace_does_not_create_on_non_404() {
    let (base, post_seen) = spawn_palace_mock("HTTP/1.1 500 Internal Server Error").await;
    let client = probe_client();
    let created = ensure_user_palace_with(&client, &base).await;
    assert!(
        !created,
        "a 500 from the palace GET must NOT confirm the palace as active"
    );
    // Give any erroneous POST a moment to land, then assert none was attempted.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !*post_seen.borrow(),
        "a non-404 GET error must NOT trigger a create POST"
    );
}

/// Why: the complement to the non-404 case — a genuine 404 means the palace is
/// absent, so the banner SHOULD create it idempotently (D4). This pins the one
/// status that is allowed to trigger a write.
/// What: stubs the palace GET to answer `404` (and the create POST to answer
/// `200`), runs `ensure_user_palace_with`, and asserts it returns `true` and that
/// a second (POST) request was observed.
/// Test: this IS the test.
#[tokio::test]
async fn ensure_user_palace_creates_only_on_404() {
    let (base, post_seen) = spawn_palace_mock("HTTP/1.1 404 Not Found").await;
    let client = probe_client();
    let created = ensure_user_palace_with(&client, &base).await;
    assert!(
        created,
        "a 404 GET followed by a 2xx create POST must confirm the palace"
    );
    assert!(
        *post_seen.borrow(),
        "a 404 GET must trigger exactly the create POST"
    );
}
