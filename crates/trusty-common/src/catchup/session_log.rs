//! Append-only per-session snapshot log (`sessions-log.jsonl`).
//!
//! Why: the old resume model relied on a single global `LATEST-SESSION.txt`
//! pointer, which concurrent `tm` sessions in the same project clobber — each
//! pause overwrote the pointer, so a resume could target another session's
//! snapshot. An append-only log records one line per pause/resume event, so
//! concurrent sessions never overwrite each other's resume target and the
//! latest snapshot for a *specific* session id is always recoverable.
//!
//! The log is also the store's ATTRIBUTION INDEX: a snapshot belongs to the
//! session whose `pause` line names it, wherever the file physically sits. That
//! is what lets [`resolve_session_snapshot`] serve pre-#5272 flat snapshots at
//! the store root and post-#5272 ones under `<session-id>/` through one code
//! path, and what makes an unattributable file resolve for nobody.
//! What: [`SessionLogEntry`] models one JSONL line; [`read_log`] parses the
//! file (fail-open, skipping malformed lines); [`latest_snapshot_for_session`]
//! and [`latest_snapshot_overall`] resolve the newest pause snapshot;
//! [`resolve_session_snapshot`] resolves a snapshot for ONE session id with no
//! cross-session fallback; [`resolve_latest_snapshot`] keeps the session-blind
//! fallback chain for the legacy claude-mpm JSON store only; [`append_entry`]
//! writes a new line without truncating.
//! Test: inline `#[cfg(test)]` module (`read_log_*`, `latest_*`, `resolve_*`,
//! `append_*`, `snapshot_path_in_*`, `session_dir_name_*`).
//!
// CUTOVER BRIDGE note: the legacy-pointer fallback exists for back-compat with
// snapshots written before the log was introduced; it can be dropped once no
// project on disk still carries a `LATEST-SESSION.txt`.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

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
/// `pause` entry whose `session_id` matches, as recorded — a bare
/// `session-<ts>.md` for a pre-#5272 flat snapshot, `<session-id>/session-<ts>.md`
/// for one written since. Returns `None` when the session has no pause entry.
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

/// Directory name a session's snapshots are written under, when its id is safe
/// to use as one.
///
/// Why: `session_id` reaches this crate straight from an MCP argument, so it
/// cannot be joined onto a path unchecked — `../../x` would place a snapshot
/// outside the store. A session id also must never be MANGLED into a safe name,
/// because two distinct ids collapsing onto one directory reintroduces exactly
/// the crosstalk #5272 fixes; an unsafe id gets no directory at all instead.
/// What: `Some(id)` when `session_id` is non-empty and made only of ASCII
/// alphanumerics, `-`, `_`, or `.`, and is not `.` or `..`; `None` otherwise.
/// UUID session ids — every id `tm` mints — always qualify.
/// Test: `session_dir_name_accepts_uuid`, `session_dir_name_rejects_traversal`.
pub fn session_dir_name(session_id: &str) -> Option<&str> {
    if session_id.is_empty() || session_id == "." || session_id == ".." {
        return None;
    }
    session_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .then_some(session_id)
}

/// Resolve a `snapshot` field from the log against `sessions_dir`, refusing to
/// escape it.
///
/// Why: `sessions-log.jsonl` is a plain file any process can append to, so its
/// `snapshot` value is untrusted input to a path join. Without containment,
/// `"../../../../etc/passwd"` would be read and rendered into a resume digest.
/// What: accepts only a relative path whose every component is an ordinary name
/// and whose file name is `session-*.<ext>`; returns the joined path only when
/// it is an existing file. `None` in every other case.
/// Test: `snapshot_path_in_accepts_flat_and_nested`,
/// `snapshot_path_in_rejects_escapes`.
fn snapshot_path_in(sessions_dir: &Path, snapshot: &str, ext: &str) -> Option<PathBuf> {
    let rel = Path::new(snapshot);
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    let name = rel.file_name()?.to_str()?;
    if !(name.starts_with("session-") && name.ends_with(&format!(".{ext}"))) {
        return None;
    }
    let path = sessions_dir.join(rel);
    path.is_file().then_some(path)
}

/// Resolve the newest snapshot belonging to `session_id`, and only to it.
///
/// Why: #5272 — resume must never hand a session another session's snapshot.
/// The session-blind chain in [`resolve_latest_snapshot`] was correct under
/// #2731's one-session-per-checkout model, but once several PM sessions share
/// one `.trusty-mpm/sessions/` store its "newest pause overall" step silently
/// answers "someone else's snapshot" whenever the asking session has none of
/// its own. This function has no such step: unattributable means empty.
/// What: (1) the newest `pause` line in `sessions-log.jsonl` for `session_id`,
/// resolved through [`snapshot_path_in`] — which covers a pre-#5272 flat
/// `session-<ts>.<ext>` at the store root and a post-#5272
/// `<session-id>/session-<ts>.<ext>` alike, since the log records the path
/// relative to `sessions_dir`; (2) failing that, the newest `session-*.<ext>`
/// inside `<sessions_dir>/<session_id>/`, which recovers a snapshot whose log
/// append did not land. Returns `None` when neither yields an existing file —
/// never another session's snapshot.
/// Test: `resolve_session_snapshot_refuses_another_sessions_snapshot`,
/// `resolve_session_snapshot_reads_legacy_flat_via_log`,
/// `resolve_session_snapshot_reads_per_session_dir`,
/// `resolve_session_snapshot_ignores_unattributed_flat_file`.
pub fn resolve_session_snapshot(
    sessions_dir: &Path,
    session_id: &str,
    ext: &str,
) -> Option<PathBuf> {
    if !sessions_dir.is_dir() {
        return None;
    }
    if let Some(name) = latest_snapshot_for_session(sessions_dir, session_id)
        && let Some(path) = snapshot_path_in(sessions_dir, &name, ext)
    {
        return Some(path);
    }
    let dir = sessions_dir.join(session_dir_name(session_id)?);
    newest_session_file(&dir, ext)
}

/// Resolve the latest snapshot file, applying the full session-blind fallback.
///
/// Why: a project may carry snapshots from before the log existed; resume must
/// still find them without a log. This centralises the ordered fallback chain so
/// every reader behaves identically.
///
/// 🔴 Session-blind by construction: steps (1)–(3) below can all return a
/// snapshot written by a different session. Since #5272 the native
/// `.trusty-mpm` store never calls this — use [`resolve_session_snapshot`]
/// there. It survives only for the legacy claude-mpm JSON store
/// (`.claude-mpm/sessions/`, `ext = "json"`), which predates per-session
/// attribution entirely and which one `tm` session never shares with another.
/// What: tries, in order — (1) the newest `pause` snapshot in
/// `sessions-log.jsonl`; (2) the legacy `LATEST-SESSION.txt` pointer (first line
/// or token ending in `.<ext>`); (3) an mtime scan for the newest
/// `session-*.<ext>` file. Returns the resolved path only when it exists on
/// disk; `None` when nothing resolves. `ext` is the snapshot extension without
/// the leading dot.
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

    // -----------------------------------------------------------------------
    // #5272 — session-scoped resolution
    // -----------------------------------------------------------------------

    #[test]
    fn session_dir_name_accepts_uuid() {
        let id = "7bd5c27a-475b-41df-9e9f-a6f630801717";
        assert_eq!(session_dir_name(id), Some(id));
        assert_eq!(session_dir_name("tm_sess.01"), Some("tm_sess.01"));
    }

    #[test]
    fn session_dir_name_rejects_traversal() {
        for bad in ["", ".", "..", "../evil", "a/b", "sess:1", "sess id"] {
            assert_eq!(session_dir_name(bad), None, "{bad:?} must not name a dir");
        }
    }

    #[test]
    fn snapshot_path_in_accepts_flat_and_nested() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("sid")).unwrap();
        fs::write(tmp.path().join("session-flat.md"), b"x").unwrap();
        fs::write(tmp.path().join("sid").join("session-nested.md"), b"x").unwrap();

        assert!(snapshot_path_in(tmp.path(), "session-flat.md", "md").is_some());
        assert!(snapshot_path_in(tmp.path(), "sid/session-nested.md", "md").is_some());
        // Names outside the `session-*.<ext>` shape, and files that don't exist.
        assert!(snapshot_path_in(tmp.path(), "session-flat.json", "md").is_none());
        assert!(snapshot_path_in(tmp.path(), "notes.md", "md").is_none());
        assert!(snapshot_path_in(tmp.path(), "session-gone.md", "md").is_none());
    }

    /// Why: `sessions-log.jsonl` is a plain file, so its `snapshot` value is
    /// untrusted input to a path join — an entry naming `../…` would otherwise
    /// be read from outside the store and rendered into a resume digest.
    /// What: absolute and parent-traversing values resolve to `None` even when
    /// the file they name exists.
    /// Test: itself.
    #[test]
    fn snapshot_path_in_rejects_escapes() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("sessions");
        fs::create_dir_all(&store).unwrap();
        let outside = tmp.path().join("session-outside.md");
        fs::write(&outside, b"secret").unwrap();

        assert!(snapshot_path_in(&store, "../session-outside.md", "md").is_none());
        assert!(snapshot_path_in(&store, "a/../../session-outside.md", "md").is_none());
        assert!(snapshot_path_in(&store, outside.to_str().unwrap(), "md").is_none());
    }

    /// Why: issue #5272's exact shape at the resolver level — one store, a
    /// snapshot logged to session A, session B asking. The old chain answered
    /// with A's file; this one answers `None`.
    /// What: B resolves to `None`, A to its own snapshot.
    /// Test: itself.
    #[test]
    fn resolve_session_snapshot_refuses_another_sessions_snapshot() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("session-20260809-010155.md"), b"A").unwrap();
        append_entry(
            tmp.path(),
            &entry("session-a", "pause", "session-20260809-010155.md", "t1"),
        )
        .unwrap();

        assert_eq!(
            resolve_session_snapshot(tmp.path(), "session-b", "md"),
            None
        );
        assert!(resolve_session_snapshot(tmp.path(), "session-a", "md").is_some());
    }

    #[test]
    fn resolve_session_snapshot_reads_legacy_flat_via_log() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("session-20260723-215536.md"), b"legacy").unwrap();
        append_entry(
            tmp.path(),
            &entry("s1", "pause", "session-20260723-215536.md", "t1"),
        )
        .unwrap();
        let got = resolve_session_snapshot(tmp.path(), "s1", "md").unwrap();
        assert_eq!(got.file_name().unwrap(), "session-20260723-215536.md");
    }

    /// Why: a snapshot whose log append did not land is still the session's
    /// own; the per-session directory is a second, layout-based attribution
    /// that cannot reach across sessions the way an mtime scan of the root can.
    /// What: with no log at all, a file under `<id>/` resolves for that id and
    /// for no other.
    /// Test: itself.
    #[test]
    fn resolve_session_snapshot_reads_per_session_dir() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("s1")).unwrap();
        fs::write(tmp.path().join("s1").join("session-old.md"), b"old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(tmp.path().join("s1").join("session-new.md"), b"new").unwrap();

        let got = resolve_session_snapshot(tmp.path(), "s1", "md").unwrap();
        assert_eq!(got.file_name().unwrap(), "session-new.md");
        assert_eq!(resolve_session_snapshot(tmp.path(), "s2", "md"), None);
    }

    /// Why: #5272 requirement — a flat snapshot `sessions-log.jsonl` cannot
    /// attribute must resolve to empty rather than to whichever session asks.
    /// What: an unlogged root-level file is invisible to every session id.
    /// Test: itself.
    #[test]
    fn resolve_session_snapshot_ignores_unattributed_flat_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("session-orphan.md"), b"whose?").unwrap();
        assert_eq!(resolve_session_snapshot(tmp.path(), "s1", "md"), None);
        assert_eq!(resolve_session_snapshot(tmp.path(), "s2", "md"), None);
    }

    #[test]
    fn resolve_session_snapshot_skips_a_log_entry_whose_file_is_gone() {
        let tmp = TempDir::new().unwrap();
        append_entry(tmp.path(), &entry("s1", "pause", "session-gone.md", "t1")).unwrap();
        assert_eq!(
            resolve_session_snapshot(tmp.path(), "s1", "md"),
            None,
            "a dangling log entry must not resolve to a phantom path"
        );
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
