//! Append-only per-session snapshot log (`sessions-log.jsonl`).
//!
//! Why: the old resume model relied on a single global `LATEST-SESSION.txt`
//! pointer, which concurrent `tm` sessions in the same project clobber — each
//! pause overwrote the pointer, so a resume could target another session's
//! snapshot. An append-only log records one line per pause/resume event, so
//! concurrent sessions never overwrite each other's resume target and the
//! latest snapshot for a *specific* session id is always recoverable.
//! What: [`SessionLogEntry`] models one JSONL line; [`read_log`] parses the
//! file (fail-open, skipping malformed lines); [`latest_snapshot_for_session`]
//! and [`latest_snapshot_overall`] resolve the newest pause snapshot;
//! [`resolve_latest_snapshot`] implements the full fallback chain
//! (log → legacy `LATEST-SESSION.txt` pointer → mtime scan); [`append_entry`]
//! writes a new line without truncating.
//! Test: inline `#[cfg(test)]` module (`read_log_*`, `latest_*`, `resolve_*`,
//! `append_*`).
//!
// CUTOVER BRIDGE note: the legacy-pointer fallback exists for back-compat with
// snapshots written before the log was introduced; it can be dropped once no
// project on disk still carries a `LATEST-SESSION.txt`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Filename of the append-only session log inside a `sessions/` directory.
pub const SESSION_LOG_FILENAME: &str = "sessions-log.jsonl";

/// Filename of the legacy global pointer, kept only as a read-side fallback.
pub const LEGACY_POINTER_FILENAME: &str = "LATEST-SESSION.txt";

/// The `event` value recorded when a session is paused (a new snapshot).
pub const EVENT_PAUSE: &str = "pause";

/// The `event` value recorded when a session is resumed.
pub const EVENT_RESUME: &str = "resume";

/// One line in `sessions-log.jsonl`.
///
/// Why: a structured, append-only record lets resume resolve the latest
/// snapshot for a specific session id — impossible with a single overwritten
/// pointer once two sessions run concurrently.
/// What: mirrors the JSON object the pause/resume skills append:
/// `{"session_id","event","snapshot","timestamp"}`. Unknown fields are ignored
/// on read (serde default), so the schema can grow without breaking old logs.
/// Test: `read_log_parses_valid_lines`, `read_log_skips_malformed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLogEntry {
    /// The originating session's id (e.g. a UUID or tmux session name).
    pub session_id: String,
    /// The lifecycle event: `"pause"` or `"resume"`.
    pub event: String,
    /// The snapshot filename this event refers to (e.g. `session-<ts>.md`).
    pub snapshot: String,
    /// ISO-8601 timestamp of the event.
    pub timestamp: String,
}

/// Parse every well-formed entry from `<sessions_dir>/sessions-log.jsonl`.
///
/// Why: callers need the raw event stream to resolve snapshots; parsing must be
/// fail-open so a single corrupt line (partial write, manual edit) never blocks
/// resume of the remaining valid history.
/// What: reads the log file if present and returns entries in file order
/// (oldest-first). Blank lines and lines that fail to deserialize are skipped;
/// a missing file yields an empty vec (not an error).
/// Test: `read_log_parses_valid_lines`, `read_log_skips_malformed`,
/// `read_log_absent_is_empty`.
pub fn read_log(sessions_dir: &Path) -> Vec<SessionLogEntry> {
    let log_path = sessions_dir.join(SESSION_LOG_FILENAME);
    let Ok(content) = std::fs::read_to_string(&log_path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<SessionLogEntry>(l).ok())
        .collect()
}

/// Return the newest `pause` snapshot recorded for `session_id`, if any.
///
/// Why: on resume, a session should reload *its own* latest snapshot rather than
/// whichever session happened to pause last — this is the concurrency fix.
/// What: scans the log newest-first and returns the `snapshot` of the last
/// `pause` entry whose `session_id` matches. Returns `None` when the session has
/// no pause entry.
/// Test: `latest_snapshot_for_session_picks_own`.
pub fn latest_snapshot_for_session(sessions_dir: &Path, session_id: &str) -> Option<String> {
    read_log(sessions_dir)
        .into_iter()
        .rev()
        .find(|e| e.event == EVENT_PAUSE && e.session_id == session_id)
        .map(|e| e.snapshot)
}

/// Return the newest `pause` snapshot across all sessions, if any.
///
/// Why: when no current session id is known (a cold catch-up), "latest overall"
/// is the best resume target — the last pause line in the log.
/// What: scans the log newest-first and returns the `snapshot` of the last
/// `pause` entry regardless of session id.
/// Test: `latest_snapshot_overall_picks_last_pause`.
pub fn latest_snapshot_overall(sessions_dir: &Path) -> Option<String> {
    read_log(sessions_dir)
        .into_iter()
        .rev()
        .find(|e| e.event == EVENT_PAUSE)
        .map(|e| e.snapshot)
}

/// Resolve the latest snapshot file, applying the full back-compat fallback.
///
/// Why: a project may carry snapshots from before the log existed; resume must
/// still find them without a log. This centralises the ordered fallback chain so
/// every reader behaves identically.
/// What: tries, in order — (1) the newest `pause` snapshot in
/// `sessions-log.jsonl`; (2) the legacy `LATEST-SESSION.txt` pointer (first line
/// or token ending in `.<ext>`); (3) an mtime scan for the newest
/// `session-*.<ext>` file. Returns the resolved path only when it exists on
/// disk; `None` when nothing resolves. `ext` is the snapshot extension without
/// the leading dot (e.g. `"md"` for the native store, `"json"` for the legacy
/// claude-mpm store).
/// Test: `resolve_prefers_log`, `resolve_falls_back_to_pointer`,
/// `resolve_falls_back_to_mtime`, `resolve_none_when_empty`.
pub fn resolve_latest_snapshot(sessions_dir: &Path, ext: &str) -> Option<PathBuf> {
    if !sessions_dir.is_dir() {
        return None;
    }

    // (1) Log — the authoritative, concurrency-safe source.
    if let Some(name) = latest_snapshot_overall(sessions_dir) {
        let path = sessions_dir.join(&name);
        if path.exists() {
            return Some(path);
        }
    }

    // (2) Legacy global pointer.
    if let Some(name) = read_legacy_pointer(sessions_dir, ext) {
        let path = sessions_dir.join(&name);
        if path.exists() {
            return Some(path);
        }
    }

    // (3) mtime scan of session-*.<ext>.
    newest_session_file(sessions_dir, ext)
}

/// Append a single entry to `<sessions_dir>/sessions-log.jsonl`.
///
/// Why: pause must never overwrite prior history; appending preserves every
/// session's snapshots so a concurrent session's resume target survives.
/// What: creates `sessions_dir` if needed, opens the log in append mode, and
/// writes the serialized entry followed by a newline. Used by tooling and tests;
/// the pause skill appends the same shape via the shell.
/// Test: `append_then_read_roundtrips`, `append_is_additive`.
pub fn append_entry(sessions_dir: &Path, entry: &SessionLogEntry) -> std::io::Result<()> {
    std::fs::create_dir_all(sessions_dir)?;
    let line = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(sessions_dir.join(SESSION_LOG_FILENAME))?;
    writeln!(f, "{line}")
}

/// Read the legacy `LATEST-SESSION.txt` pointer, extracting a `session-*.<ext>`
/// filename.
///
/// Why: older snapshots recorded the latest file only via this pointer; keep
/// reading it so pre-log projects still resume.
/// What: returns the first trimmed line that ends in `.<ext>` (handles both a
/// bare-filename pointer and a multi-line human-readable one). `None` when the
/// pointer is absent or carries no matching token.
/// Test: covered by `resolve_falls_back_to_pointer`.
fn read_legacy_pointer(sessions_dir: &Path, ext: &str) -> Option<String> {
    let content = std::fs::read_to_string(sessions_dir.join(LEGACY_POINTER_FILENAME)).ok()?;
    let suffix = format!(".{ext}");
    content
        .lines()
        .map(str::trim)
        .find(|l| l.ends_with(&suffix))
        .map(str::to_owned)
}

/// Return the `session-*.<ext>` file with the newest mtime, if any.
///
/// Why: the last-resort fallback when neither a log nor a pointer exists.
/// What: scans `sessions_dir` for `session-*.<ext>` entries and returns the one
/// with the greatest `modified()` time. `None` when none exist.
/// Test: covered by `resolve_falls_back_to_mtime`.
fn newest_session_file(sessions_dir: &Path, ext: &str) -> Option<PathBuf> {
    let suffix = format!(".{ext}");
    std::fs::read_dir(sessions_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().into_string().unwrap_or_default();
            name.starts_with("session-") && name.ends_with(&suffix)
        })
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn entry(session_id: &str, event: &str, snapshot: &str, ts: &str) -> SessionLogEntry {
        SessionLogEntry {
            session_id: session_id.to_string(),
            event: event.to_string(),
            snapshot: snapshot.to_string(),
            timestamp: ts.to_string(),
        }
    }

    #[test]
    fn read_log_absent_is_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(read_log(tmp.path()).is_empty());
    }

    #[test]
    fn read_log_parses_valid_lines() {
        let tmp = TempDir::new().unwrap();
        let log = tmp.path().join(SESSION_LOG_FILENAME);
        fs::write(
            &log,
            "{\"session_id\":\"a\",\"event\":\"pause\",\"snapshot\":\"session-1.md\",\"timestamp\":\"2026-07-15T10:00:00Z\"}\n\
             {\"session_id\":\"a\",\"event\":\"resume\",\"snapshot\":\"session-1.md\",\"timestamp\":\"2026-07-15T11:00:00Z\"}\n",
        )
        .unwrap();
        let entries = read_log(tmp.path());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].event, "pause");
        assert_eq!(entries[1].event, "resume");
    }

    #[test]
    fn read_log_skips_malformed() {
        let tmp = TempDir::new().unwrap();
        let log = tmp.path().join(SESSION_LOG_FILENAME);
        // One valid line, one truncated/garbage line, one blank line.
        fs::write(
            &log,
            "{\"session_id\":\"a\",\"event\":\"pause\",\"snapshot\":\"session-1.md\",\"timestamp\":\"t\"}\n\
             {\"session_id\":\n\
             \n",
        )
        .unwrap();
        let entries = read_log(tmp.path());
        assert_eq!(entries.len(), 1, "malformed + blank lines skipped");
        assert_eq!(entries[0].snapshot, "session-1.md");
    }

    #[test]
    fn latest_snapshot_for_session_picks_own() {
        let tmp = TempDir::new().unwrap();
        // Two concurrent sessions interleave pauses; each must recover its own.
        append_entry(tmp.path(), &entry("s1", "pause", "session-A.md", "t1")).unwrap();
        append_entry(tmp.path(), &entry("s2", "pause", "session-B.md", "t2")).unwrap();
        append_entry(tmp.path(), &entry("s1", "pause", "session-C.md", "t3")).unwrap();

        assert_eq!(
            latest_snapshot_for_session(tmp.path(), "s1").as_deref(),
            Some("session-C.md"),
            "s1 gets its own newest pause, not s2's"
        );
        assert_eq!(
            latest_snapshot_for_session(tmp.path(), "s2").as_deref(),
            Some("session-B.md")
        );
        assert!(latest_snapshot_for_session(tmp.path(), "missing").is_none());
    }

    #[test]
    fn latest_snapshot_overall_picks_last_pause() {
        let tmp = TempDir::new().unwrap();
        append_entry(tmp.path(), &entry("s1", "pause", "session-A.md", "t1")).unwrap();
        append_entry(tmp.path(), &entry("s2", "pause", "session-B.md", "t2")).unwrap();
        // A resume event must NOT be treated as the latest snapshot.
        append_entry(tmp.path(), &entry("s1", "resume", "session-A.md", "t3")).unwrap();
        assert_eq!(
            latest_snapshot_overall(tmp.path()).as_deref(),
            Some("session-B.md")
        );
    }

    #[test]
    fn append_then_read_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let e = entry("s1", "pause", "session-1.md", "2026-07-15T10:00:00Z");
        append_entry(tmp.path(), &e).unwrap();
        let entries = read_log(tmp.path());
        assert_eq!(entries, vec![e]);
    }

    #[test]
    fn append_is_additive() {
        let tmp = TempDir::new().unwrap();
        append_entry(tmp.path(), &entry("s1", "pause", "session-1.md", "t1")).unwrap();
        append_entry(tmp.path(), &entry("s1", "pause", "session-2.md", "t2")).unwrap();
        assert_eq!(
            read_log(tmp.path()).len(),
            2,
            "second append does not truncate"
        );
    }

    #[test]
    fn resolve_prefers_log() {
        let tmp = TempDir::new().unwrap();
        // Real snapshot file the log points at.
        fs::write(tmp.path().join("session-log.md"), b"snap").unwrap();
        append_entry(tmp.path(), &entry("s1", "pause", "session-log.md", "t1")).unwrap();
        // A decoy pointer + a newer file that the log should override.
        fs::write(tmp.path().join(LEGACY_POINTER_FILENAME), "session-ptr.md").unwrap();
        fs::write(tmp.path().join("session-ptr.md"), b"decoy").unwrap();

        let got = resolve_latest_snapshot(tmp.path(), "md").unwrap();
        assert_eq!(got.file_name().unwrap(), "session-log.md");
    }

    #[test]
    fn resolve_falls_back_to_pointer() {
        let tmp = TempDir::new().unwrap();
        // No log; a multi-line human-readable pointer + the file it names.
        fs::write(tmp.path().join("session-ptr.md"), b"snap").unwrap();
        fs::write(
            tmp.path().join(LEGACY_POINTER_FILENAME),
            "Resume with this file:\nsession-ptr.md\n",
        )
        .unwrap();
        let got = resolve_latest_snapshot(tmp.path(), "md").unwrap();
        assert_eq!(got.file_name().unwrap(), "session-ptr.md");
    }

    #[test]
    fn resolve_falls_back_to_mtime() {
        let tmp = TempDir::new().unwrap();
        // Neither log nor pointer: newest session-*.md by mtime wins.
        fs::write(tmp.path().join("session-old.md"), b"old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(tmp.path().join("session-new.md"), b"new").unwrap();
        let got = resolve_latest_snapshot(tmp.path(), "md").unwrap();
        assert_eq!(got.file_name().unwrap(), "session-new.md");
    }

    #[test]
    fn resolve_none_when_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(resolve_latest_snapshot(tmp.path(), "md").is_none());
        // Log points at a missing file → must not resolve to a phantom path.
        append_entry(tmp.path(), &entry("s1", "pause", "gone.md", "t1")).unwrap();
        assert!(resolve_latest_snapshot(tmp.path(), "md").is_none());
    }

    #[test]
    fn resolve_supports_json_ext() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("session-1.json"), b"{}").unwrap();
        fs::write(tmp.path().join(LEGACY_POINTER_FILENAME), "session-1.json").unwrap();
        let got = resolve_latest_snapshot(tmp.path(), "json").unwrap();
        assert_eq!(got.file_name().unwrap(), "session-1.json");
    }
}
