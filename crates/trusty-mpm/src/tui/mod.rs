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
pub mod coordinator;
pub mod dashboard;
mod event_loop;
pub mod health;
pub mod iterm2;
pub mod project_ctl;

use crossterm::{
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
use dashboard::{ChatMessage, DashboardState};
use event_loop::run_loop;
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
pub(crate) async fn poll_daemon(state: &mut DashboardState, client: &mut DaemonClient) {
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
/// panel or the keyboard. `memory_url` is resolved once by the caller before
/// this is invoked (issue #2030: via discovery — `TRUSTY_MEMORY_URL` override,
/// else the daemon's actual discovered bound address — never a hardcoded
/// port); each task then reuses one cached [`health::HealthClient`] for its
/// whole lifetime.
/// What: spawns one task per daemon; each task polls its [`health::HealthClient`]
/// on [`health::POLL_INTERVAL`] and sends every result down `tx` as a
/// [`HealthUpdate`]. A task exits quietly once the receiver is dropped (the TUI
/// is shutting down). The first poll fires immediately so the panels leave the
/// `Connecting` state quickly.
/// Test: the per-poll projection and routing are unit-tested in `health.rs`;
/// this is the thin task-spawning glue.
pub(crate) fn spawn_health_pollers(
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
/// every message goes to `POST /api/v1/sessions/chat`, which either routes a
/// `@prefix:` command at a session or answers via the LLM.
/// What: appends the user message to the transcript, calls the daemon, then
/// appends the coordinator reply (or routed-command output). A `None` response
/// (LLM not configured) or a transport error becomes a coordinator-authored
/// note so a failure is always renderable, never a panic.
/// Test: `coordinator_send_without_daemon_reports_error`.
pub(crate) async fn coordinator_send(
    state: &mut DashboardState,
    client: &DaemonClient,
    message: &str,
) {
    state.push_chat(ChatMessage::user(message));
    match client
        .coordinator_chat(message, &state.coord_history, false)
        .await
    {
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
pub(crate) fn render_screen(
    frame: &mut Frame,
    screen: Screen,
    chat: &DashboardState,
    hp: &HealthScreen,
) {
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

#[cfg(test)]
mod tests;
