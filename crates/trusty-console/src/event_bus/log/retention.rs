//! Deleting day files older than the retention window.
//!
//! Why: DOC-73 §4.3 calls for a "day-rotated NDJSON log, rotated and
//! retained" — without a bound, an idle console accumulates one file per day
//! forever. This module is the bound: applied once at [`super::DurableLog::
//! open`] (in case files piled up while console was down) and again on every
//! rotation.
//! What: [`files_to_delete`] is the pure decision (testable without a
//! filesystem): given the retained day files and today's date, which ones
//! fall outside `retain_days`. [`enforce_retention`] lists, decides, and
//! deletes, then returns the survivors sorted ascending — the caller (the
//! writer task) uses that to recompute [`super::recovery::earliest_seq`]
//! without a second directory listing.
//! Test: `super::tests::files_to_delete_keeps_exactly_retain_days`,
//! `super::tests::files_to_delete_keeps_everything_within_the_window`,
//! `super::tests::enforce_retention_deletes_only_the_expired_files`.

use std::path::Path;

use chrono::{Days, NaiveDate};

use super::error::LogError;
use super::recovery::{DayFile, list_log_files};

/// Which of `files` fall outside the most recent `retain_days` calendar days
/// counting back from `today` (inclusive of `today`).
///
/// `retain_days == 0` is treated as `1` — a retention window of zero would
/// delete every file including the one still being written, which is never
/// the intent of a caller passing zero (most likely a misconfigured value,
/// not "keep nothing").
///
/// Test: `super::tests::files_to_delete_keeps_exactly_retain_days`,
/// `super::tests::files_to_delete_keeps_everything_within_the_window`.
pub(crate) fn files_to_delete(
    files: &[DayFile],
    today: NaiveDate,
    retain_days: u32,
) -> Vec<DayFile> {
    let retain_days = retain_days.max(1);
    // `Days` only adds; going back `retain_days - 1` days from today gives the
    // oldest date still kept.
    let Some(cutoff) = today.checked_sub_days(Days::new(u64::from(retain_days - 1))) else {
        // A `NaiveDate` underflow (today near the calendar's earliest
        // representable date) is not a real deployment scenario; keep
        // everything rather than risk deleting under an unrepresentable
        // cutoff.
        return Vec::new();
    };
    files
        .iter()
        .filter(|(date, _)| *date < cutoff)
        .cloned()
        .collect()
}

/// List `dir`, delete every file [`files_to_delete`] names, and return the
/// survivors sorted ascending by date.
///
/// # Errors
///
/// [`LogError::Io`] if the directory cannot be listed, or a file that
/// [`files_to_delete`] named cannot be removed. A file already gone by the
/// time the delete runs (`NotFound`) is not an error — another retention pass
/// or an operator could have removed it first, and the end state is what this
/// function is asked to guarantee.
///
/// Test: `super::tests::enforce_retention_deletes_only_the_expired_files`.
pub(crate) async fn enforce_retention(
    dir: &Path,
    today: NaiveDate,
    retain_days: u32,
) -> Result<Vec<DayFile>, LogError> {
    let files = list_log_files(dir).await?;
    let doomed = files_to_delete(&files, today, retain_days);
    for (_, path) in &doomed {
        match tokio::fs::remove_file(path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(LogError::Io {
                    op: "delete",
                    path: path.clone(),
                    source,
                });
            }
        }
    }
    let doomed_paths: std::collections::HashSet<_> =
        doomed.into_iter().map(|(_, path)| path).collect();
    Ok(files
        .into_iter()
        .filter(|(_, path)| !doomed_paths.contains(path))
        .collect())
}
