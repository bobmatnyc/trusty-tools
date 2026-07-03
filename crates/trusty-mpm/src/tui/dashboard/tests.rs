use super::*;
use crate::core::session::{SessionId, SessionStatus};

/// A deterministic test [`SessionId`] derived from a short label.
fn sid(label: &str) -> SessionId {
    let mut bytes = [0u8; 16];
    for (slot, b) in bytes.iter_mut().zip(label.bytes()) {
        *slot = b;
    }
    SessionId(uuid::Uuid::from_bytes(bytes))
}

/// Build a `SessionRow` for tests.
fn session(id: &str, status: SessionStatus, name: &str) -> SessionRow {
    SessionRow {
        id: sid(id),
        workdir: "/tmp/proj".into(),
        status,
        active_delegations: 0,
        tmux_name: name.into(),
        last_seen: Default::default(),
    }
}

#[test]
fn command_bar_edits_buffer() {
    let mut bar = CommandBar::default();
    bar.push('h');
    bar.push('i');
    assert_eq!(bar.input, "hi");
    bar.backspace();
    assert_eq!(bar.input, "h");
}

#[test]
fn command_bar_clear_empties_input() {
    let mut bar = CommandBar::default();
    bar.push('x');
    bar.clear();
    assert!(bar.input.is_empty());
}

#[test]
fn command_bar_submit_records_history() {
    let mut bar = CommandBar {
        input: "  hello  ".into(),
        ..Default::default()
    };
    assert_eq!(bar.take_for_execution(), "hello");
    assert!(bar.input.is_empty());
    assert_eq!(bar.history, vec!["hello".to_string()]);
    // An empty submit is not recorded.
    assert_eq!(bar.take_for_execution(), "");
    assert_eq!(bar.history.len(), 1);
}

#[test]
fn command_bar_history_recall() {
    let mut bar = CommandBar {
        input: "first".into(),
        ..Default::default()
    };
    bar.take_for_execution();
    bar.input = "second".into();
    bar.take_for_execution();
    bar.history_prev();
    assert_eq!(bar.input, "second");
    bar.history_prev();
    assert_eq!(bar.input, "first");
    bar.history_next();
    assert_eq!(bar.input, "second");
    bar.history_next();
    assert!(bar.input.is_empty());
}

#[test]
fn chat_message_lines_prefix_role() {
    let state = DashboardState {
        chat_history: vec![
            ChatMessage::user("hello"),
            ChatMessage::coordinator("two sessions are active\nrun /sessions"),
        ],
        ..DashboardState::default()
    };
    let lines = chat_lines(&state);
    assert_eq!(lines[0].0, "[user] hello");
    assert_eq!(lines[0].1, ChatRole::User);
    assert_eq!(lines[1].0, "[coord] two sessions are active");
    // Continuation lines are indented, not re-prefixed.
    assert_eq!(lines[2].0, "        run /sessions");
    assert_eq!(lines[2].1, ChatRole::Coordinator);
}

#[test]
fn chat_lines_empty_placeholder() {
    let lines = chat_lines(&DashboardState::default());
    assert_eq!(lines.len(), 1);
    assert!(lines[0].0.contains("no messages yet"));
}

#[test]
fn chat_history_grows_on_send() {
    let mut state = DashboardState::default();
    state.push_chat(ChatMessage::user("hi"));
    state.push_chat(ChatMessage::coordinator("hello back"));
    assert_eq!(state.chat_history.len(), 2);
    // push_chat snaps the scroll to the bottom.
    assert_eq!(state.chat_scroll, usize::MAX);
}

#[test]
fn toggle_sidebar_flips_visibility() {
    let mut state = DashboardState::default();
    assert!(!state.sidebar_visible);
    state.toggle_sidebar();
    assert!(state.sidebar_visible);
    state.toggle_sidebar();
    assert!(!state.sidebar_visible);
}

#[test]
fn tab_toggles_focus() {
    let mut state = DashboardState {
        sidebar_visible: true,
        ..DashboardState::default()
    };
    assert_eq!(state.focus, Focus::Input);
    state.toggle_focus();
    assert_eq!(state.focus, Focus::Sidebar);
    state.toggle_focus();
    assert_eq!(state.focus, Focus::Input);
}

#[test]
fn tab_noop_when_sidebar_hidden() {
    // With the sidebar hidden, Tab must keep focus on the input bar so the
    // arrow keys never get silently captured by an invisible pane.
    let mut state = DashboardState::default();
    state.toggle_focus();
    assert_eq!(state.focus, Focus::Input);
}

#[test]
fn selection_clamps_to_bounds() {
    let mut state = DashboardState {
        sessions: vec![
            session("a", SessionStatus::Active, "tmpm-a"),
            session("b", SessionStatus::Active, "tmpm-b"),
        ],
        selected_session: 99,
        ..DashboardState::default()
    };
    state.clamp_selection();
    assert_eq!(state.selected_session, 1);
    state.sessions.clear();
    state.clamp_selection();
    assert_eq!(state.selected_session, 0);
}

#[test]
fn select_up_down_saturate() {
    let mut state = DashboardState {
        sessions: vec![
            session("a", SessionStatus::Active, "tmpm-a"),
            session("b", SessionStatus::Active, "tmpm-b"),
        ],
        ..DashboardState::default()
    };
    state.select_down();
    assert_eq!(state.selected_session, 1);
    state.select_down();
    assert_eq!(state.selected_session, 1);
    state.select_up();
    assert_eq!(state.selected_session, 0);
    state.select_up();
    assert_eq!(state.selected_session, 0);
}

#[test]
fn selected_target_returns_none_when_empty() {
    assert_eq!(DashboardState::default().selected_target(), None);
    let state = DashboardState {
        sessions: vec![session("a", SessionStatus::Active, "tmpm-quiet-falcon")],
        ..DashboardState::default()
    };
    assert_eq!(state.selected_target(), Some("tmpm-quiet-falcon".into()));
}

#[test]
fn status_indicator_maps_each_status() {
    assert_eq!(status_indicator(SessionStatus::Active).0, '●');
    assert_eq!(status_indicator(SessionStatus::Paused).0, '○');
    assert_eq!(status_indicator(SessionStatus::Stopped).0, '✕');
    assert_eq!(status_indicator(SessionStatus::Stopped).1, Color::Red);
}

#[test]
fn session_prefix_strips_managed_prefixes() {
    assert_eq!(session_prefix("tm-aipowerranking-01"), "aipowerranking-01");
    // Legacy prefix (issue #1955) must still strip correctly.
    assert_eq!(session_prefix("tmpm-aipowerranking"), "aipowerranking");
    assert_eq!(session_prefix("frontend"), "frontend");
}

#[test]
fn sidebar_items_format_each_session() {
    let state = DashboardState {
        sessions: vec![
            session("a", SessionStatus::Active, "tmpm-a"),
            session("b", SessionStatus::Paused, "tmpm-b"),
        ],
        ..DashboardState::default()
    };
    assert_eq!(sidebar_items(&state).len(), 2);
}

#[test]
fn sidebar_items_empty_when_no_sessions() {
    assert!(sidebar_items(&DashboardState::default()).is_empty());
}

#[test]
fn clamp_scroll_bounds_to_last_page() {
    // A huge offset (the "snap to bottom" sentinel) is clamped so the last
    // `height` lines stay visible.
    assert_eq!(clamp_scroll(usize::MAX, 100, 10), 90);
    // An in-range offset is left untouched.
    assert_eq!(clamp_scroll(5, 100, 10), 5);
    // Fewer lines than the height: no scroll needed.
    assert_eq!(clamp_scroll(usize::MAX, 3, 10), 0);
}

#[test]
fn title_style_signals_daemon_health() {
    let healthy = title_style(true);
    assert_eq!(healthy.fg, Some(Color::Cyan));
    assert!(!healthy.add_modifier.contains(Modifier::REVERSED));
    let down = title_style(false);
    assert_eq!(down.fg, Some(Color::Red));
    assert!(down.add_modifier.contains(Modifier::REVERSED));
}

#[test]
fn status_line_falls_back_to_key_hint() {
    assert_eq!(status_line(&DashboardState::default()), KEY_HINT);
}

#[test]
fn status_line_shows_last_action() {
    let state = DashboardState {
        last_action: Some("sent to coordinator".into()),
        ..DashboardState::default()
    };
    assert_eq!(status_line(&state), "sent to coordinator");
}

#[test]
fn command_input_line_shows_cursor() {
    let bar = CommandBar {
        input: "hello".into(),
        ..Default::default()
    };
    assert_eq!(command_input_line(&bar, true), "CMD> hello_");
    assert_eq!(command_input_line(&bar, false), "CMD> hello");
}

#[test]
fn help_text_lists_all_bindings() {
    let text = help_text();
    for token in ["Enter", "s ", "Tab", "?", "Esc", "q ", "@session:"] {
        assert!(text.contains(token), "help text missing {token}");
    }
}

#[test]
fn short_session_extracts_prefix() {
    let id = sid("abcdefghij");
    assert_eq!(short_session(&id).len(), 8);
}

#[test]
fn scroll_up_down_adjust_offset() {
    let mut state = DashboardState {
        chat_scroll: 5,
        ..DashboardState::default()
    };
    state.scroll_up();
    assert_eq!(state.chat_scroll, 4);
    state.scroll_down();
    assert_eq!(state.chat_scroll, 5);
}

/// Regression: the help overlay body must not paint text with
/// `Color::White`, because that renders the same colour as the
/// background on light terminal themes and the contents disappear.
/// The body should use `Color::Reset` so the terminal's default
/// foreground contrasts with whatever background the user has.
#[test]
fn help_overlay_body_text_is_not_white() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = DashboardState {
        show_help: true,
        ..DashboardState::default()
    };
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|f| render(f, &state))
        .expect("render must succeed");
    let buf = terminal.backend().buffer();
    // The body text "Enter" appears on the first help-overlay row inside
    // the centered popup; scan every cell that carries body-text fg and
    // confirm none use `Color::White`.
    let mut found_help_text = false;
    for y in 0..24 {
        for x in 0..120 {
            let cell = &buf[(x, y)];
            if cell.symbol() == "E" || cell.symbol() == "n" || cell.symbol() == "t" {
                // Heuristic: only consider cells that are part of the help
                // body — those inside the help overlay carry a non-Reset
                // glyph and would have previously been styled white.
                if cell.fg == Color::White {
                    panic!(
                        "help overlay body uses Color::White (invisible on light \
                         terminals) at ({x},{y}): '{}'",
                        cell.symbol()
                    );
                }
                if cell.symbol() == "E" {
                    found_help_text = true;
                }
            }
        }
    }
    assert!(
        found_help_text,
        "help overlay body did not render any expected text"
    );
}

/// Regression: chat user messages must not use `Color::White` for the
/// foreground, since on a light-themed terminal that renders the same
/// colour as the background and the message disappears. User turns
/// should adopt the terminal default (`Color::Reset`) instead.
#[test]
fn chat_user_message_is_not_white() {
    use ratatui::{Terminal, backend::TestBackend};
    let mut state = DashboardState::default();
    state.push_chat(ChatMessage::user("xyzzy-test-marker"));
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|f| render(f, &state))
        .expect("render must succeed");
    let buf = terminal.backend().buffer();
    // Find the cells carrying the marker token; assert none of them is
    // styled `Color::White`.
    let mut saw_marker = false;
    for y in 0..24 {
        for x in 0..120 {
            let cell = &buf[(x, y)];
            if cell.symbol() == "x" || cell.symbol() == "y" || cell.symbol() == "z" {
                if cell.fg == Color::White {
                    panic!(
                        "chat user message uses Color::White (invisible on light \
                         terminals) at ({x},{y}): '{}'",
                        cell.symbol()
                    );
                }
                saw_marker = true;
            }
        }
    }
    assert!(saw_marker, "chat user message marker was not rendered");
}
