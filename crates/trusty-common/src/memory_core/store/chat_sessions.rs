//! Persistent chat-session store backed by redb.
//!
//! Why: The trusty-memory web UI's chat panel wants to resume prior
//! conversations after a refresh / restart. Issue #56 migrates the store from
//! rusqlite + r2d2 to redb so the chat sidecar drops the heavy native
//! dependency chain and lines up with the rest of the Memory Palace
//! (`kg_redb.rs`, `payload_store.rs`, `palace_store.rs`). The public
//! `ChatSessionStore` API is unchanged so `trusty-memory` and any open-mpm
//! consumers continue to work as drop-ins — callers still pass a path and
//! get back a `ChatSessionStore`.
//!
//! What: `ChatSessionStore` owns an `Arc<redb::Database>` over a single
//! `chat_sessions.redb` file. Sessions are stored in the `SESSIONS` table
//! defined in `kg_store.rs` keyed by session id (UUID string); the value is
//! a postcard-encoded `ChatSessionRecord` that bundles the title,
//! created/updated timestamps, and the JSON-encoded history blob. History
//! travels as a JSON string (not a postcard sequence) so the wire format and
//! storage format stay aligned, exactly matching the prior SQLite behaviour.
//!
//! Test: `create_then_get_session_round_trips`, `list_sessions_returns_meta`,
//! `delete_session_removes_row`, `upsert_session_overwrites_history`,
//! `roundtrip_persists_across_reopen`. The one-shot SQLite → redb migration
//! was removed in issue #989 (all palaces confirmed migrated).

use chrono::{DateTime, Utc};
use redb::{ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

use crate::memory_core::store::kg_store::SESSIONS;

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
struct ChatSessionRecord {
    title: Option<String>,
    created_at: String,
    updated_at: String,
    /// JSON-encoded `Vec<ChatMessage>` blob.
    history: String,
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

type Result<T> = std::result::Result<T, ChatSessionStoreError>;

/// redb-backed per-palace chat session store.
///
/// Why: Replaces the previous r2d2/rusqlite pool; preserves the public API
/// (`open` / `create_session` / `list_sessions` / `get_session` /
/// `upsert_session` / `delete_session`) so call sites in trusty-memory and
/// open-mpm don't need to change.
/// What: Owns an `Arc<redb::Database>` over a single `chat_sessions.redb`
/// file. All reads run in `begin_read` transactions; writes serialise
/// through `begin_write`.
/// Test: see module-level test list.
pub struct ChatSessionStore {
    db: Arc<redb::Database>,
    path: PathBuf,
}

impl ChatSessionStore {
    /// Open (or create) the redb chat-session store at `path`.
    ///
    /// Why: Each palace gets one chat database; this constructor is idempotent
    /// so it's safe to call on every cold start. Historical callers passed
    /// `<palace>/chat_sessions.db`; we rewrite that to `chat_sessions.redb`
    /// transparently. The one-shot SQLite → redb migration was removed in
    /// issue #989 (all palaces confirmed migrated).
    /// What:
    /// 1. Resolves the redb path. `chat_sessions.db` is rewritten to
    ///    `chat_sessions.redb` next to it; other extensions are kept as-is.
    /// 2. Creates parent directories if missing.
    /// 3. Opens (or creates) the redb database and touches the SESSIONS
    ///    table in a write transaction so range scans on a fresh file
    ///    succeed.
    ///
    /// Test: `create_then_get_session_round_trips`,
    /// `roundtrip_persists_across_reopen`.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Self::open_inner(path).map_err(anyhow::Error::from)
    }

    fn open_inner(path: &Path) -> Result<Self> {
        let redb_path = resolve_redb_path(path);

        if let Some(parent) = redb_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| ChatSessionStoreError::Io {
                path: redb_path.clone(),
                source: e,
            })?;
        }

        // Note: the one-shot SQLite → redb migration (formerly gated behind
        // `sqlite-kg`) was removed in issue #989 — all palaces are confirmed
        // migrated.

        let db =
            super::open_or_recreate(&redb_path).map_err(|e| ChatSessionStoreError::Database {
                path: redb_path.clone(),
                source: Box::new(e),
            })?;

        // Touch the SESSIONS table so it exists on disk before the first
        // read transaction. redb only persists a table once it is opened in
        // a write transaction; doing it here keeps later read transactions
        // on a brand-new file from failing with "table does not exist".
        {
            let wtx = db
                .begin_write()
                .map_err(|e| ChatSessionStoreError::Transaction {
                    path: redb_path.clone(),
                    source: Box::new(e),
                })?;
            {
                let _ = wtx
                    .open_table(SESSIONS)
                    .map_err(|e| ChatSessionStoreError::Table {
                        path: redb_path.clone(),
                        source: Box::new(e),
                    })?;
            }
            wtx.commit().map_err(|e| ChatSessionStoreError::Commit {
                path: redb_path.clone(),
                source: Box::new(e),
            })?;
        }

        Ok(Self {
            db: Arc::new(db),
            path: redb_path,
        })
    }

    /// Create an empty session and return its id.
    ///
    /// Why: The UI creates a session before sending the first message;
    /// returning the id lets the client thread it back through subsequent
    /// `upsert_session` calls.
    /// What: Generates a fresh UUID, writes a row with empty history and
    /// `created_at == updated_at == now`.
    /// Test: `create_then_get_session_round_trips`.
    pub fn create_session(&self, title: Option<String>) -> anyhow::Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let record = ChatSessionRecord {
            title,
            created_at: now.clone(),
            updated_at: now,
            history: "[]".to_string(),
        };
        self.write_record(&id, &record)?;
        Ok(id)
    }

    /// List session metadata (no history) ordered by `updated_at DESC`.
    ///
    /// Why: The session sidebar needs a recent-first list.
    /// What: Scans the SESSIONS table, decodes each row, projects to
    /// `ChatSessionMeta`, sorts by `updated_at` descending.
    /// Test: `list_sessions_returns_meta`.
    pub fn list_sessions(&self) -> anyhow::Result<Vec<ChatSessionMeta>> {
        let metas = self.list_sessions_inner()?;
        Ok(metas)
    }

    fn list_sessions_inner(&self) -> Result<Vec<ChatSessionMeta>> {
        let rtx = self
            .db
            .begin_read()
            .map_err(|e| ChatSessionStoreError::Transaction {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        let table = rtx
            .open_table(SESSIONS)
            .map_err(|e| ChatSessionStoreError::Table {
                path: self.path.clone(),
                source: Box::new(e),
            })?;

        let mut out: Vec<ChatSessionMeta> = Vec::new();
        for entry in table.iter().map_err(|e| ChatSessionStoreError::Storage {
            path: self.path.clone(),
            source: Box::new(e),
        })? {
            let (k, v) = entry.map_err(|e| ChatSessionStoreError::Storage {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
            let id = k.value().to_string();
            let record: ChatSessionRecord = postcard::from_bytes(v.value())
                .map_err(|e| ChatSessionStoreError::Postcard { source: e })?;
            let created_at = parse_timestamp(&record.created_at, "created_at")?;
            let updated_at = parse_timestamp(&record.updated_at, "updated_at")?;
            // History may have been written by an earlier (or external) call
            // with a malformed JSON blob; preserve the historical SQLite
            // behaviour of treating an undecodable history as empty rather
            // than failing the entire list.
            let history: Vec<ChatMessage> =
                serde_json::from_str(&record.history).unwrap_or_default();
            out.push(ChatSessionMeta {
                id,
                title: record.title,
                created_at,
                updated_at,
                message_count: history.len(),
            });
        }
        // redb iterates in key order; we want recent-first by `updated_at`.
        out.sort_by_key(|s| std::cmp::Reverse(s.updated_at.timestamp_millis()));
        Ok(out)
    }

    /// Fetch one session including its full history.
    ///
    /// Why: Resuming a chat needs the entire message log in one call.
    /// What: Reads the row by id, decodes `ChatSessionRecord`, parses the
    /// JSON history. Returns `Ok(None)` on miss.
    /// Test: `create_then_get_session_round_trips`,
    /// `delete_session_removes_row`.
    pub fn get_session(&self, id: &str) -> anyhow::Result<Option<ChatSession>> {
        Ok(self.get_session_inner(id)?)
    }

    fn get_session_inner(&self, id: &str) -> Result<Option<ChatSession>> {
        let rtx = self
            .db
            .begin_read()
            .map_err(|e| ChatSessionStoreError::Transaction {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        let table = rtx
            .open_table(SESSIONS)
            .map_err(|e| ChatSessionStoreError::Table {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        let raw = table.get(id).map_err(|e| ChatSessionStoreError::Storage {
            path: self.path.clone(),
            source: Box::new(e),
        })?;
        let Some(guard) = raw else {
            return Ok(None);
        };
        let record: ChatSessionRecord = postcard::from_bytes(guard.value())
            .map_err(|e| ChatSessionStoreError::Postcard { source: e })?;
        let created_at = parse_timestamp(&record.created_at, "created_at")?;
        let updated_at = parse_timestamp(&record.updated_at, "updated_at")?;
        let history: Vec<ChatMessage> = serde_json::from_str(&record.history).unwrap_or_default();
        Ok(Some(ChatSession {
            id: id.to_string(),
            title: record.title,
            created_at,
            updated_at,
            history,
        }))
    }

    /// Insert or update a session's history (and bump `updated_at`).
    ///
    /// Why: The UI streams every new message exchange to the store; idempotent
    /// upsert keeps retries safe and matches the legacy SQLite contract.
    /// What: If the row exists, preserves `title` and `created_at` and
    /// overwrites `history` and `updated_at`. Otherwise creates a new row
    /// with `title = None` and `created_at == updated_at == now`, matching
    /// the legacy SQLite `INSERT … ON CONFLICT` behaviour.
    /// Test: `upsert_session_overwrites_history`.
    pub fn upsert_session(&self, id: &str, history: &[ChatMessage]) -> anyhow::Result<()> {
        self.upsert_session_inner(id, history)?;
        Ok(())
    }

    fn upsert_session_inner(&self, id: &str, history: &[ChatMessage]) -> Result<()> {
        let history_json =
            serde_json::to_string(history).map_err(|e| ChatSessionStoreError::Json {
                source: Box::new(e),
            })?;
        let now = Utc::now().to_rfc3339();

        // Preserve existing title / created_at if a row is already present.
        let existing = {
            let rtx = self
                .db
                .begin_read()
                .map_err(|e| ChatSessionStoreError::Transaction {
                    path: self.path.clone(),
                    source: Box::new(e),
                })?;
            let table = rtx
                .open_table(SESSIONS)
                .map_err(|e| ChatSessionStoreError::Table {
                    path: self.path.clone(),
                    source: Box::new(e),
                })?;
            let raw = table.get(id).map_err(|e| ChatSessionStoreError::Storage {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
            match raw {
                Some(g) => {
                    let r: ChatSessionRecord = postcard::from_bytes(g.value())
                        .map_err(|e| ChatSessionStoreError::Postcard { source: e })?;
                    Some(r)
                }
                None => None,
            }
        };

        let record = match existing {
            Some(prev) => ChatSessionRecord {
                title: prev.title,
                created_at: prev.created_at,
                updated_at: now,
                history: history_json,
            },
            None => ChatSessionRecord {
                title: None,
                created_at: now.clone(),
                updated_at: now,
                history: history_json,
            },
        };

        self.write_record(id, &record)
    }

    /// Delete a session row. No-op if `id` is unknown.
    ///
    /// Why: Mirrors the SQLite `DELETE … WHERE id = ?` idempotent contract.
    /// What: Removes the key from SESSIONS in a write transaction.
    /// Test: `delete_session_removes_row`.
    pub fn delete_session(&self, id: &str) -> anyhow::Result<()> {
        self.delete_session_inner(id)?;
        Ok(())
    }

    fn delete_session_inner(&self, id: &str) -> Result<()> {
        let wtx = self
            .db
            .begin_write()
            .map_err(|e| ChatSessionStoreError::Transaction {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        {
            let mut table = wtx
                .open_table(SESSIONS)
                .map_err(|e| ChatSessionStoreError::Table {
                    path: self.path.clone(),
                    source: Box::new(e),
                })?;
            table
                .remove(id)
                .map_err(|e| ChatSessionStoreError::Storage {
                    path: self.path.clone(),
                    source: Box::new(e),
                })?;
        }
        wtx.commit().map_err(|e| ChatSessionStoreError::Commit {
            path: self.path.clone(),
            source: Box::new(e),
        })?;
        Ok(())
    }

    /// Internal: serialise `record` and write it under `id` in one txn.
    fn write_record(&self, id: &str, record: &ChatSessionRecord) -> Result<()> {
        let value_bytes = postcard::to_allocvec(record)
            .map_err(|e| ChatSessionStoreError::Postcard { source: e })?;
        let wtx = self
            .db
            .begin_write()
            .map_err(|e| ChatSessionStoreError::Transaction {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        {
            let mut table = wtx
                .open_table(SESSIONS)
                .map_err(|e| ChatSessionStoreError::Table {
                    path: self.path.clone(),
                    source: Box::new(e),
                })?;
            table.insert(id, value_bytes.as_slice()).map_err(|e| {
                ChatSessionStoreError::Storage {
                    path: self.path.clone(),
                    source: Box::new(e),
                }
            })?;
        }
        wtx.commit().map_err(|e| ChatSessionStoreError::Commit {
            path: self.path.clone(),
            source: Box::new(e),
        })?;
        Ok(())
    }
}

/// Internal: callers historically passed `<palace>/chat_sessions.db` for the
/// SQLite store. Now that the store is redb-backed, accept that same path
/// and silently rewrite it to `chat_sessions.redb` so existing call sites
/// continue to work. Paths with any other extension (or no extension) are
/// kept as-is.
fn resolve_redb_path(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|e| e == "db") {
        path.with_extension("redb")
    } else {
        path.to_path_buf()
    }
}

/// Internal: parse an RFC-3339 timestamp into `DateTime<Utc>`.
fn parse_timestamp(s: &str, field: &'static str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|source| ChatSessionStoreError::Timestamp { field, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open() -> (tempfile::TempDir, ChatSessionStore) {
        let dir = tempdir().unwrap();
        let store = ChatSessionStore::open(&dir.path().join("sessions.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn create_then_get_session_round_trips() {
        let (_d, store) = open();
        let id = store.create_session(Some("Hello".into())).unwrap();
        let s = store.get_session(&id).unwrap().expect("session exists");
        assert_eq!(s.id, id);
        assert_eq!(s.title.as_deref(), Some("Hello"));
        assert!(s.history.is_empty());
    }

    #[test]
    fn list_sessions_returns_meta() {
        let (_d, store) = open();
        let a = store.create_session(Some("A".into())).unwrap();
        // Make sure timestamps differ so the recent-first ordering is
        // deterministic on fast machines.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = store.create_session(None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store
            .upsert_session(
                &b,
                &[ChatMessage {
                    role: "user".into(),
                    content: "hi".into(),
                }],
            )
            .unwrap();
        let metas = store.list_sessions().unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].id, b);
        assert_eq!(metas[0].message_count, 1);
        assert!(metas.iter().any(|m| m.id == a));
    }

    #[test]
    fn upsert_session_overwrites_history() {
        let (_d, store) = open();
        let id = store.create_session(None).unwrap();
        store
            .upsert_session(
                &id,
                &[ChatMessage {
                    role: "user".into(),
                    content: "first".into(),
                }],
            )
            .unwrap();
        store
            .upsert_session(
                &id,
                &[
                    ChatMessage {
                        role: "user".into(),
                        content: "first".into(),
                    },
                    ChatMessage {
                        role: "assistant".into(),
                        content: "second".into(),
                    },
                ],
            )
            .unwrap();
        let s = store.get_session(&id).unwrap().unwrap();
        assert_eq!(s.history.len(), 2);
        assert_eq!(s.history[1].content, "second");
    }

    #[test]
    fn delete_session_removes_row() {
        let (_d, store) = open();
        let id = store.create_session(None).unwrap();
        store.delete_session(&id).unwrap();
        assert!(store.get_session(&id).unwrap().is_none());
        // Idempotent
        store.delete_session(&id).unwrap();
    }

    #[test]
    fn upsert_session_preserves_title_across_updates() {
        let (_d, store) = open();
        let id = store.create_session(Some("Original".into())).unwrap();
        store
            .upsert_session(
                &id,
                &[ChatMessage {
                    role: "user".into(),
                    content: "hi".into(),
                }],
            )
            .unwrap();
        let s = store.get_session(&id).unwrap().unwrap();
        assert_eq!(s.title.as_deref(), Some("Original"));
        assert_eq!(s.history.len(), 1);
    }

    #[test]
    fn upsert_session_on_unknown_id_creates_row() {
        let (_d, store) = open();
        // Matches legacy SQLite "INSERT … ON CONFLICT" behaviour: upserting
        // an unknown id should create a row with NULL title.
        let id = "external-id-123";
        store
            .upsert_session(
                id,
                &[ChatMessage {
                    role: "user".into(),
                    content: "hello".into(),
                }],
            )
            .unwrap();
        let s = store.get_session(id).unwrap().expect("row created");
        assert_eq!(s.title, None);
        assert_eq!(s.history.len(), 1);
    }

    #[test]
    fn roundtrip_persists_across_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("chat_sessions.db");

        let id;
        {
            let store = ChatSessionStore::open(&path).unwrap();
            id = store.create_session(Some("Persisted".into())).unwrap();
            store
                .upsert_session(
                    &id,
                    &[ChatMessage {
                        role: "user".into(),
                        content: "remember me".into(),
                    }],
                )
                .unwrap();
        }

        // Reopen via the redb sibling; the DB file should have moved from
        // `chat_sessions.db` to `chat_sessions.redb`.
        let redb_sibling = dir.path().join("chat_sessions.redb");
        assert!(
            redb_sibling.exists(),
            "expected redb file at {}",
            redb_sibling.display()
        );

        let store2 = ChatSessionStore::open(&path).unwrap();
        let s = store2
            .get_session(&id)
            .unwrap()
            .expect("session survives reopen");
        assert_eq!(s.title.as_deref(), Some("Persisted"));
        assert_eq!(s.history.len(), 1);
        assert_eq!(s.history[0].content, "remember me");
    }
}
