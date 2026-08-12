//! The `tm ls` / `tm session ls` table renderer.
//!
//! Why: extracted from `managed.rs`, which sits within a couple of dozen SLOC
//! of the 500-SLOC production cap. Table rendering is a self-contained concern
//! — column widths, per-column color, and the two row shapes — so it splits
//! cleanly, mirroring the `session_picker.rs` → `session_picker_render.rs`
//! precedent.
//!
//! What: [`render_session_table`] resolves the color gate once and prints the
//! header plus one row per session; [`format_ls_row`] and
//! [`format_tombstone_row`] are the pure row formatters; [`format_state_column`]
//! and the two small text helpers came across unchanged.
//!
//! Color, and why padding had to change with it: `NUM`, `ID`, and `NAME` are
//! colored in DISTINCT hues. `NUM` and `NAME` must not share one — a name's
//! trailing `NN` is a per-project serial from `allocate_serial` that reuses
//! gaps, while `NUM` is a global slot, so `tm-foo-01` sitting at `NUM 1` is
//! coincidence. 57 currently listed sessions end in `-01`; one hue across both
//! columns would assert a correspondence that does not exist. Colored text also
//! breaks `{:<N}`, which counts an ANSI escape's bytes as width while the
//! terminal draws none of them — hence [`pad_visible`], which measures the
//! visible text and appends the padding outside the escape.
//!
//! Test: `format_tombstone_row_*`, `format_state_column_*`, `truncate_*`,
//! `short_timestamp_*`, and the color/alignment cases in `managed_tests.rs`.

use trusty_mpm::client::ManagedSessionSummary;

use super::session_picker_render::{StateColor, colorize};

/// Hue for the `NUM` column — the stable, global slot number (#3034).
const NUM_COLOR: StateColor = StateColor::Magenta;

/// Hue for the `ID` column — dimmed, because the full UUID is the widest thing
/// on the row and the least often read: it exists to be copy-pasted, not
/// scanned. Dimming pushes it behind `NUM`/`NAME` without hiding it, and it is
/// a third hue so no two adjacent columns share one.
const ID_COLOR: StateColor = StateColor::Dim;

/// Hue for the `NAME` column, deliberately different from [`NUM_COLOR`].
const NAME_COLOR: StateColor = StateColor::Cyan;

/// Rendered width of the `NUM` column.
const NUM_WIDTH: usize = 5;

/// Rendered width of the `ID` column — a full UUID is exactly 36 chars.
const ID_WIDTH: usize = 36;

/// Rendered width of the `NAME` column; names are truncated to match.
const NAME_WIDTH: usize = 24;

/// Rendered width of the `TASK` column; tasks are truncated to match.
const TASK_WIDTH: usize = 30;

/// Floor for the `STATE` column, preserving the pre-#4061 table shape when no
/// row carries an annotation.
const STATE_MIN_WIDTH: usize = 14;

/// Left-align `text` in a `width`-wide column, coloring only the text itself.
///
/// Why: `{:<width}` pads to the formatted value's char count. An ANSI-wrapped
/// string carries ~9 extra chars the terminal never draws, so every colored
/// column would come out that much narrow and the table would stagger. Padding
/// outside the escape keeps the visible width exact, and keeps the plain
/// (`use_color == false`) output byte-identical to the pre-color table.
/// What: colorizes `text`, then appends `width - text.chars().count()` spaces —
/// `chars().count()` being the same measure `{:<width}` uses for a `&str`. An
/// over-wide `text` gets no padding rather than a negative count (callers
/// truncate first).
/// Test: `ls_row_alignment_matches_with_and_without_color`.
fn pad_visible(text: &str, width: usize, color: StateColor, use_color: bool) -> String {
    let pad = width.saturating_sub(text.chars().count());
    format!("{}{}", colorize(text, color, use_color), " ".repeat(pad))
}

/// Report that `tm ls -a` matched nothing.
///
/// Why: an empty table under `-a` is a legitimate answer — nobody is attached
/// to anything — and printing a bare header (or nothing) leaves the operator
/// unable to tell that apart from a broken filter or an unreachable daemon.
/// The distinct wording also keeps it from reading as "you have no sessions",
/// which `render_session_table`'s "no managed sessions" would wrongly imply
/// while a dozen detached sessions are running.
/// What: prints `no attached sessions[ for <slug>]`, mirroring
/// [`render_session_table`]'s empty-list phrasing. The caller returns `Ok(())`,
/// so the exit status is 0.
/// Test: `render_no_attached_sessions_*` in `tests_behavior_d_tests.rs`.
pub(crate) fn render_no_attached_sessions(source_id: Option<&str>) {
    println!("{}", no_attached_sessions_line(source_id));
}

/// The exact line [`render_no_attached_sessions`] prints, as a pure value.
///
/// Why: splitting the string from the `println!` is what makes the message
/// assertable in a unit test without capturing stdout.
/// Test: `render_no_attached_sessions_line_unscoped`,
/// `render_no_attached_sessions_line_scoped_to_source_id`.
pub(crate) fn no_attached_sessions_line(source_id: Option<&str>) -> String {
    match source_id {
        Some(sid) => format!("no attached sessions for {sid}"),
        None => "no attached sessions".to_string(),
    }
}

/// Render the static `tm session ls` / `tm ls` table for a scoped session list.
///
/// Why: both the static list command and the `tm ls` connector's non-picker path
/// (0 sessions, or a piped/`--json`/`--all` invocation) must print the identical
/// table, so the row formatting lives in one place rather than being duplicated
/// across the two entry points.
/// What: prints a "no managed sessions[ for <slug>]" line when the list is empty;
/// otherwise prints the header + one row per session via [`format_ls_row`] (or
/// [`format_tombstone_row`] for a tombstoned slot). The color gate is resolved
/// ONCE here, at the I/O boundary, from STDOUT's TTY-ness — this table is
/// written with `println!`, so the picker's stderr gate would leak escapes into
/// `tm ls | grep`.
/// Test: `ls_source_id_filter_selects_correct_slug` covers the scoping seam; the
/// table bytes are exercised by `tests/session_manager_mvp.rs`; the row
/// formatters carry their own unit tests.
pub(crate) fn render_session_table(sessions: &[ManagedSessionSummary], source_id: Option<&str>) {
    use std::io::IsTerminal as _;
    if sessions.is_empty() {
        if let Some(sid) = source_id {
            println!("no managed sessions for {sid}");
        } else {
            println!("no managed sessions");
        }
        return;
    }
    let use_color = super::session_picker_render::table_use_color(std::io::stdout().is_terminal());
    let state_width = state_column_width(sessions);
    println!(
        "{:<NUM_WIDTH$}  {:<ID_WIDTH$}  {:<state_width$}  {:<NAME_WIDTH$}  {:<TASK_WIDTH$}  CREATED",
        "NUM", "ID", "STATE", "NAME", "TASK"
    );
    for s in sessions {
        if s.deleted {
            // #3034: the slot stays reserved — never silently reused by a
            // later session — so the operator sees exactly which number is
            // now dead rather than having it vanish from the listing.
            println!("{}", format_tombstone_row(s.slot, use_color));
        } else {
            println!("{}", format_ls_row(s, use_color, state_width));
        }
    }
}

/// Width the `STATE` column needs so no row's annotation overflows it.
///
/// Why: `STATE` used to be a hardcoded `{:<14}`, but the column's rendered
/// value is the state PLUS any annotation — `attached [stale-assets]` is 23
/// chars, `stopped [assets ?]` is 18. Every row wider than 14 pushed `NAME`,
/// `TASK`, and `CREATED` right by the overflow, so a single annotated session
/// staggered the whole table. The width has to be measured across the rows
/// actually being printed, not guessed at compile time.
/// What: the longest [`row_state`] value among the non-tombstone rows, floored
/// at [`STATE_MIN_WIDTH`] so an all-plain listing keeps its historical shape.
/// Tombstone rows are excluded — they have no `STATE` cell at all.
/// Test: `state_column_width_absorbs_longest_annotation`,
/// `ls_table_columns_align_when_a_row_carries_an_annotation`.
pub(crate) fn state_column_width(sessions: &[ManagedSessionSummary]) -> usize {
    sessions
        .iter()
        .filter(|s| !s.deleted)
        .map(|s| row_state(s).chars().count())
        .max()
        .unwrap_or(0)
        .max(STATE_MIN_WIDTH)
}

/// The `STATE` cell for one live row, annotations included.
///
/// Why: the width pass and the render pass must agree exactly on the cell's
/// text, so both go through this one function rather than rebuilding it.
/// What: maps `attached` onto the base state, then delegates to
/// [`format_state_column`].
/// Test: `state_column_width_absorbs_longest_annotation`.
fn row_state(s: &ManagedSessionSummary) -> String {
    let base_state = if s.attached {
        "attached"
    } else {
        s.state.as_str()
    };
    format_state_column(
        base_state,
        s.unresumable,
        s.stale_assets,
        s.stale_assets_unchecked,
    )
}

/// Format one live `tm ls` row.
///
/// Why: extracted as a pure function so the column widths and the two colored
/// columns are unit-testable without capturing stdout — the same reason
/// [`format_state_column`] and [`format_tombstone_row`] are.
/// What: `NUM` (colored [`NUM_COLOR`]), id, state, `NAME` (truncated to 24,
/// colored [`NAME_COLOR`]), task (truncated to 30, or the `(interactive)`
/// placeholder when the session has none, #1841), short created-at, and any
/// pending decision. The STATE column appends `[dead]` (#2595) when the
/// server-computed `unresumable` flag is set — the session's workspace no
/// longer exists anywhere on disk, so a resume would only ever fail and the
/// operator's remedy is `tm session rm`. It also appends `[stale-assets]`
/// (#2444) when `stale_assets` is set — remedy `tm sessions sync-assets <id>`
/// — or `[assets ?]` (#4322) when the daemon skipped the probe for that row
/// (`stopped` sessions), so the operator reads "undetermined" rather than
/// mistaking a missing marker for "assets fresh". An ATTACHED session
/// (live-tmux reconciled by the daemon) reads `attached` rather than the raw
/// `active`, so a session the operator is connected to is distinguishable from
/// a merely-running one.
/// `state_width` comes from [`state_column_width`] over the whole listing, so a
/// row whose annotation is longer than [`STATE_MIN_WIDTH`] widens the column for
/// every row instead of shoving its own `NAME`/`TASK`/`CREATED` out of line.
/// Test: `ls_row_colors_num_and_name_in_distinct_hues`,
/// `ls_row_colors_id_column_dimmed`, `ls_row_plain_when_color_disabled`,
/// `ls_row_alignment_matches_with_and_without_color`,
/// `ls_table_columns_align_when_a_row_carries_an_annotation`.
pub(crate) fn format_ls_row(
    s: &ManagedSessionSummary,
    use_color: bool,
    state_width: usize,
) -> String {
    let task = s
        .task
        .as_deref()
        .map(|t| truncate(t, TASK_WIDTH))
        .unwrap_or_else(|| "(interactive)".to_string());
    let created = s
        .created_at
        .as_deref()
        .map(short_timestamp)
        .unwrap_or_default();
    let pending = s
        .pending_decision
        .as_deref()
        .map(|d| format!(" [pending: {d}]"))
        .unwrap_or_default();
    format!(
        "{}  {}  {:<state_width$}  {}  {:<TASK_WIDTH$}  {}{}",
        pad_visible(&s.slot.to_string(), NUM_WIDTH, NUM_COLOR, use_color),
        pad_visible(&s.id, ID_WIDTH, ID_COLOR, use_color),
        row_state(s),
        pad_visible(
            &truncate(&s.name, NAME_WIDTH),
            NAME_WIDTH,
            NAME_COLOR,
            use_color
        ),
        task,
        created,
        pending
    )
}

/// Format the `-- deleted --` tombstone row for a deleted slot (issue #3034).
///
/// Why: extracted as a pure function — mirroring [`format_state_column`] — so
/// the exact tombstone row text is unit-testable without capturing stdout.
/// What: the slot number in [`NUM_COLOR`], padded to the live row's `NUM`
/// column width so the table stays aligned, then `-- deleted --`. The
/// placeholder itself is left uncolored: it sits under `ID`, not `NAME`, and a
/// third hue would only add noise.
/// Test: `format_tombstone_row_shows_slot_and_placeholder`,
/// `ls_row_alignment_matches_with_and_without_color`.
pub(crate) fn format_tombstone_row(slot: u32, use_color: bool) -> String {
    format!(
        "{}  -- deleted --",
        pad_visible(&slot.to_string(), NUM_WIDTH, NUM_COLOR, use_color)
    )
}

/// Format the STATE column value for one `tm session ls` row (#2595, #2444).
///
/// Why: extracted as a pure function so the `[dead]`/`[stale-assets]` markers
/// are unit-testable without capturing stdout.
/// What: renders the base state (`deleted` → the `--deleted--` marker, #2012;
/// every other state verbatim), then appends `" [dead]"` when `unresumable` is
/// `true` (the session is `stopped`/`errored` with no workdir candidate left on
/// disk, so a resume is guaranteed to fail), and `" [stale-assets]"` when
/// `stale` (the summary's `stale_assets`) is `true`, or `" [assets ?]"` when
/// `unchecked` (the summary's `stale_assets_unchecked`) is `true` (#4322: the
/// daemon did not probe this row — the list path skips `stopped` sessions — so
/// staleness is UNDETERMINED, not known-fresh). The two asset markers are
/// mutually exclusive — an unprobed row has no verdict to report — so a stale
/// verdict always wins if both flags somehow arrive set. Markers can otherwise
/// appear together.
/// Test: `format_state_column_appends_dead_marker`,
/// `format_state_column_appends_stale_assets_marker`,
/// `format_state_column_appends_unchecked_assets_marker`,
/// `format_state_column_leaves_healthy_state_unchanged`,
/// `format_state_column_renders_deleted_marker`.
pub(crate) fn format_state_column(
    state: &str,
    unresumable: bool,
    stale: bool,
    unchecked: bool,
) -> String {
    // A `deleted` record (soft-deleted via `tm sessions delete`) renders as the
    // `--deleted--` marker so the master list REFLECTS the deletion instead of
    // the row silently vanishing (#2012 marker).
    let mut out = if state == "deleted" {
        "--deleted--".to_string()
    } else {
        state.to_string()
    };
    if unresumable {
        out.push_str(" [dead]");
    }
    if stale {
        out.push_str(" [stale-assets]");
    } else if unchecked {
        out.push_str(" [assets ?]");
    }
    out
}

/// Truncate a string to at most `max` characters, appending `…` when cut.
///
/// Why: task descriptions can be arbitrarily long; the ls table needs a bounded
/// column width.
/// What: returns `&s[..max-1]…` when `s.chars().count() > max`, else `s`.
/// Test: `truncate_clips_and_appends_ellipsis`.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .nth(max.saturating_sub(1))
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        format!("{}\u{2026}", &s[..end])
    }
}

/// Render an RFC-3339 timestamp as a short local-ish string (`YYYY-MM-DD HH:MM`).
///
/// Why: the full RFC-3339 value is too wide for a table column; a date+time
/// prefix is human-readable without truncating important information.
/// What: takes the first 16 characters of the timestamp string (`YYYY-MM-DDTHH:MM`),
/// replaces the `T` separator with a space, and returns the result. Falls back to
/// the raw string if it is shorter than 16 chars.
/// Test: `short_timestamp_formats_correctly`.
pub(crate) fn short_timestamp(s: &str) -> String {
    if s.len() >= 16 {
        s[..16].replace('T', " ")
    } else {
        s.to_string()
    }
}
