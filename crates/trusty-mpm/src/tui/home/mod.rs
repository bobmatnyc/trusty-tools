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

pub mod probe;
pub mod render;
pub mod services;
pub mod state;

#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};
use tokio::sync::mpsc;

use crate::client::DaemonClient;
use crate::tui::terminal::TerminalGuard;
use services::ServiceId;
use state::{HomeAction, HomeState, MetricsUpdate, handle_key};

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
) -> anyhow::Result<()> {
    match tui_entry(single_pane) {
        TuiEntry::Home => run_home(client, interval_ms).await,
        TuiEntry::SinglePane => {
            crate::tui::run_initial_view_with(client, interval_ms, None, true).await
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
/// Test: the pure pieces are unit-tested; this loop is exercised by the
/// smoke run.
pub async fn run_home(mut client: DaemonClient, interval_ms: u64) -> anyhow::Result<()> {
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
                crate::tui::project_ctl::run_in_terminal(&mut terminal, &mut client, interval_ms)
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
