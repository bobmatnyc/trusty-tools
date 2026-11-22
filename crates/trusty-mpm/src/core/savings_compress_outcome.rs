//! The hand-off that tells the opportunity observer whether a `tm compress`
//! run saved anything (#11560).
//!
//! Why: the `PreToolUse` rewrite decides that a Bash call goes through
//! `tm compress`; the `PostToolUse` observer writes the opportunity row after
//! the call returns, and before #11560 it could only replay that decision, so
//! every rewritten call read `compressed` — 173 of 256 runs on 11-21/22 saved
//! zero tokens and still counted as compressed. The filter and the observer are
//! separate processes with no shared state, and the observer's payload carries
//! the output that reached the model, not the bytes the filter read.
//! What: the filter calls [`record_outcome`] once per run, keyed by the Claude
//! session id and the exact `--tool` argument the rewrite wrote; the observer
//! calls [`take_outcome`] with the same two values and the one file is
//! consumed. Two calls of the same tool in the same session running at once
//! share a key, so the last filter to finish wins: accepted, because the row
//! still says `compressed` or `compressed_no_gain`, never a third value.
//! Test: `an_outcome_round_trips_and_is_consumed`,
//! `a_stale_or_missing_outcome_reads_as_none`,
//! `an_unwritable_root_logs_and_returns`.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Older than this, an outcome file belongs to a call whose observer never ran.
const OUTCOME_MAX_AGE: Duration = Duration::from_secs(600);

const GAIN: &str = "gain";
const NO_GAIN: &str = "no_gain";

/// The directory the outcome files live in, under the framework root.
pub fn outcome_dir_in(root: &Path) -> PathBuf {
    root.join("usage").join("compress-outcome")
}

/// One file per (session, tool) pair, named by an FNV-1a hash of both.
///
/// Why: a tool argument carries spaces, quotes and paths; a hash keeps the name
/// a fixed, filesystem-safe width and puts no path text on disk.
fn outcome_path(root: &Path, session_id: &str, tool: &str) -> PathBuf {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in session_id.bytes().chain([0]).chain(tool.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    outcome_dir_in(root).join(format!("{hash:016x}"))
}

/// Record whether this run saved tokens, for the observer to read (#11560).
///
/// What: writes `gain` or `no_gain` to the (session, tool) file through a
/// temporary file and a rename, so a reader never sees a torn value. A blank
/// session id writes nothing. FAIL-OPEN: an IO failure is a `warn!` and a
/// return, never an error, because the compressed output is already the
/// caller's return value.
/// Test: `an_outcome_round_trips_and_is_consumed`, `an_unwritable_root_logs_and_returns`.
pub fn record_outcome(root: &Path, session_id: &str, tool: &str, gained: bool) {
    if session_id.trim().is_empty() {
        return;
    }
    let path = outcome_path(root, session_id, tool);
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let value = if gained { GAIN } else { NO_GAIN };
    let written = std::fs::create_dir_all(outcome_dir_in(root))
        .and_then(|()| std::fs::write(&tmp, value))
        .and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(source) = written {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!(
            path = %path.display(),
            %source,
            "could not record the compress outcome; the opportunity row will read `compressed`"
        );
    }
}

/// Read and delete the outcome [`record_outcome`] left for this call.
///
/// What: `Some(true)` for a run that saved tokens, `Some(false)` for one that
/// did not, `None` when there is no file, the file is older than
/// [`OUTCOME_MAX_AGE`], or it holds anything else. The file is removed on
/// every read, so one outcome classifies at most one row.
/// Test: `an_outcome_round_trips_and_is_consumed`,
/// `a_stale_or_missing_outcome_reads_as_none`.
pub fn take_outcome(root: &Path, session_id: &str, tool: &str) -> Option<bool> {
    let path = outcome_path(root, session_id, tool);
    take_outcome_at(&path, OUTCOME_MAX_AGE)
}

fn take_outcome_at(path: &Path, max_age: Duration) -> Option<bool> {
    let fresh = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age <= max_age);
    let text = std::fs::read_to_string(path).ok();
    let _ = std::fs::remove_file(path);
    match (fresh, text.as_deref()) {
        (true, Some(GAIN)) => Some(true),
        (true, Some(NO_GAIN)) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outcome_round_trips_and_is_consumed() {
        let dir = tempfile::tempdir().expect("tempdir");
        record_outcome(dir.path(), "s1", "git diff", false);
        record_outcome(dir.path(), "s1", "cargo test", true);
        assert_eq!(take_outcome(dir.path(), "s1", "git diff"), Some(false));
        assert_eq!(take_outcome(dir.path(), "s1", "git diff"), None, "consumed");
        assert_eq!(
            take_outcome(dir.path(), "s2", "cargo test"),
            None,
            "keyed by session"
        );
        assert_eq!(take_outcome(dir.path(), "s1", "cargo test"), Some(true));
        record_outcome(dir.path(), " ", "git diff", true);
        assert!(
            std::fs::read_dir(outcome_dir_in(dir.path()))
                .expect("dir")
                .next()
                .is_none(),
            "a blank session writes nothing"
        );
    }

    #[test]
    fn a_stale_or_missing_outcome_reads_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(take_outcome(dir.path(), "s1", "ls"), None);
        record_outcome(dir.path(), "s1", "ls", false);
        let path = outcome_path(dir.path(), "s1", "ls");
        assert_eq!(take_outcome_at(&path, Duration::ZERO), None, "stale");
        assert!(!path.exists(), "a stale file is removed too");
    }

    #[test]
    fn an_unwritable_root_logs_and_returns() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A file where the `usage` directory should be: create_dir_all fails.
        std::fs::write(dir.path().join("usage"), "not a dir").expect("write");
        record_outcome(dir.path(), "s1", "git diff", false);
        assert_eq!(take_outcome(dir.path(), "s1", "git diff"), None);
    }
}
