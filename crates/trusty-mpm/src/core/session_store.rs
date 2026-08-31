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
/// #4323: this used to delete only `pause.json`, leaving the `<id>/` directory
/// that [`save_pause_in`] created. Every pause/resume cycle therefore left one
/// permanent empty directory behind — 7,240 of the operator's 9,695.
/// What: deletes [`pause_path_in`]; a missing file is treated as success. Then
/// tries `remove_dir` (never `remove_dir_all`) on the `<id>/` holder, which the
/// kernel refuses when anything else lives there — so a session directory that
/// carries more than the pause file survives untouched.
/// Test: `clear_removes_file`, `clear_missing_is_ok`,
/// `clear_removes_the_emptied_session_dir`,
/// `clear_leaves_a_session_dir_holding_other_files`.
pub fn clear_pause_in(base: &Path, id: &SessionId) -> std::io::Result<()> {
    let path = pause_path_in(base, id);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    // #4323: reclaim the holder directory. A failure here is the ORDINARY case
    // (the directory still holds something) and is never a failure of the
    // clear, so it is reported at debug and not propagated.
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::remove_dir(dir)
    {
        tracing::debug!(
            dir = %dir.display(),
            error = %e,
            "session dir not reclaimed after clearing pause.json (#4323)"
        );
    }
    Ok(())
}

/// Directory holding one subdirectory per session: `<base>/.trusty-mpm/sessions`.
///
/// Why: the reaper and the pause-path helpers must agree on where session state
/// lives; deriving it twice is how the two would drift apart.
/// What: `<base>/.trusty-mpm/sessions`, the parent of every [`pause_path_in`].
/// Test: `sessions_root_is_the_pause_path_grandparent`.
pub fn sessions_root_in(base: &Path) -> PathBuf {
    sessions_root_under(&base.join(".trusty-mpm"))
}

/// The sessions directory under an already-resolved framework root.
///
/// Why (#4323): the daemon holds `state.framework_root()` — a temp dir under
/// test, `~/.trusty-mpm` in production — not the base it was derived from.
/// Joining `.trusty-mpm` again would double-nest, and resolving the home
/// directory instead would point a deleting sweep at a directory the daemon
/// does not own.
/// What: `<framework_root>/sessions`. [`sessions_root_in`] is this function
/// after one `.trusty-mpm` join, so the two cannot drift.
/// Test: `sessions_root_in_is_sessions_root_under_the_framework_root`.
pub fn sessions_root_under(framework_root: &Path) -> PathBuf {
    framework_root.join("sessions")
}

/// What one [`reap_empty_session_dirs_in`] sweep did.
///
/// Why: the daemon logs a COUNT per sweep rather than a line per path (#4323),
/// so the sweep has to return counts rather than emit them.
/// What: `removed` were reclaimed; `skipped` were left alone because the sweep
/// could not prove them safe to delete; `failed` were provably empty but the
/// removal itself errored.
/// Test: `reap_removes_only_empty_uuid_dirs`, `reap_skips_what_it_cannot_prove`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SessionDirSweep {
    /// Empty session directories reclaimed this sweep.
    pub removed: usize,
    /// Entries deliberately left alone — see [`reap_empty_session_dirs_in`].
    pub skipped: usize,
    /// Entries that looked reclaimable but whose removal errored.
    pub failed: usize,
}

impl SessionDirSweep {
    /// Nothing to report — used to suppress the daemon's per-sweep log line.
    pub fn is_empty(&self) -> bool {
        self.removed == 0 && self.failed == 0
    }
}

/// Reclaim empty per-session directories under `<base>/.trusty-mpm/sessions`.
///
/// Why (#4323): `save_pause_in` creates `<id>/` and `clear_pause_in` used to
/// delete only the file inside it, so the operator's state dir accumulated
/// 7,240 empty directories that nothing ever removed. The `clear_pause_in` fix
/// stops new ones; this sweep clears the backlog and covers the cases that
/// bypass `clear_pause_in` entirely (a crash between `create_dir_all` and the
/// write, a daemon killed mid-resume).
///
/// What: FAIL-CLOSED — an entry is removed only when the sweep can prove all of
/// it, and any question it cannot answer counts as `skipped` rather than as
/// permission to delete:
///
/// - the entry is a directory (a `file_type` error skips it),
/// - its name parses as a session UUID, so it is one of ours,
/// - `read_dir` on it succeeds and yields nothing at all,
/// - and the removal is `remove_dir`, never `remove_dir_all` — the kernel
///   refuses a non-empty directory, so even a file written between the check
///   and the removal survives.
///
/// A tracked project snapshot cannot be reached by this: git stores no empty
/// directory, so an empty directory is never tracked content, and the sweep
/// never recurses or deletes a file. A missing sessions root is `Ok` with zero
/// counts, not an error — the daemon runs this before the operator has ever
/// paused anything.
///
/// Test: `reap_removes_only_empty_uuid_dirs`, `reap_skips_what_it_cannot_prove`,
/// `reap_missing_root_is_not_an_error`, `reap_unreadable_root_is_an_error`.
pub fn reap_empty_session_dirs_in(base: &Path) -> std::io::Result<SessionDirSweep> {
    reap_empty_session_dirs_at(&sessions_root_in(base))
}

/// [`reap_empty_session_dirs_in`], taking the sessions directory directly.
///
/// Why (#4323): the daemon must sweep ITS OWN root
/// ([`sessions_root_under`]`(state.framework_root())`), never a home-resolved
/// path. An earlier revision of this module also exposed a
/// `reap_empty_session_dirs()` that called `dirs::home_dir()`; the daemon used
/// it, and a scratch-rooted daemon in a test consequently deleted an empty
/// session directory out of the operator's real `~/.trusty-mpm/sessions/`.
/// There is deliberately no home-resolving wrapper any more — the caller names
/// the directory it owns.
/// What: the fail-closed sweep described on [`reap_empty_session_dirs_in`],
/// against `sessions_dir` exactly as given.
/// Test: `reap_removes_only_empty_uuid_dirs`, `reap_skips_what_it_cannot_prove`,
/// `reap_missing_root_is_not_an_error`, `reap_unreadable_root_is_an_error`,
/// and `orphan_gc_loop_leaves_the_home_session_dirs_alone_on_a_scratch_root`
/// in `daemon::mod`.
pub fn reap_empty_session_dirs_at(sessions_dir: &Path) -> std::io::Result<SessionDirSweep> {
    let root = sessions_dir;
    if !root.exists() {
        return Ok(SessionDirSweep::default());
    }
    let mut sweep = SessionDirSweep::default();
    for entry in std::fs::read_dir(root)? {
        // A per-entry read error is a question the sweep cannot answer.
        let Ok(entry) = entry else {
            sweep.skipped += 1;
            continue;
        };
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => {}
            _ => {
                sweep.skipped += 1;
                continue;
            }
        }
        // Only OUR directories: `save_pause_in` names them `id.0.to_string()`,
        // which is `Uuid`'s HYPHENATED spelling and nothing else. #4323:
        // `parse_str` alone also accepts the simple 32-hex, braced and urn
        // forms, so it would have admitted three directory names this code can
        // never have written — a widened delete gate for no gain. Round-tripping
        // through `hyphenated()` narrows it back to what we produce.
        let is_session_dir = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
            uuid::Uuid::parse_str(n).is_ok_and(|u| u.hyphenated().to_string() == n)
        });
        if !is_session_dir {
            sweep.skipped += 1;
            continue;
        }
        let Ok(mut contents) = std::fs::read_dir(&path) else {
            sweep.skipped += 1;
            continue;
        };
        if contents.next().is_some() {
            sweep.skipped += 1;
            continue;
        }
        match std::fs::remove_dir(&path) {
            Ok(()) => sweep.removed += 1,
            Err(e) => {
                tracing::debug!(
                    dir = %path.display(),
                    error = %e,
                    "empty session dir could not be reclaimed (#4323)"
                );
                sweep.failed += 1;
            }
        }
    }
    Ok(sweep)
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

    /// Why: #4323 — `clear_pause_in` deleted `pause.json` and left the `<id>/`
    /// directory `save_pause_in` had created, so every pause/resume cycle added
    /// one permanent empty directory to `~/.trusty-mpm/sessions/`. This test
    /// fails against the pre-fix code: the directory survived the clear.
    /// What: pause, clear, then assert the holder directory is gone.
    /// Test: itself.
    #[test]
    fn clear_removes_the_emptied_session_dir() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        save_pause_in(tmp.path(), &session).expect("save pause");
        let dir = pause_path_in(tmp.path(), &session.id)
            .parent()
            .expect("pause.json has a parent")
            .to_path_buf();
        assert!(dir.is_dir(), "precondition: the holder dir was created");

        clear_pause_in(tmp.path(), &session.id).expect("clear pause");
        assert!(
            !dir.exists(),
            "the emptied session dir must not survive the clear: {}",
            dir.display()
        );
    }

    /// #4323: the counterweight — a session directory carrying anything ELSE is
    /// never reclaimed, because `remove_dir` refuses a non-empty directory.
    #[test]
    fn clear_leaves_a_session_dir_holding_other_files() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        save_pause_in(tmp.path(), &session).expect("save pause");
        let dir = pause_path_in(tmp.path(), &session.id)
            .parent()
            .expect("pause.json has a parent")
            .to_path_buf();
        std::fs::write(dir.join("scrollback.txt"), b"pane dump").expect("seed a sibling file");

        clear_pause_in(tmp.path(), &session.id).expect("clear pause");
        assert!(dir.is_dir(), "a populated session dir must survive");
        assert!(dir.join("scrollback.txt").is_file(), "and keep its content");
    }

    #[test]
    fn sessions_root_is_the_pause_path_grandparent() {
        let id = SessionId::new();
        let base = Path::new("/home/op");
        assert_eq!(
            pause_path_in(base, &id).parent().and_then(|p| p.parent()),
            Some(sessions_root_in(base).as_path())
        );
    }

    /// #4323: the reaper removes exactly the empty UUID-named directories.
    #[test]
    fn reap_removes_only_empty_uuid_dirs() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = sessions_root_in(tmp.path());
        let empty = SessionId::new().0.to_string();
        std::fs::create_dir_all(root.join(&empty)).expect("seed empty dir");

        let sweep = reap_empty_session_dirs_in(tmp.path()).expect("sweep");
        assert_eq!(
            sweep,
            SessionDirSweep {
                removed: 1,
                skipped: 0,
                failed: 0
            }
        );
        assert!(!root.join(&empty).exists());
    }

    /// #4323: FAIL-CLOSED. Three things the sweep cannot prove safe — a
    /// non-empty session dir, a directory whose name is not a session UUID, and
    /// a plain file — are all skipped, and every one of them still exists
    /// afterwards. The non-UUID directory is the guard that keeps an operator's
    /// own directory out of reach even when it happens to be empty.
    #[test]
    fn reap_skips_what_it_cannot_prove() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = sessions_root_in(tmp.path());
        std::fs::create_dir_all(&root).expect("seed root");

        let populated = SessionId::new().0.to_string();
        std::fs::create_dir_all(root.join(&populated)).expect("seed populated dir");
        std::fs::write(root.join(&populated).join("pause.json"), b"{}").expect("seed file");

        // Empty, but NOT UUID-named — e.g. an operator's own scratch directory.
        std::fs::create_dir_all(root.join("not-a-session-id")).expect("seed foreign dir");
        std::fs::write(root.join("stray.md"), b"# notes").expect("seed loose file");

        // #4323: `Uuid::parse_str` also accepts the simple 32-hex spelling, but
        // `save_pause_in` only ever writes the hyphenated one — so a 32-hex
        // directory is somebody else's and must be skipped.
        let simple = SessionId::new().0.simple().to_string();
        assert_eq!(simple.len(), 32, "the simple form carries no hyphens");
        std::fs::create_dir_all(root.join(&simple)).expect("seed 32-hex dir");

        let sweep = reap_empty_session_dirs_in(tmp.path()).expect("sweep");
        assert_eq!(
            sweep,
            SessionDirSweep {
                removed: 0,
                skipped: 4,
                failed: 0
            }
        );
        assert!(root.join(&populated).join("pause.json").is_file());
        assert!(root.join("not-a-session-id").is_dir());
        assert!(root.join("stray.md").is_file());
        assert!(
            root.join(&simple).is_dir(),
            "a 32-hex name is not a spelling this code writes; it must survive"
        );
    }

    /// #4323: the two derivations must not drift — `sessions_root_in` is
    /// `sessions_root_under` after one `.trusty-mpm` join, and the daemon uses
    /// the latter against its own `framework_root()`.
    #[test]
    fn sessions_root_in_is_sessions_root_under_the_framework_root() {
        let base = Path::new("/home/op");
        assert_eq!(
            sessions_root_in(base),
            sessions_root_under(&base.join(".trusty-mpm"))
        );
    }

    /// #4323: a sessions root that was never created is a zero-count success,
    /// not an error — the daemon sweeps before the operator has ever paused.
    #[test]
    fn reap_missing_root_is_not_an_error() {
        let tmp = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            reap_empty_session_dirs_in(tmp.path()).expect("missing root is Ok"),
            SessionDirSweep::default()
        );
    }

    /// Why: the daemon's arm for this sweep is `Err → warn → continue`, which is
    /// only reachable if the sweep can actually return `Err`. A sweep that
    /// swallowed its own listing failure would report a clean zero-count sweep
    /// while reaping nothing, and the warn arm would be dead code.
    /// What: a `sessions` path that is a FILE makes `read_dir` fail; the sweep
    /// propagates it rather than reporting success.
    /// Test: itself.
    #[test]
    fn reap_unreadable_root_is_an_error() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = sessions_root_in(tmp.path());
        std::fs::create_dir_all(root.parent().expect("root has a parent")).expect("seed parent");
        std::fs::write(&root, b"not a directory").expect("seed root as a file");

        assert!(
            reap_empty_session_dirs_in(tmp.path()).is_err(),
            "a listing failure must reach the caller's warn arm, not read as a clean sweep"
        );
    }

    #[test]
    fn sweep_is_empty_ignores_skips() {
        // A sweep that only skipped has nothing to report — otherwise the
        // daemon would log a line every 60s for a permanently-skipped entry,
        // which is the log-volume shape #4323 is bounding.
        assert!(
            SessionDirSweep {
                removed: 0,
                skipped: 42,
                failed: 0
            }
            .is_empty()
        );
        assert!(
            !SessionDirSweep {
                removed: 1,
                skipped: 0,
                failed: 0
            }
            .is_empty()
        );
        assert!(
            !SessionDirSweep {
                removed: 0,
                skipped: 0,
                failed: 1
            }
            .is_empty()
        );
    }
}
