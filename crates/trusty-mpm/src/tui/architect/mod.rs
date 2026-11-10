//! The Architect view in `tm tui`: the dashboard lists, read-only.
//!
//! Why (#10966, goal ar-G01): Bob asked for the Architect dashboard in the
//! terminal — decisions, actions, goals, sessions, tasks and Q&A as lists
//! with badges. Dc4 rules the view read-only: no key writes, and
//! [`data`] sends only `list` and `tally`.
//! What: [`ArchitectState`] holds the tab, selection and the last fetched
//! [`data::DashSnapshot`]; [`handle_key`] routes keys; [`render::render`]
//! draws. The Live tab is a name only here; its tracker is a later slice.
//! Design: `docs/design/UI/tm-tui-console-look.md` section 4.
//! Test: `tests.rs` beside this file.

pub mod data;
pub mod render;

#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};

use data::DashSnapshot;

/// How often the open view re-reads the store.
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
    /// The process tracker; its body is not built yet (#10966).
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

/// One fetch round's result, sent from the fetch task to the UI loop.
pub type ArchitectUpdate = Result<DashSnapshot, String>;

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
    in_flight: bool,
    last_fetch: Option<Instant>,
}

impl Default for ArchitectState {
    fn default() -> Self {
        Self {
            tab: Tab::Decisions,
            selected: 0,
            expanded: false,
            dashboard: Fetch::Waiting,
            in_flight: false,
            last_fetch: None,
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
        self.last_fetch = None;
    }

    /// Show `tab` without refetching.
    fn switch(&mut self, tab: Tab) {
        self.tab = tab;
        self.selected = 0;
        self.expanded = false;
    }

    /// Whether a fetch should start now; marks it in flight when so.
    ///
    /// Test: `fetch_is_due_once_per_refresh_and_never_twice_in_flight`.
    pub fn fetch_due(&mut self, now: Instant) -> bool {
        let stale = self
            .last_fetch
            .is_none_or(|t| now.saturating_duration_since(t) >= REFRESH);
        if self.in_flight || !stale {
            return false;
        }
        self.in_flight = true;
        self.last_fetch = Some(now);
        true
    }

    /// Record one fetch round and keep the selection inside the list.
    pub fn apply(&mut self, update: ArchitectUpdate) {
        self.in_flight = false;
        self.dashboard = match update {
            Ok(s) => Fetch::Ready(s),
            Err(e) => Fetch::Failed(e),
        };
        let len = self.row_count();
        if self.selected >= len {
            self.selected = len.saturating_sub(1);
        }
    }

    /// Rows on the open tab; the Live tab has none yet.
    pub fn row_count(&self) -> usize {
        match &self.dashboard {
            Fetch::Ready(d) => d.rows(self.tab).len(),
            _ => 0,
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
        KeyCode::Char('r') => state.last_fetch = None,
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
