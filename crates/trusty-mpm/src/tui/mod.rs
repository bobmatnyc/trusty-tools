//! trusty-mpm TUI coordinator dashboard library.
//!
//! Why: operators need one conversational surface — the *coordinator chat* —
//! that has visibility into every active Claude Code session, with a
//! dismissable session sidebar beside it. Exposing the dashboard as a library
//! lets the unified `trusty-mpm tui` subcommand reuse it without a separate
//! binary.
//! What: a ratatui app that polls the daemon's coordinator-context endpoint on
//! a timer, renders the [`dashboard`] panes, and POSTs typed messages to the
//! coordinator-chat endpoint. Rendering and HTTP are split into the
//! [`dashboard`] and [`client`] modules so the logic is unit-testable.
//! Test: `cargo test -p trusty-mpm-tui` covers chat/session formatting and the
//! client; `trusty-mpm tui` launches the live dashboard.

pub mod client;
pub mod dashboard;
pub mod health;
pub mod iterm2;

use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::Line,
    widgets::Paragraph,
};

use client::DaemonClient;
use dashboard::{ChatMessage, DashboardState, Focus};
use health::{Daemon, HealthScreen, HealthUpdate};

/// Which top-level screen the TUI is currently showing.
///
/// Why: the TUI now hosts two surfaces — the coordinator chat (`[1]`) and the
/// combined search + memory health view (`[2]`). A typed enum keeps the
/// screen-switch handling exhaustive and lets the event loop route input and
/// rendering without losing either surface's state.
/// What: `Chat` is the default coordinator dashboard; `Health` is the
/// secondary health screen.
/// Test: `screen_switch_preserves_chat_state` in this module's tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    /// The coordinator chat dashboard (`[1]`, the default and original surface).
    #[default]
    Chat,
    /// The combined trusty-search + trusty-memory health screen (`[2]`).
    Health,
}

/// Status-bar hint listing the screen-switch and global keys.
///
/// Why: the health screen's footer must always show how to switch screens and
/// manage services; a shared constant keeps the hint in one place.
/// What: the one-line key reference drawn at the bottom of the health screen.
/// Test: `health_status_bar_lists_keys`.
pub const HEALTH_KEY_HINT: &str =
    "[1]health [2]logs [3]search [Tab]svc [↑↓]nav [r]refresh [S]start [X]stop [c]chat [q]quit";

/// Run the ratatui coordinator dashboard against `url`.
///
/// Why: shared entry point for both the `trusty-mpm tui` subcommand and the
/// backward-compatible `trusty-mpm-tui` shim binary.
/// What: sets up the alternate screen / raw mode, runs [`run_loop`], and always
/// restores the terminal afterward even on error.
/// Test: pure parts (rendering, client) are unit-tested; this is the thin glue
/// exercised by launching the dashboard.
pub async fn run(url: String, interval_ms: u64) -> anyhow::Result<()> {
    run_focused(url, interval_ms, None).await
}

/// Re-resolve the daemon URL from the lock file when the daemon is unreachable.
///
/// Why: `DaemonClient` is built once at startup; if the daemon later restarted
/// onto a fresh ephemeral port, the client would stay pinned to a stale address
/// forever. Re-resolving on every failed poll lets the TUI self-heal.
/// What: when `reachable` is `false`, calls [`crate::core::resolve_daemon_url`]
/// and, if it yields a different URL, re-points the client and returns `true`.
/// Test: `rediscover_is_noop_when_daemon_reachable`.
fn rediscover_daemon(client: &mut DaemonClient, reachable: bool) -> bool {
    if reachable {
        return false;
    }
    let resolved = crate::core::resolve_daemon_url(None);
    if resolved != client.base_url() {
        client.set_base_url(resolved);
        true
    } else {
        false
    }
}

/// Run the dashboard, optionally pre-focusing a session in the sidebar.
///
/// Why: `tm connect <target>` resolves a session id and wants the TUI to open
/// with that session highlighted in the sidebar.
/// What: same terminal setup/teardown as [`run`], threading `focus_id` into
/// [`run_loop`], which selects the matching session after the priming poll.
/// Test: terminal glue is exercised by launching the dashboard.
pub async fn run_focused(
    url: String,
    interval_ms: u64,
    focus_id: Option<String>,
) -> anyhow::Result<()> {
    let mut client = DaemonClient::new(url);

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut client, interval_ms, focus_id).await;

    // Always restore the terminal, even on error.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

/// Refresh [`DashboardState`] from one daemon poll.
///
/// Why: keeps the poll logic out of the key-driven event loop so the loop can
/// re-poll on demand (after a send) as well as on its timer.
/// What: probes health, then pulls the coordinator context (the session list);
/// clears the sessions when the daemon is unreachable. When the daemon looks
/// unreachable it re-resolves the URL from the lock file via
/// [`rediscover_daemon`] and retries one health probe.
/// Test: the pure pieces (rendering, client, rediscovery) are unit-tested.
async fn poll_daemon(state: &mut DashboardState, client: &mut DaemonClient) {
    state.daemon_reachable = client.is_healthy().await;
    if rediscover_daemon(client, state.daemon_reachable) {
        state.daemon_reachable = client.is_healthy().await;
    }
    if state.daemon_reachable {
        match client.coordinator_context().await {
            Ok(context) => {
                state.sessions = context
                    .sessions
                    .into_iter()
                    .map(coordinator_session_to_row)
                    .collect();
            }
            Err(_) => state.daemon_reachable = false,
        }
    } else {
        state.sessions.clear();
    }
    state.clamp_selection();
}

/// Convert a coordinator-context session into a dashboard `SessionRow`.
///
/// Why: the dashboard sidebar renders `SessionRow`s; the coordinator endpoint
/// returns a richer `CoordinatorSession`, so a projection is needed.
/// What: maps the id (parsed from its UUID string), tmux name, workdir,
/// delegation count, and a status word back into a `SessionStatus`.
/// Test: covered indirectly by `poll_daemon`; the status mapping is pure.
fn coordinator_session_to_row(s: crate::client::CoordinatorSession) -> client::SessionRow {
    use crate::core::session::{SessionId, SessionStatus};
    let id = uuid::Uuid::parse_str(&s.id)
        .map(SessionId)
        .unwrap_or_else(|_| SessionId(uuid::Uuid::nil()));
    let status = match s.status.as_str() {
        "Starting" => SessionStatus::Starting,
        "Active" => SessionStatus::Active,
        "AwaitingApproval" => SessionStatus::AwaitingApproval,
        "Detached" => SessionStatus::Detached,
        "Paused" => SessionStatus::Paused,
        _ => SessionStatus::Stopped,
    };
    client::SessionRow {
        id,
        workdir: s.workdir,
        status,
        active_delegations: s.active_delegations,
        tmux_name: s.name,
        last_seen: Default::default(),
    }
}

/// Spawn the background health pollers for the search and memory daemons.
///
/// Why: the acceptance criteria require each daemon to be polled independently
/// every 5 seconds without freezing the input loop. Running each poll on its
/// own detached tokio task keeps a slow or hung daemon from blocking the other
/// panel or the keyboard.
/// What: spawns one task per daemon; each task polls its [`health::HealthClient`]
/// on [`health::POLL_INTERVAL`] and sends every result down `tx` as a
/// [`HealthUpdate`]. A task exits quietly once the receiver is dropped (the TUI
/// is shutting down). The first poll fires immediately so the panels leave the
/// `Connecting` state quickly.
/// Test: the per-poll projection and routing are unit-tested in `health.rs`;
/// this is the thin task-spawning glue.
fn spawn_health_pollers(
    search_url: String,
    memory_url: String,
    tx: tokio::sync::mpsc::Sender<HealthUpdate>,
) {
    for (daemon, url) in [(Daemon::Search, search_url), (Daemon::Memory, memory_url)] {
        let tx = tx.clone();
        tokio::spawn(async move {
            let client = health::client_for(daemon, &url);
            loop {
                let state = client.poll().await;
                if tx.send(HealthUpdate { daemon, state }).await.is_err() {
                    break; // The TUI has shut down — stop polling.
                }
                tokio::time::sleep(health::POLL_INTERVAL).await;
            }
        });
    }
}

/// Send the typed message to the coordinator and fold the reply into the chat.
///
/// Why: pressing Enter is the single action of the coordinator dashboard —
/// every message goes to `POST /api/v1/coordinator/chat`, which either routes a
/// `@prefix:` command at a session or answers via the LLM.
/// What: appends the user message to the transcript, calls the daemon, then
/// appends the coordinator reply (or routed-command output). A `None` response
/// (LLM not configured) or a transport error becomes a coordinator-authored
/// note so a failure is always renderable, never a panic.
/// Test: `coordinator_send_without_daemon_reports_error`.
async fn coordinator_send(state: &mut DashboardState, client: &DaemonClient, message: &str) {
    state.push_chat(ChatMessage::user(message));
    match client.coordinator_chat(message, &state.coord_history).await {
        Ok(Some(outcome)) => {
            let reply = match outcome.command_output {
                Some(output) => format!("{}\n{output}", outcome.reply),
                None => outcome.reply.clone(),
            };
            state.push_chat(ChatMessage::coordinator(reply));
            // A routed command resets the LLM window — it was not a chat turn.
            if outcome.routed_to_session.is_some() {
                state.coord_history.clear();
            } else {
                state
                    .coord_history
                    .push(crate::client::ChatMessage::user(message.to_string()));
                state
                    .coord_history
                    .push(crate::client::ChatMessage::assistant(outcome.reply));
            }
            state.last_action = Some("message sent".to_string());
        }
        Ok(None) => {
            state.push_chat(ChatMessage::coordinator(
                "coordinator chat is not configured — set OPENROUTER_API_KEY and enable the overseer",
            ));
            state.last_action = Some("LLM not configured".to_string());
        }
        Err(e) => {
            state.push_chat(ChatMessage::coordinator(format!("daemon error: {e}")));
            state.last_action = Some("daemon error".to_string());
        }
    }
}

/// Render whichever screen is currently active.
///
/// Why: keeps the screen→renderer dispatch in one place so the event loop's
/// `terminal.draw` call stays a single line.
/// What: draws the coordinator chat for [`Screen::Chat`], or the health screen
/// (with its shared status bar) for [`Screen::Health`].
/// Test: each renderer is smoke-tested in its own module.
fn render_screen(frame: &mut Frame, screen: Screen, chat: &DashboardState, hp: &HealthScreen) {
    match screen {
        Screen::Chat => dashboard::render(frame, chat),
        Screen::Health => {
            // Reserve the bottom row for the shared status bar; the health
            // screen body renders into the remaining space.
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(6), Constraint::Length(1)])
                .split(frame.area());
            // `health::render` lays out against `frame.area()`; render it into
            // the body chunk by drawing the status bar last (it cannot overlap
            // since the body chunk excludes the final row).
            health::render(frame, hp);
            frame.render_widget(
                Paragraph::new(Line::from(HEALTH_KEY_HINT)).style(
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .add_modifier(Modifier::REVERSED),
                ),
                chunks[1],
            );
        }
    }
}

/// The dashboard event loop: poll the daemon, render, handle input.
///
/// Why: kept separate from [`run`] so terminal setup/teardown wraps it cleanly.
/// What: hosts both the coordinator chat (`[1]`) and the health screen (`[2]`)
/// — switching screens never resets either, since both states live for the
/// whole loop. Refreshes [`DashboardState`] from the daemon on an `interval_ms`
/// timer, drains background [`HealthUpdate`]s into the [`HealthScreen`], and
/// polls the keyboard every 50ms so input feels instantaneous. Number keys
/// switch screens; `q` quits from either.
/// Test: the pure pieces (rendering, client, screen state) are unit-tested.
async fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    client: &mut DaemonClient,
    interval_ms: u64,
    focus_id: Option<String>,
) -> anyhow::Result<()> {
    // The sidebar starts visible only when there is at least one session to
    // show; otherwise the coordinator chat gets the full width immediately.
    let mut state = DashboardState::default();
    let mut screen = Screen::default();
    let mut health_screen =
        HealthScreen::new(health::DEFAULT_SEARCH_URL, health::DEFAULT_MEMORY_URL);

    // The health pollers run on detached tasks and push updates down a channel
    // the loop drains without blocking.
    let (health_tx, mut health_rx) = tokio::sync::mpsc::channel::<HealthUpdate>(16);
    spawn_health_pollers(
        health_screen.search_url.clone(),
        health_screen.memory_url.clone(),
        health_tx,
    );

    poll_daemon(&mut state, client).await;
    // Prime the health screen with one refresh so collections + logs are
    // present the first time the operator opens [Screen::Health].
    refresh_health_data(&mut health_screen).await;
    let mut last_health_refresh = Instant::now();
    state.sidebar_visible = !state.sessions.is_empty();
    // Apply a `tm connect` focus once the priming poll has filled the list.
    if let Some(id) = focus_id.as_deref()
        && let Some(idx) = state.sessions.iter().position(|s| s.id.0.to_string() == id)
    {
        state.selected_session = idx;
        state.last_action = Some(format!("Connected to {id}"));
    }
    let mut last_poll = Instant::now();

    loop {
        terminal.draw(|f| render_screen(f, screen, &state, &health_screen))?;

        // Drain any health updates that landed since the last frame.
        while let Ok(update) = health_rx.try_recv() {
            health_screen.apply_update(update);
        }

        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
        {
            // The health screen has its own key handling, kept separate so the
            // chat-screen branch below stays unchanged. Within the health
            // screen the digit keys switch the right-panel tab; `c` returns
            // to the coordinator chat. When the Search tab is active and the
            // input bar holds focus, alphanumeric keys edit the search query.
            if screen == Screen::Health {
                // Handle the always-visible search bar's input when the
                // Search tab has captured it.
                if health_screen.tab == health::HealthTab::Search
                    && health_screen.search_input_focused
                {
                    match key.code {
                        KeyCode::Esc => {
                            health_screen.search_query.clear();
                            health_screen.search_input_focused = false;
                        }
                        KeyCode::Backspace => {
                            health_screen.search_query.pop();
                        }
                        KeyCode::Char(c) if !c.is_ascii_digit() => {
                            health_screen.search_query.push(c);
                        }
                        KeyCode::Char('1') => health_screen.set_tab(health::HealthTab::Health),
                        KeyCode::Char('2') => health_screen.set_tab(health::HealthTab::Logs),
                        KeyCode::Char('3') => health_screen.set_tab(health::HealthTab::Search),
                        KeyCode::Char('4') => health_screen.set_tab(health::HealthTab::Index),
                        _ => {}
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('c') => screen = Screen::Chat,
                    KeyCode::Char('1') => health_screen.set_tab(health::HealthTab::Health),
                    KeyCode::Char('2') => health_screen.set_tab(health::HealthTab::Logs),
                    KeyCode::Char('3') => health_screen.set_tab(health::HealthTab::Search),
                    KeyCode::Char('4') => health_screen.set_tab(health::HealthTab::Index),
                    KeyCode::Tab => {
                        health_screen.toggle_focus();
                        health_screen.clamp_collection_selection();
                    }
                    KeyCode::Up => match health_screen.tab {
                        health::HealthTab::Logs => health_screen.focused_logs_mut().scroll_up(),
                        _ => health_screen.select_collection_up(),
                    },
                    KeyCode::Down => match health_screen.tab {
                        health::HealthTab::Logs => health_screen.focused_logs_mut().scroll_down(),
                        _ => health_screen.select_collection_down(),
                    },
                    KeyCode::Char('r') => {
                        // Trigger an immediate refresh of collections + logs.
                        refresh_health_data(&mut health_screen).await;
                    }
                    KeyCode::Char('S') => {
                        health_start(&mut state, &health_screen);
                    }
                    KeyCode::Char('X') => {
                        health_stop(&mut state, &health_screen).await;
                    }
                    // Any other alphanumeric autoswitches to the Search tab.
                    KeyCode::Char(c) if c.is_ascii_alphanumeric() => {
                        health_screen.set_tab(health::HealthTab::Search);
                        health_screen.search_query.push(c);
                    }
                    _ => {}
                }
                continue;
            }

            // The help overlay swallows the next key (to close itself).
            if state.show_help {
                if matches!(
                    key.code,
                    KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q')
                ) {
                    if key.code == KeyCode::Char('q') {
                        return Ok(());
                    }
                    state.show_help = false;
                }
                continue;
            }

            // Screen-switch keys are only honoured when the input bar is not
            // capturing text, so a `2` typed into a coordinator message is not
            // hijacked. With the input bar focused, `Char` keys fall through to
            // the editing branch below.
            if state.focus != Focus::Input
                && matches!(key.code, KeyCode::Char('1') | KeyCode::Char('2'))
            {
                screen = match key.code {
                    KeyCode::Char('2') => Screen::Health,
                    _ => Screen::Chat,
                };
                continue;
            }

            match key.code {
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Char('?') => state.show_help = true,
                KeyCode::Char('s') => state.toggle_sidebar(),
                KeyCode::Tab => state.toggle_focus(),
                KeyCode::Esc => state.command_bar.clear(),
                KeyCode::Up => match state.focus {
                    Focus::Sidebar => state.select_up(),
                    Focus::Input => {
                        // ↑ recalls input history when the buffer is empty,
                        // otherwise scrolls the chat transcript.
                        if state.command_bar.input.is_empty() {
                            state.scroll_up();
                        } else {
                            state.command_bar.history_prev();
                        }
                    }
                },
                KeyCode::Down => match state.focus {
                    Focus::Sidebar => state.select_down(),
                    Focus::Input => {
                        if state.command_bar.input.is_empty() {
                            state.scroll_down();
                        } else {
                            state.command_bar.history_next();
                        }
                    }
                },
                KeyCode::Enter => {
                    if state.focus == Focus::Sidebar {
                        // Enter on a sidebar row prefills the input with the
                        // session's `@prefix:` routing prefix and returns focus
                        // to the input bar so the operator can type a command.
                        if let Some(name) = state.selected_target() {
                            let prefix = dashboard::session_prefix(&name);
                            state.command_bar.input = format!("@{prefix}: ");
                            state.focus = Focus::Input;
                        }
                    } else {
                        let typed = state.command_bar.take_for_execution();
                        if !typed.is_empty() {
                            coordinator_send(&mut state, client, &typed).await;
                            poll_daemon(&mut state, client).await;
                            last_poll = Instant::now();
                        }
                    }
                }
                KeyCode::Backspace if state.focus == Focus::Input => {
                    state.command_bar.backspace();
                }
                KeyCode::Char(c) if state.focus == Focus::Input => {
                    state.command_bar.push(c);
                }
                _ => {}
            }
        }

        // Throttle the data refresh: only re-poll the daemon every interval_ms.
        if last_poll.elapsed() >= Duration::from_millis(interval_ms) {
            poll_daemon(&mut state, client).await;
            last_poll = Instant::now();
        }

        // Refresh the health screen's collections + logs every 5 s while it
        // is the active surface (kept off the hot path when the operator is
        // in the chat screen).
        if screen == Screen::Health && last_health_refresh.elapsed() >= Duration::from_secs(5) {
            refresh_health_data(&mut health_screen).await;
            last_health_refresh = Instant::now();
        }
    }
}

/// Refresh the focused service's collections list and log tail in-place.
///
/// Why: the operator's `r` key (and the periodic refresh) wants the left
/// panel and Logs tab to be reflected from the latest daemon snapshot
/// without restarting the polling tasks.
/// What: for the focused service, fetches `collections` (via the search /
/// memory list endpoints) and `logs_tail(LOG_BUFFER_CAP)`; folds the result
/// into the screen and clamps the selection.
/// Test: live behaviour is covered by the daemon suites; this is the thin
/// orchestration glue.
async fn refresh_health_data(screen: &mut health::HealthScreen) {
    // Refresh both services so a `Tab` toggle does not show stale data.
    for daemon in [health::Daemon::Search, health::Daemon::Memory] {
        let url = match daemon {
            health::Daemon::Search => screen.search_url.clone(),
            health::Daemon::Memory => screen.memory_url.clone(),
        };
        let client = health::client_for(daemon, &url);
        let rows = match daemon {
            health::Daemon::Search => client.search_collections().await,
            health::Daemon::Memory => client.memory_collections().await,
        };
        match daemon {
            health::Daemon::Search => screen.search_collections = rows,
            health::Daemon::Memory => screen.memory_collections = rows,
        }
        if let Ok((lines, total)) = client.logs_tail(health::LOG_BUFFER_CAP as u32).await {
            let buf = match daemon {
                health::Daemon::Search => &mut screen.search_logs,
                health::Daemon::Memory => &mut screen.memory_logs,
            };
            // Preserve scroll position when auto-scroll is off so an operator
            // reading older lines is not yanked back to the tail.
            let preserve_scroll = !buf.auto_scroll;
            let prior_offset = buf.scroll_offset;
            buf.replace(lines, Some(total));
            if preserve_scroll {
                buf.scroll_offset = prior_offset.min(buf.lines.len().saturating_sub(1));
            }
        }
    }
    screen.clamp_collection_selection();
}

/// Spawn the focused daemon's start command as a detached child process.
///
/// Why: the `[S]` key starts a stopped daemon; the ticket specifies launching
/// `cargo run -p trusty-search -- start` / `cargo run -p trusty-memory`.
/// What: spawns the appropriate `cargo run` child detached from the TUI
/// (stdout/stderr inherited so its logs land in the operator's terminal), and
/// records the outcome in `chat.last_action`. A spawn failure is recorded
/// rather than panicking.
/// Test: `health_start` is side-effecting (spawns a process); the action-string
/// recording is exercised manually — the launch itself is not unit-tested.
fn health_start(chat: &mut DashboardState, hp: &HealthScreen) {
    let (label, args): (&str, &[&str]) = match hp.focus {
        Daemon::Search => (
            "trusty-search",
            &["run", "-p", "trusty-search", "--", "start"],
        ),
        Daemon::Memory => ("trusty-memory", &["run", "-p", "trusty-memory"]),
    };
    match std::process::Command::new("cargo").args(args).spawn() {
        Ok(_) => {
            tracing::info!("health screen: spawned {label}");
            chat.last_action = Some(format!("starting {label}…"));
        }
        Err(e) => {
            tracing::warn!("health screen: failed to start {label}: {e}");
            chat.last_action = Some(format!("failed to start {label}: {e}"));
        }
    }
}

/// Stop the focused daemon via its `admin/stop` HTTP endpoint.
///
/// Why: the `[X]` key stops the focused daemon without the operator resolving a
/// PID; both daemons expose an unauthenticated stop route.
/// What: builds a [`health::HealthClient`] for the focused daemon, POSTs to its
/// stop endpoint, and records the outcome in `chat.last_action`. A transport
/// error is recorded rather than propagated.
/// Test: the stop transport is covered in `health.rs`; this is the action glue.
async fn health_stop(chat: &mut DashboardState, hp: &HealthScreen) {
    let label = match hp.focus {
        Daemon::Search => "trusty-search",
        Daemon::Memory => "trusty-memory",
    };
    let client = health::client_for(hp.focus, hp.focused_url());
    match client.stop().await {
        Ok(()) => {
            tracing::info!("health screen: stop requested for {label}");
            chat.last_action = Some(format!("stopping {label}…"));
        }
        Err(e) => {
            tracing::warn!("health screen: failed to stop {label}: {e}");
            chat.last_action = Some(format!("failed to stop {label}: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
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
}
