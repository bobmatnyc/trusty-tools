//! Shared TUI infrastructure for the trusty-search and trusty-memory monitors.
//!
//! Why: `search_tui.rs` and `memory_tui.rs` were ~80% byte-for-byte identical —
//! same navigation algorithms (filter / sort / group / scroll), same focus and
//! sort-key enums, same panel rendering helpers, same terminal raw-mode
//! setup / teardown. Centralising the shared pieces here keeps both TUIs in
//! sync and shrinks each file to its domain-specific surface (memory
//! recall / dream events, search reindex / disk stats). Lives behind the
//! `monitor-tui` feature so non-TUI consumers do not pull ratatui / crossterm.
//! What: a [`ListItem`] trait both [`super::dashboard::PalaceRow`] and
//! [`super::dashboard::IndexRow`] implement; a `SortKey` trait plus the
//! three-variant [`ThreeWaySortKey`] enum (callers pass a label array so the
//! third variant can read as "Vectors" or "Chunks"); a [`ListFocus`] enum
//! replacing the per-TUI focus duplicates; the [`ALL_SENTINEL`] /
//! [`ACTIVITY_PERCENT`] / [`LEFT_PANEL_MAX`] constants; pure rendering helpers
//! ([`truncate`], [`left_panel_width`], [`panel_block`],
//! [`render_help_overlay`]); generic navigation helpers
//! ([`filtered_sorted`], [`visible_ids`], [`navigate_up`], [`navigate_down`])
//! parameterised over a slice of `T: ListItem` plus the caller's filter,
//! grouping, and sort comparator; and [`enter_tui`] / [`leave_tui`] for the
//! terminal raw-mode dance.
//! Test: `cargo test -p trusty-common --features monitor-tui` covers the pure
//! helpers; the per-TUI tests in `search_tui` and `memory_tui` exercise the
//! shared navigation via their existing assertions.

use std::io::{self, Stdout};

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Clear, Paragraph},
};

/// Sentinel id reserved for the synthetic "All …" row at the top of a list.
///
/// Why: arrow navigation walks the *visible* (filtered + sorted) row order and
/// the "All" row has no real id; this sentinel slots into the id list at
/// position 0 so the same code path can handle both selection cases.
/// What: the literal `"__all__"` — chosen not to collide with any real
/// palace / index id.
/// Test: covered indirectly by `test_visible_palace_ids` /
/// `test_visible_index_ids` in the per-TUI tests.
pub const ALL_SENTINEL: &str = "__all__";

/// Percentage of the right-hand pane the ACTIVITY panel claims.
///
/// Why: both TUIs split the right column 60 / 40 between ACTIVITY (top) and
/// STATISTICS (bottom). Naming the constant documents the ratio and keeps the
/// two renderers in sync.
/// What: 60 — STATISTICS takes the remaining 40.
/// Test: side-effect-free constant; both render smoke tests exercise the path.
pub const ACTIVITY_PERCENT: u16 = 60;

/// Maximum width (in columns) of the left list panel (INDEXES / PALACES).
///
/// Why: on wide terminals the list panel must not consume the activity log;
/// capping it at 28 columns gives the right pane the bulk of the width.
/// What: 28 columns.
/// Test: `test_left_panel_width` in each TUI.
pub const LEFT_PANEL_MAX: u16 = 28;

/// Sort orders supported by every three-way list cycle.
///
/// Why: both TUIs cycle through three sort orders — most-recently-active,
/// alphabetical, and count-heavy. A shared enum keeps the variants in sync and
/// lets the renderer ask for a domain-specific label via the caller-supplied
/// label array (so memory shows "Vectors" and search shows "Chunks").
/// What: `Activity` (default — newest activity first), `Name` (alphabetical
/// asc), `Count` (count desc).
/// Test: `test_three_way_sort_key_cycle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreeWaySortKey {
    /// Sort by activity timestamp desc; count desc as tiebreak; nulls last.
    #[default]
    Activity,
    /// Sort alphabetically by name / id (ascending).
    Name,
    /// Sort by count desc (vectors for memory, chunks for search).
    Count,
}

impl ThreeWaySortKey {
    /// Advance to the next sort key in the cycle.
    ///
    /// Why: the `[s]` binding cycles through the three sort orders.
    /// What: `Activity → Name → Count → Activity`.
    /// Test: `test_three_way_sort_key_cycle`.
    pub fn next(self) -> Self {
        match self {
            Self::Activity => Self::Name,
            Self::Name => Self::Count,
            Self::Count => Self::Activity,
        }
    }

    /// Look up the caller's domain-specific label for this variant.
    ///
    /// Why: the third sort key reads as "Vectors" in the memory TUI and
    /// "Chunks" in the search TUI; rather than parameterising the enum,
    /// callers pass a 3-element label array `[Activity, Name, Count]` and this
    /// indexes into it. Keeps the enum domain-agnostic.
    /// What: returns `labels[0]` for Activity, `labels[1]` for Name,
    /// `labels[2]` for Count.
    /// Test: `test_three_way_sort_key_label`.
    pub fn label(self, labels: &[&'static str; 3]) -> &'static str {
        match self {
            Self::Activity => labels[0],
            Self::Name => labels[1],
            Self::Count => labels[2],
        }
    }
}

/// Which zone of a two-pane TUI currently holds keyboard focus.
///
/// Why: both TUIs offer a list panel and an input bar; `[Tab]` cycles focus and
/// the two zones consume keys differently (navigation vs. text entry). A shared
/// enum keeps both TUIs in sync.
/// What: `List` (the default — the INDEXES / PALACES panel) or `Input` (the
/// SEARCH / RECALL bar).
/// Test: `test_list_focus_toggle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListFocus {
    /// The list panel has focus; arrows move the selection.
    #[default]
    List,
    /// The input bar has focus; typed characters edit the query.
    Input,
}

impl ListFocus {
    /// Flip between [`ListFocus::List`] and [`ListFocus::Input`].
    ///
    /// Why: `[Tab]` decides whether arrows navigate the list or whether typed
    /// characters edit the query buffer.
    /// What: returns the opposite variant.
    /// Test: `test_list_focus_toggle`.
    pub fn toggled(self) -> Self {
        match self {
            Self::List => Self::Input,
            Self::Input => Self::List,
        }
    }
}

/// Common surface every list row exposes for filter / sort / group / nav.
///
/// Why: both TUIs share the same filtering, sorting, and grouping algorithms;
/// abstracting over the concrete row type with this trait lets the algorithms
/// live in one place. The lifetime-free getters keep the trait simple to
/// implement on both [`super::dashboard::PalaceRow`] and
/// [`super::dashboard::IndexRow`].
/// What: stable id, display name, optional project label, optional activity
/// timestamp, and an item count (vectors for memory, chunks for search).
/// Test: implementations are exercised through the TUI navigation tests.
pub trait ListItem {
    /// The stable id used to map selections back to the original Vec.
    fn id(&self) -> &str;
    /// The display name (falls back to id when empty in caller's renderer).
    fn name(&self) -> &str;
    /// The inferred project, used for grouping and filtering by project.
    fn project(&self) -> &str;
    /// The most recent activity timestamp, when reported.
    fn activity_ts(&self) -> Option<chrono::DateTime<chrono::Utc>>;
    /// The item count — vectors for palaces, chunks for indexes.
    fn count(&self) -> u64;
}

/// Case-insensitive substring match against `name` or `project`.
///
/// Why: both TUIs apply the same filter rule against the same two fields.
/// What: returns `true` when `filter` is empty, or when the lowercase form of
/// `name` or `project` contains the lowercase form of `filter`.
/// Test: covered by `test_apply_filter` in both TUIs.
pub fn matches_filter<T: ListItem>(item: &T, filter_lower: &str) -> bool {
    if filter_lower.is_empty() {
        return true;
    }
    item.name().to_lowercase().contains(filter_lower)
        || item.project().to_lowercase().contains(filter_lower)
}

/// Filter, then sort, a slice of list items.
///
/// Why: filtering and sorting are pure functions over `(filter, sort_key)`
/// shared by both TUIs; isolating them keeps the per-TUI builders terse.
/// What: returns the items whose `name()` or `project()` contains `filter`
/// (case-insensitive), then sorts by `sort_key`:
///   - `Activity`: `activity_ts` desc, None last; `count` desc as tiebreak.
///   - `Name`: `name` ascending.
///   - `Count`: `count` desc.
///
/// Test: covered by `test_apply_sort_*` and `test_apply_filter` in both TUIs.
pub fn filtered_sorted<T: ListItem + Clone>(
    items: &[T],
    filter: &str,
    sort_key: ThreeWaySortKey,
) -> Vec<T> {
    let filter_lower = filter.to_lowercase();
    let mut rows: Vec<T> = items
        .iter()
        .filter(|item| matches_filter(*item, &filter_lower))
        .cloned()
        .collect();
    match sort_key {
        ThreeWaySortKey::Activity => {
            rows.sort_by(|a, b| match (a.activity_ts(), b.activity_ts()) {
                (Some(x), Some(y)) => y.cmp(&x).then_with(|| b.count().cmp(&a.count())),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => b.count().cmp(&a.count()),
            });
        }
        ThreeWaySortKey::Name => rows.sort_by(|a, b| a.name().cmp(b.name())),
        ThreeWaySortKey::Count => rows.sort_by_key(|b| std::cmp::Reverse(b.count())),
    }
    rows
}

/// Build the ordered id list (leading with the [`ALL_SENTINEL`]) that arrow
/// navigation walks.
///
/// Why: when a filter, sort, or grouping is active the displayed row order no
/// longer matches the original `items` Vec; arrow keys must step through the
/// visible order or they appear to skip rows.
/// What: returns `[ALL_SENTINEL, …visible item ids…]`. With `group_by_project`
/// the items are walked in (project first-seen) × (sorted within project)
/// order, mirroring how the renderer interleaves group headers — headers
/// themselves are not included (they are non-selectable).
/// Test: covered by `test_visible_palace_ids` / `test_visible_index_ids`.
pub fn visible_ids<T: ListItem + Clone>(
    items: &[T],
    filter: &str,
    sort_key: ThreeWaySortKey,
    group_by_project: bool,
) -> Vec<String> {
    let visible = filtered_sorted(items, filter, sort_key);
    let mut ids = Vec::with_capacity(visible.len() + 1);
    ids.push(ALL_SENTINEL.to_string());
    if group_by_project {
        let mut seen: Vec<String> = Vec::new();
        for item in &visible {
            let proj = item.project().to_string();
            if !seen.iter().any(|s| s == &proj) {
                seen.push(proj);
            }
        }
        for project in &seen {
            for item in visible.iter().filter(|i| i.project() == project) {
                ids.push(item.id().to_string());
            }
        }
    } else {
        for row in &visible {
            ids.push(row.id().to_string());
        }
    }
    ids
}

/// Translate the original-Vec cursor into a visible-id string.
///
/// Why: `selected` indexes the original `items` Vec, but navigation works in
/// visible order; this is the bridge from one space to the other.
/// What: returns [`ALL_SENTINEL`] when `selected == 0` or `selected` is out of
/// range, otherwise the id of `items[selected - 1]`.
/// Test: covered by the per-TUI navigation tests.
pub fn current_visible_id<T: ListItem>(items: &[T], selected: usize) -> String {
    if selected == 0 {
        return ALL_SENTINEL.to_string();
    }
    items
        .get(selected - 1)
        .map(|i| i.id().to_string())
        .unwrap_or_else(|| ALL_SENTINEL.to_string())
}

/// Resolve a visible id back to an original-Vec cursor.
///
/// Why: after navigation picks the next visible id, downstream code reads
/// `selected` as an index into the original `items` Vec; this converts back.
/// What: returns `0` for [`ALL_SENTINEL`], otherwise `position(target) + 1`.
/// A missing id returns `None` so the caller can leave the cursor unchanged.
/// Test: covered by the per-TUI navigation tests.
pub fn id_to_cursor<T: ListItem>(items: &[T], target_id: &str) -> Option<usize> {
    if target_id == ALL_SENTINEL {
        return Some(0);
    }
    items
        .iter()
        .position(|i| i.id() == target_id)
        .map(|p| p + 1)
}

/// Step the cursor one visible row in `delta` direction.
///
/// Why: arrow keys must walk visible order; mapping the current cursor to its
/// position in `visible_ids` and stepping by `delta` keeps navigation faithful
/// to what the user sees.
/// What: returns the new `selected` cursor after stepping `+1` (down) or `-1`
/// (up). At the ends the cursor stays put. When the current id is not visible
/// (e.g. just filtered out) the cursor drops to 0 ("All"). When the new id
/// cannot be mapped back the cursor is unchanged.
/// Test: covered by `test_navigate_visible` in both TUIs.
pub fn navigate_step<T: ListItem + Clone>(
    items: &[T],
    selected: usize,
    filter: &str,
    sort_key: ThreeWaySortKey,
    group_by_project: bool,
    delta: i32,
) -> usize {
    let ids = visible_ids(items, filter, sort_key, group_by_project);
    let current = current_visible_id(items, selected);
    let Some(pos) = ids.iter().position(|id| id == &current) else {
        return 0;
    };
    let new_pos: usize = if delta < 0 {
        if pos == 0 {
            return selected;
        }
        pos - 1
    } else {
        if pos + 1 >= ids.len() {
            return selected;
        }
        pos + 1
    };
    let new_id = &ids[new_pos];
    id_to_cursor(items, new_id).unwrap_or(selected)
}

/// Convenience: navigate one row up in the visible list.
///
/// Why: tiny wrapper so call sites read intent at a glance.
/// What: delegates to [`navigate_step`] with `delta = -1`.
/// Test: covered by `test_navigate_visible`.
pub fn navigate_up<T: ListItem + Clone>(
    items: &[T],
    selected: usize,
    filter: &str,
    sort_key: ThreeWaySortKey,
    group_by_project: bool,
) -> usize {
    navigate_step(items, selected, filter, sort_key, group_by_project, -1)
}

/// Convenience: navigate one row down in the visible list.
///
/// Why: tiny wrapper so call sites read intent at a glance.
/// What: delegates to [`navigate_step`] with `delta = +1`.
/// Test: covered by `test_navigate_visible`.
pub fn navigate_down<T: ListItem + Clone>(
    items: &[T],
    selected: usize,
    filter: &str,
    sort_key: ThreeWaySortKey,
    group_by_project: bool,
) -> usize {
    navigate_step(items, selected, filter, sort_key, group_by_project, 1)
}

/// Truncate `s` to at most `max` characters, appending `…` when cut.
///
/// Why: list rows and panel titles use fixed-width columns; bounded labels
/// keep them aligned. Shared between both TUIs.
/// What: returns `s` unchanged when its `chars().count()` ≤ `max`, else the
/// first `max - 1` characters plus `…`.
/// Test: `test_truncate`.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

/// Compute the width (in columns) of the left list panel.
///
/// Why: caps the list panel so the activity log gets the bulk of the width on
/// wide terminals; the same formula is used by both TUIs.
/// What: returns `min(LEFT_PANEL_MAX, width / 3)`.
/// Test: `test_left_panel_width`.
pub fn left_panel_width(width: u16) -> u16 {
    LEFT_PANEL_MAX.min(width / 3)
}

/// Build a bordered block for a UI panel, highlighting it when focused.
///
/// Why: every panel in both TUIs shares this border-and-title pattern.
/// What: returns a [`Block`] titled `" {name} "` with a thick cyan border when
/// `focused`, a dim gray border otherwise.
/// Test: side-effect-only ratatui widget; render smoke tests exercise it.
pub fn panel_block(name: &str, focused: bool) -> Block<'static> {
    let border_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            format!(" {name} "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
}

/// Render a centred help overlay with the given multi-line `help` text.
///
/// Why: both TUIs show a floating help reference when `?` is pressed; sharing
/// the overlay geometry keeps the two consistent.
/// What: clears a centred 60×9 rectangle (capped at the frame size) and draws
/// `help` inside a bordered block.
/// Test: side-effect-only ratatui rendering; render smoke tests exercise it.
pub fn render_help_overlay(frame: &mut Frame, help: &str) {
    let area = frame.area();
    let w = 60.min(area.width);
    let h = 9.min(area.height);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(help.to_string())
            .style(Style::default().fg(Color::White))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Help — press ? or Esc to close "),
            ),
        rect,
    );
}

/// Enter the alternate screen with raw-mode keyboard + mouse capture.
///
/// Why: every TUI in the workspace performs the same crossterm initialisation
/// dance before spinning up ratatui; centralising it removes drift between the
/// daemons (e.g. one capturing mouse events, another not) and makes the
/// terminal-glue testable in one place.
/// What: enables raw mode, switches stdout to the alternate screen with mouse
/// capture, and returns a fresh [`Terminal`] backed by [`CrosstermBackend`].
/// Test: side-effect-only terminal glue; exercised by launching any TUI.
pub fn enter_tui() -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal after a TUI exits.
///
/// Why: mirrors [`enter_tui`]; called unconditionally on every exit path so a
/// panicking event loop still leaves the operator's shell in a sane state.
/// What: disables raw mode, releases mouse capture, leaves the alternate
/// screen, and shows the cursor.
/// Test: side-effect-only terminal glue; exercised by launching any TUI.
pub fn leave_tui(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

// `ListItem` implementations live in `dashboard.rs` alongside the row types
// themselves so the trait bound is satisfied without a circular module
// dependency.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::dashboard::{IndexRow, PalaceRow};

    #[test]
    fn test_three_way_sort_key_cycle() {
        assert_eq!(ThreeWaySortKey::default(), ThreeWaySortKey::Activity);
        assert_eq!(ThreeWaySortKey::Activity.next(), ThreeWaySortKey::Name);
        assert_eq!(ThreeWaySortKey::Name.next(), ThreeWaySortKey::Count);
        assert_eq!(ThreeWaySortKey::Count.next(), ThreeWaySortKey::Activity);
    }

    #[test]
    fn test_three_way_sort_key_label() {
        let mem = &["Activity", "Name", "Vectors"];
        let search = &["Activity", "Name", "Chunks"];
        assert_eq!(ThreeWaySortKey::Activity.label(mem), "Activity");
        assert_eq!(ThreeWaySortKey::Name.label(mem), "Name");
        assert_eq!(ThreeWaySortKey::Count.label(mem), "Vectors");
        assert_eq!(ThreeWaySortKey::Count.label(search), "Chunks");
    }

    #[test]
    fn test_list_focus_toggle() {
        assert_eq!(ListFocus::default(), ListFocus::List);
        assert_eq!(ListFocus::List.toggled(), ListFocus::Input);
        assert_eq!(ListFocus::Input.toggled(), ListFocus::List);
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 12), "short");
        assert_eq!(truncate("a-very-long-id", 8), "a-very-…");
        assert_eq!(truncate("", 4), "");
    }

    #[test]
    fn test_left_panel_width() {
        assert_eq!(left_panel_width(200), LEFT_PANEL_MAX);
        assert_eq!(left_panel_width(60), 20);
        assert_eq!(left_panel_width(30), 10);
    }

    #[test]
    fn test_filtered_sorted_palaces() {
        // Implementations of ListItem for PalaceRow live in dashboard.rs.
        let palaces = vec![
            PalaceRow {
                id: "a".into(),
                name: "alpha".into(),
                vector_count: 100,
                ..Default::default()
            },
            PalaceRow {
                id: "b".into(),
                name: "beta".into(),
                vector_count: 50,
                ..Default::default()
            },
        ];
        let rows = filtered_sorted(&palaces, "", ThreeWaySortKey::Count);
        assert_eq!(rows[0].id, "a", "Count sort puts higher count first");
        let rows = filtered_sorted(&palaces, "BETA", ThreeWaySortKey::Name);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "b");
    }

    #[test]
    fn test_filtered_sorted_indexes() {
        let indexes = vec![
            IndexRow {
                id: "alpha".into(),
                chunk_count: 100,
                ..Default::default()
            },
            IndexRow {
                id: "beta".into(),
                chunk_count: 50,
                ..Default::default()
            },
        ];
        let rows = filtered_sorted(&indexes, "", ThreeWaySortKey::Name);
        assert_eq!(rows[0].id, "alpha");
        let rows = filtered_sorted(&indexes, "BETA", ThreeWaySortKey::Name);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "beta");
    }

    #[test]
    fn test_visible_ids_and_navigation() {
        let items = vec![
            PalaceRow {
                id: "a".into(),
                name: "alpha".into(),
                vector_count: 10,
                ..Default::default()
            },
            PalaceRow {
                id: "b".into(),
                name: "beta".into(),
                vector_count: 5,
                ..Default::default()
            },
        ];
        let ids = visible_ids(&items, "", ThreeWaySortKey::Name, false);
        assert_eq!(ids, vec![ALL_SENTINEL, "a", "b"]);

        // Down from All → first item; mapping back gives cursor 1.
        let next = navigate_down(&items, 0, "", ThreeWaySortKey::Name, false);
        assert_eq!(next, 1);
        // Down from cursor 1 (visible pos 1) → cursor 2.
        let next = navigate_down(&items, 1, "", ThreeWaySortKey::Name, false);
        assert_eq!(next, 2);
        // Bottom is a no-op.
        let next = navigate_down(&items, 2, "", ThreeWaySortKey::Name, false);
        assert_eq!(next, 2);
        // Up from middle.
        let next = navigate_up(&items, 2, "", ThreeWaySortKey::Name, false);
        assert_eq!(next, 1);
        // Top is a no-op.
        let next = navigate_up(&items, 0, "", ThreeWaySortKey::Name, false);
        assert_eq!(next, 0);
    }
}
