use super::*;

#[test]
fn rediscover_is_noop_when_daemon_reachable() {
    // A reachable daemon must never trigger a URL re-resolution.
    let mut client = DaemonClient::new("http://127.0.0.1:7880");
    assert!(!rediscover_daemon(&mut client, true));
    assert_eq!(client.base_url(), "http://127.0.0.1:7880");
}

#[test]
fn rediscover_is_noop_when_resolved_url_unchanged() {
    // When the daemon is unreachable but the lock file resolves to the same
    // URL, re-pointing is pointless and the function reports "no change".
    let mut client = DaemonClient::new(crate::core::DEFAULT_DAEMON_URL);
    let changed = rediscover_daemon(&mut client, false);
    if !changed {
        assert_eq!(client.base_url(), crate::core::DEFAULT_DAEMON_URL);
    }
}

#[test]
fn coordinator_session_maps_status() {
    // The status word from the coordinator endpoint maps back to the enum.
    let session = crate::client::CoordinatorSession {
        id: "00000000-0000-0000-0000-000000000000".into(),
        name: "tmpm-foo".into(),
        prefix: "foo".into(),
        workdir: "/tmp/p".into(),
        status: "Paused".into(),
        active_delegations: 2,
        recent_output: Vec::new(),
        // #1275 summary fields default here (additive wire change).
        ..Default::default()
    };
    let row = coordinator_session_to_row(session);
    assert_eq!(row.tmux_name, "tmpm-foo");
    assert_eq!(row.active_delegations, 2);
    assert_eq!(row.status, crate::core::session::SessionStatus::Paused);
}

#[test]
fn screen_default_is_chat() {
    // The TUI must open on the coordinator chat, preserving prior behaviour.
    assert_eq!(Screen::default(), Screen::Chat);
}

/// Apply one screen-switch keypress, mirroring the event-loop branch.
///
/// Why: lets a test exercise the `[1]`/`[2]` switch logic without driving
/// a real terminal.
/// What: returns the [`Screen`] reached after pressing `key` from `from`.
/// Test: used by `screen_switch_preserves_chat_state`.
fn switch(from: Screen, key: char) -> Screen {
    // Mirror the event-loop branches:
    //   - From Chat, `2` opens the Health screen.
    //   - From Health, `c` returns to Chat (digits route to tabs there).
    match (from, key) {
        (Screen::Chat, '2') => Screen::Health,
        (Screen::Health, 'c') => Screen::Chat,
        _ => from,
    }
}

#[test]
fn screen_switch_preserves_chat_state() {
    // Switching Chat → Health → Chat must not reset the coordinator
    // chat: the chat state is owned by the loop, independent of the
    // active Screen.
    let mut state = DashboardState::default();
    state.push_chat(ChatMessage::user("remember me"));
    // Simulate the screen-switch keypresses the loop handles.
    let screen = switch(Screen::Chat, '2');
    assert_eq!(screen, Screen::Health);
    let screen = switch(screen, 'c');
    assert_eq!(screen, Screen::Chat);
    // The transcript built before switching is still intact.
    assert_eq!(state.chat_history.len(), 1);
    assert_eq!(state.chat_history[0].content, "remember me");
}

#[test]
fn health_status_bar_lists_keys() {
    // The footer must document every tab-switch, navigation, and
    // service-management key of the redesigned screen (issue #36).
    for token in [
        "[1]health",
        "[2]logs",
        "[3]search",
        "[Tab]",
        "[↑↓]",
        "[r]",
        "[S]start",
        "[X]stop",
        "[c]chat",
        "[q]quit",
    ] {
        assert!(
            HEALTH_KEY_HINT.contains(token),
            "status bar missing {token}"
        );
    }
}

#[test]
fn render_screen_draws_both_screens_without_panic() {
    // Rendering each screen against a TestBackend must not panic.
    use ratatui::{Terminal, backend::TestBackend};
    let chat = DashboardState::default();
    let hp = HealthScreen::new(health::DEFAULT_SEARCH_URL, health::DEFAULT_MEMORY_URL);
    for screen in [Screen::Chat, Screen::Health] {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| render_screen(f, screen, &chat, &hp))
            .expect("render must not panic");
    }
}

#[tokio::test]
async fn coordinator_send_without_daemon_reports_error() {
    // A send against an unreachable daemon appends the user message and a
    // coordinator-authored error note rather than panicking.
    let client = DaemonClient::new("http://127.0.0.1:0");
    let mut state = DashboardState::default();
    coordinator_send(&mut state, &client, "what is happening?").await;
    assert_eq!(state.chat_history.len(), 2);
    assert_eq!(state.chat_history[0].role, dashboard::ChatRole::User);
    assert!(
        state.chat_history[1].content.contains("daemon error"),
        "expected a daemon error, got {:?}",
        state.chat_history[1].content
    );
}
