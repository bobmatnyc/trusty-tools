//! Service-specific terminal UI for the trusty-search daemon.
//!
//! Why: operators of the trusty-search daemon want a focused, live terminal
//! surface — an index list, a streaming activity log of reindex/search events,
//! and a query bar — rather than the generic two-daemon dashboard. Living in
//! `trusty-common` behind the `monitor-tui` feature keeps the pure state /
//! rendering testable without a separate published crate (issue #34).
//! What: a ratatui app with a 3-zone layout (title bar, INDEXES + right-hand
//! split, SEARCH input bar). The INDEXES list always leads with an "All
//! indexes" entry that fans queries out across every index; the right side is
//! split vertically into an ACTIVITY feed (top) and a STATISTICS panel
//! (bottom), both scoped to the selected index — or aggregated when "All" is
//! selected. It polls the daemon every 2 seconds, streams reindex progress
//! over SSE on `[r]`, and runs hybrid searches from the input bar on `[Enter]`.
//! Input is polled every 50 ms so keys feel instant.
//! Test: `cargo test -p trusty-common --features monitor-tui` covers the pure
//! state, log capacity, selection clamp, the "All" selector, and the
//! activity / statistics line builders; `trusty-search monitor tui` launches
//! the live UI.

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame, Terminal,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use tokio::sync::mpsc;

use crate::monitor::dashboard::{IndexRow, format_count};
use crate::monitor::search_client::{ReindexEvent, SearchClient, resolve_search_url};
use crate::monitor::tui_common::{
    self, ListFocus, ThreeWaySortKey, enter_tui, leave_tui, left_panel_width, panel_block, truncate,
};
use crate::monitor::utils::{ActivityLog, DaemonStatus, fmt_uptime};

/// Data-refresh interval: how often the daemon is polled.
const REFRESH_INTERVAL: Duration = Duration::from_millis(2000);

/// Input-poll interval: how often the keyboard is checked.
const INPUT_POLL: Duration = Duration::from_millis(50);

/// Number of results requested per search query.
const SEARCH_TOP_K: usize = 5;

/// Crate version, surfaced in the title bar.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// One-line key hint shown along the bottom of the UI.
pub const KEY_HINT: &str = "[Tab] focus  [r] reindex  [↑↓] select  [Enter] search  [/] filter  [s] sort  [g] group  [q] quit  [?] help";

/// Domain-specific labels for the search TUI's three sort orders.
///
/// Why: the renderer surfaces the current sort key in the panel title; this
/// array maps the shared [`ThreeWaySortKey`] variants to search-domain text
/// (the third variant reads as "Chunks" here, "Vectors" in memory).
/// What: `["Activity", "Name", "Chunks"]`.
/// Test: covered indirectly by `test_index_sort_key_cycle` via [`sort_label`].
const SORT_LABELS: &[&str; 3] = &["Activity", "Name", "Chunks"];

/// Sort key cycled by `[s]` in the index list.
///
/// Why: kept as a re-export alias so external callers and tests that reference
/// `IndexSortKey` continue to compile after the type was consolidated into
/// the shared [`ThreeWaySortKey`].
/// What: type alias for [`ThreeWaySortKey`].
/// Test: `test_index_sort_key_cycle`.
pub type IndexSortKey = ThreeWaySortKey;

/// Search-domain label for the current sort key.
///
/// Why: the renderer needs `"Activity"` / `"Name"` / `"Chunks"`; the shared
/// enum is domain-agnostic so we map it through [`SORT_LABELS`].
/// What: delegates to [`ThreeWaySortKey::label`] with the search labels.
/// Test: `test_index_sort_key_cycle`.
pub fn sort_label(key: ThreeWaySortKey) -> &'static str {
    key.label(SORT_LABELS)
}

/// Label for the synthetic "All indexes" entry at the top of the list.
///
/// Why: selecting it fans queries / stats out across every index; a single
/// constant keeps the label consistent between the list and the panel titles.
/// What: the display text of the index list's first row.
/// Test: `test_index_lines` asserts this is the first row.
pub const ALL_LABEL: &str = "All indexes";

/// Which zone of the search UI currently holds keyboard focus.
///
/// Why: re-export alias for [`ListFocus`] so existing callers and tests that
/// reference `SearchFocus` continue to compile after the type was consolidated
/// into the shared module.
/// What: type alias for [`ListFocus`].
/// Test: `test_toggle_focus`.
pub type SearchFocus = ListFocus;

/// All mutable state the search UI renders and mutates.
///
/// Why: the event loop polls the daemon, streams reindex events, and handles
/// input — keeping every piece of state in one struct keeps the loop terse and
/// the rendering a pure function of this snapshot.
/// What: the daemon URL and status, the index list and selection cursor, the
/// scroll offset of the index panel, the bounded activity log, the query
/// buffer, the focused zone, and the help flag. The selection cursor addresses
/// a list whose first row is the synthetic "All indexes" entry, so cursor `0`
/// means "All" and cursor `n` (n ≥ 1) means `indexes[n - 1]`.
/// Test: `test_selected_clamp`, `test_toggle_focus`, `test_log_append`,
/// `test_all_selector`, `test_scroll_offset`.
#[derive(Debug, Clone)]
pub struct SearchTuiState {
    /// The trusty-search daemon base URL being monitored.
    pub base_url: String,
    /// The daemon's current liveness state.
    pub daemon_status: DaemonStatus,
    /// One row per registered index.
    pub indexes: Vec<IndexRow>,
    /// Cursor into the index list, where row `0` is the "All indexes" entry
    /// and row `n` (n ≥ 1) selects `indexes[n - 1]`.
    pub selected: usize,
    /// Index of the first row drawn in the INDEXES panel — the scroll offset
    /// that keeps [`Self::selected`] on screen when the list overflows.
    pub scroll_offset: usize,
    /// Bounded, timestamped log of reindex / search activity.
    pub log: ActivityLog,
    /// The in-progress search query buffer.
    pub input: String,
    /// Which zone currently holds keyboard focus.
    pub focus: SearchFocus,
    /// Whether the help overlay is visible (toggled with `?`).
    pub show_help: bool,
    /// Case-insensitive filter applied to index id / project; empty disables.
    pub filter: String,
    /// Whether the inline filter bar is focused (captures typed chars).
    pub filter_active: bool,
    /// Current index-list sort order.
    pub sort_key: ThreeWaySortKey,
    /// Whether the index list is grouped by inferred project.
    pub group_by_project: bool,
    /// The last daemon log line seen on the previous `logs_tail` poll.
    ///
    /// Why: `GET /logs/tail` returns the last N lines from a ring buffer; the
    /// poll cycle uses this watermark to identify only the lines that arrived
    /// since the previous tick, so historical lines are not re-pushed every
    /// 2 seconds.
    /// What: the most-recent line from the previous successful poll, or
    /// `None` before the first poll completes.
    /// Test: `test_push_new_log_lines_skips_first_poll`.
    pub log_watermark: Option<String>,
    /// True until the first successful `logs_tail` response.
    ///
    /// Why: when the operator opens the TUI, the ring buffer may already
    /// hold lines from earlier daemon activity; dumping them all into the
    /// activity feed would bury whatever happens next. This flag suppresses
    /// the initial dump and only records a watermark on the first poll.
    /// What: starts `true`, flips to `false` after the first poll resolves
    /// the watermark.
    /// Test: `test_push_new_log_lines_skips_first_poll`.
    pub log_first_poll: bool,
}

impl SearchTuiState {
    /// Build a fresh search UI state targeting `base_url`.
    ///
    /// Why: the event loop seeds the state at startup before the first poll.
    /// What: stores the URL, sets the daemon `Connecting`, and starts with an
    /// empty index list, empty log, empty query, and list focus.
    /// Test: `test_new_state_defaults`.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            daemon_status: DaemonStatus::Connecting,
            indexes: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            log: ActivityLog::new(),
            input: String::new(),
            focus: ListFocus::List,
            show_help: false,
            filter: String::new(),
            filter_active: false,
            sort_key: ThreeWaySortKey::default(),
            group_by_project: false,
            log_watermark: None,
            log_first_poll: true,
        }
    }

    /// Cycle keyboard focus between the index list and the query bar (`[Tab]`).
    ///
    /// Why: `[Tab]` decides whether arrows navigate the list or whether typed
    /// characters edit the search query.
    /// What: flips [`Self::focus`] via [`ListFocus::toggled`].
    /// Test: `test_toggle_focus`.
    pub fn toggle_focus(&mut self) {
        self.focus = self.focus.toggled();
    }

    /// Move the index selection up one row, saturating at the top.
    ///
    /// Why: `↑` navigates the INDEXES list when it has focus.
    /// What: decrements [`Self::selected`], never below zero.
    /// Test: `test_selected_clamp`.
    pub fn select_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the index selection down one row, clamped to the last index.
    ///
    /// Why: `↓` navigates the INDEXES list when it has focus.
    /// What: increments [`Self::selected`] but never past the last row. The
    /// list has `indexes.len() + 1` rows (row 0 is "All indexes").
    /// Test: `test_selected_clamp`.
    pub fn select_down(&mut self) {
        if self.selected < self.last_row() {
            self.selected += 1;
        }
    }

    /// The index of the last selectable row.
    ///
    /// Why: the list always carries the synthetic "All" row, so the last valid
    /// cursor is `indexes.len()` (not `indexes.len() - 1`).
    /// What: returns `indexes.len()` — row 0 is "All", rows `1..=len` are the
    /// individual indexes.
    /// Test: `test_selected_clamp`.
    fn last_row(&self) -> usize {
        self.indexes.len()
    }

    /// Clamp the selection cursor to the current index count.
    ///
    /// Why: a poll can shrink the index list (an index was deleted) leaving the
    /// cursor past the end; this keeps it valid before rendering.
    /// What: caps [`Self::selected`] at `indexes.len()` (the "All" row plus one
    /// row per index).
    /// Test: `test_selected_clamp`.
    pub fn clamp_selection(&mut self) {
        if self.selected > self.last_row() {
            self.selected = self.last_row();
        }
    }

    /// Recompute the scroll offset so the selected row fits a `visible` window.
    ///
    /// Why: the INDEXES panel is a fixed-height viewport; when the list has
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
    /// `Self::selected` (an index into the original `indexes` array) no
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

    /// Whether the "All indexes" entry is currently selected.
    ///
    /// Why: when "All" is selected the UI fans queries out across every index
    /// and aggregates the activity feed and statistics.
    /// What: returns `true` exactly when the cursor is on row 0.
    /// Test: `test_all_selector`.
    pub fn is_all_selected(&self) -> bool {
        self.selected == 0
    }

    /// The id of the currently selected single index, if any.
    ///
    /// Why: `[r]` reindexes and `[Enter]` searches a single index; both need
    /// its id, and neither applies when "All" is selected.
    /// What: returns `Some(id)` for the index at cursor row `n ≥ 1`, or `None`
    /// when "All" is selected or the index list is empty.
    /// Test: `test_selected_id`.
    pub fn selected_id(&self) -> Option<&str> {
        if self.selected == 0 {
            return None;
        }
        self.indexes.get(self.selected - 1).map(|i| i.id.as_str())
    }

    /// Clamp the selection to the currently visible (filtered + sorted) list.
    ///
    /// Why: when the filter changes the selected index may no longer appear in
    /// the visible subset, so arrow navigation would jump unpredictably; this
    /// drops the cursor back to "All" (row 0) in that case so navigation always
    /// starts from a visible row.
    /// What: if `selected` is non-zero and the corresponding index id is not in
    /// the visible id list, resets `selected` to 0.
    /// Test: `test_clamp_to_visible`.
    pub fn clamp_to_visible(&mut self) {
        if self.selected == 0 {
            return;
        }
        let Some(current_id) = self.indexes.get(self.selected - 1).map(|i| i.id.clone()) else {
            self.selected = 0;
            return;
        };
        let ids = visible_index_ids(self);
        if !ids.iter().any(|id| id == &current_id) {
            self.selected = 0;
        }
    }

    /// The scope filter for the activity feed and statistics panels.
    ///
    /// Why: the right-hand panels render the selected index's events / stats,
    /// or every index's when "All" is selected; this folds the cursor into the
    /// `Option<&str>` filter [`ActivityLog::tail_scoped`] expects.
    /// What: returns `None` when "All" is selected (un-filtered) or `Some(id)`
    /// for the selected single index.
    /// Test: `test_all_selector`.
    pub fn scope_filter(&self) -> Option<&str> {
        self.selected_id()
    }
}

/// Run the trusty-search monitor TUI.
///
/// Why: the single entry point the `monitor tui` subcommand of `trusty-search`
/// calls.
/// What: resolves the daemon URL from the service lock file and delegates to
/// [`run_with_url`].
/// Test: the pure pieces are unit-tested; this thin glue is exercised by
/// launching the UI.
pub async fn run() -> anyhow::Result<()> {
    run_with_url(resolve_search_url()).await
}

/// Run the search TUI against an explicit daemon URL.
///
/// Why: separated from [`run`] so a future CLI flag can override the resolved
/// address, and so terminal setup/teardown lives in one place.
/// What: builds the client and state, enters raw mode + the alternate screen,
/// runs [`run_loop`], and unconditionally restores the terminal even on error.
/// Test: terminal glue is exercised by launching the UI.
pub async fn run_with_url(base_url: String) -> anyhow::Result<()> {
    let mut client = SearchClient::new(base_url.clone());
    let mut state = SearchTuiState::new(base_url);

    let mut terminal = enter_tui()?;
    let result = run_loop(&mut terminal, &mut state, &mut client).await;
    leave_tui(&mut terminal)?;
    result
}

/// Compute which lines from `new_lines` are genuinely new since `watermark`.
///
/// Why: `GET /logs/tail` returns the last N lines from a ring buffer; on
/// every poll we receive an overlapping window. This helper finds the
/// suffix of `new_lines` that appears after the last-seen watermark line,
/// so only truly new lines get pushed to the activity log.
/// What: if `watermark` is `None`, returns all of `new_lines` (first poll
/// after the initial skip). Otherwise finds the rightmost occurrence of
/// `watermark` in `new_lines` and returns everything after it. If the
/// watermark is not found (ring buffer wrapped), returns all of `new_lines`.
/// Test: `test_new_log_lines_since_watermark`.
pub fn new_log_lines_since<'a>(new_lines: &'a [String], watermark: Option<&str>) -> &'a [String] {
    let Some(mark) = watermark else {
        return new_lines;
    };
    match new_lines.iter().rposition(|line| line == mark) {
        Some(idx) => &new_lines[idx + 1..],
        None => new_lines,
    }
}

/// Poll the trusty-search daemon and fold the result into `state`.
///
/// Why: keeps the per-poll I/O out of the event loop so the loop can re-poll
/// on demand as well as on its timer.
/// What: re-resolves the URL when the daemon is offline (it may have rebound a
/// fresh port), calls `fetch_all`, and updates the status, index list, and
/// selection clamp.
/// Test: thin I/O glue; the pure clamp is unit-tested.
async fn poll_daemon(state: &mut SearchTuiState, client: &mut SearchClient) {
    if !state.daemon_status.is_online() {
        let resolved = resolve_search_url();
        if resolved != client.base_url() {
            client.set_base_url(resolved.clone());
            state.base_url = resolved;
        }
    }
    match client.fetch_all().await {
        Ok(data) => {
            state.daemon_status = DaemonStatus::Online {
                version: data.version,
                uptime_secs: data.uptime_secs,
            };
            state.indexes = data.indexes;
            state.clamp_selection();
        }
        Err(e) => {
            state.daemon_status = DaemonStatus::Offline {
                last_error: e.to_string(),
            };
        }
    }

    // Passive background activity: surface daemon-side log lines (file-watcher
    // reindexes, startup scans) in the ACTIVITY panel without any user input.
    // Daemon lines are unscoped — they apply to the daemon as a whole, not to
    // any single index — so they appear under every scope filter.
    let tail = client.logs_tail(50).await;
    if state.log_first_poll {
        // The ring buffer may already hold lines from before the operator
        // opened the TUI; record the high-water mark but do not dump them.
        state.log_watermark = tail.last().cloned();
        state.log_first_poll = false;
    } else {
        let new = new_log_lines_since(&tail, state.log_watermark.as_deref());
        for line in new {
            state.log.push(line.clone());
        }
        if let Some(last) = tail.last() {
            state.log_watermark = Some(last.clone());
        }
    }
}

/// Run a search and append the hits to the activity log.
///
/// Why: pressing `[Enter]` in the query bar runs a hybrid search; the operator
/// sees the results inline in the ACTIVITY panel. When "All indexes" is
/// selected the search fans out across every registered index.
/// What: with a single index selected, calls `client.search` for it and logs a
/// `search "<q>" → N results` summary plus one indented continuation line per
/// hit, all tagged with that index's id. With "All" selected it iterates every
/// index, logging each index's per-index summary tagged to that index and a
/// final daemon-wide total line. An empty query is a no-op; transport errors
/// are logged.
/// Test: thin I/O glue; result projection is tested in `search_client`.
async fn run_search(state: &mut SearchTuiState, client: &SearchClient) {
    let query = state.input.trim().to_string();
    if query.is_empty() {
        return;
    }

    if state.is_all_selected() {
        run_search_all(state, client, &query).await;
    } else if let Some(id) = state.selected_id().map(str::to_string) {
        run_search_one(state, client, &id, &query).await;
    } else {
        state.log.push("search: no index selected");
    }
    state.input.clear();
}

/// Run a search against one index and append the hits to the log.
///
/// Why: the single-index path of [`run_search`]; isolating it keeps the
/// fan-out loop in [`run_search_all`] terse.
/// What: calls `client.search(id, …)`, appends an `id`-scoped summary line and
/// one indented `path:line  snippet` continuation per hit. A transport error
/// is logged as an `id`-scoped failure line.
/// Test: thin I/O glue; result projection is tested in `search_client`.
async fn run_search_one(state: &mut SearchTuiState, client: &SearchClient, id: &str, query: &str) {
    match client.search(id, query, SEARCH_TOP_K).await {
        Ok(hits) => {
            state
                .log
                .push_scoped(id, format!("search \"{query}\" → {} results", hits.len()));
            for hit in &hits {
                state
                    .log
                    .push_raw_scoped(id, format!("  {}:{}  {}", hit.file, hit.line, hit.snippet));
            }
        }
        Err(e) => state
            .log
            .push_scoped(id, format!("search \"{query}\" failed: {e}")),
    }
}

/// Fan a search out across every index and append a merged summary.
///
/// Why: the "All indexes" selector lets an operator run one query over the
/// whole machine's corpus; the activity feed then shows each index's hit count
/// (tagged so the per-index view still works) plus a daemon-wide total.
/// What: snapshots the index ids, then for each calls `client.search`,
/// appending an `id`-scoped `· <id>: N results` line. A daemon-wide
/// `search "<q>" (all) → T results across K indexes` total line closes the
/// burst. With no indexes registered it logs a single note.
/// Test: thin I/O glue; the single-index search is unit-tested in
/// `search_client`.
async fn run_search_all(state: &mut SearchTuiState, client: &SearchClient, query: &str) {
    let ids: Vec<String> = state.indexes.iter().map(|i| i.id.clone()).collect();
    if ids.is_empty() {
        state.log.push("search (all): no indexes registered");
        return;
    }
    state
        .log
        .push(format!("search \"{query}\" (all) → {} indexes", ids.len()));
    let mut total = 0usize;
    for id in &ids {
        match client.search(id, query, SEARCH_TOP_K).await {
            Ok(hits) => {
                total += hits.len();
                state
                    .log
                    .push_raw_scoped(id, format!("  · {id}: {} results", hits.len()));
            }
            Err(e) => state
                .log
                .push_raw_scoped(id, format!("  · {id}: failed: {e}")),
        }
    }
    state.log.push(format!(
        "search \"{query}\" (all) → {total} results across {} indexes",
        ids.len()
    ));
}

/// One reindex SSE event tagged with the index it concerns.
///
/// Why: a reindex is started per-index, but the streamed [`ReindexEvent`]s
/// carry no index id; pairing each with `index_id` lets the activity log scope
/// the line so the per-index feed (and the "All" merge) stay correct.
/// What: the index id the reindex targets and the streamed event.
/// Test: `test_apply_reindex_event` exercises the scoped logging.
#[derive(Debug, Clone)]
pub struct ScopedReindexEvent {
    /// The index this reindex event belongs to.
    pub index_id: String,
    /// The streamed reindex progress event.
    pub event: ReindexEvent,
}

/// Apply one streamed reindex event to the activity log, scoped to its index.
///
/// Why: the reindex SSE task forwards [`ScopedReindexEvent`]s through a
/// channel; the event loop drains them and this turns each into a
/// human-readable, index-scoped log line so the per-index activity feed shows
/// only its own reindex progress.
/// What: `Started` / `Progress` / `Complete` / `Failed` each map to a distinct
/// timestamped line tagged with `scoped.index_id`, with progress carrying a
/// percent-complete figure.
/// Test: `test_apply_reindex_event`.
pub fn apply_reindex_event(state: &mut SearchTuiState, scoped: ScopedReindexEvent) {
    let id = scoped.index_id;
    match scoped.event {
        ReindexEvent::Started { total_files } => {
            state
                .log
                .push_scoped(&id, format!("reindex started: {total_files} files"));
        }
        ReindexEvent::Progress {
            indexed,
            total_files,
        } => {
            let pct = indexed
                .saturating_mul(100)
                .checked_div(total_files)
                .unwrap_or(0);
            state
                .log
                .push_scoped(&id, format!("indexing: {indexed}/{total_files} ({pct}%)"));
        }
        ReindexEvent::Complete {
            total_chunks,
            status,
        } => {
            state.log.push_scoped(
                &id,
                format!("reindex {status}: {} chunks", format_count(total_chunks)),
            );
        }
        ReindexEvent::Failed(message) => {
            state
                .log
                .push_scoped(&id, format!("reindex error: {message}"));
        }
    }
}

/// The search TUI event loop: poll, render, handle input, drain reindex events.
///
/// Why: kept separate from [`run_with_url`] so terminal setup/teardown wraps it
/// cleanly.
/// What: polls the daemon immediately, then renders every frame while polling
/// the keyboard every 50 ms; re-polls on the 2 s timer. `[r]` spawns an SSE
/// reindex task whose events are drained via `try_recv`; `[Enter]` runs a
/// search; `Tab`, arrows, `?`, `q`/`Esc`, and `Ctrl-C` behave per [`KEY_HINT`].
/// Test: the pure pieces (state, log, rendering helpers) are unit-tested.
async fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut SearchTuiState,
    client: &mut SearchClient,
) -> anyhow::Result<()> {
    poll_daemon(state, client).await;
    let mut last_poll = Instant::now();

    // Channel for reindex SSE events forwarded by a background task. Each
    // event is tagged with its index id so the activity feed can scope it.
    let (reindex_tx, mut reindex_rx) = mpsc::channel::<ScopedReindexEvent>(64);

    loop {
        terminal.draw(|f| render(f, state))?;
        // `terminal.draw` requires `state` mutably (the renderer scrolls the
        // index list); the closure reborrows it for the rest of the loop.

        // Drain any reindex events the SSE task has produced since last frame.
        while let Ok(event) = reindex_rx.try_recv() {
            apply_reindex_event(state, event);
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
            match (state.focus, key.code) {
                // Filter-active bindings come first — they capture characters,
                // backspace, Esc, and Enter before the general handlers.
                (SearchFocus::List, KeyCode::Esc) if state.filter_active => {
                    // Keep the filter text so the user can re-activate.
                    state.filter_active = false;
                }
                (SearchFocus::List, KeyCode::Enter) if state.filter_active => {
                    state.filter_active = false;
                }
                (SearchFocus::List, KeyCode::Backspace) if state.filter_active => {
                    state.filter.pop();
                    state.clamp_to_visible();
                }
                (SearchFocus::List, KeyCode::Char(c)) if state.filter_active => {
                    state.filter.push(c);
                    state.clamp_to_visible();
                }
                // Tab is a no-op while the filter is active — otherwise it
                // would steal focus away from the list and break filter input.
                (SearchFocus::List, KeyCode::Tab) if state.filter_active => {}
                (_, KeyCode::Char('?')) => state.show_help = true,
                (_, KeyCode::Tab) => state.toggle_focus(),
                (_, KeyCode::Esc) => return Ok(()),
                // List-focus bindings.
                (SearchFocus::List, KeyCode::Char('q')) => return Ok(()),
                (SearchFocus::List, KeyCode::Up) => navigate_up_visible(state),
                (SearchFocus::List, KeyCode::Down) => navigate_down_visible(state),
                (SearchFocus::List, KeyCode::Char('/')) => {
                    state.filter_active = true;
                    state.filter.clear();
                }
                (SearchFocus::List, KeyCode::Char('s')) => {
                    state.sort_key = state.sort_key.next();
                }
                (SearchFocus::List, KeyCode::Char('g')) => {
                    state.group_by_project = !state.group_by_project;
                }
                (SearchFocus::List, KeyCode::Char('r')) => {
                    let targets: Vec<String> = if state.is_all_selected() {
                        state.indexes.iter().map(|i| i.id.clone()).collect()
                    } else {
                        state
                            .selected_id()
                            .map(str::to_string)
                            .into_iter()
                            .collect()
                    };
                    if targets.is_empty() {
                        if state.is_all_selected() {
                            state.log.push("reindex (all): no indexes registered");
                        } else {
                            state.log.push("reindex: no index selected");
                        }
                    } else {
                        if state.is_all_selected() {
                            state
                                .log
                                .push(format!("reindex triggered: all {} indexes", targets.len()));
                        }
                        for id in targets {
                            state
                                .log
                                .push_scoped(&id, format!("reindex triggered: {id}"));
                            spawn_reindex(client.clone(), reindex_tx.clone(), id);
                        }
                    }
                }
                // Input-focus bindings.
                (SearchFocus::Input, KeyCode::Enter) => {
                    run_search(state, client).await;
                    poll_daemon(state, client).await;
                    last_poll = Instant::now();
                }
                (SearchFocus::Input, KeyCode::Backspace) => {
                    state.input.pop();
                }
                (SearchFocus::Input, KeyCode::Char(c)) => state.input.push(c),
                _ => {}
            }
        }

        if last_poll.elapsed() >= REFRESH_INTERVAL {
            poll_daemon(state, client).await;
            last_poll = Instant::now();
        }
    }
}

/// Spawn a background task streaming one index's reindex into `out`.
///
/// Why: `SearchClient::reindex_stream` emits bare [`ReindexEvent`]s, but the
/// event loop needs each tagged with its index id; this bridges a per-index
/// inner channel onto the loop's [`ScopedReindexEvent`] channel so several
/// indexes can reindex concurrently (the "All" fan-out) without losing track
/// of which event belongs to which index.
/// What: spawns the SSE streaming task plus a forwarding task that wraps every
/// [`ReindexEvent`] in a [`ScopedReindexEvent`] carrying `index_id`.
/// Test: side-effect-only (spawns tasks); the scoped event handling is
/// unit-tested via `test_apply_reindex_event`.
fn spawn_reindex(client: SearchClient, out: mpsc::Sender<ScopedReindexEvent>, index_id: String) {
    let (inner_tx, mut inner_rx) = mpsc::channel::<ReindexEvent>(64);
    let stream_id = index_id.clone();
    tokio::spawn(async move {
        client.reindex_stream(&stream_id, inner_tx).await;
    });
    tokio::spawn(async move {
        while let Some(event) = inner_rx.recv().await {
            let scoped = ScopedReindexEvent {
                index_id: index_id.clone(),
                event,
            };
            if out.send(scoped).await.is_err() {
                break; // event loop gone — stop forwarding.
            }
        }
    });
}

/// The body text for the help overlay, one binding per line.
///
/// Why: kept separate so a test can assert every binding is documented.
/// What: returns the multi-line help string.
/// Test: `test_help_text_lists_bindings`.
pub fn help_text() -> String {
    [
        "  Tab     switch focus between the index list and the search bar",
        "  ↑ / ↓   move the index selection (when the list has focus)",
        "  All     the top list row fans queries / stats across every index",
        "  /       activate the inline index filter (Esc / Enter close)",
        "  s       cycle index sort: Activity → Name → Chunks",
        "  g       toggle grouping by inferred project",
        "  r       reindex the selected index — or all, when 'All' is selected",
        "  Enter   run a search against the selected index — or all of them",
        "  ?       toggle this help overlay",
        "  q / Esc quit",
    ]
    .join("\n")
}

/// One rendered row of the INDEXES panel.
///
/// Why: the renderer styles four row kinds differently — the "All" row is
/// bold, group headers are bold yellow and non-selectable, the selected row
/// is highlighted, ordinary rows are plain — so the line builder must surface
/// which kind each row is rather than just a bool.
/// What: the row `text`, whether it is `selected`, whether it is the
/// synthetic `is_all` ("All indexes") row, and whether it is a group header
/// (non-selectable when grouping by project).
/// Test: `test_index_lines`, `test_all_selector`, `test_index_lines_grouped`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexListRow {
    /// The fully-formatted row text.
    pub text: String,
    /// Whether this row is the current selection.
    pub selected: bool,
    /// Whether this row is the synthetic "All indexes" entry.
    pub is_all: bool,
    /// Whether this row is a non-selectable group header.
    pub is_header: bool,
}

/// Format one index as a fixed-width table row.
///
/// Why: kept separate from the loop body to mirror the memory TUI's
/// `palace_row` helper and keep alignment unit-testable.
/// What: returns `> <id padded to 12> <count> ✓`, with `>` replacing the
/// leading space when `selected`.
/// Test: covered indirectly via `test_index_lines`.
fn index_row_flat(idx: &IndexRow, selected: bool) -> String {
    let marker = if selected { ">" } else { " " };
    format!(
        "{marker} {:<12} {:>8} ✓",
        truncate(&idx.id, 12),
        format_count(idx.chunk_count),
    )
}

/// Format an indented index row for use under a group header.
///
/// Why: when the list is grouped, index rows are inset one extra space and
/// the id column shrinks by one to keep the count column aligned with the
/// flat layout.
/// What: returns `"  <id padded to 11> <count> ✓"`, with `>` replacing the
/// leading space when `selected`.
/// Test: `test_index_lines_grouped`.
fn index_row_indented(idx: &IndexRow, selected: bool) -> String {
    let marker = if selected { ">" } else { " " };
    format!(
        "{marker}  {:<11} {:>8} ✓",
        truncate(&idx.id, 11),
        format_count(idx.chunk_count),
    )
}

/// Apply [`SearchTuiState::filter`] and [`SearchTuiState::sort_key`] to the
/// state's indexes, returning the visible subset in display order.
///
/// Why: delegates to the shared [`tui_common::filtered_sorted`] so memory and
/// search apply identical filter / sort rules. Kept as a search-named wrapper
/// for the existing tests and callers.
/// What: thin wrapper over [`tui_common::filtered_sorted`].
/// Test: `test_apply_filter`, `test_apply_sort_*`.
pub fn filtered_sorted_indexes(state: &SearchTuiState) -> Vec<IndexRow> {
    tui_common::filtered_sorted(&state.indexes, &state.filter, state.sort_key)
}

/// Ids of the rows the user can navigate between, in visible display order.
///
/// Why: thin wrapper over the shared [`tui_common::visible_ids`].
/// What: delegates to the shared helper with the search state's fields.
/// Test: `test_visible_index_ids`, `test_navigate_visible`.
pub fn visible_index_ids(state: &SearchTuiState) -> Vec<String> {
    tui_common::visible_ids(
        &state.indexes,
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
pub fn navigate_up_visible(state: &mut SearchTuiState) {
    state.selected = tui_common::navigate_up(
        &state.indexes,
        state.selected,
        &state.filter,
        state.sort_key,
        state.group_by_project,
    );
}

/// Move the cursor down one row in the visible (filtered + sorted) list.
///
/// Why: thin wrapper over the shared [`tui_common::navigate_down`].
/// What: delegates and writes back the new cursor.
/// Test: `test_navigate_visible`.
pub fn navigate_down_visible(state: &mut SearchTuiState) {
    state.selected = tui_common::navigate_down(
        &state.indexes,
        state.selected,
        &state.filter,
        state.sort_key,
        state.group_by_project,
    );
}

/// Row index — within the rendered `index_lines` output — that the cursor
/// currently sits on.
///
/// Why: ratatui's `ListState::with_selected` and the viewport scroll math
/// both index into the rendered list, but `state.selected` is an index into
/// the *original* `state.indexes` Vec. After a filter, sort, or grouping
/// reorders rows, the two indices diverge and the highlight + scroll latch
/// onto the wrong on-screen line. This helper bridges them: given the same
/// state the renderer sees, it returns the visible row at which the current
/// selection is drawn so the highlight follows the sorted order.
/// What: returns `0` when "All" is selected; otherwise walks
/// [`index_lines`] looking for the row whose `selected` flag is set and
/// returns its index. Falls back to `0` (the "All" row) when no matching
/// row is found, which mirrors how `clamp_to_visible` collapses a hidden
/// selection back to "All".
/// Test: `test_visible_selected_row_follows_sort`,
/// `test_visible_selected_row_follows_group`.
pub fn visible_selected_row(state: &SearchTuiState) -> usize {
    if state.selected == 0 {
        return 0;
    }
    index_lines(state)
        .iter()
        .position(|row| row.selected)
        .unwrap_or(0)
}

/// Build the rows for the INDEXES panel body.
///
/// Why: separating row construction from the ratatui widgets lets a test
/// assert the rendered content without a terminal backend.
/// What: returns the synthetic "All indexes" row first (carrying the summed
/// chunk count across every index), then either a flat list of filtered +
/// sorted index rows, or — when [`SearchTuiState::group_by_project`] is set —
/// non-selectable `── <project> ──` group headers interleaved with their
/// member indexes. With no indexes registered the "All" row is still shown
/// followed by a placeholder line.
/// Test: `test_index_lines`, `test_all_selector`, `test_index_lines_grouped`.
pub fn index_lines(state: &SearchTuiState) -> Vec<IndexListRow> {
    let mut rows: Vec<IndexListRow> = Vec::with_capacity(state.indexes.len() + 1);

    // The synthetic "All indexes" row always leads the list — including when
    // filtering or grouping is active.
    let total_chunks: u64 = state.indexes.iter().map(|i| i.chunk_count).sum();
    let all_selected = state.selected == 0;
    let all_marker = if all_selected { ">" } else { " " };
    rows.push(IndexListRow {
        text: format!(
            "{all_marker} {:<12} {:>8} ∗",
            truncate(ALL_LABEL, 12),
            format_count(total_chunks),
        ),
        selected: all_selected,
        is_all: true,
        is_header: false,
    });

    if state.indexes.is_empty() {
        rows.push(IndexListRow {
            text: "  (no indexes registered)".to_string(),
            selected: false,
            is_all: false,
            is_header: false,
        });
        return rows;
    }

    let visible = filtered_sorted_indexes(state);
    if visible.is_empty() {
        rows.push(IndexListRow {
            text: "  (no matches)".to_string(),
            selected: false,
            is_all: false,
            is_header: false,
        });
        return rows;
    }

    // The cursor addresses the *original* `state.indexes` indices (cursor n →
    // indexes[n-1]) so we look up each visible index's original position by id.
    let cursor_for = |idx: &IndexRow| -> usize {
        state
            .indexes
            .iter()
            .position(|orig| orig.id == idx.id)
            .map(|i| i + 1)
            .unwrap_or(0)
    };

    if state.group_by_project {
        // Collect distinct projects in the order they first appear in `visible`.
        let mut seen: Vec<String> = Vec::new();
        for i in &visible {
            let proj = i.project().to_string();
            if !seen.iter().any(|s| s == &proj) {
                seen.push(proj);
            }
        }
        for project in &seen {
            rows.push(IndexListRow {
                text: format!("── {project} ─────"),
                selected: false,
                is_all: false,
                is_header: true,
            });
            for idx in visible.iter().filter(|i| i.project() == project) {
                let cursor = cursor_for(idx);
                let selected = cursor == state.selected;
                rows.push(IndexListRow {
                    text: index_row_indented(idx, selected),
                    selected,
                    is_all: false,
                    is_header: false,
                });
            }
        }
    } else {
        for idx in &visible {
            let cursor = cursor_for(idx);
            let selected = cursor == state.selected;
            rows.push(IndexListRow {
                text: index_row_flat(idx, selected),
                selected,
                is_all: false,
                is_header: false,
            });
        }
    }
    rows
}

/// Build the STATISTICS panel lines for the current selection.
///
/// Why: the bottom-right panel shows counts and sizes for whichever index is
/// selected, or aggregate totals plus a per-index breakdown when "All" is
/// selected; isolating the builder makes the content testable without a
/// terminal.
/// What: for a single index, returns its id, chunk count, and indexed root
/// path. For the "All" selection, returns the index count, the summed chunk
/// count, and one `· <id>: <chunks>` breakdown line per index.
/// Test: `test_stats_lines`.
pub fn stats_lines(state: &SearchTuiState) -> Vec<String> {
    if state.is_all_selected() {
        let total: u64 = state.indexes.iter().map(|i| i.chunk_count).sum();
        let total_nodes: u64 = state.indexes.iter().map(|i| i.node_count).sum();
        let mut lines = vec![
            format!("Scope:        {ALL_LABEL}"),
            format!("Indexes:      {}", state.indexes.len()),
            format!("Total chunks: {}", format_count(total)),
        ];
        // Graph total — always shown so the panel surfaces graph readiness
        // even before any index has been reindexed with the symbol graph.
        if total_nodes > 0 {
            lines.push(format!("Graph nodes:  {}", format_count(total_nodes)));
        } else {
            lines.push("Graph nodes:  (none — reindex to build)".to_string());
        }
        if state.indexes.is_empty() {
            lines.push("(no indexes registered)".to_string());
        } else {
            lines.push(String::new());
            for idx in &state.indexes {
                lines.push(format!(
                    "  · {:<14} {:>8}",
                    truncate(&idx.id, 14),
                    format_count(idx.chunk_count),
                ));
            }
        }
        return lines;
    }

    match state.indexes.get(state.selected.saturating_sub(1)) {
        Some(idx) => {
            let mut lines = vec![
                format!("Index:        {}", idx.id),
                format!("Chunks:       {}", format_count(idx.chunk_count)),
                format!(
                    "Root path:    {}",
                    if idx.root_path.is_empty() {
                        "(unknown)"
                    } else {
                        idx.root_path.as_str()
                    }
                ),
            ];
            if let Some(bytes) = idx.disk_bytes {
                lines.push(format!("Disk size:    {}", format_bytes(bytes)));
            }
            if let Some(when) = idx.last_indexed {
                lines.push(format!(
                    "Last indexed: {}",
                    when.format("%Y-%m-%d %H:%M UTC")
                ));
            }
            // Graph stats section — always shown so users know whether the
            // daemon has built a symbol graph for this index. When
            // `node_count == 0` we surface a hint to reindex; otherwise we
            // emit the full nodes/edges breakdown and edge-kind bars.
            lines.push(String::new());
            lines.push("Graph:".to_string());
            if idx.node_count == 0 {
                lines.push("  (no graph — press [r] to reindex)".to_string());
            } else {
                lines.push(format!(
                    "  Nodes:    {:>8}  Edges: {:>8}",
                    format_count(idx.node_count),
                    format_count(idx.edge_count),
                ));
                if let Some(max) = idx.edge_kinds.iter().map(|(_, n)| *n).max()
                    && max > 0
                {
                    const BAR_WIDTH: usize = 14;
                    for (kind, count) in &idx.edge_kinds {
                        let bar_len =
                            ((*count as f64 / max as f64) * BAR_WIDTH as f64).round() as usize;
                        let bar_len = bar_len.min(BAR_WIDTH);
                        let bar: String = "█".repeat(bar_len);
                        lines.push(format!(
                            "  {:<18} {:>7}  {}",
                            truncate(kind, 18),
                            format_count(*count),
                            bar,
                        ));
                    }
                }
            }
            lines
        }
        None => vec!["(no index selected)".to_string()],
    }
}

/// Format a byte count as a compact human-readable string.
///
/// Why: the STATISTICS panel surfaces an index's on-disk size; raw bytes are
/// hard to scan at a glance.
/// What: returns one of `B`, `KB`, `MB`, `GB`, `TB` with one decimal place
/// above 1 KB; below that returns the exact byte count.
/// Test: `test_format_bytes`.
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    let n = bytes as f64;
    if n < KB {
        format!("{bytes} B")
    } else if n < MB {
        format!("{:.1} KB", n / KB)
    } else if n < GB {
        format!("{:.1} MB", n / MB)
    } else if n < TB {
        format!("{:.1} GB", n / GB)
    } else {
        format!("{:.1} TB", n / TB)
    }
}

/// Build the title-bar line for the search UI.
///
/// Why: the top row shows the daemon name, version, liveness badge, and uptime
/// at a glance; isolating it keeps `render` terse and the text testable.
/// What: returns `trusty-search vX  [●] <status>  uptime: Xh Ym` — uptime is
/// only shown when the daemon is online.
/// Test: `test_title_line`.
pub fn title_line(state: &SearchTuiState) -> String {
    let (glyph, label) = state.daemon_status.badge();
    match &state.daemon_status {
        DaemonStatus::Online { uptime_secs, .. } => format!(
            "trusty-search v{VERSION}  [{glyph}] {label}  uptime: {}",
            fmt_uptime(*uptime_secs)
        ),
        _ => format!(
            "trusty-search v{VERSION}  [{glyph}] {label}  {}",
            state.base_url
        ),
    }
}

/// Draw the search TUI frame.
///
/// Why: the single entry point the event loop calls each tick.
/// What: a 4-row vertical layout — title bar, the INDEXES / right-pane split,
/// the SEARCH input bar, and the key-hint footer. The right pane is itself
/// split vertically into an ACTIVITY feed (top 60 %) and a STATISTICS panel
/// (bottom 40 %), both scoped to the selected index — or aggregated when "All"
/// is selected. A centred help overlay floats on top when `show_help` is set.
/// Test: line content is unit-tested via the `*_lines` helpers; this glue is
/// exercised by `test_render_smoke`.
pub fn render(frame: &mut Frame, state: &mut SearchTuiState) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Min(4),    // panels
            Constraint::Length(3), // search input
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

    // INDEXES on the left, the ACTIVITY / STATISTICS stack on the right.
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(left_panel_width(area.width)),
            Constraint::Min(10),
        ])
        .split(rows[1]);

    let list_focused = state.focus == SearchFocus::List;
    let index_items: Vec<ListItem> = index_lines(state)
        .into_iter()
        .map(|row| {
            let style = if row.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if row.is_header {
                // Group headers — bold yellow, non-selectable.
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if row.is_all {
                // The unselected "All" row stays distinct — bold yellow.
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(row.text, style)))
        })
        .collect();

    // When the inline filter is active or carries text, split the left column
    // vertically so the filter input renders above the index list.
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

    // Scroll the INDEXES list so the selected row stays visible: the panel
    // height minus its two border rows is the visible window. Both the
    // scroll anchor and the ratatui ListState selection index must reference
    // the *displayed* row (filter + sort + grouping reorder the rendered
    // rows relative to `state.indexes`), so we look up the visible row
    // index of the currently selected index once and use it for both.
    let index_visible = list_area.height.saturating_sub(2) as usize;
    let visible_row = visible_selected_row(state);
    state.sync_scroll_to(visible_row, index_visible);
    let mut index_state = ListState::default()
        .with_offset(state.scroll_offset)
        .with_selected(Some(visible_row));
    let index_title = format!("INDEXES [{}]", sort_label(state.sort_key));
    frame.render_stateful_widget(
        List::new(index_items).block(panel_block(&index_title, list_focused)),
        list_area,
        &mut index_state,
    );

    // Right pane: ACTIVITY (top) over STATISTICS (bottom).
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(tui_common::ACTIVITY_PERCENT),
            Constraint::Percentage(100 - tui_common::ACTIVITY_PERCENT),
        ])
        .split(split[1]);

    // ACTIVITY panel — the tail of the scoped feed that fits the panel height.
    let scope = state.scope_filter();
    let activity_title = match scope {
        Some(id) => format!("ACTIVITY — {id}"),
        None => format!("ACTIVITY — {ALL_LABEL}"),
    };
    let activity_height = right[0].height.saturating_sub(2) as usize;
    let activity_items: Vec<ListItem> = if state.log.has_scoped(scope) {
        state
            .log
            .tail_scoped(scope, activity_height.max(1))
            .map(|line| ListItem::new(line.as_str()))
            .collect()
    } else {
        vec![ListItem::new("(no activity yet)")]
    };
    frame.render_widget(
        List::new(activity_items).block(panel_block(&activity_title, false)),
        right[0],
    );

    // STATISTICS panel — counts and sizes for the selection.
    let stats_items: Vec<ListItem> = stats_lines(state).into_iter().map(ListItem::new).collect();
    frame.render_widget(
        List::new(stats_items).block(panel_block("STATISTICS", false)),
        right[1],
    );

    // SEARCH input bar.
    let input_focused = state.focus == SearchFocus::Input;
    let cursor = if input_focused { "_" } else { "" };
    let input_style = if input_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("SEARCH ▶ ", Style::default().fg(Color::Yellow)),
            Span::styled(format!("{}{cursor}", state.input), input_style),
        ]))
        .block(panel_block("SEARCH", input_focused)),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::utils::timestamped;
    use ratatui::{Terminal, backend::TestBackend};

    /// A state with three indexes for selection / rendering tests.
    fn sample_state() -> SearchTuiState {
        let mut state = SearchTuiState::new("http://127.0.0.1:7878");
        state.daemon_status = DaemonStatus::Online {
            version: "0.3.65".into(),
            uptime_secs: 7440,
        };
        state.indexes = vec![
            IndexRow {
                id: "cto".into(),
                chunk_count: 1_200,
                root_path: "/tmp/cto".into(),
                ..Default::default()
            },
            IndexRow {
                id: "trusty".into(),
                chunk_count: 18_994,
                root_path: "/tmp/trusty".into(),
                ..Default::default()
            },
            IndexRow {
                id: "duetto".into(),
                chunk_count: 900,
                root_path: "/tmp/duetto".into(),
                ..Default::default()
            },
        ];
        state
    }

    #[test]
    fn test_new_state_defaults() {
        let state = SearchTuiState::new("http://127.0.0.1:7878");
        assert_eq!(state.base_url, "http://127.0.0.1:7878");
        assert!(matches!(state.daemon_status, DaemonStatus::Connecting));
        assert!(state.indexes.is_empty());
        assert_eq!(state.selected, 0);
        assert!(state.log.is_empty());
        assert!(state.input.is_empty());
        assert_eq!(state.focus, SearchFocus::List);
        assert!(!state.show_help);
    }

    #[test]
    fn test_toggle_focus() {
        let mut state = SearchTuiState::new("http://x");
        assert_eq!(state.focus, SearchFocus::List);
        state.toggle_focus();
        assert_eq!(state.focus, SearchFocus::Input);
        state.toggle_focus();
        assert_eq!(state.focus, SearchFocus::List);
    }

    #[test]
    fn test_selected_clamp() {
        let mut state = sample_state();
        // The list has 1 ("All") + 3 indexes = 4 rows; the cursor stops at 3.
        for _ in 0..10 {
            state.select_down();
        }
        assert_eq!(state.selected, 3, "clamped to indexes.len()");
        // select_up saturates at zero (the "All" row).
        for _ in 0..10 {
            state.select_up();
        }
        assert_eq!(state.selected, 0);
        // A shrunk index list re-clamps the cursor (1 "All" + 1 index = row 1).
        state.selected = 3;
        state.indexes.truncate(1);
        state.clamp_selection();
        assert_eq!(state.selected, 1);
        // An empty list leaves only the "All" row at cursor 0.
        state.indexes.clear();
        state.selected = 5;
        state.clamp_selection();
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_selected_id() {
        let mut state = sample_state();
        // Cursor 0 is "All" — no single index.
        assert!(state.is_all_selected());
        assert_eq!(state.selected_id(), None);
        // Cursor 1 is the first index.
        state.select_down();
        assert_eq!(state.selected_id(), Some("cto"));
        state.select_down();
        assert_eq!(state.selected_id(), Some("trusty"));
        state.indexes.clear();
        state.clamp_selection();
        assert_eq!(state.selected_id(), None);
    }

    #[test]
    fn test_all_selector() {
        let mut state = sample_state();
        // The default selection is the "All indexes" row.
        assert!(state.is_all_selected());
        assert_eq!(state.scope_filter(), None);
        // Moving down off row 0 picks a single index and a scoped filter.
        state.select_down();
        assert!(!state.is_all_selected());
        assert_eq!(state.scope_filter(), Some("cto"));
        // Moving back up returns to "All".
        state.select_up();
        assert!(state.is_all_selected());
        assert_eq!(state.scope_filter(), None);

        // The index list always leads with the "All" row. Sort by Name so the
        // assertion below sees indexes in alphabetical order regardless of the
        // default Activity-with-chunk-count tiebreak.
        state.sort_key = IndexSortKey::Name;
        let rows = index_lines(&state);
        assert_eq!(rows.len(), 4, "1 'All' row + 3 indexes");
        assert!(rows[0].is_all);
        assert!(rows[0].text.contains(ALL_LABEL));
        assert!(rows[0].selected, "'All' is selected by default");
        assert!(!rows[1].is_all);
        assert!(rows[1].text.contains("cto"));
    }

    #[test]
    fn test_stats_lines() {
        let mut state = sample_state();
        // "All" selected → aggregate totals + a per-index breakdown.
        let all = stats_lines(&state);
        assert!(
            all.iter()
                .any(|l| l.contains("Indexes:") && l.contains('3'))
        );
        // 1,200 + 18,994 + 900 = 21,094 → abbreviated as 21.1k.
        assert!(all.iter().any(|l| l.contains("Total chunks:")));
        assert!(all.iter().any(|l| l.contains("cto")));
        assert!(all.iter().any(|l| l.contains("trusty")));

        // A single index selected → that index's detail.
        state.select_down(); // cursor 1 → cto
        let one = stats_lines(&state);
        assert!(
            one.iter()
                .any(|l| l.contains("Index:") && l.contains("cto"))
        );
        assert!(
            one.iter()
                .any(|l| l.contains("Chunks:") && l.contains("1,200"))
        );
        assert!(one.iter().any(|l| l.contains("/tmp/cto")));
    }

    #[test]
    fn test_stats_lines_graph_section() {
        // An index with non-zero node_count should produce a Graph section
        // including the nodes/edges line and edge-kind bars. The Communities
        // display was retired; the underlying fields stay on the data model.
        let mut state = sample_state();
        state.indexes[0].node_count = 4_821;
        state.indexes[0].edge_count = 12_034;
        state.indexes[0].edge_kinds = vec![
            ("CallsFunction".into(), 8_201),
            ("UsesType".into(), 2_411),
            ("Implements".into(), 1_422),
        ];
        state.indexes[0].community_count = 47;
        state.indexes[0].modularity = 0.712;
        state.select_down(); // cursor 1 → cto (the modified row)
        let lines = stats_lines(&state);
        assert!(lines.iter().any(|l| l == "Graph:"));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Nodes:") && l.contains("4,821") && l.contains("Edges:"))
        );
        assert!(lines.iter().any(|l| l.contains("CallsFunction")));
        assert!(
            !lines.iter().any(|l| l.contains("Communities")),
            "Communities display must not surface in the stats panel"
        );
    }

    #[test]
    fn test_stats_lines_no_graph_section() {
        // An index with node_count == 0 should still produce a Graph section
        // header, but the body collapses to a single hint line — and the
        // Nodes/Edges breakdown must remain hidden so the spurious edge
        // counters don't leak into the UI.
        let mut state = sample_state();
        state.indexes[0].node_count = 0;
        state.indexes[0].edge_count = 100; // ignored without nodes
        state.indexes[0].community_count = 5; // ignored without nodes
        state.select_down();
        let lines = stats_lines(&state);
        assert!(
            lines.iter().any(|l| l == "Graph:"),
            "Graph header should always appear"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("(no graph — press [r] to reindex)")),
            "empty-graph hint should appear when node_count == 0"
        );
        assert!(
            !lines.iter().any(|l| l.contains("Communities")),
            "Communities display was retired and must not surface"
        );
        assert!(
            !lines.iter().any(|l| l.contains("Nodes:")),
            "Nodes/Edges breakdown must stay hidden without nodes"
        );
    }

    #[test]
    fn test_stats_lines_edge_kind_bars() {
        // The largest edge kind gets the full 14-char bar; smaller kinds get
        // proportionally fewer bars.
        let mut state = sample_state();
        state.indexes[0].node_count = 100;
        state.indexes[0].edge_count = 200;
        state.indexes[0].edge_kinds = vec![
            ("Big".into(), 100), // max → 14 bars
            ("Half".into(), 50), // 50% → 7 bars
            ("Tiny".into(), 10), // 10% → ~1 bar
        ];
        state.select_down();
        let lines = stats_lines(&state);
        let bar_lines: Vec<&String> = lines.iter().filter(|l| l.contains('█')).collect();
        assert_eq!(bar_lines.len(), 3, "expected one bar line per edge kind");
        let big_bars = bar_lines[0].matches('█').count();
        let half_bars = bar_lines[1].matches('█').count();
        let tiny_bars = bar_lines[2].matches('█').count();
        assert_eq!(big_bars, 14, "largest kind gets 14 bars");
        assert!(
            half_bars < big_bars && half_bars > tiny_bars,
            "half-sized kind ({half_bars}) sits between big ({big_bars}) and tiny ({tiny_bars})"
        );
        assert!(
            tiny_bars >= 1,
            "tiny kind should still get at least one bar"
        );
    }

    #[test]
    fn test_log_append() {
        // The activity log is a bounded VecDeque capped at MAX_ENTRIES; the
        // search UI shares it via `state.log`.
        let mut state = SearchTuiState::new("http://x");
        for i in 0..(ActivityLog::MAX_ENTRIES + 50) {
            state.log.push(format!("event {i}"));
        }
        assert_eq!(state.log.len(), ActivityLog::MAX_ENTRIES);
    }

    #[test]
    fn test_timestamped_format() {
        // `timestamped` is the shared helper the activity log uses; assert the
        // `[HH:MM:SS] ...` shape it produces.
        let line = timestamped("reindex started");
        assert!(line.starts_with('['));
        assert!(line.ends_with(" reindex started"));
        assert_eq!(line.as_bytes()[9], b']');
    }

    /// Tag a [`ReindexEvent`] with a test index id.
    fn scoped(event: ReindexEvent) -> ScopedReindexEvent {
        ScopedReindexEvent {
            index_id: "cto".into(),
            event,
        }
    }

    #[test]
    fn test_apply_reindex_event() {
        let mut state = SearchTuiState::new("http://x");
        apply_reindex_event(
            &mut state,
            scoped(ReindexEvent::Started { total_files: 1200 }),
        );
        apply_reindex_event(
            &mut state,
            scoped(ReindexEvent::Progress {
                indexed: 600,
                total_files: 1200,
            }),
        );
        apply_reindex_event(
            &mut state,
            scoped(ReindexEvent::Complete {
                total_chunks: 19_012,
                status: "complete".into(),
            }),
        );
        let lines: Vec<&String> = state.log.iter().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("reindex started: 1200 files"));
        assert!(lines[1].contains("600/1200 (50%)"));
        assert!(lines[2].contains("reindex complete: 19.0k chunks"));

        // The events are scoped to the "cto" index, so the per-index activity
        // feed keeps them and a different index's feed does not.
        assert_eq!(state.log.tail_scoped(Some("cto"), 100).count(), 3);
        assert_eq!(state.log.tail_scoped(Some("trusty"), 100).count(), 0);

        // A failed event records an error line.
        apply_reindex_event(&mut state, scoped(ReindexEvent::Failed("disk full".into())));
        assert!(
            state
                .log
                .iter()
                .last()
                .expect("entry")
                .contains("reindex error: disk full")
        );
    }

    #[test]
    fn test_left_panel_width() {
        // Wide terminals cap the panel at LEFT_PANEL_MAX.
        assert_eq!(left_panel_width(200), tui_common::LEFT_PANEL_MAX);
        // Narrow terminals get a third of the width.
        assert_eq!(left_panel_width(60), 20);
    }

    #[test]
    fn test_index_lines() {
        // Sort by Name so the assertions below see indexes in alphabetical
        // order regardless of the default Activity-with-chunk-count tiebreak.
        let mut state = sample_state();
        state.sort_key = IndexSortKey::Name;
        let rows = index_lines(&state);
        // 1 "All" row + 3 index rows.
        assert_eq!(rows.len(), 4);
        // Row 0 is "All", selected by default, and bold-marked with `>`.
        assert!(rows[0].is_all);
        assert!(rows[0].selected);
        assert!(rows[0].text.starts_with('>'));
        assert!(rows[0].text.contains(ALL_LABEL));
        // Row 1 is the first index alphabetically, unselected.
        assert!(!rows[1].is_all && !rows[1].selected);
        assert!(rows[1].text.contains("cto"));
        // Row 3 is "trusty" (after "cto" and "duetto") and carries its chunk count.
        assert!(rows[3].text.contains("trusty"));
        assert!(rows[3].text.contains("19.0k"));

        // An empty index list still shows the "All" row plus a placeholder.
        let empty = SearchTuiState::new("http://x");
        let rows = index_lines(&empty);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].is_all);
        assert!(rows[1].text.contains("no indexes"));
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 12), "short");
        assert_eq!(truncate("a-very-long-index-id", 8), "a-very-…");
    }

    #[test]
    fn test_title_line() {
        let state = sample_state();
        let title = title_line(&state);
        assert!(title.contains("trusty-search v"));
        assert!(title.contains("online"));
        assert!(title.contains("uptime: 2h 4m"));

        // An offline daemon shows its URL rather than uptime.
        let mut offline = SearchTuiState::new("http://127.0.0.1:7878");
        offline.daemon_status = DaemonStatus::Offline {
            last_error: "refused".into(),
        };
        let title = title_line(&offline);
        assert!(title.contains("offline"));
        assert!(title.contains("http://127.0.0.1:7878"));
    }

    #[test]
    fn test_help_text_lists_bindings() {
        let text = help_text();
        for token in ["Tab", "r ", "Enter", "?", "q ", "/", "s ", "g "] {
            assert!(text.contains(token), "help text missing {token}");
        }
    }

    #[test]
    fn test_index_sort_key_cycle() {
        assert_eq!(IndexSortKey::default(), IndexSortKey::Activity);
        assert_eq!(IndexSortKey::Activity.next(), IndexSortKey::Name);
        assert_eq!(IndexSortKey::Name.next(), IndexSortKey::Count);
        assert_eq!(IndexSortKey::Count.next(), IndexSortKey::Activity);
        assert_eq!(sort_label(IndexSortKey::Activity), "Activity");
        assert_eq!(sort_label(IndexSortKey::Name), "Name");
        assert_eq!(sort_label(IndexSortKey::Count), "Chunks");
    }

    /// State with four indexes spanning two projects, varied chunk counts,
    /// and varied last_indexed timestamps. Used by the sort / filter / group
    /// tests.
    fn diverse_state() -> SearchTuiState {
        use chrono::{TimeZone, Utc};
        let mut state = SearchTuiState::new("http://127.0.0.1:7878");
        state.indexes = vec![
            IndexRow {
                id: "trusty-search".into(),
                chunk_count: 12,
                root_path: "/Users/masa/Projects/trusty-tools/trusty-search".into(),
                last_indexed: Some(Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap()),
                ..Default::default()
            },
            IndexRow {
                id: "trusty-memory".into(),
                chunk_count: 3_775,
                root_path: "/Users/masa/Projects/trusty-tools/trusty-memory".into(),
                last_indexed: Some(Utc.with_ymd_and_hms(2026, 5, 18, 22, 29, 50).unwrap()),
                ..Default::default()
            },
            IndexRow {
                id: "claude-mpm".into(),
                chunk_count: 6_163,
                root_path: "/Users/masa/Projects/claude-mpm".into(),
                last_indexed: Some(Utc.with_ymd_and_hms(2026, 5, 10, 0, 0, 0).unwrap()),
                ..Default::default()
            },
            IndexRow {
                id: "notes".into(),
                chunk_count: 100,
                root_path: String::new(),
                last_indexed: None,
                ..Default::default()
            },
        ];
        state
    }

    #[test]
    fn test_apply_sort_activity() {
        // Activity: last_indexed desc, None last; chunk_count desc tiebreak.
        let mut state = diverse_state();
        state.sort_key = IndexSortKey::Activity;
        let rows = filtered_sorted_indexes(&state);
        assert_eq!(rows[0].id, "trusty-memory");
        assert_eq!(rows[1].id, "claude-mpm");
        assert_eq!(rows[2].id, "trusty-search");
        // None sorts last.
        assert_eq!(rows[3].id, "notes");
    }

    #[test]
    fn test_apply_sort_name() {
        let mut state = diverse_state();
        state.sort_key = IndexSortKey::Name;
        let rows = filtered_sorted_indexes(&state);
        let ids: Vec<&str> = rows.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["claude-mpm", "notes", "trusty-memory", "trusty-search"]
        );
    }

    #[test]
    fn test_apply_sort_chunks() {
        let mut state = diverse_state();
        state.sort_key = IndexSortKey::Count;
        let rows = filtered_sorted_indexes(&state);
        assert_eq!(rows[0].id, "claude-mpm");
        assert_eq!(rows[1].id, "trusty-memory");
        assert_eq!(rows[2].id, "notes");
        assert_eq!(rows[3].id, "trusty-search");
    }

    #[test]
    fn test_apply_filter() {
        let mut state = diverse_state();
        // Case-insensitive substring match against id OR project.
        state.filter = "TRUSTY".into();
        let rows = filtered_sorted_indexes(&state);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|i| i.id.contains("trusty")));

        // Match by project (root_path basename).
        state.filter = "claude-mpm".into();
        let rows = filtered_sorted_indexes(&state);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "claude-mpm");

        // No match → empty.
        state.filter = "nothing-here".into();
        assert!(filtered_sorted_indexes(&state).is_empty());

        // Empty filter → everything.
        state.filter.clear();
        assert_eq!(filtered_sorted_indexes(&state).len(), 4);
    }

    #[test]
    fn test_index_lines_grouped() {
        let mut state = diverse_state();
        state.group_by_project = true;
        state.sort_key = IndexSortKey::Name;
        let rows = index_lines(&state);

        // "All" leads the list.
        assert!(rows[0].is_all);

        // Group headers appear and are non-selectable.
        let headers: Vec<&IndexListRow> = rows.iter().filter(|r| r.is_header).collect();
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
        let rows = index_lines(&state);
        let headers: Vec<&IndexListRow> = rows.iter().filter(|r| r.is_header).collect();
        assert_eq!(headers.len(), 1);
        assert!(headers[0].text.contains("claude-mpm"));
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2_048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
        assert!(format_bytes(2 * 1024 * 1024 * 1024).ends_with("GB"));
    }

    #[test]
    fn test_scroll_offset() {
        // A list taller than its viewport must scroll so the cursor stays in
        // view; a list that fits leaves the offset pinned at zero.
        let mut state = sample_state();
        // 3 indexes + the "All" row = 4 rows; an 8-row window holds them all.
        for row in 0..=state.last_row() {
            state.selected = row;
            state.sync_scroll(8);
            assert_eq!(state.scroll_offset, 0, "no scroll while the list fits");
        }

        // Grow the list well past a 5-row window and walk the cursor down.
        state.indexes = (0..40)
            .map(|n| IndexRow {
                id: format!("idx-{n}"),
                chunk_count: 1,
                root_path: String::new(),
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
    fn test_visible_index_ids() {
        let mut state = diverse_state();
        state.sort_key = IndexSortKey::Name;
        let ids = visible_index_ids(&state);
        assert_eq!(ids[0], tui_common::ALL_SENTINEL);
        assert_eq!(
            &ids[1..],
            &[
                "claude-mpm".to_string(),
                "notes".to_string(),
                "trusty-memory".to_string(),
                "trusty-search".to_string(),
            ]
        );

        state.filter = "trusty".into();
        let ids = visible_index_ids(&state);
        assert_eq!(ids[0], tui_common::ALL_SENTINEL);
        assert_eq!(ids.len(), 3, "All + 2 trusty-* indexes");
    }

    #[test]
    fn test_navigate_visible() {
        let mut state = diverse_state();
        state.sort_key = IndexSortKey::Name;
        // Visible order: All, claude-mpm, notes, trusty-memory, trusty-search.
        assert_eq!(state.selected, 0);
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("claude-mpm"));
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("notes"));
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("trusty-memory"));
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("trusty-search"));
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("trusty-search"));
        navigate_up_visible(&mut state);
        assert_eq!(state.selected_id(), Some("trusty-memory"));
        navigate_up_visible(&mut state);
        navigate_up_visible(&mut state);
        navigate_up_visible(&mut state);
        assert!(state.is_all_selected());
        navigate_up_visible(&mut state);
        assert!(state.is_all_selected());

        // With a filter, navigation skips hidden rows.
        state.filter = "trusty".into();
        state.selected = 0;
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("trusty-memory"));
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("trusty-search"));
        navigate_down_visible(&mut state);
        assert_eq!(state.selected_id(), Some("trusty-search"));
    }

    #[test]
    fn test_visible_selected_row_follows_sort() {
        // The visible row index for the highlight must follow the rendered
        // (filter + sort) order, not the original `state.indexes` order.
        // Diverse indexes (in original order): trusty-search, trusty-memory,
        // claude-mpm, notes. Selecting "claude-mpm" places it at cursor 3
        // (index 2 + 1). With Name sort the displayed order is:
        //   0 All, 1 claude-mpm, 2 notes, 3 trusty-memory, 4 trusty-search
        // so the highlight must land on row 1, not row 3.
        let mut state = diverse_state();
        state.sort_key = IndexSortKey::Name;
        let pos = state
            .indexes
            .iter()
            .position(|i| i.id == "claude-mpm")
            .expect("index");
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

        // With Chunks sort the displayed order is:
        //   0 All, 1 claude-mpm, 2 trusty-memory, 3 notes, 4 trusty-search
        // so notes must land on row 3.
        state.sort_key = IndexSortKey::Count;
        let pos = state
            .indexes
            .iter()
            .position(|i| i.id == "notes")
            .expect("index");
        state.selected = pos + 1;
        assert_eq!(visible_selected_row(&state), 3);
    }

    #[test]
    fn test_visible_selected_row_follows_group() {
        // Grouping interleaves project headers (non-selectable) with indexes;
        // the highlight row must skip over them and follow the grouped layout.
        let mut state = diverse_state();
        state.sort_key = IndexSortKey::Name;
        state.group_by_project = true;
        let pos = state
            .indexes
            .iter()
            .position(|i| i.id == "trusty-memory")
            .expect("index");
        state.selected = pos + 1;
        let expected = index_lines(&state)
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
        state.sort_key = IndexSortKey::Name;
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
        let mut state = diverse_state();
        state.sort_key = IndexSortKey::Name;
        let pos = state
            .indexes
            .iter()
            .position(|i| i.id == "claude-mpm")
            .expect("index");
        state.selected = pos + 1;
        state.filter = "trusty".into();
        state.clamp_to_visible();
        assert_eq!(state.selected, 0, "selection dropped to All");

        state.filter = "trusty".into();
        let pos = state
            .indexes
            .iter()
            .position(|i| i.id == "trusty-memory")
            .expect("index");
        state.selected = pos + 1;
        state.clamp_to_visible();
        assert_eq!(state.selected_id(), Some("trusty-memory"));
    }

    #[test]
    fn test_render_smoke() {
        // A full render in several states must not panic — exercise both the
        // "All" selection (aggregated panels) and a single-index selection.
        let mut state = sample_state();
        state.log.push("daemon started");
        state.log.push_scoped("cto", "reindex started: 1200 files");
        state
            .log
            .push_scoped("trusty", "search \"fn embed\" → 5 results");
        state.input = "fn authenticate".into();
        state.focus = SearchFocus::Input;
        for (w, h) in [(120u16, 30u16), (80, 24)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            terminal
                .draw(|f| render(f, &mut state))
                .expect("render (All) must not panic");
        }
        // Single-index selection — the right panels scope to that index.
        state.selected = 1;
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| render(f, &mut state))
            .expect("render (single index) must not panic");

        // A list far longer than the panel must render (and scroll) cleanly.
        state.indexes = (0..60)
            .map(|n| IndexRow {
                id: format!("idx-{n}"),
                chunk_count: 100,
                root_path: String::new(),
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

        // Help overlay and offline daemon paths.
        state.show_help = true;
        state.daemon_status = DaemonStatus::Connecting;
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| render(f, &mut state))
            .expect("help render must not panic");
    }

    #[test]
    fn test_new_log_lines_since_watermark() {
        let lines: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // watermark "c" → returns ["d","e"]
        assert_eq!(new_log_lines_since(&lines, Some("c")), &["d", "e"]);
        // watermark at last line → empty
        assert_eq!(new_log_lines_since(&lines, Some("e")), &[] as &[String]);
        // watermark not in list → all lines (ring buffer wrapped)
        assert_eq!(new_log_lines_since(&lines, Some("z")), lines.as_slice());
        // no watermark → all lines
        assert_eq!(new_log_lines_since(&lines, None), lines.as_slice());
        // empty input → empty output
        assert!(new_log_lines_since(&[], Some("a")).is_empty());
    }

    #[test]
    fn test_push_new_log_lines_skips_first_poll() {
        let mut state = SearchTuiState::new("http://x");
        assert!(state.log_first_poll);
        let lines: Vec<String> = ["info: daemon started", "info: index loaded"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // Simulate first poll: no lines pushed, watermark recorded.
        if state.log_first_poll {
            state.log_watermark = lines.last().cloned();
            state.log_first_poll = false;
        }
        assert!(!state.log_first_poll);
        assert!(state.log.is_empty());
        // Simulate second poll with a new line appended.
        let lines2: Vec<String> = [
            "info: daemon started",
            "info: index loaded",
            "info: watch triggered",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let new = new_log_lines_since(&lines2, state.log_watermark.as_deref());
        for line in new {
            state.log.push(line.clone());
        }
        state.log_watermark = lines2.last().cloned();
        assert_eq!(state.log.len(), 1);
        assert!(
            state
                .log
                .iter()
                .next()
                .map(|l| l.contains("watch triggered"))
                .unwrap_or(false)
        );
    }
}
