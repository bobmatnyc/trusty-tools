//! Palace activity classification, spinner, and colour helpers.
//!
//! Why: operators want to see at a glance which memory palaces are doing
//! something (indexing, dreaming, recently active) vs. idle. Centralising the
//! derivation, the spinner-frame lookup, and the colour mapping keeps the left
//! pane and the detail panel agreeing on what each palace is doing while
//! staying pure and unit-testable.
//! What: [`palace_activity`] bins a [`CollectionRow`]'s write recency into a
//! [`PalaceActivity`]; [`spinner_frame`] and [`activity_color`] map that state
//! to a glyph and colour; [`current_spinner_tick`] derives a wall-clock tick so
//! the renderer can animate without threading state.
//! Test: `palace_activity_from_recent_write`, `spinner_frame_for_each_state`,
//! `activity_colour_is_distinct_per_state`.

use ratatui::style::Color;

use crate::tui::health::types::{CollectionRow, PalaceActivity};

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
pub(crate) const INDEXING_SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Frames of the dreaming/compacting spinner.
///
/// Why: a heavier glyph set distinguishes the compaction state from
/// indexing at a glance.
/// What: eight frames cycled at ~10 Hz by the render loop.
/// Test: `spinner_frame_cycles_through_dreaming_frames`.
pub(crate) const DREAMING_SPINNER: &[char] = &['⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'];

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
pub(crate) fn current_spinner_tick() -> usize {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_millis() / 100) as usize)
        .unwrap_or(0)
}
