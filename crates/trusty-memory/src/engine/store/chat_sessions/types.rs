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

use crate::engine::store::palace_store::PalaceStoreError;
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
#[non_exhaustive] // #9278 A2b: build with `ChatMessage::new`.
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<trusty_common::chat_attachments::StoredAttachment>,
}

impl ChatMessage {
    /// Why: trusty-agents builds a `ChatMessage` without a struct literal (#9278).
    /// What: sets `role` and `content`; `attachments` is empty.
    /// Test: `chat_message_new_has_no_attachments_until_with_attachments`.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            attachments: Vec::new(),
        }
    }

    /// Why: a user turn carries attachments `new` leaves empty (#9278).
    /// What: replaces `attachments` and returns the message.
    /// Test: `chat_message_new_has_no_attachments_until_with_attachments`.
    pub fn with_attachments(
        mut self,
        attachments: Vec<trusty_common::chat_attachments::StoredAttachment>,
    ) -> Self {
        self.attachments = attachments;
        self
    }
}

/// Wire-shape summary used by `list_sessions`.
///
/// Why: The web UI lists sessions without their full history; this struct is
/// the minimal projection it consumes.
/// What: Carries id, title, timestamps, and message count.
/// Test: `list_sessions_returns_meta`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(not(feature = "server"), allow(dead_code))]
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
#[cfg_attr(not(feature = "server"), allow(dead_code))]
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
/// JSON, timestamp parsing) and a palace-format refusal. #9278 T2: public, the
/// error of `ChatSessionStore::{open, create_session}`; every struct variant
/// is `#[non_exhaustive]`.
/// Test: `chat_session_store_paths_return_chat_session_store_error`; the
/// round-trip and missing-row tests.
//
// Why (boxing): redb's error types are large enums; box them so the parent
// `Result<_, ChatSessionStoreError>` size stays small and Clippy's
// `result_large_err` lint stays happy.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ChatSessionStoreError {
    #[error("chat session store io error at {path}: {source}")]
    #[non_exhaustive]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("chat session store redb database error at {path}: {source}")]
    #[non_exhaustive]
    Database {
        path: PathBuf,
        #[source]
        source: Box<redb::DatabaseError>,
    },
    #[error("chat session store redb transaction error at {path}: {source}")]
    #[non_exhaustive]
    Transaction {
        path: PathBuf,
        #[source]
        source: Box<redb::TransactionError>,
    },
    #[error("chat session store redb table error at {path}: {source}")]
    #[non_exhaustive]
    Table {
        path: PathBuf,
        #[source]
        source: Box<redb::TableError>,
    },
    #[error("chat session store redb storage error at {path}: {source}")]
    #[non_exhaustive]
    Storage {
        path: PathBuf,
        #[source]
        source: Box<redb::StorageError>,
    },
    #[error("chat session store redb commit error at {path}: {source}")]
    #[non_exhaustive]
    Commit {
        path: PathBuf,
        #[source]
        source: Box<redb::CommitError>,
    },
    #[error("chat session store postcard codec error: {source}")]
    #[non_exhaustive]
    Postcard {
        #[source]
        source: postcard::Error,
    },
    #[error("chat session store json error: {source}")]
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    #[non_exhaustive]
    Json {
        #[source]
        source: Box<serde_json::Error>,
    },
    /// A stored session's `history` JSON blob failed to parse on load.
    ///
    /// Why (#6196): the load paths previously did `unwrap_or_default()`,
    /// substituting an empty history and hiding data loss — a corrupt session
    /// was then indistinguishable from a genuinely empty one. Surfacing this
    /// lets a chat-resume caller tell "prior conversation lost" from "new empty
    /// session". Carries `id` so the operator knows which row is corrupt.
    #[error("chat session {id} has corrupt history json: {source}")]
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    #[non_exhaustive]
    CorruptHistory {
        id: String,
        #[source]
        source: Box<serde_json::Error>,
    },
    /// #9278 T2: the palace format refused the open.
    #[error(transparent)]
    Palace(PalaceStoreError),
    #[error("chat session store timestamp parse error for {field}: {source}")]
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    #[non_exhaustive]
    Timestamp {
        field: &'static str,
        #[source]
        source: chrono::ParseError,
    },
}

/// Module-local `Result` alias using `ChatSessionStoreError`.
pub(super) type Result<T> = std::result::Result<T, ChatSessionStoreError>;

/// Internal: parse an RFC-3339 timestamp into `DateTime<Utc>`.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(super) fn parse_timestamp(s: &str, field: &'static str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|source| ChatSessionStoreError::Timestamp { field, source })
}
