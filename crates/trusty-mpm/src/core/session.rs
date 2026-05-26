//! Session state model.
//!
//! Why: The daemon, TUI, and Telegram bot all need a shared view of what a
//! Claude Code session is and what state it is in.
//! What: Defines `SessionId`, `SessionStatus`, and the `Session` snapshot type
//! exchanged over IPC.
//! Test: `cargo test -p trusty-mpm-core` round-trips a `Session` through JSON.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for a managed session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(value_type = String, format = "uuid")]
pub struct SessionId(pub Uuid);

impl SessionId {
    /// Generate a fresh random session id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

/// Lifecycle state of a managed Claude Code session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum SessionStatus {
    /// Session process is spawning.
    Starting,
    /// Session is running and accepting input.
    Active,
    /// Session is blocked awaiting a permission decision.
    AwaitingApproval,
    /// Session has been detached but the process is still alive.
    Detached,
    /// Session has been paused by the operator; its state is saved for resume.
    Paused,
    /// Session process has exited.
    Stopped,
}

/// Control model used to host a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum ControlModel {
    /// Session runs inside a named tmux session.
    Tmux,
    /// Session runs under a daemon-owned PTY.
    Pty,
    /// Session runs non-interactively via the Claude Code SDK / headless mode.
    Sdk,
}

/// How a managed session was discovered or created.
///
/// Why: most Claude Code sessions run in native Terminal.app windows rather
/// than tmux, so the dashboard and API must distinguish a tmux-discovered
/// session from a bare OS process so operators know what they can control.
/// What: [`Tmux`](SessionHost::Tmux) for tmux-hosted sessions (the default,
/// for backward compatibility), [`Native`](SessionHost::Native) for a
/// `claude`/`claude-code` OS process discovered via `ps`.
/// Test: covered by `Session` JSON round-trip tests and the discovery suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SessionHost {
    /// Session was created or discovered inside a tmux session.
    #[default]
    Tmux,
    /// Session is a native OS process (e.g. running in Terminal.app).
    Native,
}

/// A point-in-time snapshot of a session, returned by the daemon API.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Session {
    /// Unique session id.
    pub id: SessionId,
    /// Working directory the session was launched in.
    pub workdir: String,
    /// Current lifecycle status.
    pub status: SessionStatus,
    /// How the session is hosted.
    pub control: ControlModel,
    /// Number of active agent delegations within the session.
    pub active_delegations: u32,
    /// Friendly tmux session name (`tmpm-<adjective>-<noun>`).
    ///
    /// Why: the daemon's reaper compares this against the live tmux session
    /// list, and the dashboard shows it instead of the raw UUID.
    #[serde(default)]
    pub tmux_name: String,
    /// When the session was registered with the daemon.
    #[serde(default = "SystemTime::now")]
    #[schema(value_type = String, format = "date-time")]
    pub created_at: SystemTime,
    /// When the session was last observed alive (heartbeat / activity).
    #[serde(default = "SystemTime::now")]
    #[schema(value_type = String, format = "date-time")]
    pub last_seen: SystemTime,
    /// The trusty-mpm project this session belongs to, if any.
    ///
    /// Why: a session is started inside a registered project; recording the
    /// project root lets the CLI and dashboard filter sessions per project.
    /// `None` for sessions started outside any registered project.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub project_path: Option<PathBuf>,
    /// When the session was paused (`None` if not paused).
    ///
    /// Why: the operator pauses a session to step away and resume later; the
    /// pause timestamp is shown in the dashboard and persisted to disk.
    /// What: stamped to now on pause, cleared to `None` on resume.
    #[serde(default)]
    #[schema(value_type = Option<String>, format = "date-time")]
    pub paused_at: Option<SystemTime>,
    /// A short summary captured when the session was paused.
    ///
    /// Why: gives the operator a "where I left off" note when resuming; either
    /// supplied explicitly or derived from the captured pane output.
    /// What: free-form text, cleared to `None` on resume.
    #[serde(default)]
    pub pause_summary: Option<String>,
    /// How the session was discovered or created.
    ///
    /// Why: native Terminal.app `claude` processes are discovered alongside
    /// tmux sessions; the dashboard shows the origin so operators know whether
    /// the session is tmux-controllable.
    /// What: defaults to [`SessionHost::Tmux`] for backward compatibility.
    #[serde(default)]
    pub origin: SessionHost,
    /// OS process id, when the session is a discovered native process.
    ///
    /// Why: native sessions are keyed by pid; recording it lets the discovery
    /// scan skip a process already registered and lets the dashboard show it.
    /// What: `Some(pid)` for [`SessionHost::Native`] sessions, `None` otherwise.
    #[serde(default)]
    pub pid: Option<u32>,
}

impl Session {
    /// Build a freshly-registered session with derived metadata.
    ///
    /// Why: every call site that creates a `Session` needs the same defaults —
    /// a friendly tmux name and `created_at`/`last_seen` stamped to now;
    /// centralizing it prevents drift. A session belongs to a project, so the
    /// tmux name should identify that project rather than be random.
    /// What: when `project_dir` is `Some`, derives `tmux_name` from the folder
    /// basename via [`crate::core::names::name_from_dir`] (`tmpm-<folder>`); when
    /// `None`, falls back to the UUID-derived [`crate::core::names::name_from_uuid`].
    /// Both timestamps are stamped to the current time.
    /// Test: `new_derives_tmux_name`, `new_derives_tmux_name_from_dir`.
    pub fn new(
        id: SessionId,
        workdir: impl Into<String>,
        control: ControlModel,
        project_dir: Option<&Path>,
    ) -> Self {
        let now = SystemTime::now();
        let tmux_name = match project_dir {
            Some(dir) => crate::core::names::name_from_dir(dir),
            None => crate::core::names::name_from_uuid(&id.0),
        };
        Self {
            id,
            workdir: workdir.into(),
            status: SessionStatus::Starting,
            control,
            active_delegations: 0,
            tmux_name,
            created_at: now,
            last_seen: now,
            project_path: None,
            paused_at: None,
            pause_summary: None,
            origin: SessionHost::Tmux,
            pid: None,
        }
    }

    /// Mark the session as observed alive right now.
    ///
    /// Why: the reaper and dashboard use `last_seen` to distinguish active from
    /// stale sessions; heartbeats and activity must refresh it.
    /// What: sets `last_seen` to the current time.
    /// Test: `touch_advances_last_seen`.
    pub fn touch(&mut self) {
        self.last_seen = SystemTime::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_json_roundtrip() {
        let mut session = Session::new(SessionId::new(), "/tmp/project", ControlModel::Tmux, None);
        session.status = SessionStatus::Active;
        session.active_delegations = 2;
        let json = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, session.id);
        assert_eq!(back.active_delegations, 2);
        assert_eq!(back.tmux_name, session.tmux_name);
    }

    #[test]
    fn new_derives_tmux_name() {
        let id = SessionId::new();
        let session = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
        assert_eq!(session.tmux_name, crate::core::names::name_from_uuid(&id.0));
        assert!(session.tmux_name.starts_with("tmpm-"));
        assert_eq!(session.status, SessionStatus::Starting);
    }

    #[test]
    fn new_derives_tmux_name_from_dir() {
        // With a project dir the tmux name is the sanitized folder, not random.
        let dir = std::path::Path::new("/Users/x/trusty-mpm");
        let session = Session::new(
            SessionId::new(),
            "/Users/x/trusty-mpm",
            ControlModel::Tmux,
            Some(dir),
        );
        assert_eq!(session.tmux_name, "tmpm-trusty-mpm");
    }

    #[test]
    fn new_has_no_project_by_default() {
        let session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        assert_eq!(session.project_path, None);
    }

    #[test]
    fn project_path_survives_json_roundtrip() {
        let mut session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        session.project_path = Some(std::path::PathBuf::from("/work/proj"));
        let json = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.project_path,
            Some(std::path::PathBuf::from("/work/proj"))
        );
    }

    #[test]
    fn new_has_no_pause_state_by_default() {
        let session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        assert_eq!(session.paused_at, None);
        assert_eq!(session.pause_summary, None);
    }

    #[test]
    fn pause_state_survives_json_roundtrip() {
        let mut session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        session.status = SessionStatus::Paused;
        session.paused_at = Some(SystemTime::now());
        session.pause_summary = Some("mid-task".to_string());
        let json = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, SessionStatus::Paused);
        assert!(back.paused_at.is_some());
        assert_eq!(back.pause_summary.as_deref(), Some("mid-task"));
    }

    #[test]
    fn new_defaults_to_tmux_host_without_pid() {
        let session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        assert_eq!(session.origin, SessionHost::Tmux);
        assert_eq!(session.pid, None);
    }

    #[test]
    fn native_host_state_survives_json_roundtrip() {
        let mut session = Session::new(SessionId::new(), "/work/proj", ControlModel::Pty, None);
        session.origin = SessionHost::Native;
        session.pid = Some(4242);
        let json = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.origin, SessionHost::Native);
        assert_eq!(back.pid, Some(4242));
    }

    #[test]
    fn session_host_label_is_lowercase_in_json() {
        // The wire form must be a stable lowercase token the dashboard keys on.
        assert_eq!(
            serde_json::to_string(&SessionHost::Native).unwrap(),
            "\"native\"",
        );
        assert_eq!(
            serde_json::to_string(&SessionHost::Tmux).unwrap(),
            "\"tmux\"",
        );
    }

    #[test]
    fn legacy_session_json_without_origin_defaults_to_tmux() {
        // Sessions persisted before the `origin`/`pid` fields existed must still
        // deserialize, defaulting to a tmux host.
        let legacy = r#"{
            "id": "00000000-0000-0000-0000-000000000000",
            "workdir": "/tmp/p",
            "status": "Active",
            "control": "Tmux",
            "active_delegations": 0
        }"#;
        let back: Session = serde_json::from_str(legacy).unwrap();
        assert_eq!(back.origin, SessionHost::Tmux);
        assert_eq!(back.pid, None);
    }

    #[test]
    fn touch_advances_last_seen() {
        let mut session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        let before = session.last_seen;
        std::thread::sleep(std::time::Duration::from_millis(2));
        session.touch();
        assert!(session.last_seen >= before);
    }
}
