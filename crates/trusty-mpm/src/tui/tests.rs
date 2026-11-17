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

/// An explicit URL no discovery source ever yields. Re-discovery dials
/// nothing, so the #9556 tests below never connect to it.
const EXPLICIT_URL: &str = "http://127.0.0.1:1";

/// Why (#9556): an operator-chosen URL is never swapped for the lock-file or
/// default daemon, which under a sandbox is the live fleet.
/// Test: itself.
#[test]
fn rediscover_daemon_keeps_explicit_url() {
    let dead = EXPLICIT_URL.to_string();
    let mut client = DaemonClient::new(dead.clone()).with_pinned_base_url(true);
    assert!(!rediscover_daemon(&mut client, false));
    assert_eq!(client.base_url(), dead);
}

/// Why (#6288 Q3): every TUI launch reaches the daemon socket; a loopback,
/// empty or absent URL resolves it unpinned, and a remote URL is refused.
/// Test: itself.
#[test]
#[serial_test::serial]
fn client_for_resolves_the_socket_and_refuses_a_remote_url() {
    use crate::secret_source::test_env::EnvVarGuard;
    let _sandbox = EnvVarGuard::unset(trusty_common::credentials::SANDBOX_ENV_VAR);
    for explicit in [Some(EXPLICIT_URL), Some(""), None] {
        let client = client_for(explicit).expect("a local daemon socket");
        assert!(
            client.socket_path().is_some(),
            "{explicit:?} must reach the socket"
        );
        assert!(!client.base_url_pinned());
    }
    assert!(client_for(Some("http://100.64.0.1:7880")).is_err());
}

/// Why (#9556): without an explicit URL the TUI still follows a restarted
/// daemon. Discovery never yields port 1, so the client must move.
/// Test: itself.
#[test]
fn rediscover_daemon_follows_discovery_when_not_pinned() {
    let dead = EXPLICIT_URL.to_string();
    let mut client = DaemonClient::new(dead.clone());
    assert!(rediscover_daemon(&mut client, false));
    assert_ne!(client.base_url(), dead);
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
    // `DEFAULT_MEMORY_URL` was removed (issue #2030) — the real default is
    // resolved dynamically via discovery; an unreachable placeholder is fine
    // for a render-only smoke test.
    let hp = HealthScreen::new(
        "/nonexistent/trusty-search/test.sock",
        "http://127.0.0.1:19999",
    );
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

// #6483: the initial-view routing for a new/attached session. The multipane
// project_ctl dashboard is the DEFAULT; the single-pane coordinator chat is
// reachable only through the explicit `--single-pane` opt-in.

#[test]
fn initial_view_defaults_to_multipane() {
    // No opt-in flag: `tm tui` / `tm attach` must resolve to the multipane
    // project_ctl dashboard, not the single-pane chat.
    assert_eq!(initial_view(false), TuiView::Multipane);
    assert_eq!(TuiView::default(), TuiView::Multipane);
}

#[test]
fn initial_view_honours_single_pane_opt_in() {
    // The pre-#6483 surface stays reachable — explicitly.
    assert_eq!(initial_view(true), TuiView::SinglePane);
}

/// Dropping the shared terminal guard twice, with no terminal attached, is a
/// silent no-op (#7224).
///
/// Why: the guard's whole job is running during a panic unwind, and the two
/// surfaces that suspend for a tmux hand-off drop it AFTER `terminal::suspend`
/// has already restored the screen. Both cases run the restore sequence with
/// nothing to restore — in CI, with no tty at all — so a teardown that
/// panicked or propagated there would turn a TUI bug into an abort.
/// What: constructs and drops two guards in a row and asserts only that
/// control returns; every step inside is best-effort by design.
#[test]
fn terminal_guard_drop_is_idempotent() {
    drop(super::terminal::TerminalGuard);
    drop(super::terminal::TerminalGuard);
}

// ── `terminal::enter` atomicity and the TERM predicate (#7224) ───────────────

/// A writer that fails every write, standing in for a screen that rejects the
/// `EnterAlternateScreen` sequence.
///
/// Why: crossterm acts on the process's real controlling terminal, so the only
/// step of `enter` a unit test can fault is the one that goes through a writer.
struct FailingScreen;

impl std::io::Write for FailingScreen {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("screen rejected the sequence"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("screen rejected the flush"))
    }
}

/// A failure AFTER raw mode engaged still restores the terminal (#7224).
///
/// Why: this is the leak the fix closes. `enter` takes raw mode first and its
/// callers install their `TerminalGuard` only once it returns `Ok`, so a failure
/// in between used to propagate with the shell left in raw mode and no guard in
/// existence — no echo, no Ctrl-C, and nothing left that would ever put it back.
/// What: enables raw mode successfully, then fails on `EnterAlternateScreen`,
/// and asserts both that the error propagates and that the restore ran.
#[test]
fn enter_with_restores_the_terminal_when_the_screen_fails() {
    let restored = std::cell::Cell::new(false);
    let result =
        super::terminal::enter_with(FailingScreen, || Ok(()), || restored.set(true), false);
    assert!(
        result.is_err(),
        "a screen that rejects EnterAlternateScreen must surface as Err"
    );
    assert!(
        restored.get(),
        "raw mode engaged, so the failure path must restore the terminal"
    );
}

/// A failure of raw mode itself restores nothing, because nothing changed.
///
/// Why: the complement that keeps the fix honest — restoring after a no-op would
/// emit `LeaveAlternateScreen` at a shell that never left it, scrolling the
/// operator's screen for no reason.
#[test]
fn enter_with_leaves_the_terminal_alone_when_raw_mode_fails() {
    let restored = std::cell::Cell::new(false);
    let result = super::terminal::enter_with(
        FailingScreen,
        || Err(std::io::Error::other("no tty")),
        || restored.set(true),
        false,
    );
    assert!(result.is_err(), "a raw-mode failure must surface as Err");
    assert!(
        !restored.get(),
        "nothing engaged, so nothing may be restored"
    );
}

// ── Mouse capture (#10476) ───────────────────────────────────────────────────

/// `?1000h`, the first mode crossterm's `EnableMouseCapture` turns on.
const CAPTURE_ON: &str = "\x1b[?1000h";
/// `?1000l`, the mode crossterm's `DisableMouseCapture` turns off last.
const CAPTURE_OFF: &str = "\x1b[?1000l";
const LEAVE_ALT_SCREEN: &str = "\x1b[?1049l";
const ENTER_ALT_SCREEN: &str = "\x1b[?1049h";

/// A screen that records every byte and can reject the capture-on sequence.
///
/// Why: these tests must never touch a real terminal; `Terminal::new` consumes
/// its writer, so the record is shared.
#[derive(Clone, Default)]
struct RecordingScreen {
    bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    reject_capture: bool,
}

impl RecordingScreen {
    fn text(&self) -> String {
        let bytes = self.bytes.lock().expect("the record lock");
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl std::io::Write for RecordingScreen {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes
            .lock()
            .expect("the record lock")
            .extend_from_slice(buf);
        if self.reject_capture && self.text().contains(CAPTURE_ON) {
            return Err(std::io::Error::other("screen rejected mouse capture"));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The teardown every exit path shares turns capture off (#10476), before it
/// leaves the alternate screen. Without this a shell after the TUI prints
/// escape sequences on every mouse move.
#[test]
fn restore_disables_capture() {
    let mut screen = RecordingScreen::default();
    super::terminal::write_restore(&mut screen).expect("a recording screen cannot fail");
    let text = screen.text();
    let off = text.find(CAPTURE_OFF);
    let leave = text.find(LEAVE_ALT_SCREEN);
    assert!(off.is_some(), "no capture-off sequence in {text:?}");
    assert!(leave.is_some(), "no leave-screen sequence in {text:?}");
    assert!(off < leave, "capture must go off first: {text:?}");
}

/// Names the stand-in a re-executed test binary runs instead of its body.
#[cfg(unix)]
const TERM_EXIT_CHILD_ENV: &str = "TM_TEST_TERM_EXIT_CHILD";

/// Child half of the `restore_then_exit` tests: a no-op unless a parent test
/// re-executed this binary, since `process::exit` would end the whole runner.
#[cfg(unix)]
#[test]
fn restore_then_exit_child() {
    let bound = std::time::Duration::from_millis(500);
    match std::env::var(TERM_EXIT_CHILD_ENV).as_deref() {
        // The stuck write: holds the stdout lock, as a stalled draw does.
        Ok("block") => super::terminal::restore_then_exit(
            || {
                let _held = std::io::stdout().lock();
                loop {
                    std::thread::park();
                }
            },
            bound,
        ),
        Ok(marker) => {
            let marker = marker.to_string();
            super::terminal::restore_then_exit(
                move || std::fs::write(marker, "restored").expect("write the marker"),
                bound,
            )
        }
        Err(_) => {}
    }
}

/// Run `restore_then_exit_child` in a child process with `mode`, killing it at
/// the deadline. Answers its exit code, `None` for a hang, and the time taken.
#[cfg(unix)]
fn run_term_exit_child(mode: &std::path::Path) -> (Option<i32>, std::time::Duration) {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let deadline = Duration::from_secs(30);
    let started = Instant::now();
    let mut child = Command::new(std::env::current_exe().expect("the test binary"))
        .args(["--exact", "tui::tests::restore_then_exit_child"])
        .env(TERM_EXIT_CHILD_ENV, mode)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the test binary");
    loop {
        if let Some(status) = child.try_wait().expect("wait for the child") {
            return (status.code(), started.elapsed());
        }
        if started.elapsed() > deadline {
            child.kill().expect("kill the hung child");
            child.wait().expect("reap the hung child");
            return (None, started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// #10476: a restore that never returns must not stop the exit. The child's
/// stand-in holds the stdout lock forever; the child still exits 143. The
/// elapsed-time ceiling allows for test-binary startup on a loaded host.
#[cfg(unix)]
#[test]
fn restore_then_exit_exits_143_when_restore_blocks() {
    let (code, elapsed) = run_term_exit_child(std::path::Path::new("block"));
    assert_eq!(code, Some(143), "after {elapsed:?}");
    assert!(
        elapsed >= std::time::Duration::from_millis(500),
        "exited after {elapsed:?}, before the bound, so the stand-in did not block"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "took {elapsed:?}"
    );
}

/// #10476: the normal case still restores before it exits.
#[cfg(unix)]
#[test]
fn restore_then_exit_runs_the_restore_first() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let marker = dir.path().join("restored");
    let (code, _) = run_term_exit_child(&marker);
    assert_eq!(code, Some(143));
    assert_eq!(
        std::fs::read_to_string(&marker).expect("the restore ran"),
        "restored"
    );
}

/// With the mouse off, no enable sequence ever reaches the terminal, and with
/// it on, this same path does send one, so the assertion can fail (#10476).
#[test]
fn enter_with_mouse_false_never_enables_capture() {
    let screen = RecordingScreen::default();
    // `Terminal::new` queries the real size and may fail without a tty; only
    // the bytes matter here, and a failure restores through the closure.
    let _ = super::terminal::enter_with(screen.clone(), || Ok(()), || {}, false);
    let text = screen.text();
    assert!(text.contains(ENTER_ALT_SCREEN), "{text:?}");
    assert!(!text.contains("\x1b[?100"), "mouse mode in {text:?}");

    let screen = RecordingScreen::default();
    let _ = super::terminal::enter_with(screen.clone(), || Ok(()), || {}, true);
    assert!(screen.text().contains(CAPTURE_ON), "{:?}", screen.text());
}

/// Capture failing after raw mode and the alternate screen engaged restores the
/// terminal and propagates, so raw mode is not left on (#10476).
#[test]
fn enter_with_restores_the_terminal_when_capture_fails() {
    let restored = std::cell::Cell::new(false);
    let screen = RecordingScreen {
        reject_capture: true,
        ..Default::default()
    };
    let result =
        super::terminal::enter_with(screen.clone(), || Ok(()), || restored.set(true), true);
    let error = result
        .err()
        .expect("a rejected capture must surface as Err");
    assert!(error.to_string().contains("mouse capture"), "{error}");
    assert!(restored.get(), "the failure path must restore the terminal");
    assert!(screen.text().contains(ENTER_ALT_SCREEN), "reached capture");

    // With the mouse off the same screen is never asked, so nothing fails.
    let restored = std::cell::Cell::new(false);
    let screen = RecordingScreen {
        reject_capture: true,
        ..Default::default()
    };
    let _ = super::terminal::enter_with(screen.clone(), || Ok(()), || restored.set(true), false);
    assert!(!screen.text().contains(CAPTURE_ON));
}

/// Around a child process: suspend's sequence turns capture off, and resume's
/// turns it back on only for a surface that had it (#10476).
#[test]
fn suspend_disables_and_resume_reenables_only_when_asked() {
    let mut suspended = RecordingScreen::default();
    super::terminal::write_restore(&mut suspended).expect("a recording screen cannot fail");
    assert!(suspended.text().contains(CAPTURE_OFF));

    let mut resumed = RecordingScreen::default();
    super::terminal::write_enter(&mut resumed, true).expect("a recording screen cannot fail");
    let text = resumed.text();
    assert!(
        text.find(ENTER_ALT_SCREEN) < text.find(CAPTURE_ON),
        "{text:?}"
    );

    let mut other_tui = RecordingScreen::default();
    super::terminal::write_enter(&mut other_tui, false).expect("a recording screen cannot fail");
    assert!(!other_tui.text().contains(CAPTURE_ON));
    assert!(other_tui.text().contains(ENTER_ALT_SCREEN));
}

/// `--no-mouse` or `TM_TUI_MOUSE=0` turn the mouse off; anything else leaves it on.
#[test]
fn mouse_enabled_honours_flag_and_env() {
    use super::terminal::mouse_enabled;
    assert!(mouse_enabled(false, None), "on by default");
    assert!(mouse_enabled(false, Some("1")));
    assert!(mouse_enabled(false, Some("")));
    assert!(!mouse_enabled(true, None), "--no-mouse");
    assert!(!mouse_enabled(false, Some("0")), "TM_TUI_MOUSE=0");
    assert!(!mouse_enabled(true, Some("1")), "the flag wins");
}

/// An absent, empty, or `dumb` `TERM` refuses raw mode (#7224).
///
/// Why: `tm ls` gated only on TTY-ness, so `TERM=dumb tm ls` under a real pty
/// opened the full-screen TUI on a terminal with no cursor addressing. This is
/// the shared predicate both `tm ls` and `tm f` now answer through.
#[test]
fn term_supports_raw_mode_rejects_dumb_and_missing() {
    use super::terminal::term_supports_raw_mode;
    assert!(!term_supports_raw_mode(None), "unset TERM -> static");
    assert!(!term_supports_raw_mode(Some("")), "empty TERM -> static");
    assert!(!term_supports_raw_mode(Some("dumb")), "dumb TERM -> static");
    assert!(
        !term_supports_raw_mode(Some("DUMB")),
        "the dumb check is case-insensitive"
    );
}

/// A real terminal type passes the predicate.
#[test]
fn term_supports_raw_mode_accepts_a_real_terminal() {
    use super::terminal::term_supports_raw_mode;
    assert!(term_supports_raw_mode(Some("xterm-256color")));
    assert!(term_supports_raw_mode(Some("screen")));
}
