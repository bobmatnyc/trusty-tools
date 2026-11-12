//! The Architect view in `tm tui`: the dashboard lists, read-only, and the
//! Live tracker.
//!
//! Why (#10966, goal ar-G01): Bob asked for the Architect dashboard in the
//! terminal — decisions, actions, goals, sessions, tasks and Q&A as lists
//! with badges — and a vertical tracker in place of Visualize. Dc4 rules the
//! view read-only: no key writes, and [`data`] sends only `list` and
//! `tally`. The tracker is list-only (Dc4, Q4).
//! What: [`ArchitectState`] holds the tab, selection and the last fetched
//! [`data::DashSnapshot`] and [`live::LiveSnapshot`]; [`handle_key`] routes
//! keys; [`render::render`] draws.
//! Design: `docs/design/UI/tm-tui-console-look.md` section 4.
//! Test: `tests.rs` beside this file.

pub mod data;
pub mod live;
pub mod render;

#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};

use data::DashSnapshot;
use live::LiveSnapshot;

/// How often the open view re-reads the store and the fleet.
pub const REFRESH: Duration = Duration::from_secs(5);

/// The view's tabs, in the dashboard's order, with Live in place of Visualize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Open items of kind `question`.
    Decisions,
    /// Open items of kind `action`.
    Actions,
    /// Active and parked goals.
    Goals,
    /// Session documents.
    Sessions,
    /// Open items of kind `task`.
    Tasks,
    /// Q&A documents.
    Qa,
    /// The vertical session tracker (#10966).
    Live,
}

impl Tab {
    /// Every tab, left to right.
    pub const ALL: [Tab; 7] = [
        Tab::Decisions,
        Tab::Actions,
        Tab::Goals,
        Tab::Sessions,
        Tab::Tasks,
        Tab::Qa,
        Tab::Live,
    ];

    /// The tab's label.
    pub fn label(self) -> &'static str {
        match self {
            Tab::Decisions => "Decisions",
            Tab::Actions => "Actions",
            Tab::Goals => "Goals",
            Tab::Sessions => "Sessions",
            Tab::Tasks => "Tasks",
            Tab::Qa => "Q&A",
            Tab::Live => "Live",
        }
    }

    fn step(self, delta: isize) -> Tab {
        let i = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0) as isize;
        let n = Tab::ALL.len() as isize;
        Tab::ALL[(i + delta).rem_euclid(n) as usize]
    }
}

/// One fetched source: not asked yet, an answer, or why there is none.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetch<T> {
    /// No answer yet.
    Waiting,
    /// The last answer.
    Ready(T),
    /// The last call failed, or cannot be made; the text says why.
    Failed(String),
}

/// One source's result, sent from its fetch task to the UI loop.
///
/// #10966: one message per source, so a slow dashboard fetch never holds
/// the Live tracker's result back.
#[derive(Debug, Clone)]
pub enum ArchitectUpdate {
    /// The dashboard lists, or why not.
    Dashboard(Result<DashSnapshot, String>),
    /// The tracker, or why not.
    Live(Result<LiveSnapshot, String>),
}

/// Which sources a fetch round should ask now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Due {
    /// Start a dashboard fetch.
    pub dashboard: bool,
    /// Start a Live tracker fetch.
    pub live: bool,
}

/// One source's refresh clock: whether a fetch is out, and when it started.
#[derive(Debug, Clone, Copy, Default)]
struct Clock {
    in_flight: bool,
    last: Option<Instant>,
}

impl Clock {
    /// Whether a fetch should start now; marks it in flight when so.
    fn due(&mut self, now: Instant) -> bool {
        let stale = self
            .last
            .is_none_or(|t| now.saturating_duration_since(t) >= REFRESH);
        if self.in_flight || !stale {
            return false;
        }
        self.in_flight = true;
        self.last = Some(now);
        true
    }
}

/// Everything the Architect view shows.
#[derive(Debug, Clone)]
pub struct ArchitectState {
    /// The open tab.
    pub tab: Tab,
    /// The selected row on that tab.
    pub selected: usize,
    /// Whether the selected row's detail pane is open.
    pub expanded: bool,
    /// The dashboard lists.
    pub dashboard: Fetch<DashSnapshot>,
    /// The Live tracker.
    pub live: Fetch<LiveSnapshot>,
    // #10966: one clock per source, so each refreshes on its own schedule.
    dashboard_clock: Clock,
    live_clock: Clock,
}

impl Default for ArchitectState {
    fn default() -> Self {
        Self {
            tab: Tab::Decisions,
            selected: 0,
            expanded: false,
            dashboard: Fetch::Waiting,
            live: Fetch::Waiting,
            dashboard_clock: Clock::default(),
            live_clock: Clock::default(),
        }
    }
}

/// What the event loop must do after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchitectAction {
    /// Leave for the home screen.
    Home,
}

impl ArchitectState {
    /// Open on `tab`, asking for a fresh fetch.
    pub fn open(&mut self, tab: Tab) {
        self.switch(tab);
        self.refetch();
    }

    /// Make every source due at the next check.
    fn refetch(&mut self) {
        self.dashboard_clock.last = None;
        self.live_clock.last = None;
    }

    /// Show `tab` without refetching.
    fn switch(&mut self, tab: Tab) {
        self.tab = tab;
        self.selected = 0;
        self.expanded = false;
    }

    /// Which sources should be fetched now; marks each due one in flight.
    ///
    /// Why (#10966): the dashboard fetch can take many seconds against a
    /// wedged daemon; a shared clock would hold the Live tracker's refresh
    /// behind it and leave old rows on screen.
    /// What: each source is due once per [`REFRESH`] and never twice in
    /// flight, independent of the other.
    /// Test: `fetch_is_due_once_per_refresh_and_never_twice_in_flight`,
    /// `slow_dashboard_fetch_does_not_hold_the_live_tracker`.
    pub fn fetch_due(&mut self, now: Instant) -> Due {
        Due {
            dashboard: self.dashboard_clock.due(now),
            live: self.live_clock.due(now),
        }
    }

    /// Record one source's result and keep the selection inside the list.
    ///
    /// An `Err` replaces an earlier `Ready`, so a failed fetch never leaves
    /// stale rows drawn as current.
    /// Test: `failed_live_fetch_replaces_the_earlier_sessions_on_screen`.
    pub fn apply(&mut self, update: ArchitectUpdate) {
        match update {
            ArchitectUpdate::Dashboard(r) => {
                self.dashboard_clock.in_flight = false;
                self.dashboard = r.map_or_else(Fetch::Failed, Fetch::Ready);
            }
            ArchitectUpdate::Live(r) => {
                self.live_clock.in_flight = false;
                // #10966: an `Err` must replace the old `Ready` (fail closed).
                self.live = r.map_or_else(Fetch::Failed, Fetch::Ready);
            }
        }
        let len = self.row_count();
        if self.selected >= len {
            self.selected = len.saturating_sub(1);
        }
    }

    /// Rows on the open tab.
    pub fn row_count(&self) -> usize {
        self.count(self.tab).unwrap_or(0)
    }

    /// Rows `tab` lists, or `None` before its source has answered.
    pub fn count(&self, tab: Tab) -> Option<usize> {
        match (tab, &self.dashboard, &self.live) {
            (Tab::Live, _, Fetch::Ready(l)) => Some(l.sessions.len()),
            (Tab::Live, _, _) => None,
            (tab, Fetch::Ready(d), _) => Some(d.rows(tab).len()),
            _ => None,
        }
    }

    /// The id of the session selected on the Live tab, whose step is asked.
    ///
    /// Test: `selected_session_names_the_live_row_only_on_the_live_tab`.
    pub fn selected_session(&self) -> Option<String> {
        match (self.tab, &self.live) {
            (Tab::Live, Fetch::Ready(l)) => l.sessions.get(self.selected).map(|s| s.id.clone()),
            _ => None,
        }
    }
}

/// Route one key press on the Architect view.
///
/// Why: read-only navigation only — no key writes (Dc4).
/// What: Tab/BackTab and ←/→ change tab; ↑/↓ move the selection; Enter
/// toggles the detail pane; `r` refetches now; Esc closes the detail pane or
/// returns home.
/// Test: `tab_keys_cycle_and_esc_closes_detail_then_returns_home`.
pub fn handle_key(state: &mut ArchitectState, key: KeyEvent) -> Option<ArchitectAction> {
    match key.code {
        KeyCode::Tab | KeyCode::Right => state.switch(state.tab.step(1)),
        KeyCode::BackTab | KeyCode::Left => state.switch(state.tab.step(-1)),
        KeyCode::Down | KeyCode::Char('j') => {
            let last = state.row_count().saturating_sub(1);
            state.selected = (state.selected + 1).min(last);
        }
        KeyCode::Up | KeyCode::Char('k') => state.selected = state.selected.saturating_sub(1),
        KeyCode::Enter => state.expanded = !state.expanded,
        KeyCode::Char('r') => state.refetch(),
        KeyCode::Esc | KeyCode::Backspace => {
            if state.expanded {
                state.expanded = false;
            } else {
                return Some(ArchitectAction::Home);
            }
        }
        _ => {}
    }
    None
}
