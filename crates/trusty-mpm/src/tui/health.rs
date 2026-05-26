//! Combined search + memory health screen (`[2]`) for the trusty-mpm TUI.
//!
//! Why: operators want one glance to confirm that the two daemons the
//! coordinator depends on — trusty-search and trusty-memory — are alive and
//! healthy, without leaving the TUI to run two `status` commands. Keeping the
//! poller, the typed wire shapes, and the pure rendering helpers here (away
//! from the coordinator chat in `dashboard.rs`) keeps both surfaces small and
//! independently testable.
//! What: [`HealthClient`] is a typed `reqwest` transport for one daemon's
//! `/health` + list endpoints; [`PanelData`] is the projected per-daemon
//! payload; [`PanelState`] is `Connecting` / `Online` / `Offline`;
//! [`HealthScreen`] holds both panels plus focus and renders the side-by-side
//! layout. A background tokio task drives polling and pushes [`HealthUpdate`]s
//! down a channel into the TUI event loop.
//! Test: `cargo test -p trusty-mpm-tui` covers the wire projections, panel
//! line building, the focus toggle, and a `TestBackend` render smoke test.

use std::time::Duration;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem, ListState, Paragraph},
};
use serde::Deserialize;

/// Default trusty-search daemon address used when no override is supplied.
///
/// Why: the health screen must always have a target to probe; the search
/// daemon binds `127.0.0.1:7878` by convention.
/// What: the canonical local trusty-search HTTP base URL.
/// Test: `default_urls_are_local`.
pub const DEFAULT_SEARCH_URL: &str = "http://127.0.0.1:7878";

/// Default trusty-memory daemon address used when no override is supplied.
///
/// Why: mirrors [`DEFAULT_SEARCH_URL`]; the memory daemon's health endpoint is
/// reached at `127.0.0.1:7990` for the monitor surface.
/// What: the canonical local trusty-memory HTTP base URL.
/// Test: `default_urls_are_local`.
pub const DEFAULT_MEMORY_URL: &str = "http://127.0.0.1:7990";

/// Interval between health polls for each panel.
///
/// Why: the ticket mandates a 5-second refresh cadence for both the online and
/// the offline (retry) paths.
/// What: five seconds.
/// Test: exercised indirectly by the background poller.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Per-request timeout for a daemon health probe.
///
/// Why: a hung daemon must not stall the poll task; a short timeout turns an
/// unresponsive daemon into a clean "offline" state on the next tick.
/// What: three seconds, comfortably above a healthy local round-trip.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Which daemon a panel (or a poll result) refers to.
///
/// Why: the background poller probes two daemons and the event loop must route
/// each [`HealthUpdate`] to the correct panel; a typed tag keeps that routing
/// exhaustive.
/// What: `Search` for trusty-search, `Memory` for trusty-memory.
/// Test: `toggle_focus_cycles_panels`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Daemon {
    /// The trusty-search daemon.
    Search,
    /// The trusty-memory daemon.
    Memory,
}

/// Maximum number of buffered log lines kept per service.
///
/// Why: the Logs tab is a ring buffer of the most recent daemon log lines; a
/// fixed cap keeps memory bounded on long sessions.
/// What: 200 lines — wide enough to scroll through recent activity, small
/// enough to redraw quickly.
/// Test: `log_buffer_evicts_oldest`.
pub const LOG_BUFFER_CAP: usize = 200;

/// Which right-panel tab is currently active.
///
/// Why: the redesign in issue #36 puts the per-service detail behind three
/// tabs (`[1]HEALTH [2]LOGS [3]SEARCH`); a typed enum keeps tab-switch and
/// render dispatch exhaustive.
/// What: `Health` shows resource gauges + config, `Logs` shows a scrollable
/// log tail, `Search` shows a query input + results, `Index` shows
/// per-collection stats (graph + communities) for the selected row.
/// Test: `tab_default_is_health`, `tab_switch_keys_route`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HealthTab {
    /// The resource / config view.
    #[default]
    Health,
    /// The log-tail view.
    Logs,
    /// The interactive search/recall view.
    Search,
    /// The per-index stats view (graph + communities for the selected row).
    Index,
}

/// One row in the left-panel collections list.
///
/// Why: the redesigned screen surfaces the service's collections (search
/// indexes) or palaces (memory) so the operator can see each one's status at
/// a glance and drill into it.
/// What: a display id, an item count (chunks or vectors), and a one-line
/// status note (e.g. `indexed 2m ago`, `reindexing 42%…`, `error: …`).
/// Test: `collection_row_default_is_empty`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CollectionRow {
    /// Display id (index id or palace name).
    pub id: String,
    /// Item count — chunks for search, vectors for memory.
    pub count: u64,
    /// One-line status note rendered after the count.
    pub note: String,
    /// Whether this row currently looks healthy (`true` shows `✓`, false `✗`).
    pub ok: bool,
    /// RFC 3339 timestamp of the most recent index write, if any.
    ///
    /// Why: the left panel renders a `[Xh ago]` badge per row so operators can
    /// spot stale indexes at a glance.
    /// What: the `last_indexed` field from `GET /indexes/:id/status`; `None`
    /// when the daemon has never indexed (or when the field is absent).
    /// Test: `format_relative_time_handles_known_offsets`,
    /// `collections_lines_show_relative_time`.
    pub last_indexed: Option<String>,
    /// Symbol graph node count for the index (zero for memory palaces).
    ///
    /// Why: the INDEX tab surfaces graph stats for the highlighted row.
    /// What: the `node_count` field from `GET /indexes/:id/graph/stats`.
    /// Test: `index_tab_lines_show_graph_stats`.
    pub node_count: u64,
    /// Symbol graph edge count for the index.
    pub edge_count: u64,
    /// Edge kinds sorted by count descending — `(kind, count)` pairs.
    ///
    /// Why: the INDEX tab draws a proportional bar per edge kind so the
    /// operator can see the graph's shape at a glance.
    /// What: the `edge_kinds` map from `GET /indexes/:id/graph/stats`
    /// projected into a sorted vec.
    /// Test: `index_tab_lines_show_edge_kind_bars`.
    pub edge_kinds: Vec<(String, u64)>,
    /// Community count from the index's KG community detection.
    pub community_count: u64,
    /// Modularity score (0..=1) from the community detection.
    pub modularity: f64,
    /// On-disk bytes for this collection (already in status payload).
    pub disk_bytes: u64,
    /// Whether the index carries a context embedding model.
    pub has_context_embedding: bool,
    /// KG triple count for memory palaces (zero for search collections).
    ///
    /// Why: the PALACES left panel surfaces both the vector count and the
    /// knowledge-graph triple count so the operator can see at a glance which
    /// palaces have graph data vs. only embeddings.
    /// What: the `kg_triple_count` field from `GET /api/v1/palaces`.
    /// Test: `project_palace_rows_reads_palaces`,
    /// `collections_lines_show_graph_count_for_memory`.
    pub kg_count: u64,
    /// Drawer count for memory palaces (zero for search collections).
    ///
    /// Why: the INDEX tab on memory focus surfaces drawer + wing counts as
    /// part of the palace's graph/storage stats; centralising the read on the
    /// row keeps the renderer pure.
    /// What: the `drawer_count` field from `GET /api/v1/palaces`.
    /// Test: `project_palace_rows_reads_palaces`.
    pub drawer_count: u64,
    /// Wing count for memory palaces (zero for search collections).
    ///
    /// Why: distinct rooms across drawers — surfaced in the INDEX detail panel.
    /// What: the `wing_count` field from `GET /api/v1/palaces`.
    /// Test: `project_palace_rows_reads_palaces`.
    pub wing_count: u64,
    /// RFC 3339 timestamp of the most recent palace write, if any.
    ///
    /// Why: drives the per-palace activity indicator (idle / active / indexing)
    /// in the left pane and the "Last write" row in the detail panel.
    /// What: the `last_write_at` field from `GET /api/v1/palaces`; `None` for
    /// search rows or when the palace has never been written.
    /// Test: `palace_activity_from_recent_write`,
    /// `project_palace_rows_reads_palaces`.
    pub last_write_at: Option<String>,
    /// `true` while the palace is being compacted by the dream cycle.
    ///
    /// Why: The MEMORY tab renders the dreaming spinner when a palace is in
    /// the middle of a Dreamer pass. Reading the signal off the row keeps
    /// the activity classifier pure and unit-testable.
    /// What: the `is_compacting` field from `GET /api/v1/palaces`; defaults
    /// to `false` when absent (older daemons) or for search rows.
    /// Test: `palace_activity_marks_compacting_as_dreaming`,
    /// `project_palace_rows_reads_is_compacting`.
    pub is_compacting: bool,
}

/// Activity state of a memory palace, derived from `last_write_at`.
///
/// Why: operators want to see at a glance which palaces are doing something
/// (being indexed, recently touched) vs. idle. A typed enum keeps the
/// derivation logic, the spinner mapping, and the colour mapping exhaustive
/// and unit-testable.
/// What: `Idle` is the default (no recent activity), `Indexing` covers very
/// recent writes (within 10s) where the palace is likely still flushing
/// vectors, `Active` covers writes within the last minute, `Dreaming` is
/// returned when the row's `is_compacting` flag is set (the daemon flips it
/// for the duration of every `Dreamer::dream_cycle`), and `Error` is set
/// when a row's `ok` flag is false.
/// Test: `palace_activity_from_recent_write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PalaceActivity {
    /// Palace exists but nothing is happening — no recent writes.
    #[default]
    Idle,
    /// Vectors are being built/updated (write within ~10s).
    Indexing,
    /// Palace compaction in progress (reserved for future API signal).
    Dreaming,
    /// Recently read/written (write within ~60s).
    Active,
    /// Row is in an error state (`ok == false`).
    Error,
}

/// Threshold below which a palace is considered actively indexing.
///
/// Why: a write timestamp newer than this almost certainly reflects an
/// in-flight ingestion path — the operator should see the spinner.
/// What: 10 seconds.
/// Test: `palace_activity_from_recent_write`.
const INDEXING_WINDOW_SECS: i64 = 10;

/// Threshold below which a palace is considered "recently active".
///
/// Why: writes within the last minute are still relevant to the operator
/// even if the ingestion path has finished; the cyan indicator highlights
/// the row without animating it.
/// What: 60 seconds.
/// Test: `palace_activity_from_recent_write`.
const ACTIVE_WINDOW_SECS: i64 = 60;

/// Frames of the indexing spinner (the canonical braille rotation).
///
/// Why: a rotating spinner communicates "this is changing right now" without
/// reading a label. The braille frames are the same set ratatui's `Throbber`
/// uses, kept inline here so the spinner stays self-contained.
/// What: ten frames cycled at ~10 Hz by the render loop.
/// Test: `spinner_frame_cycles_through_indexing_frames`.
const INDEXING_SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Frames of the dreaming/compacting spinner.
///
/// Why: a heavier glyph set distinguishes the compaction state from
/// indexing at a glance.
/// What: eight frames cycled at ~10 Hz by the render loop.
/// Test: `spinner_frame_cycles_through_dreaming_frames`.
const DREAMING_SPINNER: &[char] = &['⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'];

/// Derive a [`PalaceActivity`] from a collection row.
///
/// Why: the indicator/colour mapping needs one source of truth so the left
/// pane and the (future) detail panel agree on what each palace is doing.
/// What: returns `Error` for an unhealthy row, otherwise parses
/// `last_write_at` and bins the resulting age against the [`INDEXING_WINDOW_SECS`]
/// / [`ACTIVE_WINDOW_SECS`] thresholds. A missing or unparseable timestamp
/// yields `Idle`.
/// Test: `palace_activity_from_recent_write`.
pub fn palace_activity(row: &CollectionRow) -> PalaceActivity {
    if !row.ok {
        return PalaceActivity::Error;
    }
    // A live compaction wins over the write-recency heuristic: the dream
    // cycle is the explicit signal the dashboard cares most about.
    if row.is_compacting {
        return PalaceActivity::Dreaming;
    }
    let Some(ts) = row.last_write_at.as_deref() else {
        return PalaceActivity::Idle;
    };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return PalaceActivity::Idle;
    };
    let age_secs = chrono::Utc::now()
        .signed_duration_since(parsed.with_timezone(&chrono::Utc))
        .num_seconds();
    // Future / clock-skew timestamps are still treated as "right now".
    if age_secs < INDEXING_WINDOW_SECS {
        PalaceActivity::Indexing
    } else if age_secs < ACTIVE_WINDOW_SECS {
        PalaceActivity::Active
    } else {
        PalaceActivity::Idle
    }
}

/// Pick a spinner / indicator character for a palace activity state.
///
/// Why: the left pane prefixes each row with a single glyph; centralising
/// the lookup keeps the spinner-frame arithmetic in one place and lets the
/// renderer stay terse.
/// What: returns `None` for `Idle` (no prefix), `Some('✗')` for `Error`, a
/// static `Some('⠿')` for `Active`, and a rotating frame for `Indexing` /
/// `Dreaming` selected by `tick % frames.len()`.
/// Test: `spinner_frame_for_each_state`,
/// `spinner_frame_cycles_through_indexing_frames`.
pub fn spinner_frame(activity: PalaceActivity, tick: usize) -> Option<char> {
    match activity {
        PalaceActivity::Idle => None,
        PalaceActivity::Active => Some('⠿'),
        PalaceActivity::Error => Some('✗'),
        PalaceActivity::Indexing => Some(INDEXING_SPINNER[tick % INDEXING_SPINNER.len()]),
        PalaceActivity::Dreaming => Some(DREAMING_SPINNER[tick % DREAMING_SPINNER.len()]),
    }
}

/// Pick the colour for a palace activity state.
///
/// Why: alongside the glyph, each row carries an activity-driven colour so
/// the operator can scan the pane at a glance.
/// What: maps `Idle` to default (`Reset`), `Indexing` to yellow, `Dreaming`
/// to magenta, `Active` to cyan, and `Error` to red.
/// Test: `activity_colour_is_distinct_per_state`.
pub fn activity_color(activity: PalaceActivity) -> Color {
    match activity {
        PalaceActivity::Idle => Color::Reset,
        PalaceActivity::Indexing => Color::Yellow,
        PalaceActivity::Dreaming => Color::Magenta,
        PalaceActivity::Active => Color::Cyan,
        PalaceActivity::Error => Color::Red,
    }
}

/// Compute the current spinner-frame tick from wall-clock time.
///
/// Why: the render path is otherwise pure — passing a tick from the event
/// loop would require threading state through every call site. Deriving the
/// tick from wall time keeps the renderer self-contained while still
/// animating predictably.
/// What: returns the number of 100 ms slots elapsed since the unix epoch,
/// modulo a large constant so it stays a small `usize`. The render path
/// re-evaluates this every frame.
/// Test: covered indirectly by `spinner_frame_cycles_through_indexing_frames`
/// — the modular arithmetic is enough to ensure rotation.
fn current_spinner_tick() -> usize {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_millis() / 100) as usize)
        .unwrap_or(0)
}

/// Ring buffer of recently-observed log lines for one service.
///
/// Why: the Logs tab needs the last N lines and must drop the oldest when
/// full so memory cannot grow without bound; the tab also tracks a scroll
/// offset so the operator can hold position while new lines arrive.
/// What: a `VecDeque` capped at [`LOG_BUFFER_CAP`] plus an `auto_scroll` flag
/// and a `scroll_offset` (lines from the bottom).
/// Test: `log_buffer_evicts_oldest`, `log_buffer_scroll_clamps`.
#[derive(Debug, Clone, Default)]
pub struct LogBuffer {
    /// The line ring; oldest at the front, newest at the back.
    pub lines: std::collections::VecDeque<String>,
    /// Total lines ever observed (for the "showing N/M" footer).
    pub total_seen: u64,
    /// When `true`, the view follows the tail; any ↑/↓ press disables it.
    pub auto_scroll: bool,
    /// Lines scrolled up from the bottom; `0` == tail visible.
    pub scroll_offset: usize,
}

impl LogBuffer {
    /// Build an empty, auto-scrolling buffer.
    ///
    /// Why: a fresh service view starts following the tail with no history.
    /// What: empty deque, `auto_scroll = true`, `scroll_offset = 0`.
    /// Test: `log_buffer_starts_empty`.
    pub fn new() -> Self {
        Self {
            lines: std::collections::VecDeque::new(),
            total_seen: 0,
            auto_scroll: true,
            scroll_offset: 0,
        }
    }

    /// Replace the buffer's contents with a freshly-polled tail.
    ///
    /// Why: the Logs tab polls `/logs/tail?n=…` periodically; each response
    /// is the latest snapshot and replaces the buffer rather than appending,
    /// so missed lines while paused do not duplicate.
    /// What: clears the deque, pushes up to [`LOG_BUFFER_CAP`] of `new_lines`
    /// (keeping the newest), and updates `total_seen` to `total` when given.
    /// Test: `log_buffer_replace_caps_at_limit`.
    pub fn replace(&mut self, new_lines: Vec<String>, total: Option<u64>) {
        self.lines.clear();
        let start = new_lines.len().saturating_sub(LOG_BUFFER_CAP);
        for line in new_lines.into_iter().skip(start) {
            self.lines.push_back(line);
        }
        if let Some(t) = total {
            self.total_seen = t;
        } else {
            self.total_seen = self.lines.len() as u64;
        }
    }

    /// Push one new line (the streaming path).
    ///
    /// Why: future streaming transports can append individual lines without
    /// re-fetching the full tail; centralising the cap-and-evict logic keeps
    /// every caller consistent.
    /// What: appends to the back; evicts the front when over [`LOG_BUFFER_CAP`].
    /// Test: `log_buffer_evicts_oldest`.
    pub fn push(&mut self, line: String) {
        self.lines.push_back(line);
        self.total_seen = self.total_seen.saturating_add(1);
        while self.lines.len() > LOG_BUFFER_CAP {
            self.lines.pop_front();
        }
    }

    /// Scroll up one line (toward older entries), disabling auto-scroll.
    ///
    /// Why: the operator pressing ↑ wants to hold position while the tail
    /// keeps growing; auto-scroll resumes only when the operator presses
    /// `End` or any non-arrow key per the spec.
    /// What: increments `scroll_offset` up to `lines.len() - 1`; clears
    /// `auto_scroll`.
    /// Test: `log_buffer_scroll_clamps`.
    pub fn scroll_up(&mut self) {
        self.auto_scroll = false;
        let max = self.lines.len().saturating_sub(1);
        if self.scroll_offset < max {
            self.scroll_offset += 1;
        }
    }

    /// Scroll down one line (toward newer entries).
    ///
    /// Why: lets the operator return toward the tail after ↑-scrolling.
    /// What: decrements `scroll_offset`; re-enables auto-scroll when the
    /// offset reaches zero (the tail is visible again).
    /// Test: `log_buffer_scroll_clamps`.
    pub fn scroll_down(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
        if self.scroll_offset == 0 {
            self.auto_scroll = true;
        }
    }

    /// Snap back to the tail and re-enable auto-scroll.
    ///
    /// Why: any non-scroll keypress should resume tailing per the spec.
    /// What: zeroes `scroll_offset` and sets `auto_scroll = true`.
    /// Test: `log_buffer_snap_to_tail`.
    pub fn snap_to_tail(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }
}

/// Wire shape of `GET /health` shared by both daemons (issue #35).
///
/// Why: trusty-search and trusty-memory return a compatible health block —
/// `version`, `rss_mb`, `cpu_pct`, `uptime_secs`, `disk_bytes` — so one
/// deserialization target serves both. Every field is `#[serde(default)]` so a
/// daemon on an older build (missing the issue-#35 fields) still deserializes.
/// What: the resource block both `/health` endpoints emit.
/// Test: `health_wire_deserializes_partial_payload`.
#[derive(Debug, Default, Deserialize)]
struct HealthWire {
    #[serde(default)]
    version: String,
    #[serde(default)]
    rss_mb: u64,
    #[serde(default)]
    cpu_pct: f32,
    #[serde(default)]
    uptime_secs: u64,
    #[serde(default)]
    disk_bytes: u64,
}

/// Projected health payload for one daemon panel.
///
/// Why: the panel renders a fixed set of fields; a small typed struct keeps the
/// renderer free of raw JSON and lets the line builder be unit-tested.
/// What: the version string, resource metrics, and the two key-count fields
/// (`count_a` / `count_b`) whose labels differ per daemon.
/// Test: `search_panel_lines_format_fields`, `memory_panel_lines_format_fields`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PanelData {
    /// The daemon version string (e.g. `0.3.67`).
    pub version: String,
    /// Resident set size of the daemon process, in megabytes.
    pub rss_mb: u64,
    /// CPU usage as a percentage (`100.0` == one saturated core).
    pub cpu_pct: f32,
    /// Seconds elapsed since the daemon started.
    pub uptime_secs: u64,
    /// On-disk footprint of the daemon's data directory, in bytes.
    pub disk_bytes: u64,
    /// First key count — indexes (search) or palaces (memory).
    pub count_a: u64,
    /// Second key count — total chunks (search) or total vectors (memory).
    pub count_b: u64,
    /// Third key count — `0` for search; total drawers for memory.
    pub count_c: u64,
    /// Fourth key count — `0` for search; total KG triples for memory.
    pub count_d: u64,
}

/// The connection state of one daemon panel.
///
/// Why: each panel renders distinctly whether it is still connecting, has a
/// fresh payload, or is offline with a captured error; a typed enum keeps that
/// rendering exhaustive.
/// What: `Connecting` before the first poll, `Online` with a payload, or
/// `Offline` with the last error string.
/// Test: `panel_lines_render_each_state`.
#[derive(Debug, Clone, PartialEq)]
pub enum PanelState {
    /// The first poll for this panel has not completed yet.
    Connecting,
    /// The daemon answered; carries the latest projected payload.
    Online(PanelData),
    /// The daemon is unreachable; carries the last error message.
    Offline {
        /// The error captured from the most recent failed poll.
        last_error: String,
    },
}

impl PanelState {
    /// Whether this panel is currently online.
    ///
    /// Why: the `[●]`/`[○]` indicator and the badge colour branch on liveness.
    /// What: returns `true` only for [`PanelState::Online`].
    /// Test: `panel_state_is_online`.
    pub fn is_online(&self) -> bool {
        matches!(self, PanelState::Online(_))
    }
}

/// A health poll result delivered from the background task to the event loop.
///
/// Why: polling runs off-thread so a slow daemon never freezes input handling;
/// the loop drains these messages and folds them into the [`HealthScreen`].
/// What: the [`Daemon`] the result is for, plus the new [`PanelState`].
/// Test: `apply_update_routes_to_panel`.
#[derive(Debug, Clone)]
pub struct HealthUpdate {
    /// Which daemon this update describes.
    pub daemon: Daemon,
    /// The freshly-polled panel state.
    pub state: PanelState,
}

/// Typed HTTP client for one daemon's health + list endpoints.
///
/// Why: the background poller needs a small, testable transport that yields a
/// projected [`PanelData`] or a clean error string; keeping it here mirrors the
/// `trusty-common` monitor clients without depending on that crate's feature.
/// What: holds a base URL, the [`Daemon`] tag (which decides the list
/// endpoints), and a pooled `reqwest::Client` with a request timeout.
/// Test: `health_client_stores_base_url`.
#[derive(Debug, Clone)]
pub struct HealthClient {
    base: String,
    daemon: Daemon,
    http: reqwest::Client,
}

impl HealthClient {
    /// Build a client targeting `base` for the given `daemon`.
    ///
    /// Why: the health screen is pointed at a daemon address from a CLI flag or
    /// the documented default.
    /// What: stores the base URL and a pooled `reqwest::Client` whose request
    /// timeout bounds a hung daemon.
    /// Test: `health_client_stores_base_url`.
    pub fn new(base: impl Into<String>, daemon: Daemon) -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            base: base.into(),
            daemon,
            http,
        }
    }

    /// The base URL this client targets.
    ///
    /// Why: the offline panel renders the daemon address it failed to reach.
    /// What: returns the stored base URL.
    /// Test: `health_client_stores_base_url`.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Poll the daemon and project the result into a [`PanelState`].
    ///
    /// Why: the background task wants one infallible call per tick that always
    /// yields a renderable state — `Online` on success, `Offline` on any
    /// transport or decode failure.
    /// What: GETs `/health`, then the daemon-specific list endpoints for the
    /// key counts, folding everything into [`PanelData`]. Any error along the
    /// way becomes `Offline` carrying the error string.
    /// Test: live behaviour is covered by the daemon suites; the offline path
    /// is exercised by `poll_unreachable_daemon_is_offline`.
    pub async fn poll(&self) -> PanelState {
        match self.fetch().await {
            Ok(data) => PanelState::Online(data),
            Err(e) => PanelState::Offline {
                last_error: e.to_string(),
            },
        }
    }

    /// Fetch and project the panel payload, returning a `Result` for `?`.
    ///
    /// Why: keeps [`Self::poll`]'s error-to-`Offline` mapping in one place
    /// while the happy path stays terse with `?`.
    /// What: GETs `/health` and the daemon's list endpoints; for search the
    /// counts are index count + summed chunk counts, for memory they come from
    /// `/api/v1/status`.
    /// Test: covered indirectly by `poll`; the count projections are unit-tested
    /// via `project_search_counts` / `project_memory_counts`.
    async fn fetch(&self) -> anyhow::Result<PanelData> {
        let health_path = match self.daemon {
            Daemon::Search => "/health",
            Daemon::Memory => "/health",
        };
        let health: HealthWire = self
            .http
            .get(format!("{}{health_path}", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let (count_a, count_b, count_c, count_d) = match self.daemon {
            Daemon::Search => self.search_counts().await,
            Daemon::Memory => self.memory_counts().await,
        };

        Ok(PanelData {
            version: health.version,
            rss_mb: health.rss_mb,
            cpu_pct: health.cpu_pct,
            uptime_secs: health.uptime_secs,
            disk_bytes: health.disk_bytes,
            count_a,
            count_b,
            count_c,
            count_d,
        })
    }

    /// Resolve the search key counts: `(indexes, total_chunks, 0, 0)`.
    ///
    /// Why: the search panel shows index count and summed chunk count; a
    /// failure to enumerate indexes degrades to zeroes rather than failing the
    /// whole poll, since the resource block already rendered.
    /// What: GETs `/indexes`, then `/indexes/:id/status` per index, summing
    /// `chunk_count`. Any error yields all zeroes.
    /// Test: the JSON projection is unit-tested via `project_search_counts`.
    async fn search_counts(&self) -> (u64, u64, u64, u64) {
        let Ok(list) = self.get_json(format!("{}/indexes", self.base)).await else {
            return (0, 0, 0, 0);
        };
        let ids = list
            .get("indexes")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut total_chunks = 0u64;
        for id in &ids {
            if let Ok(status) = self
                .get_json(format!("{}/indexes/{id}/status", self.base))
                .await
            {
                total_chunks = total_chunks.saturating_add(
                    status
                        .get("chunk_count")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                );
            }
        }
        (ids.len() as u64, total_chunks, 0, 0)
    }

    /// Resolve the memory key counts from `/api/v1/status`.
    ///
    /// Why: the memory panel shows palaces, vectors, drawers, and KG triples;
    /// the status endpoint returns all four in one call.
    /// What: GETs `/api/v1/status` and projects `palace_count`, `total_vectors`,
    /// `total_drawers`, `total_kg_triples`. Any error yields all zeroes.
    /// Test: the JSON projection is unit-tested via `project_memory_counts`.
    async fn memory_counts(&self) -> (u64, u64, u64, u64) {
        match self.get_json(format!("{}/api/v1/status", self.base)).await {
            Ok(status) => project_memory_counts(&status),
            Err(_) => (0, 0, 0, 0),
        }
    }

    /// GET `url` and decode the response body as JSON.
    ///
    /// Why: the count probes share the same GET-and-decode shape.
    /// What: GETs `url`, maps a non-2xx response to an error, and decodes the
    /// body into a [`serde_json::Value`].
    /// Test: covered indirectly by `search_counts` / `memory_counts`.
    async fn get_json(&self, url: String) -> anyhow::Result<serde_json::Value> {
        Ok(self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// Fetch the most recent `n` log lines from the daemon.
    ///
    /// Why: the Logs tab (`[2]`) tails the daemon's in-memory log ring via
    /// `GET /logs/tail?n=…`; both daemons share this endpoint (issue #35).
    /// What: GETs `/logs/tail?n=…` and projects `lines` + `total`. A daemon
    /// without this endpoint (older build) yields `Ok((vec![], 0))` rather
    /// than an error so the tab degrades to a placeholder cleanly.
    /// Test: live behaviour is covered by the daemon suites; the projection
    /// is unit-tested via `project_log_tail`.
    pub async fn logs_tail(&self, n: u32) -> anyhow::Result<(Vec<String>, u64)> {
        let url = format!("{}/logs/tail?n={n}", self.base);
        match self.http.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<serde_json::Value>().await {
                    Ok(body) => Ok(project_log_tail(&body)),
                    Err(_) => Ok((Vec::new(), 0)),
                }
            }
            // 404 or older daemon: no logs endpoint — degrade to empty.
            Ok(_) => Ok((Vec::new(), 0)),
            Err(e) => Err(anyhow::anyhow!("logs_tail: {e}")),
        }
    }

    /// Fetch the search daemon's index list with chunk counts.
    ///
    /// Why: the Collections list (left panel for the search service) wants a
    /// per-index name + chunk count so the operator can see at a glance
    /// which corpora are loaded.
    /// What: GETs `/indexes`, then `GET /indexes/:id/status` per index,
    /// projecting `(id, chunk_count)` into [`CollectionRow`]s. Any error
    /// yields an empty list rather than failing.
    /// Test: live behaviour is covered by the daemon suites; the projection
    /// is unit-tested via `project_index_rows`.
    pub async fn search_collections(&self) -> Vec<CollectionRow> {
        let Ok(list) = self.get_json(format!("{}/indexes", self.base)).await else {
            return Vec::new();
        };
        let ids: Vec<String> = list
            .get("indexes")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            // Status: chunk count + last_indexed + disk bytes + context embedding.
            let status = self
                .get_json(format!("{}/indexes/{id}/status", self.base))
                .await
                .ok();
            let count = status
                .as_ref()
                .and_then(|v| v.get("chunk_count").and_then(|c| c.as_u64()))
                .unwrap_or(0);
            let last_indexed = status
                .as_ref()
                .and_then(|v| v.get("last_indexed").and_then(|c| c.as_str()))
                .map(str::to_string);
            let disk_bytes = status
                .as_ref()
                .and_then(|v| v.get("disk_bytes").and_then(|c| c.as_u64()))
                .unwrap_or(0);
            let has_context_embedding = status
                .as_ref()
                .and_then(|v| v.get("has_context_embedding").and_then(|c| c.as_bool()))
                .unwrap_or(false);

            // Graph stats: nodes, edges, edge kind histogram. Errors → zeroes.
            let graph = self
                .get_json(format!("{}/indexes/{id}/graph/stats", self.base))
                .await
                .ok();
            let node_count = graph
                .as_ref()
                .and_then(|v| v.get("node_count").and_then(|c| c.as_u64()))
                .unwrap_or(0);
            let edge_count = graph
                .as_ref()
                .and_then(|v| v.get("edge_count").and_then(|c| c.as_u64()))
                .unwrap_or(0);
            let edge_kinds = graph.as_ref().map(project_edge_kinds).unwrap_or_default();

            // Communities: only the top-level summary fields are needed.
            let communities = self
                .get_json(format!("{}/indexes/{id}/communities", self.base))
                .await
                .ok();
            let community_count = communities
                .as_ref()
                .and_then(|v| v.get("community_count").and_then(|c| c.as_u64()))
                .unwrap_or(0);
            let modularity = communities
                .as_ref()
                .and_then(|v| v.get("modularity").and_then(|c| c.as_f64()))
                .unwrap_or(0.0);

            let note = format_relative_time(last_indexed.as_deref());
            rows.push(CollectionRow {
                id,
                count,
                note,
                ok: true,
                last_indexed,
                node_count,
                edge_count,
                edge_kinds,
                community_count,
                modularity,
                disk_bytes,
                has_context_embedding,
                ..Default::default()
            });
        }
        rows
    }

    /// Fetch the memory daemon's palace list with vector and KG counts.
    ///
    /// Why: the Collections list (left panel for the memory service) needs the
    /// per-palace name, vector count, and KG triple count so the operator can
    /// see at a glance which palaces hold the most memory and which carry a
    /// knowledge graph. `/api/v1/status` only exposes aggregate totals; the
    /// per-palace breakdown lives at `/api/v1/palaces`.
    /// What: GETs `/api/v1/palaces` (a JSON array of `PalaceInfo`) and
    /// projects each entry into a [`CollectionRow`]. Any error yields an empty
    /// list.
    /// Test: the projection is unit-tested via `project_palace_rows`.
    pub async fn memory_collections(&self) -> Vec<CollectionRow> {
        let Ok(list) = self.get_json(format!("{}/api/v1/palaces", self.base)).await else {
            return Vec::new();
        };
        project_palace_rows(&list)
    }

    /// Request a graceful shutdown of the daemon via its `admin/stop` endpoint.
    ///
    /// Why: the `[X]` key stops the focused daemon without the operator
    /// resolving a PID; both daemons expose an unauthenticated stop route.
    /// What: POSTs an empty body to the daemon's stop path (`/admin/stop` for
    /// search, `/api/v1/admin/stop` for memory). A non-2xx response is an error.
    /// Test: live behaviour is covered by the daemon suites; the dashboard
    /// records the outcome string in `last_action`.
    pub async fn stop(&self) -> anyhow::Result<()> {
        let path = match self.daemon {
            Daemon::Search => "/admin/stop",
            Daemon::Memory => "/api/v1/admin/stop",
        };
        self.http
            .post(format!("{}{path}", self.base))
            .json(&serde_json::json!({}))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

/// Project a `/api/v1/status` payload into `(palaces, vectors, drawers, kg)`.
///
/// Why: centralising the projection keeps [`HealthClient::memory_counts`]
/// testable without a live daemon and resilient to absent optional fields.
/// What: reads `palace_count`, `total_vectors`, `total_drawers`, and
/// `total_kg_triples`, defaulting any absent field to zero.
/// Test: `project_memory_counts`.
fn project_memory_counts(status: &serde_json::Value) -> (u64, u64, u64, u64) {
    let u = |key: &str| status.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    (
        u("palace_count"),
        u("total_vectors"),
        u("total_drawers"),
        u("total_kg_triples"),
    )
}

/// Project a `/logs/tail` response into `(lines, total)`.
///
/// Why: keeps the wire-shape parsing in one testable function so the client
/// stays terse and an older daemon's quirky payload cannot crash the TUI.
/// What: reads `lines` (array of strings, defaulting to `[]`) and `total`
/// (u64, defaulting to `lines.len()`).
/// Test: `project_log_tail_reads_fields`.
fn project_log_tail(body: &serde_json::Value) -> (Vec<String>, u64) {
    let lines: Vec<String> = body
        .get("lines")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let total = body
        .get("total")
        .and_then(|v| v.as_u64())
        .unwrap_or(lines.len() as u64);
    (lines, total)
}

/// Project a memory daemon `/api/v1/palaces` payload into palace rows.
///
/// Why: centralising the projection keeps the renderer terse and lets a unit
/// test assert the shape without a live daemon. The wire format is a JSON
/// array of `PalaceInfo` objects with per-palace `vector_count`,
/// `drawer_count`, and `kg_triple_count`. `/api/v1/status` exposes only
/// aggregate totals, so this is the only source of per-palace counts. Empty
/// palaces (no vectors AND no KG triples) are filtered out so the left pane
/// only lists palaces that actually hold memory — an empty palace is
/// indistinguishable from a placeholder and just adds visual noise.
/// What: reads the top-level array, projecting each entry's `name` (falling
/// back to `id`), `vector_count`, and `kg_triple_count` (any absent field
/// defaults to zero). Rows where both counts are zero are dropped. A
/// non-array payload yields an empty list.
/// Test: `project_palace_rows_reads_palaces`,
/// `project_palace_rows_filters_empty`.
fn project_palace_rows(list: &serde_json::Value) -> Vec<CollectionRow> {
    let Some(arr) = list.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|p| {
            let id = p
                .get("name")
                .or_else(|| p.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let count = p.get("vector_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let kg_count = p
                .get("kg_triple_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let drawer_count = p.get("drawer_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let wing_count = p.get("wing_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let last_write_at = p
                .get("last_write_at")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let node_count = p.get("node_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let edge_count = p.get("edge_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let community_count = p
                .get("community_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let is_compacting = p
                .get("is_compacting")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Skip palaces with no vectors and no graph triples: they hold
            // nothing the operator can act on and clutter the left pane.
            if count == 0 && kg_count == 0 {
                return None;
            }
            Some(CollectionRow {
                id,
                count,
                kg_count,
                drawer_count,
                wing_count,
                last_write_at,
                node_count,
                edge_count,
                community_count,
                is_compacting,
                // Note left empty: the row shows vector + graph counts inline
                // (e.g. `12v 34g`), so a trailing badge would be redundant.
                note: String::new(),
                ok: true,
                ..Default::default()
            })
        })
        .collect()
}

/// Project a `/indexes/:id/graph/stats` payload's `edge_kinds` map into a
/// vec sorted by count descending.
///
/// Why: the INDEX tab renders one row per edge kind, ordered so the heaviest
/// relationship appears at the top; keeping the projection pure makes it
/// testable without a live daemon.
/// What: reads the `edge_kinds` object from `stats`, collects `(name, count)`
/// pairs, and sorts by count descending (ties broken by name ascending).
/// A missing object yields an empty vec.
/// Test: `project_edge_kinds_sorts_desc`.
fn project_edge_kinds(stats: &serde_json::Value) -> Vec<(String, u64)> {
    let Some(map) = stats.get("edge_kinds").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut pairs: Vec<(String, u64)> = map
        .iter()
        .map(|(k, v)| (k.clone(), v.as_u64().unwrap_or(0)))
        .collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs
}

/// Format an RFC 3339 timestamp as a compact `[Xm/h/d ago]` badge.
///
/// Why: the collections list shows a freshness badge next to each row so the
/// operator can spot stale indexes; raw RFC 3339 strings are too wide for the
/// 28-column left panel.
/// What: parses the timestamp with `chrono::DateTime::parse_from_rfc3339`,
/// computes the signed delta against `Utc::now()`, and renders the largest
/// unit that yields a non-zero figure (`Xm`, `Xh`, or `Xd`). `None` or an
/// unparseable string yields `"never"`. A future timestamp (clock skew) is
/// reported as `"just now"`.
/// Test: `format_relative_time_handles_known_offsets`.
pub fn format_relative_time(ts: Option<&str>) -> String {
    let Some(s) = ts else {
        return "never".to_string();
    };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(s) else {
        return "never".to_string();
    };
    let now = chrono::Utc::now();
    let delta = now.signed_duration_since(parsed.with_timezone(&chrono::Utc));
    let secs = delta.num_seconds();
    if secs < 60 {
        // Includes negative (future) timestamps from clock skew.
        return "just now".to_string();
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

/// Format a `uptime` in seconds as a compact `Xh Ym` string.
///
/// Why: the panel shows uptime; raw seconds are hard to read at a glance.
/// What: returns `"{hours}h {minutes}m"` — e.g. `3720` → `"1h 2m"`.
/// Test: `format_uptime_is_compact`.
pub fn format_uptime(secs: u64) -> String {
    format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
}

/// Format a byte count as a human-readable size (`KB` / `MB` / `GB`).
///
/// Why: raw `disk_bytes` and `rss_mb` figures are easier to scan abbreviated.
/// What: returns one decimal place with a unit suffix, picking the largest
/// unit under which the value is `>= 1`.
/// Test: `format_bytes_picks_unit`.
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1}GB", b / GB)
    } else if b >= MB {
        format!("{:.1}MB", b / MB)
    } else if b >= KB {
        format!("{:.1}KB", b / KB)
    } else {
        format!("{bytes}B")
    }
}

/// Format an RSS figure (already in megabytes) as a human-readable size.
///
/// Why: `/health` reports RSS in whole megabytes; the panel shows it as `GB`
/// once it crosses 1024 MB so the figure stays short.
/// What: returns `"{x.x}GB"` above 1024 MB, otherwise `"{n}MB"`.
/// Test: `format_rss_picks_unit`.
pub fn format_rss(mb: u64) -> String {
    if mb >= 1024 {
        format!("{:.1}GB", mb as f64 / 1024.0)
    } else {
        format!("{mb}MB")
    }
}

/// Format a large count compactly: exact below 10k, `Xk` above.
///
/// Why: chunk and vector counts run into the tens of thousands; an abbreviated
/// form keeps the fixed-width panel readable.
/// What: counts below 10,000 are shown exactly; larger counts as `{n}k` with
/// one decimal.
/// Test: `format_count_abbreviates_large`.
pub fn format_count(n: u64) -> String {
    if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Build the text lines for the trusty-search panel body.
///
/// Why: separating line construction from the ratatui widgets lets a test
/// assert the rendered content without a terminal backend.
/// What: returns the panel body as plain strings — a header line with the
/// online indicator and version, then resource and count lines; an offline
/// panel shows its error, a connecting panel a placeholder.
/// Test: `search_panel_lines_format_fields`, `panel_lines_render_each_state`.
pub fn search_panel_lines(state: &PanelState, base_url: &str) -> Vec<String> {
    panel_lines(state, base_url, "SEARCH", |data| {
        vec![
            format!(
                "Indexes: {}  Chunks: {}",
                data.count_a,
                format_count(data.count_b)
            ),
            format!("Disk: {}", format_bytes(data.disk_bytes)),
        ]
    })
}

/// Build the text lines for the trusty-memory panel body.
///
/// Why: mirrors [`search_panel_lines`] for testable, terminal-free rendering.
/// What: returns the panel body as plain strings — header, resource lines, then
/// palace / vector / drawer / KG counts; offline and connecting states render
/// as for search.
/// Test: `memory_panel_lines_format_fields`, `panel_lines_render_each_state`.
pub fn memory_panel_lines(state: &PanelState, base_url: &str) -> Vec<String> {
    panel_lines(state, base_url, "MEMORY", |data| {
        vec![
            format!(
                "Palaces: {}  Vectors: {}",
                data.count_a,
                format_count(data.count_b)
            ),
            format!(
                "Drawers: {}  KG: {}",
                data.count_c,
                format_count(data.count_d)
            ),
        ]
    })
}

/// Shared panel-body builder for the search and memory panels.
///
/// Why: both panels share the header / resource / footer structure; only the
/// count lines differ, so they are supplied by the `counts` closure.
/// What: returns the header (indicator + version), the RSS / CPU / uptime line,
/// the daemon-specific count lines, a blank spacer, and the `[S]start [X]stop`
/// hint. Offline panels show the unreachable address, last error, and a retry
/// note; connecting panels show a placeholder.
/// Test: `panel_lines_render_each_state`.
fn panel_lines(
    state: &PanelState,
    base_url: &str,
    name: &str,
    counts: impl Fn(&PanelData) -> Vec<String>,
) -> Vec<String> {
    match state {
        PanelState::Connecting => vec![format!("{name} [○] connecting to {base_url}…")],
        PanelState::Offline { last_error } => vec![
            format!("{name} [○] OFFLINE"),
            format!("unreachable at {base_url}"),
            format!("last error: {last_error}"),
            "retrying every 5s…".to_string(),
            String::new(),
            "[S]start [X]stop".to_string(),
        ],
        PanelState::Online(data) => {
            let version = if data.version.is_empty() {
                "?".to_string()
            } else {
                format!("v{}", data.version)
            };
            let mut lines = vec![
                format!("{name} [●] {version}"),
                format!(
                    "RSS: {}  CPU: {:.0}%  Uptime: {}",
                    format_rss(data.rss_mb),
                    data.cpu_pct,
                    format_uptime(data.uptime_secs),
                ),
            ];
            lines.extend(counts(data));
            lines.push(String::new());
            lines.push("[S]start [X]stop".to_string());
            lines
        }
    }
}

/// The combined search + memory health screen (`[2]`).
///
/// Why: the event loop polls both daemons on a background task and folds the
/// results here; a clean data struct keeps the loop terse and the rendering
/// pure. Held alongside the chat `DashboardState` so switching screens never
/// resets either surface.
/// What: a [`PanelState`] and base URL per daemon, plus the focused [`Daemon`]
/// that the `[S]`/`[X]` keys act on.
/// Test: `toggle_focus_cycles_panels`, `apply_update_routes_to_panel`.
#[derive(Debug, Clone)]
pub struct HealthScreen {
    /// The trusty-search panel state.
    pub search: PanelState,
    /// The trusty-search daemon base URL.
    pub search_url: String,
    /// The trusty-memory panel state.
    pub memory: PanelState,
    /// The trusty-memory daemon base URL.
    pub memory_url: String,
    /// Which panel `[S]`/`[X]` act on; `[Tab]` cycles it.
    pub focus: Daemon,
    /// Which right-panel tab is currently visible.
    pub tab: HealthTab,
    /// Collections for the search service (issue #36 left panel).
    pub search_collections: Vec<CollectionRow>,
    /// Palaces for the memory service (issue #36 left panel).
    pub memory_collections: Vec<CollectionRow>,
    /// Highlighted row in the focused service's collections list.
    pub selected_collection: usize,
    /// Log ring buffer for the search service.
    pub search_logs: LogBuffer,
    /// Log ring buffer for the memory service.
    pub memory_logs: LogBuffer,
    /// Buffer for the Search tab's query input (always visible in footer).
    pub search_query: String,
    /// Cursor on the search input when focused (drawn as `_`).
    pub search_input_focused: bool,
}

impl HealthScreen {
    /// Build a health screen targeting the two given daemon URLs.
    ///
    /// Why: the TUI resolves both daemon addresses once at startup and seeds
    /// the panels; both start `Connecting` until the first poll lands.
    /// What: stores both URLs, sets both panels to [`PanelState::Connecting`],
    /// and defaults focus to the search panel.
    /// Test: `new_screen_starts_connecting`.
    pub fn new(search_url: impl Into<String>, memory_url: impl Into<String>) -> Self {
        Self {
            search: PanelState::Connecting,
            search_url: search_url.into(),
            memory: PanelState::Connecting,
            memory_url: memory_url.into(),
            focus: Daemon::Search,
            tab: HealthTab::default(),
            search_collections: Vec::new(),
            memory_collections: Vec::new(),
            selected_collection: 0,
            search_logs: LogBuffer::new(),
            memory_logs: LogBuffer::new(),
            search_query: String::new(),
            search_input_focused: false,
        }
    }

    /// Switch to the given right-panel tab.
    ///
    /// Why: the `1` / `2` / `3` keys move between the Health, Logs, and
    /// Search tabs; routing through one setter keeps the focus-side-effect
    /// (auto-focusing the search input on the Search tab) in one place.
    /// What: stores `tab` and auto-focuses the search input when `Search`.
    /// Test: `tab_switch_keys_route`.
    pub fn set_tab(&mut self, tab: HealthTab) {
        self.tab = tab;
        self.search_input_focused = matches!(tab, HealthTab::Search);
    }

    /// Currently-focused service's collections list.
    ///
    /// Why: the left panel renders the focused service's collections; one
    /// accessor keeps the renderer free of per-daemon branching.
    /// What: returns a borrowed slice into the focused service's list.
    /// Test: `collections_for_focus`.
    pub fn focused_collections(&self) -> &[CollectionRow] {
        match self.focus {
            Daemon::Search => &self.search_collections,
            Daemon::Memory => &self.memory_collections,
        }
    }

    /// Mutable handle to the focused service's log buffer.
    ///
    /// Why: ↑/↓ in the Logs tab scrolls the focused service's buffer; a
    /// single accessor keeps the event-loop branches small.
    /// What: returns `&mut self.search_logs` or `&mut self.memory_logs`.
    /// Test: covered by the scroll tests on `LogBuffer`.
    pub fn focused_logs_mut(&mut self) -> &mut LogBuffer {
        match self.focus {
            Daemon::Search => &mut self.search_logs,
            Daemon::Memory => &mut self.memory_logs,
        }
    }

    /// Borrow the focused service's log buffer.
    ///
    /// Why: the renderer reads (but does not mutate) the buffer to draw the
    /// Logs tab; keeping a shared accessor next to the mutable one mirrors
    /// the common ratatui borrow pattern.
    /// What: returns `&self.search_logs` or `&self.memory_logs`.
    /// Test: covered indirectly by the render smoke tests.
    pub fn focused_logs(&self) -> &LogBuffer {
        match self.focus {
            Daemon::Search => &self.search_logs,
            Daemon::Memory => &self.memory_logs,
        }
    }

    /// Move the collections selection up one row (saturating at the top).
    ///
    /// Why: ↑ on the left panel highlights the previous collection; saturate
    /// so the operator cannot scroll off the end into an undefined index.
    /// What: decrements `selected_collection` with a floor of zero.
    /// Test: `select_collection_saturates`.
    pub fn select_collection_up(&mut self) {
        self.selected_collection = self.selected_collection.saturating_sub(1);
    }

    /// Move the collections selection down one row (saturating at the bottom).
    ///
    /// Why: ↓ on the left panel highlights the next collection; saturate at
    /// the end so a shrinking list never leaves the index out of bounds.
    /// What: increments `selected_collection` up to `len - 1`.
    /// Test: `select_collection_saturates`.
    pub fn select_collection_down(&mut self) {
        let max = self.focused_collections().len().saturating_sub(1);
        if self.selected_collection < max {
            self.selected_collection += 1;
        }
    }

    /// Clamp the collections selection to the focused list's bounds.
    ///
    /// Why: after a poll replaces the list with a shorter one, a stale
    /// selection index would render an out-of-bounds row.
    /// What: pins `selected_collection` to `len - 1` (or `0` when empty).
    /// Test: `select_collection_clamps_after_shrink`.
    pub fn clamp_collection_selection(&mut self) {
        let max = self.focused_collections().len().saturating_sub(1);
        if self.selected_collection > max {
            self.selected_collection = max;
        }
    }

    /// Cycle keyboard focus between the search and memory panels (`[Tab]`).
    ///
    /// Why: `[Tab]` decides which panel the `[S]`/`[X]` service keys act on.
    /// What: flips [`Self::focus`].
    /// Test: `toggle_focus_cycles_panels`.
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Daemon::Search => Daemon::Memory,
            Daemon::Memory => Daemon::Search,
        };
    }

    /// Fold a background poll result into the matching panel.
    ///
    /// Why: the event loop drains [`HealthUpdate`]s and must route each to the
    /// correct panel without touching the other.
    /// What: replaces the [`PanelState`] of the daemon named in `update`.
    /// Test: `apply_update_routes_to_panel`.
    pub fn apply_update(&mut self, update: HealthUpdate) {
        match update.daemon {
            Daemon::Search => self.search = update.state,
            Daemon::Memory => self.memory = update.state,
        }
    }

    /// The base URL of the currently-focused panel.
    ///
    /// Why: the `[X]` stop action targets the focused daemon.
    /// What: returns the search or memory URL per [`Self::focus`].
    /// Test: `focused_url_follows_focus`.
    pub fn focused_url(&self) -> &str {
        match self.focus {
            Daemon::Search => &self.search_url,
            Daemon::Memory => &self.memory_url,
        }
    }
}

/// Build a [`HealthClient`] for the given daemon at the given base URL.
///
/// Why: the background poller and the `[S]`/`[X]` key handlers all need a
/// client; centralising construction keeps the daemon→client mapping in one
/// place.
/// What: returns a [`HealthClient`] tagged with `daemon`.
/// Test: covered by `health_client_stores_base_url`.
pub fn client_for(daemon: Daemon, base_url: &str) -> HealthClient {
    HealthClient::new(base_url, daemon)
}

/// Render the health screen into `area`-spanning `frame`.
///
/// Why: the single entry point the event loop calls when screen `[2]` is
/// active; keeps all the ratatui widget assembly in one place.
/// What: a vertical layout — a one-line title, a body split horizontally into
/// the two daemon panels (the focused one gets a bold cyan border), and the
/// shared status bar is drawn by the caller. Panel bodies come from the pure
/// `*_panel_lines` helpers.
/// Test: line content is unit-tested via `search_panel_lines` /
/// `memory_panel_lines`; this glue is exercised by `render_health_smoke`.
pub fn render(frame: &mut Frame, screen: &HealthScreen) {
    // Three-zone layout (issue #36):
    //   1. Header (2 lines: title + stats)
    //   2. Main (left collections list + right tabbed panel)
    //   3. Footer (search/command input + key hint)
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header (title + stats)
            Constraint::Min(6),    // main body
            Constraint::Length(3), // footer (search input)
        ])
        .split(frame.area());

    render_header(frame, outer[0], screen);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(30), // collections list
            Constraint::Min(20),    // tabbed panel
        ])
        .split(outer[1]);

    render_collections(frame, body[0], screen);
    render_tab_panel(frame, body[1], screen);
    render_footer(frame, outer[2], screen);
}

/// Title for the focused service ("trusty-search" / "trusty-memory").
///
/// Why: the header uses the focused service's name to make the surface
/// clearly single-service; centralising the mapping keeps both renders
/// consistent.
/// What: returns the conventional binary name for the focused daemon.
/// Test: `service_name_matches_focus`.
pub fn service_name(focus: Daemon) -> &'static str {
    match focus {
        Daemon::Search => "trusty-search",
        Daemon::Memory => "trusty-memory",
    }
}

/// Build the header text lines (issue #36 zone 1).
///
/// Why: pure helper so the header content is testable without a terminal.
/// What: line 1 is `service vX.Y.Z [●] ONLINE` (or `[○] OFFLINE`); line 2 is
/// the resource snapshot `RSS / CPU / Disk / Uptime`. An offline panel keeps
/// the layout shape — fields show `?`.
/// Test: `header_lines_show_focus_summary`.
pub fn header_lines(screen: &HealthScreen) -> Vec<String> {
    let focused = match screen.focus {
        Daemon::Search => &screen.search,
        Daemon::Memory => &screen.memory,
    };
    let name = service_name(screen.focus);
    match focused {
        PanelState::Online(data) => {
            let version = if data.version.is_empty() {
                "?".to_string()
            } else {
                format!("v{}", data.version)
            };
            vec![
                format!("{name} {version}  [●] ONLINE"),
                format!(
                    "RSS: {}  CPU: {:.0}%  Disk: {}  Uptime: {}",
                    format_rss(data.rss_mb),
                    data.cpu_pct,
                    format_bytes(data.disk_bytes),
                    format_uptime(data.uptime_secs),
                ),
            ]
        }
        PanelState::Offline { last_error } => vec![
            format!("{name}  [○] OFFLINE"),
            format!("last error: {last_error}"),
        ],
        PanelState::Connecting => vec![format!("{name}  [○] connecting…"), String::new()],
    }
}

/// Render the header zone (title + resource snapshot).
///
/// Why: kept separate so the main `render` reads top-to-bottom.
/// What: draws the two `header_lines` inside a single bordered block.
fn render_header(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let online = match screen.focus {
        Daemon::Search => screen.search.is_online(),
        Daemon::Memory => screen.memory.is_online(),
    };
    let lines = header_lines(screen);
    let title_color = if online { Color::Green } else { Color::Red };
    let body: Vec<Line> = lines.into_iter().map(Line::from).collect();
    frame.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(title_color))
                .title(Span::styled(
                    format!(" {} ", service_name(screen.focus)),
                    Style::default()
                        .fg(title_color)
                        .add_modifier(Modifier::BOLD),
                )),
        ),
        area,
    );
}

/// Build the collections list lines (left panel).
///
/// Why: a pure helper for testing the row formatting; the renderer feeds the
/// returned strings into a paragraph widget so the format is exactly what the
/// operator sees.
/// What: one line per row. Search collections use
/// `> id        count ✓ [note]`. Memory palaces use
/// `> palace-name        12v 34g` — vector + KG triple counts, each with a
/// `v` / `g` suffix and `--v` / `--g` when the count is zero so missing data
/// is visible at a glance.
/// Test: `collections_lines_format_each_row`,
/// `collections_lines_show_graph_count_for_memory`,
/// `collections_lines_show_dashes_for_zero_counts`.
pub fn collections_lines(screen: &HealthScreen) -> Vec<String> {
    collections_lines_at_tick(screen, current_spinner_tick())
}

/// Like [`collections_lines`] but with a caller-supplied spinner tick.
///
/// Why: unit tests want deterministic spinner output; passing the tick in
/// keeps the tested function pure while `collections_lines` remains a
/// convenience wrapper that samples wall-clock time.
/// What: builds one line per row. Memory rows include an activity glyph
/// prefix (driven by [`palace_activity`] + [`spinner_frame`]) so the
/// operator can spot indexing / active / error palaces at a glance.
/// Test: `collections_lines_at_tick_shows_indexing_spinner`,
/// `collections_lines_at_tick_idle_palace_has_no_spinner`.
pub fn collections_lines_at_tick(screen: &HealthScreen, tick: usize) -> Vec<String> {
    let rows = screen.focused_collections();
    if rows.is_empty() {
        return vec!["(none)".to_string()];
    }
    let focus = screen.focus;
    rows.iter()
        .enumerate()
        .map(|(i, r)| {
            let marker = if i == screen.selected_collection {
                ">"
            } else {
                " "
            };
            match focus {
                Daemon::Memory => format_palace_row(marker, r, tick),
                Daemon::Search => format_search_row(marker, r),
            }
        })
        .collect()
}

/// Format one search-collection row (the existing layout).
///
/// Why: extracted so the memory branch can use a different layout without
/// `collections_lines` growing a large match arm body.
/// What: `{marker} {id:<12} {count:>6} {glyph}[badge]` where the badge prefers
/// a parsed last-indexed time and falls back to the row's free-form note.
/// Test: covered by `collections_lines_format_each_row`.
fn format_search_row(marker: &str, r: &CollectionRow) -> String {
    let glyph = if r.ok { "✓" } else { "✗" };
    let badge_text = if r.last_indexed.is_some() {
        format_relative_time(r.last_indexed.as_deref())
    } else if !r.note.is_empty() {
        r.note.clone()
    } else {
        String::new()
    };
    let badge = if badge_text.is_empty() {
        String::new()
    } else {
        format!("  [{badge_text}]")
    };
    format!(
        "{marker} {:<12} {:>6} {glyph}{badge}",
        r.id,
        format_count(r.count)
    )
}

/// Format one memory-palace row.
///
/// Why: memory palaces have no `last_indexed` and benefit from showing both
/// their vector count and their KG triple count; the previous shared layout
/// wasted the trailing column on a hardcoded `ready` note.
/// What: `{marker} {name:<16} {vec:>4} {kg:>4}` where each count is the
/// abbreviated form (`format_count`) suffixed with `v` / `g`, falling back to
/// `--v` / `--g` when the underlying count is zero so the operator can spot
/// palaces missing vectors or a graph.
/// Test: `collections_lines_show_graph_count_for_memory`,
/// `collections_lines_show_dashes_for_zero_counts`.
fn format_palace_row(marker: &str, r: &CollectionRow, tick: usize) -> String {
    let vec_cell = format_count_suffix(r.count, 'v');
    let kg_cell = format_count_suffix(r.kg_count, 'g');
    // The activity glyph occupies a fixed one-column slot so rows stay
    // aligned whether or not a palace is active. Idle palaces get a space.
    let glyph = spinner_frame(palace_activity(r), tick).unwrap_or(' ');
    format!(
        "{marker}{glyph} {:<15} {:>4} {:>4}",
        r.id, vec_cell, kg_cell
    )
}

/// Render a count plus a single-letter suffix, using `--` for zero.
///
/// Why: the palace row format wants `12v` / `34g` cells where a zero count
/// stands out as `--v` / `--g`. Centralising the rule keeps both callers in
/// sync.
/// What: returns `"{abbrev}{suffix}"` for non-zero counts (where `abbrev` is
/// `format_count`'s output) and `"--{suffix}"` for zero.
/// Test: `format_count_suffix_handles_zero_and_value`.
fn format_count_suffix(n: u64, suffix: char) -> String {
    if n == 0 {
        format!("--{suffix}")
    } else {
        format!("{}{suffix}", format_count(n))
    }
}

/// Render the collections list (left panel).
///
/// Why: kept separate so the renderer remains small and one branch handles
/// the search-vs-memory title label.
/// What: draws a stateful `List` inside a bordered block titled
/// `COLLECTIONS (n)` for search or `PALACES (n)` for memory. The selected row
/// uses bold white-on-blue via `highlight_style`, which (unlike a manually
/// padded `Paragraph` row) is applied by ratatui across every inner cell of
/// the row regardless of the underlying text length — that is what eliminates
/// the unstyled "right gutter" the manual-padding fix struggled with.
/// Test: `render_health_smoke` exercises the layout end-to-end; row formatting
/// is covered by `collections_lines_format_each_row` and friends.
fn render_collections(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let rows = screen.focused_collections();
    let label = match screen.focus {
        Daemon::Search => "COLLECTIONS",
        Daemon::Memory => "PALACES",
    };
    let title = format!(" {label} ({}) ", rows.len());
    let lines = collections_lines(screen);
    // Switched from `Paragraph` + manual right-pad to the canonical `List` +
    // `ListState` pattern. The previous approach right-padded each row to
    // `area.width - 2` so the selected row's `Span::styled` background covered
    // the full inner width, but the residual "right gutter" the user reported
    // was actually an artefact of how ratatui's `Paragraph` clips trailing
    // styled spans on some terminals — the styled cells reached the inner
    // border but the final cell of the highlight could be reset by the
    // terminal's own SGR handling. A `List` widget styles entire rows via
    // `highlight_style`, applies the style to every cell from the inner-left
    // border to the inner-right border (independent of the item's text
    // length), and renders `HighlightSpacing::Always` so unselected rows align
    // with the selected one. This eliminates the gutter at the rendering layer
    // instead of relying on the input text being exactly inner-width chars
    // wide.
    // Apply per-row activity colour for memory palaces; search rows stay
    // default-styled. Idle palaces keep the default colour too, so only the
    // "something is happening" rows light up against the rest of the list.
    let items: Vec<ListItem<'static>> = lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let item = ListItem::new(line);
            if matches!(screen.focus, Daemon::Memory)
                && let Some(row) = rows.get(i)
            {
                let activity = palace_activity(row);
                if !matches!(activity, PalaceActivity::Idle) {
                    return item.style(Style::default().fg(activity_color(activity)));
                }
            }
            item
        })
        .collect();
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
        // `HighlightSpacing::Always` keeps every row aligned regardless of
        // whether a `highlight_symbol` is set; without it, unselected rows
        // would shift one column left when the selection changes.
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(screen.selected_collection.min(rows.len() - 1)));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

/// Build the tab bar header text ("[1]HEALTH  [2]LOGS  [3]SEARCH").
///
/// Why: pure helper so the active-tab highlighting is testable.
/// What: returns `(label, active)` pairs the renderer styles.
/// Test: `tab_bar_marks_active`.
pub fn tab_bar(active: HealthTab) -> Vec<(String, bool)> {
    [
        ("[1]HEALTH", HealthTab::Health),
        ("[2]LOGS", HealthTab::Logs),
        ("[3]SEARCH", HealthTab::Search),
        ("[4]INDEX", HealthTab::Index),
    ]
    .iter()
    .map(|(label, tab)| ((*label).to_string(), *tab == active))
    .collect()
}

/// Render the right-side tabbed panel (HEALTH / LOGS / SEARCH).
///
/// Why: keeps the per-tab body switch in one place.
/// What: draws the tab bar on the first body line; the active tab is bold
/// cyan, others dimmed. Below the bar, dispatches to a per-tab renderer.
fn render_tab_panel(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " DETAILS ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    // Tab bar line.
    let mut spans: Vec<Span> = Vec::new();
    for (label, active) in tab_bar(screen.tab) {
        let style = if active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw("  "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), split[0]);

    // Active tab body.
    match screen.tab {
        HealthTab::Health => render_health_tab(frame, split[1], screen),
        HealthTab::Logs => render_logs_tab(frame, split[1], screen),
        HealthTab::Search => render_search_tab(frame, split[1], screen),
        HealthTab::Index => render_index_tab(frame, split[1], screen),
    }
}

/// Build the HEALTH tab body lines (gauges + config summary).
///
/// Why: pure helper so the resource gauges are testable.
/// What: returns the memory bar, disk bar, embedder status, and a CoreML
/// summary line. An offline panel returns a placeholder.
/// Test: `health_tab_lines_show_gauges`.
pub fn health_tab_lines(screen: &HealthScreen) -> Vec<String> {
    let panel = match screen.focus {
        Daemon::Search => &screen.search,
        Daemon::Memory => &screen.memory,
    };
    let data = match panel {
        PanelState::Online(d) => d,
        PanelState::Offline { last_error } => {
            return vec![format!("offline: {last_error}")];
        }
        PanelState::Connecting => {
            return vec!["connecting…".to_string()];
        }
    };
    // The /health endpoint reports RSS in MB; the gauge maxes at 8 GB by
    // default (the documented memory ceiling); ratio is clamped to [0, 1].
    const MEM_CEILING_MB: u64 = 8 * 1024;
    let mem_ratio = (data.rss_mb as f64 / MEM_CEILING_MB as f64).clamp(0.0, 1.0);
    let mem_pct = (mem_ratio * 100.0).round() as u64;
    let disk_ratio = if data.disk_bytes > 0 {
        // Disk gauge is illustrative: a 10-GB axis keeps the bar useful for
        // typical developer codebases.
        const DISK_CEILING_BYTES: u64 = 10 * 1024 * 1024 * 1024;
        (data.disk_bytes as f64 / DISK_CEILING_BYTES as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    vec![
        format!(
            "Memory {bar} {used} / {cap} ({pct}%)",
            bar = ascii_bar(mem_ratio, 10),
            used = format_rss(data.rss_mb),
            cap = format_rss(MEM_CEILING_MB),
            pct = mem_pct,
        ),
        format!(
            "Disk   {bar} {used}",
            bar = ascii_bar(disk_ratio, 10),
            used = format_bytes(data.disk_bytes),
        ),
        String::new(),
        "Embedder: ready".to_string(),
        "CoreML:  batch=32  tripwire=4GB".to_string(),
    ]
}

/// Build an ASCII bar of length `width`, filled to `ratio` (0.0..=1.0).
///
/// Why: ratatui's `Gauge` widget paints with colour; the spec asks for the
/// fixed-width `████████░░` glyph form rendered as text. A pure helper keeps
/// the proportion arithmetic unit-testable.
/// What: returns a string with `filled` blocks (`█`) then `width-filled`
/// dots (`░`).
/// Test: `ascii_bar_fills_proportionally`.
pub fn ascii_bar(ratio: f64, width: usize) -> String {
    let r = ratio.clamp(0.0, 1.0);
    let filled = (r * width as f64).round() as usize;
    let filled = filled.min(width);
    let mut s = String::with_capacity(width * 3);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..width {
        s.push('░');
    }
    s
}

/// Render the HEALTH tab body.
fn render_health_tab(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let lines: Vec<Line> = health_tab_lines(screen)
        .into_iter()
        .map(Line::from)
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Render the LOGS tab body (scrollable ring buffer).
fn render_logs_tab(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let buf = screen.focused_logs();
    if buf.lines.is_empty() {
        let hint = "Log streaming not available — start daemon with RUST_LOG=debug";
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            ))),
            area,
        );
        return;
    }
    let height = area.height as usize;
    let total = buf.lines.len();
    // The view shows the bottom `height` lines minus the operator's scroll
    // offset. Auto-scroll means `scroll_offset == 0` (tail visible).
    let end = total.saturating_sub(buf.scroll_offset);
    let start = end.saturating_sub(height);
    let body: Vec<Line> = buf
        .lines
        .iter()
        .skip(start)
        .take(end - start)
        .map(|l| Line::from(l.clone()))
        .collect();
    frame.render_widget(Paragraph::new(body), area);
}

/// Render the SEARCH/RECALL tab body.
fn render_search_tab(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let lines = if screen.search_query.is_empty() {
        vec![
            Line::from(Span::styled(
                "Type a query in the search bar below.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(match screen.focus {
                Daemon::Search => "Searches the focused index for code chunks.",
                Daemon::Memory => "Recalls memories from the focused palace.",
            }),
        ]
    } else {
        vec![
            Line::from(Span::styled(
                format!("Query: {}", screen.search_query),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("(press Enter in the search bar to run — execution not yet wired)"),
        ]
    };
    frame.render_widget(Paragraph::new(lines), area);
}

/// Format a count with comma thousands separators (e.g. `1,234,567`).
///
/// Why: the detail panel surfaces graph stats that often run into the tens
/// of thousands; comma-grouping is easier to scan than a packed digit run.
/// What: walks the digits right-to-left and inserts a comma every three.
/// Test: `format_with_commas_groups_thousands`.
pub fn format_with_commas(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Build the INDEX tab body lines for a memory-palace row (the detail panel).
///
/// Why: the right detail panel needs palace-appropriate stats — vectors,
/// drawers, knowledge-graph triples, and the last-write time — rather than
/// the search-index stats `index_tab_lines` was originally built for.
/// What: returns a header section (`Vectors` / `Drawers` / `Wings`), a Graph
/// section (`Triples` / `Nodes` / `Edges` — the latter two are best-effort
/// and read from the row's KG-side fields if present), and a freshness
/// section (`Last write`). Numbers are comma-grouped for readability;
/// missing data renders as `N/A`.
/// Test: `palace_index_tab_lines_shows_graph_section`,
/// `palace_index_tab_lines_formats_last_write`.
pub fn palace_index_tab_lines(row: &CollectionRow) -> Vec<String> {
    let mut lines = Vec::with_capacity(12);

    // Header: vector / drawer / wing counts.
    lines.push(format!(
        "Vectors:    {:<12} Drawers: {}",
        format_with_commas(row.count),
        format_with_commas(row.drawer_count),
    ));
    lines.push(format!(
        "Wings:      {}",
        format_with_commas(row.wing_count),
    ));
    lines.push(String::new());

    // Graph section.
    lines.push("-- Knowledge Graph ------------------------------------".to_string());
    lines.push(format!("Triples:    {}", format_with_commas(row.kg_count),));
    let node_cell = if row.node_count == 0 {
        "N/A".to_string()
    } else {
        format_with_commas(row.node_count)
    };
    let edge_cell = if row.edge_count == 0 {
        "N/A".to_string()
    } else {
        format_with_commas(row.edge_count)
    };
    lines.push(format!(
        "Nodes:      {:<12} Edges: {}",
        node_cell, edge_cell,
    ));
    lines.push(String::new());

    // Freshness section: last write timestamp + activity state.
    lines.push("-- Activity -------------------------------------------".to_string());
    let last_write_human = match row.last_write_at.as_deref() {
        Some(s) => match chrono::DateTime::parse_from_rfc3339(s) {
            Ok(dt) => format!(
                "{} ({})",
                dt.format("%Y-%m-%d %H:%M"),
                format_relative_time(Some(s)),
            ),
            Err(_) => s.to_string(),
        },
        None => "never".to_string(),
    };
    lines.push(format!("Last write: {last_write_human}"));
    let state_label = match palace_activity(row) {
        PalaceActivity::Idle => "idle",
        PalaceActivity::Indexing => "indexing",
        PalaceActivity::Dreaming => "dreaming (compacting)",
        PalaceActivity::Active => "active (recent write)",
        PalaceActivity::Error => "error",
    };
    lines.push(format!("State:      {state_label}"));

    lines
}

/// Build the INDEX tab body lines for the currently-selected collection row.
///
/// Why: keeping the body builder pure lets a unit test assert the rendered
/// content without a terminal backend; the per-row stats live on the
/// [`CollectionRow`] so the renderer reads directly from the screen.
/// What: returns a header (`Chunks` / `Disk` / `Last index` / `Context`)
/// and a Graph section (`Nodes` / `Edges` + one bar per edge kind, scaled to
/// the largest kind with `MAX_BAR_WIDTH` blocks). If no collection is
/// selected (or the list is empty), returns a single placeholder line.
/// Test: `index_tab_lines_show_graph_stats`,
/// `index_tab_lines_show_edge_kind_bars`,
/// `index_tab_lines_empty_when_no_selection`.
pub fn index_tab_lines(screen: &HealthScreen) -> Vec<String> {
    /// Maximum width (in `█` chars) of the edge-kind histogram bars.
    ///
    /// Why: the right panel is sized to fit the bars plus a count column on
    /// 80-column terminals; 16 leaves room for both.
    /// What: the cap passed to [`ascii_bar`].
    const MAX_BAR_WIDTH: usize = 16;

    let rows = screen.focused_collections();
    if rows.is_empty() {
        return vec!["(no collection selected)".to_string()];
    }
    let Some(row) = rows.get(screen.selected_collection) else {
        return vec!["(no collection selected)".to_string()];
    };

    // Memory palaces have a different stat set than search indexes (no
    // chunks, no edge-kind histogram); branch out into a dedicated builder
    // so each focus stays readable.
    if matches!(screen.focus, Daemon::Memory) {
        return palace_index_tab_lines(row);
    }

    let mut lines = Vec::with_capacity(12);

    // Header lines: chunks, disk, last_indexed, context embedding.
    lines.push(format!(
        "Chunks:     {:<10} Disk: {}",
        format_count(row.count),
        format_bytes(row.disk_bytes),
    ));
    let last_indexed_human = match row.last_indexed.as_deref() {
        Some(s) => {
            // Render the absolute timestamp in compact form alongside the
            // relative badge. A parse failure falls back to the raw string.
            let abs = chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|_| s.to_string());
            format!("{abs} ({})", format_relative_time(Some(s)))
        }
        None => "never".to_string(),
    };
    lines.push(format!("Last index: {last_indexed_human}"));
    let context = if row.has_context_embedding {
        "embedded"
    } else {
        "none"
    };
    lines.push(format!("Context:    {context}"));
    lines.push(String::new());

    // Graph section.
    lines.push("-- Graph ----------------------------------------------".to_string());
    lines.push(format!(
        "Nodes:      {:<10} Edges:  {}",
        format_count(row.node_count),
        format_count(row.edge_count),
    ));
    let max_kind = row.edge_kinds.iter().map(|(_, c)| *c).max().unwrap_or(0);
    for (name, count) in &row.edge_kinds {
        let ratio = if max_kind == 0 {
            0.0
        } else {
            *count as f64 / max_kind as f64
        };
        let bar = ascii_bar(ratio, MAX_BAR_WIDTH);
        lines.push(format!("{:<16} {:>6}  {bar}", name, format_count(*count)));
    }

    lines
}

/// Render the INDEX tab body.
///
/// Why: kept separate so [`render_tab_panel`]'s match arm stays one line.
/// What: draws the `index_tab_lines` as a `Paragraph`. Section headers (`-- … --`)
/// render in cyan to match the existing HEALTH-tab section style.
fn render_index_tab(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let lines: Vec<Line> = index_tab_lines(screen)
        .into_iter()
        .map(|l| {
            if l.starts_with("--") {
                Line::from(Span::styled(
                    l,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(l)
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Render the footer zone: search input bar + key hint.
fn render_footer(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let cursor = if screen.search_input_focused { "_" } else { "" };
    let prompt = match screen.focus {
        Daemon::Search => "SEARCH ▶",
        Daemon::Memory => "RECALL ▶",
    };
    let input_line = format!("{prompt} {}{cursor}", screen.search_query);
    let input_style = if screen.search_input_focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    frame.render_widget(
        Paragraph::new(Line::from(input_line))
            .style(input_style)
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

/// Render one daemon panel into `area`.
///
/// Why: the two panels share their bordered-block + line-list layout; only the
/// title, body, and highlight differ.
/// What: draws a bordered [`Paragraph`] whose title carries the panel name,
/// coloured by liveness; a focused panel gets a bold cyan border.
/// Kept for callers that want the legacy side-by-side panel; the new
/// per-service render uses its own header / collections / tab helpers.
#[allow(dead_code)]
fn render_panel(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    name: &str,
    lines: &[String],
    online: bool,
    focused: bool,
) {
    let title_color = if online { Color::Green } else { Color::Red };
    let border_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let body: Vec<Line> = lines.iter().map(|l| Line::from(l.clone())).collect();
    frame.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(Span::styled(
                    format!(" {name} "),
                    Style::default()
                        .fg(title_color)
                        .add_modifier(Modifier::BOLD),
                )),
        ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    /// A sample online search payload for rendering tests.
    fn sample_search() -> PanelData {
        PanelData {
            version: "0.3.67".into(),
            rss_mb: 1280,
            cpu_pct: 4.0,
            uptime_secs: 3720,
            disk_bytes: 2_469_606_195,
            count_a: 3,
            count_b: 71_000,
            count_c: 0,
            count_d: 0,
        }
    }

    /// A sample online memory payload for rendering tests.
    fn sample_memory() -> PanelData {
        PanelData {
            version: "0.1.56".into(),
            rss_mb: 410,
            cpu_pct: 1.0,
            uptime_secs: 3720,
            disk_bytes: 104_857_600,
            count_a: 2,
            count_b: 8_400,
            count_c: 14,
            count_d: 1_200,
        }
    }

    #[test]
    fn default_urls_are_local() {
        assert_eq!(DEFAULT_SEARCH_URL, "http://127.0.0.1:7878");
        assert_eq!(DEFAULT_MEMORY_URL, "http://127.0.0.1:7990");
    }

    #[test]
    fn health_wire_deserializes_partial_payload() {
        // A daemon on an older build omits the issue-#35 resource fields; the
        // wire shape must still deserialize, defaulting the missing fields.
        let wire: HealthWire = serde_json::from_value(serde_json::json!({
            "status": "ok",
            "version": "0.3.67",
        }))
        .expect("partial health payload must deserialize");
        assert_eq!(wire.version, "0.3.67");
        assert_eq!(wire.rss_mb, 0);
        assert_eq!(wire.uptime_secs, 0);
    }

    #[test]
    fn project_memory_counts_reads_status_fields() {
        let status = serde_json::json!({
            "palace_count": 2,
            "total_vectors": 8400,
            "total_drawers": 14,
            "total_kg_triples": 1200,
        });
        assert_eq!(project_memory_counts(&status), (2, 8_400, 14, 1_200));
        // Absent fields default to zero rather than panicking.
        assert_eq!(project_memory_counts(&serde_json::json!({})), (0, 0, 0, 0));
    }

    #[test]
    fn format_uptime_is_compact() {
        assert_eq!(format_uptime(0), "0h 0m");
        assert_eq!(format_uptime(3720), "1h 2m");
        assert_eq!(format_uptime(7_380), "2h 3m");
    }

    #[test]
    fn format_bytes_picks_unit() {
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(2_048), "2.0KB");
        assert_eq!(format_bytes(5_242_880), "5.0MB");
        assert_eq!(format_bytes(2_469_606_195), "2.3GB");
    }

    #[test]
    fn format_rss_picks_unit() {
        assert_eq!(format_rss(410), "410MB");
        assert_eq!(format_rss(1_023), "1023MB");
        // 1280 MB / 1024 = 1.25 GB → one-decimal rounding yields 1.2GB.
        assert_eq!(format_rss(1_280), "1.2GB");
        assert_eq!(format_rss(1_536), "1.5GB");
    }

    #[test]
    fn format_count_abbreviates_large() {
        assert_eq!(format_count(3), "3");
        assert_eq!(format_count(9_999), "9999");
        assert_eq!(format_count(71_000), "71.0k");
    }

    #[test]
    fn panel_state_is_online() {
        assert!(PanelState::Online(PanelData::default()).is_online());
        assert!(!PanelState::Connecting.is_online());
        assert!(
            !PanelState::Offline {
                last_error: "x".into()
            }
            .is_online()
        );
    }

    #[test]
    fn search_panel_lines_format_fields() {
        let lines = search_panel_lines(
            &PanelState::Online(sample_search()),
            "http://127.0.0.1:7878",
        );
        assert!(lines.iter().any(|l| l.contains("SEARCH [●] v0.3.67")));
        // 1280 MB renders as 1.2GB (1280 / 1024 = 1.25, rounded to one place).
        assert!(lines.iter().any(|l| l.contains("RSS: 1.2GB")));
        assert!(lines.iter().any(|l| l.contains("CPU: 4%")));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Indexes: 3") && l.contains("Chunks: 71.0k"))
        );
        assert!(lines.iter().any(|l| l.contains("Disk: 2.3GB")));
        assert!(lines.iter().any(|l| l.contains("[S]start [X]stop")));
    }

    #[test]
    fn memory_panel_lines_format_fields() {
        let lines = memory_panel_lines(
            &PanelState::Online(sample_memory()),
            "http://127.0.0.1:7990",
        );
        assert!(lines.iter().any(|l| l.contains("MEMORY [●] v0.1.56")));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Palaces: 2") && l.contains("Vectors: 8400"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Drawers: 14") && l.contains("KG: 1200"))
        );
    }

    #[test]
    fn panel_lines_render_each_state() {
        // Connecting: a single placeholder line naming the target.
        let connecting = search_panel_lines(&PanelState::Connecting, "http://x");
        assert_eq!(connecting.len(), 1);
        assert!(connecting[0].contains("connecting"));

        // Offline: carries the unreachable address and the captured error.
        let offline = memory_panel_lines(
            &PanelState::Offline {
                last_error: "connection refused".into(),
            },
            "http://127.0.0.1:7990",
        );
        assert!(offline.iter().any(|l| l.contains("OFFLINE")));
        assert!(offline.iter().any(|l| l.contains("connection refused")));
        assert!(offline.iter().any(|l| l.contains("retrying every 5s")));
        assert!(offline.iter().any(|l| l.contains("[S]start [X]stop")));
    }

    #[test]
    fn online_panel_renders_missing_version_safely() {
        // A daemon that omitted `version` must not render `v` with nothing.
        let mut data = sample_search();
        data.version.clear();
        let lines = search_panel_lines(&PanelState::Online(data), "http://x");
        assert!(lines.iter().any(|l| l.contains("SEARCH [●] ?")));
    }

    #[test]
    fn new_screen_starts_connecting() {
        let screen = HealthScreen::new("http://a", "http://b");
        assert_eq!(screen.search, PanelState::Connecting);
        assert_eq!(screen.memory, PanelState::Connecting);
        assert_eq!(screen.search_url, "http://a");
        assert_eq!(screen.focus, Daemon::Search);
    }

    #[test]
    fn toggle_focus_cycles_panels() {
        let mut screen = HealthScreen::new("http://a", "http://b");
        assert_eq!(screen.focus, Daemon::Search);
        screen.toggle_focus();
        assert_eq!(screen.focus, Daemon::Memory);
        screen.toggle_focus();
        assert_eq!(screen.focus, Daemon::Search);
    }

    #[test]
    fn apply_update_routes_to_panel() {
        let mut screen = HealthScreen::new("http://a", "http://b");
        screen.apply_update(HealthUpdate {
            daemon: Daemon::Search,
            state: PanelState::Online(sample_search()),
        });
        assert!(screen.search.is_online());
        // The memory panel must be untouched by a search-targeted update.
        assert_eq!(screen.memory, PanelState::Connecting);

        screen.apply_update(HealthUpdate {
            daemon: Daemon::Memory,
            state: PanelState::Offline {
                last_error: "timeout".into(),
            },
        });
        assert!(matches!(screen.memory, PanelState::Offline { .. }));
        // The search panel must still be online.
        assert!(screen.search.is_online());
    }

    #[test]
    fn focused_url_follows_focus() {
        let mut screen = HealthScreen::new("http://search", "http://memory");
        assert_eq!(screen.focused_url(), "http://search");
        screen.toggle_focus();
        assert_eq!(screen.focused_url(), "http://memory");
    }

    #[test]
    fn health_client_stores_base_url() {
        let client = client_for(Daemon::Search, "http://127.0.0.1:7878");
        assert_eq!(client.base_url(), "http://127.0.0.1:7878");
    }

    #[tokio::test]
    async fn poll_unreachable_daemon_is_offline() {
        // Port 0 is never bound; a poll against it must yield Offline carrying
        // a non-empty error string rather than panicking.
        let client = client_for(Daemon::Memory, "http://127.0.0.1:0");
        match client.poll().await {
            PanelState::Offline { last_error } => assert!(!last_error.is_empty()),
            other => panic!("expected Offline, got {other:?}"),
        }
    }

    #[test]
    fn tab_default_is_health() {
        // The redesigned screen opens on the Health tab.
        assert_eq!(HealthTab::default(), HealthTab::Health);
    }

    #[test]
    fn tab_switch_keys_route() {
        // `set_tab` stores the requested tab and auto-focuses the search
        // input only when switching to the Search tab (per the spec).
        let mut screen = HealthScreen::new("http://a", "http://b");
        assert!(!screen.search_input_focused);
        screen.set_tab(HealthTab::Logs);
        assert_eq!(screen.tab, HealthTab::Logs);
        assert!(!screen.search_input_focused);
        screen.set_tab(HealthTab::Search);
        assert_eq!(screen.tab, HealthTab::Search);
        assert!(screen.search_input_focused);
        screen.set_tab(HealthTab::Health);
        assert!(!screen.search_input_focused);
    }

    #[test]
    fn log_buffer_starts_empty() {
        let buf = LogBuffer::new();
        assert!(buf.lines.is_empty());
        assert!(buf.auto_scroll);
        assert_eq!(buf.scroll_offset, 0);
    }

    #[test]
    fn log_buffer_evicts_oldest() {
        // Pushing past the cap evicts the oldest entries; the newest stay.
        let mut buf = LogBuffer::new();
        for i in 0..(LOG_BUFFER_CAP + 10) {
            buf.push(format!("line {i}"));
        }
        assert_eq!(buf.lines.len(), LOG_BUFFER_CAP);
        assert_eq!(buf.lines.front().map(String::as_str), Some("line 10"));
        assert_eq!(
            buf.lines.back().map(String::as_str),
            Some(format!("line {}", LOG_BUFFER_CAP + 9)).as_deref()
        );
    }

    #[test]
    fn log_buffer_replace_caps_at_limit() {
        // `replace` swaps in a fresh tail but never holds more than the cap.
        let mut buf = LogBuffer::new();
        let huge: Vec<String> = (0..(LOG_BUFFER_CAP * 2)).map(|i| format!("l{i}")).collect();
        buf.replace(huge, Some(9999));
        assert_eq!(buf.lines.len(), LOG_BUFFER_CAP);
        assert_eq!(buf.total_seen, 9999);
    }

    #[test]
    fn log_buffer_scroll_clamps() {
        // ↑ disables auto-scroll and saturates at the top; ↓ re-enables auto-scroll
        // when it returns to the tail.
        let mut buf = LogBuffer::new();
        for i in 0..5 {
            buf.push(format!("l{i}"));
        }
        assert!(buf.auto_scroll);
        buf.scroll_up();
        assert!(!buf.auto_scroll);
        assert_eq!(buf.scroll_offset, 1);
        for _ in 0..20 {
            buf.scroll_up();
        }
        // Saturates at `lines.len() - 1` (4 here).
        assert_eq!(buf.scroll_offset, 4);
        for _ in 0..10 {
            buf.scroll_down();
        }
        assert_eq!(buf.scroll_offset, 0);
        assert!(buf.auto_scroll);
    }

    #[test]
    fn log_buffer_snap_to_tail() {
        let mut buf = LogBuffer::new();
        for i in 0..3 {
            buf.push(format!("l{i}"));
        }
        buf.scroll_up();
        buf.scroll_up();
        assert!(!buf.auto_scroll);
        buf.snap_to_tail();
        assert_eq!(buf.scroll_offset, 0);
        assert!(buf.auto_scroll);
    }

    #[test]
    fn project_log_tail_reads_fields() {
        let body = serde_json::json!({
            "lines": ["a", "b", "c"],
            "total": 42u64,
        });
        let (lines, total) = project_log_tail(&body);
        assert_eq!(
            lines,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(total, 42);
        // Absent `total` falls back to `lines.len()`.
        let body = serde_json::json!({ "lines": ["a", "b"] });
        let (_, total) = project_log_tail(&body);
        assert_eq!(total, 2);
        // Absent `lines` yields an empty list.
        let body = serde_json::json!({});
        let (lines, _) = project_log_tail(&body);
        assert!(lines.is_empty());
    }

    #[test]
    fn project_palace_rows_reads_palaces() {
        // The wire format from `GET /api/v1/palaces` is a JSON array of
        // `PalaceInfo`. Each entry carries the per-palace vector and KG
        // triple counts. Palaces with non-zero vector OR KG counts pass
        // through; the empty-filter behaviour is covered separately by
        // `project_palace_rows_filters_empty`.
        let list = serde_json::json!([
            { "name": "default", "vector_count": 8400u64, "kg_triple_count": 1200u64 },
            { "name": "work",    "vector_count": 0u64,    "kg_triple_count": 42u64 },
        ]);
        let rows = project_palace_rows(&list);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "default");
        assert_eq!(rows[0].count, 8400);
        assert_eq!(rows[0].kg_count, 1200);
        assert!(rows[0].ok);
        // Empty note: the new row format shows vectors + graph inline, so the
        // trailing badge is intentionally suppressed.
        assert!(rows[0].note.is_empty());
        // A KG-only palace still passes through (only fully-empty palaces are
        // dropped).
        assert_eq!(rows[1].id, "work");
        assert_eq!(rows[1].count, 0);
        assert_eq!(rows[1].kg_count, 42);
        // A non-array payload yields an empty list (e.g. an unexpected object).
        assert!(project_palace_rows(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn project_palace_rows_filters_empty() {
        // Why: a palace with zero vectors AND zero KG triples is just a
        // placeholder — listing it in the left pane adds noise without
        // surfacing any actionable state. The filter drops those rows.
        let list = serde_json::json!([
            { "name": "live",    "vector_count": 10u64, "kg_triple_count": 0u64 },
            { "name": "empty",   "vector_count": 0u64,  "kg_triple_count": 0u64 },
            { "name": "kg-only", "vector_count": 0u64,  "kg_triple_count": 5u64 },
            { "name": "both",    "vector_count": 3u64,  "kg_triple_count": 7u64 },
            // Missing fields default to zero and are treated as empty.
            { "name": "stub" },
        ]);
        let rows = project_palace_rows(&list);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["live", "kg-only", "both"]);
    }

    #[test]
    fn format_count_suffix_handles_zero_and_value() {
        // Zero renders as `--<suffix>` so the absence is visible at a glance.
        assert_eq!(format_count_suffix(0, 'v'), "--v");
        assert_eq!(format_count_suffix(0, 'g'), "--g");
        // Non-zero re-uses `format_count`'s abbreviation.
        assert_eq!(format_count_suffix(42, 'v'), "42v");
        assert_eq!(format_count_suffix(12_345, 'g'), "12.3kg");
    }

    #[test]
    fn service_name_matches_focus() {
        assert_eq!(service_name(Daemon::Search), "trusty-search");
        assert_eq!(service_name(Daemon::Memory), "trusty-memory");
    }

    #[test]
    fn header_lines_show_focus_summary() {
        // An online panel renders the version + resource snapshot.
        let mut screen = HealthScreen::new(DEFAULT_SEARCH_URL, DEFAULT_MEMORY_URL);
        screen.search = PanelState::Online(sample_search());
        let lines = header_lines(&screen);
        assert!(lines[0].contains("trusty-search"));
        assert!(lines[0].contains("v0.3.67"));
        assert!(lines[0].contains("ONLINE"));
        assert!(lines[1].contains("RSS:"));
        assert!(lines[1].contains("CPU:"));
        assert!(lines[1].contains("Uptime:"));

        // An offline panel shows OFFLINE + the captured error.
        screen.search = PanelState::Offline {
            last_error: "connection refused".into(),
        };
        let lines = header_lines(&screen);
        assert!(lines[0].contains("OFFLINE"));
        assert!(lines[1].contains("connection refused"));
    }

    #[test]
    fn tab_bar_marks_active() {
        let bar = tab_bar(HealthTab::Logs);
        // Each entry is (label, active); exactly one is active.
        let active_count = bar.iter().filter(|(_, a)| *a).count();
        assert_eq!(active_count, 1);
        let logs_active = bar
            .iter()
            .find(|(l, _)| l.contains("LOGS"))
            .map(|(_, a)| *a)
            .unwrap();
        assert!(logs_active);
    }

    #[test]
    fn collection_row_default_is_empty() {
        let row = CollectionRow::default();
        assert!(row.id.is_empty());
        assert_eq!(row.count, 0);
        assert!(!row.ok);
    }

    #[test]
    fn collections_lines_format_each_row() {
        let mut screen = HealthScreen::new("http://a", "http://b");
        screen.search_collections = vec![
            CollectionRow {
                id: "cto".into(),
                count: 1_200,
                note: "indexed".into(),
                ok: true,
                ..Default::default()
            },
            CollectionRow {
                id: "trusty".into(),
                count: 18_994,
                note: "indexed".into(),
                ok: true,
                ..Default::default()
            },
        ];
        let lines = collections_lines(&screen);
        assert_eq!(lines.len(), 2);
        // The first row is the highlighted one (selected_collection == 0).
        assert!(lines[0].starts_with(">"));
        assert!(lines[1].starts_with(" "));
        assert!(lines[0].contains("cto"));
        assert!(lines[1].contains("trusty"));
    }

    #[test]
    fn collections_lines_show_graph_count_for_memory() {
        // Memory focus: each row renders the vector count + KG triple count
        // inline, suffixed with `v` / `g`. No trailing `[note]` badge.
        let mut screen = HealthScreen::new("http://a", "http://b");
        screen.focus = Daemon::Memory;
        screen.memory_collections = vec![CollectionRow {
            id: "default".into(),
            count: 12,
            kg_count: 34,
            ok: true,
            ..Default::default()
        }];
        let lines = collections_lines(&screen);
        assert_eq!(lines.len(), 1);
        // Vector + graph counts appear with their suffixes.
        assert!(
            lines[0].contains("12v"),
            "expected `12v` in {line:?}",
            line = lines[0]
        );
        assert!(
            lines[0].contains("34g"),
            "expected `34g` in {line:?}",
            line = lines[0]
        );
        // The palace name is rendered.
        assert!(lines[0].contains("default"));
        // No `ready` badge from the legacy format.
        assert!(!lines[0].contains("ready"));
        assert!(!lines[0].contains("["));
    }

    #[test]
    fn collections_lines_show_dashes_for_zero_counts() {
        // A palace with no vectors and no KG triples shows `--v` / `--g` so
        // the operator can spot empty palaces at a glance.
        let mut screen = HealthScreen::new("http://a", "http://b");
        screen.focus = Daemon::Memory;
        screen.memory_collections = vec![CollectionRow {
            id: "empty".into(),
            count: 0,
            kg_count: 0,
            ok: true,
            ..Default::default()
        }];
        let lines = collections_lines(&screen);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("--v"),
            "expected `--v` for zero vectors in {line:?}",
            line = lines[0]
        );
        assert!(
            lines[0].contains("--g"),
            "expected `--g` for zero KG triples in {line:?}",
            line = lines[0]
        );
        // `0v` / `0g` would be ambiguous with abbreviated counts; we render
        // dashes instead.
        assert!(!lines[0].contains("0v"));
        assert!(!lines[0].contains("0g"));
    }

    #[test]
    fn collections_for_focus() {
        let mut screen = HealthScreen::new("http://a", "http://b");
        screen.search_collections = vec![CollectionRow {
            id: "i".into(),
            ..Default::default()
        }];
        screen.memory_collections = vec![
            CollectionRow {
                id: "p1".into(),
                ..Default::default()
            },
            CollectionRow {
                id: "p2".into(),
                ..Default::default()
            },
        ];
        assert_eq!(screen.focused_collections().len(), 1);
        screen.toggle_focus();
        assert_eq!(screen.focused_collections().len(), 2);
    }

    #[test]
    fn select_collection_saturates() {
        let mut screen = HealthScreen::new("http://a", "http://b");
        screen.search_collections = vec![
            CollectionRow {
                id: "a".into(),
                ..Default::default()
            },
            CollectionRow {
                id: "b".into(),
                ..Default::default()
            },
        ];
        // Down moves toward the end, saturates at len-1.
        screen.select_collection_down();
        assert_eq!(screen.selected_collection, 1);
        screen.select_collection_down();
        assert_eq!(screen.selected_collection, 1);
        // Up saturates at 0.
        screen.select_collection_up();
        assert_eq!(screen.selected_collection, 0);
        screen.select_collection_up();
        assert_eq!(screen.selected_collection, 0);
    }

    #[test]
    fn select_collection_clamps_after_shrink() {
        let mut screen = HealthScreen::new("http://a", "http://b");
        screen.search_collections = vec![CollectionRow {
            id: "a".into(),
            ..Default::default()
        }];
        screen.selected_collection = 99;
        screen.clamp_collection_selection();
        assert_eq!(screen.selected_collection, 0);
        screen.search_collections.clear();
        screen.clamp_collection_selection();
        assert_eq!(screen.selected_collection, 0);
    }

    #[test]
    fn ascii_bar_fills_proportionally() {
        assert_eq!(ascii_bar(0.0, 10), "░░░░░░░░░░");
        assert_eq!(ascii_bar(1.0, 10), "██████████");
        // Half full: 5 blocks + 5 dots.
        let half = ascii_bar(0.5, 10);
        assert_eq!(half.chars().filter(|c| *c == '█').count(), 5);
        assert_eq!(half.chars().filter(|c| *c == '░').count(), 5);
        // Out-of-range ratios are clamped.
        assert_eq!(ascii_bar(2.0, 4), "████");
        assert_eq!(ascii_bar(-1.0, 4), "░░░░");
    }

    #[test]
    fn health_tab_lines_show_gauges() {
        let mut screen = HealthScreen::new(DEFAULT_SEARCH_URL, DEFAULT_MEMORY_URL);
        screen.search = PanelState::Online(sample_search());
        let lines = health_tab_lines(&screen);
        assert!(lines.iter().any(|l| l.starts_with("Memory ")));
        assert!(lines.iter().any(|l| l.starts_with("Disk   ")));
        assert!(lines.iter().any(|l| l.contains("Embedder")));
        assert!(lines.iter().any(|l| l.contains("CoreML")));
    }

    #[test]
    fn format_relative_time_handles_known_offsets() {
        assert_eq!(format_relative_time(None), "never");
        assert_eq!(format_relative_time(Some("not-a-time")), "never");
        let now = chrono::Utc::now();
        let mk = |d: chrono::Duration| (now - d).to_rfc3339();
        assert_eq!(
            format_relative_time(Some(&mk(chrono::Duration::minutes(5)))),
            "5m ago"
        );
        assert_eq!(
            format_relative_time(Some(&mk(chrono::Duration::hours(2)))),
            "2h ago"
        );
        assert_eq!(
            format_relative_time(Some(&mk(chrono::Duration::days(3)))),
            "3d ago"
        );
        let future = (now + chrono::Duration::minutes(5)).to_rfc3339();
        assert_eq!(format_relative_time(Some(&future)), "just now");
    }

    #[test]
    fn project_edge_kinds_sorts_desc() {
        let stats = serde_json::json!({
            "edge_kinds": {
                "CallsFunction": 8201u64,
                "Implements":    1422u64,
                "UsesType":      2411u64,
            }
        });
        let kinds = project_edge_kinds(&stats);
        assert_eq!(
            kinds,
            vec![
                ("CallsFunction".to_string(), 8201),
                ("UsesType".to_string(), 2411),
                ("Implements".to_string(), 1422),
            ]
        );
        assert!(project_edge_kinds(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn collections_lines_show_relative_time() {
        let mut screen = HealthScreen::new("http://a", "http://b");
        let ts = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
        screen.search_collections = vec![CollectionRow {
            id: "trusty".into(),
            count: 71_000,
            note: String::new(),
            ok: true,
            last_indexed: Some(ts),
            ..Default::default()
        }];
        let lines = collections_lines(&screen);
        assert!(lines[0].contains("[5m ago]"));
    }

    #[test]
    fn index_tab_lines_show_graph_stats() {
        let mut screen = HealthScreen::new("http://a", "http://b");
        screen.search_collections = vec![CollectionRow {
            id: "trusty".into(),
            count: 71_000,
            ok: true,
            disk_bytes: 2_469_606_195,
            node_count: 4_821,
            edge_count: 12_034,
            community_count: 47,
            modularity: 0.712,
            has_context_embedding: true,
            ..Default::default()
        }];
        let lines = index_tab_lines(&screen);
        assert!(lines.iter().any(|l| l.starts_with("Chunks:")));
        assert!(lines.iter().any(|l| l.contains("Disk: 2.3GB")));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Context:") && l.contains("embedded"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Nodes:") && l.contains("Edges:"))
        );
        // The Communities display (`Count:` / `Modularity:` row) was retired;
        // the data still flows on the row but must not render in the UI.
        assert!(
            !lines.iter().any(|l| l.contains("Modularity")),
            "Modularity must not appear in the index tab"
        );
        assert!(
            !lines.iter().any(|l| l.contains("Communities")),
            "Communities section must not appear in the index tab"
        );
    }

    #[test]
    fn index_tab_lines_show_edge_kind_bars() {
        let mut screen = HealthScreen::new("http://a", "http://b");
        screen.search_collections = vec![CollectionRow {
            id: "trusty".into(),
            edge_kinds: vec![
                ("CallsFunction".to_string(), 8_201),
                ("UsesType".to_string(), 2_411),
                ("Implements".to_string(), 1_422),
            ],
            ..Default::default()
        }];
        let lines = index_tab_lines(&screen);
        let calls = lines.iter().find(|l| l.contains("CallsFunction")).unwrap();
        let impls = lines.iter().find(|l| l.contains("Implements")).unwrap();
        let calls_blocks = calls.chars().filter(|c| *c == '█').count();
        let impls_blocks = impls.chars().filter(|c| *c == '█').count();
        assert!(calls_blocks >= impls_blocks);
        assert!(calls_blocks > 0);
    }

    #[test]
    fn index_tab_lines_empty_when_no_selection() {
        let screen = HealthScreen::new("http://a", "http://b");
        let lines = index_tab_lines(&screen);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("no collection selected"));
    }

    #[test]
    fn palace_activity_from_recent_write() {
        // No timestamp → Idle.
        let mut row = CollectionRow {
            id: "p".into(),
            count: 10,
            ok: true,
            ..Default::default()
        };
        assert_eq!(palace_activity(&row), PalaceActivity::Idle);

        // !ok → Error (regardless of timestamp).
        let mut bad = row.clone();
        bad.ok = false;
        assert_eq!(palace_activity(&bad), PalaceActivity::Error);

        // Write within 10s → Indexing.
        row.last_write_at = Some((chrono::Utc::now() - chrono::Duration::seconds(2)).to_rfc3339());
        assert_eq!(palace_activity(&row), PalaceActivity::Indexing);

        // Write within last minute (but >10s) → Active.
        row.last_write_at = Some((chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc3339());
        assert_eq!(palace_activity(&row), PalaceActivity::Active);

        // Write older than a minute → Idle.
        row.last_write_at = Some((chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339());
        assert_eq!(palace_activity(&row), PalaceActivity::Idle);

        // Unparseable timestamp → Idle.
        row.last_write_at = Some("not-a-date".into());
        assert_eq!(palace_activity(&row), PalaceActivity::Idle);
    }

    /// Why: When the memory daemon flags a palace as compacting the MEMORY
    /// tab must render the dreaming spinner regardless of how recently the
    /// palace was written to. The compacting flag must therefore dominate
    /// the timestamp heuristic.
    /// What: Sets `is_compacting = true` on a row with a fresh write and
    /// asserts the activity classifier returns `Dreaming`.
    /// Test: This test itself.
    #[test]
    fn palace_activity_marks_compacting_as_dreaming() {
        let row = CollectionRow {
            id: "p".into(),
            count: 10,
            ok: true,
            is_compacting: true,
            last_write_at: Some(chrono::Utc::now().to_rfc3339()),
            ..Default::default()
        };
        assert_eq!(palace_activity(&row), PalaceActivity::Dreaming);

        // An unhealthy row still trumps the compaction flag — operators
        // must see the error indicator first.
        let bad = CollectionRow {
            ok: false,
            is_compacting: true,
            ..row.clone()
        };
        assert_eq!(palace_activity(&bad), PalaceActivity::Error);
    }

    /// Why: The new wire fields (`node_count`, `edge_count`,
    /// `community_count`, `is_compacting`) must surface verbatim through the
    /// projection so the renderer can rely on the row alone.
    /// What: Feeds a synthetic `/api/v1/palaces` payload carrying every new
    /// field and asserts each surfaces with the expected typed value.
    /// Test: This test itself.
    #[test]
    fn project_palace_rows_reads_is_compacting() {
        let list = serde_json::json!([
            {
                "name": "main",
                "vector_count": 100u64,
                "kg_triple_count": 50u64,
                "node_count": 42u64,
                "edge_count": 84u64,
                "community_count": 3u64,
                "is_compacting": true,
            },
        ]);
        let rows = project_palace_rows(&list);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].node_count, 42);
        assert_eq!(rows[0].edge_count, 84);
        assert_eq!(rows[0].community_count, 3);
        assert!(rows[0].is_compacting);
    }

    #[test]
    fn spinner_frame_for_each_state() {
        assert_eq!(spinner_frame(PalaceActivity::Idle, 0), None);
        assert_eq!(spinner_frame(PalaceActivity::Error, 0), Some('✗'));
        assert_eq!(spinner_frame(PalaceActivity::Active, 0), Some('⠿'));
        // Indexing and Dreaming both yield Some(frame).
        assert!(spinner_frame(PalaceActivity::Indexing, 0).is_some());
        assert!(spinner_frame(PalaceActivity::Dreaming, 0).is_some());
    }

    #[test]
    fn spinner_frame_cycles_through_indexing_frames() {
        // Each tick picks the next frame; the cycle wraps cleanly.
        let f0 = spinner_frame(PalaceActivity::Indexing, 0).unwrap();
        let f1 = spinner_frame(PalaceActivity::Indexing, 1).unwrap();
        let f_wrap = spinner_frame(PalaceActivity::Indexing, INDEXING_SPINNER.len()).unwrap();
        assert_ne!(f0, f1);
        assert_eq!(f0, f_wrap);
    }

    #[test]
    fn spinner_frame_cycles_through_dreaming_frames() {
        let f0 = spinner_frame(PalaceActivity::Dreaming, 0).unwrap();
        let f_wrap = spinner_frame(PalaceActivity::Dreaming, DREAMING_SPINNER.len()).unwrap();
        assert_eq!(f0, f_wrap);
    }

    #[test]
    fn activity_colour_is_distinct_per_state() {
        // Idle is the only state that uses the terminal default.
        assert_eq!(activity_color(PalaceActivity::Idle), Color::Reset);
        // The other states each pick a non-default colour and none collide.
        let colours = [
            activity_color(PalaceActivity::Indexing),
            activity_color(PalaceActivity::Dreaming),
            activity_color(PalaceActivity::Active),
            activity_color(PalaceActivity::Error),
        ];
        for c in &colours {
            assert_ne!(*c, Color::Reset);
        }
        let mut sorted = colours.to_vec();
        sorted.sort_by_key(|c| format!("{c:?}"));
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            4,
            "every non-idle state needs a unique colour"
        );
    }

    #[test]
    fn collections_lines_at_tick_shows_indexing_spinner() {
        // A memory palace whose last_write_at is in the indexing window
        // gets a spinner glyph from the indexing frame set.
        let mut screen = HealthScreen::new("http://a", "http://b");
        screen.focus = Daemon::Memory;
        screen.memory_collections = vec![CollectionRow {
            id: "fresh".into(),
            count: 1,
            kg_count: 0,
            ok: true,
            last_write_at: Some((chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339()),
            ..Default::default()
        }];
        let lines = collections_lines_at_tick(&screen, 0);
        assert_eq!(lines.len(), 1);
        let expected = INDEXING_SPINNER[0];
        assert!(
            lines[0].contains(expected),
            "expected indexing spinner {expected} in {line:?}",
            line = lines[0],
        );
    }

    #[test]
    fn collections_lines_at_tick_idle_palace_has_no_spinner() {
        // An idle palace renders with a space where the spinner would go,
        // so none of the animated glyphs appear on the line.
        let mut screen = HealthScreen::new("http://a", "http://b");
        screen.focus = Daemon::Memory;
        screen.memory_collections = vec![CollectionRow {
            id: "idle".into(),
            count: 1,
            kg_count: 0,
            ok: true,
            last_write_at: None,
            ..Default::default()
        }];
        let lines = collections_lines_at_tick(&screen, 0);
        for ch in INDEXING_SPINNER.iter().chain(DREAMING_SPINNER.iter()) {
            assert!(
                !lines[0].contains(*ch),
                "idle palace must not show spinner glyph {ch} in {line:?}",
                line = lines[0],
            );
        }
        // Nor the active / error markers.
        assert!(!lines[0].contains('⠿'));
        assert!(!lines[0].contains('✗'));
    }

    #[test]
    fn format_with_commas_groups_thousands() {
        assert_eq!(format_with_commas(0), "0");
        assert_eq!(format_with_commas(42), "42");
        assert_eq!(format_with_commas(1_234), "1,234");
        assert_eq!(format_with_commas(1_234_567), "1,234,567");
        assert_eq!(format_with_commas(1_000_000_000), "1,000,000,000");
    }

    #[test]
    fn project_palace_rows_reads_extended_fields() {
        // The wire shape exposes drawer_count, wing_count, and last_write_at
        // alongside the existing fields. The projection must surface them so
        // the detail panel can render them.
        let ts = chrono::Utc::now().to_rfc3339();
        let list = serde_json::json!([
            {
                "name": "main",
                "vector_count": 100u64,
                "kg_triple_count": 50u64,
                "drawer_count": 7u64,
                "wing_count": 3u64,
                "last_write_at": ts,
            },
        ]);
        let rows = project_palace_rows(&list);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].drawer_count, 7);
        assert_eq!(rows[0].wing_count, 3);
        assert_eq!(rows[0].last_write_at.as_deref(), Some(ts.as_str()));
    }

    #[test]
    fn palace_index_tab_lines_shows_graph_section() {
        // The memory detail panel surfaces vectors / drawers / wings on the
        // header, then a Knowledge Graph section with triples, then an
        // Activity section. Counts render with comma grouping.
        let row = CollectionRow {
            id: "main".into(),
            count: 12_345,
            kg_count: 6_789,
            drawer_count: 42,
            wing_count: 3,
            ok: true,
            ..Default::default()
        };
        let lines = palace_index_tab_lines(&row);
        assert!(lines.iter().any(|l| l.starts_with("Vectors:")));
        assert!(lines.iter().any(|l| l.contains("12,345")));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Drawers:") && l.contains("42"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Wings:") && l.contains("3"))
        );
        assert!(lines.iter().any(|l| l.contains("Knowledge Graph")));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Triples:") && l.contains("6,789"))
        );
        // Without graph nodes/edges on the wire, those slots render as N/A
        // rather than 0 so the operator knows they were absent.
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Nodes:") && l.contains("N/A"))
        );
        // The Communities display was retired; verify it no longer surfaces.
        assert!(
            !lines.iter().any(|l| l.contains("Communities")),
            "Communities display must not appear in palace index tab"
        );
        assert!(lines.iter().any(|l| l.contains("Activity")));
        assert!(lines.iter().any(|l| l.starts_with("State:")));
    }

    #[test]
    fn palace_index_tab_lines_formats_last_write() {
        // A recent write renders the relative + absolute time and the state
        // line tracks it ("active" within 60s).
        let ts = (chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc3339();
        let row = CollectionRow {
            id: "main".into(),
            count: 1,
            ok: true,
            last_write_at: Some(ts),
            ..Default::default()
        };
        let lines = palace_index_tab_lines(&row);
        let last_line = lines
            .iter()
            .find(|l| l.starts_with("Last write:"))
            .expect("must include Last write line");
        // The absolute date prefix and the relative badge both render.
        assert!(last_line.contains("ago") || last_line.contains("just now"));
        let state_line = lines
            .iter()
            .find(|l| l.starts_with("State:"))
            .expect("must include State line");
        assert!(state_line.contains("active"));

        // No timestamp falls back to "never" and Idle.
        let idle = CollectionRow {
            id: "x".into(),
            count: 1,
            ok: true,
            last_write_at: None,
            ..Default::default()
        };
        let lines = palace_index_tab_lines(&idle);
        assert!(lines.iter().any(|l| l.contains("never")));
        assert!(lines.iter().any(|l| l.contains("idle")));
    }

    #[test]
    fn index_tab_lines_routes_to_palace_when_focus_memory() {
        // When focus is Memory, the dispatcher hands the row to the palace
        // builder, which produces the Knowledge Graph header (not "-- Graph").
        let mut screen = HealthScreen::new("http://a", "http://b");
        screen.focus = Daemon::Memory;
        screen.memory_collections = vec![CollectionRow {
            id: "main".into(),
            count: 10,
            kg_count: 5,
            ok: true,
            ..Default::default()
        }];
        let lines = index_tab_lines(&screen);
        assert!(lines.iter().any(|l| l.contains("Knowledge Graph")));
        // The search-only edge-kind histogram header must NOT appear here.
        assert!(
            !lines
                .iter()
                .any(|l| l == "-- Graph ----------------------------------------------")
        );
    }

    #[test]
    fn render_health_smoke() {
        // A whole-frame render in every panel state must not panic.
        let mut screen = HealthScreen::new(DEFAULT_SEARCH_URL, DEFAULT_MEMORY_URL);
        screen.search = PanelState::Online(sample_search());
        screen.memory = PanelState::Offline {
            last_error: "connection refused".into(),
        };
        screen.focus = Daemon::Memory;
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| render(f, &screen))
            .expect("health render must not panic");
    }
}
