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
    CONTROLLER_STATUS, DAEMON_REACHABLE, DAEMON_UNREACHABLE, INPUT_PROMPT, STATUS_BAR_HINT,
    daemon_indicator, input_line, status_bar_text,
};
use super::poll::{coord_poll_daemon, coord_session_to_entry};
use super::rows::{
    COLUMN_SEPARATOR, CONTROLLER_GLYPH, SESSION_GLYPH, active_row_two_col, controller_bullet_row,
    session_row, session_short_id,
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
    assert_eq!(state.selected, 0, "controller selected at start");
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
    state.selected = 2; // selection sits on the last session

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
        state.selected, 0,
        "clearing the list clamps the selection back to the controller row"
    );
}
