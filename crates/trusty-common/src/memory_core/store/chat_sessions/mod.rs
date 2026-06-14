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

mod store;
mod types;

pub use store::ChatSessionStore;
pub use types::{ChatMessage, ChatSession, ChatSessionMeta, ChatSessionStoreError};

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
