//! The production [`SessionControl`] impl over the daemon's managed surface.
//!
//! Why: the SM-STDIO adapter (DOC-14 §1A.1) and the SM-8 delegation loop both
//! drive sessions through the narrow [`SessionControl`] trait (now defined in
//! `core::sm::control` so the agent-side loop can depend on it too). This file
//! holds the PRODUCTION impl: an in-process bridge onto the daemon's existing
//! managed-session control surface (§2.6) — the SAME code `tm ticket`/the HTTP
//! `…/managed/*` routes use. Keeping it here (not in core) is deliberate: it
//! needs daemon-internal handles (`DaemonState`, `spawn_managed`, …) that core
//! must not depend on. The trait + input/error types are re-exported from this
//! module so `sm_stdio`'s public surface (`pub use control::{…}`) is unchanged.
//! What: [`DaemonSessionControl`] delegates each verb to the in-process
//! [`crate::session_manager::SessionManager`] + [`spawn_managed`]/[`resume_managed`]
//! (so the stdio path and the HTTP path cannot diverge). The trait, [`LaunchParams`],
//! and [`SessionControlError`] are re-exported from [`crate::core::sm::control`].
//! Test: `DaemonSessionControl` is exercised by the daemon's managed-session
//! integration tests (it forwards to the same helpers those cover); the
//! dispatcher's `sm.sessions.*` round-trips use the `tests.rs` mock.

use std::sync::Arc;

use async_trait::async_trait;

// Re-export the relocated trait + types so `sm_stdio`'s public surface
// (`pub use control::{DaemonSessionControl, LaunchParams, SessionControl,
// SessionControlError}`) is unchanged after the SM-8 move to core.
pub use crate::core::sm::control::{LaunchParams, SessionControl, SessionControlError};

use crate::daemon::managed_routes::{SpawnParams, record_to_json, resume_managed, spawn_managed};
use crate::daemon::state::DaemonState;
use crate::session_manager::{ManagedError, ManagedSessionId};

/// Production [`SessionControl`] over the in-process managed-session surface.
///
/// Why: the SM runs IN the daemon, so the spec's "delegate to t-mpm's
/// session-manager" is a direct in-process call to the SAME helpers the HTTP
/// `…/managed/*` routes use ([`spawn_managed`]/[`resume_managed`] +
/// [`crate::session_manager::SessionManager`]). Calling them directly (rather
/// than looping back over HTTP) keeps one code path and means the stdio surface
/// inherits every fix to the managed surface for free.
/// What: holds the shared `Arc<DaemonState>` and forwards each verb to its
/// managed-surface counterpart, returning the same flat JSON the HTTP routes do
/// (via [`record_to_json`]).
/// Test: forwards to helpers covered by the managed integration tests; the
/// dispatcher round-trips use the `tests.rs` mock instead.
pub struct DaemonSessionControl {
    /// Shared daemon state holding the managed [`crate::session_manager::SessionManager`].
    state: Arc<DaemonState>,
}

impl DaemonSessionControl {
    /// Build the production control over the daemon's shared state.
    ///
    /// Why: the stdio entry point constructs this once from the daemon state so
    /// every `sm.sessions.*` call reuses the one live session manager.
    /// What: stores the `Arc<DaemonState>`.
    /// Test: construction is trivial; behaviour is covered by the managed tests.
    pub fn new(state: Arc<DaemonState>) -> Self {
        Self { state }
    }

    /// Parse + validate a managed session id string.
    ///
    /// Why: an invalid UUID must become a graceful `NotFound` (→ JSON-RPC error)
    /// rather than a panic, mirroring the HTTP route's 400/404 handling.
    /// What: parses the string into a [`ManagedSessionId`], mapping failure to
    /// [`SessionControlError::NotFound`].
    /// Test: covered by the managed integration tests.
    fn parse_id(session_id: &str) -> Result<ManagedSessionId, SessionControlError> {
        session_id
            .parse::<uuid::Uuid>()
            .map(ManagedSessionId::from)
            .map_err(|_| SessionControlError::NotFound(session_id.to_string()))
    }

    /// Map a manager [`ManagedError`] onto the control error class.
    ///
    /// Why: a post-parse operation failure must preserve the not-found-vs-backend
    /// distinction so the dispatcher picks the right JSON-RPC code. Reserving
    /// [`SessionControlError::NotFound`] for the genuinely-absent session (and the
    /// UUID-parse failure in [`Self::parse_id`]) — and routing every OTHER manager
    /// failure (tmux, store, invalid-state, I/O) to
    /// [`SessionControlError::Backend`] (`-32603`) — keeps `get`/`stop`/`kill`
    /// consistent with `send`/`resume`, which already surface backend failures as
    /// `Backend`.
    /// What: `ManagedError::SessionNotFound` → `NotFound`; every other variant →
    /// `Backend(e.to_string())`.
    /// Test: `tests.rs::stop_backend_failure_is_backend_not_found` and
    /// `kill_backend_failure_is_backend_not_found`.
    fn map_managed_err(e: ManagedError) -> SessionControlError {
        match e {
            ManagedError::SessionNotFound(msg) => SessionControlError::NotFound(msg),
            other => SessionControlError::Backend(other.to_string()),
        }
    }
}

#[async_trait]
impl SessionControl for DaemonSessionControl {
    /// Launch via the shared [`spawn_managed`] flow (provision → tmux → harness).
    ///
    /// Why: the spec maps `sm.sessions.launch` onto `POST /sessions`, whose
    /// engine is [`spawn_managed`]. Forwarding here keeps the adapter a thin
    /// mapping with zero provisioning logic of its own.
    /// What: maps `workdir → repo_url`, `prompt → task` (defaulting to a generic
    /// description when absent), and `model → runtime`, then returns
    /// `{ session_id }`. The `goal_id` link is recorded by the caller via the
    /// goal store (§9.3), not here, so this stays purely session-control.
    /// Test: forwards to `spawn_managed` (managed integration tests).
    async fn launch(&self, params: LaunchParams) -> Result<serde_json::Value, SessionControlError> {
        let task = params
            .prompt
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| "session-manager launched task".to_string());
        let spawn = SpawnParams {
            repo_url: params.workdir,
            git_ref: String::new(),
            task,
            name_hint: None,
            runtime: params.model,
        };
        let record = spawn_managed(&self.state, spawn)
            .await
            .map_err(SessionControlError::Backend)?;
        Ok(serde_json::json!({ "session_id": record.id.to_string() }))
    }

    /// List all managed sessions as `{ sessions: [record, …] }`.
    async fn list(&self) -> Result<serde_json::Value, SessionControlError> {
        let mgr = self.state.session_manager().await;
        let sessions: Vec<serde_json::Value> =
            mgr.list().await.iter().map(record_to_json).collect();
        Ok(serde_json::json!({ "sessions": sessions }))
    }

    /// Get one session record (the flat managed JSON).
    async fn get(&self, session_id: &str) -> Result<serde_json::Value, SessionControlError> {
        let id = Self::parse_id(session_id)?;
        let mgr = self.state.session_manager().await;
        let record = mgr.get(&id).await.map_err(Self::map_managed_err)?;
        Ok(serde_json::json!({ "session": record_to_json(&record) }))
    }

    /// Inject text into the session pane; returns `{ ok: true }`.
    async fn send(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<serde_json::Value, SessionControlError> {
        let id = Self::parse_id(session_id)?;
        let mgr = self.state.session_manager().await;
        mgr.send_input(&id, text)
            .await
            .map_err(|e| SessionControlError::Backend(e.to_string()))?;
        Ok(serde_json::json!({ "ok": true }))
    }

    /// Stop the runtime, keeping the workspace; returns `{ ok: true }`.
    async fn stop(&self, session_id: &str) -> Result<serde_json::Value, SessionControlError> {
        let id = Self::parse_id(session_id)?;
        let mgr = self.state.session_manager().await;
        mgr.stop(&id).await.map_err(Self::map_managed_err)?;
        Ok(serde_json::json!({ "ok": true }))
    }

    /// Resume a stopped session and re-spawn its runtime; returns `{ ok: true }`.
    async fn resume(&self, session_id: &str) -> Result<serde_json::Value, SessionControlError> {
        let id = Self::parse_id(session_id)?;
        resume_managed(&self.state, &id)
            .await
            .map_err(|e| SessionControlError::Backend(e.to_string()))?;
        Ok(serde_json::json!({ "ok": true }))
    }

    /// Force-stop / reap a session (decommission — terminal teardown).
    ///
    /// Why: `sm.sessions.kill` is the force-stop/reap verb (§1A.1 maps it to the
    /// `DELETE /sessions/{id}` + `/sessions/dead` reap path). The strongest
    /// in-process equivalent is `decommission` (kills runtime + tombstones the
    /// record); a plain `stop` would leave a resumable session, not a reaped one.
    /// What: forwards to `SessionManager::decommission`; returns `{ ok: true }`.
    /// Test: forwards to `decommission` (managed integration tests).
    async fn kill(&self, session_id: &str) -> Result<serde_json::Value, SessionControlError> {
        let id = Self::parse_id(session_id)?;
        let mgr = self.state.session_manager().await;
        mgr.decommission(&id).await.map_err(Self::map_managed_err)?;
        Ok(serde_json::json!({ "ok": true }))
    }
}
