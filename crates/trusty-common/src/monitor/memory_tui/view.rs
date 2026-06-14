//! View builders and navigation helpers for the memory TUI.
//!
//! Why: content derivation (palace list rows, stats lines, title line,
//! drawer panel content, detail modal body) and list navigation are pure
//! functions of the state snapshot. Separating them from the ratatui render
//! calls keeps content testable without a terminal backend.
//! What: `palace_lines`, `stats_lines`, `title_line`, `drawer_panel_lines`,
//! `drawer_detail_body`, `format_drawer_row`, `navigate_*`, and related
//! helpers.
//! Test: `cargo test -p trusty-common --features monitor-tui` covers these
//! via the tests module in `mod.rs`.

use crate::monitor::dashboard::{PalaceRow, format_count};
use crate::monitor::memory_client::DrawerInfo;
use crate::monitor::memory_tui::activity::{PalaceActivity, activity_label, palace_activity_state};
use crate::monitor::memory_tui::state::MemoryTuiState;
use crate::monitor::tui_common::{self, ThreeWaySortKey, truncate};
use crate::monitor::utils::DaemonStatus;

/// Label for the synthetic "All palaces" entry at the top of the list.
///
/// Why: selecting it fans recalls / stats out across every palace; a single
/// constant keeps the label consistent between the list and the panel titles.
/// What: the display text of the palace list's first row.
/// Test: `test_palace_lines` asserts this is the first row.
pub const ALL_LABEL: &str = "All palaces";

/// Domain-specific labels for the memory TUI's three sort orders.
///
/// Why: the renderer surfaces the current sort key in the panel title; this
/// array maps the shared [`ThreeWaySortKey`] variants to memory-domain text
/// (the third variant reads as "Vectors" here, "Chunks" in search).
/// What: `["Activity", "Name", "Vectors"]`.
/// Test: covered indirectly by `test_palace_sort_key_cycle` via [`sort_label`].
const SORT_LABELS: &[&str; 3] = &["Activity", "Name", "Vectors"];

/// Memory-domain label for the current sort key.
///
/// Why: the renderer needs `"Activity"` / `"Name"` / `"Vectors"`; the shared
/// enum is domain-agnostic so we map it through [`SORT_LABELS`].
/// What: delegates to [`ThreeWaySortKey::label`] with the memory labels.
/// Test: `test_palace_sort_key_cycle`.
pub fn sort_label(key: ThreeWaySortKey) -> &'static str {
    key.label(SORT_LABELS)
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

/// Maximum characters surfaced per drawer creator label.
///
/// Why: the ACTIVITY panel is the right-hand column of the TUI; long creator
/// tags or full RFC-3339 timestamps would overflow when the terminal is
/// narrow. Truncating each field independently keeps the row alignment
/// predictable at all widths.
/// What: creator label truncated to 24 chars with the shared truncate helper.
const DRAWER_CREATOR_WIDTH: usize = 24;

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

/// Format an indented palace row for use under a group header.
///
/// Why: companion to [`palace_row_with_activity`] for the grouped layout —
/// matches the same spinner-glyph + label column structure but with the
/// one-space group indent that keeps the count column aligned.
/// What: returns `" <spinner> <name padded to 9>  <count>v"`.
/// Test: `test_palace_row_with_activity`.
pub(crate) fn palace_row_indented_with_activity(
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
                        crate::monitor::memory_tui::activity::format_relative_time(now, ts),
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

/// Crate version, surfaced in the title bar.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// One-line key hint shown along the bottom of the UI.
///
/// Why (issue #215): `Tab` cycles `List → DrawerPane → Input → List`, and the
/// drawer-pane zone adds `Enter` to open the detail pane. The hint surfaces
/// both flows so the operator doesn't have to discover the detail split-pane
/// from the help overlay.
pub const KEY_HINT: &str = "[Tab] focus  [↑↓] select  [Enter] open/recall  [d] dream  [/] filter  [s] sort  [g] group  [←→] page  [q] quit  [?] help";
