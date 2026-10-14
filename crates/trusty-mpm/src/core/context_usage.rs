//! A session's latest context-window and plan rate-limit figures, as the
//! `statusLine` payload last reported them (#8394).
//!
//! Why: a fleet poller read `ctx %` and the plan figures by scraping the tmux
//! status line, which an 80-column pane truncates before `ctx`. No hook payload
//! carries these figures; only Claude Code's `statusLine` stdin JSON does, and
//! `tm statusline` is the one process that sees it. This module is the
//! machine-readable copy: the statusline writes it on every tick, and the
//! daemon and supervisor read it back onto [`crate::session_manager::SessionRecord`]
//! and [`crate::supervisor::FleetMetrics`].
//!
//! What: [`ContextUsage::from_statusline_json`] extracts the figures from the
//! payload, [`record_context_usage`] persists them under
//! [`KIND_CONTEXT_USAGE`](crate::core::session_record::KIND_CONTEXT_USAGE),
//! keyed by the Claude session id, and [`read_context_usage`] reads them back
//! with a [`stale`](ContextUsage::stale) mark.
//!
//! Two rules every part keeps:
//!
//! - **An absent figure stays `None`.** `used_percentage` is null before the
//!   first API call and after `/compact`; `rate_limits` is absent on non-Pro/Max
//!   accounts. A null, absent or wrongly typed value is never written as `0`.
//! - **A reading carries its age.** The statusline runs only while the pane
//!   renders, so a closed or idle pane leaves its last reading on disk. Each
//!   reading carries `updated_at`, and the reader sets `stale` once it is older
//!   than [`STALE_AFTER_SECS`].
//!
//! Test: `context_usage_tests.rs` — `extracts_every_figure_from_a_full_payload`,
//! `null_and_absent_figures_stay_none`,
//! `a_payload_with_neither_object_records_nothing`,
//! `a_reading_round_trips_through_the_store`,
//! `an_old_reading_reads_back_stale`,
//! `a_failed_write_is_an_error_not_a_silent_skip`,
//! `an_unsafe_session_id_is_skipped`,
//! `a_record_lacking_the_new_fields_still_deserializes`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::session_record::{
    KIND_CONTEXT_USAGE, read_session_record, session_record_path_in,
};

/// Seconds after which a reading is marked [`stale`](ContextUsage::stale).
///
/// Why: Claude Code runs the statusline when the conversation updates, not on a
/// timer, so an idle pane goes quiet while its figure is still right, and a
/// closed pane goes quiet for good. Five minutes covers ordinary pauses between
/// turns; past it a consumer is told the figure is old and decides for itself.
pub const STALE_AFTER_SECS: i64 = 300;

/// One plan rate-limit window (`rate_limits.five_hour` or `.seven_day`).
///
/// Why: each half of a window can be absent on its own, so both are `Option`.
/// What: `used_percentage` passes through as reported; `resets_at` is the
/// reset instant, accepted as epoch seconds or an RFC 3339 string.
/// Test: `extracts_every_figure_from_a_full_payload`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RateLimitWindow {
    /// Percentage of the window used, 0–100, as Claude Code reported it.
    #[serde(default)]
    pub used_percentage: Option<f64>,
    /// When the window resets.
    #[serde(default)]
    pub resets_at: Option<DateTime<Utc>>,
}

/// The account's plan rate-limit figures from one `statusLine` tick.
///
/// Why: the figures are account-wide, so [`crate::supervisor::FleetMetrics`]
/// carries one latest value; it needs its own `updated_at` and `stale` to say
/// how old that value is without the session it came from.
/// What: the five-hour and seven-day windows, each `None` when absent.
/// Test: `extracts_every_figure_from_a_full_payload`,
/// `an_old_reading_reads_back_stale`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanUsage {
    /// `rate_limits.five_hour`, the session window.
    #[serde(default)]
    pub five_hour: Option<RateLimitWindow>,
    /// `rate_limits.seven_day`, the weekly window.
    #[serde(default)]
    pub seven_day: Option<RateLimitWindow>,
    /// When the statusline tick that reported these figures ran.
    pub updated_at: DateTime<Utc>,
    /// `true` when `updated_at` is older than [`STALE_AFTER_SECS`] at read time.
    #[serde(default)]
    pub stale: bool,
}

/// A session's latest context-window and plan figures (#8394).
///
/// Why: the poller needs `ctx %` and the plan figures without scraping a pane.
/// What: the three `context_window` figures, the plan figures, the tick time
/// and the stale mark. Every figure is `Option` and `#[serde(default)]`, so a
/// reading written by an older `tm` still deserializes.
/// Test: `a_reading_round_trips_through_the_store`,
/// `null_and_absent_figures_stay_none`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextUsage {
    /// `context_window.total_input_tokens`.
    #[serde(default)]
    pub total_input_tokens: Option<u64>,
    /// `context_window.context_window_size`.
    #[serde(default)]
    pub context_window_size: Option<u64>,
    /// `context_window.used_percentage`, 0–100.
    #[serde(default)]
    pub used_percentage: Option<f64>,
    /// The plan figures from the same tick; `None` when `rate_limits` was absent.
    #[serde(default)]
    pub plan: Option<PlanUsage>,
    /// When the statusline tick that reported these figures ran.
    pub updated_at: DateTime<Utc>,
    /// `true` when `updated_at` is older than [`STALE_AFTER_SECS`] at read time.
    #[serde(default)]
    pub stale: bool,
}

/// Why a reading could not be persisted.
///
/// Why: the statusline must log a failed write, so the writer returns it rather
/// than discarding it as [`crate::core::session_record::record_session_value`]
/// does.
/// Test: `a_failed_write_is_an_error_not_a_silent_skip`.
#[derive(Debug, thiserror::Error)]
pub enum ContextUsageError {
    /// A filesystem step failed.
    #[error("{action} {path}: {source}")]
    Io {
        /// The step that failed.
        action: &'static str,
        /// The path it failed on.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The reading did not serialize.
    #[error("serialize context usage: {0}")]
    Serialize(#[from] serde_json::Error),
}

impl ContextUsage {
    /// Extract the figures from a parsed `statusLine` payload.
    ///
    /// Why: the bin's `StatusInput` reads `context_window` into non-optional
    /// fields, which cannot tell a reported `0` from a null; this reads the raw
    /// value so a null stays `None`.
    /// What: `None` when the payload has neither a `context_window` nor a
    /// `rate_limits` object. Otherwise each figure is taken only when it is
    /// present and of the right type; `plan` is `None` when no window carries a
    /// figure. `stale` is `false`; `updated_at` is `now`.
    /// Test: `extracts_every_figure_from_a_full_payload`,
    /// `null_and_absent_figures_stay_none`,
    /// `a_payload_with_neither_object_records_nothing`.
    pub fn from_statusline_json(payload: &Value, now: DateTime<Utc>) -> Option<Self> {
        let _ = (payload, now);
        None
    }

    /// Set [`Self::stale`] (and the plan's) from the reading's age at `now`.
    ///
    /// What: stale when `now - updated_at > STALE_AFTER_SECS`; a reading dated
    /// in the future (clock skew) is not stale.
    /// Test: `an_old_reading_reads_back_stale`.
    pub fn mark_staleness(&mut self, now: DateTime<Utc>) {
        let _ = now;
    }
}

/// Persist `usage` as the latest reading for `session_id`.
///
/// Why: the statusline's tick is the only writer; it must learn of a failure
/// so it can log it.
/// What: `Ok(false)` without writing when `session_id` is not a safe file name
/// (empty included). Otherwise writes `<root>/usage/session-context-usage/<id>`
/// through a temp file renamed into place, so a reader never sees a torn file,
/// and returns `Ok(true)`.
/// Test: `a_reading_round_trips_through_the_store`,
/// `a_failed_write_is_an_error_not_a_silent_skip`,
/// `an_unsafe_session_id_is_skipped`.
pub fn record_context_usage(
    root: &Path,
    session_id: &str,
    usage: &ContextUsage,
) -> Result<bool, ContextUsageError> {
    let _ = (root, session_id, usage, session_record_path_in);
    let _ = std::io::sink().write_all(b"");
    Ok(false)
}

/// Read the latest reading for `session_id`, with its stale mark set at `now`.
///
/// What: `None` when no reading exists, the id is unsafe, or the file does not
/// parse. Fails soft: a reader never errors on this store.
/// Test: `a_reading_round_trips_through_the_store`,
/// `an_old_reading_reads_back_stale`.
pub fn read_context_usage(
    root: &Path,
    session_id: &str,
    now: DateTime<Utc>,
) -> Option<ContextUsage> {
    let _ = (root, session_id, now, read_session_record);
    None
}

#[cfg(test)]
#[path = "context_usage_tests.rs"]
mod tests;
