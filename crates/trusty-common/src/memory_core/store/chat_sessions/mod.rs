//! Persistent chat-session store backed by redb.
//!
//! Why: The trusty-memory web UI's chat panel wants to resume prior
//! conversations after a refresh / restart. Issue #56 migrates the store from
//! rusqlite + r2d2 to redb so the chat sidecar drops the heavy native
//! dependency chain and lines up with the rest of the Memory Palace
//! (`kg_redb.rs`, `payload_store.rs`, `palace_store.rs`). The public
//! `ChatSessionStore` API is unchanged so `trusty-memory` and any trusty-agents
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

    /// Write a SESSIONS row whose `history` blob is unparseable JSON, directly
    /// through the store's redb handle. There is no public API that produces a
    /// corrupt row, so the #6196 regression tests craft one here (white-box:
    /// this test module is a descendant of `chat_sessions`, so it can reach the
    /// `pub(super)` `db` field and `ChatSessionRecord`).
    fn write_corrupt_row(store: &ChatSessionStore, id: &str, title: &str) {
        use crate::memory_core::store::kg_store::SESSIONS;
        let now = chrono::Utc::now().to_rfc3339();
        let record = super::types::ChatSessionRecord {
            title: Some(title.to_string()),
            created_at: now.clone(),
            updated_at: now,
            history: "{ this is not valid history json".to_string(),
        };
        let bytes = postcard::to_allocvec(&record).unwrap();
        let wtx = store.db.begin_write().unwrap();
        {
            let mut table = wtx.open_table(SESSIONS).unwrap();
            table.insert(id, bytes.as_slice()).unwrap();
        }
        wtx.commit().unwrap();
    }

    /// Read the raw stored bytes for one SESSIONS row (the postcard-encoded
    /// `ChatSessionRecord`), or `None` if the row is absent. Used to prove the
    /// #6196 fail-closed path leaves a corrupt row byte-identical — untouched,
    /// not merely still-unparseable.
    fn read_raw_row(store: &ChatSessionStore, id: &str) -> Option<Vec<u8>> {
        use crate::memory_core::store::kg_store::SESSIONS;
        use redb::ReadableDatabase;
        let rtx = store.db.begin_read().unwrap();
        let table = rtx.open_table(SESSIONS).unwrap();
        table.get(id).unwrap().map(|g| g.value().to_vec())
    }

    /// Why (#6196): a session whose `history` JSON is corrupt must surface as an
    /// error, not `Ok(Some(session))` with an empty history — otherwise a
    /// chat-resume caller cannot tell "prior conversation lost" from "new empty
    /// session". Fails on the pre-fix commit (which returned `Ok(Some(empty))`).
    /// What: writes a corrupt row, asserts `get_session` returns `Err`.
    /// Test: this function.
    #[test]
    fn get_session_surfaces_corrupt_history_as_error() {
        let (_d, store) = open();
        let id = "corrupt-get-1";
        write_corrupt_row(&store, id, "corrupt");
        let result = store.get_session(id);
        assert!(
            result.is_err(),
            "corrupt history must surface as an error, got {result:?}"
        );
    }

    /// Why (#6196): in the list path a corrupt row must not masquerade as a
    /// valid empty session. It is skipped with a warn/count instead. Fails on
    /// the pre-fix commit (which listed the corrupt row with message_count 0).
    /// What: persists one valid session and one corrupt row, asserts the corrupt
    /// row is absent from the list and only the valid session remains.
    /// Test: this function.
    #[test]
    fn list_sessions_skips_corrupt_row_not_render_empty() {
        let (_d, store) = open();
        let good = store.create_session(Some("good".into())).unwrap();
        store
            .upsert_session(
                &good,
                &[ChatMessage {
                    role: "user".into(),
                    content: "hi".into(),
                }],
            )
            .unwrap();
        write_corrupt_row(&store, "corrupt-list-1", "corrupt");

        let metas = store.list_sessions().unwrap();
        assert!(
            metas.iter().all(|m| m.id != "corrupt-list-1"),
            "corrupt row must be skipped, not rendered as a valid empty session: {metas:?}"
        );
        assert!(
            metas.iter().any(|m| m.id == good),
            "the valid session must still be listed"
        );
        assert_eq!(metas.len(), 1, "only the valid session should be listed");
    }

    /// Why (#6196): appending to a session with corrupt history must fail closed
    /// rather than `unwrap_or_default()` and overwrite the row with only the new
    /// message, permanently destroying the corrupt blob. Fails on the pre-fix
    /// commit (which returned `Ok` and clobbered the row).
    /// What: writes a corrupt row, asserts `append_message` returns `Err` and
    /// the stored row bytes are BYTE-IDENTICAL before and after the failed call
    /// (the corrupt blob is untouched, not merely still-unparseable).
    /// Test: this function.
    #[test]
    fn append_message_on_corrupt_history_fails_closed() {
        let (_d, store) = open();
        let id = "corrupt-append-1";
        write_corrupt_row(&store, id, "corrupt");
        let before = read_raw_row(&store, id).expect("corrupt row was written");

        let result = store.append_message(
            id,
            ChatMessage {
                role: "user".into(),
                content: "new".into(),
            },
        );
        assert!(
            result.is_err(),
            "append onto corrupt history must fail closed, got {result:?}"
        );
        let after = read_raw_row(&store, id).expect("corrupt row must still exist");
        assert_eq!(
            before, after,
            "the corrupt row bytes must be byte-identical after the failed append \
             — the aborted write transaction must not have touched them"
        );
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

    /// Why (issue #1712): the historical `get_session` + `upsert_session`
    /// sequence (two separate transactions) let concurrent callers on the
    /// same session id race — the second write clobbers the first, silently
    /// dropping a message. `append_message` must instead read-modify-write
    /// inside one redb write transaction so redb's own write serialisation
    /// makes every concurrent append land.
    /// What: Spawns `N` OS threads that each call `append_message` on the
    /// same shared `Arc<ChatSessionStore>` / session id with a distinct
    /// marker string, joins them all, then asserts the persisted history has
    /// exactly `N` entries and every marker is present (order is not
    /// asserted — only that none were dropped).
    /// Test: this function.
    #[test]
    fn append_message_is_atomic_under_concurrency() {
        use std::collections::HashSet;
        use std::sync::Arc;
        use std::thread;

        let (_d, store) = open();
        let store = Arc::new(store);
        let id = store.create_session(None).unwrap();

        const N: usize = 25;
        let handles: Vec<_> = (0..N)
            .map(|i| {
                let store = Arc::clone(&store);
                let id = id.clone();
                thread::spawn(move || {
                    store
                        .append_message(
                            &id,
                            ChatMessage {
                                role: "user".into(),
                                content: format!("turn-{i}"),
                            },
                        )
                        .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let session = store.get_session(&id).unwrap().expect("session exists");
        assert_eq!(
            session.history.len(),
            N,
            "all {N} concurrent appends must land — none silently dropped"
        );
        let contents: HashSet<&str> = session.history.iter().map(|m| m.content.as_str()).collect();
        for i in 0..N {
            let marker = format!("turn-{i}");
            assert!(contents.contains(marker.as_str()), "missing {marker}");
        }
    }

    /// Why: `append_message` must create the session (matching
    /// `upsert_session`'s historical create-on-write behaviour) when the id
    /// hasn't been seen yet, so `chat_session_add_turn` can implicitly start
    /// a session on the first turn.
    /// What: Calls `append_message` with a fresh id and no prior
    /// `create_session`; asserts the returned session has that id, one
    /// message, and no title.
    /// Test: this function.
    #[test]
    fn append_message_creates_session_when_missing() {
        let (_d, store) = open();
        let id = "auto-created-id";
        let session = store
            .append_message(
                id,
                ChatMessage {
                    role: "user".into(),
                    content: "first".into(),
                },
            )
            .unwrap();
        assert_eq!(session.id, id);
        assert_eq!(session.history.len(), 1);
        assert_eq!(session.title, None);
    }

    /// Why: `handle_chat_turn_append` appends a prompt+response pair in one
    /// call; `append_messages` must preserve their relative order within the
    /// single commit.
    /// What: Appends a two-message vec and asserts both landed in order.
    /// Test: this function.
    #[test]
    fn append_messages_appends_pair_in_order() {
        let (_d, store) = open();
        let id = store.create_session(None).unwrap();
        let session = store
            .append_messages(
                &id,
                vec![
                    ChatMessage {
                        role: "user".into(),
                        content: "prompt".into(),
                    },
                    ChatMessage {
                        role: "assistant".into(),
                        content: "response".into(),
                    },
                ],
            )
            .unwrap();
        assert_eq!(session.history.len(), 2);
        assert_eq!(session.history[0].role, "user");
        assert_eq!(session.history[1].role, "assistant");
    }
}
