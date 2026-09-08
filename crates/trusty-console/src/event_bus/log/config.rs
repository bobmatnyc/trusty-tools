//! Where the durable event log lives, and how long it is kept.
//!
//! Why: the directory and retention window are the two facts every other
//! `log/` submodule needs and none of them should decide on its own — keeping
//! both here is what lets [`super::writer`], [`super::recovery`] and
//! [`super::replay`] agree without importing each other.
//! What: [`LogConfig`] is the console's private event-log directory
//! (`<data dir>/trusty-console/event_log/`, resolved through
//! [`trusty_common::resolve_data_dir`], never a raw `create_dir_all`) plus
//! `retain_days`. [`day_file_name`]/[`parse_day_file_name`] are the one
//! filename format every submodule reads and writes
//! (`YYYY-MM-DD.ndjson`), so a rename in one place can't drift from a parse
//! in another.
//! Test: `super::tests::day_file_name_round_trips`,
//! `super::tests::parse_day_file_name_rejects_a_non_matching_name`.

use std::path::PathBuf;

use chrono::NaiveDate;

use super::error::LogError;

/// Subdirectory of the console data directory the durable log lives under.
pub(crate) const LOG_SUBDIR: &str = "event_log";

/// Extension every day file carries.
pub(crate) const LOG_FILE_EXT: &str = "ndjson";

/// How many calendar days of day files are kept by default (DOC-73 §4.3's
/// "rotated and retained" requirement; no fixed number is specified there, so
/// a week is the simplest sane default — long enough that a viewer reattaching
/// after a weekend still gets replay, short enough that an idle console does
/// not accumulate files forever).
pub(crate) const DEFAULT_RETAIN_DAYS: u32 = 7;

/// Bounded capacity of the channel between [`super::EventBus::ingest`] and the
/// writer task. Sized well above any burst this workspace's harnesses are
/// expected to produce (DOC-73 §4.3's ring defaults to 8192 for the same
/// reason) while still being a real, enforced bound — see
/// `super::tests::backpressure_drops_are_counted_and_do_not_block_ingest` for
/// what happens once it fills.
pub(crate) const WRITE_CHANNEL_CAPACITY: usize = 4096;

/// Construction-time configuration for [`super::DurableLog`].
#[derive(Debug, Clone)]
pub(crate) struct LogConfig {
    /// Directory the day files live in.
    pub dir: PathBuf,
    /// Calendar days of day files retained before rotation deletes the
    /// oldest.
    pub retain_days: u32,
}

impl LogConfig {
    /// The default configuration: `<console data dir>/event_log/`,
    /// [`DEFAULT_RETAIN_DAYS`] days retained.
    ///
    /// # Errors
    ///
    /// [`LogError::ResolveDir`] when the console data directory itself
    /// cannot be resolved (a degenerate `HOME`/`XDG_DATA_HOME`, unwritable
    /// filesystem, etc. — see [`trusty_common::resolve_data_dir`]).
    pub(crate) fn resolve_default() -> Result<Self, LogError> {
        let base =
            trusty_common::resolve_data_dir("trusty-console").map_err(LogError::ResolveDir)?;
        Ok(Self {
            dir: base.join(LOG_SUBDIR),
            retain_days: DEFAULT_RETAIN_DAYS,
        })
    }
}

/// The on-disk filename for `date`'s day file: `YYYY-MM-DD.ndjson`.
///
/// Test: `super::tests::day_file_name_round_trips`.
pub(crate) fn day_file_name(date: NaiveDate) -> String {
    format!("{}.{LOG_FILE_EXT}", date.format("%Y-%m-%d"))
}

/// Parse a day file's date back out of its filename, `None` for anything that
/// does not match [`day_file_name`]'s exact format — including another file a
/// human or another tool dropped in the same directory, which must be ignored
/// rather than crash the listing.
///
/// Test: `super::tests::day_file_name_round_trips`,
/// `super::tests::parse_day_file_name_rejects_a_non_matching_name`.
pub(crate) fn parse_day_file_name(name: &str) -> Option<NaiveDate> {
    let stem = name.strip_suffix(&format!(".{LOG_FILE_EXT}"))?;
    NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()
}
