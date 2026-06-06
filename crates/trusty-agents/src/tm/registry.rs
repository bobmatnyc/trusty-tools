//! JSON-backed registry for TM projects and sessions.
//!
//! Why: TM needs durable, human-readable state for projects and sessions so
//! the harness can survive restarts and operators can inspect/diff the file.
//! Mirrors the design of `session_registry.rs` (file-read-per-call) so
//! concurrent processes share state via the file.
//! What: `TmSessionRegistry` reads/writes `.trusty-agents/state/tm_sessions.json`
//! containing a versioned envelope of sessions and projects. Provides CRUD
//! over both plus a `reconcile` helper that marks sessions as Orphaned when
//! their tmux counterpart has disappeared.
//! Test: See `tests` module — covers register/list/update, round-trip,
//! lookup-by-name/path, and the reconcile-marks-orphaned path.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::tm::project::{SessionStatus, TmProject, TmSession};

const REGISTRY_FILE: &str = "tm_sessions.json";
const CURRENT_SCHEMA_VERSION: u32 = 1;

/// On-disk envelope. New fields default so old files keep loading.
#[derive(Debug, Serialize, Deserialize, Default)]
struct TmRegistryData {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    sessions: Vec<TmSession>,
    #[serde(default)]
    projects: Vec<TmProject>,
}

/// JSON-backed registry for TM projects and sessions.
///
/// Why: Holds the file path and exposes typed CRUD so callers don't poke at
/// the JSON directly. Stays sync — writes happen at lifecycle events, not
/// per-message.
/// What: Each method loads the file, mutates, writes atomically (temp +
/// rename). Reads return owned data so callers cannot mutate registry state
/// without going through these helpers.
/// Test: See `tests` module.
pub struct TmSessionRegistry {
    path: PathBuf,
}

impl TmSessionRegistry {
    /// Open (or initialize) the registry rooted at `state_dir/tm_sessions.json`.
    ///
    /// Why: Centralize path construction; ensure parent dir exists so writes
    /// don't fail later.
    /// What: Creates `state_dir` if needed; does not create the JSON file
    /// until the first `save`.
    /// Test: `test_empty_registry_on_missing_file` confirms missing-file path
    /// loads as empty.
    pub fn open(state_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("creating {}", state_dir.display()))?;
        Ok(Self {
            path: state_dir.join(REGISTRY_FILE),
        })
    }

    /// Path of the backing file (for diagnostics / tests).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Why: Loaders need to tolerate a missing file (first run) and corrupt
    /// data must surface a clear error rather than panicking.
    /// What: Returns the parsed envelope; an empty default when the file
    /// doesn't exist.
    /// Test: `test_empty_registry_on_missing_file`.
    fn load(&self) -> Result<TmRegistryData> {
        if !self.path.exists() {
            return Ok(TmRegistryData {
                schema_version: CURRENT_SCHEMA_VERSION,
                ..Default::default()
            });
        }
        let content = std::fs::read_to_string(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        if content.trim().is_empty() {
            return Ok(TmRegistryData {
                schema_version: CURRENT_SCHEMA_VERSION,
                ..Default::default()
            });
        }
        serde_json::from_str(&content).with_context(|| format!("parsing {}", self.path.display()))
    }

    /// Why: Write must be atomic — a crash mid-write must never leave a
    /// truncated registry on disk.
    /// What: Writes pretty JSON to `<path>.tmp` then renames over the target.
    /// Test: Exercised by every mutating method's round-trip test.
    fn save(&self, data: &TmRegistryData) -> Result<()> {
        let content = serde_json::to_string_pretty(data)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), self.path.display()))?;
        Ok(())
    }

    // ==================== Session CRUD ====================

    /// Why: Inserts or replaces a session by id so callers don't have to
    /// special-case create vs update.
    /// What: Removes any existing entry with the same id, then pushes the new
    /// session.
    /// Test: `test_register_and_list_sessions`.
    pub fn register_session(&self, session: &TmSession) -> Result<()> {
        let mut data = self.load()?;
        data.sessions.retain(|s| s.id != session.id);
        data.sessions.push(session.clone());
        self.save(&data)
    }

    /// Why: Status transitions are the most common mutation; provide a small
    /// helper so call sites don't have to load/find/mutate by hand.
    /// What: Loads, finds by id, sets status, saves.
    /// Test: `test_update_status`.
    pub fn update_session_status(&self, id: &str, status: SessionStatus) -> Result<()> {
        let mut data = self.load()?;
        if let Some(s) = data.sessions.iter_mut().find(|s| s.id == id) {
            s.status = status;
        }
        self.save(&data)
    }

    /// Why: Activity tracking ("3m ago") needs `last_active` bumped on each
    /// I/O event without forcing callers to load+mutate.
    /// What: Sets `last_active = Utc::now()` for the matching session.
    /// Test: indirectly via `test_register_and_list_sessions` chains.
    pub fn touch_session(&self, id: &str) -> Result<()> {
        let mut data = self.load()?;
        if let Some(s) = data.sessions.iter_mut().find(|s| s.id == id) {
            s.last_active = Utc::now();
        }
        self.save(&data)
    }

    /// Toggle the favorite flag on a session by name OR id.
    ///
    /// Why: The WebUI's favorite endpoints (`POST/DELETE
    /// /api/tm/sessions/:name/favorite`) need a single mutation that targets
    /// the named session without callers having to load+find+save.
    /// What: Loads the registry, finds the first session matching `name_or_id`
    /// by `name` (and falling back to `id`), sets `favorite`, saves. Returns
    /// `Ok(true)` if a session was updated, `Ok(false)` otherwise so the
    /// HTTP layer can return 404 cleanly.
    /// Test: `test_set_favorite`.
    pub fn set_favorite(&self, name_or_id: &str, favorite: bool) -> Result<bool> {
        let mut data = self.load()?;
        let target = data
            .sessions
            .iter_mut()
            .find(|s| s.name == name_or_id || s.id == name_or_id);
        match target {
            Some(s) => {
                s.favorite = favorite;
                self.save(&data)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Why: Cleanly drop a session row (used after kill).
    /// What: Removes the session by id; no-op if not found.
    /// Test: `test_register_and_list_sessions` covers removal.
    pub fn remove_session(&self, id: &str) -> Result<()> {
        let mut data = self.load()?;
        data.sessions.retain(|s| s.id != id);
        self.save(&data)
    }

    /// List all sessions in the registry.
    pub fn list_sessions(&self) -> Result<Vec<TmSession>> {
        Ok(self.load()?.sessions)
    }

    /// Get a session by id.
    pub fn get_session(&self, id: &str) -> Result<Option<TmSession>> {
        Ok(self.load()?.sessions.into_iter().find(|s| s.id == id))
    }

    /// Get a session by its `name` (the human label, not the tmux name).
    pub fn get_session_by_name(&self, name: &str) -> Result<Option<TmSession>> {
        Ok(self.load()?.sessions.into_iter().find(|s| s.name == name))
    }

    /// Get all sessions belonging to a project.
    pub fn get_sessions_for_project(&self, project_id: &str) -> Result<Vec<TmSession>> {
        Ok(self
            .load()?
            .sessions
            .into_iter()
            .filter(|s| s.project_id == project_id)
            .collect())
    }

    // ==================== Project CRUD ====================

    /// Why: Insert-or-replace so callers don't have to special-case.
    /// What: Removes any existing entry with the same id, then pushes the new
    /// project.
    /// Test: `test_round_trip_project`.
    pub fn register_project(&self, project: &TmProject) -> Result<()> {
        let mut data = self.load()?;
        data.projects.retain(|p| p.id != project.id);
        data.projects.push(project.clone());
        self.save(&data)
    }

    /// Why: Updating a project (e.g., new session summary) is the same shape
    /// as registering — keep both verbs available for clarity at call sites.
    /// What: Alias for `register_project`'s replace semantics.
    /// Test: `test_round_trip_project`.
    pub fn update_project(&self, project: &TmProject) -> Result<()> {
        self.register_project(project)
    }

    /// List all projects.
    pub fn list_projects(&self) -> Result<Vec<TmProject>> {
        Ok(self.load()?.projects)
    }

    /// Get a project by id.
    pub fn get_project(&self, id: &str) -> Result<Option<TmProject>> {
        Ok(self.load()?.projects.into_iter().find(|p| p.id == id))
    }

    /// Why: TM frequently needs to ask "is this directory already a project?"
    /// without keeping a separate index.
    /// What: Linear scan comparing `path == project.path`. The number of
    /// projects per developer machine is small enough that O(n) is fine.
    /// Test: `test_get_project_by_path`.
    pub fn get_project_by_path(&self, path: &Path) -> Result<Option<TmProject>> {
        Ok(self.load()?.projects.into_iter().find(|p| p.path == path))
    }

    // ==================== Reconcile ====================

    /// Remove all sessions with status `Orphaned` from the registry.
    ///
    /// Why: Orphaned sessions accumulate after reconcile passes, bloating the
    /// list and confusing users. Pruning them keeps the registry tidy and
    /// `/tm list` output accurate.
    /// What: Loads the registry, removes any session where
    /// `status == Orphaned`, and saves only when at least one was removed.
    /// Returns the count of pruned sessions.
    /// Test: `test_prune_orphaned` below.
    pub fn prune_orphaned(&self) -> Result<usize> {
        let mut state = self.load()?;
        let before = state.sessions.len();
        state
            .sessions
            .retain(|s| s.status != SessionStatus::Orphaned);
        let pruned = before - state.sessions.len();
        if pruned > 0 {
            self.save(&state)?;
        }
        Ok(pruned)
    }

    /// Mark sessions whose tmux name is NOT in `live_tmux_sessions` as
    /// `Orphaned`. Returns the ids of newly-orphaned sessions.
    ///
    /// Why: Detect tmux sessions that died outside our control so the UI can
    /// surface them and operators can clean up.
    /// What: For each session with status `Running` whose `tmux_session_name`
    /// isn't in `live_tmux_sessions`, set status to `Orphaned`. Other statuses
    /// (Paused, Stopped, Idle, Orphaned) are left alone.
    /// Test: `test_reconcile_marks_orphaned`.
    pub fn reconcile(&self, live_tmux_sessions: &[String]) -> Result<Vec<String>> {
        let mut data = self.load()?;
        let mut orphaned_ids = Vec::new();
        for s in data.sessions.iter_mut() {
            if s.status == SessionStatus::Running
                && !live_tmux_sessions.iter().any(|n| n == &s.tmux_session_name)
            {
                s.status = SessionStatus::Orphaned;
                orphaned_ids.push(s.id.clone());
            }
        }
        if !orphaned_ids.is_empty() {
            self.save(&data)?;
        }
        Ok(orphaned_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tm::project::{AdapterType, TmProject, TmSession};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_session(name: &str, project_id: &str) -> TmSession {
        TmSession::new(
            name.to_string(),
            project_id.to_string(),
            PathBuf::from("/tmp/test"),
            AdapterType::Shell,
        )
    }

    fn make_project(path: &str) -> TmProject {
        TmProject::new(PathBuf::from(path))
    }

    fn open_registry() -> (TempDir, TmSessionRegistry) {
        let dir = TempDir::new().unwrap();
        let reg = TmSessionRegistry::open(dir.path()).unwrap();
        (dir, reg)
    }

    #[test]
    fn test_register_and_list_sessions() {
        let (_dir, reg) = open_registry();
        let s1 = make_session("alpha", "p1");
        let s2 = make_session("beta", "p1");
        reg.register_session(&s1).unwrap();
        reg.register_session(&s2).unwrap();

        let list = reg.list_sessions().unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|s| s.name == "alpha"));
        assert!(list.iter().any(|s| s.name == "beta"));

        // Re-register replaces (idempotent by id).
        let mut s1b = s1.clone();
        s1b.notes = Some("touched".to_string());
        reg.register_session(&s1b).unwrap();
        let list = reg.list_sessions().unwrap();
        assert_eq!(list.len(), 2);

        // Get by id and by name.
        let by_id = reg.get_session(&s1.id).unwrap().unwrap();
        assert_eq!(by_id.notes.as_deref(), Some("touched"));
        let by_name = reg.get_session_by_name("beta").unwrap().unwrap();
        assert_eq!(by_name.id, s2.id);

        // Remove drops the entry.
        reg.remove_session(&s2.id).unwrap();
        let list = reg.list_sessions().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, s1.id);
    }

    #[test]
    fn test_update_status() {
        let (_dir, reg) = open_registry();
        let s = make_session("alpha", "p1");
        reg.register_session(&s).unwrap();

        reg.update_session_status(&s.id, SessionStatus::Paused)
            .unwrap();
        let updated = reg.get_session(&s.id).unwrap().unwrap();
        assert_eq!(updated.status, SessionStatus::Paused);
    }

    #[test]
    fn test_prune_orphaned() {
        let (_dir, reg) = open_registry();
        let s_running = make_session("runner", "p1");
        let s_orphan = make_session("ghost", "p1");
        reg.register_session(&s_running).unwrap();
        reg.register_session(&s_orphan).unwrap();

        // Mark ghost as orphaned.
        reg.update_session_status(&s_orphan.id, SessionStatus::Orphaned)
            .unwrap();

        let pruned = reg.prune_orphaned().unwrap();
        assert_eq!(pruned, 1);

        let list = reg.list_sessions().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, s_running.id);

        // Pruning again when nothing to remove returns 0 and does not error.
        let pruned_again = reg.prune_orphaned().unwrap();
        assert_eq!(pruned_again, 0);
    }

    #[test]
    fn test_reconcile_marks_orphaned() {
        let (_dir, reg) = open_registry();
        let s_live = make_session("live", "p1"); // tmux_session_name = "live"
        let s_dead = make_session("dead", "p1"); // tmux_session_name = "dead"
        reg.register_session(&s_live).unwrap();
        reg.register_session(&s_dead).unwrap();

        let live = vec!["live".to_string()];
        let orphaned = reg.reconcile(&live).unwrap();
        assert_eq!(orphaned, vec![s_dead.id.clone()]);

        let updated_dead = reg.get_session(&s_dead.id).unwrap().unwrap();
        assert_eq!(updated_dead.status, SessionStatus::Orphaned);
        let updated_live = reg.get_session(&s_live.id).unwrap().unwrap();
        assert_eq!(updated_live.status, SessionStatus::Running);

        // Running again returns nothing new — idempotent.
        let again = reg.reconcile(&live).unwrap();
        assert!(again.is_empty());
    }

    #[test]
    fn test_round_trip_project() {
        let (_dir, reg) = open_registry();
        let mut p = make_project("/tmp/proj-a");
        p.name = "proj-a".to_string();
        reg.register_project(&p).unwrap();

        let list = reg.list_projects().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, PathBuf::from("/tmp/proj-a"));

        // Update mutates in-place.
        let mut p2 = p.clone();
        p2.name = "renamed".to_string();
        reg.update_project(&p2).unwrap();
        let got = reg.get_project(&p.id).unwrap().unwrap();
        assert_eq!(got.name, "renamed");
    }

    #[test]
    fn test_get_project_by_path() {
        let (_dir, reg) = open_registry();
        let p1 = make_project("/tmp/a");
        let p2 = make_project("/tmp/b");
        reg.register_project(&p1).unwrap();
        reg.register_project(&p2).unwrap();

        let found = reg.get_project_by_path(Path::new("/tmp/b")).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, p2.id);

        let missing = reg.get_project_by_path(Path::new("/tmp/nope")).unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_get_sessions_for_project() {
        let (_dir, reg) = open_registry();
        reg.register_session(&make_session("a1", "proj-1")).unwrap();
        reg.register_session(&make_session("a2", "proj-1")).unwrap();
        reg.register_session(&make_session("b1", "proj-2")).unwrap();

        let in_p1 = reg.get_sessions_for_project("proj-1").unwrap();
        assert_eq!(in_p1.len(), 2);
        let in_p2 = reg.get_sessions_for_project("proj-2").unwrap();
        assert_eq!(in_p2.len(), 1);
    }

    #[test]
    fn test_set_favorite() {
        let (_dir, reg) = open_registry();
        let s = make_session("starme", "p1");
        reg.register_session(&s).unwrap();

        // Toggle on.
        let updated = reg.set_favorite("starme", true).unwrap();
        assert!(updated);
        let got = reg.get_session(&s.id).unwrap().unwrap();
        assert!(got.favorite);

        // Toggle off via id path.
        let updated = reg.set_favorite(&s.id, false).unwrap();
        assert!(updated);
        let got = reg.get_session(&s.id).unwrap().unwrap();
        assert!(!got.favorite);

        // Missing target → Ok(false), not error.
        let updated = reg.set_favorite("does-not-exist", true).unwrap();
        assert!(!updated);
    }

    #[test]
    fn test_empty_registry_on_missing_file() {
        let (_dir, reg) = open_registry();
        // No file written yet.
        assert!(!reg.path().exists());
        assert!(reg.list_sessions().unwrap().is_empty());
        assert!(reg.list_projects().unwrap().is_empty());
        assert!(reg.get_session("missing").unwrap().is_none());
    }
}
