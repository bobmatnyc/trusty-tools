//! Width-driven column layout for the `tm ls` session TUI (#7224).
//!
//! Why: the pre-#7224 picker computed its column widths once, from
//! `terminal_width()` at startup, so a resize left the menu formatted for a
//! terminal that no longer existed. A TUI redraws every frame, which means the
//! column set has to be a pure function of the CURRENT width — and a pure
//! function is the only part of a TUI that can be unit-tested without a
//! terminal.
//!
//! What: [`visible_columns`] picks which columns fit at a given width by
//! PRIORITY (name, then state, project, id, tmux) and returns them in DISPLAY
//! order; [`header`] and [`cell`] build one column's text, truncating with an
//! ellipsis through the same [`super::super::managed_render::truncate`] the
//! static table uses. [`visible_window`] is the scroll seam: which slice of the
//! rows a viewport of `height` shows with `selected` inside it.
//!
//! Column vocabulary matches the console's (#7224): friendly name, short id,
//! project, state, tmux pane.
//!
//! Test: `layout_*` and `window_*` in `super::tests`.

use trusty_mpm::client::ManagedSessionSummary;

use super::super::managed_render::truncate;

/// One column of the session table.
///
/// Why: a typed set — rather than a width table indexed by string — is what
/// lets [`visible_columns`] express priority and display order as two orderings
/// over the same values without either drifting from the render.
/// What: the five columns the console's session vocabulary names.
/// Test: `layout_columns_*` in `super::tests`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Column {
    /// The session's friendly (tmux) name.
    Name,
    /// The first [`SHORT_ID_LEN`] characters of the session UUID.
    Id,
    /// The `owner/repo` project slug, or `-` when the session has none.
    Project,
    /// The lifecycle state word plus the static table's annotations.
    State,
    /// The tmux pane the session's runtime occupies, or `-`.
    Tmux,
}

/// How many characters of the session UUID the `ID` column shows.
///
/// Eight is the git-short-hash convention and stays unambiguous across a fleet
/// of a few hundred sessions; the full UUID is a copy-paste target, and the TUI
/// is not where anyone copies one from.
pub(crate) const SHORT_ID_LEN: usize = 8;

/// Rendered width of each column, excluding the one-space gutter between them.
const NAME_WIDTH: u16 = 28;
const ID_WIDTH: u16 = SHORT_ID_LEN as u16;
const PROJECT_WIDTH: u16 = 24;
const STATE_WIDTH: u16 = 18;
const TMUX_WIDTH: u16 = 8;

/// Width the selection marker column takes on the left of every row.
pub(crate) const MARKER_WIDTH: u16 = 2;

/// The gutter drawn between two adjacent columns.
const GUTTER: u16 = 1;

/// Columns in the order they are DROPPED from the right of the priority list.
///
/// Name is not in this list: it is unconditional, because a row with no name is
/// not a row anyone can act on.
const BY_PRIORITY: [Column; 4] = [Column::State, Column::Project, Column::Id, Column::Tmux];

/// Columns in the order they are DRAWN, left to right.
const BY_DISPLAY: [Column; 5] = [
    Column::Name,
    Column::Id,
    Column::Project,
    Column::State,
    Column::Tmux,
];

/// This column's fixed rendered width.
pub(crate) fn width(column: Column) -> u16 {
    match column {
        Column::Name => NAME_WIDTH,
        Column::Id => ID_WIDTH,
        Column::Project => PROJECT_WIDTH,
        Column::State => STATE_WIDTH,
        Column::Tmux => TMUX_WIDTH,
    }
}

/// The header label for a column.
pub(crate) fn header(column: Column) -> &'static str {
    match column {
        Column::Name => "NAME",
        Column::Id => "ID",
        Column::Project => "PROJECT",
        Column::State => "STATE",
        Column::Tmux => "TMUX",
    }
}

/// Which columns fit in `total_width`, in display order (#7224 requirement 2).
///
/// Why: below roughly 100 columns the full set cannot be drawn without
/// wrapping, and a wrapped table is unreadable. Dropping whole columns from the
/// least-load-bearing end beats squeezing every column into an ellipsis, which
/// would leave five columns of nothing but `…`.
/// What: keeps [`Column::Name`] unconditionally, then admits each column of
/// [`BY_PRIORITY`] while the running total — marker, columns, and one gutter
/// per column after the first — still fits. The admitted set is returned in
/// [`BY_DISPLAY`] order, so priority never leaks into what the operator sees.
/// A width too small even for `NAME` still returns `[Name]`: the table clips
/// what it cannot draw, and returning nothing would render an empty screen.
/// Test: `layout_columns_full_set_at_wide_width`,
/// `layout_columns_drops_tmux_then_id_at_eighty`,
/// `layout_columns_keeps_name_at_absurdly_narrow_width`.
pub(crate) fn visible_columns(total_width: u16) -> Vec<Column> {
    let mut used = MARKER_WIDTH + NAME_WIDTH;
    let mut kept = vec![Column::Name];
    for column in BY_PRIORITY {
        let cost = GUTTER + width(column);
        if used.saturating_add(cost) > total_width {
            continue;
        }
        used += cost;
        kept.push(column);
    }
    BY_DISPLAY
        .into_iter()
        .filter(|c| kept.contains(c))
        .collect()
}

/// Build one column's cell text for `session`, truncated to the column width.
///
/// Why: truncation goes through the static table's own [`truncate`] so a name
/// clipped in the TUI is clipped identically in `tm ls --plain` — the ellipsis
/// is the same character in both, not two conventions for one fleet.
/// What: `Name` is the friendly name; `Id` the first [`SHORT_ID_LEN`]
/// characters of the UUID (no ellipsis — a short id is a prefix, and an `…`
/// would make it un-pasteable); `Project` the `source_id` or `-`; `State` the
/// static table's annotated state cell, with `attached` winning over the raw
/// word; `Tmux` the pane id or `-`.
/// Test: `layout_cell_*` in `super::tests`.
pub(crate) fn cell(column: Column, session: &ManagedSessionSummary) -> String {
    let cap = width(column) as usize;
    match column {
        Column::Name => truncate(&session.name, cap),
        Column::Id => session.id.chars().take(SHORT_ID_LEN).collect(),
        Column::Project => truncate(session.source_id.as_deref().unwrap_or("-"), cap),
        Column::State => truncate(&state_cell(session), cap),
        Column::Tmux => truncate(session.pane_id.as_deref().unwrap_or("-"), cap),
    }
}

/// The `STATE` cell text, annotations included.
///
/// Why: the annotations (`[dead]`, `[stale-assets]`, `[resume-parked]`) are the
/// difference between a stopped session someone can resume and one nothing will
/// ever revive. Reusing
/// [`format_state_column`](super::super::managed_render::format_state_column)
/// keeps the TUI and the static table from disagreeing about which is which.
/// What: `attached` replaces the raw state word — a client is connected right
/// now, the strongest signal — then the shared formatter appends the markers.
/// Test: `layout_cell_state_uses_the_shared_annotations`.
pub(crate) fn state_cell(session: &ManagedSessionSummary) -> String {
    let base = if session.attached {
        "attached"
    } else {
        session.state.as_str()
    };
    super::super::managed_render::format_state_column(
        base,
        session.unresumable,
        session.stale_assets,
        session.stale_assets_unchecked,
        session.auto_resume_parked.is_some(),
    )
}

/// Which slice of `total` rows a `height`-row viewport shows around `selected`.
///
/// Why: ratatui's `TableState` recomputes its scroll offset from the widget's
/// own memory, which this TUI deliberately does not keep — the session list is
/// rebuilt from the daemon on every refresh, and a stale widget offset would
/// then point into rows that no longer exist. Computing the window here makes
/// scrolling a pure function of four numbers, so "the selected row is always on
/// screen" is a unit test rather than something to eyeball. Threading
/// `previous` through is what keeps the list STILL when the selection moves
/// within the rows already drawn; anchoring the window to the selection instead
/// would scroll the whole table on every arrow key.
/// What: returns `(start, len)`. `len` is `min(height, total)`; `start` begins
/// at `previous`, is clamped to `total - len`, and then moves the minimum
/// distance that brings `selected` back inside. A `height` of `0` returns
/// `(0, 0)` — a viewport with no rows shows no rows, rather than panicking on
/// the subtraction.
/// Test: `window_keeps_selection_visible_scrolling_down`,
/// `window_holds_still_while_the_selection_stays_visible`,
/// `window_clamps_to_the_end_of_the_list`, `window_zero_height_is_empty`.
pub(crate) fn visible_window(
    total: usize,
    selected: usize,
    height: usize,
    previous: usize,
) -> (usize, usize) {
    let len = height.min(total);
    if len == 0 {
        return (0, 0);
    }
    let mut start = previous.min(total - len);
    if selected < start {
        start = selected;
    } else if selected >= start + len {
        start = selected + 1 - len;
    }
    (start, len)
}
