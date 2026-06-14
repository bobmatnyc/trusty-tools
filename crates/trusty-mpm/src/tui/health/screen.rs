//! [`HealthScreen`] state methods and the pure line-builder helpers.
//!
//! Why: the event loop folds poll results into a [`HealthScreen`] and the
//! renderer reads pure line builders off it; keeping the state transitions and
//! the terminal-free line builders together (away from the ratatui widget
//! assembly in `render`) keeps both surfaces small and independently testable.
//! What: the [`HealthScreen`] impl (focus / selection / tab / update methods),
//! the service-name + header + tab-bar helpers, the collections-list builders,
//! and the per-tab body builders (`health_tab_lines`, `index_tab_lines`,
//! `palace_index_tab_lines`).
//! Test: `toggle_focus_cycles_panels`, `header_lines_show_focus_summary`,
//! `collections_lines_format_each_row`, `index_tab_lines_show_graph_stats`.

use crate::tui::health::activity::{current_spinner_tick, palace_activity, spinner_frame};
use crate::tui::health::format::{
    ascii_bar, format_bytes, format_count, format_relative_time, format_rss, format_uptime,
    format_with_commas,
};
use crate::tui::health::types::{
    CollectionRow, Daemon, HealthScreen, HealthTab, HealthUpdate, LogBuffer, PalaceActivity,
    PanelState,
};

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
pub(crate) fn format_count_suffix(n: u64, suffix: char) -> String {
    if n == 0 {
        format!("--{suffix}")
    } else {
        format!("{}{suffix}", format_count(n))
    }
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
