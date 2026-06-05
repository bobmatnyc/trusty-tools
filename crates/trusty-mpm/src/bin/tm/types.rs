//! Shared response-body types used by the CLI HTTP handlers.
//!
//! Why: the thin CLI handlers deserialize daemon API responses into typed
//! structs; centralising them keeps the handler files free of boilerplate
//! and avoids duplicate definitions.
//! What: `SessionRow`, `ProjectRow`, and `EventRow` mirror the shapes returned
//! by `GET /sessions`, `GET /projects`, and `GET /events/poll`.
//! Test: deserialization is exercised indirectly by the handler unit tests.

use serde::Deserialize;

/// One session row as returned by `GET /sessions`.
#[derive(Debug, Deserialize)]
pub(crate) struct SessionRow {
    /// Session id (a `SessionId` newtype: `{"0": "<uuid>"}`).
    pub(crate) id: serde_json::Value,
    /// Working directory.
    pub(crate) workdir: String,
    /// Lifecycle status string.
    pub(crate) status: serde_json::Value,
    /// Number of active delegations.
    #[serde(default)]
    pub(crate) active_delegations: u32,
}

/// One project row as returned by `GET /projects`.
#[derive(Debug, Deserialize)]
pub(crate) struct ProjectRow {
    /// Absolute project path.
    pub(crate) path: std::path::PathBuf,
    /// Human-readable project name.
    pub(crate) name: String,
}

/// One event row as returned by `GET /events`.
#[derive(Debug, Deserialize)]
pub(crate) struct EventRow {
    /// Originating session (`SessionId` newtype JSON).
    pub(crate) session: serde_json::Value,
    /// Claude Code wire event name.
    pub(crate) event: String,
    /// RFC3339 timestamp the daemon received the event.
    pub(crate) at: String,
    /// Raw event payload (shape varies per event).
    #[serde(default)]
    pub(crate) payload: serde_json::Value,
}
