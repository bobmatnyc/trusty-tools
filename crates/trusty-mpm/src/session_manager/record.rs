//! Session record types for the managed session-manager.
//!
//! Why: the session manager needs a canonical, serializable representation of
//! every managed session so that state can survive daemon restarts and be
//! exchanged between components without ambiguity.
//! What: defines [`ManagedSessionId`] (a UUID newtype), [`ManagedSessionState`]
//! (the session lifecycle FSM), and [`SessionRecord`] (the full record persisted
//! to disk and returned over the API).
//! Test: serde round-trips are verified in `record_serde_round_trip`; lifecycle
//! variant names are tested in `state_display`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

/// Opaque identifier for a managed session.
///
/// Why: a newtype over [`Uuid`] prevents accidental confusion with other
/// UUID-typed identifiers (e.g. `SessionId` in the core module) at the
/// type level rather than relying on naming conventions.
/// What: wraps `uuid::Uuid`; implements `Display`, `Debug`, and
/// serde derive for transparent JSON/TOML serialization.
/// Test: `managed_session_id_round_trip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ManagedSessionId(pub Uuid);

impl ManagedSessionId {
    /// Generate a new random managed session id.
    ///
    /// Why: all new sessions need a stable, globally unique identifier
    /// assigned at creation time.
    /// What: wraps `Uuid::new_v4()`.
    /// Test: used throughout manager tests.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Return the inner UUID value.
    ///
    /// Why: some callers (e.g. name derivation via `name_from_uuid`) need the
    /// raw UUID without the newtype wrapper.
    /// What: extracts the inner `Uuid`.
    /// Test: `managed_session_id_round_trip`.
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for ManagedSessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ManagedSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for ManagedSessionId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

/// Lifecycle state of a managed session.
///
/// Why: the session manager needs to track where each session is in its
/// lifecycle so operators and reconciliation logic can make informed decisions
/// about what actions are valid (e.g. you cannot send input to a Dead session).
/// What: FSM states from initial creation through active use to termination and
/// post-mortem states for orphaned / re-adopted sessions.
/// Test: `state_display`, serde round-trips in `record_serde_round_trip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSessionState {
    /// The session has been created in the store but tmux setup is in progress.
    Starting,
    /// The tmux session exists and is actively running.
    Active,
    /// The tmux session exists but has been quiet for a while.
    Idle,
    /// The tmux session has been killed or exited; terminal state.
    Dead,
    /// The session record exists in the store but no tmux session was found
    /// during reconciliation — the daemon may have crashed.
    Orphaned,
    /// A tmux session with the right prefix was found during reconciliation
    /// and adopted into the store.
    Adopted,
    /// The session failed to provision or spawn; the record is preserved for
    /// post-mortem inspection but the session is not running.
    Errored,
}

impl fmt::Display for ManagedSessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Starting => "starting",
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Dead => "dead",
            Self::Orphaned => "orphaned",
            Self::Adopted => "adopted",
            Self::Errored => "errored",
        };
        write!(f, "{s}")
    }
}

/// Full record for a managed session, persisted to disk.
///
/// Why: persistence enables crash recovery — the manager can reload all known
/// sessions on startup and reconcile them against live tmux state rather than
/// losing track of sessions between restarts.
/// What: captures every field needed to identify, describe, and operate on a
/// session: its id, tmux name, working directory, human-readable task
/// description, lifecycle state, and timestamps.
/// Test: `record_serde_round_trip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Unique managed session identifier.
    pub id: ManagedSessionId,
    /// tmux session name (e.g. `tmpm-quiet-falcon`).
    pub tmux_name: String,
    /// Working directory the session was started in.
    pub cwd: PathBuf,
    /// Human-readable task description supplied at creation.
    pub task: String,
    /// Current lifecycle state.
    pub state: ManagedSessionState,
    /// When the session record was created.
    pub created_at: DateTime<Utc>,
    /// When the session last showed activity, if ever.
    pub last_activity_at: Option<DateTime<Utc>>,
    /// Isolated workspace path provisioned by the workspace provisioner.
    pub workspace_path: Option<PathBuf>,
    /// Repository URL this session was provisioned from.
    pub repo_url: Option<String>,
    /// Git branch or ref this session was checked out at.
    pub branch: Option<String>,
    /// A pending decision question surfaced by the harness.
    pub pending_decision: Option<String>,
    /// The harness's proposed default answer to the pending decision.
    pub proposed_default: Option<String>,
}

/// Error types for session record operations.
///
/// Why: callers that manipulate session records need structured errors they can
/// pattern-match rather than opaque strings.
/// What: one variant per failure mode — malformed data, missing fields, etc.
/// Test: exercised indirectly through `SessionStore` tests.
#[derive(Debug, Error)]
pub enum RecordError {
    /// A required field was absent or invalid during deserialization.
    #[error("invalid session record: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_session_id_round_trip() {
        let id = ManagedSessionId::new();
        let json = serde_json::to_string(&id).expect("serialize");
        let back: ManagedSessionId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
        assert_eq!(id.as_uuid(), back.as_uuid());
    }

    #[test]
    fn state_display() {
        assert_eq!(ManagedSessionState::Starting.to_string(), "starting");
        assert_eq!(ManagedSessionState::Active.to_string(), "active");
        assert_eq!(ManagedSessionState::Idle.to_string(), "idle");
        assert_eq!(ManagedSessionState::Dead.to_string(), "dead");
        assert_eq!(ManagedSessionState::Orphaned.to_string(), "orphaned");
        assert_eq!(ManagedSessionState::Adopted.to_string(), "adopted");
    }

    #[test]
    fn record_serde_round_trip() {
        let record = SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-quiet-falcon".into(),
            cwd: PathBuf::from("/tmp/project"),
            task: "implement feature X".into(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: Some(Utc::now()),
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.id, record.id);
        assert_eq!(back.tmux_name, record.tmux_name);
        assert_eq!(back.state, record.state);
    }
}
