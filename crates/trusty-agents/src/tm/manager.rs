//! High-level TM (Tmux Manager) facade.
//!
//! Why: Phase 3 of TM — combines [`TmuxOrchestrator`] (live tmux state),
//! [`AdapterRegistry`] (harness detection), and [`TmSessionRegistry`]
//! (durable JSON state) behind one async API so callers don't have to
//! coordinate three subsystems by hand.
//! What: `TmManager` exposes session lifecycle (`new_session`,
//! `kill_session`), inspection (`list_sessions`, `list_projects`,
//! `capture_pane`), control (`pause_session`, `resume_session`,
//! `send_message`), and a `reconcile` helper that syncs the registry with
//! live tmux. Most methods are async even though tmux calls are sync —
//! this leaves room for I/O parallelism (capture+detect across many
//! sessions) without breaking the API later.
//! Test: Logic-only unit tests live below; full integration coverage
//! requires a real tmux server and is gated behind `#[ignore]`.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};

use crate::adapters::{AdapterRegistry, HarnessAdapter};
use crate::tm::framework::detect_framework;
use crate::tm::project::{AdapterType, SessionStatus, TmProject, TmSession};
use crate::tm::registry::TmSessionRegistry;
use crate::tmux::TmuxOrchestrator;

/// Outcome of a `TmManager::reconcile` pass.
///
/// Why: Callers (UI, CLI) want to know what changed so they can render a
/// summary ("found 1 new session, marked 2 orphaned"). Returning a single
/// struct keeps the call site clean.
/// What: `added` lists sessions discovered in tmux but not the registry;
/// `orphaned` is the tmux-name list of registry sessions whose tmux
/// counterpart has disappeared; `updated` is reserved for in-place changes
/// (e.g., re-detected adapter type) and is currently always empty.
/// Test: `test_reconcile_report_default`.
#[derive(Debug, Default)]
pub struct ReconcileReport {
    pub added: Vec<TmSession>,
    pub orphaned: Vec<String>,
    pub updated: Vec<TmSession>,
}

/// High-level facade that ties tmux, adapters, and the JSON registry together.
///
/// Why: Centralizing the cross-subsystem orchestration in one place keeps
/// the API surface small and makes it possible to reason about side effects
/// (every state mutation goes through `self.registry`).
/// What: Owns a `TmuxOrchestrator`, a shared `AdapterRegistry`, and a
/// `TmSessionRegistry` rooted at the supplied state directory.
/// Test: Logic-only tests below; integration tests live in
/// `tests/tm_manager_integration.rs` once tmux is available in CI.
pub struct TmManager {
    pub tmux: TmuxOrchestrator,
    pub adapters: Arc<AdapterRegistry>,
    pub registry: TmSessionRegistry,
}

impl TmManager {
    /// Why: Construct the full TM stack from a single state directory so
    /// callers don't have to wire three components together.
    /// What: Builds the tmux orchestrator (which verifies the binary is
    /// available), creates a default `AdapterRegistry`, and opens the JSON
    /// registry under `state_dir`.
    /// Test: `test_resolve_session_not_found` exercises construction.
    pub fn new(state_dir: &Path) -> Result<Self> {
        let tmux = TmuxOrchestrator::new().context("initializing tmux orchestrator")?;
        let adapters = Arc::new(AdapterRegistry::new());
        let registry = TmSessionRegistry::open(state_dir)
            .with_context(|| format!("opening tm registry under {}", state_dir.display()))?;
        Ok(Self {
            tmux,
            adapters,
            registry,
        })
    }

    // ==================== Session Lifecycle ====================

    /// Create a new tmux session, register it, and return the typed handle.
    ///
    /// Why: One canonical create-path so the registry never falls out of sync
    /// with tmux. Resolves name conflicts by appending `-2`, `-3`, … so the
    /// caller can pick a friendly base name without checking first.
    /// What: Picks a unique tmux name, calls `tmux new-session`, ensures a
    /// `TmProject` exists for `project_path`, builds a `TmSession`, and
    /// persists both. Defaults to `AdapterType::Shell` when `adapter_type` is
    /// `None` (a freshly-spawned tmux session has no harness yet).
    /// Test: covered by integration tests; logic-only tests assert
    /// resolve_session and report defaults.
    pub async fn new_session(
        &self,
        name: &str,
        project_path: &Path,
        adapter_type: Option<AdapterType>,
    ) -> Result<TmSession> {
        let unique_name = self.unique_tmux_name(name);
        let dir_str = project_path.to_string_lossy().to_string();
        self.tmux.create_session(&unique_name, Some(&dir_str))?;

        let mut project = self.get_or_create_project(project_path).await?;
        let mut session = TmSession::new(
            unique_name.clone(),
            project.id.clone(),
            project_path.to_path_buf(),
            adapter_type.unwrap_or(AdapterType::Shell),
        );
        // tmux name and human name match for new sessions.
        session.tmux_session_name = unique_name;

        project.add_session(session.to_summary());
        self.registry.register_session(&session)?;
        self.registry.update_project(&project)?;
        Ok(session)
    }

    /// Why: We can't `tmux attach` from inside the REPL (alt-screen/ratatui),
    /// so the manager returns a copy-pasteable command instead of trying to
    /// exec it.
    /// What: Returns `tmux attach-session -t <tmux_session_name>`.
    /// Test: covered by `test_attach_instructions_format` once tmux exists.
    pub fn attach_instructions(&self, name_or_id: &str) -> Result<String> {
        let session = self.resolve_session(name_or_id)?;
        Ok(format!(
            "tmux attach-session -t {}",
            session.tmux_session_name
        ))
    }

    /// Send the adapter's pause command and mark the session Paused.
    ///
    /// Why: Pause is the most common control operation; centralize the
    /// adapter-lookup + send + status update so every call site does the
    /// same thing.
    /// What: Looks up the session, fetches its adapter, sends
    /// `adapter.pause_command()` via tmux, and sets status to `Paused`.
    /// Errors if the adapter doesn't support pause.
    /// Test: integration only.
    pub async fn pause_session(&self, name_or_id: &str) -> Result<()> {
        let session = self.resolve_session(name_or_id)?;
        let adapter = self.get_adapter(&session.adapter_type);
        match adapter.pause_command() {
            Some(cmd) => {
                self.tmux.send_line(&session.tmux_session_name, None, cmd)?;
                self.registry
                    .update_session_status(&session.id, SessionStatus::Paused)?;
                Ok(())
            }
            None => bail!("Adapter '{}' does not support pause", session.adapter_type),
        }
    }

    /// Resume — symmetric to `pause_session`.
    pub async fn resume_session(&self, name_or_id: &str) -> Result<()> {
        let session = self.resolve_session(name_or_id)?;
        let adapter = self.get_adapter(&session.adapter_type);
        match adapter.resume_command() {
            Some(cmd) => {
                self.tmux.send_line(&session.tmux_session_name, None, cmd)?;
                self.registry
                    .update_session_status(&session.id, SessionStatus::Running)?;
                Ok(())
            }
            None => bail!("Adapter '{}' does not support resume", session.adapter_type),
        }
    }

    /// Kill the underlying tmux session and mark it Stopped.
    ///
    /// Why: The registry must stay in sync with tmux even when the kill
    /// races with manual cleanup; we always set status before returning.
    /// What: Best-effort `destroy_session` (ignored if already gone), then
    /// `update_session_status(Stopped)`.
    /// Test: integration only.
    pub async fn kill_session(&self, name_or_id: &str) -> Result<()> {
        let session = self.resolve_session(name_or_id)?;
        // Tmux may have already cleaned up the session — treat that as success.
        if self.tmux.session_exists(&session.tmux_session_name) {
            self.tmux.destroy_session(&session.tmux_session_name)?;
        }
        self.registry
            .update_session_status(&session.id, SessionStatus::Stopped)?;
        Ok(())
    }

    /// Send a user message to the session, formatted by its adapter.
    ///
    /// Why: Different harnesses prefer different message wrappers (some need
    /// a prefix, some want raw text); delegate to `adapter.format_message`
    /// so each adapter owns its own protocol.
    /// What: Resolves the session, formats the message, sends it line-by-line
    /// via `tmux.send_line`, and bumps `last_active`.
    /// Test: integration only.
    pub async fn send_message(&self, name_or_id: &str, message: &str) -> Result<()> {
        let session = self.resolve_session(name_or_id)?;
        let adapter = self.get_adapter(&session.adapter_type);
        let formatted = adapter.format_message(message);
        self.tmux
            .send_line(&session.tmux_session_name, None, &formatted)?;
        self.registry.touch_session(&session.id)?;
        Ok(())
    }

    /// Capture the last `lines` of pane output for the session.
    pub async fn capture_pane(&self, name_or_id: &str, lines: u32) -> Result<String> {
        let session = self.resolve_session(name_or_id)?;
        Ok(self
            .tmux
            .capture_output(&session.tmux_session_name, None, Some(lines))?)
    }

    /// Spawn a new session using the canonical `<project>-<harness>-<serial>`
    /// naming convention from the refined `/connect` spec.
    ///
    /// Why: The REPL's `/connect <path> <adapter> [name]` command and the
    /// HTTP API need an identical naming policy so a session created on the
    /// CLI can be addressed by the WebUI (and vice versa). The serial
    /// auto-increments per `(project, harness)` pair, computed from the live
    /// registry so deletes/renames don't desync the counter.
    /// What:
    ///   1. `project_name` defaults to `basename(project_path)` when None
    ///      (matches the spec's `name` default).
    ///   2. Scans existing session names to compute the next serial.
    ///   3. Delegates to `new_session` with the generated name.
    /// Test: integration only — requires tmux.
    pub async fn connect_session(
        &self,
        project_path: &Path,
        adapter_type: AdapterType,
        project_name: Option<&str>,
        harness: &str,
    ) -> Result<TmSession> {
        let project = project_name.map(str::to_string).unwrap_or_else(|| {
            project_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string()
        });
        let existing: Vec<String> = self
            .registry
            .list_sessions()?
            .into_iter()
            .map(|s| s.name)
            .collect();
        let name = crate::tm::project_config::next_session_name(&project, harness, &existing);
        self.new_session(&name, project_path, Some(adapter_type))
            .await
    }

    /// One-shot `/connect <path> <adapter> [name]`: ensure a project config
    /// exists for `project_path`, then spawn a session under it.
    ///
    /// Why: The refined spec calls for `/connect` to auto-create the project
    /// when missing (implicit form) and to behave identically to the explicit
    /// WebUI "Add Project" form. Both entry points must produce the same
    /// `<name>.toml` and registry rows, so we route both through a single
    /// helper here.
    /// What:
    ///   1. Open the `ProjectConfigStore` at `projects_config_dir` (e.g.,
    ///      `<project>/.trusty-agents/projects/`).
    ///   2. `find_or_create` the config — short-circuits when a project for
    ///      the path already exists; otherwise writes `<name>.toml` with the
    ///      adapter as the default harness.
    ///   3. Delegate to `connect_session`, which picks the next serial and
    ///      spawns the tmux session.
    /// Returns the resolved (existing-or-new) project config alongside the
    /// freshly-created session so callers can render "created project X and
    /// session X-adapter-1" messages with one round trip.
    /// Test: integration only — requires tmux. Unit tests cover the
    /// underlying helpers (`find_or_create`, `next_session_name`).
    pub async fn connect_or_create(
        &self,
        projects_config_dir: &Path,
        project_path: &Path,
        adapter_id: &str,
        name_override: Option<&str>,
    ) -> Result<(crate::tm::ProjectConfig, TmSession)> {
        let store = crate::tm::ProjectConfigStore::open(projects_config_dir)?;
        let cfg = store.find_or_create(project_path, adapter_id, name_override)?;
        let adapter_type = AdapterType::from_id(adapter_id);
        if matches!(adapter_type, AdapterType::Unknown) {
            bail!("unknown adapter id '{}'", adapter_id);
        }
        let session = self
            .connect_session(
                project_path,
                adapter_type,
                Some(&cfg.project.name),
                adapter_id,
            )
            .await?;
        Ok((cfg, session))
    }

    // ==================== Inspection ====================

    /// List all sessions, reconciling with live tmux first.
    ///
    /// Why: Callers expect `list` to reflect reality (no stale Running rows
    /// for sessions that have died). Doing reconcile here means the UI
    /// doesn't have to remember to call it.
    /// What: Calls `reconcile()` then returns the registry's session list.
    /// Test: integration only.
    pub async fn list_sessions(&self) -> Result<Vec<TmSession>> {
        let _ = self.reconcile().await?;
        self.registry.list_sessions()
    }

    /// List all projects (after reconcile so session summaries are fresh).
    pub async fn list_projects(&self) -> Result<Vec<TmProject>> {
        let _ = self.reconcile().await?;
        self.registry.list_projects()
    }

    /// Detect the adapter type for a running tmux session by pane snapshot.
    ///
    /// Why: Used during reconcile to identify newly-discovered sessions and
    /// also exposed for explicit "redetect" UX.
    /// What: Captures the last 100 lines of pane output, runs the adapter
    /// registry's `detect`, and converts the id string into an
    /// `AdapterType`.
    /// Test: integration only.
    pub async fn detect_adapter(&self, name_or_id: &str) -> Result<(AdapterType, f32)> {
        let session = self.resolve_session(name_or_id)?;
        let output = self
            .tmux
            .capture_output(&session.tmux_session_name, None, Some(100))?;
        let (id, confidence) = self.adapters.detect(&output);
        Ok((AdapterType::from_id(id), confidence))
    }

    // ==================== Project Helpers ====================

    /// Get the existing TmProject for `path`, or create and register one.
    ///
    /// Why: TM is directory-rooted — every session needs a project, and we
    /// want one project per path (idempotent across calls).
    /// What: Looks up by path; on miss, builds a new `TmProject`, runs
    /// framework detection against `path`, persists it, and returns it.
    /// Test: integration only (requires filesystem access).
    pub async fn get_or_create_project(&self, path: &Path) -> Result<TmProject> {
        if let Some(p) = self.registry.get_project_by_path(path)? {
            return Ok(p);
        }
        let mut project = TmProject::new(path.to_path_buf());
        project.framework = detect_framework(path);
        self.registry.register_project(&project)?;
        Ok(project)
    }

    // ==================== Reconcile ====================

    /// Sync the registry with live tmux state.
    ///
    /// Why: tmux sessions can be created/destroyed outside our control;
    /// reconcile is how the registry catches up.
    /// What:
    ///   1. List live tmux sessions.
    ///   2. For each live session not yet in the registry: capture its pane,
    ///      detect adapter, ensure a project exists, and register it.
    ///   3. For each registry session whose tmux counterpart is gone: mark
    ///      it Orphaned (delegated to `registry.reconcile`).
    /// Test: integration only.
    pub async fn reconcile(&self) -> Result<ReconcileReport> {
        let mut report = ReconcileReport::default();

        let live = self.tmux.list_sessions().unwrap_or_default();
        let live_names: Vec<String> = live.iter().map(|s| s.name.clone()).collect();
        let known = self.registry.list_sessions()?;
        let known_names: Vec<String> = known.iter().map(|s| s.tmux_session_name.clone()).collect();

        // 1. Discover new live sessions.
        for live_session in &live {
            if known_names.iter().any(|n| n == &live_session.name) {
                continue;
            }
            // Best-effort path discovery; fall back to "/" if tmux can't tell us.
            let path_str = self
                .tmux
                .get_session_path(&live_session.name)
                .unwrap_or_else(|_| "/".to_string());
            let path = std::path::PathBuf::from(&path_str);

            let pane_output = self
                .tmux
                .capture_output(&live_session.name, None, Some(100))
                .unwrap_or_default();
            let (id, _conf) = self.adapters.detect(&pane_output);
            let adapter_type = AdapterType::from_id(id);

            let mut project = self.get_or_create_project(&path).await?;
            let mut session = TmSession::new(
                live_session.name.clone(),
                project.id.clone(),
                path,
                adapter_type,
            );
            session.tmux_session_name = live_session.name.clone();
            project.add_session(session.to_summary());

            self.registry.register_session(&session)?;
            self.registry.update_project(&project)?;
            report.added.push(session);
        }

        // 2. Mark vanished sessions as Orphaned.
        let orphaned_ids = self.registry.reconcile(&live_names)?;
        // Map ids back to tmux names for the report.
        let after = self.registry.list_sessions()?;
        for id in &orphaned_ids {
            if let Some(s) = after.iter().find(|s| &s.id == id) {
                report.orphaned.push(s.tmux_session_name.clone());
            }
        }

        // Prune stale orphaned sessions from the registry so they don't accumulate.
        let pruned = self.registry.prune_orphaned()?;
        if pruned > 0 {
            tracing::debug!("TM reconcile: pruned {pruned} orphaned sessions");
        }

        Ok(report)
    }

    // ==================== Idle Monitor ====================

    /// Poll all Running sessions, observe their pane state via the adapter,
    /// and update registry status when it changes.
    ///
    /// Why: Issue #318 — the registry's session statuses go stale without
    /// periodic observation. Centralizing the poll here means the monitor
    /// task stays a thin wrapper around a single async call.
    /// What: Iterates over Running sessions; for each, captures the last 20
    /// lines of pane output, asks the adapter to observe it, and maps the
    /// resulting `HarnessState` to a `SessionStatus`. Sessions whose tmux
    /// pane has vanished are marked Orphaned. Returns `(name, old, new)`
    /// tuples for every transition so callers can log them.
    /// Test: integration only (requires a live tmux server).
    pub async fn poll_sessions(&self) -> Result<Vec<(String, SessionStatus, SessionStatus)>> {
        let sessions = self.registry.list_sessions()?;
        let mut transitions = Vec::new();

        for session in sessions
            .iter()
            .filter(|s| s.status == SessionStatus::Running)
        {
            let output = match self
                .tmux
                .capture_output(&session.tmux_session_name, None, Some(20))
            {
                Ok(o) => o,
                Err(_) => {
                    // Pane gone → orphan.
                    self.registry
                        .update_session_status(&session.id, SessionStatus::Orphaned)?;
                    transitions.push((
                        session.name.clone(),
                        SessionStatus::Running,
                        SessionStatus::Orphaned,
                    ));
                    continue;
                }
            };

            let adapter = self.get_adapter(&session.adapter_type);
            let obs = adapter.observe(&output);

            let new_status = match obs.state {
                crate::adapters::HarnessState::Idle => SessionStatus::Idle,
                crate::adapters::HarnessState::Error => {
                    tracing::warn!("TM: session '{}' in error state", session.name);
                    SessionStatus::Running
                }
                _ => SessionStatus::Running,
            };

            if new_status != session.status {
                self.registry
                    .update_session_status(&session.id, new_status.clone())?;
                transitions.push((session.name.clone(), session.status.clone(), new_status));
            }
        }

        Ok(transitions)
    }

    // ==================== Private Helpers ====================

    /// Resolve `name_or_id` to a session: tries name first, then id.
    fn resolve_session(&self, name_or_id: &str) -> Result<TmSession> {
        if let Some(s) = self.registry.get_session_by_name(name_or_id)? {
            return Ok(s);
        }
        if let Some(s) = self.registry.get_session(name_or_id)? {
            return Ok(s);
        }
        bail!("session '{}' not found", name_or_id)
    }

    /// Map an `AdapterType` to its registered adapter, falling back to shell
    /// when the variant isn't registered (which should not happen in
    /// practice — `AdapterRegistry::new` registers all built-ins).
    fn get_adapter(&self, adapter_type: &AdapterType) -> Arc<dyn HarnessAdapter> {
        let id = adapter_type.as_str();
        if let Some(a) = self.adapters.get(id) {
            return a;
        }
        // Fall back to shell for Unknown / missing.
        self.adapters
            .get("shell")
            .expect("shell adapter must be registered")
    }

    /// Pick a tmux session name that doesn't collide with an existing one.
    ///
    /// Why: Users want friendly names ("api"), but tmux requires uniqueness.
    /// Suffixing `-2`, `-3`, … is the same convention ai-commander uses.
    /// What: Returns `name` if free, otherwise tries `name-2`, `name-3`, …
    /// up to 100 attempts; bumps to `name-<timestamp>` only if all fail.
    /// Test: integration only.
    fn unique_tmux_name(&self, name: &str) -> String {
        if !self.tmux.session_exists(name) {
            return name.to_string();
        }
        for i in 2..=100 {
            let candidate = format!("{}-{}", name, i);
            if !self.tmux.session_exists(&candidate) {
                return candidate;
            }
        }
        // Pathological fallback — extremely unlikely to be hit in practice.
        format!("{}-{}", name, chrono::Utc::now().timestamp())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_reconcile_report_default() {
        let report = ReconcileReport::default();
        assert!(report.added.is_empty());
        assert!(report.orphaned.is_empty());
        assert!(report.updated.is_empty());
    }

    #[test]
    fn test_resolve_session_not_found() {
        if !TmuxOrchestrator::is_available() {
            // Without tmux we can't construct TmManager — skip this case.
            // The intent is documented; integration coverage exercises the
            // happy path elsewhere.
            return;
        }
        let dir = TempDir::new().unwrap();
        let mgr = TmManager::new(dir.path()).unwrap();
        let err = mgr.resolve_session("nonexistent").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    #[ignore = "requires tmux installed"]
    fn test_tmux_available() {
        assert!(TmuxOrchestrator::is_available());
    }
}
