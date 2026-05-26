//! Universal Claude Code hook event vocabulary.
//!
//! Why: trusty-mpm's daemon subscribes to *every* Claude Code hook event for
//! full observability — not just the seven claude-mpm wired by default. A
//! single exhaustive enum keeps the relay, the dashboard event feed, and the
//! Telegram subscription filter aligned on one canonical set of names.
//! What: `HookEvent` enumerates all 32 known Claude Code lifecycle events, a
//! `HookEventRecord` wire type carrying the event plus session/timestamp/
//! payload, and helpers for category grouping and string round-trips.
//! Test: `cargo test -p trusty-mpm-core` round-trips every variant and asserts
//! `HookEvent::ALL` has no duplicates and parses back from its wire name.

use serde::{Deserialize, Serialize};

use crate::core::session::SessionId;

/// A Claude Code hook lifecycle event.
///
/// Why: claude-mpm only registers a handful; trusty-mpm relays all of them so
/// the dashboard can show a complete live feed and Telegram can alert on any.
/// What: serde uses the exact PascalCase Claude Code wire names so
/// `settings.json` semantics and forwarded event JSON deserialize unchanged.
/// Test: `all_events_round_trip` serializes/deserializes every variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    /// Fired before a tool is invoked; handler may allow/deny.
    PreToolUse,
    /// Fired after a tool completes successfully.
    PostToolUse,
    /// Fired after a tool invocation fails.
    PostToolUseFailure,
    /// The session's top-level turn has finished.
    Stop,
    /// A subagent delegation has finished.
    SubagentStop,
    /// A new session has started.
    SessionStart,
    /// A session is ending / being torn down.
    SessionEnd,
    /// The user submitted a prompt.
    UserPromptSubmit,
    /// Context is about to be compacted.
    PreCompact,
    /// Context compaction has completed.
    PostCompact,
    /// A git worktree was created.
    WorktreeCreate,
    /// A git worktree was removed.
    WorktreeRemove,
    /// A teammate/subagent has gone idle.
    TeammateIdle,
    /// Project instructions (CLAUDE.md / skill rules) were loaded.
    InstructionsLoaded,
    /// A configuration value changed mid-session.
    ConfigChange,
    /// The working directory changed.
    CwdChanged,
    /// A watched file changed on disk.
    FileChanged,
    /// A task was created in the task tracker.
    TaskCreated,
    /// A task was marked completed.
    TaskCompleted,
    /// A task was updated.
    TaskUpdated,
    /// A task was stopped/cancelled.
    TaskStopped,
    /// A `Stop` handler itself failed.
    StopFailure,
    /// A `SubagentStop` handler itself failed.
    SubagentStopFailure,
    /// A permission request was denied.
    PermissionDenied,
    /// A permission request was granted.
    PermissionGranted,
    /// A notification was emitted to the user.
    Notification,
    /// An MCP server connected.
    McpServerConnected,
    /// An MCP server disconnected.
    McpServerDisconnected,
    /// A subagent delegation started.
    SubagentStart,
    /// Token-usage accounting was updated.
    TokenUsageUpdate,
    /// An error surfaced anywhere in the session.
    ErrorRaised,
    /// A skill was resolved/activated.
    SkillActivated,
}

impl HookEvent {
    /// Every known hook event, used to drive exhaustive subscriptions/tests.
    pub const ALL: [HookEvent; 32] = [
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::PostToolUseFailure,
        HookEvent::Stop,
        HookEvent::SubagentStop,
        HookEvent::SessionStart,
        HookEvent::SessionEnd,
        HookEvent::UserPromptSubmit,
        HookEvent::PreCompact,
        HookEvent::PostCompact,
        HookEvent::WorktreeCreate,
        HookEvent::WorktreeRemove,
        HookEvent::TeammateIdle,
        HookEvent::InstructionsLoaded,
        HookEvent::ConfigChange,
        HookEvent::CwdChanged,
        HookEvent::FileChanged,
        HookEvent::TaskCreated,
        HookEvent::TaskCompleted,
        HookEvent::TaskUpdated,
        HookEvent::TaskStopped,
        HookEvent::StopFailure,
        HookEvent::SubagentStopFailure,
        HookEvent::PermissionDenied,
        HookEvent::PermissionGranted,
        HookEvent::Notification,
        HookEvent::McpServerConnected,
        HookEvent::McpServerDisconnected,
        HookEvent::SubagentStart,
        HookEvent::TokenUsageUpdate,
        HookEvent::ErrorRaised,
        HookEvent::SkillActivated,
    ];

    /// The exact Claude Code wire name (PascalCase) for this event.
    ///
    /// Why: the forwarder shim and `settings.json` both use these strings;
    /// a single conversion point avoids drift between parse and emit paths.
    /// What: returns the same identifier serde uses.
    /// Test: `wire_name_parses_back` asserts `from_wire(e.wire_name()) == e`.
    pub fn wire_name(&self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::PostToolUseFailure => "PostToolUseFailure",
            HookEvent::Stop => "Stop",
            HookEvent::SubagentStop => "SubagentStop",
            HookEvent::SessionStart => "SessionStart",
            HookEvent::SessionEnd => "SessionEnd",
            HookEvent::UserPromptSubmit => "UserPromptSubmit",
            HookEvent::PreCompact => "PreCompact",
            HookEvent::PostCompact => "PostCompact",
            HookEvent::WorktreeCreate => "WorktreeCreate",
            HookEvent::WorktreeRemove => "WorktreeRemove",
            HookEvent::TeammateIdle => "TeammateIdle",
            HookEvent::InstructionsLoaded => "InstructionsLoaded",
            HookEvent::ConfigChange => "ConfigChange",
            HookEvent::CwdChanged => "CwdChanged",
            HookEvent::FileChanged => "FileChanged",
            HookEvent::TaskCreated => "TaskCreated",
            HookEvent::TaskCompleted => "TaskCompleted",
            HookEvent::TaskUpdated => "TaskUpdated",
            HookEvent::TaskStopped => "TaskStopped",
            HookEvent::StopFailure => "StopFailure",
            HookEvent::SubagentStopFailure => "SubagentStopFailure",
            HookEvent::PermissionDenied => "PermissionDenied",
            HookEvent::PermissionGranted => "PermissionGranted",
            HookEvent::Notification => "Notification",
            HookEvent::McpServerConnected => "McpServerConnected",
            HookEvent::McpServerDisconnected => "McpServerDisconnected",
            HookEvent::SubagentStart => "SubagentStart",
            HookEvent::TokenUsageUpdate => "TokenUsageUpdate",
            HookEvent::ErrorRaised => "ErrorRaised",
            HookEvent::SkillActivated => "SkillActivated",
        }
    }

    /// Parse a hook event from its Claude Code wire name.
    ///
    /// Why: the forwarder shim receives raw event JSON keyed by these strings.
    /// What: returns `None` for an unrecognized name so callers can log-and-skip.
    /// Test: `wire_name_parses_back` covers the full round-trip.
    pub fn from_wire(name: &str) -> Option<HookEvent> {
        HookEvent::ALL
            .iter()
            .copied()
            .find(|e| e.wire_name() == name)
    }

    /// Broad category, used for grouping in the dashboard and alert filters.
    ///
    /// Why: 32 events are too many to display flat; the TUI groups by category.
    /// What: maps each event to one `HookCategory`.
    /// Test: `every_event_has_a_category` asserts the match is exhaustive.
    pub fn category(&self) -> HookCategory {
        match self {
            HookEvent::PreToolUse | HookEvent::PostToolUse | HookEvent::PostToolUseFailure => {
                HookCategory::Tool
            }
            HookEvent::Stop
            | HookEvent::SubagentStop
            | HookEvent::SubagentStart
            | HookEvent::StopFailure
            | HookEvent::SubagentStopFailure
            | HookEvent::TeammateIdle => HookCategory::Agent,
            HookEvent::SessionStart
            | HookEvent::SessionEnd
            | HookEvent::UserPromptSubmit
            | HookEvent::InstructionsLoaded
            | HookEvent::CwdChanged
            | HookEvent::ConfigChange => HookCategory::Session,
            HookEvent::PreCompact | HookEvent::PostCompact | HookEvent::TokenUsageUpdate => {
                HookCategory::Memory
            }
            HookEvent::WorktreeCreate | HookEvent::WorktreeRemove => HookCategory::Worktree,
            HookEvent::FileChanged => HookCategory::File,
            HookEvent::TaskCreated
            | HookEvent::TaskCompleted
            | HookEvent::TaskUpdated
            | HookEvent::TaskStopped => HookCategory::Task,
            HookEvent::PermissionDenied | HookEvent::PermissionGranted => HookCategory::Permission,
            HookEvent::Notification
            | HookEvent::McpServerConnected
            | HookEvent::McpServerDisconnected
            | HookEvent::ErrorRaised
            | HookEvent::SkillActivated => HookCategory::System,
        }
    }
}

/// Coarse grouping of hook events for dashboard panels and alert filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookCategory {
    /// Tool invocation lifecycle.
    Tool,
    /// Agent / subagent delegation lifecycle.
    Agent,
    /// Session lifecycle and configuration.
    Session,
    /// Context / token / compaction events.
    Memory,
    /// Git worktree lifecycle.
    Worktree,
    /// File system change events.
    File,
    /// Task tracker events.
    Task,
    /// Permission grant/deny events.
    Permission,
    /// Notifications, MCP connectivity, errors, skills.
    System,
}

/// A hook event observed by the daemon, tagged with session and timing.
///
/// Why: the relay needs one wire type to push over the event stream to the
/// TUI feed and Telegram; raw Claude Code payloads vary per event, so the
/// arbitrary `payload` is kept as opaque JSON.
/// What: pairs a `HookEvent` with the originating `SessionId`, a UTC timestamp,
/// and the raw event payload.
/// Test: `record_round_trips` serializes a record and reads it back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEventRecord {
    /// Which session emitted this event.
    pub session: SessionId,
    /// The hook event kind.
    pub event: HookEvent,
    /// UTC timestamp the daemon received the event.
    pub at: chrono::DateTime<chrono::Utc>,
    /// Raw Claude Code event payload (shape varies per event).
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl HookEventRecord {
    /// Build a record stamped with the current UTC time.
    pub fn now(session: SessionId, event: HookEvent, payload: serde_json::Value) -> Self {
        Self {
            session,
            event,
            at: chrono::Utc::now(),
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_events_round_trip() {
        for event in HookEvent::ALL {
            let json = serde_json::to_string(&event).unwrap();
            let back: HookEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
        }
    }

    #[test]
    fn all_events_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for event in HookEvent::ALL {
            assert!(seen.insert(event), "duplicate event in ALL: {event:?}");
        }
        assert_eq!(seen.len(), 32);
    }

    #[test]
    fn wire_name_parses_back() {
        for event in HookEvent::ALL {
            assert_eq!(HookEvent::from_wire(event.wire_name()), Some(event));
        }
        assert_eq!(HookEvent::from_wire("NotAnEvent"), None);
    }

    #[test]
    fn every_event_has_a_category() {
        // Exhaustive match in `category()` means this just exercises all 32.
        for event in HookEvent::ALL {
            let _ = event.category();
        }
    }

    #[test]
    fn record_round_trips() {
        let rec = HookEventRecord::now(
            SessionId::new(),
            HookEvent::PreToolUse,
            serde_json::json!({"tool": "Bash"}),
        );
        let json = serde_json::to_string(&rec).unwrap();
        let back: HookEventRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.event, HookEvent::PreToolUse);
        assert_eq!(back.payload["tool"], "Bash");
    }
}
