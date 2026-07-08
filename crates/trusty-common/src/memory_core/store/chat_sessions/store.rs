//! `ChatSessionStore` implementation backed by redb.
//!
//! Why: extracted from `chat_sessions.rs` to keep each file under the
//! 500-SLOC cap. Contains the `ChatSessionStore` struct, its methods, and the
//! two helper functions (`resolve_redb_path`, `write_record`).
//! What: `ChatSessionStore` owns an `Arc<redb::Database>` and exposes
//! `open` / `create_session` / `list_sessions` / `get_session` /
//! `upsert_session` / `append_message` / `append_messages` /
//! `delete_session`. `append_message(s)` (issue #1712) does its
//! read-modify-write inside a single write transaction so concurrent callers
//! on the same session id never race and drop a message, unlike the
//! `get_session` + `upsert_session` two-transaction sequence.
//! Test: `create_then_get_session_round_trips`, `list_sessions_returns_meta`,
//! `delete_session_removes_row`, `upsert_session_overwrites_history`,
//! `roundtrip_persists_across_reopen`, `append_message_is_atomic_under_concurrency`.

use super::types::{
    ChatMessage, ChatSession, ChatSessionMeta, ChatSessionRecord, ChatSessionStoreError, Result,
    parse_timestamp,
};
use crate::memory_core::store::kg_store::SESSIONS;
use chrono::Utc;
use redb::{ReadableDatabase, ReadableTable};
use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

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
    pub(super) db: Arc<redb::Database>,
    pub(super) path: PathBuf,
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

        let db = super::super::open_or_recreate(&redb_path).map_err(|e| {
            ChatSessionStoreError::Database {
                path: redb_path.clone(),
                source: Box::new(e),
            }
        })?;

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
        out.sort_by_key(|s| Reverse(s.updated_at.timestamp_millis()));
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

    /// Atomically append one message to a session's history.
    ///
    /// Why (issue #1712): thin wrapper over [`Self::append_messages`] for the
    /// common single-turn case (`chat_session_add_turn`).
    /// What: Delegates to `append_messages` with a one-element vec.
    /// Test: `append_message_is_atomic_under_concurrency`,
    /// `append_message_creates_session_when_missing`.
    pub fn append_message(&self, id: &str, message: ChatMessage) -> anyhow::Result<ChatSession> {
        self.append_messages(id, vec![message])
    }

    /// Atomically append one or more messages to a session's history within a
    /// single redb write transaction.
    ///
    /// Why (issue #1712): the historical call sites (`handle_chat_session_
    /// add_turn`, `handle_chat_turn_append`) did `get_session` (a read txn)
    /// followed by `upsert_session` (a separate write txn) — a non-atomic
    /// read-modify-write. Two concurrent callers on the same `session_id`
    /// could both read the same history snapshot; the second `upsert_session`
    /// then clobbers the first, silently dropping a message. redb serialises
    /// all write transactions on one database (`begin_write` blocks until any
    /// prior writer commits or aborts), so reading the current row and
    /// appending to it INSIDE the same write transaction closes the race
    /// entirely: a second concurrent caller's `begin_write` cannot proceed
    /// until the first transaction commits, and when it does proceed it reads
    /// the already-appended history and appends after it.
    /// What: Begins one write transaction, reads the current row (if any),
    /// decodes its JSON history, pushes every message in `messages` in order,
    /// re-encodes, and inserts — all before commit. Creates the session
    /// (`title = None`) when `id` is unseen, matching `upsert_session`'s
    /// historical create-on-write behaviour. Returns the full post-append
    /// session so callers don't need a second `get_session` round trip.
    /// Test: `append_message_is_atomic_under_concurrency`,
    /// `append_messages_appends_pair_in_order`.
    pub fn append_messages(
        &self,
        id: &str,
        messages: Vec<ChatMessage>,
    ) -> anyhow::Result<ChatSession> {
        Ok(self.append_messages_inner(id, messages)?)
    }

    fn append_messages_inner(&self, id: &str, messages: Vec<ChatMessage>) -> Result<ChatSession> {
        let now = Utc::now().to_rfc3339();
        let wtx = self
            .db
            .begin_write()
            .map_err(|e| ChatSessionStoreError::Transaction {
                path: self.path.clone(),
                source: Box::new(e),
            })?;

        // Read the current row THROUGH THIS SAME write transaction (not a
        // separate `begin_read`) so no other writer can land a commit between
        // the read and the append below. Scoped to its own block so the
        // read-only `table` handle (and the `AccessGuard` borrow it hands
        // back from `get`) drops before we re-open the table mutably for the
        // insert — redb permits opening the same table more than once inside
        // one write transaction as long as the handles don't overlap.
        let existing_record: Option<ChatSessionRecord> = {
            let table = wtx
                .open_table(SESSIONS)
                .map_err(|e| ChatSessionStoreError::Table {
                    path: self.path.clone(),
                    source: Box::new(e),
                })?;
            match table.get(id).map_err(|e| ChatSessionStoreError::Storage {
                path: self.path.clone(),
                source: Box::new(e),
            })? {
                Some(g) => Some(
                    postcard::from_bytes(g.value())
                        .map_err(|e| ChatSessionStoreError::Postcard { source: e })?,
                ),
                None => None,
            }
        };

        let (title, created_at, mut history) = match existing_record {
            Some(r) => {
                let history: Vec<ChatMessage> =
                    serde_json::from_str(&r.history).unwrap_or_default();
                (r.title, r.created_at, history)
            }
            None => (None, now.clone(), Vec::new()),
        };
        history.extend(messages);

        let history_json =
            serde_json::to_string(&history).map_err(|e| ChatSessionStoreError::Json {
                source: Box::new(e),
            })?;
        let record = ChatSessionRecord {
            title,
            created_at,
            updated_at: now.clone(),
            history: history_json,
        };
        let value_bytes = postcard::to_allocvec(&record)
            .map_err(|e| ChatSessionStoreError::Postcard { source: e })?;
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

        let created_at = parse_timestamp(&record.created_at, "created_at")?;
        let updated_at = parse_timestamp(&record.updated_at, "updated_at")?;
        let history: Vec<ChatMessage> = serde_json::from_str(&record.history).unwrap_or_default();
        Ok(ChatSession {
            id: id.to_string(),
            title: record.title,
            created_at,
            updated_at,
            history,
        })
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
pub(super) fn resolve_redb_path(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|e| e == "db") {
        path.with_extension("redb")
    } else {
        path.to_path_buf()
    }
}
