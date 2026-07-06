//! Pure data types and constants for the health screen.
//!
//! Why: keeping the wire shapes, the projected per-daemon payloads, the
//! connection-state enum, and the screen/struct definitions in one place lets
//! the transport (`probes`), the formatters (`format`), the line builders
//! (`screen`), and the renderer (`render`) all depend on a single, dependency-free
//! data layer.
//! What: the default URLs / poll cadence / buffer-cap constants, the [`Daemon`]
//! / [`HealthTab`] / [`PalaceActivity`] tags, the [`CollectionRow`] /
//! [`LogBuffer`] / [`PanelData`] / [`PanelState`] data structs (with their
//! self-contained impls), the [`HealthWire`] deserialization target, the
//! [`HealthUpdate`] message, and the [`HealthClient`] / [`HealthScreen`] struct
//! definitions (their impls live in `probes` and `screen` respectively).
//! Test: `cargo test -p trusty-mpm` exercises the projections and line builders
//! that consume these types.

use std::time::Duration;

use serde::Deserialize;

/// Default trusty-search daemon address used when no override is supplied.
///
/// Why: the health screen must always have a target to probe; the search
/// daemon binds `127.0.0.1:7878` by convention.
/// What: the canonical local trusty-search HTTP base URL.
/// Test: `default_urls_are_local`.
pub const DEFAULT_SEARCH_URL: &str = "http://127.0.0.1:7878";

/// Interval between health polls for each panel.
///
/// Why: the ticket mandates a 5-second refresh cadence for both the online and
/// the offline (retry) paths.
/// What: five seconds.
/// Test: exercised indirectly by the background poller.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

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
pub(crate) struct HealthWire {
    #[serde(default)]
    pub(crate) version: String,
    #[serde(default)]
    pub(crate) rss_mb: u64,
    #[serde(default)]
    pub(crate) cpu_pct: f32,
    #[serde(default)]
    pub(crate) uptime_secs: u64,
    #[serde(default)]
    pub(crate) disk_bytes: u64,
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
    pub(crate) base: String,
    pub(crate) daemon: Daemon,
    pub(crate) http: reqwest::Client,
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
