//! The CTO tripwire's durable record, `trips.jsonl` (#8454 G3, slice S6 prep).
//!
//! Why: until the go-live alert lands, the record is the only trace that a
//! channel-tainted session reached a configured project's files. The egress
//! hook writes it and the `tripwire_record` doctor row reads it, so both use
//! one path and one line format.
//! What: [`TripLine`], one append-only JSON line per trip, holding no file
//! content. [`append`] writes it in one `write` on an `O_APPEND` file and
//! syncs it; [`parse`] reads a record back and names the first line that does
//! not parse. The file is `~/.trusty-tools/trusty-mpm/channels/tripwire/`
//! [`RECORD_FILE`], beside the taint directory and outside it, so a trip
//! never counts as a live taint.
//! Residual: the record directory is not a trust anchor; a session that may
//! write there can delete it. The hook denies every gated call of a tainted
//! session, so the tainted session itself cannot.
//! Test: `channels_tripwire_tests.rs`; the doctor row in
//! `daemon/doctor_tripwire_record_tests.rs`; end to end in
//! `tests/tm_hook_tripwire_8454.rs`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The record's file name in [`record_dir`].
pub const RECORD_FILE: &str = "trips.jsonl";

/// The line format version.
pub const FORMAT_VERSION: u32 = 1;

/// The longest path text a line keeps, in bytes; a longer one is cut.
pub const MAX_PATH_BYTES: usize = 1024;

/// One trip. It names where the call landed, never what the file holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TripLine {
    /// [`FORMAT_VERSION`].
    pub v: u32,
    /// When the trip was recorded.
    pub at: DateTime<Utc>,
    /// The payload's `session_id`; `None` when the call named none.
    pub session_id: Option<String>,
    /// The tool the call ran.
    pub tool_name: String,
    /// Why the call is sensitive: `inside`, `unplaceable`,
    /// `projects-unknown` or `panicked`.
    pub cause: String,
    /// The path the call named, at most [`MAX_PATH_BYTES`]; `None` when the
    /// cause names no path.
    pub path: Option<String>,
}

/// `~/.trusty-tools/trusty-mpm/channels/tripwire` under `home`.
pub fn record_dir(home: &Path) -> PathBuf {
    trusty_common::crate_config::crate_config_dir_at(
        home,
        crate::core::trusty_tools_config::CRATE_NAME,
    )
    .join("channels")
    .join("tripwire")
}

/// The record file under `home`.
pub fn record_path(home: &Path) -> PathBuf {
    record_dir(home).join(RECORD_FILE)
}

/// `text` cut to at most [`MAX_PATH_BYTES`] on a character boundary.
pub fn clip(text: &str) -> String {
    let mut end = text.len().min(MAX_PATH_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// Append `line` to the record in `dir`, creating both as needed.
///
/// Why: the record must survive the hook process, which exits at once after
/// its deny, and two hooks may trip at the same moment.
/// What: the directory (mode `0700` on unix) and the file (`0600`) are
/// created when missing; the line is serialized, newline included, and
/// written with one `write_all` on an append-mode file, then `sync_data`. Any
/// failure is the `Err`; the caller never lets it change a verdict.
/// Test: `tripwire_record_append_is_one_line_per_trip`.
pub fn append(dir: &Path, line: &TripLine) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    let mut text = serde_json::to_string(line).map_err(std::io::Error::other)?;
    text.push('\n');
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(dir.join(RECORD_FILE))?;
    file.write_all(text.as_bytes())?;
    file.sync_data()
}

/// The record's lines, blank lines skipped; `Err(n)` names the first line,
/// 1-based, that is not a [`TripLine`].
///
/// Test: `tripwire_record_append_is_one_line_per_trip`,
/// `tripwire_record_row_fails_on_unparseable_line_or_unwritable_dir`.
pub fn parse(text: &str) -> Result<Vec<TripLine>, usize> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(n, line)| serde_json::from_str(line).map_err(|_| n + 1))
        .collect()
}

#[cfg(test)]
#[path = "channels_tripwire_tests.rs"]
mod tests;
