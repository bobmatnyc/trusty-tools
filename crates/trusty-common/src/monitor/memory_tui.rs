//! Service-specific terminal UI for the trusty-memory daemon.
//!
//! Why: operators of the trusty-memory daemon want a focused terminal surface
//! — a palace list, a streaming activity log of dream / drawer / recall
//! events, and a recall query bar — rather than the generic two-daemon
//! dashboard. Living in `trusty-common` behind the `monitor-tui` feature keeps
//! the pure state / rendering testable (issue #34).
//! What: a ratatui app with a 3-zone layout (title bar, PALACES + right-hand
//! split, RECALL input bar). The PALACES list always leads with an "All
//! palaces" entry that fans recalls out across every palace; the right side is
//! split vertically into an ACTIVITY feed (top) and a STATISTICS panel
//! (bottom), both scoped to the selected palace — or aggregated when "All" is
//! selected. It polls the daemon every 2 seconds, subscribes to the `/sse`
//! event stream on startup, runs cross-palace recalls from the input bar on
//! `[Enter]`, and triggers a dream cycle on `[d]`.
//! Test: `cargo test -p trusty-common --features monitor-tui` covers the pure
//! state, the "All" selector, palace row formatting, the activity / statistics
//! line builders, and dream-event logging; `trusty-memory monitor tui`
//! launches the live UI.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame, Terminal,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph, Wrap,
    },
};
use tokio::sync::mpsc;

use crate::monitor::dashboard::{MemoryData, PalaceRow, format_count};
use crate::monitor::memory_client::{
    DrawerInfo, MemoryClient, MemoryDetail, MemoryEvent, RecallHit, resolve_memory_url,
};
use crate::monitor::tui_common::{
    self, ThreeWaySortKey, enter_tui, leave_tui, left_panel_width, panel_block, truncate,
};
use crate::monitor::utils::{ActivityLog, DaemonStatus};

/// Data-refresh interval: how often the daemon is polled.
const REFRESH_INTERVAL: Duration = Duration::from_millis(2000);

/// Input-poll interval: how often the keyboard is checked.
const INPUT_POLL: Duration = Duration::from_millis(50);

/// Number of results requested per recall query.
const RECALL_TOP_K: usize = 5;

/// Initial backoff after a single dream-cycle failure.
///
/// Why: when the trusty-memory daemon is down (or the lock file points at a
/// stale port), pressing `[d]` would previously fire one request per keystroke
/// and flood the activity log with `dream failed` lines at ~1-2 s cadence. A
/// short initial cooldown plus exponential growth keeps the log readable while
/// still letting the operator retry quickly once the daemon comes back.
/// What: 5 seconds — the first failure blocks further attempts for 5 s.
const DREAM_BACKOFF_INITIAL: Duration = Duration::from_secs(5);

/// Ceiling on the dream-cycle retry backoff.
///
/// Why: the backoff doubles after each consecutive failure; without a ceiling
/// it would grow unbounded. Five minutes is long enough to be unobtrusive but
/// short enough that recovery is detected within one cycle.
/// What: 5 minutes (300 s) — caps the doubled backoff.
const DREAM_BACKOFF_MAX: Duration = Duration::from_secs(300);

/// Crate version, surfaced in the title bar.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// One-line key hint shown along the bottom of the UI.
///
/// Why (issue #215): `Tab` cycles `List → DrawerPane → Input → List`, and the
/// drawer-pane zone adds `Enter` to open the detail modal. The hint surfaces
/// both flows so the operator doesn't have to discover the modal from the
/// help overlay.
pub const KEY_HINT: &str = "[Tab] focus  [↑↓] select  [Enter] open/recall  [d] dream  [/] filter  [s] sort  [g] group  [←→] page  [q] quit  [?] help";

/// Default page size for the ACTIVITY drawer list.
///
/// Why: the activity panel is narrow and only renders a handful of rows; 20
/// drawers per page balances "feels paged" with "stays inside one screen
/// scroll" for typical terminal heights.
/// What: 20 drawers per fetch.
/// Test: `drawer_state_default_page_size`.
pub const DRAWER_PAGE_SIZE: usize = 20;

/// Maximum number of timestamp / creator characters surfaced per drawer row.
///
/// Why: the ACTIVITY panel is the right-hand column of the TUI; long creator
/// tags or full RFC-3339 timestamps would overflow when the terminal is
/// narrow. Truncating each field independently keeps the row alignment
/// predictable at all widths.
/// What: timestamp shown as `MM-DD HH:MM` (11 chars), creator label
/// truncated to 24 chars with the shared truncate helper.
const DRAWER_CREATOR_WIDTH: usize = 24;

/// Domain-specific labels for the memory TUI's three sort orders.
///
/// Why: the renderer surfaces the current sort key in the panel title; this
/// array maps the shared [`ThreeWaySortKey`] variants to memory-domain text
/// (the third variant reads as "Vectors" here, "Chunks" in search).
/// What: `["Activity", "Name", "Vectors"]`.
/// Test: covered indirectly by `test_palace_sort_key_cycle` via [`sort_label`].
const SORT_LABELS: &[&str; 3] = &["Activity", "Name", "Vectors"];

/// Sort key cycled by `[s]` in the palace list.
///
/// Why: kept as a re-export alias so external callers and tests that reference
/// `PalaceSortKey` continue to compile after the type was consolidated into
/// the shared [`ThreeWaySortKey`].
/// What: type alias for [`ThreeWaySortKey`].
/// Test: `test_palace_sort_key_cycle`.
pub type PalaceSortKey = ThreeWaySortKey;

/// Memory-domain label for the current sort key.
///
/// Why: the renderer needs `"Activity"` / `"Name"` / `"Vectors"`; the shared
/// enum is domain-agnostic so we map it through [`SORT_LABELS`].
/// What: delegates to [`ThreeWaySortKey::label`] with the memory labels.
/// Test: `test_palace_sort_key_cycle`.
pub fn sort_label(key: ThreeWaySortKey) -> &'static str {
    key.label(SORT_LABELS)
}

/// Label for the synthetic "All palaces" entry at the top of the list.
///
/// Why: selecting it fans recalls / stats out across every palace; a single
/// constant keeps the label consistent between the list and the panel titles.
/// What: the display text of the palace list's first row.
/// Test: `test_palace_lines` asserts this is the first row.
pub const ALL_LABEL: &str = "All palaces";

/// Braille spinner glyphs used for the "Indexing" state (rotating wave).
///
/// Why: a recognisable spinner prefix gives the operator a glance-cue that a
/// palace is currently absorbing writes, without polling for an explicit state.
/// What: ten-frame braille cycle, indexed by a wall-clock tick.
/// Test: `test_palace_activity_state` (frames sampled deterministically).
const INDEXING_SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Braille spinner glyphs used for the "Dreaming" state (rotating block).
///
/// Why: a heavier, distinct cycle separates an in-progress dream/compaction
/// from the lighter indexing spinner at a glance.
/// What: eight-frame braille cycle.
/// Test: `test_palace_activity_state`.
const DREAMING_SPINNER: [char; 8] = ['⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'];

/// A palace's current activity state, surfaced as a coloured spinner prefix.
///
/// Why: operators want to see at a glance whether each palace is idle, taking
/// writes, recently active, dreaming, or unhealthy. A typed enum makes the
/// renderer's colour + glyph mapping exhaustive.
/// What: five mutually-exclusive states. The mapping from the underlying
/// `PalaceRow` data lives in [`palace_activity_state`].
/// Test: `test_palace_activity_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PalaceActivity {
    /// Nothing recent — no spinner, default style.
    Idle,
    /// `last_write_at` within the last 10 seconds — rotating indexing spinner.
    Indexing,
    /// `last_write_at` within the last 60 seconds — static `⠿` in cyan.
    Active,
    /// A dream/compaction cycle is currently running — rotating block spinner.
    Dreaming,
    /// The palace reported an unhealthy / error state — red `✗`.
    Error,
}

impl PalaceActivity {
    /// Resolve the rendered prefix glyph for this state at wall-clock `tick`.
    ///
    /// Why: spinners must cycle without an explicit app tick; the wall-clock
    /// driver lets every palace's frame advance independently of polls.
    /// What: returns a single rendered character: `' '` for Idle, the indexed
    /// frame from [`INDEXING_SPINNER`] / [`DREAMING_SPINNER`] for the rotating
    /// states, `'⠿'` for Active, and `'✗'` for Error.
    /// Test: `test_palace_activity_state`.
    pub fn prefix(self, tick: usize) -> char {
        match self {
            PalaceActivity::Idle => ' ',
            PalaceActivity::Indexing => INDEXING_SPINNER[tick % INDEXING_SPINNER.len()],
            PalaceActivity::Active => '⠿',
            PalaceActivity::Dreaming => DREAMING_SPINNER[tick % DREAMING_SPINNER.len()],
            PalaceActivity::Error => '✗',
        }
    }

    /// Resolve the foreground colour for this state.
    ///
    /// Why: colour reinforces the glyph — yellow for indexing, cyan for
    /// active, magenta for dreaming, red for error, default for idle.
    /// What: returns `None` for Idle (default terminal foreground) or
    /// `Some(Color)` for the four signalling states.
    /// Test: `test_palace_activity_state`.
    pub fn color(self) -> Option<Color> {
        match self {
            PalaceActivity::Idle => None,
            PalaceActivity::Indexing => Some(Color::Yellow),
            PalaceActivity::Active => Some(Color::Cyan),
            PalaceActivity::Dreaming => Some(Color::Magenta),
            PalaceActivity::Error => Some(Color::Red),
        }
    }
}

/// Wall-clock spinner tick, driven by the system clock at 10 Hz.
///
/// Why: spinners must animate even when no app event fires; a wall-clock tick
/// keeps every frame in motion without a separate timer.
/// What: returns `now.duration_since(UNIX_EPOCH).as_millis() / 100`, cast to
/// `usize` (saturating at zero on clock skew).
/// Test: `test_spinner_tick_monotonic` only sanity-checks the call surface;
/// downstream tests pass an explicit `tick` to keep them deterministic.
pub fn spinner_tick() -> usize {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_millis() / 100) as usize)
        .unwrap_or(0)
}

/// Derive a palace's current [`PalaceActivity`] from its wire fields.
///
/// Why: the row builder and the STATISTICS panel both need the same mapping.
/// Centralising it keeps the rendering and the detail panel in sync, and the
/// 10-second / 60-second cut-offs documented in one place.
/// What: `is_compacting → Dreaming`; otherwise the elapsed time since
/// `last_write_at` decides Indexing (< 10s), Active (< 60s), or Idle. Error
/// is reserved for a future health field on the wire — never returned today.
/// Test: `test_palace_activity_state`.
pub fn palace_activity_state(
    palace: &PalaceRow,
    now: chrono::DateTime<chrono::Utc>,
) -> PalaceActivity {
    if palace.is_compacting {
        return PalaceActivity::Dreaming;
    }
    match palace.last_write_at {
        Some(ts) => {
            let delta = now.signed_duration_since(ts);
            // Negative deltas (clock skew) are treated as fresh writes.
            let secs = delta.num_seconds();
            if secs < 10 {
                PalaceActivity::Indexing
            } else if secs < 60 {
                PalaceActivity::Active
            } else {
                PalaceActivity::Idle
            }
        }
        None => PalaceActivity::Idle,
    }
}

/// Whether to keep a palace in the visible list.
///
/// Why: palaces with no vectors, no KG triples, AND no drawers carry no
/// user-visible content and would only clutter the list. A palace with drawers
/// but no vectors is one whose memories have been stored but not yet embedded
/// (e.g. the embedding model has not run yet); hiding it causes confusion
/// because the palace clearly exists and has written content. Including
/// `drawer_count > 0` in the gate keeps such palaces visible in the TUI.
/// What: returns `true` when any of `vector_count`, `kg_triple_count`, or
/// `drawer_count` is non-zero; returns `false` only when all three are zero.
/// Test: `test_filter_empty_palaces`.
pub fn palace_has_content(palace: &PalaceRow) -> bool {
    palace.vector_count > 0 || palace.kg_triple_count > 0 || palace.drawer_count > 0
}

/// Render a `chrono::Duration` as a compact human-readable relative time.
///
/// Why: the detail panel's "Last write" line reads more naturally as "just
/// now" / "2m ago" / "5h ago" than as a raw timestamp; the absolute timestamp
/// is shown alongside for precision.
/// What: returns `"just now"` for < 5s; `"<n>s ago"` for < 60s;
/// `"<n>m ago"` for < 60min; `"<n>h ago"` for < 24h; `"<n>d ago"` otherwise.
/// Negative deltas are clamped to "just now".
/// Test: `test_format_relative_time`.
pub fn format_relative_time(
    now: chrono::DateTime<chrono::Utc>,
    ts: chrono::DateTime<chrono::Utc>,
) -> String {
    let secs = now.signed_duration_since(ts).num_seconds();
    if secs < 5 {
        return "just now".to_string();
    }
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    format!("{days}d ago")
}

/// Human-readable label for a [`PalaceActivity`] state.
///
/// Why: the STATISTICS panel surfaces the current state in plain text next to
/// the spinner; sharing the mapping keeps the row prefix and the detail panel
/// label in lockstep.
/// What: returns `"Idle"`, `"Indexing"`, `"Active"`, `"Dreaming"`, or
/// `"Error"`.
/// Test: covered indirectly by `test_stats_graph_section`.
pub fn activity_label(activity: PalaceActivity) -> &'static str {
    match activity {
        PalaceActivity::Idle => "Idle",
        PalaceActivity::Indexing => "Indexing",
        PalaceActivity::Active => "Active",
        PalaceActivity::Dreaming => "Dreaming",
        PalaceActivity::Error => "Error",
    }
}

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
    next_allowed_at: Option<Instant>,
    /// Number of consecutive failures observed since the last success.
    consecutive_failures: u32,
    /// `true` once the first failure of the current down-period has been
    /// surfaced in the activity log; flips back to `false` on success so the
    /// next down-period's first failure is reported again.
    first_failure_logged: bool,
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
fn backoff_delay(n: u32) -> Duration {
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
    fn last_row(&self) -> usize {
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
        let ids = visible_palace_ids(self);
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

/// Run the trusty-memory monitor TUI.
///
/// Why: the single entry point the `monitor tui` subcommand of `trusty-memory`
/// calls.
/// What: resolves the daemon URL from the service lock file and delegates to
/// [`run_with_url`].
/// Test: the pure pieces are unit-tested; this thin glue is exercised by
/// launching the UI.
pub async fn run() -> anyhow::Result<()> {
    run_with_url(resolve_memory_url()).await
}

/// Run the memory TUI against an explicit daemon URL.
///
/// Why: separated from [`run`] so a future CLI flag can override the resolved
/// address, and so terminal setup/teardown lives in one place.
/// What: builds the client and state, enters raw mode + the alternate screen,
/// runs [`run_loop`], and unconditionally restores the terminal even on error.
/// Test: terminal glue is exercised by launching the UI.
pub async fn run_with_url(base_url: String) -> anyhow::Result<()> {
    let mut client = MemoryClient::new(base_url.clone());
    let mut state = MemoryTuiState::new(base_url);

    let mut terminal = enter_tui()?;
    let result = run_loop(&mut terminal, &mut state, &mut client).await;
    leave_tui(&mut terminal)?;
    result
}

/// Poll the trusty-memory daemon and fold the result into `state`.
///
/// Why: keeps the per-poll I/O out of the event loop so the loop can re-poll
/// on demand as well as on its timer.
/// What: re-resolves the URL when the daemon is offline, calls `fetch_all`, and
/// updates the status, aggregate stats, palace list, and selection clamp.
/// Test: thin I/O glue; the pure clamp is unit-tested.
async fn poll_daemon(state: &mut MemoryTuiState, client: &mut MemoryClient) {
    if !state.daemon_status.is_online() {
        let resolved = resolve_memory_url();
        if resolved != client.base_url() {
            client.set_base_url(resolved.clone());
            state.base_url = resolved;
        }
    }
    match client.fetch_all().await {
        Ok(data) => {
            state.daemon_status = DaemonStatus::Online {
                version: data.version.clone(),
                uptime_secs: 0,
            };
            state.palaces = data.palaces.clone();
            state.status = Some(data);
            state.clamp_selection();
        }
        Err(e) => {
            state.daemon_status = DaemonStatus::Offline {
                last_error: e.to_string(),
            };
        }
    }
}

/// Run a recall and append the hits to the activity log.
///
/// Why: pressing `[Enter]` in the recall bar runs a memory recall; the
/// operator sees the results inline in the ACTIVITY panel. The recall endpoint
/// is inherently cross-palace, so when a single palace is selected the hits
/// are filtered to that palace; when "All palaces" is selected every hit is
/// shown.
/// What: calls `client.recall`, then — for the "All" selection — appends a
/// daemon-wide `recall "<q>" → N results` summary plus one `palace_id`-scoped
/// `· [palace] snippet` continuation per hit. For a single palace it appends a
/// palace-scoped summary counting only that palace's hits and a continuation
/// per kept hit. An empty query is a no-op; transport errors are logged scoped
/// to the selection.
/// Test: thin I/O glue; result projection is tested in `memory_client`.
async fn run_recall(state: &mut MemoryTuiState, client: &MemoryClient) {
    let query = state.input.trim().to_string();
    if query.is_empty() {
        return;
    }
    let scope = state.selected_id().map(str::to_string);
    match client.recall(&query, RECALL_TOP_K).await {
        Ok(hits) => match &scope {
            // "All palaces": one daemon-wide summary, each hit scoped to its
            // own palace so the per-palace feed still shows it.
            None => {
                state
                    .log
                    .push(format!("recall \"{query}\" (all) → {} results", hits.len()));
                for hit in &hits {
                    let palace = if hit.palace_id.is_empty() {
                        "?"
                    } else {
                        hit.palace_id.as_str()
                    };
                    state
                        .log
                        .push_raw_scoped(palace, format!("  · [{palace}] {}", hit.snippet));
                }
            }
            // A single palace: keep only that palace's hits.
            Some(id) => {
                let kept: Vec<&RecallHit> = hits.iter().filter(|h| h.palace_id == *id).collect();
                state
                    .log
                    .push_scoped(id, format!("recall \"{query}\" → {} results", kept.len()));
                for hit in kept {
                    state
                        .log
                        .push_raw_scoped(id, format!("  · {}", hit.snippet));
                }
            }
        },
        Err(e) => match &scope {
            None => state
                .log
                .push(format!("recall \"{query}\" (all) failed: {e}")),
            Some(id) => state
                .log
                .push_scoped(id, format!("recall \"{query}\" failed: {e}")),
        },
    }
    state.input.clear();
}

/// Append a streamed `/sse` event to the activity log, scoped to its palace.
///
/// Why: the SSE task forwards [`MemoryEvent`]s through a channel; the event
/// loop drains them and this turns each into a human-readable log entry. The
/// drawer events concern one palace, so they are tagged with its id and the
/// per-palace activity feed keeps only its own events.
/// What: `DreamCompleted` records a daemon-wide header plus an indented
/// merge/prune/compact line; `DrawerAdded` / `DrawerDeleted` record a single
/// line each scoped to `palace_id`; `PalaceCreated` records a daemon-wide line
/// (the new palace has no id yet on the wire).
/// Test: `test_log_append_dream`, `test_apply_memory_event`.
pub fn apply_memory_event(state: &mut MemoryTuiState, event: MemoryEvent) {
    match event {
        MemoryEvent::DreamCompleted {
            merged,
            pruned,
            compacted,
        } => {
            state.log.push("SSE: dream_completed");
            state.log.push_raw(format!(
                "  merged: {merged}  pruned: {pruned}  compacted: {compacted}"
            ));
        }
        MemoryEvent::DrawerAdded {
            palace_id,
            drawer_count,
            content_preview,
        } => {
            // Prefer a content preview when the daemon provided one; fall
            // back to the legacy "(<count>)" format so older daemons still
            // render a useful line.
            let line = if content_preview.is_empty() {
                format!("SSE: drawer added → {palace_id} ({drawer_count})")
            } else {
                format!("SSE: drawer added → {palace_id} ({drawer_count}): \"{content_preview}\"")
            };
            state.log.push_scoped(&palace_id, line);
        }
        MemoryEvent::DrawerDeleted {
            palace_id,
            drawer_count,
        } => {
            state.log.push_scoped(
                &palace_id,
                format!("SSE: drawer deleted → {palace_id} ({drawer_count})"),
            );
        }
        MemoryEvent::PalaceCreated { name } => {
            state.log.push(format!("SSE: palace created → {name}"));
        }
    }
}

/// Fetch the drawer page for the current selection and fold the result into
/// [`MemoryTuiState::drawer_list`].
///
/// Why: the activity panel needs a live page slice for whichever palace is
/// selected; isolating the fetch keeps the event loop free of per-trigger
/// branching and makes the loading / error transitions easy to reason about.
/// What: when no single palace is selected, clears the drawer slice and
/// returns. Otherwise issues `client.list_drawers` for the stored offset and
/// either replaces `drawers` or records the error. Always flips
/// `loading = false` so the renderer drops the in-flight badge.
/// Test: thin I/O glue; pure projection is tested in `memory_client`.
async fn fetch_drawer_page(state: &mut MemoryTuiState, client: &MemoryClient) {
    let Some(palace_id) = state.selected_id().map(str::to_string) else {
        // "All palaces" or no selection — clear the drawer slice; the panel
        // falls back to the aggregate activity log.
        state.drawer_list.palace_id = None;
        state.drawer_list.drawers.clear();
        state.drawer_list.offset = 0;
        state.drawer_list.loading = false;
        state.drawer_list.last_error = None;
        return;
    };

    state.drawer_list.palace_id = Some(palace_id.clone());
    state.drawer_list.loading = true;
    match client
        .list_drawers(&palace_id, DRAWER_PAGE_SIZE, state.drawer_list.offset)
        .await
    {
        Ok(rows) => {
            state.drawer_list.drawers = rows;
            state.drawer_list.last_error = None;
        }
        Err(e) => {
            state.drawer_list.last_error = Some(e.to_string());
            state.drawer_list.drawers.clear();
        }
    }
    state.drawer_list.loading = false;
}

/// Fetch the full memory detail for the drawer-detail modal (issue #215).
///
/// Why: when the operator presses `Enter` in the drawer pane the modal must
/// open with the verbatim drawer body. The activity-panel rows only carry
/// the truncated snippet, so we re-fetch the drawer list from the daemon —
/// which serialises every drawer's full `content` — and store the result in
/// `state.drawer_detail_memories`.
/// What: when no palace is selected, leaves the modal closed and returns.
/// Otherwise issues `client.fetch_drawer_detail` for the current scope and
/// either replaces the memories or records the failure on the log. Always
/// flips `drawer_detail_loading = false` so the modal drops its in-flight
/// label.
/// Test: thin I/O glue; pure projection is tested via `parse_memory_details`.
async fn fetch_drawer_detail(state: &mut MemoryTuiState, client: &MemoryClient) {
    let Some(palace_id) = state.selected_id().map(str::to_string) else {
        // No single palace selected — close the modal so it can't render
        // stale memories from a previous scope.
        state.close_drawer_detail();
        return;
    };
    state.drawer_detail_loading = true;
    // Use a generous limit so the modal can show the entire drawer page (the
    // pane page size is 20, but the modal lets the operator scroll through
    // every memory the daemon returns).
    match client.fetch_drawer_detail(&palace_id, 50).await {
        Ok(memories) => {
            state.drawer_detail_memories = memories;
            // Clamp the selected index to the loaded set in case the page
            // shrank between key-press and fetch completion.
            if state.drawer_detail_idx >= state.drawer_detail_memories.len() {
                state.drawer_detail_idx = state.drawer_detail_memories.len().saturating_sub(1);
            }
        }
        Err(e) => {
            // Surface the error on the activity log so the operator sees why
            // the modal stayed empty. The modal itself shows a `Loading…`
            // placeholder until either a fetch succeeds or it is closed.
            state
                .log
                .push_scoped(&palace_id, format!("drawer detail fetch failed: {e}"));
            state.drawer_detail_memories.clear();
        }
    }
    state.drawer_detail_loading = false;
}

/// Build the rendered lines for the ACTIVITY panel when a palace is selected.
///
/// Why: the activity panel shows a compact one-line summary per drawer
/// (id, timestamp, creator, memory count) plus a header line summarising the
/// current page. Isolating the line builder makes the content testable
/// without a terminal backend.
/// What: returns `Vec<String>` — one row per drawer plus optional header /
/// error / placeholder lines. When the drawer slice is empty, falls back to
/// the same `(no activity yet)` placeholder the legacy panel used so the
/// "All palaces" / never-fetched paths still render cleanly.
/// Test: `drawer_panel_lines_renders_*`.
pub fn drawer_panel_lines(state: &MemoryTuiState, total_drawer_count: u64) -> Vec<String> {
    let dl = &state.drawer_list;
    if dl.palace_id.is_none() {
        return vec![];
    }
    let mut lines: Vec<String> = Vec::with_capacity(dl.drawers.len() + 2);
    let from = dl.offset + 1;
    let to = dl.offset + dl.drawers.len();
    let header = if dl.drawers.is_empty() {
        if dl.loading {
            "loading drawers…".to_string()
        } else if let Some(err) = &dl.last_error {
            format!("drawers unavailable: {err}")
        } else {
            "(no drawers yet)".to_string()
        }
    } else {
        format!(
            "drawers {}–{} of {} (page {})",
            from,
            to,
            format_count(total_drawer_count),
            dl.page() + 1,
        )
    };
    lines.push(header);
    for d in &dl.drawers {
        lines.push(format_drawer_row(d));
    }
    lines
}

/// Maximum characters retained for the trailing snippet column.
///
/// Why: issue #202 — the activity panel row layout is `<id> <ts>
/// <creator>  <snippet>`. The snippet column adds new width to an
/// already narrow panel; capping it keeps rows from wrapping on
/// reasonable terminal widths.
/// What: 60 characters with a trailing `…` from the truncate helper
/// when cut. Matches the server's `DRAWER_SNIPPET_MAX_CHARS`.
/// Test: `drawer_row_includes_snippet`.
const DRAWER_SNIPPET_WIDTH: usize = 60;

/// Format one drawer as a single compact activity-panel row.
///
/// Why: the panel is narrow; a fixed `<id> <ts> <creator>` column layout
/// keeps the rendered list scannable. Issue #202 appends an optional
/// snippet column when the daemon supplied one, giving the operator a
/// glance at the drawer body without opening it.
/// What: `<truncated-id> <MM-DD HH:MM>  <creator>` — id truncated to 8
/// chars (the leading UUID block), timestamp rendered in UTC, creator
/// truncated to [`DRAWER_CREATOR_WIDTH`] chars via the shared truncate
/// helper. When `drawer.snippet` is `Some` and non-empty, a `  <snippet>`
/// suffix is appended (truncated to [`DRAWER_SNIPPET_WIDTH`]). A drawer
/// with no timestamp shows `--`.
/// Test: `drawer_row_layout`, `drawer_row_includes_snippet`.
pub fn format_drawer_row(drawer: &DrawerInfo) -> String {
    let id = truncate(&drawer.id, 8);
    let ts = match drawer.created_at {
        Some(t) => t.format("%m-%d %H:%M").to_string(),
        None => "--         ".to_string(),
    };
    let creator = truncate(&drawer.creator, DRAWER_CREATOR_WIDTH);
    let base = format!("{id} {ts}  {creator}");
    match drawer
        .snippet
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(snippet) => format!("{base}  {}", truncate(snippet, DRAWER_SNIPPET_WIDTH)),
        None => base,
    }
}

/// The memory TUI event loop: poll, render, handle input, drain SSE events.
///
/// Why: kept separate from [`run_with_url`] so terminal setup/teardown wraps it
/// cleanly.
/// What: polls the daemon immediately and spawns the `/sse` subscription task,
/// then renders every frame while polling the keyboard every 50 ms; re-polls on
/// the 2 s timer and drains SSE events via `try_recv`. `[d]` triggers a dream
/// cycle, `[Enter]` runs a recall; `Tab`, arrows, `?`, `q`/`Esc`, and `Ctrl-C`
/// behave per [`KEY_HINT`].
/// Test: the pure pieces (state, log, rendering helpers) are unit-tested.
async fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut MemoryTuiState,
    client: &mut MemoryClient,
) -> anyhow::Result<()> {
    poll_daemon(state, client).await;
    let mut last_poll = Instant::now();

    // Subscribe to the daemon's /sse stream on a background task.
    let (sse_tx, mut sse_rx) = mpsc::channel::<MemoryEvent>(64);
    let sse_client = client.clone();
    tokio::spawn(async move {
        sse_client.sse_stream(sse_tx).await;
    });

    // Issue #184: every time the palace selection changes, refresh the
    // drawer panel. Tracking the previously-shown scope avoids re-fetching
    // on every render tick.
    let mut last_drawer_scope: Option<String> = None;

    loop {
        terminal.draw(|f| render(f, state))?;
        // `terminal.draw` requires `state` mutably (the renderer scrolls the
        // palace list); the closure reborrows it for the rest of the loop.

        // Drain any SSE events the subscription task produced since last frame.
        while let Ok(event) = sse_rx.try_recv() {
            apply_memory_event(state, event);
        }

        let key = if event::poll(INPUT_POLL)? {
            match event::read()? {
                Event::Key(key) => Some(key),
                _ => None,
            }
        } else {
            None
        };
        if let Some(key) = key
            && key.kind != KeyEventKind::Release
        {
            // Ctrl-C always quits, regardless of focus or the help overlay.
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Ok(());
            }
            if state.show_help {
                if matches!(key.code, KeyCode::Char('?') | KeyCode::Esc) {
                    state.show_help = false;
                } else if key.code == KeyCode::Char('q') {
                    return Ok(());
                }
                continue;
            }
            // Issue #215: drawer-detail modal owns the keyboard while open —
            // `Esc`/`q` close it; `↑`/`↓` scroll its body; everything else is
            // swallowed so the underlying UI never reacts under the modal.
            if state.drawer_detail_open {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => state.close_drawer_detail(),
                    KeyCode::Up => {
                        state.drawer_detail_scroll = state.drawer_detail_scroll.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        state.drawer_detail_scroll = state.drawer_detail_scroll.saturating_add(1);
                    }
                    _ => {}
                }
                continue;
            }
            match (state.focus, key.code) {
                // Filter-active bindings come first — they capture characters,
                // backspace, Esc, and Enter before the general List handlers.
                (MemoryFocus::List, KeyCode::Esc) if state.filter_active => {
                    // Keep the filter text so the user can re-activate.
                    state.filter_active = false;
                }
                (MemoryFocus::List, KeyCode::Enter) if state.filter_active => {
                    state.filter_active = false;
                }
                (MemoryFocus::List, KeyCode::Backspace) if state.filter_active => {
                    state.filter.pop();
                    state.clamp_to_visible();
                }
                (MemoryFocus::List, KeyCode::Char(c)) if state.filter_active => {
                    state.filter.push(c);
                    state.clamp_to_visible();
                }
                // Tab is a no-op while the filter is active — otherwise it
                // would steal focus away from the list and break filter input.
                (MemoryFocus::List, KeyCode::Tab) if state.filter_active => {}
                (_, KeyCode::Char('?')) => state.show_help = true,
                // Issue #215: Tab cycles through every focusable zone.
                (_, KeyCode::Tab) => state.cycle_focus(),
                // Esc on the drawer pane returns focus to the palace list
                // (with the drawer cursor cleared); on every other zone Esc
                // still quits, matching the legacy behaviour.
                (MemoryFocus::DrawerPane, KeyCode::Esc) => {
                    state.focus = MemoryFocus::List;
                    state.drawer_cursor = 0;
                }
                (_, KeyCode::Esc) => return Ok(()),
                // List-focus bindings.
                (MemoryFocus::List, KeyCode::Char('q')) => return Ok(()),
                (MemoryFocus::List, KeyCode::Up) => navigate_up_visible(state),
                (MemoryFocus::List, KeyCode::Down) => navigate_down_visible(state),
                // Drawer-page navigation in the ACTIVITY panel — only when a
                // single palace is selected. `←` previous page, `→` next.
                (MemoryFocus::List, KeyCode::Left) if state.selected_id().is_some() => {
                    state.drawer_list.prev_page();
                    fetch_drawer_page(state, client).await;
                    state.clamp_drawer_cursor();
                }
                (MemoryFocus::List, KeyCode::Right) if state.selected_id().is_some() => {
                    state.drawer_list.next_page();
                    fetch_drawer_page(state, client).await;
                    state.clamp_drawer_cursor();
                }
                (MemoryFocus::List, KeyCode::Char('/')) => {
                    state.filter_active = true;
                    state.filter.clear();
                }
                (MemoryFocus::List, KeyCode::Char('s')) => {
                    state.sort_key = state.sort_key.next();
                }
                (MemoryFocus::List, KeyCode::Char('g')) => {
                    state.group_by_project = !state.group_by_project;
                }
                (MemoryFocus::List, KeyCode::Char('d')) => {
                    let now = Instant::now();
                    if !state.dream_backoff.ready(now) {
                        let remaining = state.dream_backoff.remaining(now);
                        tracing::debug!(
                            "dream cycle suppressed by backoff: {}s remaining",
                            remaining.as_secs()
                        );
                        // Only echo the cooldown once per quiet period — log a
                        // single hint line the first time the operator hits
                        // [d] inside the window, then stay silent on repeats.
                    } else {
                        state.log.push("dream cycle triggered");
                        match client.dream_run().await {
                            Ok(stats) => {
                                state.log.push_raw(format!(
                                    "  merged: {}  pruned: {}  compacted: {}",
                                    stats.merged, stats.pruned, stats.compacted
                                ));
                                state.dream_backoff.record_success();
                            }
                            Err(e) => {
                                let should_log = state.dream_backoff.record_failure(Instant::now());
                                if should_log {
                                    let next = state.dream_backoff.remaining(Instant::now());
                                    state.log.push(format!(
                                        "dream failed: {e} (next attempt in {}s)",
                                        next.as_secs()
                                    ));
                                } else {
                                    tracing::debug!(
                                        "dream failed (suppressed, {} consecutive failures): {e}",
                                        state.dream_backoff.consecutive_failures()
                                    );
                                }
                            }
                        }
                        poll_daemon(state, client).await;
                        last_poll = Instant::now();
                    }
                }
                // DrawerPane bindings (issue #215). `↑`/`↓` move the drawer
                // cursor through the current page; `Enter` opens the detail
                // modal for the highlighted drawer; `←`/`→` continue to do
                // page navigation so the operator can step through pages
                // without switching focus back to the list.
                (MemoryFocus::DrawerPane, KeyCode::Up) => {
                    state.drawer_cursor_up();
                }
                (MemoryFocus::DrawerPane, KeyCode::Down) => {
                    state.drawer_cursor_down();
                }
                (MemoryFocus::DrawerPane, KeyCode::Left) if state.selected_id().is_some() => {
                    state.drawer_list.prev_page();
                    fetch_drawer_page(state, client).await;
                    state.clamp_drawer_cursor();
                }
                (MemoryFocus::DrawerPane, KeyCode::Right) if state.selected_id().is_some() => {
                    state.drawer_list.next_page();
                    fetch_drawer_page(state, client).await;
                    state.clamp_drawer_cursor();
                }
                (MemoryFocus::DrawerPane, KeyCode::Enter)
                    if !state.drawer_list.drawers.is_empty()
                        && state.drawer_cursor < state.drawer_list.drawers.len() =>
                {
                    state.drawer_detail_open = true;
                    state.drawer_detail_idx = state.drawer_cursor;
                    state.drawer_detail_scroll = 0;
                    state.drawer_detail_memories.clear();
                    fetch_drawer_detail(state, client).await;
                }
                (MemoryFocus::DrawerPane, KeyCode::Char('q')) => return Ok(()),
                // Input-focus bindings.
                (MemoryFocus::Input, KeyCode::Enter) => {
                    run_recall(state, client).await;
                }
                (MemoryFocus::Input, KeyCode::Backspace) => {
                    state.input.pop();
                }
                (MemoryFocus::Input, KeyCode::Char(c)) => state.input.push(c),
                _ => {}
            }
        }

        if last_poll.elapsed() >= REFRESH_INTERVAL {
            poll_daemon(state, client).await;
            // Refresh the drawer page in lock-step with the daemon poll so
            // new drawers appear in the activity panel without needing a
            // key press (issue #184: "Real-time updates when new drawers
            // are added while viewing").
            if state.selected_id().is_some() {
                fetch_drawer_page(state, client).await;
                state.clamp_drawer_cursor();
            }
            last_poll = Instant::now();
        }

        // Detect a palace-selection change after key handling and refresh
        // the drawer slice. Comparing the stored scope means we only fire
        // the fetch on real changes, not on every render tick.
        let current_scope = state.selected_id().map(str::to_string);
        if current_scope != last_drawer_scope {
            state.drawer_list.reset_for(current_scope.clone());
            fetch_drawer_page(state, client).await;
            // Issue #215: palace change resets the drawer cursor; the modal
            // (if open) should also close since its memories belong to the
            // previous scope.
            state.drawer_cursor = 0;
            state.close_drawer_detail();
            last_drawer_scope = current_scope;
        }
    }
}

/// The body text for the help overlay, one binding per line.
///
/// Why: kept separate so a test can assert every binding is documented.
/// What: returns the multi-line help string.
/// Test: `test_help_text_lists_bindings`.
pub fn help_text() -> String {
    [
        "  Tab     cycle focus: palace list → drawer pane → recall bar",
        "  ↑ / ↓   move the active selection (list, drawers, or modal scroll)",
        "  ← / →   page through drawers in the ACTIVITY panel",
        "  Enter   in DrawerPane: open the selected drawer's detail modal",
        "          in Input: run a recall query",
        "  All     the top list row fans recalls / stats across every palace",
        "  /       activate the inline palace filter (Esc / Enter close)",
        "  s       cycle palace sort: Activity → Name → Vectors",
        "  g       toggle grouping by inferred project",
        "  d       run a dream cycle across every palace",
        "  ?       toggle this help overlay",
        "  q / Esc close modal / quit",
    ]
    .join("\n")
}

/// Format one palace as a fixed-width table row.
///
/// Why: the PALACES panel lists every palace with its vector count in aligned
/// columns; isolating the formatter makes the alignment unit-testable. The
/// selection marker is no longer baked into the row — the [`List`] widget
/// handles the highlight via `highlight_symbol` + `highlight_style` so there
/// is no unstyled gutter between the row text and the panel border.
/// What: returns `<spinner> <name padded to 10>  <count>v`, where `spinner`
/// is the [`PalaceActivity`] prefix character (a space for Idle).
/// Test: `test_palace_row_display`.
pub fn palace_row(palace: &PalaceRow, _selected: bool) -> String {
    palace_row_with_activity(palace, PalaceActivity::Idle, 0)
}

/// Format one palace row with an explicit activity state and spinner tick.
///
/// Why: the live renderer needs to emit the activity-state spinner glyph
/// (yellow / cyan / magenta / red); separating this from the pure
/// `palace_row` keeps the existing legacy callers and tests compiling while
/// the renderer uses the richer overload.
/// What: returns `<spinner-glyph> <name padded to 10>  <count>v`.
/// Test: `test_palace_row_with_activity`.
pub fn palace_row_with_activity(
    palace: &PalaceRow,
    activity: PalaceActivity,
    tick: usize,
) -> String {
    let prefix = activity.prefix(tick);
    let label = if palace.name.is_empty() {
        &palace.id
    } else {
        &palace.name
    };
    format!(
        "{prefix} {:<10} {:>7}v",
        truncate(label, 10),
        format_count(palace.vector_count),
    )
}

/// One rendered row of the PALACES panel.
///
/// Why: the renderer styles four row kinds differently — the "All" row is
/// bold, group headers are bold yellow and non-selectable, the selected row is
/// highlighted, ordinary rows are plain — so the line builder must surface
/// which kind each row is rather than just a bool.
/// What: the row `text`, whether it is `selected`, whether it is the synthetic
/// `is_all` ("All palaces") row, and whether it is a group header (non-
/// selectable when grouping by project).
/// Test: `test_palace_lines`, `test_all_selector`, `test_palace_lines_grouped`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PalaceListRow {
    /// The fully-formatted row text.
    pub text: String,
    /// Whether this row is the current selection.
    pub selected: bool,
    /// Whether this row is the synthetic "All palaces" entry.
    pub is_all: bool,
    /// Whether this row is a non-selectable group header.
    pub is_header: bool,
    /// The palace's activity state, when this row represents a real palace.
    ///
    /// Why: drives the spinner glyph's foreground colour at render time; the
    /// "All" and header rows carry `None` because their colour is fixed.
    /// What: `Some(state)` for a palace row, `None` for All / header / empty.
    /// Test: `test_palace_lines_activity`.
    pub activity: Option<PalaceActivity>,
}

/// Format an indented palace row for use under a group header.
///
/// Why: companion to [`palace_row_with_activity`] for the grouped layout —
/// matches the same spinner-glyph + label column structure but with the
/// one-space group indent that keeps the count column aligned.
/// What: returns `" <spinner> <name padded to 9>  <count>v"`.
/// Test: `test_palace_row_with_activity`.
fn palace_row_indented_with_activity(
    palace: &PalaceRow,
    activity: PalaceActivity,
    tick: usize,
) -> String {
    let prefix = activity.prefix(tick);
    let label = if palace.name.is_empty() {
        &palace.id
    } else {
        &palace.name
    };
    format!(
        " {prefix} {:<9} {:>7}v",
        truncate(label, 9),
        format_count(palace.vector_count),
    )
}

/// Apply [`MemoryTuiState::filter`] and [`MemoryTuiState::sort_key`] to the
/// state's palaces, returning the visible subset in display order.
///
/// Why: delegates to the shared [`tui_common::filtered_sorted`] so memory and
/// search apply identical filter / sort rules. Empty palaces (zero vectors and
/// zero KG triples) are dropped first — they carry no recallable or graph
/// content and would only clutter the list. Kept as a memory-named wrapper for
/// the existing tests and callers.
/// What: filters out empty palaces via [`palace_has_content`], then delegates
/// to [`tui_common::filtered_sorted`].
/// Test: `test_apply_filter`, `test_apply_sort_*`, `test_filter_empty_palaces`.
pub fn filtered_sorted_palaces(state: &MemoryTuiState) -> Vec<PalaceRow> {
    let nonempty: Vec<PalaceRow> = state
        .palaces
        .iter()
        .filter(|p| palace_has_content(p))
        .cloned()
        .collect();
    tui_common::filtered_sorted(&nonempty, &state.filter, state.sort_key)
}

/// Ids of the rows the user can navigate between, in visible display order.
///
/// Why: thin wrapper over the shared [`tui_common::visible_ids`].
/// What: delegates to the shared helper with the memory state's fields.
/// Test: `test_visible_palace_ids`, `test_navigate_visible`.
pub fn visible_palace_ids(state: &MemoryTuiState) -> Vec<String> {
    let nonempty: Vec<PalaceRow> = state
        .palaces
        .iter()
        .filter(|p| palace_has_content(p))
        .cloned()
        .collect();
    tui_common::visible_ids(
        &nonempty,
        &state.filter,
        state.sort_key,
        state.group_by_project,
    )
}

/// Move the cursor up one row in the visible (filtered + sorted) list.
///
/// Why: thin wrapper over the shared [`tui_common::navigate_up`].
/// What: delegates and writes back the new cursor.
/// Test: `test_navigate_visible`.
pub fn navigate_up_visible(state: &mut MemoryTuiState) {
    // Filter empty palaces so arrows step over visible content only — but map
    // the resulting cursor back into the original `state.palaces` array by id.
    let nonempty: Vec<PalaceRow> = state
        .palaces
        .iter()
        .filter(|p| palace_has_content(p))
        .cloned()
        .collect();
    let current_id = state
        .selected_id()
        .map(str::to_string)
        .unwrap_or_else(|| tui_common::ALL_SENTINEL.to_string());
    let local_cursor = tui_common::id_to_cursor(&nonempty, &current_id).unwrap_or(0);
    let new_local = tui_common::navigate_up(
        &nonempty,
        local_cursor,
        &state.filter,
        state.sort_key,
        state.group_by_project,
    );
    let new_id = tui_common::current_visible_id(&nonempty, new_local);
    state.selected = tui_common::id_to_cursor(&state.palaces, &new_id).unwrap_or(0);
}

/// Move the cursor down one row in the visible (filtered + sorted) list.
///
/// Why: thin wrapper over the shared [`tui_common::navigate_down`].
/// What: delegates and writes back the new cursor.
/// Test: `test_navigate_visible`.
pub fn navigate_down_visible(state: &mut MemoryTuiState) {
    let nonempty: Vec<PalaceRow> = state
        .palaces
        .iter()
        .filter(|p| palace_has_content(p))
        .cloned()
        .collect();
    let current_id = state
        .selected_id()
        .map(str::to_string)
        .unwrap_or_else(|| tui_common::ALL_SENTINEL.to_string());
    let local_cursor = tui_common::id_to_cursor(&nonempty, &current_id).unwrap_or(0);
    let new_local = tui_common::navigate_down(
        &nonempty,
        local_cursor,
        &state.filter,
        state.sort_key,
        state.group_by_project,
    );
    let new_id = tui_common::current_visible_id(&nonempty, new_local);
    state.selected = tui_common::id_to_cursor(&state.palaces, &new_id).unwrap_or(0);
}

/// Row index — within the rendered `palace_lines` output — that the cursor
/// currently sits on.
///
/// Why: ratatui's `ListState::with_selected` and the viewport scroll math
/// both index into the rendered list, but `state.selected` is an index into
/// the *original* `state.palaces` Vec. After a filter, sort, or grouping
/// reorders rows, the two indices diverge and the highlight + scroll latch
/// onto the wrong on-screen line. This helper bridges them: given the same
/// state the renderer sees, it returns the visible row at which the current
/// selection is drawn so the highlight follows the sorted order.
/// What: returns `0` when "All" is selected; otherwise walks
/// [`palace_lines`] looking for the row whose `selected` flag is set and
/// returns its index. Falls back to `0` (the "All" row) when no matching
/// row is found, which mirrors how `clamp_to_visible` collapses a hidden
/// selection back to "All".
/// Test: `test_visible_selected_row_follows_sort`,
/// `test_visible_selected_row_follows_group`.
pub fn visible_selected_row(state: &MemoryTuiState) -> usize {
    if state.selected == 0 {
        return 0;
    }
    palace_lines(state)
        .iter()
        .position(|row| row.selected)
        .unwrap_or(0)
}

/// Build the rows for the PALACES panel body.
///
/// Why: separating row construction from the ratatui widgets lets a test
/// assert the rendered content without a terminal backend.
/// What: returns the synthetic "All palaces" row first (carrying the summed
/// vector count across every palace), then either a flat list of filtered +
/// sorted palace rows, or — when [`MemoryTuiState::group_by_project`] is set —
/// non-selectable `── <project> ──` group headers interleaved with their
/// member palaces. With no palaces the "All" row is still shown followed by a
/// placeholder line.
/// Test: `test_palace_lines`, `test_all_selector`, `test_palace_lines_grouped`.
pub fn palace_lines(state: &MemoryTuiState) -> Vec<PalaceListRow> {
    palace_lines_at(state, chrono::Utc::now(), 0)
}

/// Variant of [`palace_lines`] that takes an explicit clock and spinner tick.
///
/// Why: the live renderer needs to drive the activity-state spinner from the
/// wall-clock without polluting the broader test suite with clock dependencies.
/// Splitting the time inputs out also makes the activity-state assertions
/// deterministic.
/// What: identical to [`palace_lines`] except that `now` drives the per-palace
/// [`PalaceActivity`] derivation and `tick` selects the spinner frame.
/// Test: `test_palace_lines_activity`.
pub fn palace_lines_at(
    state: &MemoryTuiState,
    now: chrono::DateTime<chrono::Utc>,
    tick: usize,
) -> Vec<PalaceListRow> {
    let mut rows: Vec<PalaceListRow> = Vec::with_capacity(state.palaces.len() + 1);

    // The synthetic "All palaces" row always leads the list — including when
    // filtering or grouping is active. The selection highlight is rendered by
    // the List widget's highlight_symbol so the row text carries no marker.
    let total_vectors: u64 = state.palaces.iter().map(|p| p.vector_count).sum();
    let all_selected = state.selected == 0;
    rows.push(PalaceListRow {
        text: format!("  {ALL_LABEL}  {}v", format_count(total_vectors)),
        selected: all_selected,
        is_all: true,
        is_header: false,
        activity: None,
    });

    if state.palaces.is_empty() {
        // While the first daemon poll is still in flight we don't yet know
        // whether the palace list is genuinely empty or just unfetched — show
        // a "Loading…" placeholder so the left panel doesn't look broken on
        // startup. Once the poll resolves we fall through to "(no palaces)".
        let text = if state.daemon_status == DaemonStatus::Connecting {
            "  Loading…".to_string()
        } else {
            "  (no palaces)".to_string()
        };
        rows.push(PalaceListRow {
            text,
            selected: false,
            is_all: false,
            is_header: false,
            activity: None,
        });
        return rows;
    }

    let visible = filtered_sorted_palaces(state);
    if visible.is_empty() {
        rows.push(PalaceListRow {
            text: "  (no matches)".to_string(),
            selected: false,
            is_all: false,
            is_header: false,
            activity: None,
        });
        return rows;
    }

    // We need to compute the cursor row each visible palace lives at. The cursor
    // addresses the *original* `state.palaces` indices (cursor n → palaces[n-1])
    // so we look up each visible palace's original index by id.
    let cursor_for = |p: &PalaceRow| -> usize {
        state
            .palaces
            .iter()
            .position(|orig| orig.id == p.id)
            .map(|i| i + 1)
            .unwrap_or(0)
    };

    if state.group_by_project {
        // Collect distinct projects in the order they first appear in `visible`.
        let mut seen: Vec<String> = Vec::new();
        for p in &visible {
            let proj = p.project().to_string();
            if !seen.iter().any(|s| s == &proj) {
                seen.push(proj);
            }
        }
        for project in &seen {
            rows.push(PalaceListRow {
                text: format!("── {project} ─────"),
                selected: false,
                is_all: false,
                is_header: true,
                activity: None,
            });
            for palace in visible.iter().filter(|p| p.project() == project) {
                let cursor = cursor_for(palace);
                let selected = cursor == state.selected;
                let activity = palace_activity_state(palace, now);
                rows.push(PalaceListRow {
                    text: palace_row_indented_with_activity(palace, activity, tick),
                    selected,
                    is_all: false,
                    is_header: false,
                    activity: Some(activity),
                });
            }
        }
    } else {
        for palace in &visible {
            let cursor = cursor_for(palace);
            let selected = cursor == state.selected;
            let activity = palace_activity_state(palace, now);
            rows.push(PalaceListRow {
                text: palace_row_with_activity(palace, activity, tick),
                selected,
                is_all: false,
                is_header: false,
                activity: Some(activity),
            });
        }
    }
    rows
}

/// Build the STATISTICS panel lines for the current selection.
///
/// Why: the bottom-right panel shows counts and sizes for whichever palace is
/// selected, or aggregate totals plus a per-palace breakdown when "All" is
/// selected; isolating the builder makes the content testable without a
/// terminal. While the daemon is still being polled for the first time we
/// surface a "Loading…" placeholder so the panel does not flash zeroes that
/// are indistinguishable from a genuinely empty daemon.
/// What: for a single palace, returns its name, vector count, and id. For the
/// "All" selection, returns the palace count and the daemon's aggregate
/// vector / drawer / KG-triple totals, plus one `· <name>: <vectors>`
/// breakdown line per palace. Returns `["Loading…"]` while the daemon status
/// is [`DaemonStatus::Connecting`].
/// Test: `test_stats_lines`, `test_stats_lines_connecting_shows_loading`.
pub fn stats_lines(state: &MemoryTuiState) -> Vec<String> {
    if state.daemon_status == DaemonStatus::Connecting {
        return vec!["Loading…".to_string()];
    }
    if state.is_all_selected() {
        let stats = state.status.clone().unwrap_or_default();
        let mut lines = vec![
            format!("Scope:        {ALL_LABEL}"),
            format!("Palaces:      {}", state.palaces.len()),
            format!("Vectors:      {}", format_count(stats.total_vectors)),
            format!("Drawers:      {}", format_count(stats.total_drawers)),
            format!("KG triples:   {}", format_count(stats.total_kg_triples)),
        ];
        if state.palaces.is_empty() {
            lines.push("(no palaces)".to_string());
        } else {
            lines.push(String::new());
            for palace in &state.palaces {
                let label = if palace.name.is_empty() {
                    &palace.id
                } else {
                    &palace.name
                };
                lines.push(format!(
                    "  · {:<12} {:>7}v",
                    truncate(label, 12),
                    format_count(palace.vector_count),
                ));
            }
        }
        return lines;
    }

    match state.palaces.get(state.selected.saturating_sub(1)) {
        Some(palace) => {
            let label = if palace.name.is_empty() {
                "(unnamed)"
            } else {
                palace.name.as_str()
            };
            let now = chrono::Utc::now();
            let activity = palace_activity_state(palace, now);
            let mut lines = vec![
                format!("Palace:       {label}"),
                format!("Vectors:      {}", format_count(palace.vector_count)),
                format!("Id:           {}", palace.id),
                String::new(),
                "Knowledge Graph".to_string(),
                format!("  Nodes:        {}", format_count(palace.node_count)),
                format!("  Edges:        {}", format_count(palace.edge_count)),
                format!("  Triples:      {}", format_count(palace.kg_triple_count)),
                String::new(),
            ];
            match palace.last_write_at {
                Some(ts) => {
                    lines.push(format!(
                        "Last write:   {} ({})",
                        format_relative_time(now, ts),
                        ts.format("%Y-%m-%d %H:%M:%S UTC"),
                    ));
                }
                None => lines.push("Last write:   never".to_string()),
            }
            lines.push(format!("State:        {}", activity_label(activity)));
            lines
        }
        None => vec!["(no palace selected)".to_string()],
    }
}

/// Build the title-bar line for the memory UI.
///
/// Why: the top row shows the daemon name, version, and liveness badge at a
/// glance; isolating it keeps `render` terse and the text testable.
/// What: returns `trusty-memory vX  [●] <status>` — the daemon's reported
/// version is appended when it is online.
/// Test: `test_title_line`.
pub fn title_line(state: &MemoryTuiState) -> String {
    let (glyph, label) = state.daemon_status.badge();
    match &state.daemon_status {
        DaemonStatus::Online { version, .. } => {
            format!("trusty-memory v{version}  [{glyph}] {label}")
        }
        _ => format!(
            "trusty-memory v{VERSION}  [{glyph}] {label}  {}",
            state.base_url
        ),
    }
}

/// Draw the memory TUI frame.
///
/// Why: the single entry point the event loop calls each tick.
/// What: a 4-row vertical layout — title bar, the PALACES / right-pane split,
/// the RECALL input bar, and the key-hint footer. The right pane is itself
/// split vertically into an ACTIVITY feed (top 60 %) and a STATISTICS panel
/// (bottom 40 %), both scoped to the selected palace — or aggregated when "All"
/// is selected. A centred help overlay floats on top when `show_help` is set.
/// Test: line content is unit-tested via the `*_lines` helpers; this glue is
/// exercised by `test_render_smoke`.
pub fn render(frame: &mut Frame, state: &mut MemoryTuiState) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Min(4),    // panels
            Constraint::Length(3), // recall input
            Constraint::Length(1), // key hint
        ])
        .split(area);

    // Title bar.
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title_line(state),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );

    // PALACES on the left, the ACTIVITY / STATISTICS stack on the right.
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(left_panel_width(area.width)),
            Constraint::Min(10),
        ])
        .split(rows[1]);

    let list_focused = state.focus == MemoryFocus::List;
    // Drive the spinner animation from the wall clock so each frame advances
    // without an explicit app tick.
    let now = chrono::Utc::now();
    let tick = spinner_tick();
    let rendered_rows = palace_lines_at(state, now, tick);
    let palace_items: Vec<ListItem> = rendered_rows
        .iter()
        .map(|row| {
            // Row styling — the List widget renders the *selection* highlight
            // via `highlight_style` so the row content carries only its base
            // colour. Activity-state rows colour the whole row to keep the
            // spinner glyph and its label visually linked.
            let style = if row.is_header || row.is_all {
                // Group headers and the "All" row share the bold-yellow style.
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if let Some(color) = row.activity.and_then(|a| a.color()) {
                Style::default().fg(color)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(row.text.clone(), style)))
        })
        .collect();

    // When the inline filter is active or carries text, split the left column
    // vertically so the filter input renders above the palace list.
    let show_filter_bar = state.filter_active || !state.filter.is_empty();
    let (filter_area, list_area) = if show_filter_bar {
        let inner = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(3)])
            .split(split[0]);
        (Some(inner[0]), inner[1])
    } else {
        (None, split[0])
    };

    if let Some(area) = filter_area {
        let border_color = if state.filter_active {
            Color::Yellow
        } else {
            Color::DarkGray
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("🔍 ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    state.filter.as_str().to_string(),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    if state.filter_active { "_" } else { "" },
                    Style::default().fg(Color::Cyan),
                ),
            ]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(
                        Style::default()
                            .fg(border_color)
                            .add_modifier(Modifier::BOLD),
                    )
                    .title(Span::styled(
                        " FILTER ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
            ),
            area,
        );
    }

    // Scroll the PALACES list so the selected row stays visible: the panel
    // height minus its two border rows is the visible window. Both the scroll
    // anchor and the ratatui ListState selection index must reference the
    // *displayed* row (filter + sort + grouping reorder the rendered rows
    // relative to `state.palaces`), so we look up the visible row index of
    // the currently selected palace once and use it for both.
    let palace_visible = list_area.height.saturating_sub(2) as usize;
    // Resolve the highlight row from the rows we are about to render so the
    // selection index matches one-for-one. Group headers (non-selectable) are
    // skipped — if the cursor maps to a header we fall back to row 0 ("All").
    let visible_row = rendered_rows
        .iter()
        .position(|row| row.selected && !row.is_header)
        .unwrap_or(0);
    state.sync_scroll_to(visible_row, palace_visible);
    let palace_title = format!("PALACES [{}]", sort_label(state.sort_key));
    // The List widget handles the selection highlight via highlight_style +
    // HighlightSpacing::Always so there is no unstyled gutter between the row
    // text and the right border. The leading `> ` symbol replaces the old
    // inline marker that used to be baked into the row text.
    let highlight_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let mut palace_state = ListState::default()
        .with_offset(state.scroll_offset)
        .with_selected(Some(visible_row));
    frame.render_stateful_widget(
        List::new(palace_items)
            .block(panel_block(&palace_title, list_focused))
            .highlight_style(highlight_style)
            .highlight_symbol("> ")
            .highlight_spacing(HighlightSpacing::Always),
        list_area,
        &mut palace_state,
    );

    // Right pane: ACTIVITY (top) over STATISTICS (bottom).
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(tui_common::ACTIVITY_PERCENT),
            Constraint::Percentage(100 - tui_common::ACTIVITY_PERCENT),
        ])
        .split(split[1]);

    // ACTIVITY panel — when a single palace is selected (issue #184) the
    // panel renders a paged drawer list; for "All palaces" it falls back to
    // the streamed event log so cross-palace events stay visible.
    let scope = state.scope_filter();
    let drawer_pane_focused = state.focus == MemoryFocus::DrawerPane;
    // Issue #215: surface the focus state in the panel title so the
    // operator can tell which zone owns the cursor at a glance. The `▶`
    // glyph mirrors the recall bar's leading marker for consistency.
    let activity_title = match scope {
        Some(id) if drawer_pane_focused => format!("DRAWER ▶ {id}"),
        Some(id) => format!("ACTIVITY — {id}"),
        None if drawer_pane_focused => format!("DRAWER ▶ {ALL_LABEL}"),
        None => format!("ACTIVITY — {ALL_LABEL}"),
    };
    let activity_height = right[0].height.saturating_sub(2) as usize;
    let drawer_total = state
        .selected
        .checked_sub(1)
        .and_then(|i| state.palaces.get(i))
        .map(|p| p.drawer_count)
        .unwrap_or(0);
    let drawer_lines = drawer_panel_lines(state, drawer_total);
    // Issue #215: render the drawer page as a stateful List so the
    // ratatui highlight can mark the focused drawer row. The header line
    // (row 0 of `drawer_lines` when the slice is non-empty) is included so
    // the page indicator stays visible; only data rows are selectable,
    // tracked via `drawer_cursor + 1` to skip the header offset.
    if !drawer_lines.is_empty() {
        let take = activity_height.max(1);
        let visible_lines: Vec<String> = drawer_lines.into_iter().take(take).collect();
        let items: Vec<ListItem> = visible_lines
            .iter()
            .map(|s| ListItem::new(s.clone()))
            .collect();
        // The first line is the header ("drawers N–M of T (page p)"); data
        // rows follow at index 1.. so the selection index is the cursor
        // plus 1 when the drawer pane has focus.
        let selected_row = if drawer_pane_focused && !state.drawer_list.drawers.is_empty() {
            Some((state.drawer_cursor + 1).min(visible_lines.len().saturating_sub(1)))
        } else {
            None
        };
        let highlight_style = Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let mut list_state = ListState::default().with_selected(selected_row);
        frame.render_stateful_widget(
            List::new(items)
                .block(panel_block(&activity_title, drawer_pane_focused))
                .highlight_style(highlight_style)
                .highlight_symbol("> ")
                .highlight_spacing(HighlightSpacing::Always),
            right[0],
            &mut list_state,
        );
    } else {
        // Fallback: streamed event log / placeholder lines.
        let fallback_items: Vec<ListItem> = if state.log.has_scoped(scope) {
            state
                .log
                .tail_scoped(scope, activity_height.max(1))
                .map(|line| ListItem::new(line.as_str()))
                .collect()
        } else if state.daemon_status == DaemonStatus::Connecting {
            // Distinguish "first poll has not completed" from "genuinely no
            // activity" so the panel doesn't look broken on startup.
            vec![ListItem::new("Loading…")]
        } else {
            vec![ListItem::new("(no activity yet)")]
        };
        frame.render_widget(
            List::new(fallback_items).block(panel_block(&activity_title, drawer_pane_focused)),
            right[0],
        );
    }

    // STATISTICS panel — counts and sizes for the selection.
    let stats_items: Vec<ListItem> = stats_lines(state).into_iter().map(ListItem::new).collect();
    frame.render_widget(
        List::new(stats_items).block(panel_block("STATISTICS", false)),
        right[1],
    );

    // RECALL input bar.
    let input_focused = state.focus == MemoryFocus::Input;
    let cursor = if input_focused { "_" } else { "" };
    let input_style = if input_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("RECALL ▶ ", Style::default().fg(Color::Yellow)),
            Span::styled(format!("{}{cursor}", state.input), input_style),
        ]))
        .block(panel_block("RECALL", input_focused)),
        rows[2],
    );

    // Key-hint footer.
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            KEY_HINT,
            Style::default().fg(Color::DarkGray),
        ))),
        rows[3],
    );

    if state.show_help {
        tui_common::render_help_overlay(frame, &help_text());
    }

    // Issue #215: drawer-detail modal floats over the rest of the UI when
    // open. Render last so it sits on top of everything (including the help
    // overlay) and is the visual "front" element.
    if state.drawer_detail_open {
        render_drawer_detail_modal(frame, state);
    }
}

/// Render the drawer-detail modal as a centred floating overlay (issue #215).
///
/// Why: the activity panel only shows a truncated snippet per drawer; the
/// modal exposes the full body, all tags, and the timestamp for the drawer
/// the operator opened with `Enter` in the drawer pane.
/// What: clears a centred rectangle (≈80 % width × ≈70 % height of the
/// frame) and draws a bordered block carrying a header (drawer id,
/// timestamp, creator tag, tag list), the verbatim memory bodies separated
/// by `───` rules, and a `Loading…` placeholder while the fetch is in
/// flight. `drawer_detail_scroll` scrolls the content vertically.
///
/// Test: smoke-tested via `test_render_with_drawer_detail_open`.
fn render_drawer_detail_modal(frame: &mut Frame, state: &MemoryTuiState) {
    let area = frame.area();
    let w = ((area.width as u32 * 80 / 100) as u16)
        .max(20)
        .min(area.width);
    let h = ((area.height as u32 * 70 / 100) as u16)
        .max(8)
        .min(area.height);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);

    let body = drawer_detail_body(state);
    let title = drawer_detail_title(state);
    let para = Paragraph::new(body)
        .style(Style::default().fg(Color::White))
        .wrap(Wrap { trim: false })
        .scroll((state.drawer_detail_scroll as u16, 0))
        .block(
            Block::default().borders(Borders::ALL).title(Span::styled(
                title,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
        );
    frame.render_widget(para, rect);
}

/// Compose the rendered body text for the drawer-detail modal (issue #215).
///
/// Why: separating the body builder from the rendering call lets a test
/// assert the modal carries the expected header, memory bodies, and
/// separators without spinning up a terminal backend.
/// What: returns a single `String` shaped as
///   `<header>\n\n<memory 0 content>\n\n───\n\n<memory 1 content>\n…`.
/// The header is the drawer id, creation timestamp, creator label, and the
/// raw tag list (joined with commas). When the fetch is still loading the
/// body collapses to `Loading…`; when the fetch returned zero memories the
/// body shows `(no memories returned)`.
/// Test: `test_drawer_detail_body_layout`, `test_drawer_detail_body_loading`.
pub fn drawer_detail_body(state: &MemoryTuiState) -> String {
    if state.drawer_detail_loading {
        return "Loading…".to_string();
    }
    if state.drawer_detail_memories.is_empty() {
        return "(no memories returned)".to_string();
    }
    let mut out = String::new();
    for (i, memory) in state.drawer_detail_memories.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n──────────────────────────────────────\n\n");
        }
        // Header for this memory.
        let ts = memory
            .created_at
            .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "(no timestamp)".to_string());
        let creator = crate::monitor::memory_client::creator_label(&memory.tags);
        let tag_join = if memory.tags.is_empty() {
            "(none)".to_string()
        } else {
            memory.tags.join(", ")
        };
        let header_id = if memory.id.is_empty() {
            "(no id)".to_string()
        } else {
            memory.id.clone()
        };
        out.push_str(&format!("Drawer: {header_id}\n"));
        out.push_str(&format!("Time:   {ts}\n"));
        out.push_str(&format!("By:     {creator}\n"));
        out.push_str(&format!("Tags:   {tag_join}\n"));
        out.push('\n');
        if memory.content.is_empty() {
            out.push_str("(empty content)");
        } else {
            out.push_str(&memory.content);
        }
    }
    out
}

/// Title-bar text for the drawer-detail modal (issue #215).
///
/// Why: kept separate so a test can verify the title surfaces the current
/// memory count and the active index without depending on the renderer.
/// What: returns ` Drawer detail — <i+1>/<n>  (esc/q close, ↑/↓ scroll) `
/// when at least one memory is loaded, or ` Drawer detail (loading…) ` /
/// ` Drawer detail (empty) ` for the transient states.
/// Test: `test_drawer_detail_title`.
pub fn drawer_detail_title(state: &MemoryTuiState) -> String {
    if state.drawer_detail_loading {
        return " Drawer detail (loading…) ".to_string();
    }
    if state.drawer_detail_memories.is_empty() {
        return " Drawer detail (empty) ".to_string();
    }
    let n = state.drawer_detail_memories.len();
    let i = state.drawer_detail_idx.min(n.saturating_sub(1));
    format!(" Drawer detail — {}/{n}  (esc/q close, ↑/↓ scroll) ", i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::utils::timestamped;
    use ratatui::{Terminal, backend::TestBackend};

    /// A state with two palaces and aggregate stats for rendering tests.
    fn sample_state() -> MemoryTuiState {
        let mut state = MemoryTuiState::new("http://127.0.0.1:7070");
        state.daemon_status = DaemonStatus::Online {
            version: "0.1.54".into(),
            uptime_secs: 0,
        };
        state.palaces = vec![
            PalaceRow {
                id: "default".into(),
                name: "default".into(),
                vector_count: 8_400,
                ..Default::default()
            },
            PalaceRow {
                id: "work".into(),
                name: "work".into(),
                vector_count: 0,
                // Non-zero KG triple count keeps the palace visible — the
                // empty-palace filter drops rows with zero vectors AND zero
                // triples.
                kg_triple_count: 42,
                ..Default::default()
            },
        ];
        state.status = Some(MemoryData {
            version: "0.1.54".into(),
            palace_count: 2,
            total_drawers: 14,
            total_vectors: 8_400,
            total_kg_triples: 1_200,
            palaces: state.palaces.clone(),
        });
        state
    }

    #[test]
    fn test_new_state_defaults() {
        let state = MemoryTuiState::new("http://127.0.0.1:7070");
        assert_eq!(state.base_url, "http://127.0.0.1:7070");
        assert!(matches!(state.daemon_status, DaemonStatus::Connecting));
        assert!(state.status.is_none());
        assert!(state.palaces.is_empty());
        assert_eq!(state.selected, 0);
        assert!(state.log.is_empty());
        assert_eq!(state.focus, MemoryFocus::List);
        assert!(!state.show_help);
    }

    #[test]
    fn test_toggle_focus() {
        let mut state = MemoryTuiState::new("http://x");
        assert_eq!(state.focus, MemoryFocus::List);
        state.toggle_focus();
        assert_eq!(state.focus, MemoryFocus::Input);
        state.toggle_focus();
        assert_eq!(state.focus, MemoryFocus::List);
    }

    #[test]
    fn test_selected_clamp() {
        let mut state = sample_state();
        // The list has 1 ("All") + 2 palaces = 3 rows; the cursor stops at 2.
        for _ in 0..10 {
            state.select_down();
        }
        assert_eq!(state.selected, 2, "clamped to palaces.len()");
        for _ in 0..10 {
            state.select_up();
        }
        assert_eq!(state.selected, 0);
        // A shrunk palace list re-clamps the cursor (1 "All" + 1 palace = 1).
        state.selected = 2;
        state.palaces.truncate(1);
        state.clamp_selection();
        assert_eq!(state.selected, 1);
        // An empty list leaves only the "All" row at cursor 0.
        state.palaces.clear();
        state.selected = 9;
        state.clamp_selection();
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_selected_id() {
        let mut state = sample_state();
        // Cursor 0 is "All" — no single palace.
        assert!(state.is_all_selected());
        assert_eq!(state.selected_id(), None);
        // Cursor 1 is the first palace.
        state.select_down();
        assert_eq!(state.selected_id(), Some("default"));
        state.select_down();
        assert_eq!(state.selected_id(), Some("work"));
        state.palaces.clear();
        state.clamp_selection();
        assert_eq!(state.selected_id(), None);
    }

    #[test]
    fn test_all_selector() {
        let mut state = sample_state();
        // The default selection is the "All palaces" row.
        assert!(state.is_all_selected());
        assert_eq!(state.scope_filter(), None);
        // Moving down off row 0 picks a single palace and a scoped filter.
        state.select_down();
        assert!(!state.is_all_selected());
        assert_eq!(state.scope_filter(), Some("default"));
        state.select_up();
        assert!(state.is_all_selected());

        // The palace list always leads with the "All" row.
        let rows = palace_lines(&state);
        assert_eq!(rows.len(), 3, "1 'All' row + 2 palaces");
        assert!(rows[0].is_all);
        assert!(rows[0].text.contains(ALL_LABEL));
        assert!(rows[0].selected, "'All' is selected by default");
        assert!(!rows[1].is_all);
        assert!(rows[1].text.contains("default"));
    }

    #[test]
    fn test_stats_lines() {
        let mut state = sample_state();
        // "All" selected → aggregate totals + per-palace breakdown.
        let all = stats_lines(&state);
        assert!(
            all.iter()
                .any(|l| l.contains("Palaces:") && l.contains('2'))
        );
        assert!(
            all.iter()
                .any(|l| l.contains("Vectors:") && l.contains("8,400"))
        );
        assert!(
            all.iter()
                .any(|l| l.contains("KG triples:") && l.contains("1,200"))
        );
        assert!(all.iter().any(|l| l.contains("default")));

        // A single palace selected → that palace's detail.
        state.select_down(); // cursor 1 → default
        let one = stats_lines(&state);
        assert!(
            one.iter()
                .any(|l| l.contains("Palace:") && l.contains("default"))
        );
        assert!(
            one.iter()
                .any(|l| l.contains("Vectors:") && l.contains("8,400"))
        );
        assert!(one.iter().any(|l| l.contains("Id:")));
    }

    #[test]
    fn test_stats_lines_connecting_shows_loading() {
        // While the daemon is still in the Connecting state the STATISTICS
        // panel should surface a single "Loading…" line — never a half-formed
        // zero-valued snapshot that would be indistinguishable from a real
        // empty daemon.
        let state = MemoryTuiState::new("http://x");
        assert!(matches!(state.daemon_status, DaemonStatus::Connecting));
        let lines = stats_lines(&state);
        assert_eq!(lines, vec!["Loading…".to_string()]);
    }

    #[test]
    fn test_palace_row_display() {
        // The selection highlight is now applied by the List widget via
        // `highlight_symbol`, so the row text itself begins with a space-
        // prefixed activity glyph (a space for the Idle state).
        let palace = PalaceRow {
            id: "default".into(),
            name: "default".into(),
            vector_count: 8_400,
            ..Default::default()
        };
        let row = palace_row(&palace, true);
        // Idle activity → leading space, then a space, then the label.
        assert!(row.starts_with("  "), "leading spinner+space: {row}");
        assert!(row.contains("default"));
        assert!(row.contains("8,400v"));

        let unselected = palace_row(&palace, false);
        assert!(unselected.starts_with(' '), "unselected: {unselected}");

        // A nameless palace falls back to its id; a zero count still renders.
        let nameless = PalaceRow {
            id: "p-xyz".into(),
            name: String::new(),
            vector_count: 0,
            ..Default::default()
        };
        let row = palace_row(&nameless, false);
        assert!(row.contains("p-xyz"));
        assert!(row.contains("0v"));

        // A long name is truncated with an ellipsis.
        let long = PalaceRow {
            id: "x".into(),
            name: "a-very-long-palace-name".into(),
            vector_count: 1,
            ..Default::default()
        };
        assert!(palace_row(&long, false).contains('…'));
    }

    #[test]
    fn test_palace_lines() {
        let state = sample_state();
        let rows = palace_lines(&state);
        // 1 "All" row + 2 palace rows.
        assert_eq!(rows.len(), 3);
        // Row 0 is "All", selected by default.
        assert!(rows[0].is_all);
        assert!(rows[0].selected);
        assert!(rows[0].text.contains(ALL_LABEL));
        // Rows 1..3 are the palaces, unselected.
        assert!(!rows[1].is_all && !rows[1].selected);
        assert!(rows[1].text.contains("default"));
        assert!(rows[2].text.contains("work"));

        // An empty palace list still shows the "All" row plus a placeholder.
        // The placeholder text depends on daemon status: "Loading…" while the
        // first poll is in flight, "(no palaces)" once the daemon is reachable
        // but has no palaces registered.
        let mut empty = MemoryTuiState::new("http://x");
        empty.daemon_status = DaemonStatus::Online {
            version: "0.1.54".into(),
            uptime_secs: 0,
        };
        let rows = palace_lines(&empty);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].is_all);
        assert!(rows[1].text.contains("no palaces"));

        // Before the first daemon poll completes the placeholder switches to
        // "Loading…" so the panel doesn't look broken on startup.
        let connecting = MemoryTuiState::new("http://x");
        assert!(matches!(connecting.daemon_status, DaemonStatus::Connecting));
        let rows = palace_lines(&connecting);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].is_all);
        assert!(
            rows[1].text.contains("Loading…"),
            "connecting state must show Loading…, got: {:?}",
            rows[1].text
        );
    }

    #[test]
    fn test_log_append_dream() {
        // A dream_completed SSE event appends a header line plus an indented
        // merge/prune/compact stats line.
        let mut state = MemoryTuiState::new("http://x");
        apply_memory_event(
            &mut state,
            MemoryEvent::DreamCompleted {
                merged: 3,
                pruned: 1,
                compacted: 0,
            },
        );
        let lines: Vec<&String> = state.log.iter().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("SSE: dream_completed"));
        assert!(lines[0].starts_with('['), "header is timestamped");
        assert!(lines[1].contains("merged: 3"));
        assert!(lines[1].contains("pruned: 1"));
        assert!(lines[1].contains("compacted: 0"));
        // The continuation line is not timestamped — it reads as a sub-line.
        assert!(lines[1].starts_with("  "));
    }

    #[test]
    fn test_apply_memory_event() {
        let mut state = MemoryTuiState::new("http://x");
        apply_memory_event(
            &mut state,
            MemoryEvent::DrawerAdded {
                palace_id: "default".into(),
                drawer_count: 14,
                content_preview: "How the migration system handles…".into(),
            },
        );
        apply_memory_event(
            &mut state,
            MemoryEvent::DrawerDeleted {
                palace_id: "work".into(),
                drawer_count: 2,
            },
        );
        apply_memory_event(
            &mut state,
            MemoryEvent::PalaceCreated {
                name: "notes".into(),
            },
        );
        let lines: Vec<&String> = state.log.iter().collect();
        assert_eq!(lines.len(), 3);
        // With a content preview present, the log line shows it after the count.
        assert!(lines[0].contains("drawer added → default (14)"));
        assert!(lines[0].contains("\"How the migration system handles…\""));
        assert!(lines[1].contains("drawer deleted → work (2)"));
        assert!(lines[2].contains("palace created → notes"));

        // Drawer events are scoped to their palace; the per-palace feed keeps
        // only its own drawer event plus the daemon-wide palace-created line.
        let default_feed: Vec<&String> = state.log.tail_scoped(Some("default"), 100).collect();
        assert_eq!(default_feed.len(), 2);
        assert!(
            default_feed
                .iter()
                .any(|l| l.contains("drawer added → default"))
        );
        assert!(
            default_feed
                .iter()
                .any(|l| l.contains("palace created → notes"))
        );
        assert!(
            !default_feed
                .iter()
                .any(|l| l.contains("drawer deleted → work"))
        );
    }

    #[test]
    fn test_log_capacity() {
        let mut state = MemoryTuiState::new("http://x");
        for i in 0..(ActivityLog::MAX_ENTRIES + 30) {
            state.log.push(format!("event {i}"));
        }
        assert_eq!(state.log.len(), ActivityLog::MAX_ENTRIES);
    }

    #[test]
    fn test_timestamped_format() {
        let line = timestamped("recall complete");
        assert!(line.starts_with('['));
        assert!(line.ends_with(" recall complete"));
        assert_eq!(line.as_bytes()[9], b']');
    }

    #[test]
    fn test_left_panel_width() {
        assert_eq!(left_panel_width(200), tui_common::LEFT_PANEL_MAX);
        assert_eq!(left_panel_width(60), 20);
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("work", 10), "work");
        assert_eq!(truncate("a-very-long-palace", 8), "a-very-…");
    }

    #[test]
    fn test_title_line() {
        let state = sample_state();
        let title = title_line(&state);
        assert!(title.contains("trusty-memory v0.1.54"));
        assert!(title.contains("online"));

        let mut offline = MemoryTuiState::new("http://127.0.0.1:7070");
        offline.daemon_status = DaemonStatus::Offline {
            last_error: "refused".into(),
        };
        let title = title_line(&offline);
        assert!(title.contains("offline"));
        assert!(title.contains("http://127.0.0.1:7070"));
    }

    #[test]
    fn test_palace_sort_key_cycle() {
        assert_eq!(PalaceSortKey::default(), PalaceSortKey::Activity);
        assert_eq!(PalaceSortKey::Activity.next(), PalaceSortKey::Name);
        assert_eq!(PalaceSortKey::Name.next(), PalaceSortKey::Count);
        assert_eq!(PalaceSortKey::Count.next(), PalaceSortKey::Activity);
        assert_eq!(sort_label(PalaceSortKey::Activity), "Activity");
        assert_eq!(sort_label(PalaceSortKey::Name), "Name");
        assert_eq!(sort_label(PalaceSortKey::Count), "Vectors");
    }

    /// State with four palaces spanning two projects, varied vector counts,
    /// and varied last_write_at timestamps. Used by the sort / filter / group
    /// tests.
    fn diverse_state() -> MemoryTuiState {
        use chrono::{TimeZone, Utc};
        let mut state = MemoryTuiState::new("http://127.0.0.1:7070");
        state.palaces = vec![
            PalaceRow {
                id: "trusty-search".into(),
                name: "trusty-search".into(),
                vector_count: 12,
                last_write_at: Some(Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap()),
                description: Some(
                    "Auto-registered from /Users/masa/Projects/trusty-tools/trusty-search".into(),
                ),
                ..Default::default()
            },
            PalaceRow {
                id: "trusty-memory".into(),
                name: "trusty-memory".into(),
                vector_count: 3_775,
                last_write_at: Some(Utc.with_ymd_and_hms(2026, 5, 18, 22, 29, 50).unwrap()),
                description: Some(
                    "Auto-registered from /Users/masa/Projects/trusty-tools/trusty-memory".into(),
                ),
                ..Default::default()
            },
            PalaceRow {
                id: "claude-mpm".into(),
                name: "claude-mpm".into(),
                vector_count: 6_163,
                last_write_at: Some(Utc.with_ymd_and_hms(2026, 5, 10, 0, 0, 0).unwrap()),
                description: Some("Auto-registered from /Users/masa/Projects/claude-mpm".into()),
                ..Default::default()
            },
            PalaceRow {
                id: "notes".into(),
                name: "notes".into(),
                vector_count: 100,
                last_write_at: None,
                description: None,
                ..Default::default()
            },
        ];
        state
    }

    #[test]
    fn test_apply_sort_activity() {
        // Activity: last_write_at desc, None last; vector_count desc tiebreak.
        let mut state = diverse_state();
        state.sort_key = PalaceSortKey::Activity;
        let rows = filtered_sorted_palaces(&state);
        assert_eq!(rows[0].id, "trusty-memory");
        assert_eq!(rows[1].id, "claude-mpm");
        assert_eq!(rows[2].id, "trusty-search");
        // None sorts last.
        assert_eq!(rows[3].id, "notes");
    }

    #[test]
    fn test_apply_sort_name() {
        let mut state = diverse_state();
        state.sort_key = PalaceSortKey::Name;
        let rows = filtered_sorted_palaces(&state);
        let names: Vec<&str> = rows.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["claude-mpm", "notes", "trusty-memory", "trusty-search"]
        );
    }

    #[test]
    fn test_apply_sort_vectors() {
        let mut state = diverse_state();
        state.sort_key = PalaceSortKey::Count;
        let rows = filtered_sorted_palaces(&state);
        assert_eq!(rows[0].id, "claude-mpm");
        assert_eq!(rows[1].id, "trusty-memory");
        assert_eq!(rows[2].id, "notes");
        assert_eq!(rows[3].id, "trusty-search");
    }

    #[test]
    fn test_apply_filter() {
        let mut state = diverse_state();
        // Case-insensitive substring match against name OR project.
        state.filter = "TRUSTY".into();
        let rows = filtered_sorted_palaces(&state);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|p| p.name.contains("trusty")));

        // Match by project (description path basename).
        state.filter = "claude-mpm".into();
        let rows = filtered_sorted_palaces(&state);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "claude-mpm");

        // No match → empty.
        state.filter = "nothing-here".into();
        assert!(filtered_sorted_palaces(&state).is_empty());

        // Empty filter → everything.
        state.filter.clear();
        assert_eq!(filtered_sorted_palaces(&state).len(), 4);
    }

    #[test]
    fn test_palace_lines_grouped() {
        let mut state = diverse_state();
        state.group_by_project = true;
        state.sort_key = PalaceSortKey::Name;
        let rows = palace_lines(&state);

        // "All" leads the list.
        assert!(rows[0].is_all);

        // Group headers appear and are non-selectable.
        let headers: Vec<&PalaceListRow> = rows.iter().filter(|r| r.is_header).collect();
        assert!(
            !headers.is_empty(),
            "grouping must emit at least one header"
        );
        for h in &headers {
            assert!(h.text.contains("──"));
            assert!(!h.selected);
        }
        // Project names appear in the header text.
        let header_text: String = headers
            .iter()
            .map(|h| h.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(header_text.contains("trusty-memory") || header_text.contains("trusty-search"));
        assert!(header_text.contains("claude-mpm"));

        // Filter narrows grouping to matching projects only.
        state.filter = "claude".into();
        let rows = palace_lines(&state);
        let headers: Vec<&PalaceListRow> = rows.iter().filter(|r| r.is_header).collect();
        assert_eq!(headers.len(), 1);
        assert!(headers[0].text.contains("claude-mpm"));
    }

    #[test]
    fn test_help_text_lists_bindings() {
        let text = help_text();
        for token in ["Tab", "d ", "Enter", "?", "q ", "/", "s ", "g "] {
            assert!(text.contains(token), "help text missing {token}");
        }
    }

    #[test]
    fn test_scroll_offset() {
        // A list taller than its viewport must scroll so the cursor stays in
        // view; a list that fits leaves the offset pinned at zero.
        let mut state = sample_state();
        // 2 palaces + the "All" row = 3 rows; a 6-row window holds them all.
        for row in 0..=state.last_row() {
            state.selected = row;
            state.sync_scroll(6);
            assert_eq!(state.scroll_offset, 0, "no scroll while the list fits");
        }

        // Grow the list well past a 5-row window and walk the cursor down.
        state.palaces = (0..40)
            .map(|n| PalaceRow {
                id: format!("p-{n}"),
                name: format!("palace-{n}"),
                vector_count: 1,
                ..Default::default()
            })
            .collect();
        let window = 5;
        for row in 0..=state.last_row() {
            state.selected = row;
            state.sync_scroll(window);
            assert!(
                row >= state.scroll_offset && row < state.scroll_offset + window,
                "row {row} must be inside [{}, {})",
                state.scroll_offset,
                state.scroll_offset + window,
            );
        }
        // The cursor at the bottom pins the window against the list end.
        assert_eq!(state.scroll_offset, state.last_row() + 1 - window);

        // Walking back up drags the window up with the cursor.
        for row in (0..=state.last_row()).rev() {
            state.selected = row;
            state.sync_scroll(window);
            assert!(
                row >= state.scroll_offset && row < state.scroll_offset + window,
                "row {row} must stay visible while scrolling up",
            );
        }
        assert_eq!(state.scroll_offset, 0, "back at the top");
    }

    #[test]
    fn test_visible_palace_ids() {
        // Visible ids lead with the "All" sentinel, then follow the filtered +
        // sorted display order — not the original `state.palaces` order.
        let mut state = diverse_state();
        state.sort_key = PalaceSortKey::Name;
        let ids = visible_palace_ids(&state);
        assert_eq!(ids[0], tui_common::ALL_SENTINEL);
        // Alphabetical: claude-mpm, notes, trusty-memory, trusty-search.
        assert_eq!(
            &ids[1..],
            &[
                "claude-mpm".to_string(),
                "notes".to_string(),
                "trusty-memory".to_string(),
                "trusty-search".to_string(),
            ]
        );

        // A filter shrinks the visible list.
        state.filter = "trusty".into();
        let ids = visible_palace_ids(&state);
        assert_eq!(ids[0], tui_common::ALL_SENTINEL);
        assert_eq!(ids.len(), 3, "All + 2 trusty-* palaces");
    }

    #[test]
    fn test_navigate_visible() {
        // Navigation walks the visible (sorted) order, mapping back to
        // `state.selected` which indexes the original `state.palaces` array.
        let mut state = diverse_state();
        state.sort_key = PalaceSortKey::Name;
        // Visible order: All, claude-mpm, notes, trusty-memory, trusty-search.
        // Start at All.
        assert_eq!(state.selected, 0);
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("claude-mpm"));
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("notes"));
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("trusty-memory"));
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("trusty-search"));
        // At the bottom: another Down is a no-op.
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("trusty-search"));
        // Walk back up to All.
        navigate_up_visible(&mut state);
        assert_eq!(state.selected_id(), Some("trusty-memory"));
        navigate_up_visible(&mut state);
        navigate_up_visible(&mut state);
        navigate_up_visible(&mut state);
        assert!(state.is_all_selected());
        // At the top: Up is a no-op.
        navigate_up_visible(&mut state);
        assert!(state.is_all_selected());

        // With a filter, navigation skips hidden rows.
        state.filter = "trusty".into();
        state.selected = 0;
        navigate_down_visible(&mut state);
        // First visible after All is trusty-memory (alphabetical among trusty-*).
        assert_eq!(state.selected_id(), Some("trusty-memory"));
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("trusty-search"));
        navigate_down_visible(&mut state);
        // No more visible rows.
        assert_eq!(state.selected_id(), Some("trusty-search"));
    }

    #[test]
    fn test_visible_selected_row_follows_sort() {
        // The visible row index for the highlight must follow the rendered
        // (filter + sort) order, not the original `state.palaces` order.
        // Diverse palaces (in original order): trusty-search, trusty-memory,
        // claude-mpm, notes. Selecting "claude-mpm" places it at cursor 3
        // (index 2 + 1). With Name sort the displayed order is:
        //   0 All, 1 claude-mpm, 2 notes, 3 trusty-memory, 4 trusty-search
        // so the highlight must land on row 1, not row 3.
        let mut state = diverse_state();
        state.sort_key = PalaceSortKey::Name;
        let pos = state
            .palaces
            .iter()
            .position(|p| p.id == "claude-mpm")
            .expect("palace");
        state.selected = pos + 1;
        assert_eq!(state.selected, 3, "original index puts claude-mpm at 3");
        assert_eq!(
            visible_selected_row(&state),
            1,
            "claude-mpm is the first non-All row after Name sort",
        );

        // "All" always sits at row 0 regardless of sort.
        state.selected = 0;
        assert_eq!(visible_selected_row(&state), 0);

        // With Vectors sort the displayed order is:
        //   0 All, 1 claude-mpm (6163), 2 trusty-memory (3775),
        //   3 notes (100), 4 trusty-search (12)
        // so notes must land on row 3.
        state.sort_key = PalaceSortKey::Count;
        let pos = state
            .palaces
            .iter()
            .position(|p| p.id == "notes")
            .expect("palace");
        state.selected = pos + 1;
        assert_eq!(visible_selected_row(&state), 3);
    }

    #[test]
    fn test_visible_selected_row_follows_group() {
        // Grouping interleaves project headers (non-selectable) with palaces;
        // the highlight row must skip over them and follow the grouped layout.
        let mut state = diverse_state();
        state.sort_key = PalaceSortKey::Name;
        state.group_by_project = true;
        // Select "trusty-memory". The row layout starts with All, then the
        // first project header, then its palace rows; the exact row index
        // must match the position palace_lines marks as `selected`.
        let pos = state
            .palaces
            .iter()
            .position(|p| p.id == "trusty-memory")
            .expect("palace");
        state.selected = pos + 1;
        let expected = palace_lines(&state)
            .iter()
            .position(|row| row.selected)
            .expect("trusty-memory must appear in the grouped layout");
        assert_eq!(visible_selected_row(&state), expected);
        assert!(expected > 0, "highlight is not on the All row");
    }

    #[test]
    fn test_sync_scroll_to_follows_sorted_order() {
        // sync_scroll_to anchors the viewport on the *visible* row, so a
        // selection deep in the sorted list scrolls the window down even
        // when state.selected refers to a low index in the original Vec.
        let mut state = diverse_state();
        state.sort_key = PalaceSortKey::Name;
        // Visible order: All(0), claude-mpm(1), notes(2),
        //                trusty-memory(3), trusty-search(4).
        // Select trusty-search (original index 0 → state.selected = 1).
        state.selected = 1;
        let visible_row = visible_selected_row(&state);
        assert_eq!(visible_row, 4, "trusty-search is the last visible row");
        // A 3-row window must scroll so row 4 fits: offset = 4 + 1 - 3 = 2.
        state.sync_scroll_to(visible_row, 3);
        assert_eq!(state.scroll_offset, 2);
    }

    #[test]
    fn test_clamp_to_visible() {
        // When the filter hides the selected palace, clamp_to_visible drops
        // back to the "All" row so arrows resume from a visible position.
        let mut state = diverse_state();
        state.sort_key = PalaceSortKey::Name;
        // Select "claude-mpm" (cursor 3 in original order).
        let pos = state
            .palaces
            .iter()
            .position(|p| p.id == "claude-mpm")
            .expect("palace");
        state.selected = pos + 1;
        // Apply a filter that excludes it.
        state.filter = "trusty".into();
        state.clamp_to_visible();
        assert_eq!(state.selected, 0, "selection dropped to All");

        // When the selection is still visible, clamp_to_visible leaves it.
        state.filter = "trusty".into();
        let pos = state
            .palaces
            .iter()
            .position(|p| p.id == "trusty-memory")
            .expect("palace");
        state.selected = pos + 1;
        state.clamp_to_visible();
        assert_eq!(state.selected_id(), Some("trusty-memory"));
    }

    #[test]
    fn test_render_smoke() {
        // A full render in several states must not panic — exercise both the
        // "All" selection (aggregated panels) and a single-palace selection.
        let mut state = sample_state();
        state.log.push("SSE: dream_completed");
        state
            .log
            .push_scoped("default", "recall \"auth flow\" → 3 results");
        state.input = "auth flow".into();
        state.focus = MemoryFocus::Input;
        for (w, h) in [(120u16, 30u16), (80, 24)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            terminal
                .draw(|f| render(f, &mut state))
                .expect("render (All) must not panic");
        }
        // Single-palace selection — the right panels scope to that palace.
        state.selected = 1;
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| render(f, &mut state))
            .expect("render (single palace) must not panic");

        // A list far longer than the panel must render (and scroll) cleanly.
        state.palaces = (0..60)
            .map(|n| PalaceRow {
                id: format!("p-{n}"),
                name: format!("palace-{n}"),
                vector_count: 100,
                ..Default::default()
            })
            .collect();
        state.selected = state.last_row();
        let backend = TestBackend::new(120, 20);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| render(f, &mut state))
            .expect("overflowing list render must not panic");
        assert!(state.scroll_offset > 0, "long list scrolled to the cursor");

        state.show_help = true;
        state.daemon_status = DaemonStatus::Connecting;
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| render(f, &mut state))
            .expect("help render must not panic");
    }

    #[test]
    fn test_palace_activity_state() {
        use chrono::{TimeZone, Utc};
        let now = Utc.with_ymd_and_hms(2026, 5, 22, 12, 0, 0).unwrap();

        // is_compacting wins over recency.
        let mut p = PalaceRow {
            id: "a".into(),
            name: "a".into(),
            vector_count: 1,
            is_compacting: true,
            ..Default::default()
        };
        assert_eq!(palace_activity_state(&p, now), PalaceActivity::Dreaming);

        // Fresh write (< 10s) → Indexing.
        p.is_compacting = false;
        p.last_write_at = Some(now - chrono::Duration::seconds(3));
        assert_eq!(palace_activity_state(&p, now), PalaceActivity::Indexing);

        // 10s ≤ delta < 60s → Active.
        p.last_write_at = Some(now - chrono::Duration::seconds(30));
        assert_eq!(palace_activity_state(&p, now), PalaceActivity::Active);

        // ≥ 60s → Idle.
        p.last_write_at = Some(now - chrono::Duration::seconds(120));
        assert_eq!(palace_activity_state(&p, now), PalaceActivity::Idle);

        // Never-written palace → Idle.
        p.last_write_at = None;
        assert_eq!(palace_activity_state(&p, now), PalaceActivity::Idle);

        // Spinner prefix glyphs cycle deterministically.
        assert_eq!(PalaceActivity::Idle.prefix(0), ' ');
        assert_eq!(PalaceActivity::Active.prefix(0), '⠿');
        assert_eq!(PalaceActivity::Error.prefix(0), '✗');
        let i0 = PalaceActivity::Indexing.prefix(0);
        let i1 = PalaceActivity::Indexing.prefix(1);
        assert_ne!(i0, i1, "indexing spinner advances per tick");
        let d0 = PalaceActivity::Dreaming.prefix(0);
        let d1 = PalaceActivity::Dreaming.prefix(1);
        assert_ne!(d0, d1, "dreaming spinner advances per tick");

        // Colour mapping.
        assert_eq!(PalaceActivity::Idle.color(), None);
        assert_eq!(PalaceActivity::Indexing.color(), Some(Color::Yellow));
        assert_eq!(PalaceActivity::Active.color(), Some(Color::Cyan));
        assert_eq!(PalaceActivity::Dreaming.color(), Some(Color::Magenta));
        assert_eq!(PalaceActivity::Error.color(), Some(Color::Red));
    }

    #[test]
    fn test_filter_empty_palaces() {
        // A palace is hidden only when ALL of vector_count, kg_triple_count,
        // and drawer_count are zero. A palace with drawers but no vectors (e.g.
        // memories stored but not yet embedded) must remain visible — this was
        // the root cause of the claude-mpm palace not appearing in the TUI.
        let mut state = MemoryTuiState::new("http://x");
        state.palaces = vec![
            PalaceRow {
                id: "vec-only".into(),
                name: "vec-only".into(),
                vector_count: 10,
                ..Default::default()
            },
            PalaceRow {
                id: "kg-only".into(),
                name: "kg-only".into(),
                kg_triple_count: 5,
                ..Default::default()
            },
            PalaceRow {
                id: "drawer-only".into(),
                name: "drawer-only".into(),
                drawer_count: 18,
                ..Default::default()
            },
            PalaceRow {
                id: "empty".into(),
                name: "empty".into(),
                ..Default::default()
            },
        ];
        let visible = filtered_sorted_palaces(&state);
        assert_eq!(visible.len(), 3, "only truly empty palace dropped");
        assert!(visible.iter().any(|p| p.id == "vec-only"));
        assert!(visible.iter().any(|p| p.id == "kg-only"));
        assert!(
            visible.iter().any(|p| p.id == "drawer-only"),
            "drawer-only palace must be visible (has stored memories, not yet embedded)"
        );
        assert!(!visible.iter().any(|p| p.id == "empty"));

        // palace_lines reflects the same filter.
        let rows = palace_lines(&state);
        assert!(!rows.iter().any(|r| r.text.contains("empty")));
        assert!(rows.iter().any(|r| r.text.contains("drawer-o")));
    }

    #[test]
    fn test_palace_row_with_activity() {
        let p = PalaceRow {
            id: "default".into(),
            name: "default".into(),
            vector_count: 8_400,
            ..Default::default()
        };
        // Indexing spinner glyph leads the row.
        let row = palace_row_with_activity(&p, PalaceActivity::Indexing, 0);
        assert_eq!(row.chars().next(), Some(INDEXING_SPINNER[0]));
        assert!(row.contains("default"));
        assert!(row.contains("8,400v"));

        // Indented variant leads with a space, then the spinner.
        let ind = palace_row_indented_with_activity(&p, PalaceActivity::Active, 0);
        assert!(ind.starts_with(' '));
        assert!(ind.contains('⠿'));
        assert!(ind.contains("default"));
    }

    #[test]
    fn test_palace_lines_activity() {
        use chrono::{TimeZone, Utc};
        let now = Utc.with_ymd_and_hms(2026, 5, 22, 12, 0, 0).unwrap();
        let mut state = MemoryTuiState::new("http://x");
        state.palaces = vec![
            PalaceRow {
                id: "indexing".into(),
                name: "indexing".into(),
                vector_count: 1,
                last_write_at: Some(now - chrono::Duration::seconds(2)),
                ..Default::default()
            },
            PalaceRow {
                id: "dreaming".into(),
                name: "dreaming".into(),
                vector_count: 1,
                is_compacting: true,
                ..Default::default()
            },
        ];
        let rows = palace_lines_at(&state, now, 0);
        // Row 0 = All (no activity), then the two palaces with activity.
        assert_eq!(rows[0].activity, None);
        assert_eq!(rows[1].activity, Some(PalaceActivity::Indexing));
        assert_eq!(rows[2].activity, Some(PalaceActivity::Dreaming));
    }

    #[test]
    fn test_stats_graph_section() {
        use chrono::{TimeZone, Utc};
        let mut state = MemoryTuiState::new("http://x");
        // Drop out of the Connecting → "Loading…" early return so the full
        // graph section is rendered.
        state.daemon_status = DaemonStatus::Online {
            version: "0.1.54".into(),
            uptime_secs: 0,
        };
        state.palaces = vec![PalaceRow {
            id: "p1".into(),
            name: "p1".into(),
            vector_count: 1_234,
            kg_triple_count: 567,
            node_count: 4_321,
            edge_count: 12_345,
            community_count: 7,
            last_write_at: Some(Utc.with_ymd_and_hms(2026, 5, 22, 11, 59, 50).unwrap()),
            ..Default::default()
        }];
        state.selected = 1; // single palace
        let lines = stats_lines(&state);
        let joined = lines.join("\n");
        assert!(joined.contains("Knowledge Graph"));
        assert!(joined.contains("Nodes:"));
        assert!(joined.contains("4,321"));
        assert!(joined.contains("Edges:"));
        assert!(joined.contains("12.3k"));
        assert!(joined.contains("Triples:"));
        assert!(joined.contains("567"));
        assert!(joined.contains("Last write:"));
        assert!(joined.contains("State:"));
    }

    #[test]
    fn test_format_relative_time() {
        use chrono::{TimeZone, Utc};
        let now = Utc.with_ymd_and_hms(2026, 5, 22, 12, 0, 0).unwrap();
        assert_eq!(
            format_relative_time(now, now - chrono::Duration::seconds(1)),
            "just now"
        );
        assert_eq!(
            format_relative_time(now, now - chrono::Duration::seconds(30)),
            "30s ago"
        );
        assert_eq!(
            format_relative_time(now, now - chrono::Duration::minutes(2)),
            "2m ago"
        );
        assert_eq!(
            format_relative_time(now, now - chrono::Duration::hours(5)),
            "5h ago"
        );
        assert_eq!(
            format_relative_time(now, now - chrono::Duration::days(3)),
            "3d ago"
        );
        // Future timestamps (clock skew) clamp to "just now".
        assert_eq!(
            format_relative_time(now, now + chrono::Duration::seconds(10)),
            "just now"
        );
    }

    #[test]
    fn test_spinner_tick_returns_value() {
        // Sanity check the call surface; the value itself is non-deterministic.
        let _t = spinner_tick();
    }

    #[test]
    fn dream_backoff_allows_first_attempt() {
        let backoff = DreamBackoff::new();
        assert!(backoff.ready(Instant::now()));
        assert_eq!(backoff.consecutive_failures(), 0);
        assert_eq!(backoff.remaining(Instant::now()), Duration::ZERO);
    }

    #[test]
    fn dream_backoff_blocks_within_window() {
        let mut backoff = DreamBackoff::new();
        let t0 = Instant::now();
        let logged = backoff.record_failure(t0);
        assert!(logged, "first failure must be loud");
        // Inside the window: blocked.
        assert!(!backoff.ready(t0 + Duration::from_secs(1)));
        // At/after the deadline: ready again.
        assert!(backoff.ready(t0 + DREAM_BACKOFF_INITIAL));
    }

    #[test]
    fn dream_backoff_remaining_reports_window() {
        let mut backoff = DreamBackoff::new();
        let t0 = Instant::now();
        backoff.record_failure(t0);
        let r = backoff.remaining(t0);
        // Should be close to the initial window (allow scheduler slop).
        assert!(r <= DREAM_BACKOFF_INITIAL && r > Duration::from_secs(0));
    }

    #[test]
    fn dream_backoff_resets_on_success() {
        let mut backoff = DreamBackoff::new();
        let t0 = Instant::now();
        backoff.record_failure(t0);
        backoff.record_failure(t0);
        assert_eq!(backoff.consecutive_failures(), 2);
        backoff.record_success();
        assert_eq!(backoff.consecutive_failures(), 0);
        assert!(backoff.ready(t0));
        // After a success, the next failure is logged again.
        assert!(backoff.record_failure(t0));
    }

    #[test]
    fn dream_backoff_logs_only_first() {
        let mut backoff = DreamBackoff::new();
        let t0 = Instant::now();
        assert!(backoff.record_failure(t0), "first failure is loud");
        assert!(
            !backoff.record_failure(t0),
            "subsequent failures are suppressed"
        );
        assert!(
            !backoff.record_failure(t0),
            "still suppressed after several failures"
        );
    }

    #[test]
    fn dream_backoff_delay_doubles_and_caps() {
        // 5s, 10s, 20s, 40s, ... then capped at DREAM_BACKOFF_MAX.
        assert_eq!(backoff_delay(1), DREAM_BACKOFF_INITIAL);
        assert_eq!(backoff_delay(2), DREAM_BACKOFF_INITIAL * 2);
        assert_eq!(backoff_delay(3), DREAM_BACKOFF_INITIAL * 4);
        // High counts saturate at the ceiling.
        assert_eq!(backoff_delay(30), DREAM_BACKOFF_MAX);
        // n = 0 is treated as 1 to avoid a zero-duration window.
        assert_eq!(backoff_delay(0), DREAM_BACKOFF_INITIAL);
    }

    #[test]
    fn dream_backoff_doubles_then_caps() {
        let mut backoff = DreamBackoff::new();
        let t0 = Instant::now();
        // Walk several failures; the cooldown grows then stops growing.
        let mut last = Duration::ZERO;
        for _ in 0..10 {
            backoff.record_failure(t0);
            let r = backoff.remaining(t0);
            assert!(r >= last || r == DREAM_BACKOFF_MAX);
            last = r;
        }
        // Eventually clamped at the ceiling.
        assert!(last <= DREAM_BACKOFF_MAX);
    }

    // -----------------------------------------------------------------
    // Issue #184 — drawer list state + rendering
    // -----------------------------------------------------------------

    /// Build a [`DrawerInfo`] for tests with the given id index and tags.
    fn sample_drawer(idx: usize, tags: &[&str]) -> DrawerInfo {
        use chrono::{TimeZone, Utc};
        DrawerInfo {
            id: format!("{idx:08x}-aaaa-bbbb-cccc-dddddddddddd"),
            created_at: Some(
                Utc.with_ymd_and_hms(2026, 5, 1, 12, idx as u32 % 60, 0)
                    .unwrap(),
            ),
            creator: crate::monitor::memory_client::creator_label(
                &tags.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
            ),
            tags: tags.iter().map(|s| (*s).to_string()).collect(),
            snippet: None,
        }
    }

    /// Build a [`DrawerInfo`] for tests with an explicit snippet (issue #202).
    fn sample_drawer_with_snippet(idx: usize, tags: &[&str], snippet: &str) -> DrawerInfo {
        let mut d = sample_drawer(idx, tags);
        d.snippet = Some(snippet.to_string());
        d
    }

    #[test]
    fn drawer_state_default_page_size() {
        // A freshly-constructed state holds no rows, no scope, offset 0.
        let state = DrawerListState::new();
        assert!(state.palace_id.is_none());
        assert!(state.drawers.is_empty());
        assert_eq!(state.offset, 0);
        assert!(!state.loading);
        assert!(state.last_error.is_none());
        assert_eq!(state.page(), 0);
        // The page-size constant is the contract the panel renders against.
        assert_eq!(DRAWER_PAGE_SIZE, 20);
    }

    #[test]
    fn drawer_state_reset_on_palace_change() {
        let mut state = DrawerListState {
            palace_id: Some("old".into()),
            drawers: vec![sample_drawer(1, &[])],
            offset: 40,
            loading: false,
            last_error: Some("stale".into()),
        };
        state.reset_for(Some("new".into()));
        assert_eq!(state.palace_id.as_deref(), Some("new"));
        assert!(state.drawers.is_empty());
        assert_eq!(state.offset, 0);
        assert!(state.loading, "should mark loading after reset");
        assert!(state.last_error.is_none());

        // Resetting to None (e.g. "All palaces") is also valid.
        state.reset_for(None);
        assert!(state.palace_id.is_none());
    }

    #[test]
    fn drawer_state_pagination() {
        let mut state = DrawerListState::new();
        // Fill a full page so next_page() advances.
        state.drawers = (0..DRAWER_PAGE_SIZE)
            .map(|i| sample_drawer(i, &[]))
            .collect();
        state.next_page();
        assert_eq!(state.offset, DRAWER_PAGE_SIZE);
        assert_eq!(state.page(), 1);
        assert!(state.loading);

        // prev_page() walks back one page.
        state.loading = false;
        state.prev_page();
        assert_eq!(state.offset, 0);
        assert_eq!(state.page(), 0);
        assert!(state.loading);

        // prev_page() at offset 0 is a no-op (does not flip loading).
        state.loading = false;
        state.prev_page();
        assert_eq!(state.offset, 0);
        assert!(!state.loading);

        // A short last page (< DRAWER_PAGE_SIZE) blocks further advancement.
        state.drawers = vec![sample_drawer(0, &[])];
        state.next_page();
        assert_eq!(
            state.offset, 0,
            "end-of-list page should not advance past last",
        );
    }

    #[test]
    fn drawer_row_layout() {
        // Compact one-line row: <id8> <MM-DD HH:MM>  <creator>.
        let drawer = sample_drawer(0xab, &["msg:from=cto"]);
        let row = format_drawer_row(&drawer);
        // The id is truncated to 8 chars (the leading UUID block, with a
        // trailing `…` when cut by the shared truncate helper).
        assert!(
            row.starts_with("000000a…") || row.starts_with("000000ab"),
            "row should start with truncated id, got: {row}",
        );
        // Timestamp shape (MM-DD HH:MM).
        assert!(row.contains("05-01"), "row should carry MM-DD: {row}");
        // Creator tag is preserved verbatim when it fits.
        assert!(row.contains("msg:from=cto"), "creator missing: {row}");

        // No-creator drawer renders the em-dash placeholder.
        let bare = sample_drawer(1, &[]);
        let row = format_drawer_row(&bare);
        assert!(
            row.contains("—"),
            "missing em-dash for no-creator row: {row}"
        );

        // No timestamp falls back to `--`.
        let mut undated = sample_drawer(2, &[]);
        undated.created_at = None;
        let row = format_drawer_row(&undated);
        assert!(row.contains("--"), "missing `--` for undated row: {row}");
    }

    /// Why (issue #202): when the daemon returns a `snippet`, the row
    /// must append it after the creator column so the operator sees a
    /// glanceable preview of the drawer body without opening it. When
    /// the snippet is absent the row must collapse back to the legacy
    /// layout — no trailing separator, no padding artefact.
    /// What: builds drawers with and without snippets and asserts both
    /// the appended-snippet and absent-snippet shapes.
    /// Test: itself.
    #[test]
    fn drawer_row_includes_snippet() {
        // Snippet is appended inline after the creator column.
        let with_snippet =
            sample_drawer_with_snippet(3, &["msg:from=cto"], "JWT middleware added to auth flow");
        let row = format_drawer_row(&with_snippet);
        assert!(
            row.contains("msg:from=cto"),
            "creator must still appear before snippet: {row}",
        );
        assert!(
            row.contains("JWT middleware added to auth flow"),
            "snippet must be appended: {row}",
        );

        // No snippet → row falls back to the legacy `<id> <ts> <creator>`
        // shape with no trailing whitespace.
        let bare = sample_drawer(4, &["msg:from=cto"]);
        let row = format_drawer_row(&bare);
        assert!(
            !row.ends_with("  "),
            "no-snippet row must not have trailing whitespace: {row:?}",
        );

        // Empty / whitespace-only snippet is treated as absent so the
        // row stays at the legacy width.
        let empty = sample_drawer_with_snippet(5, &["msg:from=cto"], "   ");
        let row = format_drawer_row(&empty);
        assert!(
            !row.ends_with("  "),
            "whitespace-only snippet must be elided: {row:?}",
        );

        // A long snippet is truncated to fit the column.
        let long = "x".repeat(200);
        let big = sample_drawer_with_snippet(6, &["msg:from=cto"], &long);
        let row = format_drawer_row(&big);
        assert!(
            row.contains('…'),
            "long snippet must be truncated with `…`: {row}",
        );
    }

    #[test]
    fn drawer_panel_lines_renders_no_palace() {
        // No palace scope → empty lines (renderer falls back to the log).
        let state = sample_state();
        assert!(state.drawer_list.palace_id.is_none());
        let lines = drawer_panel_lines(&state, 0);
        assert!(lines.is_empty(), "no-scope path should render no lines");
    }

    #[test]
    fn drawer_panel_lines_renders_loading_then_rows() {
        let mut state = sample_state();
        state.drawer_list.palace_id = Some("default".into());
        state.drawer_list.loading = true;
        let lines = drawer_panel_lines(&state, 0);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("loading"));

        // Once rows arrive the header surfaces a page indicator.
        state.drawer_list.loading = false;
        state.drawer_list.drawers = vec![
            sample_drawer(1, &["msg:from=cto"]),
            sample_drawer(2, &["creator:client=mpm"]),
        ];
        let lines = drawer_panel_lines(&state, 14);
        assert_eq!(lines.len(), 3, "header + 2 rows");
        assert!(lines[0].contains("drawers 1–2"));
        assert!(lines[0].contains("page 1"));
        assert!(lines[1].contains("msg:from=cto"));
        assert!(lines[2].contains("creator:client=mpm"));
    }

    #[test]
    fn drawer_panel_lines_renders_error() {
        let mut state = sample_state();
        state.drawer_list.palace_id = Some("default".into());
        state.drawer_list.loading = false;
        state.drawer_list.last_error = Some("connection refused".into());
        let lines = drawer_panel_lines(&state, 0);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("drawers unavailable"));
        assert!(lines[0].contains("connection refused"));
    }

    // -----------------------------------------------------------------
    // Issue #215 — Tab focus cycle, drawer pane, and detail modal
    // -----------------------------------------------------------------

    /// Why (issue #215): `Tab` must walk the three focus zones in order
    /// (`List → DrawerPane → Input → List`) so the operator can reach the
    /// new drawer pane without a mouse.
    /// What: drives `MemoryFocus::next` through a full loop.
    /// Test: itself.
    #[test]
    fn test_focus_tab_cycle() {
        assert_eq!(MemoryFocus::default(), MemoryFocus::List);
        let mut focus = MemoryFocus::List;
        focus = focus.next();
        assert_eq!(focus, MemoryFocus::DrawerPane);
        focus = focus.next();
        assert_eq!(focus, MemoryFocus::Input);
        focus = focus.next();
        assert_eq!(focus, MemoryFocus::List);
    }

    /// Why (issue #215): `cycle_focus` on the state must reset the drawer
    /// cursor when focus returns to the palace list so a re-entry doesn't
    /// resume on a stale highlight.
    /// What: drives the cycle, asserting `drawer_cursor` clears on the
    /// `Input → List` step.
    /// Test: itself.
    #[test]
    fn test_state_cycle_focus_resets_drawer_cursor() {
        let mut state = sample_state();
        state.drawer_cursor = 5;
        state.cycle_focus(); // List → DrawerPane
        assert_eq!(state.focus, MemoryFocus::DrawerPane);
        assert_eq!(state.drawer_cursor, 5, "cursor preserved while in pane");
        state.cycle_focus(); // DrawerPane → Input
        assert_eq!(state.focus, MemoryFocus::Input);
        assert_eq!(state.drawer_cursor, 5, "cursor preserved while away");
        state.cycle_focus(); // Input → List
        assert_eq!(state.focus, MemoryFocus::List);
        assert_eq!(state.drawer_cursor, 0, "cursor resets on return to list");
    }

    /// Why (issue #215): the drawer cursor must clamp to the visible drawer
    /// page so a page refresh / palace change can't leave the cursor past
    /// the end of the slice.
    /// What: exercises `drawer_cursor_up`, `drawer_cursor_down`, and
    /// `clamp_drawer_cursor` across full / empty pages.
    /// Test: itself.
    #[test]
    fn test_drawer_cursor_clamp() {
        let mut state = sample_state();
        state.drawer_list.drawers = (0..3).map(|i| sample_drawer(i, &[])).collect();
        // From cursor 0, `Up` saturates at 0.
        state.drawer_cursor_up();
        assert_eq!(state.drawer_cursor, 0);
        // `Down` walks through the page and clamps at len - 1.
        state.drawer_cursor_down();
        state.drawer_cursor_down();
        state.drawer_cursor_down();
        state.drawer_cursor_down();
        assert_eq!(state.drawer_cursor, 2, "clamped at last index");
        // A shrunk page re-clamps the cursor.
        state.drawer_list.drawers.truncate(1);
        state.clamp_drawer_cursor();
        assert_eq!(state.drawer_cursor, 0, "clamped to new last index");
        // Empty page leaves the cursor at 0.
        state.drawer_list.drawers.clear();
        state.drawer_cursor = 5;
        state.clamp_drawer_cursor();
        assert_eq!(state.drawer_cursor, 0);
        // `Down` on empty page is a no-op.
        state.drawer_cursor_down();
        assert_eq!(state.drawer_cursor, 0);
    }

    /// Why (issue #215): the modal lifecycle must clear transient state so
    /// a re-open does not flash stale memories.
    /// What: opens the modal with a fake memory set, then closes it via
    /// `close_drawer_detail`, asserting every transient field clears.
    /// Test: itself.
    #[test]
    fn test_drawer_detail_modal_lifecycle() {
        let mut state = sample_state();
        state.drawer_detail_open = true;
        state.drawer_detail_idx = 3;
        state.drawer_detail_scroll = 17;
        state.drawer_detail_loading = true;
        state.drawer_detail_memories = vec![MemoryDetail {
            id: "x".into(),
            content: "y".into(),
            tags: vec![],
            created_at: None,
        }];
        state.close_drawer_detail();
        assert!(!state.drawer_detail_open);
        assert!(state.drawer_detail_memories.is_empty());
        assert_eq!(state.drawer_detail_scroll, 0);
        assert!(!state.drawer_detail_loading);
        // `drawer_detail_idx` is preserved on close (not part of the
        // explicit reset) — the next open recomputes it from the cursor.
    }

    /// Why (issue #215): the modal body must surface the drawer header
    /// fields (id, timestamp, creator, tags) plus the verbatim content so
    /// the operator sees the full memory.
    /// What: builds a state with two memories and asserts the body carries
    /// both, separated by a horizontal rule.
    /// Test: itself.
    #[test]
    fn test_drawer_detail_body_layout() {
        use chrono::{TimeZone, Utc};
        let mut state = sample_state();
        state.drawer_detail_memories = vec![
            MemoryDetail {
                id: "abc-123".into(),
                content: "First memory body".into(),
                tags: vec!["msg:from=cto".into(), "tag:type=note".into()],
                created_at: Some(Utc.with_ymd_and_hms(2026, 5, 20, 12, 34, 56).unwrap()),
            },
            MemoryDetail {
                id: "def-456".into(),
                content: "Second memory body".into(),
                tags: vec![],
                created_at: None,
            },
        ];
        let body = drawer_detail_body(&state);
        // Header fields present for memory 0.
        assert!(
            body.contains("Drawer: abc-123"),
            "missing id header: {body}"
        );
        assert!(body.contains("2026-05-20 12:34:56 UTC"));
        assert!(body.contains("msg:from=cto"));
        assert!(body.contains("tag:type=note"));
        // First memory body present.
        assert!(body.contains("First memory body"));
        // Separator between memories.
        assert!(
            body.contains("──────────────────────────────────────"),
            "missing memory separator: {body}",
        );
        // Memory 1 falls through to safe defaults for missing fields.
        assert!(body.contains("Drawer: def-456"));
        assert!(body.contains("(no timestamp)"));
        assert!(body.contains("(none)"));
        assert!(body.contains("Second memory body"));
    }

    /// Why (issue #215): the modal must show a `Loading…` placeholder while
    /// the fetch is in flight, and a friendly empty-state message when the
    /// fetch returned no memories.
    /// What: drives both transient states through `drawer_detail_body`.
    /// Test: itself.
    #[test]
    fn test_drawer_detail_body_loading() {
        let mut state = sample_state();
        state.drawer_detail_loading = true;
        assert_eq!(drawer_detail_body(&state), "Loading…");
        state.drawer_detail_loading = false;
        // Memories vec is still empty -> empty-state placeholder.
        assert_eq!(drawer_detail_body(&state), "(no memories returned)");
    }

    /// Why (issue #215): the modal title must show how many memories are
    /// loaded and which index the operator is viewing, plus a hint at the
    /// close / scroll bindings.
    /// What: exercises every title state — loading, empty, and populated.
    /// Test: itself.
    #[test]
    fn test_drawer_detail_title() {
        let mut state = sample_state();
        state.drawer_detail_loading = true;
        assert!(drawer_detail_title(&state).contains("loading…"));
        state.drawer_detail_loading = false;
        assert!(drawer_detail_title(&state).contains("(empty)"));
        state.drawer_detail_memories = vec![
            MemoryDetail::default(),
            MemoryDetail::default(),
            MemoryDetail::default(),
        ];
        state.drawer_detail_idx = 1;
        let title = drawer_detail_title(&state);
        assert!(title.contains("2/3"), "expected 2/3 index marker: {title}");
        assert!(title.contains("esc/q close"));
        assert!(title.contains("↑/↓ scroll"));
    }

    /// Why (issue #215): the activity panel title must surface a `DRAWER ▶`
    /// marker when the drawer pane has focus so the operator sees which
    /// zone owns the cursor.
    /// What: renders the TUI with focus on the drawer pane and asserts the
    /// title carries the marker.
    /// Test: itself.
    #[test]
    fn test_render_drawer_pane_focused_title() {
        let mut state = sample_state();
        state.selected = 1; // single palace
        state.focus = MemoryFocus::DrawerPane;
        state.drawer_list.palace_id = Some("default".into());
        state.drawer_list.drawers = vec![sample_drawer(0, &["msg:from=cto"])];
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| render(f, &mut state))
            .expect("render with drawer focus must not panic");
        let buffer = terminal.backend().buffer();
        let content: String = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            content.contains("DRAWER ▶"),
            "expected DRAWER ▶ marker in rendered output",
        );
    }

    /// Why (issue #215): the modal must render without panicking when open
    /// over the existing layout, and the rendered output must include the
    /// modal title.
    /// What: opens the modal with a fake memory and asserts the title is
    /// visible in the rendered buffer.
    /// Test: itself.
    #[test]
    fn test_render_with_drawer_detail_open() {
        use chrono::{TimeZone, Utc};
        let mut state = sample_state();
        state.selected = 1;
        state.drawer_detail_open = true;
        state.drawer_detail_idx = 0;
        state.drawer_detail_memories = vec![MemoryDetail {
            id: "abc-123".into(),
            content: "Verbatim memory body for the modal".into(),
            tags: vec!["msg:from=cto".into()],
            created_at: Some(Utc.with_ymd_and_hms(2026, 5, 20, 12, 34, 56).unwrap()),
        }];
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| render(f, &mut state))
            .expect("render with modal open must not panic");
        let buffer = terminal.backend().buffer();
        let content: String = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            content.contains("Drawer detail"),
            "expected modal title in rendered output",
        );
    }

    #[test]
    fn drawer_panel_lines_renders_empty_palace() {
        let mut state = sample_state();
        state.drawer_list.palace_id = Some("default".into());
        state.drawer_list.loading = false;
        let lines = drawer_panel_lines(&state, 0);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("no drawers yet"));
    }
}
