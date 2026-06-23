//! Session lifecycle state machine for the SESSCTL control plane.
//!
//! Why: the actor task must track exactly which lifecycle state the session is
//! in so it emits the right events, applies the right restart policy, and
//! refuses invalid transitions. Having the state as a first-class Rust enum
//! makes every transition visible and exhaustive-matchable.
//! What: defines [`SessionState`] covering all states in §7.2 of
//! SPEC-SESSCTL-01, plus [`SessionMetadata`] which the registry exposes for
//! `tm list` and HTTP snapshot endpoints.
//! Test: `session_state_is_terminal`, `session_metadata_serde` in inline tests.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::event::BackendKind;
use super::id::ControlSessionId;

/// All lifecycle states a `SessionActor` can be in (§7.2 of SPEC-SESSCTL-01).
///
/// Why: the state machine is the single source of truth for what the actor
/// may do next (emit started, accept input, restart, tear down). Encoding it
/// as an enum with data lets the compiler enforce exhaustive handling.
/// What: covers Starting → Running → Stopping → Stopped, plus Restarting,
/// Failed (terminal), AwaitingAuth (tmux-only), and AuthFailed (terminal).
/// Test: `session_state_is_terminal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    /// Backend is being spawned (child process / tmux session creation).
    Starting,
    /// Backend is alive and accepting input.
    Running,
    /// Auth prompt detected (tmux backend only); operator must resolve via
    /// `tmux attach -t tm:<project>:<n>`.
    AwaitingAuth,
    /// A critical observer lagged; manual SM-agent re-attach required (§6.1).
    AwaitingIntervention,
    /// Graceful stop in progress; backend draining in-flight work.
    Stopping,
    /// Session has reached a clean terminal state.
    Stopped,
    /// Backend crashed; auto-restart in progress (attempt < max_restarts).
    Restarting { attempt: u8 },
    /// Max restarts exceeded or unrecoverable error: terminal failure state.
    Failed,
    /// Auth failed on stream-JSON backend: terminal state (requires re-launch).
    AuthFailed,
}

impl SessionState {
    /// Returns `true` if this state is terminal (actor task loop should exit).
    ///
    /// Why: the actor's `select!` loop checks `is_terminal()` after each
    /// transition to decide whether to break out and deregister.
    /// What: `Stopped`, `Failed`, and `AuthFailed` are terminal; all others
    /// are not.
    /// Test: `session_state_is_terminal`.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped | Self::Failed | Self::AuthFailed)
    }

    /// Returns a short display label for `tm list` and logs.
    ///
    /// Why: consistent string representation lets TUI and HTTP snapshots
    /// display uniform state labels.
    /// What: maps each variant to a lowercase kebab-case label.
    /// Test: `session_state_display`.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::AwaitingAuth => "awaiting-auth",
            Self::AwaitingIntervention => "awaiting-intervention",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Restarting { .. } => "restarting",
            Self::Failed => "failed",
            Self::AuthFailed => "auth-failed",
        }
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Point-in-time metadata snapshot for a live session.
///
/// Why: `tm list` and `GET /sessions/{id}` need a serializable snapshot that
/// captures everything an observer needs without a full event-stream replay.
/// What: stores the session's id, project, backend, current state, timing,
/// optional pending-decision info, and restart tracking.
/// Test: `session_metadata_serde`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    /// The session's stable identifier.
    pub session_id: ControlSessionId,
    /// The project this session belongs to.
    pub project_id: String,
    /// Which backend is running the session.
    pub backend: BackendKind,
    /// Current lifecycle state.
    pub state: SessionState,
    /// Number of times the backend has been auto-restarted.
    pub restart_count: u8,
    /// `true` if at least one restart has occurred (no history replay in alpha-1).
    pub context_lost: bool,
    /// When the session was first started.
    pub started_at: DateTime<Utc>,
    /// When the most recent output event was received.
    pub last_activity_at: DateTime<Utc>,
    /// Pending-decision prompt text, if any.
    pub pending_decision: Option<String>,
    /// Proposed answer to the pending decision, if inferrable.
    pub proposed_default: Option<String>,
    /// Last summary text produced by the §8.2 regex/NLP layer (Phase 3).
    pub last_summary: Option<String>,
}

impl SessionMetadata {
    /// Construct a new `SessionMetadata` in the `Starting` state.
    ///
    /// Why: actors create their metadata record at spawn time before the
    /// backend is ready; all optional fields start as `None`.
    /// What: sets `state = Starting`, `restart_count = 0`, and timestamps to now.
    /// Test: `session_metadata_serde`.
    pub fn new(
        session_id: ControlSessionId,
        project_id: String,
        backend: BackendKind,
    ) -> Self {
        let now = Utc::now();
        Self {
            session_id,
            project_id,
            backend,
            state: SessionState::Starting,
            restart_count: 0,
            context_lost: false,
            started_at: now,
            last_activity_at: now,
            pending_decision: None,
            proposed_default: None,
            last_summary: None,
        }
    }

    /// Uptime in seconds from `started_at` to now.
    ///
    /// Why: `tm list` output includes `uptime_secs` per §4.5 row schema.
    /// What: computes `(Utc::now() - started_at).num_seconds()`, clamped to 0.
    /// Test: uptime grows monotonically; covered indirectly by list-command tests.
    pub fn uptime_secs(&self) -> u64 {
        let delta = Utc::now() - self.started_at;
        delta.num_seconds().max(0) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_state_is_terminal() {
        assert!(SessionState::Stopped.is_terminal());
        assert!(SessionState::Failed.is_terminal());
        assert!(SessionState::AuthFailed.is_terminal());
        assert!(!SessionState::Running.is_terminal());
        assert!(!SessionState::Starting.is_terminal());
        assert!(!SessionState::Restarting { attempt: 2 }.is_terminal());
        assert!(!SessionState::Stopping.is_terminal());
        assert!(!SessionState::AwaitingAuth.is_terminal());
    }

    #[test]
    fn session_state_display() {
        assert_eq!(SessionState::Running.to_string(), "running");
        assert_eq!(SessionState::Restarting { attempt: 1 }.to_string(), "restarting");
        assert_eq!(SessionState::AwaitingIntervention.to_string(), "awaiting-intervention");
    }

    #[test]
    fn session_metadata_serde() {
        let id = ControlSessionId::new("p", 0);
        let meta = SessionMetadata::new(id.clone(), "p".into(), BackendKind::StreamJson);
        let json = serde_json::to_string(&meta).expect("serialize");
        let back: SessionMetadata = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.session_id, id);
        assert_eq!(back.state, SessionState::Starting);
        assert_eq!(back.restart_count, 0);
        assert!(!back.context_lost);
    }

    #[test]
    fn session_metadata_uptime_non_negative() {
        let meta = SessionMetadata::new(
            ControlSessionId::new("x", 0),
            "x".into(),
            BackendKind::Tmux,
        );
        assert!(meta.uptime_secs() >= 0);
    }
}
