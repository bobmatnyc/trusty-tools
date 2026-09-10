//! Persisted orchestration contracts. Source content and credentials never enter this state.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum KnowledgeError {
    #[error("{0}")]
    BadRequest(String),
    #[error("Knowledge configuration changed; reload before retrying")]
    Conflict,
    #[error("{0}")]
    InvalidState(String),
    #[error("Knowledge state unavailable: {0}")]
    Unavailable(String),
}
pub type Result<T> = std::result::Result<T, KnowledgeError>;
impl From<std::io::Error> for KnowledgeError {
    fn from(e: std::io::Error) -> Self {
        Self::Unavailable(e.to_string())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Project,
    Gmail,
    Gdrive,
    Slack,
    Gcal,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceDescriptor {
    pub id: String,
    pub revision: String,
    pub kind: SourceKind,
    pub display_name: String,
    pub dependency_reasons: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtectedStore {
    pub root: PathBuf,
    pub index_id: String,
    pub protected: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    BlockedOnDependency,
    Retryable,
    Cancelled,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StageStatus {
    pub status: JobStatus,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordAdmission {
    pub event_digest: String,
    pub record_time: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeJob {
    pub record: Option<RecordAdmission>,
    pub id: String,
    pub source_id: String,
    pub source_revision: String,
    pub window: Window,
    pub status: JobStatus,
    pub indexing: StageStatus,
    pub extraction: StageStatus,
    pub cleanup: StageStatus,
    pub publication: StageStatus,
    pub dependency_reasons: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeState {
    pub schema_version: u32,
    pub assistant_id: String,
    pub revision: String,
    pub anchor_at: DateTime<Utc>,
    pub history_months: u32,
    pub paused: bool,
    #[serde(default)]
    pub binding_confirmed: bool,
    pub store: ProtectedStore,
    pub projects_by_chat: BTreeMap<String, Vec<String>>,
    pub sources: Vec<SourceDescriptor>,
    pub jobs: Vec<KnowledgeJob>,
    pub admitted_events: Vec<String>,
}
