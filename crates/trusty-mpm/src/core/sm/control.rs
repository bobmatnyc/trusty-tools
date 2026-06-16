//! The session-control seam the SM drives its fleet through (DOC-14 §2.6).
//!
//! Why: SM-8's delegation loop (§3.4) and the SM-STDIO adapter (§1A.1) both need
//! to drive managed sessions — launch, observe, send, stop — WITHOUT carrying
//! session-lifecycle business logic and WITHOUT a real tmux/workspace in tests.
//! Hiding that surface behind a narrow trait (Dependency Inversion) lets the
//! delegation loop and the dispatcher depend on the trait while production wires
//! the real `DaemonSessionControl` (in `daemon::sm_stdio`) and tests inject a
//! deterministic mock. SM-STDIO originally defined this trait under
//! `daemon::sm_stdio`; SM-8 RELOCATES it here (a feature-independent core
//! location) so the agent-side delegation loop can depend on it too, with
//! `sm_stdio` re-exporting it so its public surface is unchanged.
//! What: [`LaunchParams`] (the launch inputs), [`SessionControlError`] (the typed
//! failure), and [`SessionControl`] — the seven async verbs the spec's session
//! methods need, each returning a plain `serde_json::Value` (the wire result
//! body) or a [`SessionControlError`]. The production `DaemonSessionControl`
//! impl lives in `daemon::sm_stdio::control` (it needs daemon-internal handles);
//! the agent-side mock lives in `agent/delegate/mock_control.rs`.
//! Test: the dispatcher's `sm.sessions.*` round-trips and the SM-8 delegation
//! tests both inject mock impls of this trait.

use async_trait::async_trait;

/// Inputs for `sessions.launch` (§1A.1: `{ workdir, model?, prompt?, goal_id? }`).
///
/// Why: the spec's launch params differ from the managed-session `SpawnParams`
/// shape (`{ repo_url, ref, task }`); keeping the adapter's own owned input
/// struct lets callers (the dispatcher AND the SM-8 delegation loop) parse the
/// spec params once and hand them to ANY [`SessionControl`] impl (real or mock)
/// without re-deriving the mapping at the call site.
/// What: `workdir` (the directory/repo the session launches against, mapped to
/// the managed `repo_url`), an optional `model` (runtime selector), an optional
/// `prompt` (the task/intent, mapped to the managed `task`), and an optional
/// `goal_id` the caller wants the new session linked to (§9.3).
/// Test: `tests.rs::launch_round_trips` and `delegate` tests construct this.
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
/// Why: callers map these onto JSON-RPC error responses (the dispatcher) or onto
/// goal-link failures (the SM-8 loop); a typed enum lets them choose
/// `INVALID_PARAMS` (bad id) vs `INTERNAL_ERROR` (control failure) by VARIANT
/// rather than string-matching, and keeps the adapter panic-free per the
/// workspace convention.
/// What: [`NotFound`](SessionControlError::NotFound) is reserved for a malformed
/// session id (UUID parse failure) OR a genuinely-absent session
/// (`ManagedError::SessionNotFound`); [`Backend`](SessionControlError::Backend)
/// for any other control-surface failure (provision/tmux/store/invalid-state).
/// Test: `sm_stdio::tests` mock returns both variants; the dispatcher asserts the
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

/// The narrow session-control surface the SM drives its fleet through (§2.6).
///
/// Why: keeps both `sm.sessions.*` (a thin translation) AND the SM-8 delegation
/// loop decoupled from the concrete managed-session surface. Callers depend on
/// this trait (Dependency Inversion) so production wires `DaemonSessionControl`
/// (the real managed-session surface) while tests wire a deterministic mock with
/// NO tmux/workspace. Every method returns the wire `result` body directly so the
/// dispatcher does no shaping.
/// What: `launch` → `{ session_id }`; `list` → `{ sessions: [...] }`; `get` →
/// the full record JSON; `send` → `{ ok }`; `stop`/`resume`/`kill` → `{ ok }`.
/// All are `Send + Sync` to live behind `Arc<dyn SessionControl>`.
/// Test: `sm_stdio::tests` mock; the SM-8 `delegate` tests mock; the real
/// `DaemonSessionControl` via the managed integration tests.
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
