//! The console home screen `tm tui` opens on (#10491).
//!
//! Why: operators asked for one TUI that starts at the console's service
//! roster and drills into each service, with trusty-mpm's tile leading to
//! today's projects view. It is built only from interfaces the services
//! already serve; trusty-mpm keeps no edge on trusty-console (#1318).
//! What: [`run_tui_entry`] routes `tm tui` to the home screen (or the
//! `--single-pane` chat); [`run_home`] owns the terminal, drains probe and
//! metrics results from background tasks, draws, and routes keys.
//! Test: `tm_tui_opens_on_the_console_home_screen` and the state, probe and
//! render tests in `tests.rs`.

// #10966: the console look (goal ar-G01).
pub mod console;
pub mod probe;
pub mod render;
pub mod services;
pub mod state;

#[cfg(test)]
mod tests;
// #10966: the console look's render tests.
#[cfg(test)]
mod console_tests;
// #10476: the analyze probe driven over real sockets.
#[cfg(test)]
mod analyze_probe_tests;

#[cfg(test)]
mod no_uds_endpoint_tests;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};
use tokio::sync::mpsc;

use crate::client::DaemonClient;
use crate::tui::architect::{ArchitectUpdate, data, live};
use crate::tui::terminal::TerminalGuard;
use services::ServiceId;
use state::{HomeAction, HomeState, MetricsUpdate, View, handle_key};

/// How often each tile is re-probed.
pub const HEALTH_INTERVAL: Duration = Duration::from_secs(5);
/// The deadline on one health probe.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// The deadline on one `console_metrics` fetch, spawn included.
pub const METRICS_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the loop waits for a key before redrawing.
const KEY_POLL: Duration = Duration::from_millis(50);

/// Which surface `tm tui` opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiEntry {
    /// The console home screen — the default since #10491.
    Home,
    /// The single-pane coordinator chat — `--single-pane` opt-in.
    SinglePane,
}

/// Resolve `tm tui`'s first surface from the `--single-pane` flag.
///
/// Test: `tm_tui_opens_on_the_console_home_screen`.
pub fn tui_entry(single_pane: bool) -> TuiEntry {
    // #10491: the home screen replaces the projects view as `tm tui`'s start;
    // `tm projects` and `tm attach` still open the projects view directly.
    if single_pane {
        TuiEntry::SinglePane
    } else {
        TuiEntry::Home
    }
}

/// `tm tui`'s entry point.
///
/// Test: routing via `tm_tui_opens_on_the_console_home_screen`; the await
/// glue is exercised by launching the TUI.
pub async fn run_tui_entry(
    client: DaemonClient,
    interval_ms: u64,
    single_pane: bool,
    mouse: bool,
    architect_store: Option<PathBuf>,
) -> anyhow::Result<()> {
    match tui_entry(single_pane) {
        TuiEntry::Home => run_home(client, interval_ms, mouse, architect_store).await,
        TuiEntry::SinglePane => {
            crate::tui::run_initial_view_with(client, interval_ms, None, true, false).await
        }
    }
}

/// Run the home screen until the operator quits.
///
/// Why: probes and metrics fetches run on spawned tasks and arrive over
/// channels, so the loop below only drains, draws and reads keys — a slow
/// service never delays a keystroke.
/// What: spawns the health poller, enters the terminal under a
/// [`TerminalGuard`], then loops. Enter on trusty-mpm hands the same terminal
/// to `project_ctl::run_in_terminal`; leaving that view (`q`) returns here.
/// #10476: `mouse` applies to that view; this screen never captures.
/// #10966: while the Architect view is open, a fetch task reads the store at
/// `architect_store` and the Live tracker every `architect::REFRESH`, over
/// the daemon socket.
/// Test: the pure pieces are unit-tested; this loop is exercised by the
/// smoke run.
pub async fn run_home(
    mut client: DaemonClient,
    interval_ms: u64,
    mouse: bool,
    architect_store: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (architect_tx, mut architect_rx) = mpsc::channel::<ArchitectUpdate>(4);
    let (health_tx, mut health_rx) = mpsc::channel(64);
    let (metrics_tx, mut metrics_rx) = mpsc::channel::<MetricsUpdate>(16);
    probe::spawn_health_poller(
        &ServiceId::ALL,
        |id| probe::probe_service(id, PROBE_TIMEOUT),
        health_tx,
        HEALTH_INTERVAL,
    );

    let mut terminal = crate::tui::terminal::enter()?;
    let _guard = TerminalGuard;
    let mut state = HomeState::default();

    loop {
        while let Ok(update) = health_rx.try_recv() {
            state.apply_health(update);
        }
        while let Ok(update) = metrics_rx.try_recv() {
            state.apply_metrics(update, Instant::now());
        }
        while let Ok(update) = architect_rx.try_recv() {
            state.architect.apply(update);
        }
        if state.view == View::Architect && state.architect.fetch_due(Instant::now()) {
            let tx = architect_tx.clone();
            let client = client.clone();
            let store = architect_store.clone();
            let selected = state.architect.selected_session();
            tokio::spawn(async move {
                let socket = client.socket_path();
                let dashboard = data::fetch_dashboard(socket, store.as_deref()).await;
                let live = live::fetch_live(&client, selected.as_deref()).await;
                let _ = tx.send(ArchitectUpdate { dashboard, live }).await;
            });
        }
        terminal.draw(|frame| render::render(frame, &state))?;

        if !event::poll(KEY_POLL)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match handle_key(&mut state, key, Instant::now()) {
            Some(HomeAction::OpenProjects) => {
                crate::tui::project_ctl::run_in_terminal(
                    &mut terminal,
                    &mut client,
                    interval_ms,
                    mouse,
                )
                .await?;
                terminal.clear()?;
            }
            Some(HomeAction::FetchMetrics(id)) => {
                let tx = metrics_tx.clone();
                tokio::spawn(async move {
                    let _ = tx
                        .send(probe::fetch_metrics(id, METRICS_TIMEOUT).await)
                        .await;
                });
            }
            None => {}
        }
        if state.should_exit {
            return Ok(());
        }
    }
}
