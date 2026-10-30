//! Pure state and key routing for the console home screen.
//!
//! Why (#10491): the navigation, tile states, Enter routing and the metrics
//! throttle are the parts that can be wrong; keeping them free of terminal
//! and network I/O makes each one a plain unit test.
//! What: [`HomeState`], [`TileHealth`], [`MetricsState`], [`handle_key`] and
//! the [`HomeAction`] it returns for the event loop to carry out.
//! Test: `tests.rs` beside this file.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use trusty_common::console_metrics::ConsoleMetricsReport;

use super::services::ServiceId;

/// Tiles per grid row; vertical moves step by this many.
pub const COLUMNS: usize = 4;

/// How long a fetched (or failed) `console_metrics` answer is reused before
/// opening the view spawns the service's stdio command again.
pub const METRICS_TTL: Duration = Duration::from_secs(30);

/// One tile's health, as the last probe saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileHealth {
    /// No probe has answered yet.
    Pending,
    /// The service answered health.
    Up {
        /// The version it reported, when it sent one.
        version: Option<String>,
    },
    /// The service answered, but not with health.
    Degraded {
        /// Why the answer was not health.
        reason: String,
    },
    /// Installed, but nothing answered.
    Down {
        /// What the probe saw.
        reason: String,
    },
    /// The binary is not on `PATH`.
    Absent,
    /// The probe overran its deadline.
    Timeout,
    /// Installed; runs on demand with no resident daemon.
    OnDemand,
}

impl TileHealth {
    /// The one-word status a tile shows.
    pub fn label(&self) -> &'static str {
        match self {
            TileHealth::Pending => "probing",
            TileHealth::Up { .. } => "up",
            TileHealth::Degraded { .. } => "degraded",
            TileHealth::Down { .. } => "down",
            TileHealth::Absent => "not installed",
            TileHealth::Timeout => "timeout",
            TileHealth::OnDemand => "on demand",
        }
    }

    /// Whether a stdio `console_metrics` spawn may run. A spawned bridge can
    /// auto-start a stopped daemon, so only an answering (or on-demand)
    /// service is asked.
    pub fn metrics_allowed(&self) -> bool {
        matches!(self, TileHealth::Up { .. } | TileHealth::OnDemand)
    }
}

/// One probe result, sent from a poll task to the UI loop.
#[derive(Debug, Clone)]
pub struct HealthUpdate {
    /// The service probed.
    pub id: ServiceId,
    /// What the probe saw.
    pub health: TileHealth,
}

/// One `console_metrics` result, sent from a fetch task to the UI loop.
#[derive(Debug, Clone)]
pub struct MetricsUpdate {
    /// The service asked.
    pub id: ServiceId,
    /// The report, or why there is none.
    pub result: Result<ConsoleMetricsReport, String>,
}

/// The cached `console_metrics` state for one service view.
#[derive(Debug, Clone)]
pub enum MetricsState {
    /// A fetch is in flight.
    Loading,
    /// The last fetch answered.
    Ready {
        /// The decoded report.
        report: Box<ConsoleMetricsReport>,
        /// When it arrived.
        at: Instant,
    },
    /// The last fetch failed, or was not attempted.
    Unavailable {
        /// Why.
        reason: String,
        /// When this was decided.
        at: Instant,
    },
}

/// Which surface the home screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The tile grid.
    Home,
    /// One service's first-level view.
    Service(ServiceId),
}

/// Work the event loop must do after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeAction {
    /// Hand the terminal to today's projects view (`tui::project_ctl`).
    OpenProjects,
    /// Spawn one background `console_metrics` fetch for this service.
    FetchMetrics(ServiceId),
}

/// The whole home-screen state.
#[derive(Debug, Clone)]
pub struct HomeState {
    /// One entry per tile, in [`ServiceId::ALL`] order.
    pub tiles: Vec<(ServiceId, TileHealth)>,
    /// The selected tile index.
    pub selected: usize,
    /// The surface on screen.
    pub view: View,
    /// Cached metrics per service.
    pub metrics: HashMap<ServiceId, MetricsState>,
    /// Set by `q` / Ctrl-C.
    pub should_exit: bool,
}

impl Default for HomeState {
    fn default() -> Self {
        Self {
            tiles: ServiceId::ALL
                .iter()
                .map(|&id| (id, TileHealth::Pending))
                .collect(),
            selected: 0,
            view: View::Home,
            metrics: HashMap::new(),
            should_exit: false,
        }
    }
}

impl HomeState {
    /// The health last recorded for `id`.
    pub fn health(&self, id: ServiceId) -> &TileHealth {
        self.tiles
            .iter()
            .find(|(tile, _)| *tile == id)
            .map(|(_, health)| health)
            .unwrap_or(&TileHealth::Pending)
    }

    /// The selected service.
    pub fn selected_id(&self) -> ServiceId {
        self.tiles
            .get(self.selected)
            .map(|(id, _)| *id)
            .unwrap_or(ServiceId::Mpm)
    }

    /// Record one probe result.
    pub fn apply_health(&mut self, update: HealthUpdate) {
        if let Some(tile) = self.tiles.iter_mut().find(|(id, _)| *id == update.id) {
            tile.1 = update.health;
        }
    }

    /// Record one `console_metrics` result.
    pub fn apply_metrics(&mut self, update: MetricsUpdate, now: Instant) {
        let state = match update.result {
            Ok(report) => MetricsState::Ready {
                report: Box::new(report),
                at: now,
            },
            Err(reason) => MetricsState::Unavailable { reason, at: now },
        };
        self.metrics.insert(update.id, state);
    }

    /// Decide whether opening `id`'s view should spawn a fetch.
    ///
    /// Why (#10491): the stdio command is a process spawn; it must not run per
    /// render, per tick, or per repeated Enter.
    /// What: `true` (and marks `Loading`) only when the service offers the
    /// tool, its tile allows a spawn, no fetch is in flight, and the cached
    /// answer is older than [`METRICS_TTL`]. A tile that does not allow a
    /// spawn records `Unavailable` instead.
    /// Test: `metrics_fetch_is_throttled_by_the_ttl`,
    /// `a_down_service_shows_unavailable_without_a_fetch`.
    pub fn request_metrics(&mut self, id: ServiceId, now: Instant) -> bool {
        if id.metrics_command().is_none() {
            let reason = format!("{} offers no console_metrics tool", id.id());
            self.metrics
                .insert(id, MetricsState::Unavailable { reason, at: now });
            return false;
        }
        let fresh = match self.metrics.get(&id) {
            Some(MetricsState::Loading) => true,
            Some(MetricsState::Ready { at, .. } | MetricsState::Unavailable { at, .. }) => {
                now.saturating_duration_since(*at) < METRICS_TTL
            }
            None => false,
        };
        if fresh {
            return false;
        }
        let health = self.health(id);
        if !health.metrics_allowed() {
            let reason = format!("service is {}", health.label());
            self.metrics
                .insert(id, MetricsState::Unavailable { reason, at: now });
            return false;
        }
        self.metrics.insert(id, MetricsState::Loading);
        true
    }

    /// Move the selection by `delta`, clamped to the grid.
    fn move_by(&mut self, delta: isize) {
        let last = self.tiles.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }
}

/// Route one key press.
///
/// Why (#10491): Enter on trusty-mpm opens today's projects view; Enter on any
/// other tile opens its service view; Esc returns home.
/// What: mutates `state` and returns the I/O the event loop must start.
/// Test: `enter_on_mpm_opens_projects`, `enter_on_search_opens_its_view_and_esc_returns`,
/// `arrows_move_the_selection_within_the_grid`, `q_quits_from_any_view`.
pub fn handle_key(state: &mut HomeState, key: KeyEvent, now: Instant) -> Option<HomeAction> {
    if key.code == KeyCode::Char('q')
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
    {
        state.should_exit = true;
        return None;
    }
    match state.view {
        View::Service(_) => {
            if matches!(key.code, KeyCode::Esc | KeyCode::Backspace) {
                state.view = View::Home;
            }
            None
        }
        View::Home => match key.code {
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                state.move_by(1);
                None
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                state.move_by(-1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.move_by(COLUMNS as isize);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                state.move_by(-(COLUMNS as isize));
                None
            }
            KeyCode::Enter => {
                let id = state.selected_id();
                if id == ServiceId::Mpm {
                    return Some(HomeAction::OpenProjects);
                }
                state.view = View::Service(id);
                state
                    .request_metrics(id, now)
                    .then_some(HomeAction::FetchMetrics(id))
            }
            _ => None,
        },
    }
}
