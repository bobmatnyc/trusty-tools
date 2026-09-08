//! Startup recovery: list day files, and find the seq high-water mark.
//!
//! Why: `HarnessEvent.seq` is console-assigned and must never repeat across a
//! restart (DOC-73 §4.3 — "seq is now minted once, by console"). The only
//! source of truth for "what was the last seq console handed out" is the log
//! itself, since the in-memory ring is empty on every fresh process. This
//! module is what [`super::DurableLog::open`] calls before it starts accepting
//! new events.
//! What: [`list_log_files`] enumerates day files in date order, ignoring
//! anything whose name is not a `day_file_name` (a stray file a human or
//! another tool dropped in the directory must not abort startup).
//! [`recover_next_seq`] scans files newest-first and returns the highest seq
//! found plus one, tolerating a truncated final line via
//! [`super::format::read_events`]; an empty log starts at 1. [`earliest_seq`]
//! is the first event's seq in the OLDEST retained file — the boundary
//! [`super::replay`] compares `since_seq` against to decide whether a replay
//! request predates everything retention still holds.
//! Test: `super::tests::recover_next_seq_starts_at_one_with_no_files`,
//! `super::tests::recover_next_seq_continues_from_the_newest_non_empty_file`,
//! `super::tests::recover_next_seq_tolerates_a_truncated_final_line`,
//! `super::tests::earliest_seq_reads_the_oldest_retained_file`.

use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use super::config::parse_day_file_name;
use super::error::LogError;
use super::format::read_events;

/// One day file, its date parsed out of the filename, and its full path.
pub(crate) type DayFile = (NaiveDate, PathBuf);

/// List every day file in `dir`, sorted ascending by date (oldest first).
///
/// A missing directory is not an error — it returns an empty list, so a
/// first-ever run (before [`trusty_common::uds::prepare_socket_dir`] has
/// created anything) recovers cleanly at seq 1 rather than failing.
///
/// Test: `super::tests::list_log_files_ignores_non_matching_names`,
/// `super::tests::list_log_files_sorts_ascending_by_date`.
pub(crate) async fn list_log_files(dir: &Path) -> Result<Vec<DayFile>, LogError> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(LogError::Io {
                op: "list",
                path: dir.to_path_buf(),
                source,
            });
        }
    };

    let mut files = Vec::new();
    loop {
        let entry = entries.next_entry().await.map_err(|source| LogError::Io {
            op: "list",
            path: dir.to_path_buf(),
            source,
        })?;
        let Some(entry) = entry else { break };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(date) = parse_day_file_name(name) {
            files.push((date, entry.path()));
        }
    }
    files.sort_by_key(|(date, _)| *date);
    Ok(files)
}

/// The seq a fresh [`super::DurableLog`] should assign next: one past the
/// highest seq recorded anywhere in `files`, or `1` if none is found.
///
/// Scans newest-first and stops at the first file that yields at least one
/// event — under normal operation that is always the very last file, but
/// scanning backward instead of assuming it tolerates a day boundary that
/// rolled over onto an empty file right before a crash (`super::tests::
/// recover_next_seq_continues_from_the_newest_non_empty_file` covers exactly
/// this).
///
/// Test: `super::tests::recover_next_seq_starts_at_one_with_no_files`,
/// `super::tests::recover_next_seq_continues_from_the_newest_non_empty_file`,
/// `super::tests::recover_next_seq_tolerates_a_truncated_final_line`.
pub(crate) async fn recover_next_seq(files: &[DayFile]) -> Result<u64, LogError> {
    for (_, path) in files.iter().rev() {
        let events = read_events(path).await?;
        if let Some(highest) = events.iter().map(|e| e.seq).max() {
            return Ok(highest + 1);
        }
    }
    Ok(1)
}

/// The seq of the first event in the OLDEST retained file — `None` when no
/// file holds a single readable event (a brand-new log, or every retained
/// file is empty/fully truncated).
///
/// This is the boundary [`super::replay::replay_since`] compares an incoming
/// `since_seq` against: a request older than this value asks for history
/// retention has already discarded, and must get an explicit gap marker
/// rather than silently starting from whatever remains.
///
/// Test: `super::tests::earliest_seq_reads_the_oldest_retained_file`,
/// `super::tests::earliest_seq_is_none_for_an_empty_log`.
pub(crate) async fn earliest_seq(files: &[DayFile]) -> Result<Option<u64>, LogError> {
    for (_, path) in files {
        let events = read_events(path).await?;
        if let Some(first) = events.first() {
            return Ok(Some(first.seq));
        }
    }
    Ok(None)
}
