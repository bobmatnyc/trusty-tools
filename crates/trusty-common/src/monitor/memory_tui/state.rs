//! Core state types for the memory TUI.
//!
//! Why: the event loop polls the daemon, streams SSE events, and handles
//! input — keeping every piece of state in one module makes the loop terse,
//! the rendering a pure function of a state snapshot, and the types testable
//! without a terminal backend.
//! What: [`MemoryFocus`], [`DreamBackoff`], [`DrawerListState`], and
//! [`MemoryTuiState`] together with their impl blocks.
//! Test: `cargo test -p trusty-common --features monitor-tui` covers these
//! via the `tests` module in `mod.rs`.

use std::time::{Duration, Instant};

use crate::monitor::dashboard::{MemoryData, PalaceRow};
use crate::monitor::memory_client::{DrawerInfo, MemoryDetail};
use crate::monitor::tui_common::ThreeWaySortKey;
use crate::monitor::utils::{ActivityLog, DaemonStatus};

/// Number of results requested per recall query.
pub const RECALL_TOP_K: usize = 5;

/// Default page size for the ACTIVITY drawer list.
///
/// Why: the activity panel is narrow and only renders a handful of rows; 20
/// drawers per page balances "feels paged" with "stays inside one screen
/// scroll" for typical terminal heights.
/// What: 20 drawers per fetch.
/// Test: `drawer_state_default_page_size`.
pub const DRAWER_PAGE_SIZE: usize = 20;

/// Initial backoff after a single dream-cycle failure.
///
/// Why: when the trusty-memory daemon is down (or the lock file points at a
/// stale port), pressing `[d]` would previously fire one request per keystroke
/// and flood the activity log with `dream failed` lines at ~1-2 s cadence. A
/// short initial cooldown plus exponential growth keeps the log readable while
/// still letting the operator retry quickly once the daemon comes back.
/// What: 5 seconds — the first failure blocks further attempts for 5 s.
pub const DREAM_BACKOFF_INITIAL: Duration = Duration::from_secs(5);

/// Ceiling on the dream-cycle retry backoff.
///
/// Why: the backoff doubles after each consecutive failure; without a ceiling
/// it would grow unbounded. Five minutes is long enough to be unobtrusive but
/// short enough that recovery is detected within one cycle.
/// What: 5 minutes (300 s) — caps the doubled backoff.
pub const DREAM_BACKOFF_MAX: Duration = Duration::from_secs(300);

/// Which zone of the memory UI currently holds keyboard focus.
///
/// Why (issue #215): the memory TUI added a third focus zone — the right-hand
/// drawer pane — alongside the existing palace list and recall input bar.
/// The shared `tui_common::ListFocus` only covers the two-zone model used by
/// the search TUI, so the memory UI carries its own three-way enum and the
/// shared focus helpers are no longer used here.
/// What: three variants — `List` (palace list), `DrawerPane` (right-hand
/// drawer activity panel), and `Input` (recall bar). `Tab` cycles
/// `List → DrawerPane → Input → List`.
/// Test: `test_toggle_focus`, `test_focus_tab_cycle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemoryFocus {
    /// The palace list has focus; arrows move the selection.
    #[default]
    List,
    /// The drawer activity panel has focus; arrows move the drawer cursor and
    /// `Enter` opens the detail modal.
    DrawerPane,
    /// The recall input bar has focus; typed characters edit the query.
    Input,
}

impl MemoryFocus {
    /// Cycle to the next focus zone (issue #215).
    ///
    /// Why: `[Tab]` walks through every focusable zone so the operator can
    /// reach the new drawer pane without a mouse.
    /// What: returns the next variant in the order
    /// `List → DrawerPane → Input → List`.
    /// Test: `test_focus_tab_cycle`.
    pub fn next(self) -> Self {
        match self {
            Self::List => Self::DrawerPane,
            Self::DrawerPane => Self::Input,
            Self::Input => Self::List,
        }
    }

    /// Legacy two-way toggle preserved for the public API.
    ///
    /// Why: a handful of callers (and tests) historically swapped focus
    /// between the list and the recall bar; the new three-way cycle would
    /// surprise them. This stays as a thin alias for the legacy behaviour
    /// (`List ↔ Input`) and explicitly drops `DrawerPane` through to `List`
    /// so the old flip never lands on the new zone.
    /// What: `List → Input`, `Input → List`, `DrawerPane → List`.
    /// Test: `test_toggle_focus`.
    pub fn toggled(self) -> Self {
        match self {
            Self::List => Self::Input,
            Self::Input => Self::List,
            Self::DrawerPane => Self::List,
        }
    }
}

/// Exponential-backoff gate for repeated dream-cycle attempts.
///
/// Why: when the trusty-memory daemon is unreachable, pressing `[d]` (or key
/// repeat from holding `d`) used to flood the activity log with one failure
/// per attempt at ~1 s cadence. This gate enforces a minimum interval between
/// attempts that doubles after each consecutive failure, suppresses log noise
/// after the first failure of a down-period, and resets on the first success.
/// What: tracks the earliest [`Instant`] at which the next attempt may fire,
/// the consecutive-failure count, and whether the down-period's first failure
/// has already been logged so subsequent attempts can stay silent at INFO.
/// Test: `dream_backoff_*` unit tests cover the state transitions.
#[derive(Debug, Clone, Default)]
pub struct DreamBackoff {
    /// Wall-clock instant at which the next dream attempt is allowed.
    pub(crate) next_allowed_at: Option<Instant>,
    /// Number of consecutive failures observed since the last success.
    pub(crate) consecutive_failures: u32,
    /// `true` once the first failure of the current down-period has been
    /// surfaced in the activity log; flips back to `false` on success so the
    /// next down-period's first failure is reported again.
    pub(crate) first_failure_logged: bool,
}

impl DreamBackoff {
    /// Build a fresh backoff gate with no pending cooldown.
    ///
    /// Why: the TUI state needs a starting value that allows the first attempt
    /// to fire immediately.
    /// What: returns the default — no `next_allowed_at`, zero failures.
    /// Test: `dream_backoff_allows_first_attempt`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a fresh dream attempt is allowed at `now`.
    ///
    /// Why: the `[d]` handler must skip the network call while a cooldown is
    /// active so it stops flooding the daemon with doomed requests.
    /// What: returns `true` when no cooldown has been set, or when `now` has
    /// reached the stored `next_allowed_at`.
    /// Test: `dream_backoff_blocks_within_window`.
    pub fn ready(&self, now: Instant) -> bool {
        match self.next_allowed_at {
            Some(deadline) => now >= deadline,
            None => true,
        }
    }

    /// Remaining cooldown at `now`, or `Duration::ZERO` when ready.
    ///
    /// Why: the activity log surfaces "next attempt allowed in Ns" so the
    /// operator can see they need to wait rather than wondering why `[d]` did
    /// nothing.
    /// What: returns `deadline - now` when a cooldown is active, else zero.
    /// Test: `dream_backoff_remaining_reports_window`.
    pub fn remaining(&self, now: Instant) -> Duration {
        self.next_allowed_at
            .and_then(|d| d.checked_duration_since(now))
            .unwrap_or(Duration::ZERO)
    }

    /// Reset the gate after a successful dream cycle.
    ///
    /// Why: a single success means the daemon is healthy again; the next
    /// failure should be loud and the backoff should restart from the initial
    /// window.
    /// What: clears `next_allowed_at`, zeroes `consecutive_failures`, and
    /// flips `first_failure_logged` back to `false`.
    /// Test: `dream_backoff_resets_on_success`.
    pub fn record_success(&mut self) {
        self.next_allowed_at = None;
        self.consecutive_failures = 0;
        self.first_failure_logged = false;
    }

    /// Record a failure observed at `now` and return whether to log it loudly.
    ///
    /// Why: the first failure of a down-period is informative; the 50th in a
    /// row is just noise. The TUI calls this once per failed attempt and only
    /// pushes a `dream failed:` line when the return is `true`.
    /// What: increments the failure counter, computes the next cooldown as
    /// [`DREAM_BACKOFF_INITIAL`] doubled `consecutive_failures - 1` times and
    /// clamped to [`DREAM_BACKOFF_MAX`], stores `now + delay` as the next
    /// allowed instant, and returns `true` exactly when this is the first
    /// failure in the current down-period.
    /// Test: `dream_backoff_doubles_then_caps`, `dream_backoff_logs_only_first`.
    pub fn record_failure(&mut self, now: Instant) -> bool {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let delay = backoff_delay(self.consecutive_failures);
        self.next_allowed_at = Some(now + delay);
        let should_log = !self.first_failure_logged;
        self.first_failure_logged = true;
        should_log
    }

    /// The number of consecutive failures recorded since the last success.
    ///
    /// Why: tests assert the counter advances and resets correctly.
    /// What: returns the running counter.
    /// Test: `dream_backoff_doubles_then_caps`.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }
}

/// Compute the backoff delay for the `n`-th consecutive failure (`n ≥ 1`).
///
/// Why: extracted so the doubling-and-cap math is unit-testable without an
/// [`Instant`].
/// What: returns `DREAM_BACKOFF_INITIAL * 2^(n-1)` clamped to
/// [`DREAM_BACKOFF_MAX`]. `n = 0` is treated as 1.
/// Test: `dream_backoff_delay_doubles_and_caps`.
pub(crate) fn backoff_delay(n: u32) -> Duration {
    let shift = n.saturating_sub(1).min(20); // cap exponent before overflow
    let multiplier: u64 = 1u64 << shift;
    let secs = DREAM_BACKOFF_INITIAL
        .as_secs()
        .saturating_mul(multiplier)
        .min(DREAM_BACKOFF_MAX.as_secs());
    Duration::from_secs(secs)
}

/// Paged drawer list rendered in the ACTIVITY panel when a palace is selected.
///
/// Why: issue #184 — operators want to see the actual drawers in a palace
/// (id, creation timestamp, creator tag, memory count) rather than just the
/// streamed event log. Keeping the page slice + paging cursor + scope id +
/// loading flag in a small struct makes the renderer pure and the event-loop
/// fetch trigger easy to test.
/// What: `palace_id` records which palace this slice belongs to (so a quick
/// palace switch doesn't render stale rows), `drawers` holds the latest page,
/// `offset` is the page anchor (advanced by `←`/`→`), `loading` flips while
/// a fetch is in flight, and `last_error` captures the most recent fetch
/// error so the panel can surface it.
/// Test: `drawer_state_*` unit tests plus the renderer smoke tests.
#[derive(Debug, Clone, Default)]
pub struct DrawerListState {
    /// The palace id this page belongs to (`None` when no palace is scoped,
    /// e.g. when "All palaces" is selected).
    pub palace_id: Option<String>,
    /// The current page of drawers, newest first.
    pub drawers: Vec<DrawerInfo>,
    /// Page anchor — the number of drawers skipped before this page.
    pub offset: usize,
    /// Whether a fetch is currently in flight; the renderer surfaces this so
    /// the operator sees the panel reacting to a palace switch.
    pub loading: bool,
    /// Most recent fetch error, or `None` when the last fetch succeeded.
    pub last_error: Option<String>,
}

impl DrawerListState {
    /// Build an empty state — no palace, no drawers, page 0.
    ///
    /// Why: every fresh [`MemoryTuiState`] starts with the activity panel in
    /// the "no palace selected" state.
    /// What: returns the default.
    /// Test: `drawer_state_default_page_size`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset the page slice and anchor for a new palace selection.
    ///
    /// Why: switching palaces (or back to "All") must drop stale rows so the
    /// renderer doesn't show another palace's drawers between the selection
    /// click and the first fetch completion.
    /// What: clears `drawers`, sets `offset = 0`, sets `palace_id = scope`,
    /// records `loading = true`, and clears any previous error.
    /// Test: `drawer_state_reset_on_palace_change`.
    pub fn reset_for(&mut self, scope: Option<String>) {
        self.palace_id = scope;
        self.drawers.clear();
        self.offset = 0;
        self.loading = true;
        self.last_error = None;
    }

    /// Move to the next page; saturating at the current page when the daemon
    /// returned fewer than [`DRAWER_PAGE_SIZE`] rows (signalling end-of-list).
    ///
    /// Why: `→` navigates forward in the drawer list; without an end-of-list
    /// guard the operator could page past the last drawer into empty pages.
    /// What: increments `offset` by [`DRAWER_PAGE_SIZE`] only when the
    /// current page is full; flips `loading = true` and clears the error so
    /// the next fetch trigger handles the new anchor.
    /// Test: `drawer_state_pagination`.
    pub fn next_page(&mut self) {
        if self.drawers.len() >= DRAWER_PAGE_SIZE {
            self.offset = self.offset.saturating_add(DRAWER_PAGE_SIZE);
            self.loading = true;
            self.last_error = None;
        }
    }

    /// Move to the previous page; saturating at page 0.
    ///
    /// Why: `←` navigates backward in the drawer list.
    /// What: decrements `offset` by [`DRAWER_PAGE_SIZE`] (never below zero);
    /// flips `loading = true` when the anchor actually changed.
    /// Test: `drawer_state_pagination`.
    pub fn prev_page(&mut self) {
        if self.offset == 0 {
            return;
        }
        self.offset = self.offset.saturating_sub(DRAWER_PAGE_SIZE);
        self.loading = true;
        self.last_error = None;
    }

    /// The current page number (zero-indexed) for display.
    ///
    /// Why: the panel title surfaces `page N` so the operator knows where
    /// they are in the list.
    /// What: returns `offset / DRAWER_PAGE_SIZE`.
    /// Test: `drawer_state_pagination`.
    pub fn page(&self) -> usize {
        self.offset / DRAWER_PAGE_SIZE.max(1)
    }
}

/// All mutable state the memory UI renders and mutates.
///
/// Why: the event loop polls the daemon, streams `/sse` events, and handles
/// input — keeping every piece of state in one struct keeps the loop terse and
/// the rendering a pure function of this snapshot.
/// What: the daemon URL and status, the aggregate stats, the palace list and
/// selection cursor, the scroll offset of the palace panel, the bounded
/// activity log, the query buffer, the focused zone, and the help flag. The
/// selection cursor addresses a list whose first row is the synthetic "All
/// palaces" entry, so cursor `0` means "All" and cursor `n` (n ≥ 1) means
/// `palaces[n - 1]`.
/// Test: `test_selected_clamp`, `test_toggle_focus`, `test_palace_row_display`,
/// `test_all_selector`, `test_scroll_offset`.
#[derive(Debug, Clone)]
pub struct MemoryTuiState {
    /// The trusty-memory daemon base URL being monitored.
    pub base_url: String,
    /// The daemon's current liveness state.
    pub daemon_status: DaemonStatus,
    /// The latest aggregate stats, or `None` before the first poll.
    pub status: Option<MemoryData>,
    /// One row per palace.
    pub palaces: Vec<PalaceRow>,
    /// Cursor into the palace list, where row `0` is the "All palaces" entry
    /// and row `n` (n ≥ 1) selects `palaces[n - 1]`.
    pub selected: usize,
    /// Index of the first row drawn in the PALACES panel — the scroll offset
    /// that keeps [`Self::selected`] on screen when the list overflows.
    pub scroll_offset: usize,
    /// Bounded, timestamped log of dream / drawer / recall activity.
    pub log: ActivityLog,
    /// The in-progress recall query buffer.
    pub input: String,
    /// Which zone currently holds keyboard focus.
    pub focus: MemoryFocus,
    /// Whether the help overlay is visible (toggled with `?`).
    pub show_help: bool,
    /// Case-insensitive filter applied to palace name / project; empty disables.
    pub filter: String,
    /// Whether the inline filter bar is focused (captures typed chars).
    pub filter_active: bool,
    /// Current palace-list sort order.
    pub sort_key: ThreeWaySortKey,
    /// Whether the palace list is grouped by inferred project.
    pub group_by_project: bool,
    /// Exponential-backoff gate that throttles repeated dream-cycle attempts
    /// while the daemon is unreachable.
    pub dream_backoff: DreamBackoff,
    /// Paged drawer list for the ACTIVITY panel when a single palace is
    /// selected. The "All palaces" row leaves [`DrawerListState::palace_id`]
    /// set to `None` and the panel falls back to the aggregate event log.
    pub drawer_list: DrawerListState,
    /// Cursor into the current drawer page (issue #215). Indexes
    /// [`DrawerListState::drawers`] when the drawer pane has focus; reset to
    /// 0 on every page or palace change.
    pub drawer_cursor: usize,
    /// Whether the drawer-detail modal is open (issue #215). The render path
    /// floats the modal over the rest of the UI when `true`.
    pub drawer_detail_open: bool,
    /// Index into [`Self::drawer_detail_memories`] identifying which drawer
    /// the modal renders. Recorded when `Enter` opens the modal; used by the
    /// renderer to highlight the active memory.
    pub drawer_detail_idx: usize,
    /// The full set of memories returned by `fetch_drawer_detail` for the
    /// currently-open modal (issue #215). Empty until the fetch completes.
    pub drawer_detail_memories: Vec<MemoryDetail>,
    /// Vertical scroll offset (in lines) inside the modal content.
    pub drawer_detail_scroll: usize,
    /// Whether a `fetch_drawer_detail` request is currently in flight. The
    /// modal renders `Loading…` while this is `true`.
    pub drawer_detail_loading: bool,
}

impl MemoryTuiState {
    /// Build a fresh memory UI state targeting `base_url`.
    ///
    /// Why: the event loop seeds the state at startup before the first poll.
    /// What: stores the URL, sets the daemon `Connecting`, and starts with no
    /// stats, an empty palace list, empty log, empty query, and list focus.
    /// Test: `test_new_state_defaults`.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            daemon_status: DaemonStatus::Connecting,
            status: None,
            palaces: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            log: ActivityLog::new(),
            input: String::new(),
            focus: MemoryFocus::List,
            show_help: false,
            filter: String::new(),
            filter_active: false,
            sort_key: ThreeWaySortKey::default(),
            group_by_project: false,
            dream_backoff: DreamBackoff::new(),
            drawer_list: DrawerListState::new(),
            drawer_cursor: 0,
            drawer_detail_open: false,
            drawer_detail_idx: 0,
            drawer_detail_memories: Vec::new(),
            drawer_detail_scroll: 0,
            drawer_detail_loading: false,
        }
    }

    /// Legacy two-way focus toggle (issue #215 keeps the API for callers /
    /// tests that don't know about the new drawer pane).
    ///
    /// Why: a handful of callers (and the legacy `test_toggle_focus` test)
    /// expect `Tab` to bounce between the list and the recall bar; the new
    /// three-way cycle uses [`Self::cycle_focus`] instead.
    /// What: flips [`Self::focus`] via [`MemoryFocus::toggled`].
    /// Test: `test_toggle_focus`.
    pub fn toggle_focus(&mut self) {
        self.focus = self.focus.toggled();
    }

    /// Cycle keyboard focus through every focusable zone (issue #215).
    ///
    /// Why: `[Tab]` walks `List → DrawerPane → Input → List` so the operator
    /// can reach the drawer pane without a mouse.
    /// What: advances [`Self::focus`] via [`MemoryFocus::next`]; when the new
    /// focus is `List`, also clears the drawer-pane cursor so a re-entry
    /// starts at the top of the list.
    /// Test: `test_focus_tab_cycle`.
    pub fn cycle_focus(&mut self) {
        self.focus = self.focus.next();
        if self.focus == MemoryFocus::List {
            self.drawer_cursor = 0;
        }
    }

    /// Move the drawer cursor up one row, saturating at the top.
    ///
    /// Why: `↑` in the drawer pane walks the visible drawer list.
    /// What: decrements [`Self::drawer_cursor`], never below zero.
    /// Test: `test_drawer_cursor_clamp`.
    pub fn drawer_cursor_up(&mut self) {
        self.drawer_cursor = self.drawer_cursor.saturating_sub(1);
    }

    /// Move the drawer cursor down one row, clamped to the last drawer.
    ///
    /// Why: `↓` in the drawer pane walks the visible drawer list.
    /// What: increments [`Self::drawer_cursor`] but never past the last
    /// drawer in [`DrawerListState::drawers`].
    /// Test: `test_drawer_cursor_clamp`.
    pub fn drawer_cursor_down(&mut self) {
        let len = self.drawer_list.drawers.len();
        if len == 0 {
            self.drawer_cursor = 0;
            return;
        }
        if self.drawer_cursor + 1 < len {
            self.drawer_cursor += 1;
        }
    }

    /// Clamp the drawer cursor to the current drawer page length.
    ///
    /// Why: a page refresh can shrink the drawer list, leaving the cursor
    /// past the end; this keeps the cursor valid before rendering.
    /// What: caps [`Self::drawer_cursor`] at `len - 1` (or 0 when the page
    /// is empty).
    /// Test: `test_drawer_cursor_clamp`.
    pub fn clamp_drawer_cursor(&mut self) {
        let len = self.drawer_list.drawers.len();
        if len == 0 {
            self.drawer_cursor = 0;
        } else if self.drawer_cursor >= len {
            self.drawer_cursor = len - 1;
        }
    }

    /// Close the drawer-detail modal and clear its transient state.
    ///
    /// Why: `Esc`/`q` while the modal is open should drop back to the drawer
    /// pane without leaving stale `drawer_detail_memories` (which would
    /// flash on a re-open before the fetch completes).
    /// What: flips `drawer_detail_open` to `false`, clears the memories
    /// vector and scroll, and resets the loading flag.
    /// Test: `test_drawer_detail_modal_lifecycle`.
    pub fn close_drawer_detail(&mut self) {
        self.drawer_detail_open = false;
        self.drawer_detail_memories.clear();
        self.drawer_detail_scroll = 0;
        self.drawer_detail_loading = false;
    }

    /// Move the palace selection up one row, saturating at the top.
    ///
    /// Why: `↑` navigates the PALACES list when it has focus.
    /// What: decrements [`Self::selected`], never below zero.
    /// Test: `test_selected_clamp`.
    pub fn select_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the palace selection down one row, clamped to the last palace.
    ///
    /// Why: `↓` navigates the PALACES list when it has focus.
    /// What: increments [`Self::selected`] but never past the last row. The
    /// list has `palaces.len() + 1` rows (row 0 is "All palaces").
    /// Test: `test_selected_clamp`.
    pub fn select_down(&mut self) {
        if self.selected < self.last_row() {
            self.selected += 1;
        }
    }

    /// The index of the last selectable row.
    ///
    /// Why: the list always carries the synthetic "All" row, so the last valid
    /// cursor is `palaces.len()` (not `palaces.len() - 1`).
    /// What: returns `palaces.len()` — row 0 is "All", rows `1..=len` are the
    /// individual palaces.
    /// Test: `test_selected_clamp`.
    pub fn last_row(&self) -> usize {
        self.palaces.len()
    }

    /// Clamp the selection cursor to the current palace count.
    ///
    /// Why: a poll can shrink the palace list leaving the cursor past the end;
    /// this keeps it valid before rendering.
    /// What: caps [`Self::selected`] at `palaces.len()` (the "All" row plus one
    /// row per palace).
    /// Test: `test_selected_clamp`.
    pub fn clamp_selection(&mut self) {
        if self.selected > self.last_row() {
            self.selected = self.last_row();
        }
    }

    /// Recompute the scroll offset so the selected row fits a `visible` window.
    ///
    /// Why: the PALACES panel is a fixed-height viewport; when the list has
    /// more rows than fit, the panel must scroll so [`Self::selected`] is never
    /// drawn off-screen — otherwise `↑`/`↓` appear to do nothing past the edge.
    /// What: given the panel's visible row count, shifts [`Self::scroll_offset`]
    /// down when the cursor falls below the window and up when it rises above
    /// it, leaving it untouched while the cursor is already in view. A zero
    /// `visible` is treated as one row so the offset always tracks the cursor.
    /// Test: `test_scroll_offset`.
    pub fn sync_scroll(&mut self, visible: usize) {
        let cursor = self.selected;
        self.sync_scroll_to(cursor, visible);
    }

    /// Recompute the scroll offset for an arbitrary cursor row.
    ///
    /// Why: when filtering, sorting, or grouping reorders the rendered rows,
    /// `Self::selected` (an index into the original `palaces` array) no
    /// longer matches the row's on-screen position. The renderer must pass
    /// in the *visible* row index so the viewport scrolls to the row the
    /// user actually sees as selected.
    /// What: identical scroll math to [`Self::sync_scroll`] but anchored on
    /// the supplied `cursor_row` instead of `self.selected`.
    /// Test: `test_sync_scroll_to_follows_sorted_order`.
    pub fn sync_scroll_to(&mut self, cursor_row: usize, visible: usize) {
        let window = visible.max(1);
        if cursor_row >= self.scroll_offset + window {
            self.scroll_offset = cursor_row + 1 - window;
        } else if cursor_row < self.scroll_offset {
            self.scroll_offset = cursor_row;
        }
    }

    /// Whether the "All palaces" entry is currently selected.
    ///
    /// Why: when "All" is selected the UI fans recalls out across every palace
    /// and aggregates the activity feed and statistics.
    /// What: returns `true` exactly when the cursor is on row 0.
    /// Test: `test_all_selector`.
    pub fn is_all_selected(&self) -> bool {
        self.selected == 0
    }

    /// The id of the currently selected single palace, if any.
    ///
    /// Why: `[Enter]` recalls and the log labels the selected palace; neither
    /// applies to a single palace when "All" is selected.
    /// What: returns `Some(id)` for the palace at cursor row `n ≥ 1`, or `None`
    /// when "All" is selected or the palace list is empty.
    /// Test: `test_selected_id`.
    pub fn selected_id(&self) -> Option<&str> {
        if self.selected == 0 {
            return None;
        }
        self.palaces.get(self.selected - 1).map(|p| p.id.as_str())
    }

    /// Clamp the selection to the currently visible (filtered + sorted) list.
    ///
    /// Why: when the filter changes the selected palace may no longer appear in
    /// the visible subset, so arrow navigation would jump unpredictably; this
    /// drops the cursor back to "All" (row 0) in that case so navigation always
    /// starts from a visible row.
    /// What: if `selected` is non-zero and the corresponding palace id is not in
    /// the visible id list, resets `selected` to 0.
    /// Test: `test_clamp_to_visible`.
    pub fn clamp_to_visible(&mut self) {
        if self.selected == 0 {
            return;
        }
        let Some(current_id) = self.palaces.get(self.selected - 1).map(|p| p.id.clone()) else {
            self.selected = 0;
            return;
        };
        let ids = crate::monitor::memory_tui::view::visible_palace_ids(self);
        if !ids.iter().any(|id| id == &current_id) {
            self.selected = 0;
        }
    }

    /// The scope filter for the activity feed and statistics panels.
    ///
    /// Why: the right-hand panels render the selected palace's events / stats,
    /// or every palace's when "All" is selected; this folds the cursor into the
    /// `Option<&str>` filter [`ActivityLog::tail_scoped`] expects.
    /// What: returns `None` when "All" is selected (un-filtered) or `Some(id)`
    /// for the selected single palace.
    /// Test: `test_all_selector`.
    pub fn scope_filter(&self) -> Option<&str> {
        self.selected_id()
    }
}
