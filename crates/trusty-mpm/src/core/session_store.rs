//! Persistent session pause state.
//!
//! Why: a paused session must survive a daemon restart. The daemon's in-memory
//! registry is volatile, so the pause timestamp and summary are mirrored to a
//! small JSON file under `~/.trusty-mpm/sessions/<id>/pause.json`. On boot the
//! daemon can rehydrate a session's `Paused` state from this file.
//! What: [`pause_path`] resolves the on-disk location, [`save_pause`] writes the
//! pause record, [`load_pause`] reads it back, and [`clear_pause`] removes it on
//! resume or stop.
//! Test: `cargo test -p trusty-mpm-core` round-trips a paused session through
//! the filesystem in a temp-scoped `HOME`.

use std::path::{Path, PathBuf};

use crate::core::session::{Session, SessionId};

/// Returns `<base>/.trusty-mpm/sessions/<id>/pause.json`.
///
/// Why: the on-disk pause-state operations all derive the same path; taking the
/// base directory explicitly keeps them testable against a temp directory
/// without mutating process-global `$HOME`.
/// What: joins `base`, `.trusty-mpm/sessions`, the session UUID, and
/// `pause.json`.
/// Test: `pause_path_in_layout`.
pub fn pause_path_in(base: &Path, id: &SessionId) -> PathBuf {
    base.join(".trusty-mpm")
        .join("sessions")
        .join(id.0.to_string())
        .join("pause.json")
}

/// Returns `~/.trusty-mpm/sessions/<id>/pause.json`.
///
/// Why: every pause-state operation needs the same path; deriving it once keeps
/// the layout consistent with the rest of the framework directory.
/// What: resolves the home directory and delegates to [`pause_path_in`]. Falls
/// back to a relative base when the home directory cannot be determined.
/// Test: `pause_path_is_under_home`.
pub fn pause_path(id: &SessionId) -> PathBuf {
    pause_path_in(&dirs::home_dir().unwrap_or_default(), id)
}

/// Persist the pause state for a session under an explicit base directory.
///
/// Why: the base-taking core keeps the write testable against a temp directory.
/// What: writes the pause record to [`pause_path_in`], creating parent dirs.
/// Test: `save_then_load_round_trips`.
pub fn save_pause_in(base: &Path, session: &Session) -> std::io::Result<()> {
    let path = pause_path_in(base, &session.id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let paused_at: chrono::DateTime<chrono::Utc> = session
        .paused_at
        .unwrap_or_else(std::time::SystemTime::now)
        .into();
    let record = serde_json::json!({
        "paused_at": paused_at.to_rfc3339(),
        "summary": session.pause_summary,
        "session_id": session.id.0.to_string(),
    });
    let json = serde_json::to_string_pretty(&record).map_err(std::io::Error::other)?;
    std::fs::write(&path, json)
}

/// Persist the pause state for a session, creating the directory if needed.
///
/// Why: when the operator pauses a session the daemon must record enough to
/// rehydrate it later — the timestamp, the summary note, and the session id.
/// What: writes `{ "paused_at": <rfc3339>, "summary": <text|null>,
/// "session_id": <uuid> }` to [`pause_path`]; `paused_at` defaults to "now" when
/// the session carries no explicit pause timestamp.
/// Test: `save_then_load_round_trips`.
pub fn save_pause(session: &Session) -> std::io::Result<()> {
    save_pause_in(&dirs::home_dir().unwrap_or_default(), session)
}

/// Load the pause state for a session from an explicit base directory.
///
/// Why: the base-taking core keeps the read testable against a temp directory.
/// What: reads and parses [`pause_path_in`]; a missing file or malformed JSON
/// both yield `None`.
/// Test: `load_missing_returns_none`, `save_then_load_round_trips`.
pub fn load_pause_in(base: &Path, id: &SessionId) -> Option<serde_json::Value> {
    let bytes = std::fs::read(pause_path_in(base, id)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Load the pause state for a session, or `None` when no file exists.
///
/// Why: on daemon boot the persisted record is the source of truth for whether
/// a session is paused.
/// What: reads and parses [`pause_path`]; a missing file or malformed JSON both
/// yield `None` rather than an error so callers can treat "not paused" uniformly.
/// Test: `load_missing_returns_none`, `save_then_load_round_trips`.
pub fn load_pause(id: &SessionId) -> Option<serde_json::Value> {
    load_pause_in(&dirs::home_dir().unwrap_or_default(), id)
}

/// Remove the pause file for a session under an explicit base directory.
///
/// Why: the base-taking core keeps the delete testable against a temp directory.
/// What: deletes [`pause_path_in`]; a missing file is treated as success.
/// Test: `clear_removes_file`, `clear_missing_is_ok`.
pub fn clear_pause_in(base: &Path, id: &SessionId) -> std::io::Result<()> {
    match std::fs::remove_file(pause_path_in(base, id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Remove the pause file for a session (called on resume or stop).
///
/// Why: a resumed or stopped session is no longer paused; leaving a stale
/// `pause.json` behind would wrongly rehydrate it as paused after a restart.
/// What: deletes [`pause_path`]; a missing file is treated as success so the
/// call is idempotent.
/// Test: `clear_removes_file`, `clear_missing_is_ok`.
pub fn clear_pause(id: &SessionId) -> std::io::Result<()> {
    clear_pause_in(&dirs::home_dir().unwrap_or_default(), id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::{ControlModel, SessionStatus};

    #[test]
    fn pause_path_in_layout() {
        let id = SessionId::new();
        let path = pause_path_in(Path::new("/home/op"), &id);
        assert!(path.ends_with("pause.json"));
        assert!(path.to_string_lossy().contains(".trusty-mpm"));
        assert!(path.to_string_lossy().contains(&id.0.to_string()));
    }

    #[test]
    fn pause_path_is_under_home() {
        // The home-resolving wrapper produces the same suffix layout.
        let id = SessionId::new();
        let path = pause_path(&id);
        assert!(path.ends_with("pause.json"));
        assert!(path.to_string_lossy().contains(".trusty-mpm"));
    }

    #[test]
    fn load_missing_returns_none() {
        let tmp = tempfile::tempdir().expect("temp dir");
        assert!(load_pause_in(tmp.path(), &SessionId::new()).is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let mut session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        session.status = SessionStatus::Paused;
        session.paused_at = Some(std::time::SystemTime::now());
        session.pause_summary = Some("stopped to grab coffee".to_string());

        save_pause_in(tmp.path(), &session).expect("save pause");
        let loaded = load_pause_in(tmp.path(), &session.id).expect("pause file exists");
        assert_eq!(loaded["session_id"], session.id.0.to_string());
        assert_eq!(loaded["summary"], "stopped to grab coffee");
        assert!(loaded["paused_at"].as_str().is_some());
    }

    #[test]
    fn save_with_no_summary_writes_null() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        save_pause_in(tmp.path(), &session).expect("save pause");
        let loaded = load_pause_in(tmp.path(), &session.id).expect("pause file exists");
        assert!(loaded["summary"].is_null());
    }

    #[test]
    fn clear_removes_file() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        save_pause_in(tmp.path(), &session).expect("save pause");
        assert!(load_pause_in(tmp.path(), &session.id).is_some());
        clear_pause_in(tmp.path(), &session.id).expect("clear pause");
        assert!(load_pause_in(tmp.path(), &session.id).is_none());
    }

    #[test]
    fn clear_missing_is_ok() {
        // Clearing a session that was never paused is a no-op success.
        let tmp = tempfile::tempdir().expect("temp dir");
        clear_pause_in(tmp.path(), &SessionId::new()).expect("clear is idempotent");
    }
}
