//! TUI event loop and health-management helpers.
//!
//! Why: keeping the key-event dispatch, periodic polling, and health-action
//! handlers separate from the module-level setup (`run`, `run_focused`) and
//! the pure rendering helpers keeps each file under the 500-SLOC cap.
//! What: [`run_loop`] is the central polling + input dispatch loop; the
//! health helpers (`refresh_health_data`, `health_start`, `health_stop`) are
//! pure side-effecting functions called from the loop.
//! Test: the pure pieces (rendering, client, screen state) are unit-tested in
//! the containing module.

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
use ratatui::Terminal;

use super::client::DaemonClient;
use super::dashboard::{DashboardState, Focus};
use super::health::{self, Daemon, HealthScreen, HealthUpdate};
use super::{Screen, coordinator_send, poll_daemon, render_screen, spawn_health_pollers};

/// The dashboard event loop: poll the daemon, render, handle input.
///
/// Why: kept separate from [`super::run`] so terminal setup/teardown wraps it cleanly.
/// What: hosts both the coordinator chat (`[1]`) and the health screen (`[2]`)
/// — switching screens never resets either, since both states live for the
/// whole loop. Refreshes [`DashboardState`] from the daemon on an `interval_ms`
/// timer, drains background [`HealthUpdate`]s into the [`HealthScreen`], and
/// polls the keyboard every 50ms so input feels instantaneous. Number keys
/// switch screens; `q` quits from either.
/// Test: the pure pieces (rendering, client, screen state) are unit-tested.
pub(super) async fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    client: &mut DaemonClient,
    interval_ms: u64,
    focus_id: Option<String>,
) -> anyhow::Result<()> {
    // The sidebar starts visible only when there is at least one session to
    // show; otherwise the coordinator chat gets the full width immediately.
    let mut state = DashboardState::default();
    let mut screen = Screen::default();
    // Issue #2030: seed the memory panel from discovery (TRUSTY_MEMORY_URL
    // override, else the daemon's actual discovered bound address) rather
    // than a hardcoded port. This resolved value is what the background
    // poller's `HealthClient` is built with and polls for the TUI's lifetime.
    let mut health_screen = HealthScreen::new(
        health::DEFAULT_SEARCH_URL,
        trusty_common::mcp::memory_rpc::resolve_memory_base_url_or_unreachable(),
    );

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
                            let prefix = super::dashboard::session_prefix(&name);
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
pub(super) async fn refresh_health_data(screen: &mut health::HealthScreen) {
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
/// `cargo run -p trusty-search -- start` / `cargo run -p trusty-memory`. The
/// spawn routes through [`crate::core::spawn_disclaim::disclaimed_spawn_detached`]
/// (issue #3126) so macOS TCC does not attribute the child `cargo build`'s
/// file access back to the signed `trusty-mpm` daemon — this was the last
/// undisclaimed `Command::new(..).spawn()` call site in the crate.
/// What: spawns the appropriate `cargo run` child detached from the TUI and
/// records the outcome in `chat.last_action`. A spawn failure is recorded
/// rather than panicking.
/// Test: `health_start` is side-effecting (spawns a process); the action-string
/// recording is exercised manually. The disclaim itself is covered by
/// `disclaimed_spawn_detached_*` in `crate::core::spawn_disclaim`.
pub(super) fn health_start(chat: &mut DashboardState, hp: &HealthScreen) {
    let (label, args): (&str, &[&str]) = match hp.focus {
        Daemon::Search => (
            "trusty-search",
            &["run", "-p", "trusty-search", "--", "start"],
        ),
        Daemon::Memory => ("trusty-memory", &["run", "-p", "trusty-memory"]),
    };
    match crate::core::spawn_disclaim::disclaimed_spawn_detached("cargo", args) {
        Ok(()) => {
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
pub(super) async fn health_stop(chat: &mut DashboardState, hp: &HealthScreen) {
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
