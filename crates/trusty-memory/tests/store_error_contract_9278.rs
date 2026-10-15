//! #9278 T2: the chat-session store and the vector store return typed errors.
//!
//! Why: a store failure must reach the caller as a variant it can match, not as
//! an `anyhow::Error` whose root type the caller has to guess.
//! What: drives each path into a real failure, or reads its error type by name
//! where no cheap failure exists, and matches the variant.
//! Test: `chat_session_store_paths_return_chat_session_store_error`,
//! `vector_store_paths_return_hnsw_store_error`.

use tempfile::tempdir;
use trusty_memory::engine::store::chat_sessions::{ChatSessionStore, ChatSessionStoreError};
use trusty_memory::engine::store::hnsw_store::HnswStoreError;
use trusty_memory::engine::store::palace_store::PalaceStoreError;
use trusty_memory::engine::store::vector::UsearchStore;
use uuid::Uuid;

/// The error type name of a `Result`, read without naming the type.
fn err_type<T, E>(_: &Result<T, E>) -> &'static str {
    std::any::type_name::<E>()
}

#[test]
fn chat_session_store_paths_return_chat_session_store_error() {
    let dir = tempdir().expect("tempdir");
    let chat = dir.path().join("chat_sessions.db");
    let store = ChatSessionStore::open(&chat).expect("a current palace opens");
    let created = store.create_session(None);
    assert!(
        err_type(&created).ends_with("::ChatSessionStoreError"),
        "create_session: {}",
        err_type(&created)
    );
    drop(store);

    // A newer format in the palace mirror refuses the open before redb runs.
    std::fs::write(dir.path().join("palace.json"), r#"{"format_version": 999}"#)
        .expect("write mirror");
    let err = ChatSessionStore::open(&chat)
        .err()
        .expect("a newer palace must refuse the chat store");
    assert!(
        matches!(
            &err,
            ChatSessionStoreError::Palace(PalaceStoreError::FormatTooNew { found: 999, .. })
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn vector_store_paths_return_hnsw_store_error() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("not-a-dir");
    std::fs::write(&file, b"x").expect("write file");
    let err = UsearchStore::new(file.join("idx.usearch"), 4)
        .err()
        .expect("a parent that is a file must fail the open");
    assert!(matches!(err, HnswStoreError::Other(_)), "{err:?}");
    assert!(err.to_string().contains("parent dir"), "{err}");

    let store = UsearchStore::new(dir.path().join("idx.usearch"), 4).expect("open");
    let shapes = store.chunk_shapes();
    assert!(
        err_type(&shapes).ends_with("::HnswStoreError"),
        "chunk_shapes: {}",
        err_type(&shapes)
    );
    let bad = store
        .upsert_chunks(Uuid::new_v4(), vec![vec![1.0; 5]])
        .await
        .expect_err("a wrong dimension must fail");
    assert!(
        matches!(
            bad,
            HnswStoreError::DimensionMismatch {
                expected: 4,
                got: 5,
                ..
            }
        ),
        "{bad:?}"
    );
}
