//! TmProject and TmSession data model.
//!
//! Why: Provides a stable, serde-friendly representation of a directory-rooted
//! project and the tmux sessions associated with it. New fields use
//! `#[serde(default)]` so older on-disk records remain loadable.
//! What: Defines TmProject, TmSession, and the supporting enums/structs
//! (DetectedFramework, SessionStatus, AdapterType, SessionSummary,
//! ProjectProcessState).
//! Test: Unit tests at the bottom of this file plus framework tests in
//! `framework.rs` exercise construction, serde round-trips, and helpers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A directory-rooted project grouping related tmux sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmProject {
    /// Stable identifier — UUID v4.
    pub id: String,
    /// Human label (e.g., "my-api"). Defaults to directory name.
    pub name: String,
    /// (1) Absolute path of the project root.
    pub path: PathBuf,
    /// IDs of sessions associated with this project.
    pub session_ids: Vec<String>,
    pub created_at: DateTime<Utc>,

    /// (2) Detected code/framework.
    #[serde(default)]
    pub framework: DetectedFramework,

    /// (3) Session summaries (richer than session_ids alone).
    #[serde(default)]
    pub sessions: Vec<SessionSummary>,

    /// (4) Process and memory state.
    #[serde(default)]
    pub process_state: ProjectProcessState,
}

impl TmProject {
    /// Why: Construct a project rooted at `path` with sensible defaults so
    /// callers don't have to hand-build every field.
    /// What: Returns a TmProject with a fresh UUID, name derived from the
    /// directory's basename, and empty session/process state.
    /// Test: `test_tm_project_new` asserts name derivation and empty defaults.
    pub fn new(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            path,
            session_ids: Vec::new(),
            created_at: Utc::now(),
            framework: DetectedFramework::default(),
            sessions: Vec::new(),
            process_state: ProjectProcessState::default(),
        }
    }

    /// Add a session to this project (both id list and summary).
    ///
    /// Why: Keep session_ids and sessions in sync; replace any existing entry
    /// with the same session_id so updates are idempotent.
    /// What: Appends session_id if absent and replaces/appends the summary.
    /// Test: `test_project_add_remove_session` covers add and replace.
    pub fn add_session(&mut self, summary: SessionSummary) {
        if !self.session_ids.contains(&summary.session_id) {
            self.session_ids.push(summary.session_id.clone());
        }
        self.sessions.retain(|s| s.session_id != summary.session_id);
        self.sessions.push(summary);
    }

    /// Why: Cleanly drop a session from both tracking lists when it ends.
    /// What: Removes the session_id from session_ids and the matching summary.
    /// Test: `test_project_add_remove_session` asserts both lists shrink.
    pub fn remove_session(&mut self, session_id: &str) {
        self.session_ids.retain(|id| id != session_id);
        self.sessions.retain(|s| s.session_id != session_id);
    }
}

/// Detected language/framework info for a project root.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DetectedFramework {
    pub language: Option<String>,
    pub framework: Option<String>,
    pub package_manager: Option<String>,
    /// Files that triggered detection (e.g., ["Cargo.toml"]).
    #[serde(default)]
    pub detected_from: Vec<String>,
    pub detected_at: Option<DateTime<Utc>>,
}

impl DetectedFramework {
    /// Why: Quick check used by UI to decide whether to render a "?" badge.
    /// What: Returns true iff a language was detected.
    /// Test: `test_detect_unknown` asserts false for an empty directory.
    pub fn is_known(&self) -> bool {
        self.language.is_some()
    }

    /// Why: Provide a single human-readable label for status lines.
    /// What: Returns "lang/framework", "lang", or "unknown" depending on
    /// which fields are populated.
    /// Test: `test_detected_framework_display` covers all three branches.
    pub fn display(&self) -> String {
        match (&self.language, &self.framework) {
            (Some(lang), Some(fw)) => format!("{}/{}", lang, fw),
            (Some(lang), None) => lang.clone(),
            _ => "unknown".to_string(),
        }
    }
}

/// Lifecycle state of a tmux session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    Running,
    Paused,
    Idle,
    Orphaned,
    Stopped,
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "Running"),
            Self::Paused => write!(f, "Paused"),
            Self::Idle => write!(f, "Idle"),
            Self::Orphaned => write!(f, "Orphaned"),
            Self::Stopped => write!(f, "Stopped"),
        }
    }
}

/// Which AI harness adapter drives a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdapterType {
    ClaudeMpm,
    ClaudeCode,
    Codex,
    Augment,
    GeminiCode,
    Shell,
    OpenMpm,
    Unknown,
}

impl AdapterType {
    /// Why: Decouple the adapter registry's string ids from the typed enum so
    /// new adapters can be added without breaking serialized data.
    /// What: Maps known adapter id strings to enum variants; everything else
    /// becomes `Unknown`.
    /// Test: `test_adapter_type_roundtrip` asserts from_id+as_str is identity
    /// for every known variant.
    pub fn from_id(id: &str) -> Self {
        match id {
            "claude-mpm" => Self::ClaudeMpm,
            "claude-code" => Self::ClaudeCode,
            "codex" => Self::Codex,
            "augment" => Self::Augment,
            "gemini" => Self::GeminiCode,
            "shell" => Self::Shell,
            "trusty-agents" => Self::OpenMpm,
            _ => Self::Unknown,
        }
    }

    /// Why: Reverse of `from_id` for persisting to TOML/JSON or rendering.
    /// What: Returns the canonical id string for this adapter variant.
    /// Test: `test_adapter_type_roundtrip` asserts round-trip identity.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ClaudeMpm => "claude-mpm",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Augment => "augment",
            Self::GeminiCode => "gemini",
            Self::Shell => "shell",
            Self::OpenMpm => "trusty-agents",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for AdapterType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Summary of a session stored within TmProject.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub name: String,
    pub adapter_type: AdapterType,
    pub status: SessionStatus,
}

/// Process and memory state for a project.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectProcessState {
    /// PIDs of tracked processes (dev servers, watchers, etc.).
    #[serde(default)]
    pub tracked_pids: Vec<u32>,
    /// Last memory/context snapshot from a pause event.
    pub memory_snapshot: Option<String>,
    pub snapshot_at: Option<DateTime<Utc>>,
    /// Total tokens consumed across all sessions.
    #[serde(default)]
    pub total_tokens: u64,
}

impl ProjectProcessState {
    /// Why: Centralize the snapshot-stamp so callers can't forget to update
    /// `snapshot_at` alongside `memory_snapshot`.
    /// What: Stores `snapshot` and stamps `snapshot_at = now()`.
    /// Test: indirectly covered by integration code; basic round-trip via
    /// project tests.
    pub fn save_snapshot(&mut self, snapshot: String) {
        self.memory_snapshot = Some(snapshot);
        self.snapshot_at = Some(Utc::now());
    }

    /// Why: Token totals are added from many sessions and we don't want to
    /// panic on overflow in long-running processes.
    /// What: Saturating add of `tokens` into `total_tokens`.
    /// Test: covered indirectly; saturating semantics rely on stdlib.
    pub fn add_tokens(&mut self, tokens: u64) {
        self.total_tokens = self.total_tokens.saturating_add(tokens);
    }
}

/// One tmux session managed by TM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmSession {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub project_path: PathBuf,
    pub adapter_type: AdapterType,
    pub tmux_session_name: String,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
    pub notes: Option<String>,
    /// Whether the user has pinned this session in the UI.
    ///
    /// Why: Lets the WebUI surface frequently-used sessions at the top of
    /// lists. Stored on the session (not the project) because favorites are
    /// per-session.
    /// What: Bool, defaulting to `false` on legacy records via `serde(default)`.
    /// Test: `test_favorite_default_false` and `test_serde_legacy_session_no_favorite`.
    #[serde(default)]
    pub favorite: bool,
}

impl TmSession {
    /// Why: One canonical constructor so created_at/last_active are always
    /// initialized to the same instant and the tmux name defaults to the
    /// human name.
    /// What: Returns a Running session with a fresh UUID.
    /// Test: `test_tm_session_to_summary` builds a session and inspects fields.
    pub fn new(
        name: String,
        project_id: String,
        project_path: PathBuf,
        adapter_type: AdapterType,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            tmux_session_name: name.clone(),
            name,
            project_id,
            project_path,
            adapter_type,
            status: SessionStatus::Running,
            created_at: now,
            last_active: now,
            notes: None,
            favorite: false,
        }
    }

    /// Why: TmProject stores compact summaries rather than full TmSession
    /// objects to keep on-disk records small.
    /// What: Projects the session into a SessionSummary.
    /// Test: `test_tm_session_to_summary` asserts every summary field matches.
    pub fn to_summary(&self) -> SessionSummary {
        SessionSummary {
            session_id: self.id.clone(),
            name: self.name.clone(),
            adapter_type: self.adapter_type.clone(),
            status: self.status.clone(),
        }
    }

    /// Why: Activity tracking — bump `last_active` on user input or output.
    /// What: Sets `last_active = now()`.
    /// Test: covered indirectly by `test_tm_session_last_active_ago`.
    pub fn touch(&mut self) {
        self.last_active = Utc::now();
    }

    /// Why: UI needs a compact "X ago" label for session lists.
    /// What: Returns "<n>s ago", "<n>m ago", or "<n>h ago" based on the delta
    /// between now and last_active. Negative deltas (clock skew) clamp to 0.
    /// Test: `test_tm_session_last_active_ago` checks both seconds and minutes
    /// branches by mutating `last_active` directly.
    pub fn last_active_ago(&self) -> String {
        let secs = (Utc::now() - self.last_active).num_seconds().max(0) as u64;
        if secs < 60 {
            format!("{}s ago", secs)
        } else if secs < 3600 {
            format!("{}m ago", secs / 60)
        } else {
            format!("{}h ago", secs / 3600)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_tm_project_new() {
        let p = TmProject::new(PathBuf::from("/tmp/my-api"));
        assert_eq!(p.name, "my-api");
        assert!(p.session_ids.is_empty());
        assert!(p.sessions.is_empty());
        assert!(!p.framework.is_known());
        assert_eq!(p.process_state.total_tokens, 0);
        assert!(!p.id.is_empty());
    }

    #[test]
    fn test_tm_project_new_unknown_when_path_has_no_basename() {
        let p = TmProject::new(PathBuf::from("/"));
        assert_eq!(p.name, "unknown");
    }

    #[test]
    fn test_tm_session_last_active_ago() {
        let mut s = TmSession::new(
            "work".to_string(),
            "pid".to_string(),
            PathBuf::from("/tmp/work"),
            AdapterType::ClaudeMpm,
        );
        // Just-created session — should be in seconds.
        assert!(s.last_active_ago().ends_with("s ago"));

        // Backdate ~5 minutes — should be in minutes.
        s.last_active = Utc::now() - Duration::seconds(5 * 60 + 10);
        let ago = s.last_active_ago();
        assert!(ago.ends_with("m ago"), "expected minutes, got {}", ago);

        // Backdate ~2 hours — should be in hours.
        s.last_active = Utc::now() - Duration::seconds(2 * 3600 + 30);
        let ago = s.last_active_ago();
        assert!(ago.ends_with("h ago"), "expected hours, got {}", ago);
    }

    #[test]
    fn test_adapter_type_roundtrip() {
        for id in [
            "claude-mpm",
            "claude-code",
            "codex",
            "augment",
            "gemini",
            "shell",
            "trusty-agents",
        ] {
            assert_eq!(AdapterType::from_id(id).as_str(), id);
        }
        // Unknown ids round-trip through Unknown.
        assert_eq!(AdapterType::from_id("nope"), AdapterType::Unknown);
        assert_eq!(AdapterType::Unknown.as_str(), "unknown");
    }

    #[test]
    fn test_tm_session_to_summary() {
        let s = TmSession::new(
            "main".to_string(),
            "p1".to_string(),
            PathBuf::from("/tmp/p1"),
            AdapterType::ClaudeCode,
        );
        let sum = s.to_summary();
        assert_eq!(sum.session_id, s.id);
        assert_eq!(sum.name, "main");
        assert_eq!(sum.adapter_type, AdapterType::ClaudeCode);
        assert_eq!(sum.status, SessionStatus::Running);
    }

    #[test]
    fn test_project_add_remove_session() {
        let mut p = TmProject::new(PathBuf::from("/tmp/proj"));
        let s = TmSession::new(
            "s1".to_string(),
            p.id.clone(),
            p.path.clone(),
            AdapterType::Shell,
        );
        let sid = s.id.clone();
        p.add_session(s.to_summary());
        assert_eq!(p.session_ids.len(), 1);
        assert_eq!(p.sessions.len(), 1);

        // Idempotent re-add updates summary in place.
        let mut s2 = s.clone();
        s2.status = SessionStatus::Paused;
        let mut updated_summary = s2.to_summary();
        updated_summary.status = SessionStatus::Paused;
        p.add_session(updated_summary);
        assert_eq!(p.session_ids.len(), 1);
        assert_eq!(p.sessions.len(), 1);
        assert_eq!(p.sessions[0].status, SessionStatus::Paused);

        p.remove_session(&sid);
        assert!(p.session_ids.is_empty());
        assert!(p.sessions.is_empty());
    }

    #[test]
    fn test_detected_framework_display() {
        let mut fw = DetectedFramework::default();
        assert_eq!(fw.display(), "unknown");
        assert!(!fw.is_known());

        fw.language = Some("rust".to_string());
        assert_eq!(fw.display(), "rust");
        assert!(fw.is_known());

        fw.framework = Some("axum".to_string());
        assert_eq!(fw.display(), "rust/axum");
    }

    #[test]
    fn test_favorite_default_false() {
        let s = TmSession::new(
            "a".to_string(),
            "p".to_string(),
            PathBuf::from("/tmp/x"),
            AdapterType::Shell,
        );
        assert!(!s.favorite);
    }

    #[test]
    fn test_serde_legacy_session_no_favorite() {
        // Legacy on-disk session JSON without the `favorite` field must load
        // with `favorite = false`.
        let json = r#"{
            "id": "sid",
            "name": "s",
            "project_id": "pid",
            "project_path": "/tmp",
            "adapter_type": "Shell",
            "tmux_session_name": "s",
            "status": "Running",
            "created_at": "2025-01-01T00:00:00Z",
            "last_active": "2025-01-01T00:00:00Z",
            "notes": null
        }"#;
        let s: TmSession = serde_json::from_str(json).expect("legacy session must load");
        assert!(!s.favorite);
    }

    #[test]
    fn test_serde_backward_compat_minimal_project() {
        // Older records may lack the new fields; serde defaults must fill them.
        let json = r#"{
            "id": "abc",
            "name": "old",
            "path": "/tmp/old",
            "session_ids": [],
            "created_at": "2025-01-01T00:00:00Z"
        }"#;
        let p: TmProject = serde_json::from_str(json).expect("legacy json must load");
        assert_eq!(p.name, "old");
        assert!(!p.framework.is_known());
        assert!(p.sessions.is_empty());
        assert_eq!(p.process_state.total_tokens, 0);
    }
}
