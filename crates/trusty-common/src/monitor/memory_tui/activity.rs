//! Palace activity-state types, spinner glyphs, and helpers.
//!
//! Why: the activity-state logic (idle / indexing / active / dreaming / error)
//! is consumed by both the palace-list row builder and the STATISTICS panel.
//! Isolating it here keeps the rendering and the detail panel in sync and the
//! mapping documented in one place.
//! What: [`PalaceActivity`], its two spinner glyph arrays, and the helper
//! functions that derive the activity state from a [`PalaceRow`] and map it
//! to human-readable labels and wall-clock spinner ticks.
//! Test: `cargo test -p trusty-common --features monitor-tui` covers these via
//! `test_palace_activity_state` in the integration-level tests module.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::style::Color;

use crate::monitor::dashboard::PalaceRow;

/// Braille spinner glyphs used for the "Indexing" state (rotating wave).
///
/// Why: a recognisable spinner prefix gives the operator a glance-cue that a
/// palace is currently absorbing writes, without polling for an explicit state.
/// What: ten-frame braille cycle, indexed by a wall-clock tick.
/// Test: `test_palace_activity_state` (frames sampled deterministically).
pub const INDEXING_SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Braille spinner glyphs used for the "Dreaming" state (rotating block).
///
/// Why: a heavier, distinct cycle separates an in-progress dream/compaction
/// from the lighter indexing spinner at a glance.
/// What: eight-frame braille cycle.
/// Test: `test_palace_activity_state`.
pub const DREAMING_SPINNER: [char; 8] = ['⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'];

/// A palace's current activity state, surfaced as a coloured spinner prefix.
///
/// Why: operators want to see at a glance whether each palace is idle, taking
/// writes, recently active, dreaming, or unhealthy. A typed enum makes the
/// renderer's colour + glyph mapping exhaustive.
/// What: five mutually-exclusive states. The mapping from the underlying
/// `PalaceRow` data lives in [`palace_activity_state`].
/// Test: `test_palace_activity_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PalaceActivity {
    /// Nothing recent — no spinner, default style.
    Idle,
    /// `last_write_at` within the last 10 seconds — rotating indexing spinner.
    Indexing,
    /// `last_write_at` within the last 60 seconds — static `⠿` in cyan.
    Active,
    /// A dream/compaction cycle is currently running — rotating block spinner.
    Dreaming,
    /// The palace reported an unhealthy / error state — red `✗`.
    Error,
}

impl PalaceActivity {
    /// Resolve the rendered prefix glyph for this state at wall-clock `tick`.
    ///
    /// Why: spinners must cycle without an explicit app tick; the wall-clock
    /// driver lets every palace's frame advance independently of polls.
    /// What: returns a single rendered character: `' '` for Idle, the indexed
    /// frame from [`INDEXING_SPINNER`] / [`DREAMING_SPINNER`] for the rotating
    /// states, `'⠿'` for Active, and `'✗'` for Error.
    /// Test: `test_palace_activity_state`.
    pub fn prefix(self, tick: usize) -> char {
        match self {
            PalaceActivity::Idle => ' ',
            PalaceActivity::Indexing => INDEXING_SPINNER[tick % INDEXING_SPINNER.len()],
            PalaceActivity::Active => '⠿',
            PalaceActivity::Dreaming => DREAMING_SPINNER[tick % DREAMING_SPINNER.len()],
            PalaceActivity::Error => '✗',
        }
    }

    /// Resolve the foreground colour for this state.
    ///
    /// Why: colour reinforces the glyph — yellow for indexing, cyan for
    /// active, magenta for dreaming, red for error, default for idle.
    /// What: returns `None` for Idle (default terminal foreground) or
    /// `Some(Color)` for the four signalling states.
    /// Test: `test_palace_activity_state`.
    pub fn color(self) -> Option<Color> {
        match self {
            PalaceActivity::Idle => None,
            PalaceActivity::Indexing => Some(Color::Yellow),
            PalaceActivity::Active => Some(Color::Cyan),
            PalaceActivity::Dreaming => Some(Color::Magenta),
            PalaceActivity::Error => Some(Color::Red),
        }
    }
}

/// Wall-clock spinner tick, driven by the system clock at 10 Hz.
///
/// Why: spinners must animate even when no app event fires; a wall-clock tick
/// keeps every frame in motion without a separate timer.
/// What: returns `now.duration_since(UNIX_EPOCH).as_millis() / 100`, cast to
/// `usize` (saturating at zero on clock skew).
/// Test: `test_spinner_tick_returns_value` only sanity-checks the call surface;
/// downstream tests pass an explicit `tick` to keep them deterministic.
pub fn spinner_tick() -> usize {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_millis() / 100) as usize)
        .unwrap_or(0)
}

/// Derive a palace's current [`PalaceActivity`] from its wire fields.
///
/// Why: the row builder and the STATISTICS panel both need the same mapping.
/// Centralising it keeps the rendering and the detail panel in sync, and the
/// 10-second / 60-second cut-offs documented in one place.
/// What: `is_compacting → Dreaming`; otherwise the elapsed time since
/// `last_write_at` decides Indexing (< 10s), Active (< 60s), or Idle. Error
/// is reserved for a future health field on the wire — never returned today.
/// Test: `test_palace_activity_state`.
pub fn palace_activity_state(
    palace: &PalaceRow,
    now: chrono::DateTime<chrono::Utc>,
) -> PalaceActivity {
    if palace.is_compacting {
        return PalaceActivity::Dreaming;
    }
    match palace.last_write_at {
        Some(ts) => {
            let delta = now.signed_duration_since(ts);
            // Negative deltas (clock skew) are treated as fresh writes.
            let secs = delta.num_seconds();
            if secs < 10 {
                PalaceActivity::Indexing
            } else if secs < 60 {
                PalaceActivity::Active
            } else {
                PalaceActivity::Idle
            }
        }
        None => PalaceActivity::Idle,
    }
}

/// Human-readable label for a [`PalaceActivity`] state.
///
/// Why: the STATISTICS panel surfaces the current state in plain text next to
/// the spinner; sharing the mapping keeps the row prefix and the detail panel
/// label in lockstep.
/// What: returns `"Idle"`, `"Indexing"`, `"Active"`, `"Dreaming"`, or
/// `"Error"`.
/// Test: covered indirectly by `test_stats_graph_section`.
pub fn activity_label(activity: PalaceActivity) -> &'static str {
    match activity {
        PalaceActivity::Idle => "Idle",
        PalaceActivity::Indexing => "Indexing",
        PalaceActivity::Active => "Active",
        PalaceActivity::Dreaming => "Dreaming",
        PalaceActivity::Error => "Error",
    }
}

/// Render a `chrono::Duration` as a compact human-readable relative time.
///
/// Why: the detail panel's "Last write" line reads more naturally as "just
/// now" / "2m ago" / "5h ago" than as a raw timestamp; the absolute timestamp
/// is shown alongside for precision.
/// What: returns `"just now"` for < 5s; `"<n>s ago"` for < 60s;
/// `"<n>m ago"` for < 60min; `"<n>h ago"` for < 24h; `"<n>d ago"` otherwise.
/// Negative deltas are clamped to "just now".
/// Test: `test_format_relative_time`.
pub fn format_relative_time(
    now: chrono::DateTime<chrono::Utc>,
    ts: chrono::DateTime<chrono::Utc>,
) -> String {
    let secs = now.signed_duration_since(ts).num_seconds();
    if secs < 5 {
        return "just now".to_string();
    }
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    format!("{days}d ago")
}
