//! Rendering helpers: row builders, stat lines, title line, and the ratatui draw call.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::monitor::dashboard::{IndexRow, format_count};
use crate::monitor::tui_common::{
    self, ListFocus, ThreeWaySortKey, left_panel_width, panel_block, truncate,
};
use crate::monitor::utils::{DaemonStatus, fmt_uptime};

use super::nav::{filtered_sorted_indexes, visible_selected_row};
use super::state::SearchTuiState;

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
        let text = if state.daemon_status == DaemonStatus::Connecting {
            "  Loading…".to_string()
        } else {
            "  (no indexes registered)".to_string()
        };
        rows.push(IndexListRow {
            text,
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

    let cursor_for = |idx: &IndexRow| -> usize {
        state
            .indexes
            .iter()
            .position(|orig| orig.id == idx.id)
            .map(|i| i + 1)
            .unwrap_or(0)
    };

    if state.group_by_project {
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
/// terminal. While the daemon is still being polled for the first time we
/// surface a "Loading…" placeholder so the panel does not flash zeroes that
/// are indistinguishable from a genuinely empty daemon.
/// What: for a single index, returns its id, chunk count, and indexed root
/// path. For the "All" selection, returns the index count, the summed chunk
/// count, and one `· <id>: <chunks>` breakdown line per index. Returns
/// `["Loading…"]` while the daemon status is [`DaemonStatus::Connecting`].
/// Test: `test_stats_lines`, `test_stats_lines_connecting_shows_loading`.
pub fn stats_lines(state: &SearchTuiState) -> Vec<String> {
    if state.daemon_status == DaemonStatus::Connecting {
        return vec!["Loading…".to_string()];
    }
    if state.is_all_selected() {
        let total: u64 = state.indexes.iter().map(|i| i.chunk_count).sum();
        let total_nodes: u64 = state.indexes.iter().map(|i| i.node_count).sum();
        let mut lines = vec![
            format!("Scope:        {ALL_LABEL}"),
            format!("Indexes:      {}", state.indexes.len()),
            format!("Total chunks: {}", format_count(total)),
        ];
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
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title_line(state),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );

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
            } else if row.is_header || row.is_all {
                // Group headers and the unselected "All" row — bold yellow.
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(row.text, style)))
        })
        .collect();

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

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(tui_common::ACTIVITY_PERCENT),
            Constraint::Percentage(100 - tui_common::ACTIVITY_PERCENT),
        ])
        .split(split[1]);

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
    } else if state.daemon_status == DaemonStatus::Connecting {
        vec![ListItem::new("Loading…")]
    } else {
        vec![ListItem::new("(no activity yet)")]
    };
    frame.render_widget(
        List::new(activity_items).block(panel_block(&activity_title, false)),
        right[0],
    );

    let stats_items: Vec<ListItem> = stats_lines(state).into_iter().map(ListItem::new).collect();
    frame.render_widget(
        List::new(stats_items).block(panel_block("STATISTICS", false)),
        right[1],
    );

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
