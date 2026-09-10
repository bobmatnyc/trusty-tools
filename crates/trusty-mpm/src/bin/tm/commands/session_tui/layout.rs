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
//! [`wrap_message`] is the same idea for the status line: how many rows a
//! message needs at a given width, and what goes on each of them.
//!
//! Column vocabulary matches the console's (#7224): friendly name, short id,
//! project, state, tmux pane.
//!
//! Test: `layout_*`, `window_*` and `wrap_*` in `super::tests`.

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

/// How many rows the status line may grow to before its tail is elided (#7224).
///
/// Four is what the daemon's longest refusal — the delete guard's, which ends
/// with a full UUID on its own line — needs at 80 columns.
pub(crate) const STATUS_MAX_LINES: usize = 4;

/// Wrap a status message to `width`, on whitespace, never mid-token (#7224).
///
/// Why: the status line was a one-row unwrapped `Paragraph`, so ratatui clipped
/// whatever did not fit — and what did not fit was the tail of the delete
/// guard's refusal, cutting a session UUID in half. A half-UUID is worse than
/// no UUID: it looks copy-pasteable and is not. Wrapping here rather than
/// through ratatui's own `Wrap` is what lets the caller size the status region
/// from the SAME lines it will draw, so the height and the content cannot
/// disagree. Packing greedily from the front had the same end result at a
/// narrow width by a different route: below about 70 columns the refusal's
/// first paragraph filled all four rows on its own and the id paragraph behind
/// it was truncated away whole.
/// What: splits on `'\n'` first (the refusal puts the id on its own line) and
/// packs each paragraph's whitespace-separated tokens greedily into lines of at
/// most `width` characters. A token longer than `width` is emitted alone and
/// intact — at that width no wrapping could show it whole, and splitting it is
/// the exact failure this exists to prevent. The LAST paragraph's rows are then
/// reserved before anything ahead of them is kept, so the identifier a message
/// ends with survives every width; the paragraphs in front of it fill what
/// budget remains and the last of them is elided on a token boundary. A single
/// paragraph has nothing to reserve behind it and is simply capped at
/// `max_lines` with its last line elided. A `width` or `max_lines` of zero
/// yields no lines.
/// Test: `wrap_message_keeps_a_uuid_whole_at_eighty_columns`,
/// `wrap_message_keeps_the_session_id_at_narrow_widths`,
/// `wrap_message_keeps_an_overlong_token_whole`,
/// `wrap_message_honours_explicit_newlines`,
/// `wrap_message_elides_on_a_token_boundary`,
/// `wrap_message_zero_width_is_empty`.
pub(crate) fn wrap_message(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let mut paragraphs: Vec<Vec<String>> = text
        .split('\n')
        .map(|paragraph| wrap_paragraph(paragraph, width))
        .collect();
    // A message ending in a newline (or made only of whitespace) would otherwise
    // reserve a blank row under the table. An empty line is only ever a whole
    // paragraph, so dropping trailing empty paragraphs is the same trim.
    while paragraphs
        .last()
        .is_some_and(|p| p.len() == 1 && p[0].is_empty())
    {
        paragraphs.pop();
    }
    let Some(mut tail) = paragraphs.pop() else {
        return Vec::new();
    };
    if paragraphs.is_empty() {
        if tail.len() > max_lines {
            tail.truncate(max_lines);
            if let Some(last) = tail.last_mut() {
                elide(last, width);
            }
        }
        return tail;
    }
    // #7224: the last paragraph is where the caller put the identifier, so its
    // rows are reserved first. A tail longer than the whole budget keeps its
    // FINAL rows — the end of the identifier is what must survive.
    if tail.len() > max_lines {
        let drop = tail.len() - max_lines;
        tail = tail.split_off(drop);
    }
    let mut lines: Vec<String> = paragraphs.into_iter().flatten().collect();
    let head_budget = max_lines - tail.len();
    if lines.len() > head_budget {
        lines.truncate(head_budget);
        if let Some(last) = lines.last_mut() {
            elide(last, width);
        }
    }
    lines.extend(tail);
    lines
}

/// Pack one newline-free paragraph into `width`-wide lines, never mid-token.
///
/// Always returns at least one line, empty when the paragraph holds no tokens —
/// which is what lets [`wrap_message`] recognise and trim a trailing blank.
fn wrap_paragraph(paragraph: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for token in paragraph.split_whitespace() {
        let fits = current.chars().count() + 1 + token.chars().count() <= width;
        if current.is_empty() {
            current.push_str(token);
        } else if fits {
            current.push(' ');
            current.push_str(token);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(token);
        }
    }
    lines.push(current);
    lines
}

/// Trim `line` back to a whole-token boundary and mark it elided.
///
/// Drops trailing tokens until the ellipsis fits within `width`. A line with no
/// whitespace to cut at is left exactly as it is rather than gaining a mark that
/// would push it further over.
fn elide(line: &mut String, width: usize) {
    while line.chars().count() + 1 > width {
        let Some(cut) = line.rfind(' ') else { return };
        line.truncate(cut);
    }
    line.push('…');
}
