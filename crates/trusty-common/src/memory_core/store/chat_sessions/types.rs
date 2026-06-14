//! Public and internal types for the chat-session store.
//!
//! Why: extracted from `chat_sessions.rs` to keep each file under the
//! 500-SLOC cap. All structs, the error enum, and the crate-local `Result`
//! alias live here so `store.rs` can import them cleanly without circular
//! module references.
//! What: `ChatMessage`, `ChatSessionMeta`, `ChatSession`, `ChatSessionRecord`
//! (internal wire type), `ChatSessionStoreError`, and the local `Result<T>`
//! alias.
//! Test: exercised by every round-trip test in `mod.rs`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

/// A single message in a chat session.
///
/// Why: Mirrors `trusty_common::ChatMessage` shape (role + content) without
/// taking a dep on it from the core crate — `serde_json` handles round-trip
/// translation at the API boundary.
/// What: Plain struct, JSON-serialised in the stored `history` blob.
/// Test: Exercised by every session round-trip test.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// Wire-shape summary used by `list_sessions`.
///
/// Why: The web UI lists sessions without their full history; this struct is
/// the minimal projection it consumes.
/// What: Carries id, title, timestamps, and message count.
/// Test: `list_sessions_returns_meta`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSessionMeta {
    pub id: String,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: usize,
}

/// Full session with history.
///
/// Why: `get_session` returns this so the UI can hydrate the chat panel in
/// one round trip.
/// What: Session metadata plus the decoded `Vec<ChatMessage>` history.
/// Test: `create_then_get_session_round_trips`,
/// `roundtrip_persists_across_reopen`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: String,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub history: Vec<ChatMessage>,
}

/// Postcard-encoded value layout for one SESSIONS row.
///
/// Why: redb table values are raw byte slices; we postcard-encode this struct
/// so the (title, timestamps, history) tuple travels as a single fixed
/// schema. Storing `history` as a JSON string (instead of nesting the
/// `Vec<ChatMessage>` in postcard) keeps the wire/storage formats aligned
/// and matches the legacy SQLite shape so the migration is a 1:1 copy.
/// What: Title (Option<String>), RFC-3339 timestamps, and JSON-encoded
/// history string.
/// Test: `roundtrip_persists_across_reopen`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ChatSessionRecord {
    pub(super) title: Option<String>,
    pub(super) created_at: String,
    pub(super) updated_at: String,
    /// JSON-encoded `Vec<ChatMessage>` blob.
    pub(super) history: String,
}

/// Errors raised by `ChatSessionStore`.
///
/// Why: Callers historically saw `anyhow::Error`; switching to a typed error
/// lets them distinguish redb I/O from postcard codec failures while still
/// converting cleanly into `anyhow::Error` at API boundaries via the `?`
/// operator. `NotFound` is a value not an error path — missing rows surface
/// as `Ok(None)` instead.
/// What: Wraps the error sources (redb storage, transaction, table, postcard,
/// JSON, timestamp parsing).
/// Test: Covered indirectly by the round-trip and missing-row tests.
//
// Why (boxing): redb's error types are large enums; box them so the parent
// `Result<_, ChatSessionStoreError>` size stays small and Clippy's
// `result_large_err` lint stays happy.
#[derive(Debug, Error)]
pub enum ChatSessionStoreError {
    #[error("chat session store io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("chat session store redb database error at {path}: {source}")]
    Database {
        path: PathBuf,
        #[source]
        source: Box<redb::DatabaseError>,
    },
    #[error("chat session store redb transaction error at {path}: {source}")]
    Transaction {
        path: PathBuf,
        #[source]
        source: Box<redb::TransactionError>,
    },
    #[error("chat session store redb table error at {path}: {source}")]
    Table {
        path: PathBuf,
        #[source]
        source: Box<redb::TableError>,
    },
    #[error("chat session store redb storage error at {path}: {source}")]
    Storage {
        path: PathBuf,
        #[source]
        source: Box<redb::StorageError>,
    },
    #[error("chat session store redb commit error at {path}: {source}")]
    Commit {
        path: PathBuf,
        #[source]
        source: Box<redb::CommitError>,
    },
    #[error("chat session store postcard codec error: {source}")]
    Postcard {
        #[source]
        source: postcard::Error,
    },
    #[error("chat session store json error: {source}")]
    Json {
        #[source]
        source: Box<serde_json::Error>,
    },
    #[error("chat session store timestamp parse error for {field}: {source}")]
    Timestamp {
        field: &'static str,
        #[source]
        source: chrono::ParseError,
    },
}

/// Module-local `Result` alias using `ChatSessionStoreError`.
pub(super) type Result<T> = std::result::Result<T, ChatSessionStoreError>;

/// Internal: parse an RFC-3339 timestamp into `DateTime<Utc>`.
pub(super) fn parse_timestamp(s: &str, field: &'static str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|source| ChatSessionStoreError::Timestamp { field, source })
}
