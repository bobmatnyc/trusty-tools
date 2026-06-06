//! CTRL sessions — interactive REPL sessions scoped to a project, optionally
//! backed by a git worktree.
//!
//! Why: `om session new/list/attach/kill` (#406) needs a persistent record of
//! interactive sessions distinct from the existing chat-history `SessionManager`
//! (`src/session.rs`) and from workflow sessions (`.trusty-agents/state/sessions.json`).
//! Each CTRL session may own a git worktree so multiple agents can work on the
//! same repository in parallel without stepping on each other.
//! What: `Session` is the persisted record (id, project, worktree, status).
//! `SessionStore` provides JSON-backed CRUD on `~/.trusty-agents/sessions/ctrl-sessions.json`.
//! Test: See `tests` module — covers `Session::new` defaults and a full
//! upsert / find / terminate roundtrip via a temp HOME dir.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle state of a CTRL session.
///
/// Why: `om session list` filters by status; the API exposes the same enum.
/// The supervisor (#408 follow-up) needs a distinct `Blocked` state so that a
/// session record accurately reflects "the supervisor gave up after exhausting
/// retries", separate from `Terminated` (user kill / clean shutdown).
/// What: Four explicit variants serialised as snake_case.
/// Test: Covered by the roundtrip test (terminate flips the variant) and by
/// `session_status_blocked_serialises` for the new variant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Idle,
    Blocked,
    Terminated,
}

/// Persisted CTRL session record.
///
/// Why: Surfaces every field the CLI / API needs without requiring callers to
/// reach back into git or the project registry.
/// What: Identity, project location, optional worktree info, timestamps, status,
/// and the server port the session was created against.
/// Test: `session_new_sets_project_name` validates the constructor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub project_path: PathBuf,
    pub project_name: String,
    pub name: String,
    pub agent: String,
    pub worktree_path: Option<PathBuf>,
    pub worktree_branch: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
    pub status: SessionStatus,
    pub port: u16,
}

impl Session {
    /// Build a new session with `Idle` status and no worktree attached.
    ///
    /// Why: All callers (API + tests) need consistent defaults; centralising
    /// them keeps `created_at == last_active` invariant in one place.
    /// What: Derives `project_name` from the trailing path component, generates
    /// a v4 UUID, and stamps both timestamps with `Utc::now()`.
    /// Test: `session_new_sets_project_name`.
    pub fn new(project_path: PathBuf, name: String, agent: String, port: u16) -> Self {
        let project_name = project_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            project_path,
            project_name,
            name,
            agent,
            worktree_path: None,
            worktree_branch: None,
            created_at: now,
            last_active: now,
            status: SessionStatus::Idle,
            port,
        }
    }
}

/// JSON-backed store for CTRL sessions.
///
/// Why: Sessions must survive restarts of the API server and be visible to
/// concurrent CLI invocations. A flat JSON file under the user's home dir
/// (alongside other trusty-agents runtime state) is the simplest correct primitive
/// and avoids reaching for a database.
/// What: Reads/writes `~/.trusty-agents/sessions/ctrl-sessions.json` as a JSON array
/// of `Session`. Missing/unreadable files yield an empty list (non-fatal).
/// Test: `session_store_roundtrip` exercises `upsert` -> `find` -> `terminate`
/// with `HOME` redirected to a tempdir.
pub struct SessionStore;

impl SessionStore {
    fn path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".trusty-agents")
            .join("sessions")
            .join("ctrl-sessions.json")
    }

    /// Load every session record from disk.
    ///
    /// Why: All other operations are read-modify-write; loading the full list
    /// is the common path. Missing or malformed files return an empty list so
    /// a fresh install or a corrupted file degrades gracefully.
    /// What: Reads the JSON array; on any error returns `Vec::new()`.
    /// Test: `session_store_roundtrip` (indirectly).
    pub fn load() -> Vec<Session> {
        let path = Self::path();
        if !path.exists() {
            return Vec::new();
        }
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        serde_json::from_str(&content).unwrap_or_default()
    }

    /// Persist the full session list to disk.
    ///
    /// Why: Single chokepoint for serialisation keeps the on-disk format in
    /// one place and lets us evolve it later (e.g. atomic rename).
    /// What: Creates the parent dir if absent, writes pretty JSON.
    /// Test: Exercised by `session_store_roundtrip`.
    pub fn save(sessions: &[Session]) -> anyhow::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(sessions)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Insert or update a session in the store, returning the saved record.
    ///
    /// Why: API handlers need a single call that handles both create and
    /// status-update flows; same-id sessions replace, new ids append.
    /// What: Loads the list, replaces by `id` or pushes, then saves.
    /// Test: `session_store_roundtrip`.
    pub fn upsert(session: Session) -> anyhow::Result<Session> {
        let mut sessions = Self::load();
        if let Some(pos) = sessions.iter().position(|s| s.id == session.id) {
            sessions[pos] = session.clone();
        } else {
            sessions.push(session.clone());
        }
        Self::save(&sessions)?;
        Ok(session)
    }

    /// Find a session by id.
    ///
    /// Why: Used by `GET /api/ctrl/sessions/:id`, attach, and the worktree
    /// cleanup path in DELETE.
    /// What: Linear scan — the list is tiny in practice.
    /// Test: `session_store_roundtrip`.
    pub fn find(id: &Uuid) -> Option<Session> {
        Self::load().into_iter().find(|s| s.id == *id)
    }

    /// Mark a session as blocked. Returns true if it existed.
    ///
    /// Why: The supervisor (#408 follow-up) escalates with `Blocked` instead of
    /// `Terminated` so users can distinguish "we gave up" from "user killed it".
    /// What: Flips `status` to `Blocked` and bumps `last_active`, then saves.
    /// Test: `session_store_blocked_status` exercises the round-trip.
    pub fn mark_blocked(id: &Uuid) -> anyhow::Result<bool> {
        let mut sessions = Self::load();
        let found = sessions.iter_mut().find(|s| s.id == *id);
        if let Some(s) = found {
            s.status = SessionStatus::Blocked;
            s.last_active = Utc::now();
            Self::save(&sessions)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Mark a session as terminated. Returns true if it existed.
    ///
    /// Why: We keep terminated sessions in the JSON for history rather than
    /// deleting them; the CLI can later filter them out by status.
    /// What: Flips `status` and bumps `last_active`, then saves.
    /// Test: `session_store_roundtrip`.
    pub fn terminate(id: &Uuid) -> anyhow::Result<bool> {
        let mut sessions = Self::load();
        let found = sessions.iter_mut().find(|s| s.id == *id);
        if let Some(s) = found {
            s.status = SessionStatus::Terminated;
            s.last_active = Utc::now();
            Self::save(&sessions)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn session_new_sets_project_name() {
        let session = Session::new(
            PathBuf::from("/tmp/my-project"),
            "test-session".to_string(),
            "pm".to_string(),
            8765,
        );
        assert_eq!(session.project_name, "my-project");
        assert_eq!(session.status, SessionStatus::Idle);
        assert!(session.worktree_path.is_none());
        assert_eq!(session.port, 8765);
    }

    #[test]
    fn session_status_blocked_serialises() {
        // Why: The CLI / API filter clauses need to round-trip the new `Blocked`
        // variant through serde without falling back to a different status.
        let s = SessionStatus::Blocked;
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"blocked\"");
        let back: SessionStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, SessionStatus::Blocked);
    }

    #[test]
    fn session_store_blocked_status() {
        let _g = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        // SAFETY: HOME_LOCK held for the entire test body.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        let session = Session::new(
            PathBuf::from("/tmp/proj"),
            "blocked-test".to_string(),
            "pm".to_string(),
            8765,
        );
        let id = session.id;
        SessionStore::upsert(session).unwrap();

        let blocked = SessionStore::mark_blocked(&id).unwrap();
        assert!(blocked);
        let after = SessionStore::find(&id).unwrap();
        assert_eq!(after.status, SessionStatus::Blocked);

        // SAFETY: HOME_LOCK still held.
        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn session_store_roundtrip() {
        // #409: Hold the process-wide HOME_LOCK for the duration of this
        // test. Without it, a parallel test that also mutates HOME (e.g.
        // session_e2e_*, connect_project_persists_to_registry) races us
        // and the SessionStore writes/reads land in the wrong directory.
        let _g = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        // SAFETY: HOME_LOCK held for the entire test body.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        let session = Session::new(
            PathBuf::from("/tmp/proj"),
            "roundtrip".to_string(),
            "pm".to_string(),
            8765,
        );
        let id = session.id;

        SessionStore::upsert(session).unwrap();
        let found = SessionStore::find(&id);
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "roundtrip");

        let terminated = SessionStore::terminate(&id).unwrap();
        assert!(terminated);
        let after = SessionStore::find(&id).unwrap();
        assert_eq!(after.status, SessionStatus::Terminated);

        // Restore HOME so subsequent tests aren't surprised. SAFETY: still
        // holding HOME_LOCK; guard drops at end of function.
        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
