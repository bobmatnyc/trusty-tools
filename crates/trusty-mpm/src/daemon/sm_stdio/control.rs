//! The session-control seam the SM stdio adapter maps `sm.sessions.*` onto.
//!
//! Why: the SM-STDIO adapter (DOC-14 §1A.1) is a THIN mapping — it must not
//! carry session-lifecycle business logic. The `sm.sessions.launch/list/get/
//! send/stop/resume/kill` methods map onto the daemon's existing managed-session
//! control surface (§2.6) — the SAME code `tm ticket`/the HTTP `…/managed/*`
//! routes use. Hiding that surface behind a narrow trait keeps the dispatcher
//! (a) transport-neutral and (b) HERMETICALLY testable: the round-trip tests
//! inject a deterministic mock instead of provisioning a real tmux/workspace.
//! What: [`SessionControl`] — the seven async verbs the spec's session methods
//! need, each returning a plain `serde_json::Value` (the wire result body) or a
//! [`SessionControlError`]. [`DaemonSessionControl`] is the production impl that
//! delegates to the in-process [`crate::session_manager::SessionManager`] +
//! [`spawn_managed`]/[`resume_managed`] (so the stdio path and the HTTP path
//! cannot diverge). The mock impl lives in `tests.rs`.
//! Test: `DaemonSessionControl` is exercised by the daemon's managed-session
//! integration tests (it forwards to the same helpers those cover); the
//! dispatcher's `sm.sessions.*` round-trips use the `tests.rs` mock.

use std::sync::Arc;

use async_trait::async_trait;

use crate::daemon::managed_routes::{SpawnParams, record_to_json, resume_managed, spawn_managed};
use crate::daemon::state::DaemonState;
use crate::session_manager::ManagedSessionId;

/// Inputs for `sm.sessions.launch` (§1A.1: `{ workdir, model?, prompt?, goal_id? }`).
///
/// Why: the spec's launch params differ from the managed-session `SpawnParams`
/// shape (`{ repo_url, ref, task }`); keeping the adapter's own owned input
/// struct lets the dispatcher parse the spec params once and hand them to ANY
/// [`SessionControl`] impl (real or mock) without re-deriving the mapping at the
/// call site.
/// What: `workdir` (the directory/repo the session launches against, mapped to
/// the managed `repo_url`), an optional `model` (runtime selector), an optional
/// `prompt` (the task/intent, mapped to the managed `task`), and an optional
/// `goal_id` the caller wants the new session linked to (§9.3).
/// Test: `tests.rs::launch_round_trips` constructs this from JSON params.
#[derive(Debug, Clone)]
pub struct LaunchParams {
    /// Working directory / repository the session launches against.
    pub workdir: String,
    /// Optional model / runtime selector (mapped to the managed `runtime`).
    pub model: Option<String>,
    /// Optional initial prompt / task description (mapped to the managed `task`).
    pub prompt: Option<String>,
    /// Optional goal id to link the launched session to (§9.3).
    pub goal_id: Option<String>,
}

/// A failure surfaced by a [`SessionControl`] operation.
///
/// Why: the dispatcher maps these onto JSON-RPC error responses; a typed enum
/// lets it choose `INVALID_PARAMS` (bad id) vs `INTERNAL_ERROR` (control
/// failure) by VARIANT rather than string-matching, and keeps the adapter
/// panic-free per the workspace convention.
/// What: [`NotFound`](SessionControlError::NotFound) for an absent/invalid
/// session id; [`Backend`](SessionControlError::Backend) for any control-surface
/// failure (provision/tmux/store).
/// Test: `tests.rs` mock returns both variants; the dispatcher asserts the
/// mapped JSON-RPC code.
#[derive(Debug, thiserror::Error)]
pub enum SessionControlError {
    /// The session id was invalid or not present.
    #[error("session not found: {0}")]
    NotFound(String),

    /// Any backend control failure (provisioning, tmux, store, resume).
    #[error("{0}")]
    Backend(String),
}

/// The narrow session-control surface the SM stdio adapter maps onto (§2.6).
///
/// Why: keeps `sm.sessions.*` a thin translation. The dispatcher depends on this
/// trait (Dependency Inversion) so production wires [`DaemonSessionControl`]
/// (the real managed-session surface) while tests wire a deterministic mock with
/// NO tmux/workspace. Every method returns the wire `result` body directly so
/// the dispatcher does no shaping.
/// What: `launch` → `{ session_id }`; `list` → `{ sessions: [...] }`; `get` →
/// the full record JSON; `send` → `{ ok }`; `stop`/`resume`/`kill` → `{ ok }`.
/// All are `Send + Sync` to live behind `Arc<dyn SessionControl>`.
/// Test: `tests.rs` mock; `DaemonSessionControl` via the managed integration tests.
#[async_trait]
pub trait SessionControl: Send + Sync {
    /// Launch a managed session (`POST /sessions`, §2.6) and return its id.
    async fn launch(&self, params: LaunchParams) -> Result<serde_json::Value, SessionControlError>;

    /// List all managed sessions (`GET /sessions`, §2.6).
    async fn list(&self) -> Result<serde_json::Value, SessionControlError>;

    /// Get one session record (`GET /sessions/{id}`, §2.6).
    async fn get(&self, session_id: &str) -> Result<serde_json::Value, SessionControlError>;

    /// Send text into a session's pane (`POST /sessions/{id}/command`, §2.6).
    async fn send(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<serde_json::Value, SessionControlError>;

    /// Stop a session's runtime, keeping the workspace (`DELETE /sessions/{id}`).
    async fn stop(&self, session_id: &str) -> Result<serde_json::Value, SessionControlError>;

    /// Resume a stopped session (`POST /sessions/{id}/resume`, §2.6).
    async fn resume(&self, session_id: &str) -> Result<serde_json::Value, SessionControlError>;

    /// Force-stop / reap a session (`DELETE /sessions/{id}`, §2.6).
    async fn kill(&self, session_id: &str) -> Result<serde_json::Value, SessionControlError>;
}

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
    /// `{ session_id }`. The `goal_id` link is recorded by the dispatcher via the
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
        let record = mgr
            .get(&id)
            .await
            .map_err(|_| SessionControlError::NotFound(session_id.to_string()))?;
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
        mgr.stop(&id)
            .await
            .map_err(|_| SessionControlError::NotFound(session_id.to_string()))?;
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
        mgr.decommission(&id)
            .await
            .map_err(|_| SessionControlError::NotFound(session_id.to_string()))?;
        Ok(serde_json::json!({ "ok": true }))
    }
}
